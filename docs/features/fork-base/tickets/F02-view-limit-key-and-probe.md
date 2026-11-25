## F02: The view limit in the shader cache key, and a probe that waits for the device
**Status:** DONE
**What to build:** Two defects of the fork's view-heap clamp fixed as on `w64-port` T10: the clamp's limit is part of the shader cache key (a cache written without it would be reused on a capped host without the clamp), and the cap probe, called before the device exists, asks again later instead of being used up (`std::sync::Once`).
**Blocked by:** F01
**Requirements addressed:** stability (NVIDIA heap overrun)
- [x] Cache key test with a "view limit" case, failing first. Evidence: `each_translation_setting_changes_the_key` failed to build before (`cannot find function settings`), passes after.
- [x] Probe call order checked: **no defect**. `host()` (`crates/victory-gpu/src/host.rs`, `OnceLock<Option<Host>>`) makes the device on first use and is `None` only when there is no GPU, for good; the probe's own call to `host()` makes the device, so its `Once` cannot be used up before a device exists. Left unchanged (a retrying version was written, then reverted).
- [x] Written to go upstream on its own: one file of logic (`shader_cache.rs`) and a getter (`emit::view_heap_limit`).

### Change
- `crates/victory-gpu/src/shader_cache.rs`: the key's settings moved into a pure `settings()` (loop limit, group sync, the form switches `FORM_SWITCHES` with their values, clean-up passes, **view limit**), so a test can check each changes the key. `build()` runs `probe_view_limit()` first, so the limit is known before the first key, and appends `view_heap_limit()`. Existing caches are keyed afresh (the key bytes changed).
- `crates/victory-shader/src/emit/shader.rs`, `mod.rs`: `view_heap_limit()` getter, exported.

### Execution log
### Attempt 1 — 2026-11-24
**Status:** TO-DO -> IN PROGRESS -> DONE
#### What was done:
- Test first (red: no `settings`), then `settings()` and the key change (green).
- Probe: a version retrying until the device exists was written, then reverted after reading `host()`: the defect does not exist (the same note was added to `w64-port`'s T10, which carries that unneeded change).
#### What failed:
- The second defect was a misreading on my part; nothing to fix.
#### What worked:
- The key change, as tested.
#### Tests run:
| Run | Result | What it confirmed |
|-----|--------|-------------------|
| `each_translation_setting_changes_the_key` | fail (build) before, pass after | the view limit and every other setting change the key |
| `cargo test --workspace --locked` | 497 passed, 7 ignored | no regressions |
| diagnostics tests `victory-gpu` / `victory-shader` (`--test-threads=1`) | 118 / 210 passed | no regressions |
| clippy, both configurations (`-A deprecated`) | 0 findings | lint clean |
| `cargo xtask policy` | 177 files ok | policy |
#### Files created/modified:
| File | Action |
|------|--------|
| `crates/victory-gpu/src/shader_cache.rs` | Modified |
| `crates/victory-shader/src/emit/shader.rs` | Modified |
| `crates/victory-shader/src/emit/mod.rs` | Modified |
