use crate::app::{App, Focus, ViewMode};
use crate::session::{format_duration, format_file_size, size_category};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(1),
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

    if app.focus == Focus::Search {
        frame.set_cursor_position((
            area.x + 1 + app.search_query.chars().count() as u16,
            area.y + 1,
        ));
    }
}

fn draw_content(frame: &mut Frame, app: &App, area: Rect) {
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    match app.view_mode {
        ViewMode::Normal => draw_session_list(frame, app, panes[0]),
        ViewMode::Timeline => draw_timeline(frame, app, panes[0]),
    }
    draw_preview(frame, app, panes[1]);
}

fn draw_session_list(frame: &mut Frame, app: &App, area: Rect) {
    let border_style = if app.focus == Focus::List {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let mut title = format!(
        " Sessions ({}/{}) ",
        app.filtered_indices.len(),
        app.sessions.len()
    );
    if let Some(filter) = app.size_filter {
        title = format!(
            " Sessions ({}/{}) [{}] ",
            app.filtered_indices.len(),
            app.sessions.len(),
            filter
        );
    }

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

    let items_per_page = (inner.height as usize) / 4;
    let items_per_page = items_per_page.max(1);
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
        let is_bookmarked = app.bookmarks.contains(&session.id);

        let prefix = match (is_selected, is_bookmarked) {
            (true, true) => "▸★",
            (true, false) => "▸ ",
            (false, true) => " ★",
            (false, false) => "  ",
        };

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

        let ts = chrono_format(session.timestamp);
        let max_width = inner.width as usize;

        // Line 1: prefix · timestamp · project · branch · name
        let mut header = format!("{} {}  {}  {}", prefix, ts, session.project, session.branch);
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
        let prefix_len =
            dur_str.chars().count() + size_str.chars().count() + cat_str.chars().count() + 2;
        let title_budget = max_width.saturating_sub(prefix_len);
        let title_str = if title_budget > 3 {
            format!(
                "  \"{}\"",
                truncate(&session.title, title_budget.saturating_sub(3))
            )
        } else {
            String::new()
        };
        lines.push(Line::from(vec![
            Span::styled(dur_str, dim),
            Span::styled(size_str, dim),
            Span::styled(
                cat_str,
                Style::default()
                    .fg(category_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(title_str, dim),
        ]));

        // Line 3: left off
        if !session.last_message.is_empty() {
            let left_off = format!("  └ left off: \"{}\"", session.last_message);
            lines.push(Line::from(Span::styled(
                truncate(&left_off, max_width),
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            lines.push(Line::from(""));
        }

        if lines.len() < inner.height as usize {
            lines.push(Line::from(""));
        }
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn draw_timeline(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Timeline ");

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Group ALL sessions (not just filtered) by date
    use chrono::{Datelike, Local, NaiveDate, TimeZone};

    let today = Local::now().date_naive();
    let num_weeks: usize = ((inner.width as usize).saturating_sub(5)) / 2;
    let num_weeks = num_weeks.clamp(4, 26);

    // Find the Monday of the earliest week we'll show
    let days_since_monday = today.weekday().num_days_from_monday();
    let this_monday = today - chrono::Duration::days(days_since_monday as i64);
    let start_date = this_monday - chrono::Duration::weeks(num_weeks as i64 - 1);

    // Count sessions per date
    let mut counts: std::collections::HashMap<NaiveDate, u32> = std::collections::HashMap::new();
    for s in &app.sessions {
        if let chrono::LocalResult::Single(dt) = Local.timestamp_opt(s.timestamp, 0) {
            let date = dt.date_naive();
            if date >= start_date && date <= today {
                *counts.entry(date).or_insert(0) += 1;
            }
        }
    }

    let mut lines: Vec<Line> = Vec::new();

    // Month labels row
    let mut month_spans: Vec<Span> = vec![Span::raw("     ")]; // left padding for day labels
    let mut prev_month = 0u32;
    for w in 0..num_weeks {
        let week_start = start_date + chrono::Duration::weeks(w as i64);
        let month = week_start.month();
        if month != prev_month {
            let name = month_abbrev(month);
            month_spans.push(Span::styled(
                format!("{:<2}", name),
                Style::default().fg(Color::DarkGray),
            ));
            prev_month = month;
        } else {
            month_spans.push(Span::raw("  "));
        }
    }
    lines.push(Line::from(month_spans));

    // One row per weekday
    let day_names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    for (day_idx, day_name) in day_names.iter().enumerate() {
        let mut spans: Vec<Span> = vec![Span::styled(
            format!("{} ", day_name),
            Style::default().fg(Color::DarkGray),
        )];

        for w in 0..num_weeks {
            let date = start_date
                + chrono::Duration::weeks(w as i64)
                + chrono::Duration::days(day_idx as i64);
            if date > today {
                spans.push(Span::raw("  "));
                continue;
            }
            let count = counts.get(&date).copied().unwrap_or(0);
            let (ch, color) = heatmap_cell(count);
            spans.push(Span::styled(
                format!("{} ", ch),
                Style::default().fg(color),
            ));
        }

        lines.push(Line::from(spans));
    }

    // Summary below
    lines.push(Line::from(""));
    let total_sessions: u32 = counts.values().sum();
    let active_days = counts.len();
    let max_day = counts
        .iter()
        .max_by_key(|&(_, v)| *v)
        .map(|(d, c)| format!("{} ({})", d.format("%b %e"), c))
        .unwrap_or_else(|| "—".to_string());

    lines.push(Line::from(vec![
        Span::styled("  Total: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{} sessions", total_sessions),
            Style::default().fg(Color::White),
        ),
        Span::styled("  Active days: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}", active_days),
            Style::default().fg(Color::White),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Peak: ", Style::default().fg(Color::DarkGray)),
        Span::styled(max_day, Style::default().fg(Color::Yellow)),
    ]));

    // Legend
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled("░ ", Style::default().fg(Color::Rgb(30, 30, 30))),
        Span::styled("0  ", Style::default().fg(Color::DarkGray)),
        Span::styled("░ ", Style::default().fg(Color::Rgb(14, 68, 41))),
        Span::styled("1  ", Style::default().fg(Color::DarkGray)),
        Span::styled("▒ ", Style::default().fg(Color::Rgb(0, 109, 50))),
        Span::styled("2-3  ", Style::default().fg(Color::DarkGray)),
        Span::styled("▓ ", Style::default().fg(Color::Rgb(38, 166, 65))),
        Span::styled("4-5  ", Style::default().fg(Color::DarkGray)),
        Span::styled("█ ", Style::default().fg(Color::Rgb(57, 211, 83))),
        Span::styled("6+", Style::default().fg(Color::DarkGray)),
    ]));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn heatmap_cell(count: u32) -> (char, Color) {
    match count {
        0 => ('░', Color::Rgb(30, 30, 30)),
        1 => ('░', Color::Rgb(14, 68, 41)),
        2..=3 => ('▒', Color::Rgb(0, 109, 50)),
        4..=5 => ('▓', Color::Rgb(38, 166, 65)),
        _ => ('█', Color::Rgb(57, 211, 83)),
    }
}

fn month_abbrev(month: u32) -> &'static str {
    match month {
        1 => "Ja",
        2 => "Fe",
        3 => "Mr",
        4 => "Ap",
        5 => "My",
        6 => "Jn",
        7 => "Jl",
        8 => "Au",
        9 => "Se",
        10 => "Oc",
        11 => "Nv",
        12 => "Dc",
        _ => "??",
    }
}

fn draw_preview(frame: &mut Frame, app: &App, area: Rect) {
    let border_style = if matches!(app.focus, Focus::Preview | Focus::PreviewSearch) {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let title = if app.preview_loading {
        " Preview (loading...) ".to_string()
    } else if !app.preview_search_matches.is_empty() {
        format!(
            " Preview ({}/{} matches) ",
            app.preview_search_current + 1,
            app.preview_search_matches.len()
        )
    } else {
        " Preview ".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Reserve space for preview search bar if active
    let (preview_area, search_area) = if app.focus == Focus::PreviewSearch {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);
        (chunks[0], Some(chunks[1]))
    } else {
        (inner, None)
    };

    if app.preview_lines.is_empty() {
        let msg = if app.filtered_indices.is_empty() {
            "No session selected."
        } else if app.preview_loading {
            "Loading conversation..."
        } else {
            "Select a session to preview."
        };
        let p = Paragraph::new(msg).style(Style::default().fg(Color::DarkGray));
        frame.render_widget(p, preview_area);
    } else {
        let search_query_lower = app.preview_search_query.to_lowercase();
        let has_search = !search_query_lower.is_empty();
        let current_match_idx = app
            .preview_search_matches
            .get(app.preview_search_current)
            .copied();
        let match_set: std::collections::HashSet<usize> =
            app.preview_search_matches.iter().copied().collect();

        let mut lines: Vec<Line> = Vec::new();
        for (msg_idx, (text, is_user)) in app.preview_lines.iter().enumerate() {
            let is_match = has_search && match_set.contains(&msg_idx);
            let is_current = current_match_idx == Some(msg_idx);

            let (prefix, base_style) = if *is_user {
                (
                    "USER: ",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("ASST: ", Style::default().fg(Color::White))
            };

            // Match indicator: ▸ for current match, │ for other matches
            let marker = if is_current {
                Span::styled("▸", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            } else if is_match {
                Span::styled("│", Style::default().fg(Color::Yellow))
            } else {
                Span::styled(" ", Style::default())
            };

            let full_text = format!("{}{}", prefix, text);
            let wrap_width = preview_area.width.saturating_sub(1) as usize;
            for chunk in wrap_text(&full_text, wrap_width) {
                lines.push(Line::from(vec![marker.clone(), Span::styled(chunk, base_style)]));
            }
            lines.push(Line::from(""));
        }

        let scroll = app.preview_scroll as usize;
        let visible: Vec<Line> = lines.into_iter().skip(scroll).collect();

        let paragraph = Paragraph::new(visible);
        frame.render_widget(paragraph, preview_area);
    }

    // Draw preview search bar
    if let Some(area) = search_area {
        let search_line = Line::from(vec![
            Span::styled(
                "/",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                &app.preview_search_query,
                Style::default().fg(Color::Yellow),
            ),
        ]);
        let p = Paragraph::new(search_line).style(Style::default().bg(Color::Rgb(30, 30, 30)));
        frame.render_widget(p, area);

        frame.set_cursor_position((
            area.x + 1 + app.preview_search_query.chars().count() as u16,
            area.y,
        ));
    }
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    // Temporary status message takes priority
    if let Some(msg) = app.active_status() {
        let line = Line::from(Span::styled(
            format!(" {}", msg),
            Style::default()
                .fg(Color::White)
                .bg(Color::Rgb(0, 80, 40)),
        ));
        let paragraph =
            Paragraph::new(line).style(Style::default().bg(Color::Rgb(0, 80, 40)));
        frame.render_widget(paragraph, area);
        return;
    }

    if app.confirm_delete {
        let warn = Style::default()
            .fg(Color::White)
            .bg(Color::Red)
            .add_modifier(Modifier::BOLD);
        let hint = Style::default()
            .fg(Color::Rgb(200, 200, 200))
            .bg(Color::Red);
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

    let key = Style::default()
        .fg(Color::Cyan)
        .bg(Color::Rgb(40, 40, 40))
        .add_modifier(Modifier::BOLD);
    let desc = Style::default()
        .fg(Color::Rgb(180, 180, 180))
        .bg(Color::Rgb(40, 40, 40));

    let sort_label = format!("sort:{}  ", app.sort_mode.label());
    let filter_label = if let Some(f) = app.size_filter {
        format!("[{}]  ", f)
    } else {
        String::new()
    };

    let mut spans = vec![
        Span::styled(" ↑↓ ", key),
        Span::styled("nav  ", desc),
        Span::styled("/ ", key),
        Span::styled("search  ", desc),
        Span::styled("s ", key),
        Span::styled(sort_label, desc),
    ];

    if !filter_label.is_empty() {
        spans.push(Span::styled("1-4 ", key));
        spans.push(Span::styled(filter_label, desc));
    } else {
        spans.push(Span::styled("1-4 ", key));
        spans.push(Span::styled("filter  ", desc));
    }

    spans.extend([
        Span::styled("b ", key),
        Span::styled("pin  ", desc),
        Span::styled("e ", key),
        Span::styled("export  ", desc),
        Span::styled("t ", key),
        Span::styled("timeline  ", desc),
        Span::styled("Enter ", key),
        Span::styled("launch  ", desc),
        Span::styled("y ", key),
        Span::styled("yolo  ", desc),
        Span::styled("c ", key),
        Span::styled("copy  ", desc),
        Span::styled("d ", key),
        Span::styled("delete  ", desc),
        Span::styled("q ", key),
        Span::styled("quit ", desc),
    ]);

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line).style(Style::default().bg(Color::Rgb(40, 40, 40)));
    frame.render_widget(paragraph, area);
}

fn chrono_format(timestamp: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(timestamp, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%b %e %H:%M").to_string(),
        _ => "??? ?? ??:??".to_string(),
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut iter = s.char_indices();
    // Find byte offset of the max_chars-th character
    let fits = iter.nth(max_chars).is_none();
    if fits {
        return s.to_string();
    }
    // String is longer than max_chars; truncate to max_chars - 1 + ellipsis
    if max_chars == 1 {
        return "…".to_string();
    }
    let end = s
        .char_indices()
        .nth(max_chars - 1)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    format!("{}…", &s[..end])
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut result = Vec::new();
    let mut remaining = text;
    while let Some((byte_limit, _)) = remaining.char_indices().nth(width) {
        let split_at = remaining[..byte_limit].rfind(' ').unwrap_or(0);
        let split_at = if split_at == 0 { byte_limit } else { split_at };
        result.push(remaining[..split_at].to_string());
        remaining = remaining[split_at..].trim_start();
    }
    if !remaining.is_empty() {
        result.push(remaining.to_string());
    }
    result
}
