## F17: The shadow-mask trace programs that do not translate
**Status:** TO-DO
**What to build:** The five shadow-mask trace programs (of the title's 64 programs that walk its ray-tracing structures) translated and made into host pipelines, so the shadows of lamps and the sun are traced once the builder runs (F16). Today 59 of 64 trace programs translate (`docs/fork-ray-tracing.md`, "Known gaps").
**Blocked by:** None — can start immediately (translation is offline; seeing the shadows needs F16)
**Requirements addressed:** correct shader output (shadows)

### Plan
1. Name the five: a corpus translate with `VICTORY_WAVE_WHY=1` and the host's 32-lane setting (`victory translate --pipelines`, see the diag workflow notes), filtered to the trace programs (`image_bvh*_intersect_ray`), with the refusal reason of each.
2. Per reason, the smallest fixture from the program's pattern and a test that fails first (`emit/wave64_tests.rs` or `lower.rs` tests), then the translation; WARP and RTX 4080 runs as in F12.
3. Offline: all five make host pipelines; live: none in the drop list ("dropped ... without a host pipeline").
- [ ] The five named, each with its refusal
- [ ] Each translates and makes a host pipeline (tests red first)
- [ ] Live: no trace program dropped
