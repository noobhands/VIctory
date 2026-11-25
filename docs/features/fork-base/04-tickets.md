# Master ticket: the fxceless fork as the working base

Move the work onto the fxceless VIctory fork (`fxceless/victory-fork` `main`, `4e17478`; upstream
`Marvisak/VIctory` `ffe310e` plus the fork's changes, GPL-3.0-or-later), which ran 7.0 minutes in the
world on the RTX 4080 without a removal, drawing every wave64 mesh program, where upstream was removed
after 0.7 minutes and `w64-port` stayed up only by refusing them (`<work>\vrun-logs\cmp-{fork,base,ours}`).
Carry over what the fork lacks from `w64-port`, then continue on its open problems. Decided by the
user on 2026-11-24 (route 2 of the wave64-mesh investigation; evidence in `w64-port`'s
`docs/features/wave64-mesh/tickets/T08`-`T10`). Branch `fork-base` in the worktree `<work>\fork-wt`.
Remote ID: _(unpublished, local-first)_

| File | Title | Blocked by | Remote | Status |
|------|-------|------------|--------|--------|
| [F01](tickets/F01-bootstrap-and-baseline.md) | Bootstrap `fork-base` and record its baseline | — | — | DONE |
| [F02](tickets/F02-view-limit-key-and-probe.md) | The view limit in the shader cache key, and a probe that waits for the device | F01 | — | DONE |
| [F03](tickets/F03-carry-over-diagnostics.md) | Carry over the diagnostics the fork lacks | F01 | — | DONE |
| [F04](tickets/F04-translator-gap-diff.md) | Translator coverage: fork versus `w64-port`, and the gaps to carry over | F01 | — | TO-DO |
| [F05](tickets/F05-close-wave64-investigation.md) | Close the wave64-mesh investigation (its T06) | F01 | — | TO-DO |
| [F06](tickets/F06-next-targets.md) | Next targets on the fork: frame time, stability margin, drops | F01-F04 | — | TO-DO |
| [F07](tickets/F07-frame-time-decomposition.md) | Where a frame's time goes (to 33 ms) | F01 | — | IN PROGRESS |
| [F08](tickets/F08-title-memory-in-place.md) | Title memory used in place by the GPU | F07 | — | IN PROGRESS |
| [F09](tickets/F09-gpu-hotspots.md) | GPU time hotspots | F07 | — | TO-DO |
| [F10](tickets/F10-texture-path.md) | Texture uploads and detiling | F07 | — | IN PROGRESS |
| [F11](tickets/F11-large-dispatches.md) | Dispatches of more than 65,535 groups in a dimension | F01 | — | TO-DO |
| [F12](tickets/F12-ray-tracing-builder-wave64.md) | The ray-tracing builder's last refused programs on 32-lane waves | F01 | — | DONE |
| [F13](tickets/F13-pooled-gpu-allocations.md) | GPU allocations sub-allocated from pooled heaps | F07 | — | IN PROGRESS |
| [F14](tickets/F14-allocator-contention.md) | The process heap's lock (allocator contention) | user | — | IN PROGRESS |
| [F15](tickets/F15-intro-video-regression.md) | The intro video flickers and zooms (regression) | — | — | DONE |
| [F16](tickets/F16-builder-memory-coverage.md) | The builder sees all the title memory it reads (ray-traced lighting) | F12, F08 | — | IN PROGRESS |
| [F17](tickets/F17-shadow-trace-programs.md) | The shadow-mask trace programs that do not translate | — | — | TO-DO |
