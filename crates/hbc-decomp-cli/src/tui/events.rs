use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::Terminal;

use super::app::{App, ViewMode};
use super::ui::draw_ui;

pub fn run_loop(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    app: &mut App,
) -> io::Result<()> {
    loop {
        terminal.draw(|frame| draw_ui(frame, app))?;
        app.tick = app.tick.wrapping_add(1);

        // Spin faster while loading (animated spinner), idle slower.
        let loading = app.git_computing || app.pipeline_building || app.pipeline_building2;
        let timeout = Duration::from_millis(if loading { 80 } else { 200 });

        if event::poll(timeout)? {
            // Drain ALL pending events before redrawing once. Trackpad/wheel
            // scrolling emits many events per gesture; handling them one-redraw-
            // each made scrolling lag badly.
            let mut quit = false;
            loop {
                match event::read()? {
                    // Only react to key *presses*: the crossterm 0.29 enhanced
                    // protocol also sends repeats/releases, which would double
                    // every action.
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if handle_key(app, key) {
                            quit = true;
                            break;
                        }
                    }
                    Event::Mouse(me) => handle_mouse(app, me),
                    // Force a full repaint on resize so no stale cells linger.
                    Event::Resize(_, _) => terminal.clear()?,
                    _ => {}
                }
                if !event::poll(Duration::from_millis(0))? {
                    break;
                }
            }
            if quit {
                break;
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

    // Content search mode: search within the selected function's content
    if app.is_content_searching {
        match key.code {
            KeyCode::Esc => {
                app.is_content_searching = false;
                app.content_search.clear();
                app.content_search_matches.clear();
                app.content_search_index = 0;
            }
            KeyCode::Enter => {
                app.content_search_next();
            }
            KeyCode::Down => {
                app.content_search_next();
            }
            KeyCode::Up => {
                app.content_search_prev();
            }
            KeyCode::Backspace => {
                app.content_search.pop();
                app.update_content_search();
            }
            KeyCode::Char(c) => {
                app.content_search.push(c);
                app.update_content_search();
            }
            _ => {}
        }
        return false;
    }

    // Xref picker mode
    if app.xref_open {
        return handle_xref_key(app, key);
    }

    // Full-program git diff is a separate full-screen mode with its own keys.
    if app.git_diff {
        return handle_git_key(app, key);
    }

    let has_diff = app.file2.is_some();
    match key.code {
        KeyCode::Char('/') => {
            app.is_searching = true;
        }
        KeyCode::Char('s') => {
            app.is_content_searching = true;
        }
        KeyCode::Char('d') => {
            app.show_diff_colors = !app.show_diff_colors;
            app.disasm_cache.clear();
            app.decompile_cache.clear();
            app.disasm_cache2.clear();
            app.decompile_cache2.clear();
        }
        KeyCode::Char('q') => return true,
        KeyCode::Char('g') => {
            app.open_xref();
        }
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
        // `u` enters the full-program git diff (whole code, base vs modified,
        // side by side). Only meaningful when a second file is loaded.
        KeyCode::Char('u') => {
            if app.file2.is_some() {
                app.git_diff = true;
                app.scroll = 0;
                app.request_git_diff();
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

// Mouse: wheel scrolls, left click selects functions or starts text selection
// in the content pane.  On release the selected text is pushed to the terminal
// clipboard via OSC 52 so Cmd+C / Ctrl+Shift+C works.
fn handle_mouse(app: &mut App, me: MouseEvent) {
    match me.kind {
        MouseEventKind::ScrollDown => app.scroll = app.scroll.saturating_add(3),
        MouseEventKind::ScrollUp => app.scroll = app.scroll.saturating_sub(3),
        MouseEventKind::Down(MouseButton::Left) => {
            if app.git_diff {
                app.git_toggle_fold_at(me.row);
            } else if app.is_inside_content(me.column, me.row) {
                app.selection_anchor = Some((me.column, me.row));
                app.selection_target = Some((me.column, me.row));
                app.selecting = true;
            } else {
                app.select_at_row(me.column, me.row);
                app.clear_selection();
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if app.selecting {
                app.selection_target = Some((me.column, me.row));
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if app.selecting {
                app.selecting = false;
                // Only copy when the user actually dragged (anchor != target).
                if app.selection_anchor != app.selection_target {
                    app.copy_selection_to_clipboard();
                }
            }
        }
        _ => {}
    }
}

// Keys for the full-program git diff mode (no function list; both columns are
// scrolled together).
fn handle_git_key(app: &mut App, key: KeyEvent) -> bool {
    // Search input mode.
    if app.git_searching {
        match key.code {
            KeyCode::Esc => app.git_searching = false,
            // Navigate matches while the popup stays open. Use Enter/arrows so
            // letter keys (n/N) keep going into the query text.
            KeyCode::Enter | KeyCode::Down => app.git_search_next(),
            KeyCode::Up => app.git_search_prev(),
            KeyCode::Backspace => {
                app.git_search.pop();
                app.git_search_live();
            }
            KeyCode::Char(c) => {
                app.git_search.push(c);
                app.git_search_live(); // incremental: jump as you type
            }
            _ => {}
        }
        return false;
    }

    match key.code {
        KeyCode::Char('q') => return true,
        // Search within the diff. `/` opens input; n/N or Enter cycle matches.
        KeyCode::Char('/') => app.git_searching = true,
        KeyCode::Char('n') | KeyCode::Enter => app.git_search_next(),
        KeyCode::Char('N') => app.git_search_prev(),
        // Leave the git diff and go back to the normal browser.
        KeyCode::Char('u') | KeyCode::Esc => {
            app.git_diff = false;
            app.scroll = 0;
        }
        // Toggle decompiled vs disassembly; rebuild the diff for the new kind.
        KeyCode::Char('v') => {
            app.git_kind = match app.git_kind {
                ViewMode::Disasm => ViewMode::Decompile,
                _ => ViewMode::Disasm,
            };
            app.invalidate_git_diff();
            app.request_git_diff();
            app.scroll = 0;
        }
        // Toggle syntax coloring (vs. plain red/green diff tint).
        KeyCode::Char('c') => app.git_syntax = !app.git_syntax,
        // Toggle ignoring volatile Metro ids (module_955 vs module_769).
        KeyCode::Char('i') => {
            app.git_normalize = !app.git_normalize;
            app.invalidate_git_diff();
            app.request_git_diff();
            app.scroll = 0;
        }
        KeyCode::Down | KeyCode::Char('j') => app.scroll = app.scroll.saturating_add(1),
        KeyCode::Up | KeyCode::Char('k') => app.scroll = app.scroll.saturating_sub(1),
        KeyCode::PageDown => app.scroll = app.scroll.saturating_add(20),
        KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(20),
        KeyCode::Home => app.scroll = 0,
        KeyCode::End => app.scroll = u32::MAX, // clamped during rendering
        _ => {}
    }
    false
}

fn handle_xref_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            app.xref_open = false;
        }
        KeyCode::Enter => {
            app.xref_jump();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.xref_selected + 1 < app.xref_list.len() {
                app.xref_selected += 1;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.xref_selected > 0 {
                app.xref_selected -= 1;
            }
        }
        KeyCode::PageDown => {
            app.xref_selected = (app.xref_selected + 10).min(app.xref_list.len().saturating_sub(1));
        }
        KeyCode::PageUp => {
            app.xref_selected = app.xref_selected.saturating_sub(10);
        }
        _ => {}
    }
    false
}
