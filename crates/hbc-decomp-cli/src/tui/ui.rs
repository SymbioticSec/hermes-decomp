use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, ViewMode};
use super::diff::DiffStatus;

pub fn draw_ui(frame: &mut Frame, app: &mut App) {
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
            Span::styled("Tab", Style::default().fg(Color::White)),
            Span::styled(" view ", Style::default().fg(Color::DarkGray)),
            Span::styled("PgUp/Dn", Style::default().fg(Color::White)),
            Span::styled(" scroll ", Style::default().fg(Color::DarkGray)),
            Span::styled("d", Style::default().fg(Color::White)),
            Span::styled(" diff colors ", Style::default().fg(Color::DarkGray)),
            Span::styled("v", Style::default().fg(Color::White)),
            Span::styled(" asm/code ", Style::default().fg(Color::DarkGray)),
            Span::styled("u", Style::default().fg(Color::White)),
            Span::styled(" split/unified ", Style::default().fg(Color::DarkGray)),
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

    let paragraph = Paragraph::new(content)
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
