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

### One location per byte for the by-address shaders (2026-11-25, session 2)
- The wild reads land in **private** memory (the title's `XMemVirtualAlloc` / `VirtualAlloc` blocks), which page-pool regions do not cover; private memory is reached through several overlapping copies (a buffer's, an enclosing buffer's, 4 MiB windows) that do not see each other's writes within a submission.
- The title calls `VirtualFree` **4 times in 7 minutes, all at start-up** (placeholder splits, `0x8002`), and imports no `VirtualProtect`: private memory held open as a GPU heap during play is safe. The title's `VirtualFree` and `VirtualAlloc` (memory API set) now go through `xmem.rs` (`title_virtual_free` tells the GPU side before freeing; both record allocations).
- `inplace.rs`: the title's allocations are recorded (`title_memory_allocated`; none lies past the directory's 128 GiB, the largest are 0.6-2.2 GB at `0xd0..`-`0xdf..` GiB); `private_run`, `place_by_address`.
- `shadow.rs`: while a by-address pipeline binds its root views and the directory is filled (`ByAddress`, from `draw.rs` after its tables), `find` gives private memory in place: parts of each committed run (1 GiB steps, 64 MiB overlap), opened once (`in_place_private`; the map is asked first, `VirtualQuery` only on a miss: asked every time, the first build ran past the title's watchdog, `f16-priv3`). `VICTORY_MEMORY_IN_PLACE=2`: for every pipeline, tables too (a check; slow).
- `memory.rs`: the first walk numbers every private part (12 parts, 4.8 GB live) besides the 6 page-pool regions; faults are read after every waited submission too (`after_submission`), 8,192 slices a frame.
- The title's fatal handler (`title+0x23cf683`: `mov dword [0], 0x4d8bd421`) is a deliberate stop; with the stall it was the watchdog.

### Live (no late wait)
| Run | In place | First builder frame | Result |
|---|---|---|---|
| `f16-priv1`, `-2` | page pool + private for by-address | 872-995 dispatches, never ended | title's watchdog after ~30 s (`VirtualQuery` per lookup) |
| `f16-priv5` | the same, part map first | 6,294 dispatches in ~9 s | crash reading `-1` at `title+0x4e4b715` (the CPU code that reads the builder's results) |
| `f16-all1`, `-2` | `VICTORY_MEMORY_IN_PLACE=2` | 6,295 dispatches | crash reading `-1` (`title+0x32672e2`, `+0x43c5d59`) |

`f16-all2` (every path consistent): dispatches 1-2009 read nothing wild; the first wild read is `5e68767498c57eb6` (a **wave64** program) at `0x433c`, dispatch 2010: `global_load_dwordx4 v[25:28], v[17:18] + 0x20`, the pointer `v[17:18]` past 128 GiB (`0x1fffff0000`). `v[17:18]` is `buffer_load_dwordx2 v[17:18], v1, s[28:31] idxen` at `0x174`: an instance record (stride 48) of the root SRV at user data 39 (`s[6:7]` from the table at `+0x5c`, `s29 |= 0x300000`), indexed by a 24-bit index unpacked from a byte stream (`v_alignbit_b32` by `addr * 8`, checked: the shift is masked to 5 bits as the hardware does). Next: is the record wild in title memory (written so by an earlier stage or the title) or read wrong (the 64-lane form of `5e68...`, the structured root SRV with stride 48 and `USER_VIEW_BIAS`)? A check at bind: the records this dispatch indexes, read on the CPU from title memory before it runs.

### The index stream: which side is wrong (2026-11-25, session 3)
- The record probe (`VICTORY_F16_RECORD=1`) now also stores the index stream: the descriptor base `s[32:33]` (0 when, as here, it is built around a root descriptor; the host then lists every root view), the byte offset, the two dwords `buffer_load_dwordx2` at `0x10c` read, and the `v_alignbit` shift (16 dwords a sample). The host's `FI` line adds the words at each root view + offset in title memory, what holds root view 1 (`Shadow::describe`), a ±1 MiB scan of title memory for the GPU's words, and `VirtualQuery` of the stream's page.
- Mapping checked against the console code: `s[0:1]` is the spilled user-data table from dword 16; `s[32:35]` = table `+0x4c` = dwords 19-20 = root slot 1 (`u0`), `s33 |= 1 << 16` (stride 1), `s34 = -1`, `s35 = 0x11014fac`; `s[28:31]` = `+0x5c` = slot 3, stride 48. Offset = `const[3] + group·0x20000 + 3·thread & ~3`. The translator's mapping is right.
- `f16-rec7`, 23 samples: **the GPU side is wrong**. Title memory at `u0 + offset` holds packed 24-bit indices (mostly 0; one `02000002,00020000` = 2, 2, 2). The GPU read fp32/fp16 bounds (`7f800000`, `ff800000`, `7c00`, `fc00`, `7bff`): what an earlier build left there. The window's CPU side holds yet other bytes.
- Why: `u0` is the builder's scratch in the page pool: fresh 64 KiB views (`MEM_MAPPED`, several read-only), outside the title's private allocations, at reused addresses. The builder's passes write it by address, in place; root view 1 came from a 4 MiB window copy keyed by address, never refreshed (the CPU never writes those pages).
- Change: `shadow::find`, under `ByAddress` or `VICTORY_MEMORY_IN_PLACE=2`, puts page-pool view memory in place too (`in_place_view`): the run of one committed section's views from the view that holds the address (up to 16 MiB), placed in the section's heap, or in a heap on a view of our own of the part for the 6.2 GB section (`inplace::place_view_run`, heaps kept per part). The lookup uses a mirror of the views behind its own lock (`inplace::view_run`): `find` runs under the directory's lock, and the views' lock is taken before that one. Let go when the views go (`in_place_views_gone`). Test `a_view_run_follows_one_committed_section`.
- `f16-rec8` (the change): no fault was recorded, but the title crashed 0.6 s after the builder's first bind, before rec7's first faults were read, so that proves nothing yet: `title+0x40206` reading `-1`, `r12 = 0xfc007c007c007c00` (fp16 bounds read by CPU code as a pointer). The same site crashed `f16-rec5` before the change. Crash sites vary run to run (`+0x55ceb22`, `+0x4e4b7a6`, `+0x11f4ce`, `+0x40206`).

### Next
1. A run that lives long enough to read the faults with the change (`VICTORY_FAULTS_BY_DISPATCH=1`, faults read after the first waited submission), then whether 5e68's indices now match title memory.
2. Where CPU code at `title+0x40206` reads fp16 bounds as a pointer: which buffer, written by which pass, and whether copy-back of a stale copy wrote it.

### Longer runs with the change (2026-11-25, session 3, continued)
- `f16-rec9`/`-10`/`-11` lived 5-6 s after the builder's first bind: faults are still there (82-100 `FD` lines, 13-21 samples), and the index words 5e68 reads are still bounds (`7f800000`, `fc00`, `fbff`...).
- Root view 1 is now in place: the stream's runs are found (`view_run`: section `0xf90`, the 6.2 GB pool) and placed through views of our own (609 parts in rec10). `alias_check` (diagnostics) compares our view with the title's view on the CPU when a part is opened: 2,048+ parts, **0 differ**.
- So the readback comparison of `f16-rec7` (title memory read after the submission) was no proof: the scratch is reused by later builds before readback. With the root view in place, what 5e68 reads *is* title memory at that moment: **the index stream in memory holds bounds when 5e68 runs**. The stage that should write the indices did not write them there (dropped, writes lost, or another address), or ran after.
- Leads from the same runs: `unordered buffer view the host would refuse left null` (raw, `count=0`, `first=3326976`, width 49 MB: a UAV of a builder pass, its writes dropped); `a dispatch without a host pipeline dropped`; `f133e35efed2a48a` does not translate (`a lane mask exchanged where not every lane runs`, `f16-rec8`).

### Root causes found (2026-11-25, session 3, continued)
Method: per-dispatch call trace (`BC` lines: the list methods between builder dispatches, each dispatch's root views with the bytes they reach and their page-pool section/offset, its root constants), record/submit CPU snapshots of the leaf stream (`FS`), GPU-timeline snapshots (`WS`, `streamwatch.rs`; not reliable: a UAV barrier does not order a copy after a dispatch), the in-place probe on the card (`VICTORY_TEST_CARD=1`), the register-memory dump in crash reports (diagnostics).

Ruled out, each by a run:
- Dropped dispatches while pipelines are made: `f16-warm1` (warm cache, 0 drops) still faulted and crashed.
- Root views placed at record time going stale before submission: 0 of 32,768 checks (`f16-fs1`, `SV`).
- The in-place path itself: the title's page and the section page the GPU's heap shows are byte-identical (`f16-page1`, 736 of 736); the card probe passes for a part opened through a view of our own (reads, writes, after the title's view is remapped).
- Overlap between builder dispatches: `VICTORY_BUILDER_SERIAL=1` (a barrier before every builder dispatch) changed nothing (`f16-serial1`).
- Translation of 5e68: the stream loads come before the barrier (mode 11: group sync, global UAV fence, groupshared fence), the leaf stores after it.
- Views cut where section offsets stop being contiguous: 0 of 363 leaf stages and 0 of ~2,600 box stages write past their run (`f16-gcopy2`/`-3`).
- Descriptor slot reuse within a list: no slot names two addresses (`f16-ws5`).
- Tiled (reserved) buffers following the title's pages: the RTX 4080 refuses tiles mapped onto heaps opened by address (device removed in the probe).

Found and fixed:
1. **The console's pipeline syncs did nothing** (`list::sync`, slots 0x408/0x410): D3D12 dispatches without a barrier between them overlap on the 4080. Now one barrier on every shader write (`VICTORY_SYNC_BARRIER=0` turns it off). The builder itself separates its stages with barrier groups; this matters for other titles' passes.
2. **The title's buffer copies ran on the CPU at submission**, before any of the list's GPU work: the builder copies the compacted sizes `GetBVHCompactedSize` (1622) has just written (61 eight-byte `CopyBufferRegion`s before each batch's `DecompressHeader`, `f16-calls2`), so the title got stale sizes, laid structures out wrong, and rehydration read and wrote other structures' and objects' memory. Now (`gpucopy.rs`) a copy between two ranges of title memory in place is a dispatch of a copy shader at the recorded point between two barriers (`VICTORY_GPU_COPIES=0` keeps the CPU path). 1,453 of 1,459 copies checked equal source and destination after three frames (`f16-gcopy3`); the 6 others were changed afterwards.
- Result: **no wild GPU access at all** with the builder on (`FD=0` in `f16-gcopy1`..`-3`, `f16-dump1`, `f16-alias1`; 70-100 before).

Still open: the title still stops ~1 s after the builder starts, on box-node data read by CPU code (`+0x55ceb22` with fp16 boxes in an object at `0x6ed36ef00`; `+0x28cd253` walking a box-node blob at `0x12acd0000` past its committed 0x30000 bytes; `+0x4da6315` the allocator's assert on pointer `0xffffffff`). No builder root view, no GPU copy recorded, covers those addresses. Next: log every title copy at recording (GPU and CPU path) and every builder by-address store target, to find what fills that blob.
- Default settings (builder off) with today's changes: `default-check1` ran 5 minutes in the world, no crash.

### Later the same session
3. **DirectStorage read page arrays before the title had filled them** (`dstorage.rs`): the title fills a page-destination request's page array after enqueuing (copied at enqueue: 22,659 of 27,971 still zeros, `f16-dsfix1`) and even after submitting (6,097 at submission, `f16-dsfix2`); read at once by a worker, some arrays were zeros and the request failed ("physical pages could not be written"), leaving the pages' old bytes as the asset. Now the worker waits (up to 500 ms) until every page of the array is known (`wait_for_pages`); `f16-dswait1`: 0 failures (70-344 before; default runs had 3-362), 24 requests waited. Test `pages_are_waited_for_until_the_title_has_filled_them`.
- Box16's wave64 prefix sum (its words 0x7c-0x168, `dpp8`, `row_mirror`, `permlanex16`, `readlane 32/31`, EXEC halves) translates to the console's results on the card: test `the_box_stages_prefix_over_the_wave_is_the_consoles`.
- Tiled buffers over heaps opened by address are refused by the 4080 (device removed); the probe stage was removed again (it removed the shared WARP device and broke later tests).

Where it stands: the title still stops ~1 s after the builder starts. The crash dumps (`crash.rs` register dump) show the CPU reading an object at a `0x53b…` address (the title maps each physical page twice, at `A` and `A + 0x600000000`; 23,519 page pairs) whose page now holds a BLAS the builder rehydrated through the `0xb3b…` view. The address `0x53b740000` was mapped three times within seconds to different physical pages; the object pointer the CPU follows belongs to an earlier one. No builder write, GPU copy or DirectStorage write (page or address destination) reaches those bytes outside the BLAS's own layout. Next: trace unmaps and physical-page frees (`XMemMapPhysicalPages` with no pages, `XMemFreePhysicalPages`) against the object's lifetime, to tell a title use of a reused address from our map/free emulation handing out pages or addresses too early.
