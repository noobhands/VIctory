//! `VICTORY_STORE_TRACE=<file>` (diagnostics): every write the host makes
//! into title memory (what shaders stored, copied back) and every dispatch
//! of the ray-tracing structure builder, one line each, in order, flushed
//! as written (the run this is for ends in a crash). A title pointer found
//! overwritten is then traced to the store that put the bytes there, or to
//! none (the title wrote them itself).

use std::fs::File;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

struct Trace {
    out: Mutex<File>,
    start: Instant,
}

/// The open trace, if `VICTORY_STORE_TRACE` names a file that could be made.
fn trace() -> Option<&'static Trace> {
    static TRACE: OnceLock<Option<Trace>> = OnceLock::new();
    TRACE
        .get_or_init(|| {
            let path = std::env::var_os("VICTORY_STORE_TRACE")?;
            let out = File::create(path).ok()?;
            Some(Trace { out: Mutex::new(out), start: Instant::now() })
        })
        .as_ref()
}

/// Whether the trace is on.
pub(crate) fn on() -> bool {
    trace().is_some()
}

/// One line for a write of `bytes` at title address `at`: the time in
/// microseconds since the trace opened, the frame, the address, the length
/// and the first 16 bytes written (hex).
pub(crate) fn wrote(at: usize, bytes: &[u8]) {
    let Some(t) = trace() else { return };
    let head: String = bytes.iter().take(16).map(|b| format!("{b:02x}")).collect();
    line(t, &format!("W {at:#x} {:#x} {head}", bytes.len()));
}

/// One line for something other than a write (a builder dispatch: `what`
/// says which, with its roots).
pub(crate) fn note(what: &str) {
    let Some(t) = trace() else { return };
    line(t, what);
}

/// Writes `text` with the time and frame in front, and flushes it.
fn line(t: &Trace, text: &str) {
    let us = t.start.elapsed().as_micros();
    let frame = crate::frame::current();
    let mut out = t.out.lock().unwrap_or_else(|e| e.into_inner());
    let _ = writeln!(out, "{us} {frame} {text}");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_trace_is_off_without_its_switch() {
        // The test process does not set VICTORY_STORE_TRACE.
        if std::env::var_os("VICTORY_STORE_TRACE").is_none() {
            assert!(!super::on());
        }
    }
}
