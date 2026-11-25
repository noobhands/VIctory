## F08: Title memory used in place by the GPU (the console's one memory)
**Status:** TO-DO
**What to build:** The title's physical page pool (`xmem` `XMemAllocatePhysicalPages`, a pagefile section; the title asks for about 6.1 GiB) created committed (`SEC_COMMIT`, as under Wine) behind a switch, opened once as a D3D12 heap (`OpenExistingHeapFromFileMapping`), and title buffers placed over it so shaders read and write the title's bytes with no copy or write-watch; textures stay copied but detiled on the GPU from the heap (F10).
**Blocked by:** F07 (the numbers that size the win)
**Requirements addressed:** 30 fps: removes most copies, refreshes and copy-back
- [ ] Switch for a committed page pool; the fork's `VICTORY_INPLACE_PROBE=2` on the RTX 4080 driver says whether the section opens as a heap and what GPU reads from it cost.
- [ ] Title virtual address to (section, offset) through `xmem`'s views; placed buffers over the heap for buffers in the page pool (the fork's stage 1 covers private memory only).
- [ ] Measured: copies, refreshes, copy-back and frame time before and after (F07's method).
Background: `crates/victory-gpu/src/inplace.rs` (what D3D12 accepts: whole `SEC_COMMIT` sections open in 0.2 ms per 256 MiB and lock nothing; reserved sections are refused; textures must be row-major with one level; tile mappings into such a heap remove the device).

### This machine (2026-11-24)
RTX 4080, 16,376 MiB VRAM; **BAR1 16,384 MiB (Resizable BAR on: the CPU can map all of VRAM)**; PCIe 4.0 x16 (about 25 GB/s for GPU reads of system memory); about 47 GB of system memory available to the GPU as shared memory; driver 617.42; 64 GB RAM. Two tiers follow:
1. Shared memory: the committed page pool opened as a heap (`OpenExistingHeapFromFileMapping`), read by the GPU over PCIe with no copy, no write watch and no copy-back; for data the GPU reads about once per frame.
2. VRAM through Resizable BAR (`D3D12_HEAP_TYPE_GPU_UPLOAD`, `D3D12_FEATURE_DATA_D3D12_OPTIONS16::GPUUploadHeapSupported`; needs a recent Agility SDK / OS): CPU writes of hot buffers (vertex, index, constants) straight into VRAM, no staging copy and no copy command; GPU reads at full VRAM speed.
- [ ] Probe both on this driver (heap from the section; GPU upload heap support) before building either.
