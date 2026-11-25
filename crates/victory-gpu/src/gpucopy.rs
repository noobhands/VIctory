//! The title's buffer copies on the GPU, in the order the title recorded
//! them.
//!
//! A title copy between buffers (`CopyBufferRegion`, `CopyResource` of
//! buffers, `CopyMemoryToMemoryX`) is otherwise done by the CPU when the
//! list is submitted ([`crate::host::HostList::defer_copy`]): before any of
//! the list's GPU work. A copy of what a dispatch earlier in the list
//! writes then copies the bytes from before that dispatch. The ray-tracing
//! structure builder copies the compacted sizes its size stage has just
//! computed this way; copied stale, the title laid structures out with
//! wrong sizes, their rehydration wrote over each other and over the
//! title's streamed objects, and the title crashed on what it then read
//! (F16, `f16-calls2`).
//!
//! When both ranges are title memory the GPU reaches in place (no copy of
//! ours between it and the title's bytes, so a GPU write is the title's
//! write), the copy is a dispatch of a small shader of ours at the
//! recorded point, between two barriers on every shader write: it sees
//! every earlier shader write and every later one sees it. Other copies
//! stay on the CPU, as before. `VICTORY_GPU_COPIES=0` keeps every copy on
//! the CPU.

use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::s;

use crate::host::HostList;

const SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=2, b0), UAV(u0), UAV(u1)"
cbuffer Copy : register(b0) {
    uint words;
    uint first;
};
RWByteAddressBuffer dst : register(u0);
RWByteAddressBuffer src : register(u1);
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint at = first + id.x;
    if (at < words) {
        dst.Store(at * 4, src.Load(at * 4));
    }
}
"#;

/// Words one dispatch copies at most (64 per group, 65,535 groups).
const PER_DISPATCH: u64 = 64 * 65_535;
/// The largest copy made on the GPU; larger ones stay on the CPU.
const MOST: u64 = 256 << 20;

struct Copier {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
}

// SAFETY: D3D12 objects are free-threaded.
unsafe impl Send for Copier {}
// SAFETY: as above.
unsafe impl Sync for Copier {}

/// Whether title copies may run on the GPU (`VICTORY_GPU_COPIES`, on
/// unless `0`).
pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_GPU_COPIES").is_none_or(|v| v != "0"))
}

fn copier() -> Option<&'static Copier> {
    static COPIER: std::sync::OnceLock<Option<Copier>> = std::sync::OnceLock::new();
    COPIER
        .get_or_init(|| {
            let device = &crate::host::host()?.device;
            match make(device) {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!(error = %e, "no copy shader: the title's buffer copies stay on the CPU");
                    None
                }
            }
        })
        .as_ref()
}

fn make(device: &ID3D12Device) -> windows::core::Result<Copier> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source and names live through the call; out pointers ours.
    let compiled = unsafe {
        D3DCompile(SOURCE.as_ptr().cast(), SOURCE.len(), s!("copy"), None, None, s!("main"), s!("cs_5_1"), 0, 0, &mut code, Some(&mut errors))
    };
    if let Err(e) = compiled {
        if let Some(errors) = errors {
            // SAFETY: the blob's own bytes.
            let text = unsafe { std::slice::from_raw_parts(errors.GetBufferPointer().cast::<u8>(), errors.GetBufferSize()) };
            tracing::warn!("copy shader: {}", String::from_utf8_lossy(text));
        }
        return Err(e);
    }
    let code = code.ok_or_else(windows::core::Error::empty)?;
    // SAFETY: the blob's own bytes; the root signature is inside the shader.
    unsafe {
        let bytes = std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize());
        let root: ID3D12RootSignature = device.CreateRootSignature(0, bytes)?;
        let pso = device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
            pRootSignature: std::mem::transmute_copy(&root),
            CS: D3D12_SHADER_BYTECODE { pShaderBytecode: bytes.as_ptr().cast(), BytecodeLength: bytes.len() },
            ..Default::default()
        })?;
        Ok(Copier { root, pso })
    }
}

/// Whether a copy of `len` bytes from `src` to `dst` (title addresses) can
/// be made on the GPU: both 4-byte aligned, not overlapping, not too big.
fn shape_ok(dst: u64, src: u64, len: u64) -> bool {
    len > 0
        && len <= MOST
        && (dst | src | len).is_multiple_of(4)
        && (dst + len <= src || src + len <= dst)
}

/// Records the copy of `len` bytes of title memory from `src` to `dst` on
/// the GPU in `list`, if both ranges are title memory in place; false
/// leaves it to the caller (the CPU at submission). The caller must make
/// the title's next draw or dispatch set its host state again (the
/// copy's pipeline and root signature take its place).
pub(crate) fn record(list: &HostList, dst: u64, src: u64, len: u64) -> bool {
    if !on() || !shape_ok(dst, src, len) {
        return false;
    }
    let (Some(to), Some(from)) = (crate::shadow::find(dst, len), crate::shadow::find(src, len)) else {
        return false;
    };
    if !to.in_place() || !from.in_place() {
        return false;
    }
    let Some(copier) = copier() else { return false };
    // The ranges as the lists of the submission see them, and the write
    // declared (as a root view's are).
    from.refresh(list, src, len);
    to.refresh(list, dst, len);
    to.gpu_writes(list, dst, len);
    let words = len / 4;
    crate::list::uav_barrier_all(list);
    // SAFETY: an open list; our root signature and pipeline; both
    // addresses inside live buffers placed over the title's memory, kept
    // with the list by the refreshes above.
    unsafe {
        list.list.SetComputeRootSignature(&copier.root);
        list.list.SetPipelineState(&copier.pso);
        list.list.SetComputeRootUnorderedAccessView(1, to.gpu(dst));
        list.list.SetComputeRootUnorderedAccessView(2, from.gpu(src));
        let mut first = 0u64;
        while first < words {
            let n = (words - first).min(PER_DISPATCH);
            let constants = [words as u32, first as u32];
            list.list.SetComputeRoot32BitConstants(0, 2, constants.as_ptr().cast(), 0);
            list.list.Dispatch(n.div_ceil(64) as u32, 1, 1);
            first += n;
        }
    }
    crate::list::uav_barrier_all(list);
    #[cfg(feature = "diagnostics")]
    {
        crate::copystat::add(crate::copystat::C::GpuCopies, 1);
        let mut made = MADE.lock().unwrap_or_else(|e| e.into_inner());
        if made.1 < 4096 {
            made.1 += 1;
            made.0.push((dst, src, len, crate::frame::current(), crate::host::moment()));
        }
    }
    true
}

/// Diagnostics: the copies made, for [`report`]: destination, source,
/// bytes, frame, when; and how many were noted.
#[cfg(feature = "diagnostics")]
static MADE: std::sync::Mutex<(Vec<(u64, u64, u64, u64, u64)>, u32)> = std::sync::Mutex::new((Vec::new(), 0));

/// Diagnostics: the copies made, with what their destinations hold now
/// (title memory: after a waited submission, what the copy wrote), to the
/// store trace (`GC` lines).
#[cfg(feature = "diagnostics")]
pub(crate) fn report() {
    if !crate::storetrace::on() {
        return;
    }
    let now = crate::frame::current();
    let made: Vec<_> = {
        let mut m = MADE.lock().unwrap_or_else(|e| e.into_inner());
        let (old, young): (Vec<_>, Vec<_>) = m.0.drain(..).partition(|x| x.3 + 3 <= now);
        m.0 = young;
        old
    };
    let words = |at: u64, len: u64| {
        let mut bytes = vec![0u8; len.min(32) as usize];
        let got = crate::host::read_into(at as *const u8, &mut bytes);
        let w: Vec<String> = bytes
            .chunks(4)
            .map(|c| {
                let mut w = [0u8; 4];
                w[..c.len()].copy_from_slice(c);
                format!("{:08x}", u32::from_le_bytes(w))
            })
            .collect();
        format!("{}{}", if got { "" } else { "unreadable " }, w.join(","))
    };
    for (dst, src, len, frame, _) in made {
        let same = {
            let n = len.min(4096) as usize;
            let (mut a, mut b) = (vec![0u8; n], vec![0u8; n]);
            crate::host::read_into(dst as *const u8, &mut a) && crate::host::read_into(src as *const u8, &mut b) && a == b
        };
        crate::storetrace::note(&format!(
            "GC frame={frame} now={now} dst={dst:#x} src={src:#x} len={len:#x} same_now={same} dst={} src={}",
            words(dst, len),
            words(src, len)
        ));
    }
}

#[cfg(test)]
mod tests {
    /// Copies are made on the GPU only when aligned, apart and not huge.
    #[test]
    fn only_aligned_separate_ranges_go_to_the_gpu() {
        assert!(super::shape_ok(0x1000, 0x2000, 8));
        assert!(!super::shape_ok(0x1000, 0x2000, 0), "empty");
        assert!(!super::shape_ok(0x1002, 0x2000, 8), "unaligned destination");
        assert!(!super::shape_ok(0x1000, 0x2001, 8), "unaligned source");
        assert!(!super::shape_ok(0x1000, 0x2000, 6), "a length off four bytes");
        assert!(!super::shape_ok(0x1000, 0x1004, 8), "overlapping");
        assert!(super::shape_ok(0x1000, 0x1008, 8), "touching is apart");
        assert!(!super::shape_ok(0x1000, 0x2000_0000, super::MOST + 4), "too big");
    }
}
