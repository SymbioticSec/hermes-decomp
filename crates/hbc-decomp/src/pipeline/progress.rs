// User-facing progress on stderr for long decompile runs.
// Disabled by default (library / tests stay quiet). The CLI enables it.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Enable or disable progress messages on stderr.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

fn eprint_line(msg: &str) {
    let _ = writeln!(std::io::stderr(), "{msg}");
    let _ = std::io::stderr().flush();
}

/// Print a progress line when enabled. Always flushes so the user sees it live.
pub fn status(msg: impl AsRef<str>) {
    if !is_enabled() {
        return;
    }
    eprint_line(&format!("  • {}", msg.as_ref()));
}

/// Timed phase: announces start, then duration on drop / `finish`.
pub struct Phase {
    label: String,
    start: Instant,
    finished: bool,
}

impl Phase {
    pub fn start(label: impl Into<String>) -> Self {
        let label = label.into();
        if is_enabled() {
            eprint_line(&format!("  • {label}…"));
        }
        Self {
            label,
            start: Instant::now(),
            finished: false,
        }
    }

    pub fn finish(mut self) {
        self.finish_inner(None);
    }

    pub fn finish_with(mut self, detail: impl AsRef<str>) {
        self.finish_inner(Some(detail.as_ref()));
    }

    fn finish_inner(&mut self, detail: Option<&str>) {
        if self.finished || !is_enabled() {
            self.finished = true;
            return;
        }
        self.finished = true;
        let secs = self.start.elapsed().as_secs_f64();
        match detail {
            Some(d) => eprint_line(&format!("  • {}: {d} ({secs:.1}s)", self.label)),
            None => eprint_line(&format!("  • {}: done ({secs:.1}s)", self.label)),
        }
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        if !self.finished {
            self.finish_inner(None);
        }
    }
}
