## F09: GPU time hotspots
**Status:** TO-DO
**What to build:** The programs and passes that make up the GPU time (190-292 ms per frame for about 2,000 draws, where the console takes under 33 ms in all), each explained (long loops, waterfall loops, exchanges, overdraw, missing clean-up, oversize dispatches) and fixed in the translator or the GPU layer, largest first.
**Blocked by:** F07
**Requirements addressed:** 30 fps
- [ ] Top programs by GPU time from `perf-gputime`, each with its translated DXIL inspected.
- [ ] Fixes with a measured before / after per program.
