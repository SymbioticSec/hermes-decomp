use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, ViewMode};
use super::diff::DiffStatus;
use super::formatting::highlight_code;

pub fn draw_ui(frame: &mut Frame, app: &mut App) {
    if app.git_diff {
        draw_git_diff(frame, app);
        return;
    }

    let size = frame.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(2),
            Constraint::Length(1),
        ])
        .split(size);

    let diff_title = match app.diff_kind {
        ViewMode::Disasm => "Assembly",
        _ => "Source",
    };

    let header_text = if let Some(p2) = &app.path2 {
        if app.view == ViewMode::Diff {
            format!(
                "Hermes Decompiler | Diff ({}) | {} vs {}",
                diff_title, app.path, p2
            )
        } else {
            format!("Hermes Decompiler | {} vs {}", app.path, p2)
        }
    } else {
        format!(
            "Hermes Decompiler | {} | v{}",
            app.path, app.file.header.version
        )
    };

    let header = Paragraph::new(Line::from(vec![Span::styled(
        header_text,
        Style::default().add_modifier(Modifier::BOLD),
    )]));
    frame.render_widget(header, layout[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(layout[1]);

    draw_function_list(frame, app, body[0]);

    // Check if in diff mode for split view content
    if app.view == ViewMode::Diff {
        let (left_content, right_content) = app.content();

        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(body[1]);

        draw_content_pane(frame, app, split[0], left_content, "Left (v1)");
        if let Some(right) = right_content {
            draw_content_pane(frame, app, split[1], right, "Right (v2)");
        }
    } else {
        let (content, _) = app.content();
        let title = if app.view == ViewMode::Decompile && app.pipeline_building {
            "Decompile (analyzing...)"
        } else {
            app.view.title()
        };
        draw_content_pane(frame, app, body[1], content, title);
    }

    let footer_line = if app.file2.is_some() {
        Line::from(vec![
            Span::styled("q", Style::default().fg(Color::White)),
            Span::styled(" quit ", Style::default().fg(Color::DarkGray)),
            Span::styled("j/k", Style::default().fg(Color::White)),
            Span::styled(" nav ", Style::default().fg(Color::DarkGray)),
            Span::styled("/", Style::default().fg(Color::White)),
            Span::styled(" search ", Style::default().fg(Color::DarkGray)),
            Span::styled("s", Style::default().fg(Color::White)),
            Span::styled(" find ", Style::default().fg(Color::DarkGray)),
            Span::styled("Tab", Style::default().fg(Color::White)),
            Span::styled(" view ", Style::default().fg(Color::DarkGray)),
            Span::styled("PgUp/Dn", Style::default().fg(Color::White)),
            Span::styled(" scroll ", Style::default().fg(Color::DarkGray)),
            Span::styled("d", Style::default().fg(Color::White)),
            Span::styled(" diff colors ", Style::default().fg(Color::DarkGray)),
            Span::styled("v", Style::default().fg(Color::White)),
            Span::styled(" asm/code ", Style::default().fg(Color::DarkGray)),
            Span::styled("u", Style::default().fg(Color::White)),
            Span::styled(" git diff ", Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled("\u{25cf}", Style::default().fg(Color::Green)),
            Span::styled(" added ", Style::default().fg(Color::DarkGray)),
            Span::styled("\u{25cf}", Style::default().fg(Color::Red)),
            Span::styled(" removed ", Style::default().fg(Color::DarkGray)),
            Span::styled("\u{25cf}", Style::default().fg(Color::Yellow)),
            Span::styled(" modified ", Style::default().fg(Color::DarkGray)),
            Span::styled("\u{25cf}", Style::default().fg(Color::Blue)),
            Span::styled(" renamed", Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(vec![
            Span::styled("q", Style::default().fg(Color::White)),
            Span::styled(" quit ", Style::default().fg(Color::DarkGray)),
            Span::styled("j/k", Style::default().fg(Color::White)),
            Span::styled(" navigate ", Style::default().fg(Color::DarkGray)),
            Span::styled("/", Style::default().fg(Color::White)),
            Span::styled(" search ", Style::default().fg(Color::DarkGray)),
            Span::styled("s", Style::default().fg(Color::White)),
            Span::styled(" find ", Style::default().fg(Color::DarkGray)),
            Span::styled("Tab", Style::default().fg(Color::White)),
            Span::styled(" view ", Style::default().fg(Color::DarkGray)),
            Span::styled("PgUp/Dn", Style::default().fg(Color::White)),
            Span::styled(" scroll ", Style::default().fg(Color::DarkGray)),
            Span::styled("1-3", Style::default().fg(Color::White)),
            Span::styled(" views", Style::default().fg(Color::DarkGray)),
        ])
    };
    let footer = Paragraph::new(footer_line);
    frame.render_widget(footer, layout[2]);

    if app.is_content_searching {
        let popup = centered_rect(54, 3, frame.area());
        frame.render_widget(Clear, popup);
        let count = if app.content_search.is_empty() {
            String::new()
        } else if app.content_search_matches.is_empty() {
            "   no match".to_string()
        } else {
            format!(
                "   [{}/{}]",
                app.content_search_index + 1,
                app.content_search_matches.len()
            )
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("s ", Style::default().fg(Color::Cyan)),
                Span::raw(app.content_search.clone()),
                Span::styled("_", Style::default().fg(Color::Cyan)),
                Span::styled(count, Style::default().fg(Color::Yellow)),
            ]))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Double)
                    .title(" Search in content — \u{2193}/Enter: next  \u{2191}: prev  Esc: close "),
            ),
            popup,
        );
    }
}

fn draw_function_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app
        .function_names
        .iter()
        .enumerate()
        .map(|(idx, name)| {
            let mut label = format!("{idx:>4} {name}");

            let mut style = Style::default();

            // Apply diff color
            if let Some(status) = app.diff_status.get(name) {
                style = match status {
                    DiffStatus::Added => style.fg(Color::Green),
                    DiffStatus::Removed => style.fg(Color::Red),
                    DiffStatus::Modified => style.fg(Color::Yellow),
                    DiffStatus::Identical => style,
                    DiffStatus::Renamed(new_name) => {
                        label = format!("{idx:>4} {name} ({new_name})");
                        style.fg(Color::Blue)
                    }
                };
            }

            if idx == app.selected {
                ListItem::new(Line::from(Span::styled(
                    label,
                    style.fg(Color::Black).bg(Color::Yellow),
                )))
            } else {
                ListItem::new(Line::from(Span::styled(label, style)))
            }
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .title(app.function_list_title()),
    );

    // Calculate layout for list + search bar
    let list_area = if app.is_searching || !app.search_query.is_empty() {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(3)])
            .split(area);

        let search_text = format!("Search: {}", app.search_query);
        let search = Paragraph::new(search_text)
            .style(if app.is_searching {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            })
            .block(Block::default().borders(Borders::ALL).title("Filter"));
        frame.render_widget(search, chunks[1]);
        chunks[0]
    } else {
        area
    };

    // Record the inner (content) area so mouse clicks can map to a row.
    app.list_inner = Block::default().borders(Borders::ALL).inner(list_area);
    frame.render_stateful_widget(list, list_area, &mut app.list_state);
}

fn draw_content_pane(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    content: Text<'static>,
    title: &str,
) {
    let max_scroll = content
        .lines
        .len()
        .saturating_sub(area.height as usize) as u32;

    if app.scroll > max_scroll {
        app.scroll = max_scroll;
    }

    let q = if !app.content_search.is_empty() {
        Some(app.content_search.to_lowercase())
    } else {
        None
    };

    let mut highlighted: Vec<Line<'static>> = if q.is_some() {
        content
            .lines
            .into_iter()
            .map(|line| highlight_line_with_search(line, q.as_deref()))
            .collect()
    } else {
        content.lines
    };

    // Record inner area for coordinate mapping.
    let inner = Block::default().borders(Borders::ALL).inner(area);
    app.content_inner = inner;

    // Overlay text selection highlight (reverse video).
    if let Some((sel_sc, sel_sr, sel_ec, sel_er)) = app.normalized_selection() {
        let scroll = app.scroll as u16;
        for vi in 0..inner.height {
            let term_row = inner.y + vi;
            let line_idx = vi + scroll;
            if line_idx as usize >= highlighted.len() {
                break;
            }
            if term_row < sel_sr || term_row > sel_er {
                continue;
            }
            let col_start = if term_row == sel_sr {
                sel_sc
            } else {
                inner.x
            };
            let col_end = if term_row == sel_er {
                (sel_ec + 1).min(inner.x + inner.width)
            } else {
                inner.x + inner.width
            };
            if col_start >= col_end {
                continue;
            }
            let off = col_start.saturating_sub(inner.x) as usize;
            let end = (col_end - inner.x) as usize;
            highlighted[line_idx as usize] =
                apply_selection_styling(&highlighted[line_idx as usize], off, end);
        }
    }

    let paragraph = Paragraph::new(highlighted)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .title(title),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.scroll as u16, 0));
    frame.render_widget(paragraph, area);
}

// Split `text` into spans, highlighting case-insensitive occurrences of the
// (lowercase, ASCII) search query. Falls back to a single plain span.
fn highlight_spans(text: &str, base: Style, query: Option<&str>) -> Vec<Span<'static>> {
    let hl = Style::default().fg(Color::Black).bg(Color::Yellow);
    match query {
        Some(q) if !q.is_empty() && text.is_ascii() && q.is_ascii() => {
            let lower = text.to_lowercase();
            let mut spans = Vec::new();
            let mut i = 0;
            while let Some(pos) = lower[i..].find(q) {
                let s = i + pos;
                let e = s + q.len();
                if s > i {
                    spans.push(Span::styled(text[i..s].to_string(), base));
                }
                spans.push(Span::styled(text[s..e].to_string(), hl));
                i = e;
            }
            if i < text.len() {
                spans.push(Span::styled(text[i..].to_string(), base));
            }
            if spans.is_empty() {
                spans.push(Span::styled(text.to_string(), base));
            }
            spans
        }
        _ => vec![Span::styled(text.to_string(), base)],
    }
}

fn highlight_line_with_search(line: Line<'static>, query: Option<&str>) -> Line<'static> {
    let q = match query {
        Some(q) if !q.is_empty() => q,
        _ => return line,
    };

    let hl = Style::default().fg(Color::Black).bg(Color::Yellow);
    let mut result_spans = Vec::new();

    for span in line.spans {
        let text = &span.content;
        let base_style = span.style;
        let lower = text.to_lowercase();

        if !lower.contains(q) {
            result_spans.push(span);
            continue;
        }

        let mut i = 0;
        while let Some(pos) = lower[i..].find(q) {
            let s = i + pos;
            let e = s + q.len();
            if s > i {
                result_spans.push(Span::styled(text[i..s].to_string(), base_style));
            }
            result_spans.push(Span::styled(text[s..e].to_string(), hl));
            i = e;
        }
        if i < text.len() {
            result_spans.push(Span::styled(text[i..].to_string(), base_style));
        }
    }

    Line::from(result_spans)
}

// Apply reverse-video highlight to character range [start, end) on a line.
// Splits spans at boundaries so only the selected portion is highlighted.
fn apply_selection_styling(
    line: &Line<'static>,
    start: usize,
    end: usize,
) -> Line<'static> {
    let sel = Style::default().add_modifier(Modifier::REVERSED);
    let mut out = Vec::new();
    let mut col = 0usize;

    for span in &line.spans {
        let len: usize = span
            .content
            .chars()
            .map(|c| if c == '\t' { 4 } else { 1 })
            .sum();
        let span_end = col + len;

        if span_end <= start || col >= end {
            out.push(span.clone());
        } else if col >= start && span_end <= end {
            out.push(Span::styled(span.content.clone(), sel.patch(span.style)));
        } else {
            let mut cc = col;
            for ch in span.content.chars() {
                let w = if ch == '\t' { 4 } else { 1 };
                let s = if cc >= start && cc < end {
                    sel.patch(span.style)
                } else {
                    span.style
                };
                out.push(Span::styled(ch.to_string(), s));
                cc += w;
            }
        }
        col = span_end;
    }
    Line::from(out)
}

// One side of a diff row: git-style sign (`+`/`-`/space), a line-number
// gutter, then the text — either syntax-highlighted or diff-tinted, with the
// active search term highlighted on top.
#[allow(clippy::too_many_arguments)]
fn git_side_line(
    sign: char,
    sign_style: Style,
    line_no: Option<usize>,
    text: &str,
    base: Style,
    query: Option<&str>,
    syntax: bool,
) -> Line<'static> {
    let gutter = match line_no {
        Some(n) => format!("{n:>5} \u{2502} "),
        None => "      \u{2502} ".to_string(),
    };
    let mut spans = vec![
        Span::styled(format!("{sign} "), sign_style),
        Span::styled(gutter, Style::default().fg(Color::DarkGray)),
    ];
    if syntax && !text.is_empty() {
        // Only the visible lines are rendered each frame, so per-line syntax
        // highlighting here is cheap.
        match highlight_code(text).into_iter().next() {
            Some(line) => spans.extend(line.spans),
            None => spans.push(Span::styled(text.to_string(), base)),
        }
    } else {
        spans.extend(highlight_spans(text, base, query));
    }
    Line::from(spans)
}

// Full-program git diff: base (file 1) on the left, modified (file 2) on the
// right, aligned line by line, both columns scrolled together.
fn draw_git_diff(frame: &mut Frame, app: &mut App) {
    use super::gitdiff::GitRow;

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Min(1),    // panes
            Constraint::Length(1), // footer
        ])
        .split(frame.area());

    let kind = match app.git_kind {
        ViewMode::Disasm => "disassembly",
        _ => "decompiled",
    };
    const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let spin = SPINNER[app.tick % SPINNER.len()];
    let verb = if app.git_kind == ViewMode::Disasm {
        "disassembling"
    } else {
        "decompiling"
    };
    let progress = if app.git_computing {
        let (done, total) = app.git_progress;
        format!("   {spin} {verb} {done}/{total} functions…")
    } else {
        String::new()
    };
    // Word-style position feedback: "term [3/324]". The input itself is in the
    // centered popup while typing.
    let search = if app.git_search.is_empty() {
        String::new()
    } else if app.git_match_count == 0 {
        format!("   {} [no match]", app.git_search)
    } else {
        format!(
            "   {} [{}/{}]",
            app.git_search, app.git_match_index, app.git_match_count
        )
    };
    let title = Paragraph::new(Line::from(vec![
        Span::styled(" Git Diff ", Style::default().fg(Color::Black).bg(Color::Cyan)),
        Span::raw(format!("  base (file 1) vs modified (file 2) — {kind}")),
        Span::styled(progress, Style::default().fg(Color::Yellow)),
        Span::styled(search, Style::default().fg(Color::Cyan)),
    ]));
    frame.render_widget(title, outer[0]);

    let footer = Paragraph::new(Line::from(vec![
        Span::styled("u/Esc", Style::default().fg(Color::White)),
        Span::styled(" back ", Style::default().fg(Color::DarkGray)),
        Span::styled("j/k wheel", Style::default().fg(Color::White)),
        Span::styled(" scroll ", Style::default().fg(Color::DarkGray)),
        Span::styled("/ n/N", Style::default().fg(Color::White)),
        Span::styled(" search ", Style::default().fg(Color::DarkGray)),
        Span::styled("click", Style::default().fg(Color::White)),
        Span::styled(" fold ", Style::default().fg(Color::DarkGray)),
        Span::styled("v", Style::default().fg(Color::White)),
        Span::styled(" asm/code ", Style::default().fg(Color::DarkGray)),
        Span::styled("c", Style::default().fg(Color::White)),
        Span::styled(
            if app.git_syntax { " syntax " } else { " plain " },
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled("i", Style::default().fg(Color::White)),
        Span::styled(
            if app.git_normalize {
                " ids:ignored "
            } else {
                " ids:shown "
            },
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled("q", Style::default().fg(Color::White)),
        Span::styled(" quit", Style::default().fg(Color::DarkGray)),
    ]));
    frame.render_widget(footer, outer[2]);

    // Only show a full-screen message until the first chunk arrives; after that
    // we render (and let the user scroll) the partial result as it streams in.
    if app.git_rows.is_empty() {
        let msg = if app.git_computing {
            let (done, total) = app.git_progress;
            format!("\n   {spin}  {} bundle… {done}/{total} functions\n\n        Streaming per function — results appear as they finish. Press 'v' to switch asm/code.", if app.git_kind == ViewMode::Disasm { "Disassembling" } else { "Decompiling" })
        } else {
            "\n   No diff available yet…".to_string()
        };
        frame.render_widget(
            Paragraph::new(msg).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Loading "),
            ),
            outer[1],
        );
        return;
    }

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(outer[1]);

    // Visible window (minus the block borders), with scroll clamped. `scroll`
    // and the window index into git_visible (fold-aware), not git_rows.
    let height = cols[0].height.saturating_sub(2) as usize;
    let total = app.git_visible.len();
    let start = (app.scroll as usize).min(total.saturating_sub(height));
    app.scroll = start as u32;
    let end = (start + height).min(total);
    // Record the columns' content top/height for click-to-fold (both share y).
    app.git_view_top = cols[0].y + 1;
    app.git_view_height = height as u16;

    let q_owned = (!app.git_search.is_empty()).then(|| app.git_search.to_lowercase());
    let q = q_owned.as_deref();
    let syntax = app.git_syntax;
    let dim = Style::default().fg(Color::DarkGray);
    let red = Style::default().fg(Color::Red);
    let green = Style::default().fg(Color::Green);
    let plain = Style::default();
    let blank = || git_side_line(' ', dim, None, "", plain, None, false);

    let mut left_lines: Vec<Line<'static>> = Vec::new();
    let mut right_lines: Vec<Line<'static>> = Vec::new();
    for &ri in &app.git_visible[start..end] {
        match &app.git_rows[ri] {
            GitRow::Header(name) => {
                let s = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
                // ▶ when folded, ▼ when expanded (click the header to toggle).
                let marker = if app.git_folded.contains(&ri) {
                    "  \u{25b6} "
                } else {
                    "  \u{25bc} "
                };
                let mut spans = vec![Span::styled(marker.to_string(), s)];
                spans.extend(highlight_spans(name, s, q));
                left_lines.push(Line::from(spans.clone()));
                right_lines.push(Line::from(spans));
            }
            GitRow::Same { old, new, text } => {
                left_lines.push(git_side_line(' ', dim, Some(*old), text, plain, q, syntax));
                right_lines.push(git_side_line(' ', dim, Some(*new), text, plain, q, syntax));
            }
            // Differs only by a volatile id: shown plainly (not a real change).
            GitRow::Cosmetic {
                old,
                new,
                left,
                right,
            } => {
                left_lines.push(git_side_line(' ', dim, Some(*old), left, plain, q, syntax));
                right_lines.push(git_side_line(' ', dim, Some(*new), right, plain, q, syntax));
            }
            GitRow::Changed {
                old,
                new,
                left,
                right,
            } => {
                left_lines.push(git_side_line('-', red, Some(*old), left, red, q, syntax));
                right_lines.push(git_side_line('+', green, Some(*new), right, green, q, syntax));
            }
            GitRow::Removed { old, text } => {
                left_lines.push(git_side_line('-', red, Some(*old), text, red, q, syntax));
                right_lines.push(blank());
            }
            GitRow::Added { new, text } => {
                left_lines.push(blank());
                right_lines.push(git_side_line('+', green, Some(*new), text, green, q, syntax));
            }
            GitRow::Blank => {
                left_lines.push(Line::from(""));
                right_lines.push(Line::from(""));
            }
        }
    }

    frame.render_widget(
        Paragraph::new(left_lines)
            .block(Block::default().borders(Borders::ALL).title("file 1 (base)")),
        cols[0],
    );
    frame.render_widget(
        Paragraph::new(right_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title("file 2 (modified)"),
        ),
        cols[1],
    );

    // Centered search popup while typing.
    if app.git_searching {
        let popup = centered_rect(54, 3, frame.area());
        frame.render_widget(Clear, popup);
        let count = if app.git_search.is_empty() {
            String::new()
        } else if app.git_match_count == 0 {
            "   no match".to_string()
        } else {
            format!("   [{}/{}]", app.git_match_index, app.git_match_count)
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("/", Style::default().fg(Color::Cyan)),
                Span::raw(app.git_search.clone()),
                Span::styled("_", Style::default().fg(Color::Cyan)),
                Span::styled(count, Style::default().fg(Color::Yellow)),
            ]))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Double)
                    .title(" Search — \u{2193}/Enter: next  \u{2191}: prev  Esc: close "),
            ),
            popup,
        );
    }
}

// A `Rect` of the given size centered within `area`.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}
