use crate::app::{App, Focus, Scope, ViewMode};
use crate::input::TextInput;
use crate::parser::Speaker;
use crate::session::{format_duration, format_file_size, size_category, SessionMeta};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Width of the speaker column ("USER: ", "ASST: ", …) in the preview.
pub const PREFIX_WIDTH: usize = 6;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    app.terminal_height = area.height;

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

    if app.show_help {
        draw_help(frame, area, app);
    }
}

fn draw_search_bar(frame: &mut Frame, app: &App, area: Rect) {
    // The top bar doubles as the rename prompt.
    if app.focus == Focus::Rename {
        let style = Style::default().fg(Color::Magenta);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(style)
            .title(Span::styled(" Rename session (Enter saves, Esc cancels) ", style));
        let inner = block.inner(area);
        let (visible, col) = visible_input(&app.rename_input, inner.width as usize);
        frame.render_widget(Paragraph::new(visible).block(block), area);
        frame.set_cursor_position((inner.x + col, inner.y));
        return;
    }

    let focused = app.focus == Focus::Search;
    let style = if focused {
        Style::default().fg(Color::Yellow)
    } else if !app.search_query.is_empty() {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::DIM)
    } else {
        Style::default().fg(Color::DarkGray)
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
    let inner = block.inner(area);

    if app.search_query.is_empty() {
        let hint = if focused {
            "titles, projects, branches, files, tickets, full conversation text…"
        } else {
            "Press / to search"
        };
        let p = Paragraph::new(Span::styled(hint, Style::default().fg(Color::DarkGray))).block(block);
        frame.render_widget(p, area);
    } else {
        let (visible, _) = visible_input(&app.search_query, inner.width as usize);
        frame.render_widget(Paragraph::new(visible).block(block), area);
    }

    if focused {
        let (_, col) = visible_input(&app.search_query, inner.width as usize);
        frame.set_cursor_position((inner.x + col, inner.y));
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

/// `~`-abbreviated path for titles.
fn display_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().to_string();
    if let Some(home) = dirs::home_dir() {
        let home = home.to_string_lossy().to_string();
        if s == home {
            return "~".to_string();
        }
        if let Some(rest) = s.strip_prefix(&format!("{}/", home)) {
            return format!("~/{}", rest);
        }
    }
    s
}

fn draw_session_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let border_style = if app.focus == Focus::List {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let scope = match (app.scope, &app.scope_root) {
        (Scope::Current, Some(root)) => display_path(root),
        _ => "all projects".to_string(),
    };
    let filter = app
        .size_filter
        .map(|f| format!(" [{}]", f))
        .unwrap_or_default();
    let counts = format!("({}/{})", app.filtered_indices.len(), app.sessions.len());
    let mut title = format!(" Sessions · {}{} {} ", scope, filter, counts);
    // Narrow pane: the project's own name says enough.
    if title.width() + 2 > area.width as usize {
        if let (Scope::Current, Some(name)) = (app.scope, app.scope_root.as_ref().and_then(|r| r.file_name())) {
            title = format!(" {}{} {} ", name.to_string_lossy(), filter, counts);
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.filtered_indices.is_empty() {
        let msg = empty_list_message(app);
        let empty = Paragraph::new(msg)
            .style(Style::default().fg(Color::DarkGray))
            .wrap(Wrap { trim: false });
        frame.render_widget(empty, inner);
        return;
    }

    // Each session takes 4 rows (3 lines + gap). Keep the selection inside
    // the window, scrolling only when it would leave it.
    let per_page = ((inner.height as usize) / 4).max(1);
    app.list_page_size = per_page;
    if app.selected < app.list_offset {
        app.list_offset = app.selected;
    } else if app.selected >= app.list_offset + per_page {
        app.list_offset = app.selected + 1 - per_page;
    }
    let max_offset = app.filtered_indices.len().saturating_sub(per_page);
    app.list_offset = app.list_offset.min(max_offset);

    let mut lines: Vec<Line> = Vec::new();
    let max_width = inner.width as usize;

    for (visual_idx, &real_idx) in app
        .filtered_indices
        .iter()
        .enumerate()
        .skip(app.list_offset)
    {
        if lines.len() >= inner.height as usize {
            break;
        }

        let session = &app.sessions[real_idx];
        let is_selected = visual_idx == app.selected;
        let is_bookmarked = app.bookmarks.contains(&session.id);

        let prefix = match (is_selected, is_bookmarked) {
            (true, true) => "▸★ ",
            (true, false) => "▸  ",
            (false, true) => " ★ ",
            (false, false) => "   ",
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
        let branch_style = if is_selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::Yellow)
        };
        let name_style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);

        // Line 1: prefix · timestamp · project · branch · name, fitted so the
        // name (the most telling part) survives narrow panes.
        let ts = format!("{}  ", chrono_format(session.timestamp));
        let avail = max_width.saturating_sub(prefix.width() + ts.width());
        let (project, branch, name) = fit_line1(&session.project, &session.branch, &session.name, avail);
        let mut line1_spans: Vec<Span> = vec![
            Span::styled(prefix, highlight),
            Span::styled(ts, dim),
            Span::styled(project, highlight),
        ];
        if !branch.is_empty() {
            line1_spans.push(Span::styled(format!("  {}", branch), branch_style));
        }
        if !name.is_empty() {
            line1_spans.push(Span::styled(format!("  {}", name), name_style));
        }
        lines.push(Line::from(line1_spans));

        // Line 2: duration · size [category] · msgs · PR · first message
        let category = size_category(session.file_size);
        let category_color = match category {
            "quick" => Color::Green,
            "medium" => Color::Yellow,
            "deep" => Color::Magenta,
            "massive" => Color::Red,
            _ => Color::White,
        };
        let dur_str = format!("   {}  ", format_duration(session.duration_secs));
        let size_str = format!("{} ", format_file_size(session.file_size));
        let cat_str = format!("[{}]", category);
        let msgs_str = match session.message_count {
            0 => String::new(),
            1 => " · 1 msg".to_string(),
            n => format!(" · {} msgs", n),
        };
        let pr_str = pr_badge(session)
            .map(|b| format!(" · {}", b))
            .unwrap_or_default();
        let used = dur_str.width() + size_str.width() + cat_str.width() + msgs_str.width() + pr_str.width();
        let title_budget = max_width.saturating_sub(used + 4);
        let title_str = if title_budget > 3 && !session.title.is_empty() {
            format!("  \"{}\"", truncate_width(&session.title, title_budget))
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
            Span::styled(pr_str, Style::default().fg(Color::Magenta)),
            Span::styled(title_str, dim),
        ]));

        // Line 3: where you left off (skipped when it's just the first message again)
        if !session.last_message.is_empty() && session.last_message != session.title {
            let left_off = format!("   └ left off: \"{}\"", session.last_message);
            lines.push(Line::from(Span::styled(
                truncate_width(&left_off, max_width),
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

fn empty_list_message(app: &App) -> String {
    if app.sessions.is_empty() {
        return format!(
            "No Claude Code sessions found in {}.",
            display_path(&crate::index::claude_projects_dir())
        );
    }
    let mut hints: Vec<&str> = Vec::new();
    let mut msg = if !app.search_query.is_empty() {
        hints.push("Esc  clears the search");
        format!("No sessions match \"{}\".", app.search_query.text())
    } else if let Some(filter) = app.size_filter {
        format!("No {} sessions here.", filter)
    } else {
        "No sessions in this project yet.".to_string()
    };
    if app.size_filter.is_some() {
        hints.push("0  clears the size filter");
    }
    if app.scope == Scope::Current && app.scope_root.is_some() {
        hints.push("a  shows all projects");
    }
    if !hints.is_empty() {
        msg.push_str("\n\n");
        msg.push_str(&hints.join("\n"));
    }
    msg
}

/// Shrink project/branch/name to fit `avail` columns (with two-space gaps),
/// sacrificing the branch, then the project, before the name.
fn fit_line1(project: &str, branch: &str, name: &str, avail: usize) -> (String, String, String) {
    let total = |p: &str, b: &str, n: &str| {
        p.width()
            + if b.is_empty() { 0 } else { 2 + b.width() }
            + if n.is_empty() { 0 } else { 2 + n.width() }
    };
    let (mut p, mut b, mut n) = (project.to_string(), branch.to_string(), name.to_string());
    if total(&p, &b, &n) > avail {
        b = truncate_width(&b, 20);
    }
    if total(&p, &b, &n) > avail {
        p = truncate_width(&p, 20);
    }
    if total(&p, &b, &n) > avail && !n.is_empty() {
        let room = avail.saturating_sub(total(&p, &b, "") + 2);
        n = truncate_width(&n, room);
    }
    (p, b, n)
}

fn pr_badge(s: &SessionMeta) -> Option<String> {
    match s.prs.as_slice() {
        [] => None,
        [pr] => Some(format!("PR #{}", pr.number)),
        prs => Some(format!("{} PRs", prs.len())),
    }
}

fn draw_timeline(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Timeline ");

    let inner = block.inner(area);
    frame.render_widget(block, area);

    use chrono::{Datelike, Local, NaiveDate, TimeZone};

    let today = Local::now().date_naive();
    let num_weeks: usize = ((inner.width as usize).saturating_sub(5)) / 2;
    let num_weeks = num_weeks.clamp(4, 26);

    // Find the Monday of the earliest week we'll show
    let days_since_monday = today.weekday().num_days_from_monday();
    let this_monday = today - chrono::Duration::days(days_since_monday as i64);
    let start_date = this_monday - chrono::Duration::weeks(num_weeks as i64 - 1);

    // Count sessions per date, honoring the current scope so the heatmap
    // matches the list.
    let mut counts: std::collections::HashMap<NaiveDate, u32> = std::collections::HashMap::new();
    for s in app.sessions.iter().filter(|s| app.in_scope(s)) {
        if let chrono::LocalResult::Single(dt) = Local.timestamp_opt(s.timestamp, 0) {
            let date = dt.date_naive();
            if date >= start_date && date <= today {
                *counts.entry(date).or_insert(0) += 1;
            }
        }
    }

    let mut lines: Vec<Line> = Vec::new();

    // Month labels row
    let mut month_spans: Vec<Span> = vec![Span::raw("    ")]; // left padding for day labels
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
        .max_by_key(|&(d, v)| (*v, *d))
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

fn speaker_style(speaker: Speaker) -> Style {
    match speaker {
        Speaker::User => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        Speaker::Assistant => Style::default().fg(Color::White),
        Speaker::Tool => Style::default().fg(Color::Rgb(110, 160, 190)),
        Speaker::Recap => Style::default()
            .fg(Color::Rgb(215, 175, 95))
            .add_modifier(Modifier::ITALIC),
    }
}

fn draw_preview(frame: &mut Frame, app: &mut App, area: Rect) {
    let border_style = if matches!(app.focus, Focus::Preview | Focus::PreviewSearch) {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    // Width first: wrapped-row offsets (and so the scroll range) depend on it.
    let inner_width = area.width.saturating_sub(2);
    if app.preview_inner_width != inner_width {
        app.preview_inner_width = inner_width;
        app.recompute_preview_offsets();
    }

    // Track viewport height (inner minus the optional search row) so the app
    // can clamp over-scroll and report a position percentage.
    let inner_height = area.height.saturating_sub(2) as usize;
    let viewport = if app.focus == Focus::PreviewSearch {
        inner_height.saturating_sub(1)
    } else {
        inner_height
    };
    app.preview_viewport_height = viewport;
    let max_scroll = app.max_preview_scroll();
    // A resize can leave the old position past the new end.
    app.preview_scroll = app.preview_scroll.min(max_scroll);
    let scroll_pct = if !app.show_files && max_scroll > 0 {
        Some((app.preview_scroll * 100 + max_scroll / 2) / max_scroll)
    } else {
        None
    };

    let session_label = app
        .selected_session()
        .map(|s| truncate_width(s.display_name(), 40))
        .unwrap_or_default();
    let meta_badges = app.selected_session().map(session_badges).unwrap_or_default();
    let title = if app.show_files {
        format!(" Files changed — {} ", session_label)
    } else if !session_label.is_empty() {
        format!(" {}{} ", session_label, meta_badges)
    } else {
        " Preview ".to_string()
    };
    // Position/progress on the right, so long titles can't push it off.
    let status = if app.show_files {
        let n = app
            .selected_session()
            .map(|s| s.changed_files.len())
            .unwrap_or(0);
        format!(" {} files ", n)
    } else if app.preview_loading {
        " loading… ".to_string()
    } else if !app.preview_search_query.is_empty() && app.preview_search_matches.is_empty() {
        " no matches ".to_string()
    } else if !app.preview_search_matches.is_empty() {
        format!(
            " match {}/{} ",
            app.preview_search_current + 1,
            app.preview_search_matches.len()
        )
    } else if let Some(pct) = scroll_pct {
        format!(" {}% ", pct)
    } else {
        String::new()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(title))
        .title(Line::from(status).right_aligned());

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

    if app.show_files {
        let dim = Style::default().fg(Color::DarkGray);
        let lines: Vec<Line> = match app.selected_session() {
            None => vec![Line::from(Span::styled("No session selected.", dim))],
            Some(s) if s.changed_files.is_empty() => {
                vec![Line::from(Span::styled("No tracked file changes.", dim))]
            }
            Some(s) => {
                let room = (preview_area.width as usize).saturating_sub(2);
                let base = format!("{}/", s.cwd);
                s.changed_files
                    .iter()
                    .skip(app.preview_scroll)
                    .take(viewport)
                    .map(|f| {
                        let rel = if s.cwd.is_empty() {
                            f.as_str()
                        } else {
                            f.strip_prefix(&base).unwrap_or(f)
                        };
                        let shown = truncate_left(&crate::parser::sanitize_line(rel), room);
                        Line::from(Span::styled(
                            format!("  {}", shown),
                            Style::default().fg(Color::White),
                        ))
                    })
                    .collect()
            }
        };
        frame.render_widget(Paragraph::new(lines), preview_area);
    } else if app.preview_lines.is_empty() {
        let msg = if app.filtered_indices.is_empty() {
            "No session selected."
        } else if app.preview_loading {
            "Loading conversation…"
        } else {
            // A session is selected but its extraction produced nothing
            // (e.g. only slash commands, no conversation).
            "No conversation content in this session."
        };
        let p = Paragraph::new(msg).style(Style::default().fg(Color::DarkGray));
        frame.render_widget(p, preview_area);
    } else {
        let lines = visible_preview_lines(app, viewport);
        frame.render_widget(Paragraph::new(lines), preview_area);
    }

    // Draw preview search bar
    if let Some(area) = search_area {
        let (visible, col) = visible_input(
            &app.preview_search_query,
            (area.width as usize).saturating_sub(1),
        );
        let search_line = Line::from(vec![
            Span::styled(
                "/",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(visible, Style::default().fg(Color::Yellow)),
        ]);
        let p = Paragraph::new(search_line).style(Style::default().bg(Color::Rgb(30, 30, 30)));
        frame.render_widget(p, area);
        frame.set_cursor_position((area.x + 1 + col, area.y));
    }
}

/// Build only the rows inside the viewport. Long sessions have tens of
/// thousands of wrapped rows; formatting them all every frame pegs a core.
fn visible_preview_lines(app: &App, viewport: usize) -> Vec<Line<'static>> {
    let width = app.preview_body_width();
    let scroll = app.preview_scroll;
    let offsets = &app.preview_line_offsets;
    let start_msg = offsets.partition_point(|&o| o <= scroll).saturating_sub(1);
    let mut skip = scroll.saturating_sub(offsets.get(start_msg).copied().unwrap_or(0));

    let query = app.preview_search_query.text().to_lowercase();
    let current_match = app
        .preview_search_matches
        .get(app.preview_search_current)
        .copied();
    let match_style_current = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let match_style_other = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(viewport);
    for (msg_idx, (text, _lower, speaker)) in app.preview_lines.iter().enumerate().skip(start_msg) {
        if lines.len() >= viewport {
            break;
        }
        let is_current = current_match == Some(msg_idx);
        let is_match = is_current
            || (!query.is_empty() && app.preview_search_matches.binary_search(&msg_idx).is_ok());

        // Match indicator: ▸ for current match, │ for other matches
        let marker = if is_current {
            Span::styled("▸", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        } else if is_match {
            Span::styled("│", Style::default().fg(Color::Yellow))
        } else {
            Span::raw(" ")
        };
        let base_style = speaker_style(*speaker);
        let highlight_query = if is_match { query.as_str() } else { "" };
        let match_style = if is_current { match_style_current } else { match_style_other };

        for (row_idx, row) in message_rows(text, width).into_iter().enumerate() {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            if lines.len() >= viewport {
                break;
            }
            let lead = if row_idx == 0 {
                Span::styled(speaker.prefix(), base_style.add_modifier(Modifier::BOLD))
            } else {
                Span::raw(" ".repeat(PREFIX_WIDTH))
            };
            let mut spans = vec![marker.clone(), lead];
            spans.extend(highlight_spans(&row, highlight_query, base_style, match_style));
            lines.push(Line::from(spans));
        }
        // Blank separator row between messages.
        if skip > 0 {
            skip -= 1;
        } else if lines.len() < viewport {
            lines.push(Line::from(""));
        }
    }
    lines
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let bar_bg = Color::Rgb(40, 40, 40);

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
            .map(|s| truncate_width(s.display_name(), 40))
            .unwrap_or_default();
        let line = Line::from(vec![
            Span::styled(" DELETE ", warn),
            Span::styled(format!("\"{}\"? ", session_name), hint),
            Span::styled("y ", warn),
            Span::styled("confirm  ", hint),
            Span::styled("any other key ", warn),
            Span::styled("cancel ", hint),
        ]);
        let paragraph = Paragraph::new(line).style(Style::default().bg(Color::Red));
        frame.render_widget(paragraph, area);
        return;
    }

    let sort = format!("sort:{}", app.sort_label());
    let scope = format!("scope:{}", app.scope.label());
    let size = match app.size_filter {
        Some(f) => format!("size:{}", f),
        None => "size".to_string(),
    };
    let enter = if app.print_mode {
        "print id"
    } else if app.enter_yolo {
        "resume (yolo)"
    } else {
        "resume"
    };

    let hints: Vec<(&str, &str)> = if app.show_help {
        vec![("any key", "close help")]
    } else {
        match app.focus {
            Focus::Search => vec![
                ("type", "to filter"),
                ("↑↓", "move"),
                ("Enter", "browse results"),
                ("Tab", "preview"),
                ("Esc", "clear"),
            ],
            Focus::PreviewSearch => vec![
                ("type", "to find"),
                ("Enter", "keep matches (n/N)"),
                ("Esc", "cancel"),
            ],
            Focus::Rename => vec![("Enter", "save"), ("Esc", "cancel")],
            Focus::Preview => vec![
                ("↑↓ PgUp PgDn", "scroll"),
                ("g G", "top/end"),
                ("/", "find"),
                ("n N", "next/prev"),
                ("T", "tools"),
                ("f", "files"),
                ("Tab", "list"),
                ("?", "help"),
            ],
            Focus::List if app.view_mode == ViewMode::Timeline => {
                vec![("t Esc", "back to list"), ("q", "quit")]
            }
            Focus::List => vec![
                ("↑↓", "move"),
                ("Enter", enter),
                ("/", "search"),
                ("s", sort.as_str()),
                ("a", scope.as_str()),
                ("1-4", size.as_str()),
                ("b", "pin"),
                ("r", "rename"),
                ("c", "copy cmd"),
                ("Tab", "preview"),
                ("?", "all keys"),
                ("q", "quit"),
            ],
        }
    };

    let key = Style::default()
        .fg(Color::Cyan)
        .bg(bar_bg)
        .add_modifier(Modifier::BOLD);
    let desc = Style::default()
        .fg(Color::Rgb(180, 180, 180))
        .bg(bar_bg);
    let mut spans = vec![Span::styled(" ", desc)];
    let mut used = 1;
    let width = area.width as usize;
    for (k, d) in hints {
        let piece = k.width() + 1 + d.width() + 2;
        // Drop trailing hints rather than wrapping or clipping mid-word; `?`
        // lists everything.
        if used + piece > width {
            break;
        }
        spans.push(Span::styled(format!("{} ", k), key));
        spans.push(Span::styled(format!("{}  ", d), desc));
        used += piece;
    }

    let paragraph = Paragraph::new(Line::from(spans)).style(Style::default().bg(bar_bg));
    frame.render_widget(paragraph, area);
}

/// Compact metadata suffix for the preview title: permission mode, skills,
/// linked PRs. Returns "" when there's nothing to show, otherwise
/// " · plan · tdd · PR #12".
fn session_badges(s: &SessionMeta) -> String {
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
        // Plugin skills are namespaced ("plugin:skill"); the skill part is enough.
        let shown: Vec<&str> = s
            .skills
            .iter()
            .take(3)
            .map(|x| x.rsplit(':').next().unwrap_or(x))
            .collect();
        let mut sk = shown.join(",");
        let extra = s.skills.len().saturating_sub(3);
        if extra > 0 {
            sk.push_str(&format!("+{}", extra));
        }
        parts.push(sk);
    }
    if let Some(pr) = pr_badge(s) {
        parts.push(pr);
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" · {}", parts.join(" · "))
    }
}

/// Centered overlay listing every keybinding. Drawn on top of everything when open.
fn draw_help(frame: &mut Frame, area: Rect, app: &App) {
    let enter = if app.print_mode {
        "print session id and exit"
    } else if app.enter_yolo {
        "resume with --dangerously-skip-permissions"
    } else {
        "resume"
    };
    let sections: &[(&str, &[(&str, &str)])] = &[
        (
            "Sessions",
            &[
                ("↑↓  j k", "move (wraps around)"),
                ("g G  Home End", "first / last"),
                ("PgUp PgDn", "page"),
                ("Enter", enter),
                ("l", "resume (normal permissions)"),
                ("c", "copy the resume command"),
                ("p", "print session id and exit"),
                ("/", "search titles, files, tickets, full text"),
                ("s", "sort: date · size · duration · messages"),
                ("a", "scope: this project ↔ all projects"),
                ("1-4  0", "size filter: quick…massive / clear"),
                ("b", "pin to top"),
                ("r", "rename (same as Claude Code's /rename)"),
                ("o", "open the linked pull request"),
                ("e", "export as markdown"),
                ("d", "delete"),
                ("t", "activity timeline"),
            ],
        ),
        (
            "Preview",
            &[
                ("Tab", "switch between list and preview"),
                ("↑↓ PgUp PgDn", "scroll (also Ctrl+D / Ctrl+U)"),
                ("g G", "top / end of conversation"),
                ("/  n N", "find in conversation / next, previous"),
                ("T", "show tool calls"),
                ("f", "show changed files"),
            ],
        ),
        (
            "Anywhere",
            &[
                ("Esc", "back: clear search → list → quit"),
                ("q  Ctrl+C", "quit"),
            ],
        ),
    ];

    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(Color::Rgb(200, 200, 200));
    let head_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line> = Vec::new();
    for (i, (heading, rows)) in sections.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(format!(" {}", heading), head_style)));
        for (k, d) in rows.iter() {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<15}", k), key_style),
                Span::styled((*d).to_string(), desc_style),
            ]));
        }
    }

    let w = 64u16.min(area.width.saturating_sub(2));
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let popup = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Keys — press any key to close ")
        .style(Style::default().bg(Color::Rgb(20, 20, 20)));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The slice of an input line that fits in `width` columns with the cursor
/// visible, plus the cursor's column within it. Long queries scroll
/// horizontally instead of running off the box.
fn visible_input(input: &TextInput, width: usize) -> (String, u16) {
    let chars: Vec<char> = input.text().chars().collect();
    let cursor = input.cursor_chars().min(chars.len());
    let cw = |c: &char| c.width().unwrap_or(0);
    let width = width.max(1);
    // Walk back from the cursor to the furthest start that keeps it on screen.
    let mut start = cursor;
    let mut before = 0;
    while start > 0 && before + cw(&chars[start - 1]) < width {
        start -= 1;
        before += cw(&chars[start]);
    }
    let mut visible = String::new();
    let mut used = 0;
    for c in &chars[start..] {
        let w = cw(c);
        if used + w > width {
            break;
        }
        visible.push(*c);
        used += w;
    }
    let col = chars[start..cursor].iter().map(cw).sum::<usize>().min(u16::MAX as usize) as u16;
    (visible, col)
}

fn chrono_format(timestamp: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(timestamp, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%b %e %H:%M").to_string(),
        _ => "??? ?? ??:??".to_string(),
    }
}

/// Cut `s` to at most `max_cols` display columns, ending in `…` when cut.
/// Wide characters (CJK, emoji) count as two columns.
pub fn truncate_width(s: &str, max_cols: usize) -> String {
    if s.width() <= max_cols {
        return s.to_string();
    }
    if max_cols == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > max_cols - 1 {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Keep the end of `s` (a path's file name is its telling part) within
/// `max_cols` columns, marking the cut with a leading `…`.
fn truncate_left(s: &str, max_cols: usize) -> String {
    if s.width() <= max_cols {
        return s.to_string();
    }
    if max_cols == 0 {
        return String::new();
    }
    let mut tail: Vec<char> = Vec::new();
    let mut used = 0;
    for c in s.chars().rev() {
        let w = c.width().unwrap_or(0);
        if used + w > max_cols - 1 {
            break;
        }
        tail.push(c);
        used += w;
    }
    let mut out = String::from("…");
    out.extend(tail.into_iter().rev());
    out
}

/// Wrapped rows of a preview message body: split on newlines (paragraphs and
/// code keep their shape), each line word-wrapped to `width` display columns.
/// Runs of blank lines collapse to one. Always at least one row.
pub fn message_rows(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut prev_blank = true; // also drops leading blank lines
    for line in text.split('\n') {
        let line = crate::parser::sanitize_line(line);
        let line = line.trim_end();
        if line.is_empty() {
            if !prev_blank {
                rows.push(String::new());
            }
            prev_blank = true;
            continue;
        }
        prev_blank = false;
        rows.extend(wrap_text(line, width));
    }
    while rows.len() > 1 && rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// Word-wrap one line to `width` display columns. Breaks at the last space
/// that fits (after any indentation), or mid-word when a word is too long.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut result = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        let mut used = 0;
        let mut cut = None;
        let mut last_space = None;
        let mut seen_word = false;
        for (i, c) in remaining.char_indices() {
            let w = c.width().unwrap_or(0);
            if used + w > width {
                cut = Some(i);
                break;
            }
            if c == ' ' {
                if seen_word {
                    last_space = Some(i);
                }
            } else {
                seen_word = true;
            }
            used += w;
        }
        let Some(cut) = cut else {
            result.push(remaining.to_string());
            break;
        };
        let split = match last_space {
            Some(s) if s > 0 => s,
            // A single character wider than the row still has to go somewhere.
            _ if cut == 0 => remaining.chars().next().map_or(remaining.len(), char::len_utf8),
            _ => cut,
        };
        result.push(remaining[..split].trim_end().to_string());
        remaining = remaining[split..].trim_start_matches(' ');
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
    fn test_truncate_width_counts_columns() {
        // Emoji are two columns wide; cut on char boundaries, never mid-codepoint.
        assert_eq!(truncate_width("🐢🐢🐢🐢", 5), "🐢🐢…");
        assert_eq!(truncate_width("🐢🐢", 4), "🐢🐢");
        assert_eq!(truncate_width("αβγδε", 3), "αβ…");
        assert_eq!(truncate_width("abc", 0), "");
        assert_eq!(truncate_width("abc", 1), "…");
        assert_eq!(truncate_width("abc", 3), "abc");
    }

    #[test]
    fn test_truncate_left_keeps_file_name() {
        assert_eq!(truncate_left("/very/long/path/to/route.ts", 12), "…to/route.ts");
        assert_eq!(truncate_left("short.rs", 12), "short.rs");
        assert_eq!(truncate_left("abc", 0), "");
    }

    #[test]
    fn test_wrap_text_unicode_no_panic() {
        // Long unbroken emoji run wider than the wrap width.
        let text = "🐢".repeat(50);
        let chunks = wrap_text(&text, 10);
        assert_eq!(chunks.len(), 10);
        assert!(chunks.iter().all(|c| c.width() <= 10));
        // Greek with spaces wraps on spaces.
        let chunks = wrap_text("καλημέρα κόσμε γεια σου", 10);
        assert!(chunks.len() >= 2);
        assert!(chunks.iter().all(|c| c.width() <= 10));
        // Width 1 with a wide char must still make progress.
        assert_eq!(wrap_text("🐢🐢", 1).len(), 2);
    }

    #[test]
    fn test_wrap_text_zero_width() {
        assert_eq!(wrap_text("hello", 0), vec!["hello".to_string()]);
        assert!(wrap_text("", 10).is_empty());
    }

    #[test]
    fn test_wrap_text_keeps_indentation_on_first_row() {
        let rows = wrap_text("    let x = compute_something(a, b);", 20);
        assert!(rows[0].starts_with("    let"), "got {:?}", rows);
        assert!(rows.iter().all(|r| r.width() <= 20), "got {:?}", rows);
    }

    #[test]
    fn test_message_rows_preserve_newlines() {
        let rows = message_rows("first paragraph\n\n\n\nsecond\n  indented code", 40);
        assert_eq!(
            rows,
            vec!["first paragraph", "", "second", "  indented code"],
            "paragraph breaks survive, blank runs collapse"
        );
        assert_eq!(message_rows("", 40), vec![String::new()]);
        assert_eq!(message_rows("\n\nx\n\n", 40), vec!["x".to_string()]);
    }

    #[test]
    fn test_visible_input_scrolls_to_cursor() {
        // Short input: shown whole, cursor at the end.
        assert_eq!(visible_input(&TextInput::from("ab"), 10), ("ab".to_string(), 2));
        // Cursor mid-string: column follows the cursor, not the text length.
        let mut mid = TextInput::from("abcdef");
        mid.move_home();
        assert_eq!(visible_input(&mid, 10).1, 0);
        // Long input: the tail around the cursor stays visible.
        let (vis, col) = visible_input(&TextInput::from("x".repeat(100).as_str()), 10);
        assert!(vis.width() <= 10);
        assert_eq!(col, 9);
        // Huge paste: no overflow.
        let (_, col) = visible_input(&TextInput::from("y".repeat(70_000).as_str()), 40);
        assert_eq!(col, 39);
    }

    #[test]
    fn test_fit_line1_keeps_name_visible() {
        let (p, b, n) = fit_line1(
            "acme/web",
            "feature/PIT-1234-a-very-long-branch-name-indeed",
            "Fix login",
            50,
        );
        assert_eq!(n, "Fix login");
        assert!(b.ends_with('…'));
        assert!(p.width() + b.width() + n.width() + 4 <= 50);
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
