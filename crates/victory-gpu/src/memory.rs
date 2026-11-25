//! Title memory by address, for translated shaders that read and write
//! through raw 40-bit addresses (`victory_shader::emit::SLOT_MEMORY`: the
//! console's ray-tracing structure builder and the programs that walk its
//! structures; the structures themselves lie in plain title memory, in no
//! buffer object).
//!
//! The copies are the ones every other access uses ([`crate::shadow`]: a
//! title buffer's copy, or the 4 MiB window of memory outside buffers), so
//! a root view and a raw address reach the same host bytes. This module
//! keeps what a shader needs to find them:
//!
//! - the **directory**: per 64 KiB page of title address space the number
//!   of the copy that holds the page (0: none) and the page's byte offset
//!   in it; one buffer for all lists, entries only ever go from empty to
//!   filled;
//! - per view heap a run of slots with a raw unordered access view of each
//!   numbered copy (slot 0 of the run: a null view, for pages without a
//!   copy);
//! - the **faults**: the page numbers shaders found empty, read back here
//!   once a frame, so the copies exist from the next frame on. A pointer a
//!   shader takes out of memory cannot be known before it runs; the
//!   addresses a dispatch is given as root views are made ready before it.
//!
//! **A page that is not there when a shader touches it is a lost access**:
//! the read gives 0, the store is dropped, and the fault note only helps
//! the next frame. For the structure builder that is the same as a dropped
//! stage, so before a dispatch the host makes ready everything it can see
//! the dispatch name: the whole copy around each root view, around each
//! pair of root constants that is a title address and around each root
//! constant that is one in 256-byte units (the builder passes structures
//! all three ways), and the 4 MiB after each (a structure may cross a
//! window's end).
//! shortcut: what a stage reaches only through pointers it reads out of
//! memory (InitHeaders' table of structure addresses, LoadInstances'
//! instance records) is not made ready unless something else named it;
//! the fault count in the log says whether that happens. The whole answer
//! is the extent of the title's allocation around an address, which the
//! memory module of the layer knows and this one does not ask yet.
//!
//! shortcut: a copy that is numbered is kept for the rest of the run, and
//! there are at most [`COPIES`] of them (4 MiB windows mostly: 4 GiB at
//! most). Every dispatch of such a shader brings all numbered copies up to
//! date and, if it can store, declares all of them written (the bytes are
//! compared on the CPU afterwards, as for any unordered access view): a
//! dirty-page mask written by the shader would bound both.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use victory_shader::emit::{MEMORY_FAULTS, MEMORY_PAGES};
use windows::Win32::Graphics::Direct3D12::*;

use crate::descriptor::host::HostHeap;
use crate::host::{HostList, host};
use crate::shadow::Shadow;

/// Copies that can be numbered (the run of heap slots is one longer). A
/// listed frame of the opening cutscene names about 50 windows.
/// `VICTORY_MEMORY_COPIES` overrides it (at most 65,535): the builder's
/// walk of the world's geometry names thousands of title buffers, each a
/// copy of its own, and ran out of numbers within seconds (its pages then
/// read zeros).
const COPIES: u32 = 1023;

/// [`COPIES`], or `VICTORY_MEMORY_COPIES` if set to a number from 1 to
/// 65,535.
fn copies_limit() -> u32 {
    static LIMIT: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *LIMIT.get_or_init(|| copies_from(std::env::var("VICTORY_MEMORY_COPIES").ok().as_deref()))
}

/// The copy limit `VICTORY_MEMORY_COPIES` = `v` asks for ([`copies_limit`]).
fn copies_from(v: Option<&str>) -> u32 {
    v.and_then(|v| v.trim().parse::<u32>().ok()).filter(|n| (1..=65_535).contains(n)).unwrap_or(COPIES)
}
/// A window of memory outside buffers (`shadow.rs`).
const CELL: u64 = 4 << 20;
/// A directory page.
const PAGE: u64 = 1 << 16;
/// Bytes of a section one region shows: a raw view reaches 4 GiB by its
/// 32-bit byte offsets, and the directory's offsets are 32-bit too.
const CHUNK: u64 = 2 << 30;

/// `VICTORY_MEMORY_IN_PLACE=1`: pages of the page pool that no copy holds
/// are reached in place, through the committed section's heap
/// (`VICTORY_XMEM_COMMIT=1`, `crate::inplace::section_part`), from the
/// first by-address shader on: no lost first access, no copy to number.
/// A page a copy holds keeps the copy (root views bind copies, and a root
/// view and a raw address must reach the same bytes). Private memory
/// stays with copies: a heap opened on it would refuse the title's own
/// `VirtualFree` and `VirtualProtect` there.
fn in_place() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_MEMORY_IN_PLACE").is_some_and(|v| v == "1"))
}

/// `VICTORY_FAULTS_BY_DISPATCH=1`: each by-address dispatch of a frame
/// (up to [`slices`]) leaves its faults in a slice of its own, read with
/// the others at the next frame and traced with the pipeline's name
/// (`VICTORY_STORE_TRACE`, `FD` lines): the first stage that reads where
/// nothing is names itself. Run without `VICTORY_LATE_WAIT` (the faults
/// are read when the frame's work has run).
fn by_dispatch() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_FAULTS_BY_DISPATCH").is_some_and(|v| v == "1"))
}

/// The faults' slices past the first ([`by_dispatch`]).
fn slices() -> u32 {
    if by_dispatch() { 2048 } else { 0 }
}

/// A part of a committed section of the page pool as one buffer placed in
/// the section's heap: what any view of those pages shows, in place.
struct Region {
    buffer: ID3D12Resource,
    len: u64,
}

/// What a number in the directory names.
enum Numbered {
    Copy(Arc<Shadow>),
    Region(Region),
}

impl Numbered {
    /// The buffer and its bytes, for the raw view in a heap's run.
    fn view(&self) -> (&ID3D12Resource, u64) {
        match self {
            Numbered::Copy(shadow) => (&shadow.buffer, shadow.span().1),
            Numbered::Region(region) => (&region.buffer, region.len),
        }
    }
}

/// The regions' numbers by section (handle; a number per [`CHUNK`]), for
/// the runtime's thread that maps pages ([`view_mapped`]).
static REGIONS: std::sync::RwLock<Vec<(usize, Vec<u32>)>> = std::sync::RwLock::new(Vec::new());

/// The directory's mapping once made, for [`view_mapped`] and
/// [`view_unmapped`] (which do not take the memory's lock).
static DIRECTORY: std::sync::atomic::AtomicPtr<u32> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Whether directory number `n` names a region.
fn is_region(regions: &[(usize, Vec<u32>)], n: u32) -> bool {
    n != 0 && regions.iter().any(|(_, numbers)| numbers.contains(&n))
}

/// The directory entry for the page at `offset` of a section whose
/// regions have `numbers`: the region's number and the page's offset in
/// it; `None` past the last region.
fn region_entry(numbers: &[u32], offset: u64) -> Option<(u32, u32)> {
    let n = *numbers.get((offset / CHUNK) as usize)?;
    Some((n, (offset % CHUNK) as u32))
}

struct Memory {
    /// Buffer, its mapping, its GPU address.
    directory: (ID3D12Resource, *mut u32, u64),
    faults: (ID3D12Resource, *mut u32, u64),
    /// Number `n` is `copies[n - 1]`: a copy, or a region of a section.
    copies: Vec<Numbered>,
    /// Per host view heap (by address): the first slot of its run, and how
    /// many copies have their view there.
    heaps: HashMap<usize, (u32, u32)>,
    /// The frame the faults were last read in.
    frame: u64,
    /// Pages asked for this frame that are not the title's memory.
    refused: std::collections::HashSet<u64>,
    /// With [`by_dispatch`]: what used each slice of the faults after the
    /// first, this frame.
    slices: Vec<String>,
}

// SAFETY: the mappings are of buffers the struct owns; all access is under
// the mutex.
unsafe impl Send for Memory {}

static MEMORY: Mutex<Option<Memory>> = Mutex::new(None);

/// Pages a shader asked for that got no copy, by why (diagnostics, with
/// `VICTORY_STORE_TRACE`): past the directory, refused earlier this frame,
/// not committed, no copy holds the page whole, the copy cannot be
/// written by shaders, no number left.
static UNMAPPED: [std::sync::atomic::AtomicU64; 6] = [const { std::sync::atomic::AtomicU64::new(0) }; 6];

/// With `VICTORY_STORE_TRACE`, for the first 3,000 pages refused as not
/// committed: what the kernel says of the page (state, protection, type,
/// the allocation's base and the run's size), to tell a pointer into
/// nothing from memory the title reserved and has not committed.
#[cfg(feature = "diagnostics")]
fn query_note(start: u64) {
    use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
    static SAID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if !crate::storetrace::on() || SAID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 3000 {
        return;
    }
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size; any address may be asked.
    let n = unsafe { VirtualQuery(start as *const _, &mut info, size_of_val(&info)) };
    crate::storetrace::note(&format!(
        "Q {start:#x} answered {} state {:#x} protect {:#x} type {:#x} base {:#x} run {:#x}",
        n != 0,
        info.State,
        info.Protect,
        info.Type,
        info.AllocationBase as usize,
        info.RegionSize
    ));
}

/// Counts a page left without a copy for reason `why` ([`UNMAPPED`]).
fn unmapped(why: usize) {
    UNMAPPED[why].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// What a draw binds.
pub(crate) struct Ready {
    pub(crate) directory: u64,
    pub(crate) faults: u64,
    /// The view heap slot of the null view; copy `n`'s view is `n` further.
    pub(crate) base: u32,
}

impl Memory {
    fn new() -> Option<Memory> {
        let host = host()?;
        let make = |dwords: u64, what: &str| {
            let (buffer, data) = host.shared_buffer(dwords * 4)?;
            crate::host::name(&buffer, || format!("title memory by address: {what}"));
            // SAFETY: a fresh mapping of at least that many bytes.
            unsafe { std::ptr::write_bytes(data, 0, (dwords * 4) as usize) };
            // SAFETY: a live buffer.
            let gpu = unsafe { buffer.GetGPUVirtualAddress() };
            Some((buffer, data.cast::<u32>(), gpu))
        };
        tracing::info!("title memory by address: a shader reads or writes through raw addresses");
        let mut m = Memory {
            directory: make(u64::from(MEMORY_PAGES) * 2, "directory")?,
            faults: make(u64::from(MEMORY_FAULTS) * (1 + u64::from(slices())), "faults")?,
            copies: Vec::new(),
            heaps: HashMap::new(),
            frame: u64::MAX,
            refused: Default::default(),
            slices: Vec::new(),
        };
        if in_place() {
            m.regions();
        }
        Some(m)
    }

    /// [`in_place`]: a region per [`CHUNK`] of every committed section,
    /// numbered first, and every page the title's views show now given
    /// its region; from here on the runtime's mappings keep the entries
    /// ([`view_mapped`], [`view_unmapped`]).
    fn regions(&mut self) {
        let mut table = Vec::new();
        let sections = crate::inplace::committed_sections();
        tracing::info!(?sections, "title memory by address: the committed sections of the page pool (handle, bytes)");
        for (section, bytes) in sections {
            let mut numbers = Vec::new();
            for from in (0..bytes).step_by(CHUNK as usize) {
                let len = CHUNK.min(bytes - from);
                let Some(buffer) = crate::inplace::section_part(section, from, len) else {
                    break;
                };
                crate::host::name(&buffer, || format!("title memory by address: section {section:#x} from {from:#x}, in place"));
                self.copies.push(Numbered::Region(Region { buffer, len }));
                numbers.push(self.copies.len() as u32);
            }
            if !numbers.is_empty() {
                table.push((section, numbers));
            }
        }
        let regions: usize = table.iter().map(|(_, n)| n.len()).sum();
        *REGIONS.write().unwrap_or_else(|e| e.into_inner()) = table;
        DIRECTORY.store(self.directory.1, std::sync::atomic::Ordering::Release);
        // Under the views' lock: no mapping changes between the walk and
        // the entries.
        let pages = crate::inplace::with_views(view_mapped);
        tracing::info!(regions, pages, "title memory by address: the page pool in place, through its sections' heaps");
    }

    /// Gives the page of `address` its copy, if it has none and the memory
    /// is the title's. `named` (a root view or constant names it): a
    /// region's entry gives way to the copy, which the root view binds.
    fn map(&mut self, address: u64, named: bool) {
        let page = address / PAGE;
        // The last entry stays empty: addresses past the directory read it.
        if page >= u64::from(MEMORY_PAGES) - 1 {
            unmapped(0);
            return;
        }
        // SAFETY: inside the directory's mapping.
        let entry = unsafe { self.directory.1.add(page as usize * 2) };
        // SAFETY: as above.
        let current = unsafe { entry.read_volatile() };
        if current != 0 && !(named && is_region(&REGIONS.read().unwrap_or_else(|e| e.into_inner()), current)) {
            return;
        }
        let start = page * PAGE;
        // A shader's pointer, or a constant taken for an address, may be
        // anything: only memory the title has.
        if self.refused.contains(&page) {
            unmapped(1);
            return;
        }
        if !crate::texture::committed(start as *const u8, PAGE as usize) {
            self.refused.insert(page);
            unmapped(2);
            #[cfg(feature = "diagnostics")]
            query_note(start);
            return;
        }
        // The copy a root view at that address would get; a buffer that
        // ends inside the page is passed over for what holds all of it.
        let whole = |s: &Arc<Shadow>| s.offset_in(start).is_some_and(|_| s.left(start) >= PAGE);
        let Some(shadow) = crate::shadow::find(start, 1)
            .filter(whole)
            .or_else(|| crate::shadow::find(start, PAGE).filter(whole))
        else {
            unmapped(3);
            return;
        };
        // SAFETY: a live buffer.
        let flags = unsafe { shadow.buffer.GetDesc() }.Flags;
        if flags.0 & D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS.0 == 0 {
            unmapped(4);
            return;
        }
        let n = match self.copies.iter().position(|c| matches!(c, Numbered::Copy(c) if Arc::ptr_eq(c, &shadow))) {
            Some(n) => n,
            None if self.copies.len() < copies_limit() as usize => {
                self.copies.push(Numbered::Copy(shadow.clone()));
                let n = self.copies.len() - 1;
                // Every page the copy holds whole, not only the one asked
                // for: a structure is more than 64 KiB. A region's entry
                // gives way to the copy (root views bind the copy).
                // shortcut: a page of a title buffer inside this one that
                // has a copy of its own is given this copy too.
                let regions = REGIONS.read().unwrap_or_else(|e| e.into_inner());
                let (first, len) = shadow.span();
                let mut at = first.next_multiple_of(PAGE);
                while at + PAGE <= first + len && at / PAGE < u64::from(MEMORY_PAGES) - 1 {
                    // SAFETY: inside the directory's mapping.
                    let other = unsafe { self.directory.1.add((at / PAGE) as usize * 2) };
                    // SAFETY: as above; the offset first, as below.
                    unsafe {
                        let was = other.read_volatile();
                        if was == 0 || is_region(&regions, was) {
                            other.add(1).write_volatile((at - first) as u32);
                            other.write_volatile(n as u32 + 1);
                        }
                    }
                    at += PAGE;
                }
                n
            }
            None => {
                static ONCE: std::sync::Once = std::sync::Once::new();
                ONCE.call_once(|| {
                    tracing::warn!(copies = copies_limit(), "title memory by address: no number left for another copy; its pages read zeros")
                });
                unmapped(5);
                return;
            }
        };
        let offset = shadow.offset_in(start).unwrap_or(0);
        // The offset first: a shader that sees the number sees it too.
        // SAFETY: inside the directory's mapping.
        unsafe {
            entry.add(1).write_volatile(offset as u32);
            entry.write_volatile(n as u32 + 1);
        }
    }

    /// The pages shaders found empty since the last frame: in the first
    /// slice of the faults, and with [`by_dispatch`] in each dispatch's.
    fn take_faults(&mut self) {
        let mut pages = Vec::new();
        let used = std::mem::take(&mut self.slices);
        // Pages in the first half of a slice, each with the native offset
        // of the access that found it empty half further on (the shader's
        // `Lower::memory`).
        let half = MEMORY_FAULTS as usize / 2;
        for slice in 0..=used.len() {
            let mut here = Vec::new();
            for i in 1..half {
                // SAFETY: inside the faults' mapping; the GPU may be
                // writing, a number missed now is written again next frame.
                let (at, by) = unsafe {
                    let at = self.faults.1.add(slice * MEMORY_FAULTS as usize + i);
                    (at, at.add(half))
                };
                // SAFETY: as above.
                let page = unsafe { at.read_volatile() };
                if page != 0 {
                    // SAFETY: as above.
                    here.push((page, unsafe { by.read_volatile() }));
                    // SAFETY: as above.
                    unsafe { at.write_volatile(0) };
                }
            }
            #[cfg(feature = "diagnostics")]
            if slice > 0 && !here.is_empty() && crate::storetrace::on() {
                here.sort_unstable();
                let list: Vec<String> = here.iter().map(|&(p, pc)| format!("{:#x}@{pc:#x}", u64::from(p) * PAGE)).collect();
                crate::storetrace::note(&format!("FD {slice} {} {} {}", used[slice - 1], here.len(), list.join(",")));
            }
            pages.extend(here.into_iter().map(|(page, _)| page));
        }
        #[cfg(feature = "diagnostics")]
        if crate::storetrace::on() && !pages.is_empty() {
            let mut sorted = pages.clone();
            sorted.sort_unstable();
            let list: Vec<String> = sorted.iter().map(|&p| format!("{:#x}", u64::from(p) * PAGE)).collect();
            let why: Vec<u64> = UNMAPPED.iter().map(|c| c.swap(0, std::sync::atomic::Ordering::Relaxed)).collect();
            crate::storetrace::note(&format!("U {why:?} (past the directory, refused this frame, not committed, no copy, copy without unordered access, no number)"));
            crate::storetrace::note(&format!("F {} {}", sorted.len(), list.join(",")));
        }
        if !pages.is_empty() {
            // Each is an access that read 0 or was dropped.
            static SAID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if SAID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 50 {
                tracing::warn!(
                    pages = pages.len(),
                    first = format_args!("{:#x}", u64::from(pages[0]) * PAGE),
                    "title memory by address: shaders touched pages that had no copy (those accesses read 0 or were dropped); the copies are made now"
                );
            }
        }
        for page in pages {
            self.map(u64::from(page) * PAGE, false);
        }
    }

    /// What a dispatch names: the copy around `address` and the 4 MiB
    /// after it. `root`: a root view binds that copy, so it takes the page
    /// from a region (a root view and a raw address reach the same bytes).
    fn name(&mut self, address: u64, root: bool) {
        self.map(address, root);
        self.map((address & !(CELL - 1)) + CELL, false);
    }
}

/// The runtime mapped `[at, at + len)` (64 KiB pages) as a view of
/// `section` from `offset` on: with [`in_place`], each page no copy holds
/// gets its region's entry. Returns the entries written. Runs on the
/// runtime's thread, without the memory's lock (a page a copy is being
/// given meanwhile may keep the region until it is named again).
pub(crate) fn view_mapped(at: usize, len: usize, section: usize, offset: u64) -> usize {
    let directory = DIRECTORY.load(std::sync::atomic::Ordering::Acquire);
    if directory.is_null() {
        return 0;
    }
    let regions = REGIONS.read().unwrap_or_else(|e| e.into_inner());
    let Some((_, numbers)) = regions.iter().find(|(s, _)| *s == section) else {
        return 0;
    };
    let mut written = 0;
    for k in (0..len as u64).step_by(PAGE as usize) {
        let page = (at as u64 + k) / PAGE;
        if page >= u64::from(MEMORY_PAGES) - 1 {
            continue;
        }
        let Some((n, within)) = region_entry(numbers, offset + k) else {
            continue;
        };
        // SAFETY: inside the directory's mapping, which lives as long as
        // the process.
        unsafe {
            let entry = directory.add(page as usize * 2);
            let was = entry.read_volatile();
            if was == 0 || is_region(&regions, was) {
                // The offset first: a shader that sees the number sees it.
                entry.add(1).write_volatile(within);
                entry.write_volatile(n);
                written += 1;
            }
        }
    }
    written
}

/// The runtime unmapped `[at, at + len)`: with [`in_place`], its pages'
/// region entries are emptied (a shader's access there is then a fault,
/// as before; a copy's entry stays, the copy is refreshed).
pub(crate) fn view_unmapped(at: usize, len: usize) {
    let directory = DIRECTORY.load(std::sync::atomic::Ordering::Acquire);
    if directory.is_null() {
        return;
    }
    let regions = REGIONS.read().unwrap_or_else(|e| e.into_inner());
    let (first, end) = (at as u64 / PAGE, (at as u64).saturating_add(len as u64).div_ceil(PAGE));
    for page in first..end.min(u64::from(MEMORY_PAGES) - 1) {
        // SAFETY: inside the directory's mapping.
        unsafe {
            let entry = directory.add(page as usize * 2);
            if is_region(&regions, entry.read_volatile()) {
                entry.write_volatile(0);
            }
        }
    }
}

/// Before a draw whose shader reads title memory by address: what its
/// root arguments name gets its copies (`addresses`: its root views;
/// `constants`: its root constants, see the module), every numbered copy
/// has its view in `heap`'s run and is brought up to date when `list` is
/// submitted (and, with `writes`, copied back after). None if the host
/// cannot hold it; the draw is then dropped.
pub(crate) fn ready(
    list: &HostList,
    heap: &HostHeap,
    addresses: &[u64],
    constants: &[u32],
    writes: bool,
    who: &dyn Fn() -> String,
) -> Option<Ready> {
    let mut guard = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Memory::new();
    }
    let m = guard.as_mut()?;
    let frame = crate::frame::current();
    if m.frame != frame {
        m.frame = frame;
        m.refused.clear();
        m.take_faults();
    }
    let numbered = m.copies.len();
    for &address in addresses {
        m.name(address, true);
    }
    // A title address in two constants (low dword, then bits 32 to 39), or
    // in one in 256-byte units. A constant that only looks like one names
    // memory nothing reads; one that is not the title's memory is refused.
    for (i, &c) in constants.iter().enumerate() {
        if let Some(&high) = constants.get(i + 1)
            && (1..=0xFF).contains(&high)
            && c >= 0x1_0000
        {
            m.name(u64::from(c) | u64::from(high) << 32, false);
        }
        if c >= 0x0100_0000 {
            m.name(u64::from(c) << 8, false);
        }
    }
    let key = std::ptr::from_ref(heap) as usize;
    let (base, made) = match m.heaps.get(&key) {
        Some(&run) => run,
        None => {
            // Held for the heap's life: not given back with a list's slots.
            let base = heap.dynamic(copies_limit() + 1)?;
            heap.null_raw_uav_at(base);
            (base, 0)
        }
    };
    for (n, item) in m.copies.iter().enumerate().skip(made as usize) {
        let (buffer, len) = item.view();
        heap.raw_uav_at(base + 1 + n as u32, buffer, 0, len);
    }
    m.heaps.insert(key, (base, m.copies.len() as u32));
    // Once per list, and for copies numbered since: the builder alone
    // dispatches hundreds of times a frame.
    // shortcut: a copy another list numbers meanwhile is brought up to date
    // by that list's submission, which may run after this one's.
    const READ: usize = usize::MAX - 0x6d;
    const WRITE: usize = usize::MAX - 0x6e;
    let first_read = list.first_use(READ);
    let first_write = writes && list.first_use(WRITE);
    // Regions are the title's memory itself: nothing to bring up to date
    // or copy back.
    for (n, item) in m.copies.iter().enumerate() {
        let Numbered::Copy(shadow) = item else { continue };
        let (start, len) = shadow.span();
        if first_read || n >= numbered {
            shadow.refresh(list, start, len);
        }
        if writes && (first_write || n >= numbered) {
            shadow.gpu_writes(list, start, len);
        }
    }
    // A slice of the faults of its own ([`by_dispatch`]).
    let faults = if (m.slices.len() as u32) < slices() {
        m.slices.push(who());
        m.faults.2 + m.slices.len() as u64 * u64::from(MEMORY_FAULTS) * 4
    } else {
        m.faults.2
    };
    if first_read {
        list.keep(&m.directory.0);
        list.keep(&m.faults.0);
    }
    Some(Ready {
        directory: m.directory.2,
        faults,
        base,
    })
}

#[cfg(test)]
mod tests {
    /// A page of a section, in its region: the region of the 2 GiB part it
    /// is in, and its offset there; none past the last region. A number is
    /// a region's only if a section's list holds it.
    #[test]
    fn a_section_page_finds_its_region() {
        let numbers = [4, 5, 6];
        assert_eq!(super::region_entry(&numbers, 0x1_0000), Some((4, 0x1_0000)));
        assert_eq!(super::region_entry(&numbers, super::CHUNK + 0x2_0000), Some((5, 0x2_0000)));
        assert_eq!(super::region_entry(&numbers, 2 * super::CHUNK), Some((6, 0)));
        assert_eq!(super::region_entry(&numbers, 3 * super::CHUNK), None);
        let regions = vec![(0x40, vec![1, 2]), (0x44, vec![3])];
        assert!(super::is_region(&regions, 2));
        assert!(super::is_region(&regions, 3));
        assert!(!super::is_region(&regions, 0));
        assert!(!super::is_region(&regions, 4), "a copy's number");
    }

    /// `VICTORY_MEMORY_COPIES`: a number from 1 to 65,535, else the default.
    #[test]
    fn the_copy_limit_takes_a_number_in_range() {
        assert_eq!(super::copies_from(None), super::COPIES);
        assert_eq!(super::copies_from(Some(" 8191 ")), 8191);
        assert_eq!(super::copies_from(Some("0")), super::COPIES);
        assert_eq!(super::copies_from(Some("70000")), super::COPIES);
        assert_eq!(super::copies_from(Some("many")), super::COPIES);
    }
}
