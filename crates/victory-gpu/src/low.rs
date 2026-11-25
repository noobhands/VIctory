//! Memory for the title below 2^40, as the console gives it.
//!
//! The console's addresses, the GPU's included, are 40-bit, and its shaders
//! build addresses from 40 bits. Memory the runtime hands the title out of
//! the host's own heap (`xmem`'s `XMemAllocDefault`, the memory of a
//! committed resource) lay above 1 TiB: the ray-tracing builder's instance
//! tables were there, and the pointers it read from them were cut to 40
//! bits (F16, `f16-rec1`). This allocator hands out memory below 2^40:
//! blocks of 64 KiB and more as reservations of their own, smaller ones in
//! power-of-two classes carved from 1 MiB arenas and kept on free lists.
//! The memory is write-watched, as the title's own reservations are, and
//! told to the GPU side as title memory.
//!
//! `VICTORY_LOW_ALLOC=0` leaves it to the host's heap, as before.

use std::sync::Mutex;

use windows_sys::Win32::System::Memory::{
    MEM_ADDRESS_REQUIREMENTS, MEM_COMMIT, MEM_EXTENDED_PARAMETER, MEM_RELEASE, MEM_RESERVE,
    MemExtendedParameterAddressRequirements, PAGE_READWRITE, VirtualAlloc2, VirtualFree,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// The write watch (not in windows-sys' memory module).
const MEM_WRITE_WATCH: u32 = 0x20_0000;
/// Blocks this size and larger are reservations of their own.
const BIG: usize = 64 << 10;
/// The smallest class.
const SMALLEST: usize = 16;
/// Classes from [`SMALLEST`] to half of [`BIG`].
const CLASSES: usize = (BIG / SMALLEST).trailing_zeros() as usize;
/// Bytes of an arena the small classes are carved from.
const ARENA: usize = 1 << 20;

/// Whether the title's memory comes from here.
pub fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_LOW_ALLOC").is_none_or(|v| v != "0"))
}

/// `size` bytes (64 KiB granules) reserved and committed below 2^40 and
/// above 4 GiB, aligned to `align` (at least 64 KiB), write-watched;
/// zeroed. Null if the system has none.
fn reserve(size: usize, align: usize) -> *mut u8 {
    let mut requirements = MEM_ADDRESS_REQUIREMENTS {
        LowestStartingAddress: (1usize << 32) as *mut _,
        HighestEndingAddress: ((1usize << 40) - 1) as *mut _,
        Alignment: if align > BIG { align } else { 0 },
    };
    let mut parameter = MEM_EXTENDED_PARAMETER::default();
    parameter.Anonymous1._bitfield = MemExtendedParameterAddressRequirements as u64;
    parameter.Anonymous2.Pointer = (&raw mut requirements).cast();
    // SAFETY: a fresh reservation of ours; the parameter points at
    // `requirements`, which outlives the call.
    let at = unsafe {
        VirtualAlloc2(
            GetCurrentProcess(),
            std::ptr::null(),
            size.next_multiple_of(BIG),
            MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH,
            PAGE_READWRITE,
            &mut parameter,
            1,
        )
    };
    if !at.is_null() {
        crate::inplace::title_memory_allocated(at as usize, size.next_multiple_of(BIG));
        OWNED.lock().unwrap_or_else(|e| e.into_inner()).insert(at as usize, size.next_multiple_of(BIG));
    }
    at.cast()
}

/// Every reservation made here (base, bytes), for [`owns`].
static OWNED: Mutex<std::collections::BTreeMap<usize, usize>> = Mutex::new(std::collections::BTreeMap::new());

/// Whether `at` lies in memory handed out here (a block of the host's heap
/// given to `free` must go back to the host's heap).
pub fn owns(at: *const u8) -> bool {
    let at = at as usize;
    OWNED.lock().unwrap_or_else(|e| e.into_inner()).range(..=at).next_back().is_some_and(|(&b, &n)| at < b + n)
}

/// The class of a small block of `size` bytes aligned to `align`.
fn class(size: usize, align: usize) -> usize {
    let bytes = size.max(align).max(SMALLEST).next_power_of_two();
    (bytes / SMALLEST).trailing_zeros() as usize
}

struct Small {
    /// Free blocks by class.
    free: [Vec<usize>; CLASSES],
    /// What is left of the arena being carved: next byte, end.
    arena: (usize, usize),
}

static SMALL: Mutex<Small> = Mutex::new(Small { free: [const { Vec::new() }; CLASSES], arena: (0, 0) });

/// `size` bytes aligned to `align` (a power of two), zeroed, below 2^40;
/// null if none is to be had.
pub fn alloc_zeroed(size: usize, align: usize) -> *mut u8 {
    if size >= BIG || align >= BIG {
        return reserve(size.max(1), align);
    }
    let c = class(size, align);
    let bytes = SMALLEST << c;
    let mut s = SMALL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(at) = s.free[c].pop() {
        drop(s);
        // SAFETY: a free block of this class, ours.
        unsafe { std::ptr::write_bytes(at as *mut u8, 0, bytes) };
        return at as *mut u8;
    }
    let (mut next, mut end) = s.arena;
    let mut at = next.next_multiple_of(bytes);
    if next == 0 || at + bytes > end {
        let fresh = reserve(ARENA, BIG) as usize;
        if fresh == 0 {
            return std::ptr::null_mut();
        }
        (next, end) = (fresh, fresh + ARENA);
        at = next;
    }
    let _ = next;
    s.arena = (at + bytes, end);
    // Fresh pages are zero.
    at as *mut u8
}

/// Gives back what [`alloc_zeroed`] gave for `size` and `align`.
///
/// # Safety
/// `at` came from [`alloc_zeroed`] with the same `size` and `align`, and is
/// not used again.
pub unsafe fn dealloc(at: *mut u8, size: usize, align: usize) {
    if at.is_null() {
        return;
    }
    if size >= BIG || align >= BIG {
        OWNED.lock().unwrap_or_else(|e| e.into_inner()).remove(&(at as usize));
        // SAFETY: a reservation of its own (the caller's promise).
        unsafe { VirtualFree(at.cast(), 0, MEM_RELEASE) };
        return;
    }
    SMALL.lock().unwrap_or_else(|e| e.into_inner()).free[class(size, align)].push(at as usize);
}

#[cfg(test)]
mod tests {
    /// Small and big blocks lie below 2^40, aligned, zeroed; a freed small
    /// block is handed out again, zeroed.
    #[test]
    fn blocks_lie_below_two_to_the_forty() {
        let reach = 1usize << 40;
        for (size, align) in [(24, 8), (100, 64), (4000, 16), (70_000, 16), (1 << 20, 1 << 16)] {
            let p = super::alloc_zeroed(size, align);
            assert!(!p.is_null());
            assert!((p as usize) + size < reach, "{size}: {p:p}");
            assert!((p as usize).is_multiple_of(align));
            // SAFETY: `size` bytes of ours.
            let bytes = unsafe { std::slice::from_raw_parts_mut(p, size) };
            assert!(bytes.iter().all(|&b| b == 0));
            bytes.fill(0xab);
            // SAFETY: from `alloc_zeroed` with these arguments.
            unsafe { super::dealloc(p, size, align) };
        }
        let p = super::alloc_zeroed(24, 8);
        // SAFETY: 24 bytes of ours.
        assert!(unsafe { std::slice::from_raw_parts(p, 24) }.iter().all(|&b| b == 0), "a reused block is zeroed");
        // SAFETY: as above.
        unsafe { super::dealloc(p, 24, 8) };
    }
}
