## F13: GPU allocations sub-allocated from pooled heaps (no kernel trips in the frame)
**Status:** IN PROGRESS
**What to build:** The rendering and submission threads create and release a committed resource per staging buffer, title-memory window, unordered-access buffer and host texture in the middle of frames; each is a trip into the kernel's video memory manager, which also serialises the threads. Sub-allocate them instead: a few large `ID3D12Heap`s per kind (upload, custom write-back L0, default), placed resources (`CreatePlacedResource`) cut from free lists by size class, space recycled on release (the D3D12 Memory Allocator's approach); committed resources only for what a heap cannot hold.
**Blocked by:** F07
**Requirements addressed:** 30 fps (CPU-bound frame)

### Evidence (`perf-profile`, F07): kernel calls (`win32u.dll` top frame) by the first VIctory frame, over 281 dumps
| Samples | Site |
|---:|---|
| 47 | `Host::upload_buffer` (a committed upload buffer; texture staging: one per title texture) |
| 45 | `resource::hosted` (host textures and resources) |
| 38 | `Host::shared_buffer` (custom heap, write-back, L0: title-memory windows; 1,588 released in 8 minutes, 7.1 GB) |
| 20 | `Host::uav_buffer` |
| ~40 | dropping `Staging`, `HostTexture`, `Shadow`, `HostList` (releases) |
| 135 | `host::frame` closure on the caption thread (window title; off the critical path) |
About 0.6 of a thread at any moment in the frame's allocations and releases.
- [ ] A sub-allocator for buffers (upload, L0, default) with a unit test for its free lists; then textures where the heap tier allows.
- [ ] Measured: kernel samples, frame time before / after (F07's method).

### Execution log
### Attempt 1 — 2026-11-25
**Status:** TO-DO -> IN PROGRESS
#### What was done:
- Not placed resources (they would need a wrapper type in every struct that holds a buffer) but recycling of committed buffers: `crates/victory-gpu/src/recycle.rs`. The three helpers (`upload_buffer`, `shared_buffer`, `uav_buffer`) take a free buffer of the size (at most a quarter bigger, test `a_free_buffer_fits_a_request_up_to_a_quarter_smaller`) before making one, and register what they make; a buffer whose only COM reference left is the recycler's is free again (the layer lets go only once the card is done). Shared buffers are zeroed when handed out again (host copies of title memory count on unread bytes being zero). Free bytes capped at 1 GiB a kind; `VICTORY_RECYCLE=0` turns it off. Frame line: "buffers per frame: N made, N handed out again".
#### What failed:
- First version (`f13-recycle-on` against `f13-recycle-off`, best settings, no DRED, 480 s each, both 6.7 min in the world with no removal): **slower**, 890 against 519 ms a frame (textures step 302 against 80 ms). It reused 15-312 buffers a frame, but swept every handed-out buffer (thousands, a reference count each) under the lock at every miss. Fixed: free buffers indexed by size (`BTreeMap`, a range lookup) and a sweep at most every 20 ms; second pair `f13-recycle2-on` / `-off` running.
#### Attempt 1, second version (`f13-recycle2-on` / `-off`):
- Buffers made per frame 163 -> 20 (183 handed out again); **frame time unchanged**: 524 ms on, 519 and 535 ms off (6.6-6.7 min in the world, no removal). The kernel trips were real work but not on the critical path. Kept (neutral, less kernel churn); not the lever.
