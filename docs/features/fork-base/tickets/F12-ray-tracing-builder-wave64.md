## F12: The ray-tracing builder's last refused programs on 32-lane waves
**Status:** IN PROGRESS
**What to build:** The four compute programs the RTX 4080 refuses live (`02384a60fda6f2d0`, `2cc4c197ee29e291`, `2da4beb386509b77`, `369cbe906ea72764`; each still declares 64-lane waves) translated for 32-lane waves, so the fork's builder rule lets the ray-tracing structure builder run (`VICTORY_BVH_BUILDER=1`): 1,104 draws a frame are dropped as "a stage of the ray-tracing structure builder, which is off", and the title lights everything out of direct sun by ray tracing (`docs/fork-ray-tracing.md`).
**Blocked by:** F01
**Requirements addressed:** correct shader output (lighting in shadow)

### What stops each (`VICTORY_WAVE_WHY=1`, offline, `VICTORY_VIEW_LIMIT=999999 VICTORY_HOST_WAVE=32`)
| Program | Group | Blockers | With `VICTORY_WAVE_WIDE=1` |
|---|---:|---|---|
| `369cbe906ea72764` | 64 | LDS read and write in one lane-branch region | **translates, pipeline made** |
| `2cc4c197ee29e291` | 64 | the same, mask as data, `v_mbcnt` | **translates, pipeline made** |
| `2da4beb386509b77` | 256 | the same, `v_readlane` | refused: "wave-wide branches in a thread group of several console waves" |
| `02384a60fda6f2d0` | 128 | the same, mask as data, `v_mbcnt`, `v_readlane` | refused (several console waves) |

### Plan
1. Turn `widen` on for the programs that need it only (not globally: `e9765a7` turned it off after it hung on `ptfx_CS_lightAtlas_Sprite` / `_Trail`, barriers inside branches and loops).
2. Extend `widen` to groups of several console waves: the two 32-lane halves of each console wave meet through group-shared arrival counters (XCL's `docs/wave64-on-wave32.md`, `xcl_x_meet`: double-buffered slots, counted arrivals, a bounded wait that falls back to half-local values instead of a TDR), not group barriers that wait for the other console waves.
3. A WARP and RTX 4080 test per form with a fixture from each program's pattern; then a live run with `VICTORY_BVH_BUILDER=1 VICTORY_BVH_QUARANTINE=1` first.
- [x] 1. `widen` as the last resort (`emit/mod.rs`: after every other form, for a program still at 64 lanes; `VICTORY_WAVE_WIDE=0` turns it off, `=1` keeps the early place). Offline, no switches: `369cbe906ea72764` and `2cc4c197ee29e291` translate for 32 lanes and make pipelines. Live (`f12-widen-live`, best settings, 480 s): 6.7 min in the world, no removal, no crash; both have host pipelines and no refused drops (before: refused). Checks: workspace 501, diagnostics 122 / 211, clippy clean.
- [x] 2. `widen` for groups of several console waves: their halves meet pair by pair (below). Offline, no switches, all four programs translate for 32 lanes and make pipelines (`02384a60fda6f2d0`: 15 branches wave-wide; `2da4beb386509b77` after the descriptor fix below).
- [ ] 3. Live with `VICTORY_BVH_BUILDER=1 VICTORY_BVH_QUARANTINE=1`, then without quarantine.

### Pair-by-pair meetings (step 2)
- `lower.rs`: with branches wave-wide in a group of several whole console waves (`pairs`: 8 LDS dwords a console wave), the two host waves of a console wave meet through their own arrival counters instead of group barriers (`halves_meet`, `pair_wait`, as XCL's `xcl_x_meet`): slots double-buffered by the parity of the meeting number; a fence of LDS; `waveIsFirstLane` so every lane of the host wave has fenced; one `atomicrmw add`; polls of the other half's count by `cmpxchg 0, 0` (an add of 0 can be hoisted, XCL #1244) at most 2^20 times; a wait that runs out marks the console wave dead, so later meetings do not wait (wrong values, never a hung GPU). Used by `exchange_halves`, the `v_readlane` exchange, `lds_meeting` and the barrier before each split LDS access. The entry block, which every thread runs before any branch, keeps the group barrier exchange; a meeting outside the block being lowered is refused (`PAIR_OUTSIDE`).
- `bitcode.rs` `split_front`: the block a meeting is in is split, so what it held runs before the loop and what callers add after runs after (their saved block stays valid).
- `emit/mod.rs` `widen`: groups that are a multiple of 64 threads allowed.
- Tests: `a_split_block_keeps_its_end_and_gives_its_start_away` (red first); `two_console_waves_meet_pair_by_pair_inside_a_wave_wide_branch` (red first: static check of the DXIL, no group barrier past the entry block and `cmpxchg` polls; then **run on the RTX 4080**: 128 threads, each console wave's OR of its threads below s0, with s0 = 128 / 100 / 70 / 40, one console wave skipping the branch while the other meets: all correct, no hang).

### Descriptor built over registers that held no descriptor (`2da4beb386509b77`)
With its branch wave-wide the program refused "a descriptor neither loaded nor built" at 0x4d0: `image_sample_l` uses a sampler built from constants in s[0:3] inside a skip (0x4ac-0x4bc) and read after the join by every lane; on the skip edge s[0:3] hold lane masks of earlier code. `resource.rs` `phi`: when no single edge is proved (`only_edge`), one edge that makes a descriptor nameable anywhere while every other holds only what cannot be one (`as_got`: constants, registers as the program got them, ALU results; not a load or a join) gives that descriptor. Test `a_sampler_built_in_a_wave_wide_skip_over_input_registers_is_the_built_one` (red first, the program's error).
Checks: workspace 506 passed (twice; one earlier run stopped on the known flaky GPU test), diagnostics `victory-gpu` / `victory-shader` 124 / 214, clippy clean both configurations, policy ok.

### Live (step 3)
| Run | Settings | World min | Result |
|---|---|---:|---|
| `f12-bvh-quarantine` | `VICTORY_BVH_BUILDER=1 VICTORY_BVH_QUARANTINE=1` + speed switches, no DRED | 0.7 | the builder ran (no "builder is off" drops); device hung |
| `f12-bvh-q-dred` | the same with DRED, no late wait | 0.8 | hung at `dispatch 1x1x1 [compute 02384a60fda6f2d0]` (the 128-thread program meeting pair by pair) |
| `f12-bvh-q-dred2` | after zeroing the pair records | **3.6 (300 s run)** | **no removal, no crash**; builder on; drops 209 a frame (about 900 with it off) |

Cause of the hang: LDS is not cleared between groups (D3D12), and NVIDIA leaves what earlier groups wrote: the pairs' arrival counters started unequal (or the dead flag set), so the halves waited out their polls or did not wait, and the builder computed garbage. The test passed because the same program ran again, leaving both counters equal. Fixed: the entry block zeroes the records (each thread a word, modulo their size) and the group meets there; the test checks the zeroing (written after the fix, so not red first).
