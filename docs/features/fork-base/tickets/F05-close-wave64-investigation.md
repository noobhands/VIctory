## F05: Close the wave64-mesh investigation (its T06)
**Status:** TO-DO
**What to build:** `w64-port`'s T06 decision record written: D1 (wave64 mesh hangs because first-lane reads see a 32-lane half) is not the main cause; the fork draws every wave64 mesh program per half without hanging; the main cause found is view indices past NVIDIA's capped heap (T10), with the rest of the fork's stability lead unexplained; the work continues on `fork-base`. Its open questions (the 183 programs, `eeed82b877b44b5f`) carried as notes, not tickets.
**Blocked by:** F01
**Requirements addressed:** the investigation's FR-07 / FR-08 closure
- [ ] T06 written on `w64-port`, T08-T10 closed with their remaining items stated.
- [ ] `w64-port`'s own commits (T08 loop-exit switch, T10 clamp port, dossier) committed there or left, as the user decides.
