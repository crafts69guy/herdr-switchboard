//! Opt-in perf tracing, inert unless `SWITCHBOARD_TRACE` is set.
//!
//! The picker owns the terminal for its whole life, so a trace line must never
//! reach stdout or stderr — it would print into the middle of the TUI and, worse,
//! only on the runs you were measuring. Every line is appended to a file instead:
//! `$SWITCHBOARD_TRACE_FILE`, or `trace.log` inside [`crate::state::state_dir`].
//!
//! Format is one tab-separated line per event, so `awk` reads it without help:
//!
//! ```text
//! <ms since process start>\t<label>\t<duration ms, or ->\t<detail, or ->
//! ```
//!
//! [`init`] must run before anything else in `main`: it fixes the zero point that
//! every later `ms since process start` is measured against.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

use crate::state;

static START: OnceLock<Instant> = OnceLock::new();
/// `Some(path)` only when tracing is on; `None` makes every call below a no-op.
static SINK: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Fix the zero point and resolve the sink. Idempotent, but only the first call
/// counts — call it as the first statement of `main`.
pub fn init() {
    START.get_or_init(Instant::now);
    SINK.get_or_init(|| {
        if !std::env::var("SWITCHBOARD_TRACE").is_ok_and(|v| !v.is_empty()) {
            return None;
        }
        std::env::var("SWITCHBOARD_TRACE_FILE")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| state::state_file("trace.log"))
    });
}

/// Whether tracing is on. Callers use this to skip work that only exists to be
/// measured (an extra `Instant::now`, a formatted detail string).
pub fn enabled() -> bool {
    SINK.get().is_some_and(Option::is_some)
}

/// Milliseconds since [`init`], or 0.0 when tracing never started.
fn since_start_ms() -> f64 {
    START
        .get()
        .map(|s| s.elapsed().as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// Append one line. Failures are swallowed: a trace that cannot be written must
/// never change how the picker behaves.
fn emit(label: &str, duration: Option<f64>, detail: Option<&str>) {
    let Some(Some(path)) = SINK.get() else {
        return;
    };
    let dur = match duration {
        Some(ms) => format!("{ms:.2}"),
        None => "-".into(),
    };
    let line = format!(
        "{:.2}\t{}\t{}\t{}\n",
        since_start_ms(),
        label,
        dur,
        detail.unwrap_or("-")
    );
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// A point in time: "this happened, N ms into the process".
pub fn mark(label: &str) {
    emit(label, None, None);
}

/// [`mark`] with a detail column — an entry count, a source name, a repo path.
pub fn mark_with(label: &str, detail: &str) {
    emit(label, None, Some(detail));
}

/// A measured interval, from an [`Instant`] the caller took at the start.
pub fn span(label: &str, started: Instant) {
    emit(label, Some(started.elapsed().as_secs_f64() * 1000.0), None);
}

/// [`span`] with a detail column.
pub fn span_with(label: &str, started: Instant, detail: &str) {
    emit(
        label,
        Some(started.elapsed().as_secs_f64() * 1000.0),
        Some(detail),
    );
}
