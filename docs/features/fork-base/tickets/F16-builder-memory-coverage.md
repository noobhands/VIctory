## F16: The builder sees all the title memory it reads (ray-traced lighting)
**Status:** IN PROGRESS
**What to build:** The ray-tracing structure builder (`VICTORY_BVH_BUILDER=1`) running on the title's real data, so its structures are right and can be copied back without quarantine, and the trace programs light what direct sun does not reach. Today its stages read zeros wherever a page has no host copy yet (F12 step 3), so every build of the loading screen is garbage.
**Blocked by:** F12 (done), F08 (title memory in place: the cheap way to give the GPU every page)
**Requirements addressed:** correct shader output (lighting in shadow)

### Plan
1. Every committed page of the title's memory reachable before the first builder dispatch, through the directory (`memory.rs`) pointed at the title's own memory instead of copies: the page pool's committed sections as one heap each (F08 `section_heap`; a 6.2 GB section needs two views, a raw view reaches 4 GB), private allocations by part (`place_part`), numbered up front. In place only for the by-address shaders (the builder, the trace programs): F08 measured that hot draw data must stay in VRAM.
2. Until then, a measured fallback: map all committed private memory before the first builder dispatch and see whether the fault lists empty and an unquarantined run survives (memory: about 19 GB of host copies; refresh cost per dispatch).
3. Reads of uncommitted memory after that are wrong addresses: compare a stage's inputs and outputs on the first build (`VICTORY_STORE_TRACE`) with what the title gave it.
4. Then copy-back without quarantine; captures of shadowed areas against the builder-off run.
- [ ] Fault lists empty on the first builder frame (`VICTORY_STORE_TRACE` `F` lines)
- [ ] Unquarantined run reaches the world without a crash
- [ ] Shadowed areas lit (captures against builder off)

Evidence: F12 "Without quarantine" (runs `f12-bvh-on` ... `f12-faults3`).

### Coverage in place (2026-11-25)
- `memory.rs` (`VICTORY_MEMORY_IN_PLACE=1`, with `VICTORY_XMEM_COMMIT=1`): every committed section of the page pool is numbered in the directory as regions (one buffer per 2 GiB, `CHUNK`) before the first by-address dispatch; every page the title's views show gets its region's entry (`regions`, `with_views`), and the runtime's mappings keep them current (`view_mapped` from `inplace::title_memory_view`, `view_unmapped` from `views_gone`). A page a copy holds keeps the copy; a root view's page takes it from a region (`map(.., named)`). Private memory stays with copies (a heap opened on it refuses the title's `VirtualFree` / `VirtualProtect`, which go straight to Windows).
- The 6.2 GB pool section is refused as one heap (E_INVALIDARG; 710 and 185 MB sections open): its parts are reached through views of our own, opened by address (`inplace::view_heap`). Live: 6 regions, ~236,000 page entries.
- Faults on the first builder frames fell from hundreds of lost first accesses to **25 committed pages** (private memory, `0xd9...`, `0xdb...`, `0x70b...`) and **435 wild reads** into memory the title reserved but never committed (and some free memory), e.g. `0x100000000`, `0x200000000`, `0x500090000`, and addresses past 128 GiB (`0x1fffff0000`, the directory's clamp).

### Which stage and instruction (diagnostics)
- `VICTORY_FAULTS_BY_DISPATCH=1`: each by-address dispatch of a frame gets a slice of the faults of its own; `VICTORY_STORE_TRACE` writes `FD <slice> <pipeline> <n> <page>@<native offset>` lines. The translator now stores, beside each fault's page, the native offset of the access (`lower.rs` `memory`, `inst_at`).
- `f16-fd2`: the first wild reads come from `190fbf0ebe040169` at `0x4b24`, `0x364c`, `0x37c4` (the same offsets fault in `3df629ffc3d0a2a6`, `f28a33a89eaa2caa`, `ffd93c8a029ff016`: variants of one program), then `5e68767498c57eb6` at `0x433c`, `0xb60`, `0x2f4`, `0x50e4`. No lost first access precedes them.
- `0x4b24` loads a node: `base + (index >> 3) * 64`, the index a root read from the structure's header (`global_load_dword` at `base + 48`, `& 0x3fffffff`). So the header holds a wrong root: written wrong by an earlier stage, or written through a descriptor (a copy) and read here in place (the title's own memory) before the copy goes back.
- With the descriptors' buffers in place too (`VICTORY_INPLACE=3`, so both paths reach the title's bytes; slow) the title crashed 2 s after the builder started (`f16-ip3`, `title+0x43c6087`), before a frame's faults were read.

### Next
1. Make every builder access reach the same bytes: copies created for view memory after the directory's walk are numbered and take their pages from regions before the next by-address dispatch (a queue the next `ready()` drains), or descriptors of by-address pipelines resolved to regions.
2. Find the stage that writes the header (`base + 48`) and compare its stores' path (raw address or descriptor) and translation with the console code; the instruction attribution above narrows each step to one access.
3. Then the unquarantined run and the captures (criteria below).
