## F08: Title memory used in place by the GPU (the console's one memory)
**Status:** IN PROGRESS
**What to build:** The title's physical page pool (`xmem` `XMemAllocatePhysicalPages`, a pagefile section; the title asks for about 6.1 GiB) created committed (`SEC_COMMIT`, as under Wine) behind a switch, opened once as a D3D12 heap (`OpenExistingHeapFromFileMapping`), and title buffers placed over it so shaders read and write the title's bytes with no copy or write-watch; textures stay copied but detiled on the GPU from the heap (F10).
**Blocked by:** F07 (the numbers that size the win)
**Requirements addressed:** 30 fps: removes most copies, refreshes and copy-back
- [x] Switch for a committed page pool (`VICTORY_XMEM_COMMIT=1`, existing); `VICTORY_INPLACE_PROBE=2` on the RTX 4080 driver (`f08-probe1`, below): sections open as heaps.
- [x] Title virtual address to (section, offset) through `xmem`'s views; placed buffers over the section's heap for buffers in the page pool (`inplace.rs` `place_view`, `section_heap`, `view_part`).
- [x] Measured (`f08-ip3a`, `f08-ip2a`): CPU work down, **GPU time up 3-5x, frames slower**: in place is for coverage, not for hot data (below).
Background: `crates/victory-gpu/src/inplace.rs` (what D3D12 accepts: whole `SEC_COMMIT` sections open in 0.2 ms per 256 MiB and lock nothing; reserved sections are refused; textures must be row-major with one level; tile mappings into such a heap remove the device).

### This machine (2026-11-24)
RTX 4080, 16,376 MiB VRAM; **BAR1 16,384 MiB (Resizable BAR on: the CPU can map all of VRAM)**; PCIe 4.0 x16 (about 25 GB/s for GPU reads of system memory); about 47 GB of system memory available to the GPU as shared memory; driver 617.42; 64 GB RAM. Two tiers follow:
1. Shared memory: the committed page pool opened as a heap (`OpenExistingHeapFromFileMapping`), read by the GPU over PCIe with no copy, no write watch and no copy-back; for data the GPU reads about once per frame.
2. VRAM through Resizable BAR (`D3D12_HEAP_TYPE_GPU_UPLOAD`, `D3D12_FEATURE_DATA_D3D12_OPTIONS16::GPUUploadHeapSupported`; needs a recent Agility SDK / OS): CPU writes of hot buffers (vertex, index, constants) straight into VRAM, no staging copy and no copy command; GPU reads at full VRAM speed.
- [ ] Probe both on this driver (heap from the section; GPU upload heap support) before building either.

### Probe on the RTX 4080 (`f08-probe1`, 2026-11-25; `VICTORY_INPLACE_PROBE=2 VICTORY_XMEM_COMMIT=1`)
- Private memory: the whole allocation and parts open as heaps; the GPU reads the CPU's bytes and the CPU reads the GPU's, with no call between; while the heap stands the title cannot decommit or protect those pages (decommit outside works).
- A reserved section is refused (0x80070057); a **committed section opens** (64 MB never touched: 3.5 ms, no working set); a buffer placed at a view's section offset reads the view's bytes, and GPU writes show through views mapped later, read-only or not.
- GPU copy out of 64 MB: 2.6 ms for private memory, a section, or today's shared buffers alike (~25 GB/s, PCIe 4.0 x16).
- Census in the world (300 s): title buffers 1,049 MB (302 private, 426 view memory in one run, 320 scattered); windows outside buffers 5,768 MB (428 private, 1,140 view memory with unmapped pages, 4,128 private not committed whole or guarded); 92,729 views of physical pages, all but 6 one 64 KiB page; three sections, 710 + 185 + 6,236 MB, committed.
- The costliest copy is one buffer: `0x12450000+0xae00000` (174 MB, view memory, in the card's memory): refreshes check ~2.1 GB/frame of it and copy-back compares 674 MB/frame for ~0.5 MB/frame written (`f10-range2`).

### Built (switches, off by default)
- `inplace.rs`: committed sections open once as one heap each (`section_heap`); a title buffer or window that is one run of a committed section is a buffer placed at the run's offset (`place_view`, `view_part`); scattered pages and runs with holes stay copies; a section's heap is forgotten when the runtime closes the section (`title_memory_section_gone`, `xmem.rs` `Section::drop`).
- `VICTORY_INPLACE=3`: title buffers the card would hold go in place too (`shadow.rs` `lookup`).
- With `VICTORY_LATE_WAIT`, shader writes through in-place buffers are still declared to the flights (nothing copied back), so reads of ours wait for them: levels 2-3 combine with late wait (the old note said not to).
- Tests: `a_run_of_a_section_is_placed_on_its_pages`, `warp_a_buffer_in_view_memory_is_placed_in_its_sections_heap` (GPU reads and writes through a committed section's heap at a view's offset; a reserved section refused). Checks: workspace 513 passed, clippy clean both configurations (toolchain deprecation notes only).

### Measured (world, warm cache, `VICTORY_LATE_WAIT=3 VICTORY_WIDE_BACK=1` + card buffers + `VICTORY_XMEM_COMMIT=1`; median / mean of the last 60 lines, ms)
| Run | In place | Frame | Copies | Copy-back | GPU | Heap-buffer pass |
|---|---|---:|---:|---:|---:|---:|
| `f10-range1` / `-2` | off | 293 / 281, 291 / 283 | ~50 | ~40 | 60-90 | 80-210 |
| `f08-ip3a` | `VICTORY_INPLACE=3`: 3,700 buffers 655 MB (353 MB in section heaps), 86 windows 408 MB | **590 / 628** | 34 | **9** | **378** | 266 |
| `f08-ip2a` | `VICTORY_INPLACE=2`: 3,600 buffers 40 MB, 87 windows 408 MB | **551 / 595** | 48 | 27 | **290** | 264 |

- The CPU's part goes down as intended (copy-back 40 -> 9 ms, copies 50 -> 34 ms), but the GPU now reads the title's hot buffers and windows over PCIe (~25 GB/s) instead of from VRAM (the card copies and card windows they replaced): GPU time 3-5x, frames twice as slow. **On a discrete GPU, in place suits memory the GPU touches rarely or sparsely, not the world's vertex and constant data.** Level 2 shows windows alone are enough to do it (they were in the card's memory).
- What stays useful: section heaps give every page-pool page a GPU address with no copy. That is what F16 needs (the builder and trace programs reach title memory by address, sparsely); there in place replaces lost accesses, not VRAM copies.

### Next
- Tier 2 for hot data: VRAM the CPU writes through Resizable BAR (`D3D12_HEAP_TYPE_GPU_UPLOAD`; probe `GPUUploadHeapSupported`): refreshes as CPU writes straight into the card copy, no staging buffer, no copy command.
- Copy-back of card copies by a GPU-side dirty mask (what shaders wrote, found on the card) instead of comparing 674 MB a frame on the CPU.
- F16: the memory-by-address directory over the section heaps and private parts (coverage), not over copies.
