//! Title memory as the card's own memory: a host heap opened on memory
//! the title already has, and a buffer placed in it, so that shaders read
//! and write the title's bytes themselves and no host copy stands between
//! (the console's one memory). This file holds the two openings Direct3D
//! has, a probe that says what this PC's driver makes of them
//! (`VICTORY_INPLACE_PROBE`), and a census of which title memory could be
//! opened.
//!
//! What the Direct3D runtime and the kernel answer, measured on this PC
//! with the software adapter (the rules are theirs, not the adapter's;
//! what the card's driver adds is what the probe is for):
//!
//! - `OpenExistingHeapFromAddress` takes the base address of a whole
//!   `VirtualAlloc` allocation and nothing inside it; every page must be
//!   committed and have one protection. Opening locks every page into the
//!   PC's memory at once (256 MiB never touched: 70 ms and 256 MB of
//!   working set).
//! - `OpenExistingHeapFromAddress1` (`ID3D12Device13`) takes any committed
//!   part of an allocation, with the same rule for its pages; parts may
//!   overlap, and the whole may be opened beside them. It also takes a
//!   committed view of a section.
//! - `OpenExistingHeapFromFileMapping` takes a section made with
//!   `SEC_COMMIT` and refuses one made with `SEC_RESERVE` (how `xmem`
//!   makes the title's page pool today). The heap is the section, not a
//!   view of it: views may be unmapped, mapped elsewhere or made
//!   read-only, and the section's handle closed, with the heap alive.
//!   Opening it locks nothing (256 MiB: 0.2 ms, no working set).
//! - The heap is a custom heap in the PC's memory (write-back pages, pool
//!   L0), shared and cross-adapter shared, 64 KiB aligned. A buffer in it
//!   must have `ALLOW_CROSS_ADAPTER`; unordered access beside it is taken.
//!   A texture must be row-major with one level. Tile mappings into such
//!   a heap are refused and REMOVE THE DEVICE: never call them.
//! - While a heap opened from an address stands, the title cannot
//!   decommit, release or protect the pages in it (`VirtualFree` and
//!   `VirtualProtect` fail with error 87), nor unmap a view in it. Pages
//!   outside the opened part are free to change.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::{Interface, s};
use windows_sys::Win32::System::Memory as mem;

/// The unit heaps and placed buffers are cut on, and the console's page.
const K64: usize = 64 << 10;
/// The host's write watching (not in windows-sys' memory module).
const MEM_WRITE_WATCH: u32 = 0x20_0000;

/// `VICTORY_INPLACE_PROBE`: 0 (unset) nothing; 1 the probe on memory of
/// its own at the first sweep, and the census 30 and 150 seconds later;
/// 2 also opens one part of the title's own memory for a moment at the
/// first census.
pub(crate) fn probe_level() -> u8 {
    static LEVEL: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(|| std::env::var("VICTORY_INPLACE_PROBE").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0))
}

/// `VICTORY_INPLACE`: 0 (unset) off. 1: a window of title memory outside
/// buffers that lies in private memory committed whole is not copied: a
/// part of the title's allocation is opened as a host heap and the window
/// is a buffer placed over it ([`place_for`]). 2: title buffers too,
/// except the big ones in the card's memory. 3: the big ones too (in
/// place of the card's memory: the card reads them over the bus, the CPU
/// no longer refreshes, compares and copies them back).
///
/// Private memory (`XMemVirtualAlloc`'s) is opened by part. View memory
/// (the page pool) is placed when the runtime made its section committed
/// (`VICTORY_XMEM_COMMIT=1`) and the range is one run of it; when the
/// title maps other pages there, what was placed is let go
/// ([`title_memory_freeing`]).
///
/// Shader writes through such a buffer are still declared to the flights
/// sent ahead (`VICTORY_LATE_WAIT`; `Shadow::gpu_writes`), with nothing to
/// copy back: a read of ours of that memory waits for them, as for a copy.
///
/// shortcut: while a window is in place the title cannot decommit, free
/// or protect its pages (the call fails with error 87; the probe shows
/// it). The title's heaps are allocated once and kept, and nothing of the
/// kind was seen; [`title_memory_freeing`] is there for the runtime to
/// call before such a call once it takes them over.
pub(crate) fn level() -> u8 {
    static LEVEL: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(|| std::env::var("VICTORY_INPLACE").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0))
}

/// The most title memory in place at once, in bytes
/// (`VICTORY_INPLACE_MB`, 4096): opening locks the pages into the PC's
/// memory, also those the title never touched.
fn most() -> u64 {
    static MOST: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *MOST.get_or_init(|| std::env::var("VICTORY_INPLACE_MB").ok().and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(4096) << 20)
}

/// In place now: windows and their bytes, title buffers and their bytes;
/// then, since the start: asks turned down because the memory is a view
/// or not private memory committed whole, because the host refused,
/// because of [`most`]; what was let go because its memory changed; and
/// of all placed, those in view memory (a section's heap) and their bytes.
static STATS: [AtomicU64; 10] = [const { AtomicU64::new(0) }; 10];

/// A buffer placed over title memory, and its heap.
pub(crate) struct Placed {
    pub(crate) buffer: ID3D12Resource,
    pub(crate) heap: ID3D12Heap,
    /// The host GPU address of the title memory's first byte.
    pub(crate) gpu: u64,
    /// Bytes from the buffer's first byte to the title memory's first.
    pub(crate) bias: u64,
}

/// Opens the 64 KiB pages that hold title memory `[start, start + len)`
/// as a heap of their own and places one buffer over all of them. `Err`
/// with nothing if the memory is not private memory committed whole
/// (nothing was asked of the host), with the host's error if it refused.
pub(crate) fn place_part(device: &ID3D12Device, start: u64, len: u64) -> Result<Placed, Option<windows::core::Error>> {
    let at = start & !(K64 as u64 - 1);
    let bytes = (start + len.max(1)).next_multiple_of(K64 as u64) - at;
    if !private_whole(at as usize, bytes as usize) {
        return Err(None);
    }
    let heap = open(device, From::Part(at as usize, bytes as usize)).map_err(Some)?;
    let buffer = place(device, &heap, 0, bytes).map_err(Some)?;
    // SAFETY: a live buffer.
    let gpu = unsafe { buffer.GetGPUVirtualAddress() } + (start - at);
    Ok(Placed { buffer, heap, gpu, bias: start - at })
}

/// Title memory `[start, start + len)` in place, for a window outside
/// buffers or a title buffer, if [`level`] asks for it and the memory can
/// be opened; `None` leaves the caller to make a copy, as before.
///
/// View memory (the page pool) is placed when it is one run of a section
/// the runtime made committed (`VICTORY_XMEM_COMMIT=1`): the section is
/// opened as one heap, once, and the buffer placed at the run's offset
/// ([`place_view`]). Opening a section locks nothing, so [`most`] does not
/// count it. Scattered pages, or a run with unmapped pages, stay copies.
pub(crate) fn place_for(start: u64, len: u64, window: bool) -> Option<Placed> {
    if level() < if window { 1 } else { 2 } {
        return None;
    }
    // View memory: known from the runtime without asking Windows.
    let section = match cover(start as usize, len as usize) {
        Cover::None => None,
        Cover::Run(section, offset) => match section_bytes(section) {
            Some(bytes) => Some((section, bytes, offset)),
            None => {
                STATS[4].fetch_add(1, Ordering::Relaxed);
                return None;
            }
        },
        Cover::Scattered | Cover::Holes => {
            STATS[4].fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };
    if section.is_none() && STATS[1].load(Ordering::Relaxed) + STATS[3].load(Ordering::Relaxed) + len > most() {
        STATS[6].fetch_add(1, Ordering::Relaxed);
        return None;
    }
    let host = crate::host::host()?;
    let placed = match section {
        Some((section, bytes, offset)) => place_view(&host.device, section, bytes, offset, len),
        None => place_part(&host.device, start, len),
    };
    match placed {
        Ok(placed) => {
            let at = if window { 0 } else { 2 };
            STATS[at].fetch_add(1, Ordering::Relaxed);
            STATS[at + 1].fetch_add(len, Ordering::Relaxed);
            if section.is_some() {
                STATS[8].fetch_add(1, Ordering::Relaxed);
                STATS[9].fetch_add(len, Ordering::Relaxed);
            }
            Some(placed)
        }
        Err(None) => {
            STATS[4].fetch_add(1, Ordering::Relaxed);
            None
        }
        Err(Some(e)) => {
            if STATS[5].fetch_add(1, Ordering::Relaxed) < 8 {
                tracing::warn!(start = format_args!("{start:#x}"), len, error = %e, "title memory not opened in place: a copy instead");
            }
            None
        }
    }
}

/// The title's own allocations of private memory (base, bytes), as the
/// runtime makes them (`XMemVirtualAlloc`, and the title's `VirtualAlloc`):
/// only these are opened for the shaders that reach title memory by
/// address, never memory of ours or the driver's.
static ALLOCATIONS: Mutex<BTreeMap<usize, usize>> = Mutex::new(BTreeMap::new());

/// The title reserved or committed `[base, base + size)` (a new
/// allocation, or pages of one).
pub fn title_memory_allocated(base: usize, size: usize) {
    if base == 0 || size == 0 {
        return;
    }
    let mut a = ALLOCATIONS.lock().unwrap_or_else(|e| e.into_inner());
    // Pages committed inside an allocation already known add nothing.
    if a.range(..=base).next_back().is_some_and(|(&b, &s)| base + size <= b + s) {
        return;
    }
    a.insert(base, size);
    // The directory of title memory by address reaches 128 GiB
    // (`victory_shader::emit::MEMORY_PAGES` pages of 64 KiB): an
    // allocation past it is memory those shaders cannot reach.
    let reach = u64::from(victory_shader::emit::MEMORY_PAGES) << 16;
    if (base + size) as u64 > reach || size >= 256 << 20 {
        tracing::info!(
            base = format_args!("{base:#x}"),
            mb = size >> 20,
            past_the_directory = (base + size) as u64 > reach,
            "a title allocation"
        );
    }
}

/// The title released the allocation at `base`.
pub fn title_memory_released(base: usize) {
    ALLOCATIONS.lock().unwrap_or_else(|e| e.into_inner()).remove(&base);
}

/// The title's allocations (base, bytes), in address order.
pub(crate) fn title_allocations() -> Vec<(usize, usize)> {
    ALLOCATIONS.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(&b, &s)| (b, s)).collect()
}

/// The run of private, committed, read-write pages around `address`, if
/// it is in a title allocation: (start, end), whole 64 KiB pages.
pub(crate) fn private_run(address: usize) -> Option<(usize, usize)> {
    let (base, size) = {
        let a = ALLOCATIONS.lock().unwrap_or_else(|e| e.into_inner());
        a.range(..=address).next_back().map(|(&b, &s)| (b, s)).filter(|&(b, s)| address < b + s)?
    };
    // SAFETY: plain data filled in by the query.
    let mut info: mem::MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { mem::VirtualQuery(address as *const c_void, &mut info, size_of_val(&info)) };
    if n == 0 || info.State != mem::MEM_COMMIT || info.Type != mem::MEM_PRIVATE || info.Protect != mem::PAGE_READWRITE {
        return None;
    }
    let start = (info.BaseAddress as usize).max(base).next_multiple_of(K64);
    let end = (info.BaseAddress as usize + info.RegionSize).min(base + size) & !(K64 - 1);
    (start <= address && address < end).then_some((start, end))
}

/// Opens `[start, start + len)` (private, committed whole, 64 KiB lines)
/// for the shaders that reach title memory by address, counted as a title
/// buffer in place.
pub(crate) fn place_by_address(start: u64, len: u64) -> Option<Placed> {
    let host = crate::host::host()?;
    match place_part(&host.device, start, len) {
        Ok(placed) => {
            STATS[2].fetch_add(1, Ordering::Relaxed);
            STATS[3].fetch_add(len, Ordering::Relaxed);
            Some(placed)
        }
        Err(e) => {
            if STATS[5].fetch_add(1, Ordering::Relaxed) < 8 {
                tracing::warn!(start = format_args!("{start:#x}"), len, error = ?e, "title memory not opened for the shaders that reach it by address");
            }
            None
        }
    }
}

/// The bytes of `section` if the runtime made it committed (only such a
/// section opens as a heap), else `None`.
fn section_bytes(section: usize) -> Option<u64> {
    views().sections.iter().find(|s| s.0 == section && s.2).map(|s| s.1)
}

/// Where `len` bytes at `offset` of a section of `section_bytes` go in its
/// heap: the 64 KiB line at or below the first byte, the bytes from there
/// to the line at or above the last (a page at least), and the first
/// byte's distance from the line. `None` if that reaches past the section.
fn view_part(section_bytes: u64, offset: u64, len: u64) -> Option<(u64, u64, u64)> {
    let k = K64 as u64;
    let at = offset & !(k - 1);
    let end = offset.checked_add(len.max(1))?.next_multiple_of(k);
    (end <= section_bytes).then_some((at, end - at, offset - at))
}

/// Heaps opened on the title's committed sections, by the section's
/// handle; `None` where the host refused (not asked again).
struct SectionHeaps(Vec<(usize, Option<ID3D12Heap>)>);

// SAFETY: the heaps are free-threaded host objects; all access is under
// the mutex.
unsafe impl Send for SectionHeaps {}

static SECTION_HEAPS: Mutex<SectionHeaps> = Mutex::new(SectionHeaps(Vec::new()));

/// The heap of `section`, opened on first use. `Err` with the host's
/// error the first time it refuses, with nothing after.
fn section_heap(device: &ID3D12Device, section: usize) -> Result<ID3D12Heap, Option<windows::core::Error>> {
    let mut heaps = SECTION_HEAPS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, heap)) = heaps.0.iter().find(|h| h.0 == section) {
        return heap.clone().ok_or(None);
    }
    let opened = open(device, From::Section(section));
    heaps.0.push((section, opened.as_ref().ok().cloned()));
    match &opened {
        Ok(_) => tracing::info!(section = format_args!("{section:#x}"), "a committed section of the title's page pool opened as one heap"),
        Err(e) => tracing::warn!(section = format_args!("{section:#x}"), error = %e, "a committed section of the title's page pool not opened as a heap"),
    }
    opened.map_err(Some)
}

/// Forgets the heap of `section` (the section is gone; the heap lives on
/// in what was placed in it).
pub(crate) fn forget_section_heap(section: usize) {
    SECTION_HEAPS.lock().unwrap_or_else(|e| e.into_inner()).0.retain(|h| h.0 != section);
}

/// The runtime is closing `section`'s handle: its record and heap are
/// forgotten (a later section may get the same handle). What was placed
/// in the heap keeps it, and the section's memory, alive.
pub fn title_memory_section_gone(section: usize) {
    views().sections.retain(|s| s.0 != section);
    mirror().sections.retain(|s| s.0 != section);
    OWN_VIEW_HEAPS.lock().unwrap_or_else(|e| e.into_inner()).0.retain(|k, _| k.0 != section);
    forget_section_heap(section);
}

/// Places `len` bytes at `offset` of `section` (of `bytes`, committed) in
/// the section's heap ([`view_part`]): the title memory a run of the
/// section's pages shows, in place. `Err` with nothing if it reaches past
/// the section, with the host's error if it refused.
pub(crate) fn place_view(device: &ID3D12Device, section: usize, bytes: u64, offset: u64, len: u64) -> Result<Placed, Option<windows::core::Error>> {
    let (at, size, bias) = view_part(bytes, offset, len).ok_or(None)?;
    let heap = section_heap(device, section)?;
    let buffer = place(device, &heap, at, size).map_err(Some)?;
    // SAFETY: a live buffer.
    let gpu = unsafe { buffer.GetGPUVirtualAddress() } + bias;
    Ok(Placed { buffer, heap, gpu, bias })
}

/// What [`place_for`] gave is gone.
pub(crate) fn gone(window: bool, bytes: u64) {
    let at = if window { 0 } else { 2 };
    STATS[at].fetch_sub(1, Ordering::Relaxed);
    STATS[at + 1].fetch_sub(bytes, Ordering::Relaxed);
}

/// Title memory `[at, at + len)` is about to show something else, or the
/// title is about to decommit, free or protect it: what is in place there
/// is let go (the tables forget it; its heap goes when the last list and
/// slot that hold it do). Called for memory the runtime maps anew; for
/// the runtime to call before the title's own `VirtualFree` once it takes
/// that call over.
pub fn title_memory_freeing(at: usize, len: usize) {
    if STATS[0].load(Ordering::Relaxed) + STATS[2].load(Ordering::Relaxed) == 0 {
        return;
    }
    let gone = crate::shadow::in_place_let_go(at as u64, len as u64);
    STATS[7].fetch_add(gone, Ordering::Relaxed);
}

/// The line of what is in place, every ten seconds while [`level`] is on.
fn say() {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = crate::host::now_ms();
    let last = LAST.load(Ordering::Relaxed);
    if now < last + 10_000 || LAST.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_err() {
        return;
    }
    let s: Vec<u64> = STATS.iter().map(|s| s.load(Ordering::Relaxed)).collect();
    tracing::info!(
        "title memory in place: {} windows {} MB, {} title buffers {} MB (no copy, no refresh, no copy-back); turned down: {} view memory or not private memory committed whole, \
         {} refused by the host, {} over VICTORY_INPLACE_MB; {} let go because their memory changed; placed in a section's heap since the start: {} of {} MB",
        s[0],
        s[1] >> 20,
        s[2],
        s[3] >> 20,
        s[4],
        s[5],
        s[6],
        s[7],
        s[8],
        s[9] >> 20
    );
}

/// What a heap is opened on.
pub(crate) enum From {
    /// A whole `VirtualAlloc` allocation, by its base address.
    Region(usize),
    /// A committed part of one: address and bytes.
    Part(usize, usize),
    /// A section made with `SEC_COMMIT`, by its handle.
    Section(usize),
}

/// Opens a heap on memory the process has.
pub(crate) fn open(device: &ID3D12Device, from: From) -> windows::core::Result<ID3D12Heap> {
    // SAFETY: the calls take an address or a handle and check both
    // themselves; a refusal is an error code.
    unsafe {
        match from {
            From::Region(at) => device.cast::<ID3D12Device3>()?.OpenExistingHeapFromAddress(at as *const c_void),
            From::Part(at, bytes) => device.cast::<ID3D12Device13>()?.OpenExistingHeapFromAddress1(at as *const c_void, bytes),
            From::Section(handle) => device.cast::<ID3D12Device3>()?.OpenExistingHeapFromFileMapping(HANDLE(handle as *mut c_void)),
        }
    }
}

fn buffer_desc(size: u64, flags: D3D12_RESOURCE_FLAGS) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: size.max(1),
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: flags,
        ..Default::default()
    }
}

/// A buffer of `size` bytes at `offset` (a multiple of 64 KiB) of an
/// opened heap, which shaders may read and write, in the common state.
pub(crate) fn place(device: &ID3D12Device, heap: &ID3D12Heap, offset: u64, size: u64) -> windows::core::Result<ID3D12Resource> {
    let mut placed: Option<ID3D12Resource> = None;
    // The heap is cross-adapter shared, so the buffer must say so too.
    let flags = D3D12_RESOURCE_FLAG_ALLOW_CROSS_ADAPTER | D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
    // SAFETY: plain creation in a live heap.
    unsafe { device.CreatePlacedResource(heap, offset, &buffer_desc(size, flags), D3D12_RESOURCE_STATE_COMMON, None, &mut placed)? };
    placed.ok_or_else(windows::core::Error::empty)
}

// The title's views of its physical pages, as the runtime maps them: the
// census needs them to say which view memory is one run of a section (one
// placed buffer could show it) and which is scattered pages.

struct Views {
    /// By address: bytes, the section's handle, the offset in it.
    views: BTreeMap<usize, (usize, usize, u64)>,
    /// Sections by handle: bytes, and whether made committed.
    sections: Vec<(usize, u64, bool)>,
}

static VIEWS: Mutex<Views> = Mutex::new(Views { views: BTreeMap::new(), sections: Vec::new() });

fn views() -> std::sync::MutexGuard<'static, Views> {
    VIEWS.lock().unwrap_or_else(|e| e.into_inner())
}

/// The views and sections again, behind a lock under which nothing else is
/// taken: for [`view_run`], which `shadow::find` asks with the memory
/// directory's lock held (the views' own lock is taken before that one).
static MIRROR: std::sync::RwLock<Views> = std::sync::RwLock::new(Views { views: BTreeMap::new(), sections: Vec::new() });

fn mirror() -> std::sync::RwLockWriteGuard<'static, Views> {
    MIRROR.write().unwrap_or_else(|e| e.into_inner())
}

/// The run of the title's views of one committed section that `at` lies
/// in, from the view holding `at` on, at most `most` bytes past `at`:
/// (first address, bytes, section, the section's bytes, offset of the
/// first address in it). `None` if no view holds `at` or its section was
/// not made committed.
pub(crate) fn view_run(at: usize, most: usize) -> Option<(usize, usize, usize, u64, u64)> {
    let m = MIRROR.read().unwrap_or_else(|e| e.into_inner());
    let (&base, &(bytes, section, offset)) = m.views.range(..=at).next_back().filter(|(b, v)| **b + v.0 > at)?;
    let section_bytes = m.sections.iter().find(|s| s.0 == section && s.2)?.1;
    let mut reached = base + bytes;
    while reached < at.saturating_add(most) {
        match m.views.get(&reached) {
            Some(&(n, s, o)) if s == section && o == offset + (reached - base) as u64 => reached += n,
            _ => break,
        }
    }
    Some((base, reached - base, section, section_bytes, offset))
}

/// Diagnostics: the 64 KiB page of title memory holding `at` against the
/// section page the views say is mapped there (what a heap placed over it
/// shows the GPU): how many of its bytes differ, and the 16 bytes at `at`
/// in the section. `None` if no view holds `at`, or the page cannot be
/// read or mapped.
#[cfg(feature = "diagnostics")]
pub(crate) fn section_page_diff(at: usize) -> Option<(usize, [u8; 16])> {
    let (base, _, section, _, offset) = view_run(at, 1)?;
    let in_section = offset + (at - base) as u64;
    let page = in_section & !(K64 as u64 - 1);
    let mut title = vec![0u8; K64];
    if !crate::host::read_into((at & !(K64 - 1)) as *const u8, &mut title) {
        return None;
    }
    // SAFETY: a read-only view of a section the runtime keeps open,
    // unmapped below.
    let view = unsafe { mem::MapViewOfFile(section as windows_sys::Win32::Foundation::HANDLE, mem::FILE_MAP_READ, (page >> 32) as u32, page as u32, K64) };
    if view.Value.is_null() {
        return None;
    }
    // SAFETY: the view holds 64 KiB.
    let ours = unsafe { std::slice::from_raw_parts(view.Value.cast::<u8>(), K64) };
    let differ = title.iter().zip(ours).filter(|(a, b)| a != b).count();
    let within = (in_section - page) as usize;
    let mut bytes = [0u8; 16];
    let n = 16.min(K64 - within);
    bytes[..n].copy_from_slice(&ours[within..within + n]);
    // SAFETY: our view.
    unsafe { mem::UnmapViewOfFile(view) };
    Some((differ, bytes))
}

/// Heaps opened through views of our own on parts of sections that will
/// not open whole ([`view_heap`]), by section, offset and bytes: each part
/// is mapped once.
static OWN_VIEW_HEAPS: Mutex<OwnViewHeaps> = Mutex::new(OwnViewHeaps(BTreeMap::new()));

struct OwnViewHeaps(BTreeMap<(usize, u64, u64), (ID3D12Heap, usize)>);

// SAFETY: free-threaded host objects, reached under the mutex.
unsafe impl Send for OwnViewHeaps {}

/// Diagnostics: whether our own view of a part shows the bytes the title's
/// view at `start` shows (the same physical pages), compared now on the
/// CPU; mismatches are counted and the first few told.
#[cfg(feature = "diagnostics")]
fn alias_check(start: u64, len: u64, ours: usize) {
    static CHECKED: AtomicU64 = AtomicU64::new(0);
    static DIFFER: AtomicU64 = AtomicU64::new(0);
    let n = len.min(64 << 10) as usize;
    let mut theirs = vec![0u8; n];
    if !crate::host::read_into(start as *const u8, &mut theirs) {
        return;
    }
    // SAFETY: our view maps at least `len` bytes from `ours`.
    let mine = unsafe { std::slice::from_raw_parts(ours as *const u8, n) };
    let checked = CHECKED.fetch_add(1, Ordering::Relaxed) + 1;
    match theirs.iter().zip(mine).position(|(a, b)| a != b) {
        Some(at) => {
            let d = DIFFER.fetch_add(1, Ordering::Relaxed) + 1;
            if d <= 8 {
                tracing::warn!(start = format_args!("{start:#x}"), at, checked, differ = d, "our view of a section part shows other bytes than the title's view");
            }
        }
        None if checked.is_power_of_two() => {
            tracing::info!(checked, differ = DIFFER.load(Ordering::Relaxed), "our views of section parts against the title's views");
        }
        None => {}
    }
}

/// `[start, start + len)` of the title's view memory, `offset` bytes into
/// committed `section` (of `section_bytes`), in place: a buffer placed in
/// the section's heap, or, where the host will not open the section whole,
/// in a heap opened on a view of our own of the part.
pub(crate) fn place_view_run(start: u64, len: u64, section: usize, section_bytes: u64, offset: u64) -> Option<Placed> {
    let host = crate::host::host()?;
    if let Ok(placed) = place_view(&host.device, section, section_bytes, offset, len) {
        return Some(placed);
    }
    let (at, size, bias) = view_part(section_bytes, offset, len)?;
    let heap = {
        let mut heaps = OWN_VIEW_HEAPS.lock().unwrap_or_else(|e| e.into_inner());
        match heaps.0.get(&(section, at, size)) {
            Some((heap, _)) => heap.clone(),
            None => {
                let (heap, view) = view_heap(&host.device, section, at, size)
                    .inspect_err(|e| tracing::warn!(section = format_args!("{section:#x}"), at, size, error = %e, "a part of a section not opened through a view of our own"))
                    .ok()?;
                #[cfg(feature = "diagnostics")]
                alias_check(start, len, view + bias as usize);
                heaps.0.insert((section, at, size), (heap.clone(), view));
                heap
            }
        }
    };
    let buffer = place(&host.device, &heap, 0, size)
        .inspect_err(|e| tracing::warn!(start = format_args!("{start:#x}"), size, error = %e, "view memory not placed in place"))
        .ok()?;
    // SAFETY: a live buffer.
    let gpu = unsafe { buffer.GetGPUVirtualAddress() } + bias;
    Some(Placed { buffer, heap, gpu, bias })
}

/// The runtime made a section of `bytes` for physical pages; `committed`
/// if with `SEC_COMMIT` (only such a section can be opened as a heap).
pub fn title_memory_section(section: usize, bytes: u64, committed: bool) {
    let mut v = views();
    v.sections.retain(|s| s.0 != section);
    v.sections.push((section, bytes, committed));
    let mut m = mirror();
    m.sections.retain(|s| s.0 != section);
    m.sections.push((section, bytes, committed));
}

/// The runtime mapped `[at, at + len)` as a view of `section` from
/// `offset` on. Call after [`crate::title_memory_remapped`] of the range.
pub fn title_memory_view(at: usize, len: usize, section: usize, offset: u64) {
    #[cfg(feature = "diagnostics")]
    if crate::storetrace::on() && victory_shader::emit::builder_probe() {
        crate::storetrace::note(&format!("VM {at:#x}+{len:#x} section={section:#x} offset={offset:#x}"));
    }
    let mut v = views();
    v.views.insert(at, (len, section, offset));
    mirror().views.insert(at, (len, section, offset));
    // Under the views' lock, as the directory's first walk (`with_views`).
    crate::memory::view_mapped(at, len, section, offset);
    drop(v);
    // The guards: every view of a guarded page is guarded with it.
    crate::watch::view_mapped(at, len, section, offset);
}

/// The committed sections the runtime told of: handle and bytes.
pub(crate) fn committed_sections() -> Vec<(usize, u64)> {
    views().sections.iter().filter(|s| s.2).map(|s| (s.0, s.1)).collect()
}

/// A buffer over `[from, from + len)` of committed `section`, placed in
/// the section's heap (opened on first use); `None` if the host refuses.
///
/// A section the host will not open whole (the title's 6.2 GB pool:
/// E_INVALIDARG on the RTX 4080, where 710 and 185 MB sections open) is
/// reached through a view of our own of the part, opened as a heap by
/// address (`OpenExistingHeapFromAddress1` takes a committed view of a
/// section). The view is ours and stays mapped for the run: the title's
/// own views of those pages map and unmap as they please beside it.
pub(crate) fn section_part(section: usize, from: u64, len: u64) -> Option<ID3D12Resource> {
    let host = crate::host::host()?;
    let placed = match section_heap(&host.device, section) {
        Ok(heap) => place(&host.device, &heap, from, len),
        Err(_) => view_heap(&host.device, section, from, len).and_then(|(heap, _)| place(&host.device, &heap, 0, len)),
    };
    match placed {
        Ok(buffer) => Some(buffer),
        Err(e) => {
            tracing::warn!(section = format_args!("{section:#x}"), from, len, error = %e, "no buffer placed in a section's heap");
            None
        }
    }
}

/// A view of our own of `[from, from + len)` of `section` (`from` a
/// multiple of 64 KiB), opened as a heap; the view is never unmapped.
fn view_heap(device: &ID3D12Device, section: usize, from: u64, len: u64) -> windows::core::Result<(ID3D12Heap, usize)> {
    // SAFETY: maps a view of a section the runtime keeps open; failing is
    // a null address.
    let view = unsafe {
        mem::MapViewOfFile(section as windows_sys::Win32::Foundation::HANDLE, mem::FILE_MAP_ALL_ACCESS, (from >> 32) as u32, from as u32, len as usize)
    };
    if view.Value.is_null() {
        return Err(windows::core::Error::from_thread());
    }
    let heap = open(device, From::Part(view.Value as usize, len as usize));
    if heap.is_ok() {
        tracing::info!(section = format_args!("{section:#x}"), from, len, "a part of a section opened as a heap through a view of our own");
    }
    heap.map(|h| (h, view.Value as usize))
}

/// Calls `f` with each of the title's views (address, bytes, section,
/// offset) under the views' lock, so that no mapping changes meanwhile;
/// returns the sum of what `f` returned.
pub(crate) fn with_views(mut f: impl FnMut(usize, usize, usize, u64) -> usize) -> usize {
    let v = views();
    v.views.iter().map(|(&at, &(len, section, offset))| f(at, len, section, offset)).sum()
}

/// What `[at, at + len)` showed is gone ([`crate::title_memory_remapped`]):
/// the views that touch it are forgotten (the runtime tells again those it
/// maps anew).
pub(crate) fn views_gone(at: usize, len: usize) {
    let mut v = views();
    // The directory's region entries there go too (`memory::view_unmapped`).
    crate::memory::view_unmapped(at, len);
    crate::watch::view_gone(at, len);
    if v.views.is_empty() {
        return;
    }
    let end = at.saturating_add(len);
    let first = v.views.range(..=at).next_back().filter(|(base, view)| *base + view.0 > at).map(|(base, _)| *base);
    let gone: Vec<usize> = first.into_iter().chain(v.views.range(at.saturating_add(1)..end).map(|(base, _)| *base)).collect();
    let mut m = mirror();
    for base in gone {
        v.views.remove(&base);
        m.views.remove(&base);
    }
    drop(m);
    drop(v);
    crate::shadow::in_place_views_gone(at as u64, len as u64);
}

/// How the views cover `[at, at + len)`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum Cover {
    /// No view holds its first byte.
    None,
    /// Views hold a part of it only.
    Holes,
    /// Views hold all of it, from more than one place.
    Scattered,
    /// All of it is one run of one section: the section and the offset of
    /// `at` in it.
    Run(usize, u64),
}

fn cover_in(views: &BTreeMap<usize, (usize, usize, u64)>, at: usize, len: usize) -> Cover {
    let end = at + len.max(1);
    let Some((&base, &(bytes, section, offset))) = views.range(..=at).next_back().filter(|(base, view)| *base + view.0 > at) else {
        return Cover::None;
    };
    let first = (section, offset + (at - base) as u64);
    let (mut reached, mut one) = (base + bytes, true);
    while reached < end {
        let Some(&(bytes, s, o)) = views.get(&reached) else {
            return Cover::Holes;
        };
        one &= s == first.0 && o == first.1 + (reached - at) as u64;
        reached += bytes;
    }
    if one { Cover::Run(first.0, first.1) } else { Cover::Scattered }
}

/// [`Cover`] of `[at, at + len)` by the title's views as they are now.
pub(crate) fn cover(at: usize, len: usize) -> Cover {
    cover_in(&views().views, at, len)
}

/// Whether `[at, at + len)` is private memory, all committed and
/// read-write: what [`From::Part`] takes. One question to Windows (it
/// walks the page tables to the end of the pages that share the first
/// one's state: a millisecond in the title's big blocks, so ask when a
/// copy is made, not when it is used).
pub(crate) fn private_whole(at: usize, len: usize) -> bool {
    // SAFETY: plain data filled in by the query.
    let mut info: mem::MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { mem::VirtualQuery(at as *const c_void, &mut info, size_of_val(&info)) };
    n != 0
        && info.State == mem::MEM_COMMIT
        && info.Type == mem::MEM_PRIVATE
        && info.Protect == mem::PAGE_READWRITE
        && (info.BaseAddress as usize).saturating_add(info.RegionSize) >= at.saturating_add(len)
}

fn hr<T>(result: &windows::core::Result<T>) -> String {
    match result {
        Ok(_) => "ok".to_owned(),
        Err(e) => format!("REFUSED {:#010x} ({})", e.code().0, e.message().trim()),
    }
}

/// A queue, a list and a fence of the probe's own on `device`: nothing of
/// the host's submissions is touched.
struct Rig {
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    done: u64,
    event: HANDLE,
}

impl Drop for Rig {
    fn drop(&mut self) {
        // SAFETY: our event.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.event);
        }
    }
}

impl Rig {
    fn new(device: &ID3D12Device) -> windows::core::Result<Rig> {
        // SAFETY: plain creation of objects of our own.
        unsafe {
            let queue: ID3D12CommandQueue =
                device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })?;
            let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
            let list: ID3D12GraphicsCommandList = device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)?;
            list.Close()?;
            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let event = CreateEventW(None, false, false, None)?;
            Ok(Rig { device: device.clone(), queue, allocator, list, fence, done: 0, event })
        }
    }

    /// Records with `record`, runs it and waits (ten seconds at most).
    fn run(&mut self, record: impl FnOnce(&ID3D12GraphicsCommandList)) -> windows::core::Result<()> {
        // SAFETY: our own list and queue, one run at a time.
        unsafe {
            self.allocator.Reset()?;
            self.list.Reset(&self.allocator, None)?;
            record(&self.list);
            self.list.Close()?;
            self.queue.ExecuteCommandLists(&[Some(self.list.cast()?)]);
            self.done += 1;
            self.queue.Signal(&self.fence, self.done)?;
            self.fence.SetEventOnCompletion(self.done, self.event)?;
            if WaitForSingleObject(self.event, 10_000).0 != 0 {
                return Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL));
            }
            self.device.GetDeviceRemovedReason()
        }
    }

    fn committed(&self, kind: D3D12_HEAP_TYPE, size: u64, state: D3D12_RESOURCE_STATES) -> windows::core::Result<ID3D12Resource> {
        let heap = D3D12_HEAP_PROPERTIES { Type: kind, ..Default::default() };
        let mut buffer: Option<ID3D12Resource> = None;
        // SAFETY: plain creation.
        unsafe {
            self.device
                .CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &buffer_desc(size, D3D12_RESOURCE_FLAG_NONE), state, None, &mut buffer)?
        };
        buffer.ok_or_else(windows::core::Error::empty)
    }

    /// What the GPU reads of `size` bytes of `resource` from `offset`.
    fn fetch(&mut self, resource: &ID3D12Resource, offset: u64, size: usize) -> windows::core::Result<Vec<u8>> {
        let back = self.committed(D3D12_HEAP_TYPE_READBACK, size as u64, D3D12_RESOURCE_STATE_COPY_DEST)?;
        // SAFETY: two live buffers, the range inside both.
        self.run(|list| unsafe { list.CopyBufferRegion(&back, 0, resource, offset, size as u64) })?;
        let mut data = std::ptr::null_mut();
        // SAFETY: the mapping holds `size` bytes until unmapped.
        unsafe {
            back.Map(0, None, Some(&mut data))?;
            let bytes = std::slice::from_raw_parts(data.cast::<u8>(), size).to_vec();
            back.Unmap(0, None);
            Ok(bytes)
        }
    }

    /// The GPU writes `bytes` at `offset` of `resource` (a copy).
    fn store(&mut self, resource: &ID3D12Resource, offset: u64, bytes: &[u8]) -> windows::core::Result<()> {
        let up = self.committed(D3D12_HEAP_TYPE_UPLOAD, bytes.len() as u64, D3D12_RESOURCE_STATE_GENERIC_READ)?;
        let mut data = std::ptr::null_mut();
        // SAFETY: the mapping holds the buffer's bytes until unmapped.
        unsafe {
            up.Map(0, None, Some(&mut data))?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data.cast::<u8>(), bytes.len());
            up.Unmap(0, None);
        }
        // SAFETY: two live buffers, the range inside both.
        self.run(|list| unsafe { list.CopyBufferRegion(resource, offset, &up, 0, bytes.len() as u64) })
    }

    /// A compute shader stores, through a root unordered-access view at
    /// `gpu`, the sum of the buffer's first two words in its third.
    fn shade(&mut self, gpu: u64) -> windows::core::Result<()> {
        const SOURCE: &str = "#define RS \"UAV(u0)\"\nRWByteAddressBuffer b : register(u0);\n[RootSignature(RS)]\n[numthreads(1, 1, 1)]\nvoid main() { b.Store(8, b.Load(0) + b.Load(4)); }\n";
        let mut code: Option<ID3DBlob> = None;
        // SAFETY: the source and names live through the call; then the
        // blob's own bytes, which hold the root signature too.
        let (root, pso): (ID3D12RootSignature, ID3D12PipelineState) = unsafe {
            D3DCompile(SOURCE.as_ptr().cast(), SOURCE.len(), s!("inplace"), None, None, s!("main"), s!("cs_5_1"), 0, 0, &mut code, None)?;
            let code = code.ok_or_else(windows::core::Error::empty)?;
            let bytes = std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize());
            let root: ID3D12RootSignature = self.device.CreateRootSignature(0, bytes)?;
            let pso = self.device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE { pShaderBytecode: bytes.as_ptr().cast(), BytecodeLength: bytes.len() },
                ..Default::default()
            })?;
            (root, pso)
        };
        // SAFETY: an open list of ours; the view names a live buffer.
        self.run(|list| unsafe {
            list.SetComputeRootSignature(&root);
            list.SetPipelineState(&pso);
            list.SetComputeRootUnorderedAccessView(0, gpu);
            list.Dispatch(1, 1, 1);
        })
    }

    /// Milliseconds the card takes to copy `bytes` of `from` into its own
    /// memory, the best of three.
    fn copy_ms(&mut self, from: &ID3D12Resource, bytes: u64) -> windows::core::Result<f64> {
        let heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
        let mut to: Option<ID3D12Resource> = None;
        // SAFETY: plain creation.
        unsafe {
            self.device.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &buffer_desc(bytes, D3D12_RESOURCE_FLAG_NONE), D3D12_RESOURCE_STATE_COMMON, None, &mut to)?
        };
        let to = to.ok_or_else(windows::core::Error::empty)?;
        let mut best = f64::MAX;
        for _ in 0..3 {
            let started = std::time::Instant::now();
            // SAFETY: two live buffers of `bytes` bytes or more.
            self.run(|list| unsafe { list.CopyBufferRegion(&to, 0, from, 0, bytes) })?;
            best = best.min(started.elapsed().as_secs_f64() * 1000.0);
        }
        Ok(best)
    }
}

fn fill(at: *mut u8, len: usize, seed: u8) {
    for i in 0..len {
        // SAFETY: the caller's own committed memory of `len` bytes.
        unsafe { at.add(i).write((i as u8).wrapping_mul(31).wrapping_add(seed)) };
    }
}

fn same(at: *const u8, bytes: &[u8]) -> bool {
    // SAFETY: the caller's own committed memory of that length.
    unsafe { std::slice::from_raw_parts(at, bytes.len()) == bytes }
}

/// The process's working set in MB.
fn working_set_mb() -> u64 {
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    let mut counters = PROCESS_MEMORY_COUNTERS { cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
    // SAFETY: our own process and a sized structure.
    unsafe { K32GetProcessMemoryInfo(windows_sys::Win32::System::Threading::GetCurrentProcess(), &mut counters, counters.cb) };
    counters.WorkingSetSize as u64 >> 20
}

/// What the probe found. The lines are the whole account; the flags are
/// what the stages build on.
#[derive(Default)]
pub(crate) struct Probe {
    pub(crate) lines: Vec<String>,
    /// A part of private, write-watched memory opens, a buffer in it shows
    /// the memory's bytes to the GPU, also those the CPU wrote afterwards.
    pub(crate) part_reads: bool,
    /// What the GPU writes there (a copy, and a shader through unordered
    /// access) the CPU reads at the memory's address.
    pub(crate) part_writes: bool,
    /// The same two for a committed section, through a view of it.
    pub(crate) section_reads: bool,
    pub(crate) section_writes: bool,
    /// The section's heap outlives the view: the GPU's write with no view
    /// mapped shows in a view mapped afterwards.
    pub(crate) section_outlives_views: bool,
    /// A section made with `SEC_RESERVE` (as `xmem` makes the page pool)
    /// was refused.
    pub(crate) reserved_section_refused: bool,
    /// A part of a committed section opened through a view of our own
    /// ([`view_heap`], for a section the host will not open whole): the GPU
    /// reads what the CPU writes at the title's view of the same pages,
    /// also later, and the CPU reads there what the GPU wrote.
    pub(crate) own_view_reads: bool,
    pub(crate) own_view_writes: bool,
    /// The same after the title's view went and other pages were mapped
    /// there: the heap still shows the section's pages.
    pub(crate) own_view_outlives_title_view: bool,
}

/// Tries the openings on memory of the probe's own, made the way `xmem`
/// makes title memory, with a queue of its own on `device`; `memory`
/// gives the card's memory account (`Host::video_memory`) where there is
/// one. Nothing of the title's is touched. `mb` is the size of the cost
/// measurement (0 leaves it out).
pub(crate) fn probe(device: &ID3D12Device, memory: &dyn Fn() -> Option<[u64; 4]>, mb: usize) -> Probe {
    let mut p = Probe::default();
    let mut rig = match Rig::new(device) {
        Ok(rig) => rig,
        Err(e) => {
            p.lines.push(format!("no queue of its own: {e}"));
            return p;
        }
    };
    let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS::default();
    // SAFETY: a plain query that fills our structure.
    let _ = unsafe { device.CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS, (&raw mut options).cast(), size_of_val(&options) as u32) };
    p.lines.push(format!(
        "the device: ID3D12Device3 {}, ID3D12Device13 {} (parts of an allocation); cross-adapter row-major textures {}; heap tier {}",
        device.cast::<ID3D12Device3>().is_ok(),
        device.cast::<ID3D12Device13>().is_ok(),
        options.CrossAdapterRowMajorTextureSupported.as_bool(),
        options.ResourceHeapTier.0
    ));
    // SAFETY: memory of the probe's own, made and released here; every
    // address used lies inside it.
    unsafe {
        private(&mut p, &mut rig);
        section(&mut p, &mut rig);
        own_view(&mut p, &mut rig);
        if mb > 0 {
            cost(&mut p, &mut rig, memory, mb);
        }
    }
    p.lines.push(format!(
        "VERDICT: private memory in place: reads {}, writes {}; the page pool's section in place: reads {}, writes {}, whatever the views do {}; \
         a reserved section is refused {} (the pool must be made committed to be opened); a part through a view of our own: reads {}, writes {}, \
         after the title's view is remapped {}",
        p.part_reads, p.part_writes, p.section_reads, p.section_writes, p.section_outlives_views, p.reserved_section_refused,
        p.own_view_reads, p.own_view_writes, p.own_view_outlives_title_view
    ));
    p
}

fn describe(heap: &ID3D12Heap) -> String {
    // SAFETY: a plain query.
    let d = unsafe { heap.GetDesc() };
    format!(
        "heap of {} KiB: type {} (4 = custom), CPU pages {} (3 = write-back), pool {} (1 = the PC's memory), alignment {}, flags {:#x} (0x1 shared, 0x20 cross-adapter, 0x200 write watch, 0x400 shader atomics)",
        d.SizeInBytes >> 10,
        d.Properties.Type.0,
        d.Properties.CPUPageProperty.0,
        d.Properties.MemoryPoolPreference.0,
        d.Alignment,
        d.Flags.0
    )
}

/// Private memory, reserved as `xmem` reserves the title's heaps
/// (committed, read-write, write-watched): a part of it opened.
unsafe fn private(p: &mut Probe, rig: &mut Rig) {
    const SIZE: usize = 64 * K64;
    // SAFETY (the whole function): the allocation below, `SIZE` bytes.
    unsafe {
        let base = mem::VirtualAlloc(std::ptr::null(), SIZE, mem::MEM_RESERVE | mem::MEM_COMMIT | MEM_WRITE_WATCH, mem::PAGE_READWRITE).cast::<u8>();
        if base.is_null() {
            p.lines.push("private memory: none to try with".to_owned());
            return;
        }
        // The part: 16 pages from the 16th on.
        let (at, bytes) = (base.add(16 * K64), 16 * K64);
        let whole = open(&rig.device, From::Region(base as usize));
        p.lines.push(format!("private memory, the whole allocation (OpenExistingHeapFromAddress): {}", hr(&whole)));
        drop(whole);
        let opened = open(&rig.device, From::Part(at as usize, bytes));
        p.lines.push(format!("private memory, a part of it (OpenExistingHeapFromAddress1): {}", hr(&opened)));
        if let Ok(heap) = &opened {
            p.lines.push(format!("  {}", describe(heap)));
            let placed = place(&rig.device, heap, K64 as u64, 4 * K64 as u64);
            p.lines.push(format!("  a buffer placed in it (cross-adapter, unordered access): {}", hr(&placed)));
            if let Ok(buffer) = &placed {
                let memory = at.add(K64);
                fill(memory, 4 * K64, 1);
                let first = rig.fetch(buffer, 0, 4 * K64).is_ok_and(|got| same(memory, &got));
                fill(memory, 4 * K64, 77);
                let second = rig.fetch(buffer, 0, 4 * K64).is_ok_and(|got| same(memory, &got));
                p.part_reads = first && second;
                p.lines.push(format!("  the GPU reads the memory's bytes: {first}; and those the CPU wrote afterwards, with no call in between: {second}"));
                mem::ResetWriteWatch(base.cast(), SIZE);
                let new: Vec<u8> = (0..K64).map(|i| (i as u8) ^ 0x5a).collect();
                let copied = rig.store(buffer, 4096, &new).is_ok() && same(memory.add(4096), &new);
                let (mut found, mut count, mut granularity) = ([0usize; 64], 64usize, 0u32);
                let watched = mem::GetWriteWatch(0, base.cast(), SIZE, found.as_mut_ptr().cast(), &mut count, &mut granularity) == 0;
                memory.cast::<[u32; 3]>().write([40, 2, 0]);
                let shaded = rig.shade(buffer.GetGPUVirtualAddress());
                let shader = shaded.is_ok() && memory.cast::<[u32; 3]>().read()[2] == 42;
                p.part_writes = copied && shader;
                p.lines.push(format!(
                    "  the CPU reads at the memory's address what the GPU wrote: by a copy {copied}; by a shader through unordered access {shader} ({}); \
                     write watching saw {} of the copy's 16 pages{}",
                    hr(&shaded),
                    if watched { count } else { 0 },
                    if watched { "" } else { " (the asking failed)" }
                ));
            }
            // What the title can still do with the pages while the heap stands.
            let mut old = 0;
            let protect_in = mem::VirtualProtect(at.add(8 * K64).cast(), K64, mem::PAGE_READONLY, &mut old) != 0;
            if protect_in {
                mem::VirtualProtect(at.add(8 * K64).cast(), K64, mem::PAGE_READWRITE, &mut old);
            }
            let decommit_in = mem::VirtualFree(at.add(9 * K64).cast(), K64, mem::MEM_DECOMMIT) != 0;
            let decommit_out = mem::VirtualFree(base.add(40 * K64).cast(), K64, mem::MEM_DECOMMIT) != 0;
            p.lines.push(format!(
                "  with the heap standing, the title's own calls: read-only inside the part {protect_in}, decommit inside {decommit_in}, decommit outside {decommit_out} \
                 (false inside means the heap must go before the title frees or guards those pages)"
            ));
            drop(placed);
        }
        drop(opened);
        let freed = mem::VirtualFree(base.cast(), 0, mem::MEM_RELEASE) != 0;
        p.lines.push(format!("  released after the heap went: {freed}"));
    }
}

/// A part of a committed section opened as [`view_heap`] opens one: a view
/// of our own of the part (`MapViewOfFile`), opened by address
/// (`OpenExistingHeapFromAddress1`), beside the title's view of the same
/// pages in a placeholder (as `xmem` maps them). Bytes the CPU writes at
/// the title's view must reach the GPU, now and later; what the GPU writes
/// must show there; and after the title's view goes and other pages are
/// mapped at its address, the heap must still show the section's pages.
unsafe fn own_view(p: &mut Probe, rig: &mut Rig) {
    const SIZE: usize = 32 * K64;
    // SAFETY (the whole function): the section, placeholder and views made
    // below; every address used lies inside them.
    unsafe {
        let process = windows_sys::Win32::System::Threading::GetCurrentProcess();
        let invalid = windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        let handle = mem::CreateFileMappingW(invalid, std::ptr::null(), mem::PAGE_READWRITE | mem::SEC_COMMIT, 0, SIZE as u32, std::ptr::null());
        let hole = mem::VirtualAlloc2(process, std::ptr::null(), 8 * K64, mem::MEM_RESERVE | mem::MEM_RESERVE_PLACEHOLDER, mem::PAGE_NOACCESS, std::ptr::null_mut(), 0).cast::<u8>();
        if handle.is_null() || hole.is_null() {
            p.lines.push("a part through a view of our own: nothing to try with".to_owned());
            return;
        }
        // The title's view: section pages 4..12 in the placeholder.
        let title = mem::MapViewOfFile3(handle, process, hole.cast(), (4 * K64) as u64, 8 * K64, mem::MEM_REPLACE_PLACEHOLDER, mem::PAGE_READWRITE, std::ptr::null_mut(), 0);
        // Ours of the same pages, as `view_heap` maps it.
        let ours = mem::MapViewOfFile(handle, mem::FILE_MAP_ALL_ACCESS, 0, (4 * K64) as u32, 8 * K64);
        if title.Value.is_null() || ours.Value.is_null() {
            p.lines.push("a part through a view of our own: the views were not mapped".to_owned());
            return;
        }
        let opened = open(&rig.device, From::Part(ours.Value as usize, 8 * K64));
        p.lines.push(format!("a part of a committed section through a view of our own (OpenExistingHeapFromAddress1): {}", hr(&opened)));
        if let Ok(heap) = &opened {
            p.lines.push(format!("  {}", describe(heap)));
            let placed = place(&rig.device, heap, 0, 8 * K64 as u64);
            p.lines.push(format!("  a buffer placed in it: {}", hr(&placed)));
            if let Ok(buffer) = &placed {
                let memory = title.Value.cast::<u8>();
                // Written before the GPU first looks, then again after.
                fill(memory.add(K64), 2 * K64, 9);
                let first = rig.fetch(buffer, K64 as u64, 2 * K64).is_ok_and(|got| same(memory.add(K64), &got));
                fill(memory.add(K64), 2 * K64, 99);
                let later = rig.fetch(buffer, K64 as u64, 2 * K64).is_ok_and(|got| same(memory.add(K64), &got));
                p.own_view_reads = first && later;
                let new: Vec<u8> = (0..K64).map(|i| (i as u8) ^ 0x6c).collect();
                let copied = rig.store(buffer, 3 * K64 as u64, &new).is_ok() && same(memory.add(3 * K64), &new);
                memory.add(5 * K64).cast::<[u32; 3]>().write([40, 2, 0]);
                let shaded = rig.shade(buffer.GetGPUVirtualAddress() + 5 * K64 as u64);
                let shader = shaded.is_ok() && memory.add(5 * K64).cast::<[u32; 3]>().read()[2] == 42;
                p.own_view_writes = copied && shader;
                p.lines.push(format!(
                    "  the GPU reads what the CPU wrote at the title's view: first {first}, later {later}; the CPU reads there what the GPU wrote: by a copy {copied}, by a shader {shader} ({})",
                    hr(&shaded)
                ));
                // The title's view goes; section pages 20..28 are mapped at
                // its address; the GPU writes through the heap; the
                // section's pages 4..12 hold it, the new view does not.
                let unmapped = mem::UnmapViewOfFile2(process, title, mem::MEM_PRESERVE_PLACEHOLDER) != 0;
                let other = mem::MapViewOfFile3(handle, process, hole.cast(), (20 * K64) as u64, 8 * K64, mem::MEM_REPLACE_PLACEHOLDER, mem::PAGE_READWRITE, std::ptr::null_mut(), 0);
                let new: Vec<u8> = (0..K64).map(|i| (i as u8) ^ 0x7d).collect();
                let stored = rig.store(buffer, 0, &new).is_ok();
                let in_section = same(ours.Value.cast::<u8>(), &new);
                let not_at_title = !other.Value.is_null() && !same(other.Value.cast::<u8>(), &new);
                p.own_view_outlives_title_view = unmapped && stored && in_section && not_at_title;
                p.lines.push(format!(
                    "  after the title's view went (unmapped {unmapped}) and other pages were mapped there: the GPU's write is in the section's pages {in_section}, not at the title's address {not_at_title}"
                ));
                if !other.Value.is_null() {
                    mem::UnmapViewOfFile2(process, other, 0);
                }
            }
            drop(placed);
        } else {
            mem::UnmapViewOfFile2(process, title, 0);
        }
        drop(opened);
        mem::UnmapViewOfFile(ours);
        mem::VirtualFree(hole.cast(), 0, mem::MEM_RELEASE);
        windows_sys::Win32::Foundation::CloseHandle(handle);
    }
}

/// A section and a view of it in a placeholder, as `xmem` maps physical
/// pages: reserved (as it is made today) and committed.
unsafe fn section(p: &mut Probe, rig: &mut Rig) {
    const SIZE: usize = 32 * K64;
    // SAFETY (the whole function): the sections, placeholder and views
    // made below.
    unsafe {
        let process = windows_sys::Win32::System::Threading::GetCurrentProcess();
        let invalid = windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        let reserved = mem::CreateFileMappingW(invalid, std::ptr::null(), mem::PAGE_READWRITE | mem::SEC_RESERVE, 0, SIZE as u32, std::ptr::null());
        if !reserved.is_null() {
            let opened = open(&rig.device, From::Section(reserved as usize));
            p.reserved_section_refused = opened.is_err();
            p.lines.push(format!("a section made reserved, as the page pool is today (OpenExistingHeapFromFileMapping): {}", hr(&opened)));
            drop(opened);
            windows_sys::Win32::Foundation::CloseHandle(reserved);
        }
        let handle = mem::CreateFileMappingW(invalid, std::ptr::null(), mem::PAGE_READWRITE | mem::SEC_COMMIT, 0, SIZE as u32, std::ptr::null());
        let hole = mem::VirtualAlloc2(process, std::ptr::null(), SIZE, mem::MEM_RESERVE | mem::MEM_RESERVE_PLACEHOLDER, mem::PAGE_NOACCESS, std::ptr::null_mut(), 0).cast::<u8>();
        if handle.is_null() || hole.is_null() {
            p.lines.push("a committed section: none to try with".to_owned());
            return;
        }
        // Pages 4..12 of the section at the placeholder's start.
        mem::VirtualFree(hole.cast(), 8 * K64, mem::MEM_RELEASE | mem::MEM_PRESERVE_PLACEHOLDER);
        let view = mem::MapViewOfFile3(handle, process, hole.cast(), (4 * K64) as u64, 8 * K64, mem::MEM_REPLACE_PLACEHOLDER, mem::PAGE_READWRITE, std::ptr::null_mut(), 0);
        let opened = open(&rig.device, From::Section(handle as usize));
        p.lines.push(format!("a section made committed (OpenExistingHeapFromFileMapping): {}", hr(&opened)));
        let mut unmapped = false;
        if let (Ok(heap), false) = (&opened, view.Value.is_null()) {
            p.lines.push(format!("  {}", describe(heap)));
            // The section's page 5 is the view's second page.
            let placed = place(&rig.device, heap, 5 * K64 as u64, 2 * K64 as u64);
            p.lines.push(format!("  a buffer placed at the section's offset of a view's pages: {}", hr(&placed)));
            if let Ok(buffer) = &placed {
                let memory = hole.add(K64);
                fill(memory, 2 * K64, 3);
                p.section_reads = rig.fetch(buffer, 0, 2 * K64).is_ok_and(|got| same(memory, &got));
                let new: Vec<u8> = (0..K64).map(|i| (i as u8) ^ 0x33).collect();
                p.section_writes = rig.store(buffer, 0, &new).is_ok() && same(memory, &new);
                // The guard of `watch.rs`: the view read-only.
                let mut old = 0;
                let guarded = mem::VirtualProtect(memory.cast(), K64, mem::PAGE_READONLY, &mut old) != 0;
                let new: Vec<u8> = (0..K64).map(|i| (i as u8) ^ 0x44).collect();
                let through = rig.store(buffer, 0, &new).is_ok() && same(memory, &new);
                // The view goes; the GPU writes; another view shows it.
                unmapped = mem::UnmapViewOfFile2(process, view, mem::MEM_PRESERVE_PLACEHOLDER) != 0;
                let new: Vec<u8> = (0..K64).map(|i| (i as u8) ^ 0x55).collect();
                let stored = rig.store(buffer, 0, &new).is_ok();
                let other = mem::MapViewOfFile(handle, mem::FILE_MAP_WRITE, 0, (5 * K64) as u32, K64);
                p.section_outlives_views = unmapped && stored && !other.Value.is_null() && same(other.Value.cast(), &new);
                p.lines.push(format!(
                    "  the GPU reads the view's bytes {}; the CPU reads through the view what the GPU wrote {}; the view made read-only under the heap {guarded}, \
                     and the GPU writes it all the same {through}; the view unmapped under the heap {unmapped}, the GPU's write then shows in a view mapped afterwards {}",
                    p.section_reads, p.section_writes, p.section_outlives_views
                ));
                if !other.Value.is_null() {
                    mem::UnmapViewOfFile(other);
                }
            }
        }
        drop(opened);
        if !unmapped && !view.Value.is_null() {
            mem::UnmapViewOfFile2(process, view, mem::MEM_PRESERVE_PLACEHOLDER);
        }
        windows_sys::Win32::Foundation::CloseHandle(handle);
        // The placeholder was split in two: each half is released.
        mem::VirtualFree(hole.cast(), 0, mem::MEM_RELEASE);
        mem::VirtualFree(hole.add(8 * K64).cast(), 0, mem::MEM_RELEASE);
    }
}

/// What opening costs, on `mb` MB each of private memory never touched
/// and of a committed section never touched: the time, the working set,
/// the card's account of the PC's memory shared with it; and how fast the
/// card copies out of such a heap against a shared buffer of today's kind.
unsafe fn cost(p: &mut Probe, rig: &mut Rig, memory: &dyn Fn() -> Option<[u64; 4]>, mb: usize) {
    let bytes = mb << 20;
    let shared = || memory().map_or(0, |m| m[3] >> 20);
    let ms = |speed: Option<f64>| speed.map_or("?".to_owned(), |ms| format!("{ms:.1}"));
    // SAFETY (the whole function): the allocation and the section below.
    unsafe {
        let base = mem::VirtualAlloc(std::ptr::null(), bytes, mem::MEM_RESERVE | mem::MEM_COMMIT | MEM_WRITE_WATCH, mem::PAGE_READWRITE).cast::<u8>();
        if !base.is_null() {
            let (set, account, started) = (working_set_mb(), shared(), std::time::Instant::now());
            let opened = open(&rig.device, From::Part(base as usize, bytes));
            let took = started.elapsed().as_secs_f64() * 1000.0;
            let placed = opened.as_ref().ok().and_then(|heap| place(&rig.device, heap, 0, bytes as u64).ok());
            let speed = placed.as_ref().and_then(|buffer| rig.copy_ms(buffer, bytes as u64).ok());
            p.lines.push(format!(
                "cost, {mb} MB of private memory never touched: {}; opened in {took:.1} ms; working set {set} -> {} MB; shared with the card {account} -> {} MB; \
                 the card copies it out in {} ms",
                hr(&opened),
                working_set_mb(),
                shared(),
                ms(speed)
            ));
            drop((placed, opened));
            mem::VirtualFree(base.cast(), 0, mem::MEM_RELEASE);
        }
        let invalid = windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        let handle = mem::CreateFileMappingW(invalid, std::ptr::null(), mem::PAGE_READWRITE | mem::SEC_COMMIT, (bytes as u64 >> 32) as u32, bytes as u32, std::ptr::null());
        if !handle.is_null() {
            let (set, account, started) = (working_set_mb(), shared(), std::time::Instant::now());
            let opened = open(&rig.device, From::Section(handle as usize));
            let took = started.elapsed().as_secs_f64() * 1000.0;
            let (set_opened, account_opened) = (working_set_mb(), shared());
            let placed = opened.as_ref().ok().and_then(|heap| place(&rig.device, heap, 0, bytes as u64).ok());
            let speed = placed.as_ref().and_then(|buffer| rig.copy_ms(buffer, bytes as u64).ok());
            p.lines.push(format!(
                "cost, {mb} MB of a committed section never touched: {}; opened in {took:.1} ms; working set {set} -> {set_opened} -> after the card read all of it {} MB; \
                 shared with the card {account} -> {account_opened} -> {} MB; the card copies it out in {} ms",
                hr(&opened),
                working_set_mb(),
                shared(),
                ms(speed)
            ));
            drop((placed, opened));
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
        // Today's kind of host copy, for the comparison.
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_CUSTOM,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_WRITE_BACK,
            MemoryPoolPreference: D3D12_MEMORY_POOL_L0,
            ..Default::default()
        };
        let mut buffer: Option<ID3D12Resource> = None;
        let made = rig.device.CreateCommittedResource(
            &heap,
            D3D12_HEAP_FLAG_NONE,
            &buffer_desc(bytes as u64, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS),
            D3D12_RESOURCE_STATE_COMMON,
            None,
            &mut buffer,
        );
        if let (Ok(()), Some(buffer)) = (made, buffer) {
            let speed = rig.copy_ms(&buffer, bytes as u64).ok();
            p.lines.push(format!("cost, for comparison: the card copies {mb} MB out of a shared buffer of today's kind in {} ms", ms(speed)));
        }
    }
}

// The census: which of the title memory that has host copies today could
// be opened, and by which opening.

/// Counts and bytes by kind, in the order of [`KINDS`].
#[derive(Default, Clone, Copy)]
struct Tally([(u64, u64); 6]);

const KINDS: [&str; 6] = [
    "private, committed whole (a part can be opened)",
    "view memory, one run of a section (one placed buffer)",
    "view memory, scattered pages",
    "view memory with unmapped pages",
    "private but not committed whole, or guarded",
    "other",
];

fn kind_of(at: usize, len: usize, known: &mut Vec<(usize, usize)>) -> usize {
    match cover(at, len) {
        Cover::Run(..) => return 1,
        Cover::Scattered => return 2,
        Cover::Holes => return 3,
        Cover::None => {}
    }
    let end = at + len;
    if known.iter().any(|&(from, to)| from <= at && end <= to) {
        return 0;
    }
    // SAFETY: plain data filled in by the query.
    let mut info: mem::MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { mem::VirtualQuery(at as *const c_void, &mut info, size_of_val(&info)) };
    if n == 0 {
        return 5;
    }
    let reach = (info.BaseAddress as usize).saturating_add(info.RegionSize);
    if info.Type == mem::MEM_PRIVATE && info.State == mem::MEM_COMMIT && info.Protect == mem::PAGE_READWRITE {
        known.push((info.BaseAddress as usize, reach));
        return if reach >= end { 0 } else { 4 };
    }
    match info.Type {
        mem::MEM_PRIVATE => 4,
        mem::MEM_MAPPED => 3,
        _ => 5,
    }
}

/// The census lines for `spans` (start, bytes, whether a window outside
/// buffers).
fn census_of(spans: &[(u64, u64, bool)]) -> Vec<String> {
    let mut known = Vec::new();
    let (mut buffers, mut windows) = (Tally::default(), Tally::default());
    for &(start, len, window) in spans {
        let kind = kind_of(start as usize, len as usize, &mut known);
        let tally = if window { &mut windows } else { &mut buffers };
        tally.0[kind].0 += 1;
        tally.0[kind].1 += len;
    }
    let mut lines = Vec::new();
    for (name, tally) in [("title buffers", buffers), ("windows outside buffers", windows)] {
        let all = tally.0.iter().fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
        lines.push(format!("census, {name}: {} of {} MB", all.0, all.1 >> 20));
        for (kind, &(n, bytes)) in KINDS.iter().zip(&tally.0) {
            if n > 0 {
                lines.push(format!("census,   {kind}: {n} of {} MB", bytes >> 20));
            }
        }
    }
    let v = views();
    let (count, bytes) = (v.views.len(), v.views.values().map(|view| view.0 as u64).sum::<u64>());
    let pages = v.views.values().filter(|view| view.0 == K64).count();
    let big = v.views.values().map(|view| view.0).max().unwrap_or(0);
    lines.push(format!(
        "census, the title's views of physical pages: {count} of {} MB; {pages} of them one 64 KiB page; the longest {} MB",
        bytes >> 20,
        big >> 20
    ));
    for &(_, bytes, committed) in &v.sections {
        lines.push(format!(
            "census, a section of {} MB, {}",
            bytes >> 20,
            if committed { "committed: it can be opened as one heap" } else { "reserved: refused as a heap until it is made with SEC_COMMIT" }
        ));
    }
    lines
}

/// With [`probe_level`] 2: one part of the title's own private memory
/// (the start of the biggest such title buffer, 4 MiB at most) is opened
/// for a moment, a buffer placed, 64 KiB read by the GPU and compared
/// with what the CPU reads there (the title may be writing it meanwhile:
/// the count of equal bytes is given), and all of it released. While the
/// heap stands (milliseconds) a `VirtualFree` of the title's on those
/// pages would fail.
fn real_part(spans: &[(u64, u64, bool)]) -> Vec<String> {
    let Some(host) = crate::host::host() else {
        return Vec::new();
    };
    let mut known = Vec::new();
    let found = spans
        .iter()
        .filter(|s| !s.2 && s.1 >= 2 * K64 as u64)
        .filter(|s| kind_of(s.0 as usize, s.1 as usize, &mut known) == 0)
        .max_by_key(|s| s.1);
    let Some(&(start, len, _)) = found else {
        return vec!["the title's memory: no title buffer in private, committed memory to try".to_owned()];
    };
    let at = (start as usize).next_multiple_of(K64);
    let bytes = ((start + len) as usize).saturating_sub(at).min(4 << 20) & !(K64 - 1);
    if bytes == 0 || !private_whole(at, bytes) {
        return vec!["the title's memory: the buffer found holds no whole 64 KiB page that is still committed".to_owned()];
    }
    let mut lines = Vec::new();
    let (set, started) = (working_set_mb(), std::time::Instant::now());
    let opened = open(&host.device, From::Part(at, bytes));
    lines.push(format!(
        "the title's memory, {} KiB at {at:#x} (of the title buffer {start:#x}+{len:#x}): {}; in {:.1} ms; working set {set} -> {} MB",
        bytes >> 10,
        hr(&opened),
        started.elapsed().as_secs_f64() * 1000.0,
        working_set_mb()
    ));
    if let Ok(heap) = &opened {
        let placed = place(&host.device, heap, 0, bytes as u64);
        lines.push(format!("  a buffer placed over it: {}", hr(&placed)));
        if let (Ok(buffer), Ok(mut rig)) = (&placed, Rig::new(&host.device)) {
            let mut before = vec![0u8; K64];
            crate::host::copy_mapped(before.as_mut_ptr(), at as *const u8, K64, false);
            let got = rig.fetch(buffer, 0, K64);
            let mut after = vec![0u8; K64];
            crate::host::copy_mapped(after.as_mut_ptr(), at as *const u8, K64, false);
            match got {
                Ok(got) => {
                    let equal = (0..K64).filter(|&i| got[i] == before[i] || got[i] == after[i]).count();
                    let zeros = got.iter().filter(|&&b| b == 0).count();
                    lines.push(format!(
                        "  the GPU read 64 KiB of the title's own bytes: {equal} of {K64} equal to what the CPU read just before or just after ({zeros} of them zero)"
                    ));
                }
                Err(e) => lines.push(format!("  the GPU's read failed: {e}")),
            }
        }
    }
    lines
}

/// Called once a second by the thread that submits, before a submission
/// (`host::sweep`): with [`probe_level`], the probe at the first call, the
/// census (on a thread of its own: it asks Windows about every copy) 30,
/// 150 and 300 seconds later.
pub(crate) fn tick() {
    if level() > 0 {
        say();
    }
    if probe_level() == 0 {
        return;
    }
    static STARTED: AtomicU64 = AtomicU64::new(0);
    static DONE: AtomicU64 = AtomicU64::new(0);
    let now = crate::host::now_ms().max(1);
    let started = match STARTED.compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed) {
        Ok(_) => {
            let Some(host) = crate::host::host() else {
                return;
            };
            let mb = std::env::var("VICTORY_INPLACE_PROBE_MB").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(64usize);
            let took = std::time::Instant::now();
            let probe = probe(&host.device, &|| host.video_memory(), mb.min(1024));
            for line in &probe.lines {
                tracing::info!("in place probe: {line}");
            }
            tracing::info!("in place probe: took {:.0} ms", took.elapsed().as_secs_f64() * 1000.0);
            now
        }
        Err(started) => started,
    };
    // The third in the world (the menus and loading take about three
    // minutes).
    let due = match DONE.load(Ordering::Relaxed) {
        0 => now >= started + 30_000,
        1 => now >= started + 150_000,
        2 => now >= started + 300_000,
        _ => false,
    };
    if due {
        let which = DONE.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            let spans = crate::shadow::spans();
            for line in census_of(&spans) {
                tracing::info!("in place {line}");
            }
            if which == 0 && probe_level() >= 2 {
                for line in real_part(&spans) {
                    tracing::info!("in place probe: {line}");
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run is the views from the one holding the address on that continue
    /// each other in one committed section, up to the asked reach; a view
    /// of another section, or of an uncommitted one, ends it or gives none.
    #[test]
    fn a_view_run_follows_one_committed_section() {
        // Handles no runtime section has (the mirror is shared by the tests).
        let (sec, other, loose) = (0x7e57_0001, 0x7e57_0002, 0x7e57_0003);
        let base = 0x7e57_0000_0000usize;
        {
            let mut m = mirror();
            m.sections.push((sec, 1 << 30, true));
            m.sections.push((other, 1 << 30, true));
            m.sections.push((loose, 1 << 30, false));
            m.views.insert(base, (K64, sec, 0x10_0000));
            m.views.insert(base + K64, (K64, sec, 0x10_0000 + K64 as u64));
            m.views.insert(base + 2 * K64, (K64, other, 0x10_0000 + 2 * K64 as u64));
            m.views.insert(base + 3 * K64, (K64, loose, 0));
        }
        assert_eq!(view_run(base + 0x100, 16 << 20), Some((base, 2 * K64, sec, 1 << 30, 0x10_0000)));
        assert_eq!(view_run(base + 0x100, 0x100), Some((base, K64, sec, 1 << 30, 0x10_0000)), "no further than asked");
        assert_eq!(view_run(base + K64 + 8, 16 << 20), Some((base + K64, K64, sec, 1 << 30, 0x10_0000 + K64 as u64)));
        assert_eq!(view_run(base + 3 * K64, 1), None, "an uncommitted section");
        assert_eq!(view_run(base + 4 * K64, 1), None, "no view");
        let mut m = mirror();
        m.sections.retain(|s| ![sec, other, loose].contains(&s.0));
        for k in 0..4 {
            m.views.remove(&(base + k * K64));
        }
    }

    /// Views as the runtime tells them: a range inside one view, or over
    /// views that continue each other in one section, is one run; over
    /// views from elsewhere it is scattered; over a gap it has holes.
    #[test]
    fn views_say_what_one_placed_buffer_could_show() {
        let mut views = BTreeMap::new();
        let at = 0x7_0000_0000usize;
        views.insert(at, (4 * K64, 9, 16 * K64 as u64));
        // Continues the first in the section.
        views.insert(at + 4 * K64, (2 * K64, 9, 20 * K64 as u64));
        // Follows in address, from elsewhere in the section.
        views.insert(at + 6 * K64, (K64, 9, 3 * K64 as u64));
        // After a gap of one page.
        views.insert(at + 8 * K64, (K64, 9, 4 * K64 as u64));
        assert_eq!(cover_in(&views, at + K64, 2 * K64), Cover::Run(9, 17 * K64 as u64));
        assert_eq!(cover_in(&views, at + K64, 5 * K64), Cover::Run(9, 17 * K64 as u64));
        assert_eq!(cover_in(&views, at, 7 * K64), Cover::Scattered);
        assert_eq!(cover_in(&views, at + 6 * K64, 3 * K64), Cover::Holes);
        assert_eq!(cover_in(&views, at + 7 * K64, K64), Cover::None);
        assert_eq!(cover_in(&views, at - K64, 2 * K64), Cover::None);
    }

    /// What the runtime forgets when memory is mapped anew.
    #[test]
    fn remapped_memory_forgets_its_views() {
        let at = 0x7_4000_0000usize;
        title_memory_view(at, 4 * K64, 77, 0);
        title_memory_view(at + 4 * K64, K64, 77, 4 * K64 as u64);
        assert_eq!(cover(at, 5 * K64), Cover::Run(77, 0));
        // One page inside the first view goes: the whole view is forgotten
        // (the runtime maps and tells again what stays of it).
        views_gone(at + K64, K64);
        assert_eq!(cover(at, K64), Cover::None);
        assert_eq!(cover(at + 4 * K64, K64), Cover::Run(77, 4 * K64 as u64));
        views_gone(at + 4 * K64, K64);
        assert_eq!(cover(at + 4 * K64, K64), Cover::None);
    }

    /// Only private memory committed whole, read-write, is taken for a part.
    #[test]
    fn only_private_memory_committed_whole_is_a_part() {
        // SAFETY: an allocation of the test's own.
        unsafe {
            let base = mem::VirtualAlloc(std::ptr::null(), 8 * K64, mem::MEM_RESERVE, mem::PAGE_NOACCESS).cast::<u8>();
            assert!(!base.is_null());
            assert!(!private_whole(base as usize, K64), "reserved only");
            mem::VirtualAlloc(base.cast(), 4 * K64, mem::MEM_COMMIT, mem::PAGE_READWRITE);
            assert!(private_whole(base as usize, 4 * K64));
            assert!(private_whole(base as usize + K64, 2 * K64));
            assert!(!private_whole(base as usize, 5 * K64), "reaches into the reserved half");
            let mut old = 0;
            mem::VirtualProtect(base.cast(), K64, mem::PAGE_READONLY, &mut old);
            assert!(!private_whole(base as usize, 2 * K64), "a guarded page");
            mem::VirtualFree(base.cast(), 0, mem::MEM_RELEASE);
        }
    }

    pub(crate) fn warp() -> Option<ID3D12Device> {
        use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
        use windows::Win32::Graphics::Dxgi::*;
        // SAFETY: plain DXGI queries and device creation on the software
        // adapter; a card is never opened.
        unsafe {
            let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).ok()?;
            let adapter: IDXGIAdapter = factory.EnumWarpAdapter().ok()?;
            let desc = adapter.cast::<IDXGIAdapter1>().ok()?.GetDesc1().ok()?;
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0 {
                return None;
            }
            let mut device: Option<ID3D12Device> = None;
            D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &mut device).ok()?;
            device
        }
    }

    /// The probe, whole, on the software adapter: the same calls the card
    /// gets. The runtime's and the kernel's rules hold there; the card's
    /// driver is what a run with `VICTORY_INPLACE_PROBE=1` adds.
    #[test]
    fn warp_probe_opens_title_like_memory_and_the_gpu_sees_its_bytes() {
        let Some(device) = warp() else { return };
        if device.cast::<ID3D12Device13>().is_err() {
            eprintln!("no ID3D12Device13 here: parts of an allocation cannot be opened");
            return;
        }
        let probe = probe(&device, &|| None, 4);
        for line in &probe.lines {
            eprintln!("in place probe: {line}");
        }
        assert!(probe.part_reads, "the GPU reads private memory in place");
        assert!(probe.part_writes, "GPU writes land in private memory");
        assert!(probe.section_reads && probe.section_writes, "a committed section in place");
        assert!(probe.section_outlives_views, "the section's heap does not depend on views");
        assert!(probe.reserved_section_refused, "a reserved section is refused");
        assert!(probe.own_view_reads && probe.own_view_writes, "a part through a view of our own shows the title's pages");
        assert!(probe.own_view_outlives_title_view, "the part's heap keeps the section's pages when the title remaps");
    }

    /// The probe on the machine's card (`VICTORY_TEST_CARD=1`; the card's
    /// driver, not the software adapter's, decides what opens and what is
    /// shared).
    #[test]
    fn card_probe_opens_title_like_memory_and_the_gpu_sees_its_bytes() {
        use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
        if std::env::var_os("VICTORY_TEST_CARD").is_none_or(|v| v != "1") {
            return;
        }
        let mut device: Option<ID3D12Device> = None;
        // SAFETY: device creation on the default adapter.
        unsafe { D3D12CreateDevice(None, D3D_FEATURE_LEVEL_12_0, &mut device) }.expect("a card");
        let device = device.expect("a device");
        let probe = probe(&device, &|| None, 4);
        for line in &probe.lines {
            eprintln!("in place probe (card): {line}");
        }
        assert!(probe.part_reads && probe.part_writes, "private memory in place");
        assert!(probe.own_view_reads && probe.own_view_writes, "a part through a view of our own shows the title's pages");
        assert!(probe.own_view_outlives_title_view, "the part's heap keeps the section's pages when the title remaps");
    }

    /// Stage 1's own call on the software adapter: a title buffer that
    /// begins and ends off the 64 KiB lines is placed over its pages, the
    /// GPU reads the buffer's bytes at the offset the views use, and its
    /// write lands at the title's address. Memory that is not committed
    /// whole is turned down without asking the host.
    #[test]
    fn warp_a_title_buffer_in_place_shows_its_bytes_at_its_offset() {
        let Some(device) = warp() else { return };
        if device.cast::<ID3D12Device13>().is_err() {
            return;
        }
        // SAFETY: an allocation of the test's own.
        unsafe {
            let base = mem::VirtualAlloc(std::ptr::null(), 8 * K64, mem::MEM_RESERVE | mem::MEM_COMMIT | MEM_WRITE_WATCH, mem::PAGE_READWRITE).cast::<u8>();
            let (start, len) = (base as u64 + K64 as u64 + 0x1230, 2 * K64 as u64 + 5);
            let placed = place_part(&device, start, len).unwrap_or_else(|e| panic!("not placed: {e:?}"));
            assert_eq!(placed.bias, 0x1230);
            assert_eq!(placed.gpu, placed.buffer.GetGPUVirtualAddress() + 0x1230);
            assert_eq!(placed.buffer.GetDesc().Width, 3 * K64 as u64, "the pages that hold the buffer");
            let mut rig = Rig::new(&device).unwrap();
            fill(start as *mut u8, len as usize, 7);
            let got = rig.fetch(&placed.buffer, placed.bias, len as usize).unwrap();
            assert!(same(start as *const u8, &got), "the GPU reads the title buffer's bytes");
            rig.store(&placed.buffer, placed.bias + 16, &[0xa5; 64]).unwrap();
            assert!(same((start + 16) as *const u8, &[0xa5; 64]), "the GPU's write is at the title's address");
            drop(placed);
            // Half of it decommitted: turned down, the host not asked.
            assert_ne!(mem::VirtualFree(base.add(2 * K64).cast(), K64, mem::MEM_DECOMMIT), 0);
            assert!(matches!(place_part(&device, start, len), Err(None)));
            mem::VirtualFree(base.cast(), 0, mem::MEM_RELEASE);
        }
    }

    /// Where a title buffer that is one run of a section goes in the
    /// section's heap: from the 64 KiB line below its first byte to the
    /// one above its last, the buffer's first byte that far in; nothing
    /// past the section's end.
    #[test]
    fn a_run_of_a_section_is_placed_on_its_pages() {
        let k = K64 as u64;
        assert_eq!(view_part(16 * k, 3 * k + 0x1230, 2 * k + 5), Some((3 * k, 3 * k, 0x1230)));
        assert_eq!(view_part(16 * k, 4 * k, k), Some((4 * k, k, 0)));
        assert_eq!(view_part(16 * k, 15 * k, k), Some((15 * k, k, 0)));
        assert_eq!(view_part(16 * k, 15 * k + 1, k), None, "past the section's end");
        assert_eq!(view_part(16 * k, 4 * k, 0), Some((4 * k, k, 0)), "an empty ask holds a page");
    }

    /// A title buffer in view memory that is one run of a committed
    /// section: the section opened as one heap, a buffer placed at the
    /// run's offset; the GPU reads the bytes the title wrote through its
    /// view and its write shows through the view. A run of a section that
    /// is not committed is refused by the host.
    #[test]
    fn warp_a_buffer_in_view_memory_is_placed_in_its_sections_heap() {
        let Some(device) = warp() else { return };
        const SIZE: usize = 16 * K64;
        let invalid = windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        // SAFETY: a section and a view of the test's own.
        unsafe {
            let handle = mem::CreateFileMappingW(invalid, std::ptr::null(), mem::PAGE_READWRITE | mem::SEC_COMMIT, 0, SIZE as u32, std::ptr::null());
            assert!(!handle.is_null());
            // The view shows the section from its fifth page on, as the
            // runtime maps pages: the title's address and the section's
            // offset differ.
            let view = mem::MapViewOfFile(handle, mem::FILE_MAP_ALL_ACCESS, 0, (4 * K64) as u32, 8 * K64);
            let base = view.Value.cast::<u8>();
            assert!(!base.is_null());
            let (start, len) = (base as u64 + K64 as u64 + 0x40, 2 * K64 as u64);
            fill(start as *mut u8, len as usize, 3);
            let offset = (5 * K64 + 0x40) as u64;
            let placed = place_view(&device, handle as usize, SIZE as u64, offset, len).unwrap_or_else(|e| panic!("not placed: {e:?}"));
            assert_eq!(placed.bias, 0x40);
            let mut rig = Rig::new(&device).unwrap();
            let got = rig.fetch(&placed.buffer, placed.bias, len as usize).unwrap();
            assert!(same(start as *const u8, &got), "the GPU reads what the title wrote through its view");
            rig.store(&placed.buffer, placed.bias + 8, &[0x5a; 32]).unwrap();
            assert!(same((start + 8) as *const u8, &[0x5a; 32]), "the GPU's write shows through the view");
            drop(placed);
            forget_section_heap(handle as usize);
            let _ = mem::UnmapViewOfFile(view);
            windows_sys::Win32::Foundation::CloseHandle(handle);
            // A reserved section: the host refuses its heap.
            let reserved = mem::CreateFileMappingW(invalid, std::ptr::null(), mem::PAGE_READWRITE | mem::SEC_RESERVE, 0, SIZE as u32, std::ptr::null());
            assert!(matches!(place_view(&device, reserved as usize, SIZE as u64, 0, K64 as u64), Err(Some(_))));
            forget_section_heap(reserved as usize);
            windows_sys::Win32::Foundation::CloseHandle(reserved);
        }
    }

    /// The census names each kind of memory.
    #[test]
    fn census_tells_private_memory_from_views() {
        // SAFETY: an allocation of the test's own.
        unsafe {
            let base = mem::VirtualAlloc(std::ptr::null(), 8 * K64, mem::MEM_RESERVE | mem::MEM_COMMIT, mem::PAGE_READWRITE) as u64;
            let view = 0x7_8000_0000u64;
            title_memory_view(view as usize, 2 * K64, 5, 0);
            title_memory_view(view as usize + 2 * K64, K64, 5, 9 * K64 as u64);
            let lines = census_of(&[(base, 2 * K64 as u64, false), (base + 4 * K64 as u64, K64 as u64, true), (view, K64 as u64, true), (view, 3 * K64 as u64, false)]);
            let text = lines.join("\n");
            assert!(text.contains("census, title buffers: 2 of"), "{text}");
            assert!(text.contains("census,   private, committed whole (a part can be opened): 1 of"), "{text}");
            assert!(text.contains("census,   view memory, scattered pages: 1 of"), "{text}");
            assert!(text.contains("census,   view memory, one run of a section (one placed buffer): 1 of"), "{text}");
            views_gone(view as usize, 3 * K64);
            mem::VirtualFree(base as *mut c_void, 0, mem::MEM_RELEASE);
        }
    }
}
