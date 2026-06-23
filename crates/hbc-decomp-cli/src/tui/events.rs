use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;

use super::app::{App, ViewMode};
use super::ui::draw_ui;

pub fn run_loop(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    app: &mut App,
) -> io::Result<()> {
    loop {
        terminal.draw(|frame| draw_ui(frame, app))?;

        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                // Only react to key *presses*: with the crossterm 0.29 enhanced
                // protocol, repeats/releases also arrive and would double every
                // action (navigation, toggles).
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if handle_key(app, key) {
                        break;
                    }
                }
                // Force a full repaint on resize so no stale cells linger.
                Event::Resize(_, _) => terminal.clear()?,
                _ => {}
            }
        }

        app.poll_background_tasks();
    }

    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.is_searching {
        match key.code {
            KeyCode::Esc => {
                app.is_searching = false;
                app.search_query.clear();
                app.update_search();
            }
            KeyCode::Enter => {
                app.deep_search();
                app.is_searching = false;
            }
            KeyCode::Backspace => {
                app.search_query.pop();
                app.update_search();
            }
            KeyCode::Char(c) => {
                app.search_query.push(c);
                app.update_search();
            }
            _ => {}
        }
        return false;
    }

    let has_diff = app.file2.is_some();
    match key.code {
        KeyCode::Char('/') => {
            app.is_searching = true;
        }
        KeyCode::Char('d') => {
            app.show_diff_colors = !app.show_diff_colors;
            // Force redraw/recalc if needed?
            // The content() method checks this flag, so next draw will pick it up.
            // But we might want to clear cache?
            app.disasm_cache.clear();
            app.decompile_cache.clear();
            app.disasm_cache2.clear();
            app.decompile_cache2.clear();
        }
        KeyCode::Char('q') => return true,
        KeyCode::Up | KeyCode::Char('k') => {
            if app.selected > 0 {
                app.set_selected(app.selected - 1);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.selected + 1 < app.function_names.len() {
                app.set_selected(app.selected + 1);
            }
        }
        KeyCode::PageUp => {
            app.scroll = app.scroll.saturating_sub(20);
        }
        KeyCode::PageDown => {
            app.scroll = app.scroll.saturating_add(20);
        }
        KeyCode::Home => app.scroll = 0,
        KeyCode::End => app.scroll = u32::MAX,
        KeyCode::Tab => app.set_view(app.view.next(has_diff)),
        KeyCode::Char('1') => app.set_view(ViewMode::Disasm),
        KeyCode::Char('2') => app.set_view(ViewMode::Decompile),
        KeyCode::Char('3') => app.set_view(ViewMode::Info),
        KeyCode::Char('4') => {
            if has_diff {
                app.set_view(ViewMode::Diff)
            }
        }
        // `v` toggles between disassembly and decompiled code. In the split
        // diff view it flips which of the two is being diffed instead.
        KeyCode::Char('v') => match app.view {
            ViewMode::Diff => app.toggle_diff_kind(),
            ViewMode::Decompile => app.set_view(ViewMode::Disasm),
            _ => app.set_view(ViewMode::Decompile),
        },
        // `u` switches the diff view between split (side-by-side) and unified
        // (git-style single column).
        KeyCode::Char('u') => {
            if app.view == ViewMode::Diff {
                app.diff_unified = !app.diff_unified;
                app.scroll = 0;
            }
        }
        _ => {}
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('n') => {
                if app.selected + 1 < app.function_names.len() {
                    app.set_selected(app.selected + 1);
                }
            }
            KeyCode::Char('p') => {
                if app.selected > 0 {
                    app.set_selected(app.selected - 1);
                }
            }
            _ => {}
        }
    }

    false
}
