## F10: Texture uploads and detiling
**Status:** IN PROGRESS
**What to build:** The textures step (120-280 ms per frame on the fork) cut down: what is detiled and hashed per frame and why, with detiling moved to the GPU (a compute pass reading tiled bytes from a buffer, or from the in-place heap of F08) and re-uploads only of what changed.
**Blocked by:** F07
**Requirements addressed:** 30 fps
- [x] The textures step attributed (bytes, counts, hashing, detiling, uploads): below; detiling is not where the time goes.
- [ ] A GPU detiler for the modes the title uses, tested against the CPU detiler.

### Attribution (2026-11-25, world, `VICTORY_LATE_WAIT=3 VICTORY_WIDE_BACK=1` + card buffers, no DRED)
Diagnostics added: the frame line's texture part gives hashing and detiling time and the textures detiled; a "textures detiled" table names the textures detiled most (address, size, format, tiling); "title memory remapped per frame" gives the runtime's remappings, how long it held reads off, our bookkeeping's share and the GPU's page-mapping copies (pages and views).

| Run | Frame (median / mean of last 60 lines, ms) | Textures step | Texture waits for title memory | Remap lock held |
|---|---|---|---|---|
| `f10-attr1` (cold cache) | 344 / 417 | 48 / 76 | 61 / 79 | — |
| `f10-remap1` (cold) | 377 / 400 | 50 / 77 | 4 / 12 | 30 / 43 |
| `f10-runs1` (warm, runs of pages) | 372 / 364 | 50 / 73 | 14 / 109 | 53 / 195 |
| `f10-book1` (warm) | 351 / 361 | 41 / 46 | 1 / 4 | 18 / 22 |
| **`f10-range1`** (warm, range lock) | **293 / 281** | **34 / 36** | **0 / 0** | 13 / 17 |
| **`f10-range2`** (repeat) | **291 / 283** | **41 / 38** | **0 / 0** | 12 / 14 |

- Hashing is 15-23 ms a frame and detiling 7-10 ms (100-171 textures, 95-150 MB). No texture is detiled every frame: they stream in once (4096x4096 BC textures, 11 MB each). A GPU detiler would save under 10 ms a frame: not the lever.
- The "textures" step was mostly **waiting for the remap lock**: one lock for all title memory, which the runtime held while it mapped streamed pages (1,000-3,000 remappings a frame, held up to 330 ms a frame in bursts, four fifths of that in the kernel's view calls, `f10-book1`). Every texture refresh waited, whatever memory it read.
- Fix: the lock is **by range** (`watch.rs`): a remapping holds off only reads of the views it changes (`xmem.rs` `changed_by`: the range and every view `unmap` takes away and maps again). Texture waits went to 0 and frames from about 350-375 to 291-293 ms (median, two runs each side; single runs vary).
- Page-mapping copies are now mapped in runs of consecutive source slices (`alias_runs`, `map_run`), and `map` joins the placeholders of several unmapped views. Live, the copies' source pages were never consecutive (pages = views), so this changes nothing measurable today.
- Tests: `reads_and_remappings_of_other_memory_do_not_wait`; `a_read_of_one_page_never_meets_the_gap_of_a_change_to_its_view` (faults with the change range cut to the asked range: red checked); `a_page_mapping_copy_of_consecutive_pages_is_one_view`. Checks: workspace 511 passed, `victory-gpu` diagnostics 128, clippy both configurations clean (only the toolchain's deprecation notes).

Next: what remains of a ~290 ms frame (preparing copies ~50 ms, copy-back ~45 ms, GPU ~60 ms, the heap-buffers pass 90-210 ms, and time outside submissions).
