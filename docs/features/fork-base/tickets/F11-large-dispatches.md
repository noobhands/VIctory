## F11: Dispatches of more than 65,535 groups in a dimension
**Status:** TO-DO
**What to build:** The console runs 1-D compute dispatches of more than 65,535 groups (115,200 seen, `perf-cargs`); D3D12 allows at most 65,535 per dimension, and the indirect clamp (`indirect.rs`, kind 2) cuts such a record to 65,535, so the dispatch does part of its work and the rest of its output is missing. Split them: run (65,535, ceil(n / 65,535), 1) and have the translated shader rebuild the console's group index (y * 65,535 + x) and leave when it is past n, for direct and indirect dispatches alike.
**Blocked by:** F01
**Requirements addressed:** correct shader output
- [ ] Which programs ask for more (log the pipeline with the over-limit count).
- [ ] Translator: the group index rebuilt from a 2-D dispatch, behind the host's dispatch size, with a test on WARP comparing against a 1-D dispatch.
- [ ] Clamp and direct dispatch: the split arguments.
