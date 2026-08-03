use crate::app::{App, Focus, Scope, ViewMode};
use crate::parser::Speaker;
use crate::session::{format_duration, format_file_size, size_category};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    app.terminal_height = area.height;

    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(2),
        ])
        .split(area);

    draw_search_bar(frame, app, main_chunks[0]);
    draw_content(frame, app, main_chunks[1]);
    draw_status_bar(frame, app, main_chunks[2]);

    if app.show_help {
        draw_help(frame, area);
    }
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

    let search_title = if !app.search_query.is_empty() {
        format!(
            " Search ({}/{}) ",
            app.filtered_indices.len(),
            app.sessions.len()
        )
    } else {
        " Search ".to_string()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(Span::styled(search_title, style));

    let paragraph = Paragraph::new(search_text).block(block);
    frame.render_widget(paragraph, area);

    if app.focus == Focus::Search {
        frame.set_cursor_position((
            cursor_x(area.x + 1, &app.search_query, area.right().saturating_sub(2)),
            area.y + 1,
        ));
    }
}

fn draw_content(frame: &mut Frame, app: &mut App, area: Rect) {
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

    let scope_tag = app.scope.label();
    let mut title = format!(
        " Sessions ({}/{}) [{}] ",
        app.filtered_indices.len(),
        app.sessions.len(),
        scope_tag
    );
    if let Some(filter) = app.size_filter {
        title = format!(
            " Sessions ({}/{}) [{}|{}] ",
            app.filtered_indices.len(),
            app.sessions.len(),
            scope_tag,
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
        let msg = if app.scope == Scope::Current && app.cwd_encoded.is_some() {
            "No sessions in this directory.\n\nPress  a  to show all projects, or  /  to search."
        } else {
            "No sessions found."
        };
        let empty = Paragraph::new(msg).style(Style::default().fg(Color::DarkGray));
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

        // Line 1: prefix · timestamp · project · branch · name (each styled separately)
        let ts_style = if is_selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let project_style = highlight;
        let branch_style = if is_selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::Yellow)
        };
        let name_style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);

        let mut line1_spans: Vec<Span> = vec![
            Span::styled(format!("{} ", prefix), highlight),
            Span::styled(format!("{}  ", ts), ts_style),
            Span::styled(session.project.clone(), project_style),
        ];
        if !session.branch.is_empty() {
            line1_spans.push(Span::styled(format!("  {}", session.branch), branch_style));
        }
        if !session.name.is_empty() {
            line1_spans.push(Span::styled(format!("  {}", session.name), name_style));
        }
        lines.push(Line::from(line1_spans));

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
        let msgs_str = if session.message_count > 0 {
            format!(" · {} msgs", session.message_count)
        } else {
            String::new()
        };
        let prefix_len = dur_str.chars().count()
            + size_str.chars().count()
            + cat_str.chars().count()
            + msgs_str.chars().count()
            + 2;
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
            Span::styled(msgs_str, dim),
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

    // Count sessions per date, honoring the current scope so the heatmap
    // matches the list (cwd-only unless scope is All).
    let scope_needle = match (app.scope, &app.cwd_encoded) {
        (Scope::Current, Some(enc)) => Some(format!("/{}/", enc)),
        _ => None,
    };
    let mut counts: std::collections::HashMap<NaiveDate, u32> = std::collections::HashMap::new();
    for s in &app.sessions {
        if let Some(needle) = &scope_needle {
            if !s.file_path.to_string_lossy().contains(needle.as_str()) {
                continue;
            }
        }
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

fn draw_preview(frame: &mut Frame, app: &mut App, area: Rect) {
    let border_style = if matches!(app.focus, Focus::Preview | Focus::PreviewSearch) {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    // Track viewport height (inner minus the optional search row) so the app
    // can clamp over-scroll and report a position percentage.
    let inner_height = area.height.saturating_sub(2);
    let viewport = if app.focus == Focus::PreviewSearch {
        inner_height.saturating_sub(1)
    } else {
        inner_height
    };
    app.preview_viewport_height = viewport;
    let max_scroll = app.max_preview_scroll();
    let scroll_pct = if app.preview_total_rows > viewport && max_scroll > 0 {
        Some((app.preview_scroll.min(max_scroll) as f32 / max_scroll as f32 * 100.0).round() as u16)
    } else {
        None
    };

    let session_label = app
        .selected_session()
        .and_then(|s| {
            if !s.name.is_empty() {
                Some(s.name.as_str())
            } else {
                None
            }
        })
        .unwrap_or("");
    let meta_badges = app.selected_session().map(session_badges).unwrap_or_default();
    let title = if app.show_files {
        let n = app
            .selected_session()
            .map(|s| s.changed_files.len())
            .unwrap_or(0);
        format!(" Files changed — {} ({}) ", session_label, n)
    } else if app.preview_loading {
        format!(" Preview — {} (loading...) ", session_label)
    } else if !app.preview_search_matches.is_empty() {
        format!(
            " Preview — {} ({}/{} matches) ",
            session_label,
            app.preview_search_current + 1,
            app.preview_search_matches.len()
        )
    } else if let Some(pct) = scroll_pct {
        format!(" Preview — {}{} ({}%) ", session_label, meta_badges, pct)
    } else if !session_label.is_empty() {
        format!(" Preview — {}{} ", session_label, meta_badges)
    } else {
        " Preview ".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);

    let inner = block.inner(area);
    let new_width = inner.width;
    if app.preview_inner_width != new_width {
        app.preview_inner_width = new_width;
        app.recompute_preview_offsets();
    }
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

    if app.show_files {
        let dim = Style::default().fg(Color::DarkGray);
        let lines: Vec<Line> = match app.selected_session() {
            None => vec![Line::from(Span::styled("No session selected.", dim))],
            Some(s) if s.changed_files.is_empty() => {
                vec![Line::from(Span::styled("No tracked file changes.", dim))]
            }
            Some(s) => s
                .changed_files
                .iter()
                .skip(app.preview_scroll as usize)
                .map(|f| {
                    Line::from(Span::styled(
                        format!("  {}", f),
                        Style::default().fg(Color::White),
                    ))
                })
                .collect(),
        };
        frame.render_widget(Paragraph::new(lines), preview_area);
    } else if app.preview_lines.is_empty() {
        let msg = if app.filtered_indices.is_empty() {
            "No session selected."
        } else if app.preview_loading {
            "Loading conversation..."
        } else {
            // A session is selected but its extraction produced nothing
            // (e.g. only slash commands, no conversation).
            "No conversation content in this session."
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
        for (msg_idx, (text, _text_lc, speaker)) in app.preview_lines.iter().enumerate() {
            let is_match = has_search && match_set.contains(&msg_idx);
            let is_current = current_match_idx == Some(msg_idx);

            let (prefix, base_style) = match speaker {
                Speaker::User => (
                    "USER: ",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Speaker::Assistant => ("ASST: ", Style::default().fg(Color::White)),
                Speaker::Tool => (
                    "TOOL: ",
                    Style::default().fg(Color::Rgb(110, 160, 190)),
                ),
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
            let highlight_query = if is_match { &search_query_lower } else { "" };
            let match_style = if is_current {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            };
            for chunk in wrap_text(&full_text, wrap_width) {
                let mut spans = vec![marker.clone()];
                spans.extend(highlight_spans(&chunk, highlight_query, base_style, match_style));
                lines.push(Line::from(spans));
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
            cursor_x(
                area.x + 1,
                &app.preview_search_query,
                area.right().saturating_sub(1),
            ),
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

    let sort_label = if app.search_query.is_empty() {
        format!("sort:{}  ", app.sort_mode.label())
    } else {
        "sort:relevance  ".to_string()
    };
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
        Span::styled("a ", key),
        Span::styled(format!("{}  ", app.scope.label()), desc),
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
        Span::styled("f ", key),
        Span::styled("files  ", desc),
        Span::styled("T ", key),
        Span::styled("tools  ", desc),
        Span::styled("Enter ", key),
        Span::styled(if app.print_mode { "print  " } else { "yolo  " }, desc),
        Span::styled("l ", key),
        Span::styled("launch  ", desc),
        Span::styled("c ", key),
        Span::styled("copy  ", desc),
        Span::styled("d ", key),
        Span::styled("delete  ", desc),
        Span::styled("? ", key),
        Span::styled("help  ", desc),
        Span::styled("q ", key),
        Span::styled("quit ", desc),
    ]);

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line)
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(Color::Rgb(40, 40, 40)));
    frame.render_widget(paragraph, area);
}

/// Compact metadata suffix for the preview title: permission mode + skills.
/// Returns "" when there's nothing to show, otherwise a " · plan · tdd" string.
fn session_badges(s: &crate::session::SessionMeta) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mode = match s.permission_mode.as_str() {
        "bypassPermissions" => "yolo",
        "acceptEdits" => "accept",
        "plan" => "plan",
        _ => "",
    };
    if !mode.is_empty() {
        parts.push(mode.to_string());
    }
    if !s.skills.is_empty() {
        let shown: Vec<&str> = s.skills.iter().take(3).map(|x| x.as_str()).collect();
        let mut sk = shown.join(",");
        let extra = s.skills.len().saturating_sub(3);
        if extra > 0 {
            sk.push_str(&format!("+{}", extra));
        }
        parts.push(sk);
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" · {}", parts.join(" · "))
    }
}

/// Centered overlay listing every keybinding. Drawn on top of everything when open.
fn draw_help(frame: &mut Frame, area: Rect) {
    let rows: &[(&str, &str)] = &[
        ("j / k  ↑ / ↓", "move selection (wraps)"),
        ("g / G  Home / End", "jump to top / bottom"),
        ("PgUp / PgDn", "page up / down"),
        ("/", "search (project, title, branch, file, text)"),
        ("s", "cycle sort: date → size → duration → messages"),
        ("1-4 / 0", "filter by size / clear filter"),
        ("a", "toggle scope: current dir ↔ all projects"),
        ("b", "bookmark / pin to top"),
        ("e", "export session as markdown"),
        ("t", "timeline heatmap"),
        ("Tab", "focus preview pane"),
        ("f", "show files changed in this session"),
        ("T", "toggle tool activity in preview"),
        ("Enter", "resume (yolo: --dangerously-skip-permissions)"),
        ("l", "resume (safe mode)"),
        ("c", "copy `claude --resume <id>`"),
        ("p", "print session id and exit"),
        ("d", "delete session (confirm)"),
        ("? / Esc", "close help / quit"),
    ];

    let w = 56u16.min(area.width.saturating_sub(2));
    let h = (rows.len() as u16 + 2).min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let popup = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Keybindings ")
        .style(Style::default().bg(Color::Rgb(20, 20, 20)));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(Color::Rgb(200, 200, 200));
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!(" {:<18}", k), key_style),
                Span::styled((*d).to_string(), desc_style),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Cursor column after `query`, clamped so a pasted mega-string can neither
/// overflow u16 arithmetic nor push the cursor outside the input box.
fn cursor_x(start: u16, query: &str, max_x: u16) -> u16 {
    let chars = query.chars().count().min(u16::MAX as usize) as u16;
    start.saturating_add(chars).min(max_x.max(start))
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

pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
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

/// Returns true for Unicode combining diacritical marks (U+0300–U+036F).
/// These can appear as extra code points when lowercasing characters such as
/// Turkish İ (U+0130) → i + U+0307. Skipping them keeps the lowercased text
/// byte-for-byte matchable against a plain query like "istanbul".
#[inline]
fn is_combining_diacritic(c: char) -> bool {
    ('\u{0300}'..='\u{036F}').contains(&c)
}

/// Split text into spans, highlighting occurrences of `query` (case-insensitive).
fn highlight_spans(
    text: &str,
    query: &str,
    base_style: Style,
    match_style: Style,
) -> Vec<Span<'static>> {
    if query.is_empty() {
        return vec![Span::styled(text.to_string(), base_style)];
    }

    // Build a lowercased copy of `text` alongside a mapping from every byte
    // position in `text_lower` back to the corresponding byte position in
    // `text`.  Combining diacritical marks introduced by `to_lowercase()` (e.g.
    // İ → i + U+0307) are skipped so that a plain query like "istanbul" still
    // finds a match.
    let mut text_lower = String::with_capacity(text.len());
    let mut lower_to_orig: Vec<usize> = Vec::with_capacity(text.len() + 1);
    for (orig_idx, ch) in text.char_indices() {
        for lc in ch.to_lowercase() {
            if is_combining_diacritic(lc) {
                continue;
            }
            let mut buf = [0u8; 4];
            let s = lc.encode_utf8(&mut buf);
            let len = s.len();
            for _ in 0..len {
                lower_to_orig.push(orig_idx);
            }
            text_lower.push(lc);
        }
    }
    lower_to_orig.push(text.len());

    let mut spans = Vec::new();
    let mut last_end = 0;
    let mut search_from = 0;

    while let Some(pos) = text_lower[search_from..].find(query) {
        let start_lower = search_from + pos;
        let end_lower = start_lower + query.len();
        let start_orig = lower_to_orig[start_lower];
        let end_orig = lower_to_orig[end_lower];

        if start_orig > last_end {
            spans.push(Span::styled(text[last_end..start_orig].to_string(), base_style));
        }
        spans.push(Span::styled(text[start_orig..end_orig].to_string(), match_style));
        last_end = end_orig;
        search_from = end_lower;
        if search_from >= text_lower.len() {
            break;
        }
    }

    if last_end < text.len() {
        spans.push(Span::styled(text[last_end..].to_string(), base_style));
    }

    if spans.is_empty() {
        vec![Span::styled(text.to_string(), base_style)]
    } else {
        spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_emoji_char_boundaries() {
        // Must cut on char boundaries, never mid-codepoint.
        assert_eq!(truncate("🐢🐢🐢🐢", 2), "🐢…");
        assert_eq!(truncate("🐢🐢", 5), "🐢🐢");
        assert_eq!(truncate("αβγδε", 3), "αβ…");
        assert_eq!(truncate("abc", 0), "");
        assert_eq!(truncate("abc", 1), "…");
    }

    #[test]
    fn test_wrap_text_unicode_no_panic() {
        // Long unbroken emoji run wider than the wrap width.
        let text = "🐢".repeat(50);
        let chunks = wrap_text(&text, 10);
        assert!(chunks.len() >= 5);
        assert!(chunks.iter().all(|c| c.chars().count() <= 10));
        // Greek with spaces wraps on spaces.
        let chunks = wrap_text("καλημέρα κόσμε γεια σου", 10);
        assert!(chunks.len() >= 2);
    }

    #[test]
    fn test_wrap_text_zero_width() {
        assert_eq!(wrap_text("hello", 0), vec!["hello".to_string()]);
        assert!(wrap_text("", 10).is_empty());
    }

    #[test]
    fn test_cursor_x_clamps() {
        // Normal case: start + query length.
        assert_eq!(cursor_x(1, "ab", 40), 3);
        // Clamped to the box edge.
        assert_eq!(cursor_x(1, &"x".repeat(100), 40), 40);
        // Degenerate box (max < start) must not underflow or move left of start.
        assert_eq!(cursor_x(5, "abc", 2), 5);
        // Huge paste: no u16 overflow panic.
        assert_eq!(cursor_x(u16::MAX - 1, &"y".repeat(70_000), u16::MAX), u16::MAX);
    }

    #[test]
    fn test_highlight_unicode_length_change() {
        // Turkish dotted-I (\u{0130}) lowercases to "i\u{0307}" (two code points),
        // which makes text_lower.len() differ from text.len().
        let text = "İstanbul project";
        let base = Style::default();
        let mat = Style::default();
        let spans = highlight_spans(text, "istanbul", base, mat);
        // Should produce at least 2 spans (matched + remainder), not bail out.
        assert!(spans.len() >= 2, "got spans: {}", spans.len());
    }
}
