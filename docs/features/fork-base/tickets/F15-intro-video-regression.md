## F15: The intro video flickers and zooms (regression)
**Status:** DONE
**What to build:** Reported by the user on 2026-11-25: the opening logo video flickers and zooms on the fork, though it runs at 30 fps; it played correctly before. Find the change that did it and fix it.
**Blocked by:** None
**Requirements addressed:** correct output
- [x] Captures of consecutive intro frames compared across builds and settings (evidence below).
- [x] The change found: the fork's texture guard (`texture.rs` `refresh_timed`, `VICTORY_TEXTURE_GUARD`).
- [x] Fixed with the guard kept on: a texture found written through another view is hashed at every check. The user confirmed the intro correct (run J).

### Evidence
Capture runs (`<work>\vrun-asm-tools\intro-bisect2.sh` / `-3.sh`: 90 s, one Enter at 40 s, frames 1170-1249 = the start of the intro's illustrated montage, found by `intro-find-D`), measured by `cycle-check.py` (frames that differ from the previous one but repeat the one 3 back) and judged by the user on screen:
| Run | Build / settings | Frames changed | of them repeating the frame 3 back | User |
|---|---|---:|---:|---|
| A | fork-base as of F03, none | 53 | 46 | poor |
| B | current fork-base, none | 69 | 60 | poor |
| C | B + `VICTORY_LATE_WAIT=3 VICTORY_WIDE_BACK=1` | 69 | 60 | poor |
| D | C + VRAM card buffers | 18 (another window) | 2 | poor |
| E | `w64-port` (upstream-based) | 51 | **0** | good (perfect without captures) |
| F | upstream `ffe310e` | 77 | **0** | good (perfect without captures) |
| G | B + `VICTORY_HEAP_TEXTURES=0` | 77 | 68 | poor |
| H | B + `VICTORY_TEXTURE_GUARD=0` (no capture) | — | — | **good** |
| J | B + the fix, guard on (no capture) | — | — | **good** |
The fork showed three video frames in rotation, a new one entering about every 8 frames: Bink decodes into three textures in turn.

### Cause
The fork guards textures in view memory (no write-watch there) read-only to learn of the title's writes; "what the guard cannot see (a write to the same pages through another view) is still found by the hash, which is taken every eighth frame only" (`refresh_timed`). Bink writes each decoded frame through another view of the texture's pages: the guard never fires, the hash every eighth frame catches each of the three textures up to eight frames late, and the screen cycles through stale frames.

### Fix
`Last::aliased`: set when the hash finds a change the guard did not see (`written_elsewhere`); such a texture is hashed at every check (`hash_skipped`). Others keep the guard and the eighth-frame hash. Test `a_texture_written_through_another_view_is_hashed_at_every_check` (red first, then green).

### Execution log
### Attempt 1 — 2026-11-25
**Status:** TO-DO -> IN PROGRESS -> DONE
#### What failed:
- The first captures (45 s, keys off) never reached the intro: the title waits at the account picker for a button. The capture's PNG writing (3-4 MB a frame) made the start of each captured intro stutter; judged without captures afterwards (E, F, H, J), as the user asked.
- G (the queue of textures picked by index off) was the first suspect and was wrong.
#### Tests run:
| Run | Result |
|-----|--------|
| `a_texture_written_through_another_view_is_hashed_at_every_check` | fail before, pass after |
| `cargo test --workspace --locked` | 503 passed, 7 ignored |
| diagnostics `victory-gpu` / `victory-shader` (`--test-threads=1`) | 124 / 211 passed |
| clippy, default and `diagnostics,fast-alloc` (`-A deprecated`) | 0 findings (after `recycle::fits` was used by the lookup again) |
| `cargo xtask policy` | 187 files ok |
| live J (user) | intro correct |
#### Files modified:
| File | Action |
|------|--------|
| `crates/victory-gpu/src/texture.rs` | Modified |
| `crates/victory-gpu/src/recycle.rs` | Modified (lookup uses `fits`) |
