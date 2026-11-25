## F14: The process heap's lock (about six threads in the allocator at a time)
**Status:** IN PROGRESS
**What to build:** In the CPU profile (`perf-profile`, F07) **1,650 of the samples over 281 dumps are inside the allocator** (`RtlAllocateHeap` / `RtlFreeHeap` under the diagnostics build's counting wrapper `copystat::Counting`): about six threads in the Windows heap at any instant, contending on its lock, the recording threads among them. Either a scalable allocator with per-thread heaps (mimalloc, as a `fast-alloc` cargo feature: one global allocator, chosen at build time), or fewer allocations in the hot paths (callers not identifiable from these samples: identical-code folding in the release build merges the small closures the symbolizer would name).
**Blocked by:** none (the user allowed the `mimalloc` dependency on 2026-11-25 after the permission system stopped the first attempt, whose edits had been reverted).
**Requirements addressed:** 30 fps (CPU-bound frame)
- [ ] With the dependency allowed: `fast-alloc` builds, A/B live (frame time, allocator samples in a new profile).
- [ ] Without: allocation counts per call site (a diagnostics counter by caller) and the hot ones removed.

### Execution log
### Attempt 1 — 2026-11-25
**Status:** TO-DO -> IN PROGRESS
#### What was done:
- `mimalloc` 0.1.52 (`libmimalloc-sys` 0.1.49, built from C with MSVC) as an optional workspace dependency; feature `fast-alloc` on `victory-gpu`, passed through `victory-hle` and the app (`--features fast-alloc`). With diagnostics the counting allocator (`copystat::Counting`) wraps mimalloc (`INNER`); without, mimalloc is the global allocator (`lib.rs`). Without the feature nothing changes. `Cargo.lock` gained the two crates.
- A/B live: `f14-mimalloc` (`target/diag4`) against `f14-system` (`target/diag3`, same code), best settings, no DRED.
#### Result (world-only frame lines, ms per frame)
| Run | p10 | median | p90 | mean | draws (median) |
|---|---:|---:|---:|---:|---:|
| `f14-system` (recycler on, system heap) | 274 | 558 | 736 | 543 | 2,220 |
| `f14-mimalloc` (recycler on, mimalloc) | 190 | **376** | 907 | 556 | 2,674 |
| `f13-recycle2-on` (same build as `f14-system`) | 158 | 310 | 799 | 459 | 2,694 |
| `f13-recycle-off` / `f13-recycle2-off` | 205 / 207 | 456 / 451 | 785 / 834 | 485 / 504 | 2,768 / 2,466 |
**Inconclusive**: the same build and settings gave medians of 310 and 558 ms in two runs; the world segment each run reaches differs (draws 2,220-2,768), so single 8-minute runs cannot show changes of tens of percent. Kept as an opt-in feature (`--features fast-alloc`), not the default, until a steadier measurement (several runs per configuration over the same segment, or a fixed scene) shows it.
