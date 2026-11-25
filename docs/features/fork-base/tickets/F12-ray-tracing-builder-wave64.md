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
- [ ] 2. `widen` for groups of several console waves (`2da4beb386509b77` 256 threads, `02384a60fda6f2d0` 128 threads; still refused live).
- [ ] 3. Fixture tests per form; live with `VICTORY_BVH_BUILDER=1 VICTORY_BVH_QUARANTINE=1`.
