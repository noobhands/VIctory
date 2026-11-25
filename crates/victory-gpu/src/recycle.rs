//! Committed buffers reused instead of made and released in the frame
//! (F13). Making or releasing a committed resource is a trip into the
//! kernel's video memory manager, which also serialises the threads that
//! record and submit; on the RTX 4080 about 0.6 of a thread was in such
//! trips at any moment of a world frame (`perf-profile`). The layer lets go
//! of a buffer only once the card is done with it, so a buffer whose only
//! reference left is ours is free to hand out again.
//!
//! Each kind of buffer the host makes (upload, shared with the processor,
//! the card's own) has a [`Recycler`]: the buffers handed out, each kept by
//! one reference of ours, and the free ones. A request takes the smallest
//! free buffer of at least its size and at most a quarter more
//! ([`fits`]); with none, the handed-out buffers whose callers have let go
//! are swept into the free ones first. Free bytes past [`CAP`] are released.
//! `VICTORY_RECYCLE=0` turns it off.
//!
//! A buffer handed out again holds what it held: shared ones are zeroed
//! first (host copies of title memory count on unread bytes being zero);
//! upload buffers are written whole by their users before use, and the
//! card's own (indirect arguments, twins' copies) are written by the card
//! before it reads them.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use windows::Win32::Graphics::Direct3D12::ID3D12Resource;
use windows::core::{IUnknown_Vtbl, Interface};

/// The kinds of buffer recycled, by the helper that makes them.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    /// `Host::upload_buffer`: upload heap, mapped.
    Upload = 0,
    /// `Host::shared_buffer`: custom write-back heap in system memory, mapped.
    Shared = 1,
    /// `Host::uav_buffer`: the card's own, unordered access.
    Card = 2,
}

/// Free bytes kept per kind; more are released.
const CAP: u64 = 1 << 30;

/// A buffer, its mapping (null for the card's own) and its size.
struct Entry {
    buffer: ID3D12Resource,
    data: usize,
    size: u64,
}

// SAFETY: a COM resource is free-threaded; the mapping is plain memory.
unsafe impl Send for Entry {}

#[derive(Default)]
struct Recycler {
    out: Vec<Entry>,
    /// Free buffers by size.
    free: std::collections::BTreeMap<u64, Vec<Entry>>,
    free_bytes: u64,
    /// When the handed-out buffers were last swept ([`SWEEP_EVERY`]).
    swept: Option<std::time::Instant>,
}

/// The least time between two sweeps of the handed-out buffers: a sweep
/// asks every one of them for its reference count, under the lock the
/// recording threads take (a sweep at every miss made frames slower, not
/// faster: `f13-recycle-on`, 890 ms against 519 ms without).
const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_millis(20);

static RECYCLERS: [Mutex<Option<Recycler>>; 3] = [const { Mutex::new(None) }; 3];

/// Buffers handed out again, and their bytes, since the last line.
pub(crate) static REUSED: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

/// Whether recycling is on (`VICTORY_RECYCLE=0` turns it off).
pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_RECYCLE").is_none_or(|v| v != "0"))
}

/// Whether a free buffer of `have` bytes may serve a request of `want`:
/// at least as big, and at most a quarter bigger (a big buffer is not
/// spent on a small request). Returns true if it fits.
pub(crate) fn fits(want: u64, have: u64) -> bool {
    have >= want && have <= want.saturating_add(want / 4)
}

/// The COM reference count of `buffer` (one AddRef and one Release).
fn references(buffer: &ID3D12Resource) -> u32 {
    let raw = buffer.as_raw();
    // SAFETY: a live COM object; AddRef then Release leaves its count as it
    // was, and Release returns the count.
    unsafe {
        let vtable = *(raw as *const *const IUnknown_Vtbl);
        ((*vtable).AddRef)(raw);
        ((*vtable).Release)(raw)
    }
}

/// A free buffer of `kind` for a request of `size` bytes, handed out again
/// (its mapping too), or None: then the caller makes one and [`gives`] it.
pub(crate) fn take(kind: Kind, size: u64) -> Option<(ID3D12Resource, usize)> {
    if !on() {
        return None;
    }
    let mut guard = RECYCLERS[kind as usize].lock().unwrap_or_else(|e| e.into_inner());
    let r = guard.get_or_insert_with(Recycler::default);
    let pick = |r: &mut Recycler| {
        // The smallest free size of at least `size`, if it fits ([`fits`]).
        let key = *r.free.range(size..).next().filter(|(have, _)| fits(size, **have))?.0;
        let list = r.free.get_mut(&key)?;
        let e = list.pop()?;
        if list.is_empty() {
            r.free.remove(&key);
        }
        Some(e)
    };
    let mut got = pick(r);
    if got.is_none() && r.swept.is_none_or(|t| t.elapsed() >= SWEEP_EVERY) {
        sweep(r);
        r.swept = Some(std::time::Instant::now());
        got = pick(r);
    }
    let e = got?;
    r.free_bytes -= e.size;
    if matches!(kind, Kind::Shared) && e.data != 0 {
        // A host copy of title memory counts on what it never copied
        // reading as zeros, as a buffer just made does (`shadow.rs`).
        // SAFETY: the buffer's own mapping, `e.size` bytes, no other user.
        unsafe { std::ptr::write_bytes(e.data as *mut u8, 0, e.size as usize) };
    }
    REUSED[0].fetch_add(1, Ordering::Relaxed);
    REUSED[1].fetch_add(e.size, Ordering::Relaxed);
    let handed = (e.buffer.clone(), e.data);
    r.out.push(e);
    Some(handed)
}

/// Keeps a reference to a buffer just made for `kind`, so that its space
/// can be handed out again once its callers let go of it.
pub(crate) fn gives(kind: Kind, buffer: &ID3D12Resource, data: usize, size: u64) {
    if !on() {
        return;
    }
    let mut guard = RECYCLERS[kind as usize].lock().unwrap_or_else(|e| e.into_inner());
    let r = guard.get_or_insert_with(Recycler::default);
    r.out.push(Entry {
        buffer: buffer.clone(),
        data,
        size,
    });
}

/// Moves the handed-out buffers nobody else holds into the free ones,
/// releasing what passes [`CAP`].
fn sweep(r: &mut Recycler) {
    let mut i = 0;
    while i < r.out.len() {
        if references(&r.out[i].buffer) == 1 {
            let e = r.out.swap_remove(i);
            if r.free_bytes + e.size <= CAP {
                r.free_bytes += e.size;
                r.free.entry(e.size).or_default().push(e);
            }
            // (past the cap, `e` drops here: released)
        } else {
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fits;

    /// A free buffer serves a request it holds, up to a quarter bigger.
    #[test]
    fn a_free_buffer_fits_a_request_up_to_a_quarter_smaller() {
        assert!(fits(1000, 1000));
        assert!(fits(1000, 1250));
        assert!(!fits(1000, 1251), "too big for it");
        assert!(!fits(1000, 999), "too small");
        assert!(fits(0, 0));
        assert!(fits(u64::MAX, u64::MAX), "no overflow");
    }
}
