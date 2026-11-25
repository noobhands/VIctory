//! Diagnostics (`VICTORY_F16_RECORD`): the ray-tracing structure builder's
//! leaf streams on the GPU's own timeline (F16).
//!
//! When the builder's header stage binds a structure at `R`, the stream
//! its leaf stage will read lies at `R` plus the header's dword 13; it is
//! watched from then on in that host list. Before each later dispatch of
//! the builder in the list, a barrier on every shader write and a copy of
//! the watched 16 bytes into a readback ring record what the GPU holds
//! there at that point. Once the frame is two frames old the records are
//! written to the store trace (`WS` lines): which dispatch changed the
//! stream, or that it was unchanged when the leaf stage ran.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC};

use crate::host::HostList;

/// Records in the ring, 16 bytes each.
const SLOTS: u32 = 1 << 18;
/// Streams watched per list at most.
const WATCHED: usize = 32;
/// What a slot holds until its copy has run.
const SENTINEL: u8 = 0xcd;

/// The readback ring: GPU copies land here; the CPU reads them later.
struct Ring {
    buffer: ID3D12Resource,
    data: *const u8,
    next: AtomicU32,
}

// SAFETY: a free-threaded resource and a persistent mapping read only.
unsafe impl Send for Ring {}
// SAFETY: as above.
unsafe impl Sync for Ring {}

/// What a record is: its slot, the frame, the watched address, and the
/// point in the list (`before <program> #n`); the copy it read from is
/// kept alive until the record is reported.
struct Record {
    slot: u32,
    frame: u64,
    at: u64,
    label: String,
    _source: std::sync::Arc<crate::shadow::Shadow>,
}

static RING: std::sync::OnceLock<Option<Ring>> = std::sync::OnceLock::new();
static WATCHES: Mutex<Option<HashMap<usize, Vec<u64>>>> = Mutex::new(None);
static RECORDS: Mutex<Vec<Record>> = Mutex::new(Vec::new());

fn on() -> bool {
    victory_shader::emit::builder_probe() && crate::storetrace::on()
}

fn ring() -> Option<&'static Ring> {
    RING.get_or_init(|| {
        let host = crate::host::host()?;
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: u64::from(SLOTS) * 16,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_UNKNOWN,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            ..Default::default()
        };
        let heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_READBACK, ..Default::default() };
        let mut buffer: Option<ID3D12Resource> = None;
        // SAFETY: a plain committed buffer of ours.
        unsafe { host.device.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, D3D12_RESOURCE_STATE_COPY_DEST, None, &mut buffer) }.ok()?;
        let buffer = buffer?;
        let mut data = std::ptr::null_mut();
        // SAFETY: a readback buffer, mapped for the run.
        unsafe { buffer.Map(0, None, Some(&mut data)) }.ok()?;
        // A slot no copy has reached yet holds the sentinel.
        // SAFETY: the mapping holds the whole ring.
        unsafe { std::ptr::write_bytes(data.cast::<u8>(), SENTINEL, SLOTS as usize * 16) };
        Some(Ring { buffer, data: data.cast_const().cast(), next: AtomicU32::new(0) })
    })
    .as_ref()
}

/// The header stage bound structure `r` in `list`: its leaf stream is
/// watched from here on.
pub(crate) fn header_bound(list: &HostList, r: u64) {
    if !on() {
        return;
    }
    let mut dword = [0u8; 4];
    if !crate::host::read_into((r + 52) as *const u8, &mut dword) {
        return;
    }
    let at = r + u64::from(u32::from_le_bytes(dword));
    let key = std::ptr::from_ref(list) as usize;
    {
        let mut w = WATCHES.lock().unwrap_or_else(|e| e.into_inner());
        let v = w.get_or_insert_with(HashMap::new).entry(key).or_default();
        if v.len() >= WATCHED || v.contains(&at) {
            return;
        }
        v.push(at);
    }
    snapshot_one(list, at, "at the header's bind".to_owned());
}

/// Before a dispatch of the builder's `who` in `list`: every watched
/// stream of the list is recorded.
pub(crate) fn before_dispatch(list: &HostList, who: &str) {
    if !on() {
        return;
    }
    // Records whose copies ran are written out as they come (a crash
    // soon after the builder starts left them all unwritten).
    static CALLS: AtomicU32 = AtomicU32::new(0);
    if CALLS.fetch_add(1, Ordering::Relaxed) % 64 == 63 {
        report();
        report_extents();
    }
    let key = std::ptr::from_ref(list) as usize;
    let watched: Vec<u64> = WATCHES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|w| w.get(&key).cloned()).unwrap_or_default();
    if watched.is_empty() {
        return;
    }
    crate::list::uav_barrier_all(list);
    for at in watched {
        snapshot_one(list, at, format!("before {who}"));
    }
}

/// The list was reset or closed for good: its watches end.
pub(crate) fn list_done(list: &HostList) {
    let key = std::ptr::from_ref(list) as usize;
    if let Some(w) = WATCHES.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        w.remove(&key);
    }
}

fn snapshot_one(list: &HostList, at: u64, label: String) {
    let Some(ring) = ring() else { return };
    let Some(shadow) = crate::shadow::find(at, 16) else { return };
    let slot = ring.next.fetch_add(1, Ordering::Relaxed);
    if slot >= SLOTS {
        return;
    }
    let offset = shadow.offset(at);
    // SAFETY: an open list; both buffers live (the source kept with the
    // record until it is reported).
    unsafe { list.list.CopyBufferRegion(&ring.buffer, u64::from(slot) * 16, &shadow.buffer, offset, 16) };
    RECORDS.lock().unwrap_or_else(|e| e.into_inner()).push(Record { slot, frame: crate::frame::current(), at, label, _source: shadow });
}

/// Writes the records whose copies have run (the slot no longer holds the
/// sentinel) to the store trace; records still waiting stay, up to 600
/// frames.
pub(crate) fn report() {
    let Some(ring) = RING.get().and_then(Option::as_ref) else { return };
    let now = crate::frame::current();
    // SAFETY: inside the mapped ring.
    let slot = |s: u32| unsafe { std::slice::from_raw_parts(ring.data.add(s as usize * 16), 16) };
    let done: Vec<Record> = {
        let mut r = RECORDS.lock().unwrap_or_else(|e| e.into_inner());
        let (ran, waiting): (Vec<Record>, Vec<Record>) =
            r.drain(..).partition(|x| slot(x.slot).iter().any(|&b| b != SENTINEL) || x.frame + 600 <= now);
        *r = waiting;
        ran
    };
    for rec in done {
        let bytes = slot(rec.slot);
        let words: Vec<String> = bytes.chunks(4).map(|c| format!("{:08x}", u32::from_le_bytes([c[0], c[1], c[2], c[3]]))).collect();
        crate::storetrace::note(&format!("WS frame={} at={:#x} {} gpu={}", rec.frame, rec.at, rec.label, words.join(",")));
    }
}

/// Diagnostics: how far the builder's rehydration writes past a structure's
/// start. For each structure its box stage binds (`R`, root view 1), the
/// 32 KiB from `R` are taken when the list is submitted and compared after a
/// waited submission: the changed bytes' offsets from `R` (`BX` lines).
struct Extent {
    at: u64,
    frame: u64,
    /// The bytes at submission, and when (`crate::host::moment`).
    before: std::sync::Arc<Mutex<Option<(Vec<u8>, std::time::Instant)>>>,
    stages: String,
}



static EXTENTS: Mutex<Vec<Extent>> = Mutex::new(Vec::new());
const EXTENT: usize = 32 << 10;

/// The box stage bound structure `r` in `list` (`what`: its constants and
/// groups, for the line).
pub(crate) fn box_bound(list: &HostList, r: u64, what: String) {
    if !on() {
        return;
    }
    let mut e = EXTENTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(x) = e.iter_mut().find(|x| x.at == r && x.frame == crate::frame::current()) {
        if x.stages.len() < 400 {
            x.stages.push_str(&what);
        }
        return;
    }
    if e.len() >= 2048 {
        return;
    }
    let before: std::sync::Arc<Mutex<Option<(Vec<u8>, std::time::Instant)>>> = std::sync::Arc::default();
    let into = before.clone();
    list.defer_host(move || {
        let mut bytes = vec![0u8; EXTENT];
        if crate::host::read_into(r as *const u8, &mut bytes) {
            *into.lock().unwrap_or_else(|e| e.into_inner()) = Some((bytes, std::time::Instant::now()));
        }
    });
    e.push(Extent { at: r, frame: crate::frame::current(), before, stages: what });
}

/// After a waited submission: the structures whose snapshot was taken and
/// whose frame has passed, compared.
pub(crate) fn report_extents() {
    // Submissions are waited for: 100 ms after the snapshot, the list has run.
    let done: Vec<Extent> = {
        let mut e = EXTENTS.lock().unwrap_or_else(|e| e.into_inner());
        let (old, young): (Vec<Extent>, Vec<Extent>) = e
            .drain(..)
            .partition(|x| x.before.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some());
        *e = young;
        old
    };
    for x in done {
        let Some((before, _)) = x.before.lock().unwrap_or_else(|e| e.into_inner()).take() else { continue };
        let mut after = vec![0u8; EXTENT];
        if !crate::host::read_into(x.at as *const u8, &mut after) {
            continue;
        }
        let changed: Vec<usize> = (0..EXTENT).filter(|&i| before[i] != after[i]).collect();
        let (lo, hi) = (changed.first().copied().unwrap_or(0), changed.last().map_or(0, |h| h + 1));
        // Changed runs, coarsely: 256-byte blocks with a change.
        let mut blocks: Vec<usize> = changed.iter().map(|i| i / 256).collect();
        blocks.dedup();
        crate::storetrace::note(&format!(
            "BX R={:#x} frame={} changed={} range=[{lo:#x},{hi:#x}) blocks256={:x?} stages={}",
            x.at,
            x.frame,
            changed.len(),
            &blocks[..blocks.len().min(48)],
            x.stages
        ));
    }
}
