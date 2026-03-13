use crate::app::{App, Focus};
use crate::session::{format_duration, format_file_size, size_category};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Main layout: search bar (3) | content | status bar (1)
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // search bar
            Constraint::Min(5),    // content
            Constraint::Length(1), // status bar
        ])
        .split(area);

    draw_search_bar(frame, app, main_chunks[0]);
    draw_content(frame, app, main_chunks[1]);
    draw_status_bar(frame, app, main_chunks[2]);
}

fn draw_search_bar(frame: &mut Frame, app: &App, area: Rect) {
    let style = if app.focus == Focus::Search {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let search_text = if app.search_query.is_empty() {
        if app.focus == Focus::Search {
            String::new()
        } else {
            "Type / to search...".to_string()
        }
    } else {
        app.search_query.clone()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(Span::styled(" Search ", style));

    let paragraph = Paragraph::new(search_text).block(block);
    frame.render_widget(paragraph, area);

    // Show cursor in search mode
    if app.focus == Focus::Search {
        frame.set_cursor_position((
            area.x + 1 + app.search_query.chars().count() as u16,
            area.y + 1,
        ));
    }
}

fn draw_content(frame: &mut Frame, app: &App, area: Rect) {
    // Split into two panes: 45% list | 55% preview
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    draw_session_list(frame, app, panes[0]);
    draw_preview(frame, app, panes[1]);
}

fn draw_session_list(frame: &mut Frame, app: &App, area: Rect) {
    let border_style = if app.focus == Focus::List {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let title = format!(
        " Sessions ({}/{}) ",
        app.filtered_indices.len(),
        app.sessions.len()
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.filtered_indices.is_empty() {
        let empty = Paragraph::new("No sessions found.")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(empty, inner);
        return;
    }

    // Each session takes 3 lines + 1 blank = 4 lines per entry
    let items_per_page = (inner.height as usize) / 4;
    let scroll_offset = if app.selected >= items_per_page {
        app.selected - items_per_page + 1
    } else {
        0
    };

    let mut lines: Vec<Line> = Vec::new();

    for (visual_idx, &real_idx) in app
        .filtered_indices
        .iter()
        .enumerate()
        .skip(scroll_offset)
    {
        if lines.len() as u16 >= inner.height {
            break;
        }

        let session = &app.sessions[real_idx];
        let is_selected = visual_idx == app.selected;

        let prefix = if is_selected { "▸ " } else { "  " };
        let highlight = if is_selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let dim = if is_selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        // Format timestamp
        let ts = chrono_format(session.timestamp);

        // Line 1: timestamp · project · branch · name
        let max_width = inner.width as usize;
        let mut header = format!(
            "{}{}  {}  {}",
            prefix, ts, session.project, session.branch
        );
        if !session.name.is_empty() {
            header.push_str("  ");
            header.push_str(&session.name);
        }
        lines.push(Line::from(Span::styled(
            truncate(&header, max_width),
            highlight,
        )));

        // Line 2: duration · size [category] · title
        let category = size_category(session.file_size);
        let category_color = match category {
            "quick" => Color::Green,
            "medium" => Color::Yellow,
            "deep" => Color::Magenta,
            "massive" => Color::Red,
            _ => Color::White,
        };
        let dur_str = format!("  {}  ", format_duration(session.duration_secs));
        let size_str = format!("{} ", format_file_size(session.file_size));
        let cat_str = format!("[{}]", category);
        let prefix_len = dur_str.chars().count()
            + size_str.chars().count()
            + cat_str.chars().count()
            + 2; // "  " before title
        let title_budget = max_width.saturating_sub(prefix_len);
        let title_str = if title_budget > 3 {
            format!("  \"{}\"", truncate(&session.title, title_budget.saturating_sub(3)))
        } else {
            String::new()
        };
        lines.push(Line::from(vec![
            Span::styled(dur_str, dim),
            Span::styled(size_str, dim),
            Span::styled(cat_str, Style::default().fg(category_color).add_modifier(Modifier::BOLD)),
            Span::styled(title_str, dim),
        ]));

        // Line 3: └ left off: last message
        if !session.last_message.is_empty() {
            let left_off = format!("  └ left off: \"{}\"", session.last_message);
            lines.push(Line::from(Span::styled(
                truncate(&left_off, max_width),
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            lines.push(Line::from(""));
        }

        // Blank separator line
        if lines.len() < inner.height as usize {
            lines.push(Line::from(""));
        }
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn draw_preview(frame: &mut Frame, app: &App, area: Rect) {
    let border_style = if app.focus == Focus::Preview {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let title = if app.preview_loading {
        " Preview (loading...) "
    } else {
        " Preview "
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.preview_lines.is_empty() {
        let msg = if app.filtered_indices.is_empty() {
            "No session selected."
        } else if app.preview_loading {
            "Loading conversation..."
        } else {
            "Select a session to preview."
        };
        let p = Paragraph::new(msg).style(Style::default().fg(Color::DarkGray));
        frame.render_widget(p, inner);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    for (text, is_user) in &app.preview_lines {
        let (prefix, style) = if *is_user {
            (
                "USER: ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ("ASST: ", Style::default().fg(Color::White))
        };

        // Wrap long messages into multiple display lines
        let full_text = format!("{}{}", prefix, text);
        let wrap_width = inner.width as usize;
        for chunk in wrap_text(&full_text, wrap_width) {
            lines.push(Line::from(Span::styled(chunk, style)));
        }
        lines.push(Line::from("")); // blank separator
    }

    let scroll = app.preview_scroll as usize;
    let visible: Vec<Line> = lines.into_iter().skip(scroll).collect();

    let paragraph = Paragraph::new(visible);
    frame.render_widget(paragraph, inner);
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    if app.confirm_delete {
        let warn = Style::default().fg(Color::White).bg(Color::Red).add_modifier(Modifier::BOLD);
        let hint = Style::default().fg(Color::Rgb(200, 200, 200)).bg(Color::Red);
        let session_name = app
            .selected_session()
            .map(|s| s.title.chars().take(40).collect::<String>())
            .unwrap_or_default();
        let line = Line::from(vec![
            Span::styled(" DELETE ", warn),
            Span::styled(format!("\"{}\"? ", session_name), hint),
            Span::styled("d/y ", warn),
            Span::styled("confirm  ", hint),
            Span::styled("any key ", warn),
            Span::styled("cancel ", hint),
        ]);
        let paragraph = Paragraph::new(line).style(Style::default().bg(Color::Red));
        frame.render_widget(paragraph, area);
        return;
    }

    let key = Style::default().fg(Color::Cyan).bg(Color::Rgb(40, 40, 40)).add_modifier(Modifier::BOLD);
    let desc = Style::default().fg(Color::Rgb(180, 180, 180)).bg(Color::Rgb(40, 40, 40));

    let sort_label = format!("sort:{}  ", app.sort_mode.label());
    let keybindings = vec![
        Span::styled(" ↑↓/jk ", key),
        Span::styled("navigate  ", desc),
        Span::styled("/ ", key),
        Span::styled("search  ", desc),
        Span::styled("s ", key),
        Span::styled(sort_label, desc),
        Span::styled("Enter ", key),
        Span::styled("launch  ", desc),
        Span::styled("y ", key),
        Span::styled("yolo  ", desc),
        Span::styled("c ", key),
        Span::styled("copy  ", desc),
        Span::styled("p ", key),
        Span::styled("print  ", desc),
        Span::styled("d ", key),
        Span::styled("delete  ", desc),
        Span::styled("Tab ", key),
        Span::styled("focus  ", desc),
        Span::styled("q ", key),
        Span::styled("quit ", desc),
    ];

    let line = Line::from(keybindings);
    let paragraph = Paragraph::new(line)
        .style(Style::default().bg(Color::Rgb(40, 40, 40)));
    frame.render_widget(paragraph, area);
}

/// Format a Unix timestamp into "Mar 13 09:49" in local time.
fn chrono_format(timestamp: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(timestamp, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%b %e %H:%M").to_string(),
        _ => "??? ?? ??:??".to_string(),
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else if max_chars > 1 {
        let end = s.char_indices().nth(max_chars - 1).map(|(i, _)| i).unwrap_or(s.len());
        format!("{}…", &s[..end])
    } else {
        "…".to_string()
    }
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut result = Vec::new();
    let mut remaining = text;
    while remaining.chars().count() > width {
        // Find byte offset of the width-th character
        let byte_limit = remaining
            .char_indices()
            .nth(width)
            .map(|(i, _)| i)
            .unwrap_or(remaining.len());
        let split_at = remaining[..byte_limit]
            .rfind(' ')
            .unwrap_or(byte_limit);
        let split_at = if split_at == 0 { byte_limit } else { split_at };
        result.push(remaining[..split_at].to_string());
        remaining = remaining[split_at..].trim_start();
    }
    if !remaining.is_empty() {
        result.push(remaining.to_string());
    }
    result
}
