use std::io;
use std::io::Write;
use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use hbc_decomp::{BytecodeFile, BytecodeFormat};
use ratatui::Terminal;

pub mod app;
pub mod background;
pub mod content;
pub mod diff;
pub mod events;
pub mod formatting;
pub mod ui;

use app::App;
use events::run_loop;

pub(crate) fn debug_log(message: &str) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let line = format!("[{ts:.3}] {message}");
    // Only write to file — never to stderr/stdout, as that corrupts the TUI.
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/hermes-decomp-tui.log")
    {
        let _ = writeln!(file, "{line}");
    }
}

pub fn run_tui(
    file: BytecodeFile,
    format: BytecodeFormat,
    path: String,
    diff_target: Option<(BytecodeFile, BytecodeFormat, String)>,
    diff_code: bool,
) -> io::Result<()> {
    debug_log(
        "TUI logging enabled: also writing to /tmp/hermes-decomp-tui.log",
    );
    debug_log(&format!(
        "[TUI] Starting setup (diff_target: {}, diff_mode: {})",
        diff_target.is_some(),
        if diff_code { "code" } else { "assembly" }
    ));
    let init_started = Instant::now();
    let mut app = App::new(file, format, path, diff_target, diff_code);
    debug_log(&format!(
        "[TUI] App initialized in {:.2?}. Opening terminal UI...",
        init_started.elapsed()
    ));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    debug_log("[TUI] Entered alternate screen. Running event loop.");

    let result = run_loop(&mut terminal, &mut app);
    debug_log("[TUI] Event loop exited. Restoring terminal.");

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}
