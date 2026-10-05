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
use std::path::{Path, PathBuf};
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
    #[cfg(test)]
    let _ = sink();
    let resolved = resolve_sink(
        std::env::var("SWITCHBOARD_TRACE").ok(),
        std::env::var("SWITCHBOARD_TRACE_FILE").ok(),
        || state::state_file("trace.log"),
    );
    SINK.get_or_init(|| resolved);
}

/// Where trace lines go: nowhere unless `SWITCHBOARD_TRACE` is non-empty, then
/// `SWITCHBOARD_TRACE_FILE` when that is non-empty, else the state-dir default.
fn resolve_sink(
    trace: Option<String>,
    file: Option<String>,
    default: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    if !trace.is_some_and(|v| !v.is_empty()) {
        return None;
    }
    file.filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(default)
}

/// Whether tracing is on. Callers use this to skip work that only exists to be
/// measured (an extra `Instant::now`, a formatted detail string).
pub fn enabled() -> bool {
    sink().is_some()
}

/// The resolved trace file, if tracing is on.
///
/// The test build always traces, into a scratch file of its own: every
/// `if trace::enabled()` branch then runs under the suite, and a test can read
/// back what was written instead of trusting that a line would have been.
fn sink() -> Option<&'static PathBuf> {
    #[cfg(test)]
    SINK.get_or_init(|| Some(test_sink()));
    SINK.get().and_then(Option::as_ref)
}

#[cfg(test)]
fn test_sink() -> PathBuf {
    std::env::temp_dir().join(format!("swb-trace-{}.log", std::process::id()))
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
    let Some(path) = sink() else {
        return;
    };
    append(path, &line(since_start_ms(), label, duration, detail));
}

/// One tab-separated trace line, in the format the module docs describe.
fn line(at_ms: f64, label: &str, duration: Option<f64>, detail: Option<&str>) -> String {
    let dur = match duration {
        Some(ms) => format!("{ms:.2}"),
        None => "-".into(),
    };
    format!("{at_ms:.2}\t{label}\t{dur}\t{}\n", detail.unwrap_or("-"))
}

/// Append `line` to `path`, creating its directory. Every failure is swallowed.
fn append(path: &Path, line: &str) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracing_is_off_unless_asked_for_and_then_finds_a_file() {
        let fallback = || Some(PathBuf::from("/state/trace.log"));
        assert_eq!(resolve_sink(None, Some("/x".into()), fallback), None);
        assert_eq!(resolve_sink(Some(String::new()), None, fallback), None);
        assert_eq!(
            resolve_sink(Some("1".into()), Some("/tmp/t.log".into()), fallback),
            Some(PathBuf::from("/tmp/t.log"))
        );
        assert_eq!(
            resolve_sink(Some("1".into()), Some(String::new()), fallback),
            Some(PathBuf::from("/state/trace.log"))
        );
    }

    #[test]
    fn a_line_is_four_tab_separated_columns() {
        assert_eq!(
            line(1.5, "draw", Some(2.25), Some("17")),
            "1.50\tdraw\t2.25\t17\n"
        );
        assert_eq!(line(0.0, "start", None, None), "0.00\tstart\t-\t-\n");
    }

    #[test]
    fn lines_are_appended_into_a_directory_made_for_them() {
        let dir = std::env::temp_dir().join(format!("swb-trace-{}", std::process::id()));
        let path = dir.join("nested/trace.log");
        append(&path, "a\n");
        append(&path, "b\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nb\n");
        let _ = std::fs::remove_dir_all(&dir);
        // An unwritable path is swallowed rather than surfaced.
        append(Path::new("/dev/null/not-a-dir/trace.log"), "c\n");
    }

    /// The test build traces into its own scratch file, so every entry point
    /// can be checked by what it actually wrote.
    #[test]
    fn every_entry_point_writes_its_line_to_the_sink() {
        init();
        assert!(enabled());
        let started = Instant::now();
        let tag = format!("probe-{}", std::process::id());
        mark(&format!("{tag}-mark"));
        mark_with(&format!("{tag}-mark-with"), "d");
        span(&format!("{tag}-span"), started);
        span_with(&format!("{tag}-span-with"), started, "d");
        assert!(since_start_ms() >= 0.0);
        let written = std::fs::read_to_string(test_sink()).unwrap();
        for suffix in ["mark", "mark-with", "span", "span-with"] {
            assert!(
                written.contains(&format!("\t{tag}-{suffix}\t")),
                "{suffix} missing from the trace"
            );
        }
    }
}
