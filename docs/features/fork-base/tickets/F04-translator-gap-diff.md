## F04: Translator coverage: fork versus `w64-port`, and the gaps to carry over
**Status:** TO-DO
**What to build:** The corpus (`<work>\vrun-shaders`, 2,175 programs) translated by both trees as on a capped NVIDIA host, the outcomes compared program by program, and any program `w64-port` translates that the fork refuses traced to the commit that fixed it (candidates: `v_frexp_exp_i32_f32` in `52c43f5`, the mesh primitive index cull `e33ed1b`), then carried over.
**Blocked by:** F01
**Requirements addressed:** dropped draws
- [ ] Both outcome lists (`translate --pipelines --list`, `VICTORY_VIEW_LIMIT=999999` on the fork) and the per-program diff.
- [ ] Each `w64-port`-only success traced and carried over with its test, or recorded as not needed.
- [ ] T09's tests B and C repeated on the fork (refusals ranked by program and by dropped draws).
