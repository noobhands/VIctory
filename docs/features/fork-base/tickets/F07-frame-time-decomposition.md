## F07: Where a frame's time goes (to 33 ms)
**Status:** IN PROGRESS
**What to build:** A measured breakdown of a world frame on the RTX 4080 / Ryzen 9 7900X3D, without DRED: CPU preparation (copies, textures, write watching, descriptor work), GPU time by command and pass, copy-back, waits; with the fork's speed switches compared. Every later performance ticket cites a number from here. Target: 30 fps (33 ms per frame).
**Blocked by:** F01
**Requirements addressed:** the user's requirement of 30 fps minimum
- [ ] `perf-default`, `perf-fast` (`VICTORY_LATE_WAIT=3 VICTORY_WIDE_BACK=1 VICTORY_CARD_BUFFERS_MIN_MB=1 VICTORY_CARD_BUFFERS_MB=1024`), `perf-gputime` (`VICTORY_GPU_TIMING=1`): runner `perf-chain.sh`, `NODRED=1`.
- [ ] GPU time attributed to programs and passes (top 20 by milliseconds per frame).
- [ ] CPU time attributed (frame line and copystat parts; a sampling profile of the process if the parts leave a gap).

### Starting point (F01, diagnostics build with DRED)
905-1,049 ms per frame: preparing 324-463 (copies 49-54, textures 144-280), GPU 190-292, copying back 95, the rest about 300 ms unattributed. About 12 GB of refreshes checked per frame.

### Measured 2026-11-25 (fork-base diagnostics build `10145ce5945e05d3`, no DRED, `cmp-run.sh`, 480 s each, all 6.7 min in the world with no removal)
| Run | fps | Frame ms | Preparing (copies / textures) | GPU wait | Copying back | Outside submissions |
|---|---:|---:|---|---:|---:|---:|
| `perf-default` | 1.6 | 857 | 380 (51 / 219) | 189 | 72 | **216** |
| `perf-fast` (`VICTORY_LATE_WAIT=3 VICTORY_WIDE_BACK=1 VICTORY_CARD_BUFFERS_MIN_MB=1 VICTORY_CARD_BUFFERS_MB=1024`) | 2.2 | **601** | 213 (50 / 135) | 58 | 29 | **301** |
| `perf-gputime` (`VICTORY_GPU_TIMING=1`) | 1.7 | 699 | 254 (56 / 84) | 241 | 75 | 129 |

- DRED costs about 6% (914 vs 857 ms); not the problem.
- The fork's speed switches cut the frame by 30%, mostly by not waiting for the GPU (189 -> 58 ms): the GPU work then overlaps the CPU's.
- "Preparing" and "GPU wait" are time inside submissions (`SUBMISSION_US`); the 216-301 ms outside them is the title's own CPU work, VIctory's implementation of the title's D3D12 calls while it records lists, and presents: not yet attributed (next: a sampling profile from `VICTORY_STACKS`, symbolized with `llvm-symbolizer` against `victory.pdb`).
- GPU time: every window's longest wait is one submission of dozens of compute pipelines, up to 405 ms. Per-command times need the commands noted, which the fork does only with DRED on: `perf-gpucmd` (DRED + timing) is the run for that.

### GPU time by command (`perf-gpucmd`: DRED + `VICTORY_GPU_TIMING=1`, 70 report windows in the world)
GPU time **157 ms per frame** on average (0.4-1,000). By kind: indirect 112.6, dispatch 36.4, texture uploads 3.8, indexed 1.7, alias copies 1.2, mesh 0.8, draws 0.6.

| ms / frame | per frame | longest ms | Command |
|---:|---:|---:|---|
| **42.7** | 0.6 | **554** | `indirect [compute 3592426e3e9142d8]` (7,900 lines of GFX10, 48 loops, no barriers; its `u4` table slot often a **null** heap slot) |
| **21.7** | 0.6 | **335** | `indirect [compute cfd83392ec9ae408]` (about 400 lines, **no loops**) |
| **15.8** | 0.8 | 46 | `indirect [compute f22a76fcb967ee59]` (11 loops) |
| 6.5 | 0.8 | 31 | `dispatch [compute cb6adcc6da2613ff]` |
| 4.7 | 0.8 | 7.5 | `dispatch [compute 7c90a988167650a1]` |
| 3.8 | 710 | 1.3 | texture uploads |
| 2.2 | 166 | 0.9 | `indirect [mesh 7972997d15fd9e69, pixel 9f14b72117e2abec]` |

**Three indirect compute dispatches are half the GPU time.** A loop-free 400-instruction program taking 335 ms in one dispatch can only be a huge group count: the clamp (`indirect.rs`) caps each dimension at 65,535 and the product at 2^20 groups, so a garbage record still runs up to about a million groups (the budget assumes 64 threads a group). Same pattern as upstream PR #1799's indirect mesh finding. Next: `perf-cargs` logs the most groups a compute record asked for and how many records were over the limit (clamp words 3-4, frame line "indirect compute dispatches: ...").

### Why the GPU time is high: the buffers live in system memory (`perf-cargs`, 2026-11-25)
- The compute arguments are **not** garbage: the most groups a record asked for per window is 11,664-377,096 (often 58,320 or 115,200), never an unwritten slot's all-ones. One to four records a frame ask for more than 65,535 groups in one dimension (a 1-D dispatch of 115,200 groups): the clamp cuts them to 65,535 (F11).
- The costly programs are small: `cfd83392ec9ae408` (8x4 = 32 threads a group, about 14 loads and 20 stores a thread, no loops) at up to about a million groups moves about 4.5 GB; 335 ms for that is about 13 GB/s, PCIe speed (PCIe 4.0 x16: 25 GB/s at most), against the RTX 4080's 717 GB/s.
- Cause: every title buffer a draw uses is mirrored in a host buffer **in system memory** (`shadow.rs`: "a persistent host buffer of its size on first use (system memory shaders may also write)"; `Host::shared_buffer`: `D3D12_HEAP_TYPE_CUSTOM`, `WRITE_BACK`, `MEMORY_POOL_L0`), so the CPU can copy title bytes in and read shader writes back; every shader access to it crosses PCIe. The fork's card buffers (default on) keep a VRAM copy only of buffers of 16 MB or more, 512 MB in all (sized for a 12 GB RTX 4070 with 1.6 GB free).
- Next: `perf-card8g` (`VICTORY_CARD_BUFFERS_MIN_MB=1 VICTORY_CARD_BUFFERS_MB=8192`, with timing) to see the three dispatches' GPU time with their buffers in VRAM.

### Confirmed: buffers in VRAM halve the GPU time (`perf-card8g`, DRED + timing, `VICTORY_CARD_BUFFERS_MIN_MB=1 VICTORY_CARD_BUFFERS_MB=8192`, 6.8 min in the world, no removal)
| ms / frame (longest) | system memory (`perf-gpucmd`) | VRAM (`perf-card8g`) |
|---|---:|---:|
| GPU in all | **157.3** | **74.7** |
| `indirect [compute 3592426e3e9142d8]` | 42.73 (554) | 0.49 (6.7) |
| `indirect [compute cfd83392ec9ae408]` | 21.68 (335) | 0.07 (1.7) |
| `indirect [compute f22a76fcb967ee59]` | 15.83 (46) | 0.83 (2.8) |
| `dispatch [compute cb6adcc6da2613ff]` | 6.46 | 8.52 |

98 copies (945 MB) went to the card; 4,446 smaller copies (4,051 MB) stayed in system memory, below the 1 MiB floor. `VICTORY_CARD_BUFFERS_MIN_KB` (test `card_limits_take_a_floor_in_kib_over_mib`, red first) lowers the floor to one 64 KiB piece; `perf-vram-all` (64 KiB, 12 GB, with the speed switches) is queued after `perf-vram-fast` (1 MiB, 8 GB, with the speed switches, on an idle CPU).
Note: the PC had 5.5 GB free of 64 GB during the run (25 GB private in the process, 4 GB in the batch translate beside it).

### Whole frame with buffers in VRAM (no DRED, with the speed switches; all 6.5-6.7 min, no removal)
| Run | Card copies | ms / frame | Preparing (copies / textures) | GPU wait | Back |
|---|---|---:|---|---:|---:|
| `perf-fast` (default floor 16 MiB, 1,024 MB) | — | 601 | 213 (50 / 135) | 58 | 29 |
| `perf-vram-fast` (1 MiB, 8 GB) | about 100, 945 MB | **584** | 181 (53 / 104) | 47 | 27 |
| `perf-vram-all` (64 KiB, 12 GB) | 3,698, 1,193 MB | 681 | 215 (64 / 118) | 57 | 24 |

- Halving the GPU time moves the frame little: with the late wait the GPU already overlaps the CPU, so **the frame is CPU-bound**: about 330 ms outside submissions and 100-470 ms of textures.
- Small card copies cost more CPU (ranges sent, pieces compared on the card) than they save on the GPU while the CPU is the limit: keep the 1 MiB floor; small buffers want F08's in-place heap, not more card copies.
- Next: the CPU sampling profile (`perf-profile`: `VICTORY_STACKS` once a second, 130-410 s; `stack-profile.py`).

### CPU sampling profile (`perf-profile`: `VICTORY_STACKS` once a second 130-410 s, 281 dumps of 121 threads, all in the world; `stack-profile.py`, symbols from `victory.pdb` by `llvm-symbolizer`)
Caveat: each comparison run starts with an empty shader cache, so pipeline compiling is overstated against a normal session.
- **Eight `victory-pipelines` threads spend about 50% each in `nvgpucomp64.dll` (NVIDIA's shader compiler)** 6 minutes into the world: about four cores compiling pipelines throughout.
- **Five command-recording threads** (the title's) are each about 50% busy in VIctory's draw translation: `draw::State::execute_indirect` 4-9%, `table_entry` / `tables` 4-11%, `prepare_for` 3-4%, **`texture::staging` 2-8% with the top frame in `win32u.dll` (kernel)**, **`descriptor::host::make` 1-5% in `win32u.dll`**, `draw::readable`, `descriptor::mirror`.
- **The submission thread**: `host::read` 6%, `HostList::run_deferred` 5%, `Host::execute_late` 5%, **`resource::let_go_idle` 4% in `win32u.dll`**, `HostList::reset` 3%, `Host::submit` 3%, write-watch queries (`watch::ask_kernel`, `uncommitted_end`) 2% each.
- Kernel trips from resource creation and release in the frame (`texture::staging`: one committed upload buffer per title texture, cached behind one global lock, recreated after eviction; `let_go_idle`; descriptor making) are the first CPU targets, then the draw-translation path, then pipeline compiling measured with a warm cache.
