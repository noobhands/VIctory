## F01: Bootstrap `fork-base` and record its baseline
**Status:** DONE
**What to build:** The fork's tree as our base, verified on this machine: its tests, clippy and policy check as they stand, and its live baseline recorded, so later changes are measured against it.
**Blocked by:** None — can start immediately
**Requirements addressed:** baseline for F02-F06
- [x] Branch `fork-base` from `4e17478` in `<work>\fork-wt`; `docs/agents/issue-tracker.md` as on `w64-port` (local-first). Evidence: `git -C <work>/fork-wt branch --show-current` = `fork-base`.
- [x] Checks recorded (table below): all pass as they stand.
- [x] Live baseline: two runs, both reached the timeout without a removal (table below).
- [x] The fork's switches that are off by default, below.

### Checks (fork `4e17478`, Rust 1.99 MSVC)
| Run | Result |
|-----|--------|
| `cargo test --workspace --locked` | 496 passed, 0 failed, 7 ignored |
| diagnostics tests, `victory-gpu` (`--test-threads=1`) | 117 passed, 1 ignored |
| diagnostics tests, `victory-shader` (`--test-threads=1`) | 210 passed, 2 ignored |
| clippy, default and `victory/diagnostics` (`-A deprecated`) | 0 findings |
| `cargo xtask policy` | 177 files ok |

### Live baseline (RTX 4080; diagnostics build of `4e17478`, SHA-256 prefix `7bd4126f92d579d8`)
Plain settings with DRED, Enter from 40 to 100 s, 480 s, isolated cold cache and a copy of the same saves (`<work>\vrun-asm-tools\cmp-run.sh`; logs `<work>\vrun-logs\cmp-fork{,-2}.*`). Averages over frame lines at or below 5 fps (`t08-frame-costs.py 5`).

| Run | World min | Removal | Draws / dropped per frame | Frame ms | Preparing (copies / textures) | GPU | Copying back |
|---|---:|---|---|---:|---|---:|---:|
| `cmp-fork` | 7.0 | none | 1,915 / 845 | 1,049 | 463 (49 / 280) | 292 | 95 |
| `cmp-fork-2` | 6.6 | none | 2,104 / 926 | 905 | 324 (54 / 144) | 190 | 95 |

For comparison on the same setup: upstream `ffe310e` was removed 0.7 minutes in (`cmp-base`); `w64-port` stayed up drawing 223 per frame with 1,495 wave64 mesh drops refused (`cmp-ours`).

### Switches off by default (from the fork's README and where they are read)
`VICTORY_LATE_WAIT`, `VICTORY_RELAY` (the README's faster pair: `VICTORY_RELAY=2`, or `VICTORY_LATE_WAIT=3 VICTORY_WIDE_BACK=1 VICTORY_CARD_BUFFERS_MIN_MB=1 VICTORY_CARD_BUFFERS_MB=1024`), `VICTORY_WIDE_BACK`, `VICTORY_INPLACE`, `VICTORY_BVH_BUILDER`, `VICTORY_WAVE_WIDE`. On by default with an off switch: card buffers (`VICTORY_CARD_BUFFERS*`), clean-up passes (`VICTORY_SHADER_OPT=0`). Trying them belongs to F06.

### Execution log
### Attempt 1 — 2026-11-24
**Status:** TO-DO -> IN PROGRESS -> DONE
#### What was done:
- Worktree `<work>\fork-wt` at the fork's `main`, branch `fork-base`; dossier `docs/features/fork-base/`; tracker settings copied.
- Checks and two live runs as above.
#### What failed:
- Nothing.
#### What worked:
- The fork builds and passes its own checks unchanged with the machine's Rust 1.99 (the tree pins 1.98.1).
#### Files created/modified:
| File | Action |
|------|--------|
| `docs/agents/issue-tracker.md` | Created (copy) |
| `docs/features/fork-base/04-tickets.md`, `tickets/F01`-`F06` | Created |
