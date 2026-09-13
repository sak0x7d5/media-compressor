//! Startup timings.
//!
//! This exists because "it feels slow" and "it hangs" are the same sentence
//! from the outside, and they have completely different causes. The marks turn
//! that into a number per stage.
//!
//! Off unless `MEDIA_COMPRESSOR_TRACE` is set in the environment. A release
//! build has no console to print to — `windows_subsystem = "windows"` detaches
//! it — so the marks go to a file instead:
//!
//! ```text
//! %TEMP%\media-compressor-startup.log
//! ```
//!
//! The clock starts at the first mark, which is the first line of `main`.
//! Everything before that — process creation, DLL loading, the antivirus
//! reading the binary — is invisible here by construction, so a trace that
//! totals 300 ms for a launch that took four seconds is itself the finding:
//! the time went somewhere Windows owns, not somewhere this code does.

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Set this to anything to turn tracing on.
pub const ENABLE_ENV: &str = "MEDIA_COMPRESSOR_TRACE";

struct Trace {
    started: Instant,
    path: PathBuf,
}

static TRACE: OnceLock<Option<Trace>> = OnceLock::new();

pub fn log_path() -> PathBuf {
    std::env::temp_dir().join("media-compressor-startup.log")
}

fn trace() -> Option<&'static Trace> {
    TRACE
        .get_or_init(|| {
            std::env::var_os(ENABLE_ENV)?;

            // Start each run from an empty file. Appending across runs would
            // make two launches look like one very slow launch.
            let path = log_path();
            let _ = std::fs::remove_file(&path);

            Some(Trace { started: Instant::now(), path })
        })
        .as_ref()
}

/// UTC wall clock, so a mark can be lined up against when the icon was clicked.
fn wall_clock() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = now.as_secs() % 86_400;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60,
        now.subsec_millis()
    )
}

/// Record a checkpoint. Does nothing, and costs nothing, when tracing is off.
pub fn mark(label: &str) {
    let Some(trace) = trace() else { return };

    let line = format!("{:>6} ms  [{}]  {label}\n", trace.started.elapsed().as_millis(), wall_clock());
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&trace.path) {
        let _ = file.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty log reads as "startup did nothing", which is the opposite of
    /// what it would mean. This is the only test that touches the trace, on
    /// purpose: enablement is decided once per process and cannot be undecided.
    #[test]
    fn marks_reach_the_log_once_tracing_is_on() {
        std::env::set_var(ENABLE_ENV, "1");
        mark("a checkpoint");

        let written = std::fs::read_to_string(log_path()).expect("the log must exist");
        assert!(written.contains("a checkpoint"), "got {written:?}");
        assert!(
            written.contains(" ms  ["),
            "a mark carries both the elapsed and the wall clock: {written:?}"
        );
    }
}
