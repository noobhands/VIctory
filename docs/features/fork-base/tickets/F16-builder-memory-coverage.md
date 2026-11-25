## F16: The builder sees all the title memory it reads (ray-traced lighting)
**Status:** TO-DO
**What to build:** The ray-tracing structure builder (`VICTORY_BVH_BUILDER=1`) running on the title's real data, so its structures are right and can be copied back without quarantine, and the trace programs light what direct sun does not reach. Today its stages read zeros wherever a page has no host copy yet (F12 step 3), so every build of the loading screen is garbage.
**Blocked by:** F12 (done), F08 (title memory in place: the cheap way to give the GPU every page)
**Requirements addressed:** correct shader output (lighting in shadow)

### Plan
1. Every committed page of the title's memory reachable before the first builder dispatch: with F08, placed buffers over the title's own memory (no copy, always current), numbered in the directory up front (a walk of the title's allocations; `VICTORY_MEMORY_COPIES` sized for them).
2. Until then, a measured fallback: map all committed private memory before the first builder dispatch and see whether the fault lists empty and an unquarantined run survives (memory: about 19 GB of host copies; refresh cost per dispatch).
3. Reads of uncommitted memory after that are wrong addresses: compare a stage's inputs and outputs on the first build (`VICTORY_STORE_TRACE`) with what the title gave it.
4. Then copy-back without quarantine; captures of shadowed areas against the builder-off run.
- [ ] Fault lists empty on the first builder frame (`VICTORY_STORE_TRACE` `F` lines)
- [ ] Unquarantined run reaches the world without a crash
- [ ] Shadowed areas lit (captures against builder off)

Evidence: F12 "Without quarantine" (runs `f12-bvh-on` ... `f12-faults3`).
