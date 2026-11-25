## F03: Carry over the diagnostics the fork lacks
**Status:** DONE
**What to build:** The `w64-port` diagnostics the fork does not have, on `fork-base`, diagnostics builds only: `VICTORY_SHADER_DUMP` (each shader's console container, refusing a folder inside the repository: `a32afb6`, `7c53500`), the frame line's direct mesh dispatch count (`ae094b0`, `a5aeda4`), `VICTORY_SKIP_INDIRECT_MESH` (`9c86f74`), `VICTORY_SHADER_LOOP_EXIT` (`w64-port` T08, uncommitted there).
**Blocked by:** F01
**Requirements addressed:** observability for F06
- [x] Each switch with its test from `w64-port`. Evidence: `shader_dumps_are_named_by_hash_and_stage`, `shader_dumps_are_refused_inside_the_repository`, `mesh_dispatches_are_counted_per_frame_line`, `a_loop_has_a_single_exit_only_when_one_block_is_left_to`, and the cache key's "loop exit" case pass; live, the dump wrote 2,262 containers and the frame line reads e.g. "3016 draws/frame (320 mesh)". (Carried code with its tests, so these were not red first on this tree.)
- [x] The offline tools work with the fork's logs: `t04-analyse.py` and `t08-frame-costs.py` read `cmp-fork*` unchanged; `t09-coverage.py` now also reads the fork's `translate --list` format (`#<hash> ~<size>` before the stage), checked on both formats.

### Change
| Carried | How | Where |
|---|---|---|
| `VICTORY_SHADER_DUMP` (`a32afb6`, `7c53500`) | `git apply -3`, clean | `crates/victory-gpu/src/pipeline.rs`, `Cargo.toml`, `Cargo.lock` |
| Frame-line mesh dispatch count (`ae094b0`, `a5aeda4`) | `git apply -3`, clean; hunks checked against the fork's draw path (counted beside its `DRAWN` after `DispatchMesh`) | `draw.rs`, `host.rs` (the fork's `Meter` line) |
| `VICTORY_SKIP_INDIRECT_MESH` (`9c86f74`) | `git apply -3`, clean; before the fork's indirect handling | `draw.rs` |
| `VICTORY_SHADER_LOOP_EXIT` (`w64-port` T08) | by hand (the 3-way patch conflicted): `limit_exits`, `limit_exit`, `lane_successors`, `single_loop_exit`, the phis' added edges in `fill_phis`, the switch `emit::loop_exit`, its test; and a `loop_exit` input to F02's `settings()` key | `emit/lower.rs`, `emit/mod.rs`, `shader_cache.rs` |

Offline on the fork's lowering (`VICTORY_VIEW_LIMIT=999999 VICTORY_HOST_WAVE=32 VICTORY_SHADER_LOOP_LIMIT=64`): `eeed82b877b44b5f` validates either way; without the switch it has a loop-limit `ret` before its barrier (census flags it), with it none.

### Live (RTX 4080; `fork-base` diagnostics build, SHA-256 prefix `10145ce5945e05d3`; `cmp-run.sh`)
| Run | World min | Result | Draws / mesh per frame | Frame ms |
|---|---:|---|---|---:|
| `cmp-forkbase-f03` (+ `VICTORY_SHADER_DUMP`, 300 s) | 3.4 | **the title crashed**: null write in a title thread, `victory-image…dll+0x2215c35`; no GPU removal | 2,155 / 153 | — |
| `cmp-forkbase-f03b` (plain, 480 s) | 6.7 | timeout, no removal, no crash | 2,164 / 242 | 914 |

The fork's baseline was 7.0 / 6.6 minutes, 905-1,049 ms per frame (F01), so the carried code changes nothing measurable when its switches are off. The crash in the dump run is the title's: null writes in title threads predate this work (the seeded saves' `crashes.log` already held one at `+0x23cf683`, the same site as `w64-port` T10's validation run), and this run was slowed by writing 2,262 dumps; recorded for F06.

### Execution log
### Attempt 1 — 2026-11-24
**Status:** TO-DO -> IN PROGRESS -> DONE
#### What was done:
- Four `w64-port` commits applied as code-only patches (no commits); T08's switch ported by hand; checks; offline check; two live runs.
#### What failed:
- The T08 patch did not apply to the fork's `lower.rs` (conflicts); ported by hand. The first port copied the test module's imports and `S_ENDPGM` along with the test (13 duplicate-name errors); the duplicate block removed.
- The dump run ended in a title crash (above), judged pre-existing after the plain repeat ran clean.
#### What worked:
- Clean applies for the GPU-side diagnostics; the hand port, checked offline.
#### Tests run:
| Run | Result | What it confirmed |
|-----|--------|-------------------|
| the five tests named above | pass | the carried switches |
| `cargo test --workspace --locked` | 499 passed, 7 ignored | no regressions |
| diagnostics tests `victory-gpu` / `victory-shader` (`--test-threads=1`) | 121 / 211 passed | no regressions |
| clippy, both configurations (`-A deprecated`) | 0 findings | lint clean |
| `cargo xtask policy` | 177 files ok | policy |
| live, above | dump and mesh count work; plain run as the baseline | behaviour on the title |
#### Files created/modified:
| File | Action |
|------|--------|
| `crates/victory-gpu/src/pipeline.rs`, `draw.rs`, `host.rs`, `shader_cache.rs`, `Cargo.toml` | Modified |
| `crates/victory-shader/src/emit/lower.rs`, `mod.rs` | Modified |
| `Cargo.lock` | Modified |
| `<work>\vrun-asm-tools\t09-coverage.py` | Modified (outside the repository) |
