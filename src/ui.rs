use crate::app::{App, Density, Focus, Scope, Snippet, ViewMode};
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

    app.search_area = main_chunks[0];
    // Panes not drawn this frame can't be clicked.
    app.list_area = Rect::default();
    app.preview_area = Rect::default();
    app.timeline_cells.clear();
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
        let exact = match app.exact_match_count() {
            0 => String::new(),
            n => format!("{} exact · ", n),
        };
        format!(
            " Search ({}{}/{}) ",
            exact,
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
            "words, tickets, #PR · filters: project: branch: file: is:open|pinned|pr \
             after:7d before:2026-10-01 on:today"
        } else {
            "Press / to search, or paste"
        };
        let hint = truncate_width(hint, inner.width as usize);
        let p = Paragraph::new(Span::styled(hint, Style::default().fg(Color::DarkGray))).block(block);
        frame.render_widget(p, area);
    } else {
        let (visible, _) = visible_input(&app.search_query, inner.width as usize);
        frame.render_widget(Paragraph::new(query_spans(&visible)).block(block), area);
    }

    if focused {
        let (_, col) = visible_input(&app.search_query, inner.width as usize);
        frame.set_cursor_position((inner.x + col, inner.y));
    }
}

/// Below this many columns the two panes are both too cramped to read;
/// only the focused one is shown, full width (Tab switches).
pub const NARROW_WIDTH: u16 = 100;

/// Whether only the focused pane is drawn (narrow terminal, or `z`).
pub fn single_pane(app: &App, width: u16) -> bool {
    app.zoom || width < NARROW_WIDTH
}

/// The search box's text with `key:value` filters coloured, so it's clear
/// which words filter and which are searched for.
fn query_spans(visible: &str) -> Line<'static> {
    let filter = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    let mut word = String::new();
    let flush = |word: &mut String, spans: &mut Vec<Span<'static>>| {
        if !word.is_empty() {
            let style = if crate::query::is_filter_token(word) {
                filter
            } else {
                Style::default()
            };
            spans.push(Span::styled(std::mem::take(word), style));
        }
    };
    for c in visible.chars() {
        if c.is_whitespace() {
            flush(&mut word, &mut spans);
            spans.push(Span::raw(c.to_string()));
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut spans);
    Line::from(spans)
}

fn draw_content(frame: &mut Frame, app: &mut App, area: Rect) {
    let draw_left = |frame: &mut Frame, app: &mut App, area: Rect| match app.view_mode {
        ViewMode::Normal => draw_session_list(frame, app, area),
        ViewMode::Timeline => draw_timeline(frame, app, area),
    };
    if single_pane(app, area.width) {
        if matches!(app.focus, Focus::Preview | Focus::PreviewSearch) {
            draw_preview(frame, app, area);
        } else {
            // The hidden preview keeps its geometry current, so scrolling it
            // to a search hit lands right once it's shown.
            sync_preview_geometry(app, area);
            draw_left(frame, app, area);
        }
        return;
    }
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);
    draw_left(frame, app, panes[0]);
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
    app.list_area = area;
    app.list_rows.clear();

    if app.filtered_indices.is_empty() {
        let msg = empty_list_message(app);
        let empty = Paragraph::new(msg)
            .style(Style::default().fg(Color::DarkGray))
            .wrap(Wrap { trim: false });
        frame.render_widget(empty, inner);
        return;
    }

    let compact = app.density == Density::Compact;
    let searching = !app.search_query.is_empty();
    let exact_count = app.exact_match_count();
    let split_tiers = exact_count > 0 && exact_count < app.filtered_indices.len();
    let height = inner.height as usize;
    // Compact rows have no gap to hold the exact/partial divider; it takes a
    // row of its own.
    let usable = if compact && split_tiers { height.saturating_sub(1) } else { height };

    // Keep the selection inside the window, scrolling only when it would leave it.
    let per_page = (usable / app.density.rows()).max(1);
    app.list_page_size = per_page;
    if app.selected < app.list_offset {
        app.list_offset = app.selected;
    } else if app.selected >= app.list_offset + per_page {
        app.list_offset = app.selected + 1 - per_page;
    }
    let max_offset = app.filtered_indices.len().saturating_sub(per_page);
    app.list_offset = app.list_offset.min(max_offset);

    // Why each visible search result matched, when its row can't show it.
    let visible: Vec<usize> = app
        .filtered_indices
        .iter()
        .skip(app.list_offset)
        .take(per_page + 1)
        .copied()
        .collect();
    let snippets: std::collections::HashMap<usize, Snippet> = if searching && !compact {
        visible
            .iter()
            .filter_map(|&i| app.snippet(i).map(|s| (i, s)))
            .collect()
    } else {
        Default::default()
    };

    let mut owners: Vec<usize> = Vec::new();
    let lines = {
    let app: &App = app;
    let max_width = inner.width as usize;
    let phrase = app.search_phrase();
    let phrase_terms = [phrase.as_str()];
    let words: Vec<&str> = phrase.split(' ').filter(|w| !w.is_empty()).collect();
    let match_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let now = chrono::Local::now();

    let mut lines: Vec<Line> = Vec::new();
    for (visual_idx, &real_idx) in app
        .filtered_indices
        .iter()
        .enumerate()
        .skip(app.list_offset)
    {
        if lines.len() >= height {
            break;
        }
        let first_row = lines.len();

        let session = &app.sessions[real_idx];
        let is_selected = visual_idx == app.selected;
        let is_exact = app.is_exact_match(real_idx);
        // Exact matches show where the query hit; partial ones stay plain.
        let hl: &[&str] = if is_exact { &phrase_terms } else { &[] };
        let st = RowStyles::new(is_selected);

        // Prefix: selection, pin, live marker.
        let live = app.live_status(session);
        let mut line1_spans: Vec<Span> = vec![
            Span::styled(if is_selected { "▸" } else { " " }, st.text),
            Span::styled(
                if app.bookmarks.contains(&session.id) { "★" } else { " " },
                st.text,
            ),
            match live {
                Some(l) => Span::styled(
                    "●",
                    Style::default().fg(if l.busy { Color::Yellow } else { Color::Green }),
                ),
                None => Span::raw(" "),
            },
            Span::raw(" "),
        ];
        let ts = format!("{}  ", when_label(session.timestamp, now));
        line1_spans.push(Span::styled(ts.clone(), st.dim));

        // Compact rows end with the stats, right-aligned.
        let category = size_category(session.file_size);
        let category_color = match category {
            "quick" => Color::Green,
            "medium" => Color::Yellow,
            "deep" => Color::Magenta,
            "massive" => Color::Red,
            _ => Color::Reset,
        };
        let msgs = match session.message_count {
            0 => String::new(),
            1 => "1 msg".to_string(),
            n => format!("{} msgs", n),
        };
        let size = format_file_size(session.file_size);
        let mut avail = max_width.saturating_sub(PREFIX_COLS + ts.width());
        // Compact stats, as room allows: messages and size, the size alone, or none.
        let (show_msgs, stats_width) = if !compact {
            (false, 0)
        } else if avail >= 23 + 40 {
            (true, 23)
        } else if avail >= 11 + 30 {
            (false, 11)
        } else {
            (false, 0)
        };
        let show_stats = stats_width > 0;
        avail -= stats_width;

        // Headline: project · branch · name, fitted so the name (the most
        // telling part) survives narrow panes.
        // Compact rows have no second line for the first message, so an
        // unnamed session shows it here.
        let headline = if compact { session.display_name() } else { session.name.as_str() };
        let (project, branch, name) = fit_line1(&session.project, &session.branch, headline, avail);
        line1_spans.extend(highlight_spans(&project, hl, st.text, match_style));
        if !branch.is_empty() {
            let branch = format!("  {}", branch);
            line1_spans.extend(highlight_spans(&branch, hl, st.branch, match_style));
        }
        if !name.is_empty() {
            line1_spans.extend(highlight_spans(&format!("  {}", name), hl, st.name, match_style));
        }
        if show_stats {
            let used: usize = line1_spans.iter().map(|s| s.content.width()).sum();
            let pad = max_width.saturating_sub(used + stats_width);
            line1_spans.push(Span::raw(" ".repeat(pad + 2)));
            if show_msgs {
                line1_spans.push(Span::styled(format!("{:>10}", msgs), st.dim));
                line1_spans.push(Span::raw("  "));
            }
            line1_spans.push(Span::styled(
                format!("{:>9}", size),
                Style::default().fg(category_color),
            ));
        }
        lines.push(Line::from(line1_spans));

        if !compact {
            // Line 2: duration · size [category] · msgs · PR · first message,
            // dropping the least telling parts when the pane is narrow.
            let dur_str = format!("{}{}  ", INDENT, format_duration(session.duration_secs));
            let size_str = format!("{} ", size);
            let mut cat_str = format!("[{}]", category);
            let mut msgs_str = if msgs.is_empty() {
                String::new()
            } else {
                format!(" · {}", msgs)
            };
            let mut pr_str = pr_badge(session)
                .map(|b| format!(" · {}", b))
                .unwrap_or_default();
            let used = |c: &str, m: &str, p: &str| {
                dur_str.width() + size_str.width() + c.width() + m.width() + p.width()
            };
            if used(&cat_str, &msgs_str, &pr_str) > max_width {
                cat_str.clear();
            }
            if used(&cat_str, &msgs_str, &pr_str) > max_width {
                pr_str.clear();
            }
            if used(&cat_str, &msgs_str, &pr_str) > max_width {
                msgs_str.clear();
            }
            let title_budget = max_width.saturating_sub(used(&cat_str, &msgs_str, &pr_str) + 4);
            let title_str = if title_budget > 3 && !session.title.is_empty() {
                format!("  \"{}\"", truncate_width(&session.title, title_budget))
            } else {
                String::new()
            };
            let mut line2_spans = vec![
                Span::styled(truncate_width(&dur_str, max_width), st.dim),
                Span::styled(size_str, st.dim),
                Span::styled(
                    cat_str,
                    Style::default().fg(category_color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(msgs_str, st.dim),
                Span::styled(pr_str, Style::default().fg(Color::Magenta)),
            ];
            line2_spans.extend(highlight_spans(&title_str, hl, st.dim, match_style));
            lines.push(Line::from(line2_spans));

            // Line 3: why it matched (search hits the row doesn't show), else
            // where you left off (skipped when it's just the first message again).
            let quiet = Style::default().fg(Color::DarkGray);
            if let Some(snippet) = snippets.get(&real_idx) {
                let label = if snippet.in_file { "└ file: " } else { "└ match: " };
                let room = max_width.saturating_sub(INDENT.width() + label.width());
                let body = window_around(&snippet.text, snippet.hit, room);
                let mut spans = vec![Span::styled(format!("{}{}", INDENT, label), quiet)];
                spans.extend(highlight_spans(&body, &words, quiet, match_style));
                lines.push(Line::from(spans));
            } else if !session.last_message.is_empty() && session.last_message != session.title {
                let left_off = format!("{}└ left off: \"{}\"", INDENT, session.last_message);
                lines.push(Line::from(highlight_spans(
                    &truncate_width(&left_off, max_width),
                    hl,
                    quiet,
                    match_style,
                )));
            } else {
                lines.push(Line::from(""));
            }
        }

        // After the last exact match, a divider separates the partial ones
        // (in the gap row, or a row of its own when compact).
        let next_is_partial = app
            .filtered_indices
            .get(visual_idx + 1)
            .is_some_and(|&next| !app.is_exact_match(next));
        let divider = is_exact && next_is_partial;
        if lines.len() < height && (divider || !compact) {
            if divider {
                lines.push(Line::from(Span::styled(
                    tier_divider("partial matches", max_width),
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                lines.push(Line::from(""));
            }
        }
        owners.extend(std::iter::repeat_n(visual_idx, lines.len() - first_row));
    }
    lines
    };

    // Which session each screen row belongs to, for mouse clicks.
    app.list_rows = owners
        .into_iter()
        .enumerate()
        .map(|(i, visual)| (inner.y + i as u16, visual))
        .collect();
    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

/// Columns taken by the row prefix: selection, pin, live marker, space.
const PREFIX_COLS: usize = 4;
/// Indent of a session's second and third lines (under the timestamp).
const INDENT: &str = "    ";

struct RowStyles {
    text: Style,
    dim: Style,
    branch: Style,
    name: Style,
}

impl RowStyles {
    fn new(selected: bool) -> Self {
        let cyan = Style::default().fg(Color::Cyan);
        Self {
            // The terminal's own foreground, so light themes stay readable.
            text: if selected { cyan.add_modifier(Modifier::BOLD) } else { Style::default() },
            dim: if selected { cyan } else { Style::default().fg(Color::DarkGray) },
            branch: if selected { cyan } else { Style::default().fg(Color::Yellow) },
            name: cyan.add_modifier(Modifier::BOLD),
        }
    }
}

/// List timestamp, 12 columns: "today 14:32", a weekday for the past week
/// ("Tue 09:15"), the date this year ("Oct 24 14:03"), or the date and year
/// before that ("Oct 24  2025").
pub fn when_label(timestamp: i64, now: chrono::DateTime<chrono::Local>) -> String {
    use chrono::{Datelike, Local, TimeZone};
    let chrono::LocalResult::Single(dt) = Local.timestamp_opt(timestamp, 0) else {
        return format!("{:<12}", "?");
    };
    let days_ago = (now.date_naive() - dt.date_naive()).num_days();
    let label = if days_ago == 0 {
        dt.format("today %H:%M").to_string()
    } else if (1..7).contains(&days_ago) {
        dt.format("%a %H:%M").to_string()
    } else if dt.year() == now.year() {
        dt.format("%b %e %H:%M").to_string()
    } else {
        dt.format("%b %e  %Y").to_string()
    };
    format!("{:<12}", label)
}

/// `width` columns of `text` around the char at `hit`: a little context
/// before it, `…` where the text was cut.
fn window_around(text: &str, hit: usize, width: usize) -> String {
    let text = text.trim_end();
    if text.width() <= width {
        return text.to_string();
    }
    if width < 2 {
        return String::new();
    }
    let chars: Vec<char> = text.chars().collect();
    let hit = hit.min(chars.len());
    let cw = |c: char| c.width().unwrap_or(0);
    // Up to a quarter of the room shows what came before the hit.
    let lead_room = width / 4;
    let mut start = hit;
    let mut lead = 0;
    while start > 0 && lead + cw(chars[start - 1]) <= lead_room {
        start -= 1;
        lead += cw(chars[start]);
    }
    let mut out = String::new();
    let mut used = 0;
    if start > 0 {
        out.push('…');
        used += 1;
    }
    let mut end = start;
    while end < chars.len() && used + cw(chars[end]) <= width {
        out.push(chars[end]);
        used += cw(chars[end]);
        end += 1;
    }
    if end < chars.len() {
        while used + 1 > width {
            match out.pop() {
                Some(c) => used -= cw(c),
                None => break,
            }
        }
        out.push('…');
    }
    out
}

/// `── label ─────` filling `width` columns.
fn tier_divider(label: &str, width: usize) -> String {
    let head = format!("   ── {} ", label);
    let fill = width.saturating_sub(head.width());
    truncate_width(&format!("{}{}", head, "─".repeat(fill)), width)
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

fn draw_timeline(frame: &mut Frame, app: &mut App, area: Rect) {
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
    app.timeline_start = Some(start_date);
    // A selection that scrolled out of view (narrower pane) snaps back in.
    if app.timeline_selected() < start_date {
        app.timeline_day = Some(start_date);
    }
    let selected = app.timeline_selected();
    app.timeline_cells.clear();

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
            let x = inner.x + 4 + (w as u16) * 2;
            let y = inner.y + 1 + day_idx as u16;
            app.timeline_cells.push((x, y, date));
            if date == selected {
                spans.push(Span::styled(
                    ch.to_string(),
                    Style::default().fg(color).bg(Color::Cyan),
                ));
                spans.push(Span::raw(" "));
            } else {
                spans.push(Span::styled(format!("{} ", ch), Style::default().fg(color)));
            }
        }

        lines.push(Line::from(spans));
    }

    // The selected day
    lines.push(Line::from(""));
    let picked = counts.get(&selected).copied().unwrap_or(0);
    lines.push(Line::from(vec![
        Span::styled(
            format!("  {}", selected.format("%a %b %e %Y")),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::raw(match picked {
            1 => " · 1 session".to_string(),
            n => format!(" · {} sessions", n),
        }),
        Span::styled(
            if picked > 0 { "  Enter lists them" } else { "" },
            Style::default().fg(Color::DarkGray),
        ),
    ]));

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
        Span::raw(format!("{} sessions", total_sessions)),
        Span::styled("  Active days: ", Style::default().fg(Color::DarkGray)),
        Span::raw(format!("{}", active_days)),
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
        Speaker::Assistant => Style::default(),
        Speaker::Tool => Style::default().fg(Color::Rgb(110, 160, 190)),
        Speaker::Recap => Style::default()
            .fg(Color::Rgb(215, 175, 95))
            .add_modifier(Modifier::ITALIC),
        Speaker::Day => Style::default().fg(Color::DarkGray),
    }
}

fn draw_preview(frame: &mut Frame, app: &mut App, area: Rect) {
    let border_style = if matches!(app.focus, Focus::Preview | Focus::PreviewSearch) {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let viewport = sync_preview_geometry(app, area);
    app.preview_area = area;
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
    let meta_badges = app
        .selected_session()
        .map(|s| session_badges(s, app.live_status(s)))
        .unwrap_or_default();
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
    } else if let Some(hint) = hidden_hits_hint(app) {
        hint.to_string()
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
                        Line::from(Span::raw(format!("  {}", shown)))
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

/// Why a search result's preview shows no hits, when it's because they're
/// in text it doesn't show (tool output, thinking) or in a changed file's
/// path. `None` when the hit is visible in the list row itself.
fn hidden_hits_hint(app: &mut App) -> Option<&'static str> {
    if app.search_query.is_empty() || app.preview_lines.is_empty() || app.show_files {
        return None;
    }
    let real = *app.filtered_indices.get(app.selected)?;
    let snippet = app.snippet(real)?;
    Some(if snippet.in_file {
        " hit in a changed file · f files "
    } else if app.show_tools {
        " hits not in the shown text "
    } else {
        " hits not shown · T tools "
    })
}

/// Record the preview pane's size (`area`, borders included): wrapped-row
/// offsets, and so the scroll range, depend on its width; the viewport
/// (minus the find row while typing) bounds scrolling. Returns the viewport
/// height.
fn sync_preview_geometry(app: &mut App, area: Rect) -> usize {
    let inner_width = area.width.saturating_sub(2);
    if app.preview_inner_width != inner_width {
        app.preview_inner_width = inner_width;
        app.recompute_preview_offsets();
    }
    let inner_height = area.height.saturating_sub(2) as usize;
    let viewport = if app.focus == Focus::PreviewSearch {
        inner_height.saturating_sub(1)
    } else {
        inner_height
    };
    app.preview_viewport_height = viewport;
    viewport
}

/// Build only the rows inside the viewport. Long sessions have tens of
/// thousands of wrapped rows; formatting them all every frame pegs a core.
fn visible_preview_lines(app: &App, viewport: usize) -> Vec<Line<'static>> {
    let width = app.preview_body_width();
    let scroll = app.preview_scroll;
    let offsets = &app.preview_line_offsets;
    let start_msg = offsets.partition_point(|&o| o <= scroll).saturating_sub(1);
    let mut skip = scroll.saturating_sub(offsets.get(start_msg).copied().unwrap_or(0));

    let terms = app.preview_terms();
    let terms: Vec<&str> = terms.iter().map(String::as_str).collect();
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
        let is_match = is_current || app.preview_search_matches.binary_search(&msg_idx).is_ok();

        // Match indicator: ▸ for current match, │ for other matches
        let marker = if is_current {
            Span::styled("▸", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        } else if is_match {
            Span::styled("│", Style::default().fg(Color::Yellow))
        } else {
            Span::raw(" ")
        };
        let base_style = speaker_style(*speaker);
        let highlight_terms: &[&str] = if is_match { &terms } else { &[] };
        let match_style = if is_current { match_style_current } else { match_style_other };

        // A day rule is one row, drawn across the pane.
        if *speaker == Speaker::Day {
            if skip > 0 {
                skip -= 1;
            } else if lines.len() < viewport {
                let full = width + PREFIX_WIDTH;
                let head = format!("── {} ", text);
                let rule = format!("{}{}", head, "─".repeat(full.saturating_sub(head.width())));
                lines.push(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(truncate_width(&rule, full), base_style),
                ]));
            }
        }
        let rows = if *speaker == Speaker::Day {
            Vec::new()
        } else {
            message_rows(text, width)
        };
        for (row_idx, row) in rows.into_iter().enumerate() {
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
            spans.extend(highlight_spans(&row, highlight_terms, base_style, match_style));
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

    // A pending prompt outranks everything: it's waiting for the next key.
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
        let open = app.selected_session().and_then(|s| app.live_status(s)).is_some();
        let mut spans = vec![
            Span::styled(" DELETE ", warn),
            Span::styled(format!("\"{}\"? ", session_name), hint),
            Span::styled("y ", warn),
            Span::styled("confirm  ", hint),
            Span::styled("any other key ", warn),
            Span::styled("cancel ", hint),
        ];
        if open {
            spans.insert(2, Span::styled("(open in Claude Code right now) ", warn));
        }
        let line = Line::from(spans);
        let paragraph = Paragraph::new(line).style(Style::default().bg(Color::Red));
        frame.render_widget(paragraph, area);
        return;
    }

    if let Some(action) = app.confirm_resume {
        let warn = Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
        let hint = Style::default().fg(Color::Black).bg(Color::Yellow);
        let verb = match action {
            crate::app::AppAction::LaunchDangerously(_) => "resume (yolo) anyway",
            _ => "resume anyway",
        };
        // Keys first: on a narrow terminal the explanation is what gets cut.
        let mut spans = vec![
            Span::styled(" OPEN ELSEWHERE ", warn),
            Span::styled(" y ", warn),
            Span::styled(format!("{}  ", verb), hint),
            Span::styled("any other key ", warn),
            Span::styled("cancel  ", hint),
        ];
        if let Some(s) = app.selected_session() {
            let state = match app.live_status(s) {
                Some(l) if l.busy => format!("working in Claude Code pid {}", l.pid),
                Some(l) => format!("open in Claude Code pid {}", l.pid),
                None => String::new(),
            };
            let used: usize = spans.iter().map(|s| s.content.width()).sum();
            let room = (area.width as usize).saturating_sub(used + state.width() + 4);
            spans.push(Span::styled(
                format!("\"{}\" is {}", truncate_width(s.display_name(), room.max(8)), state),
                hint,
            ));
        }
        let line = Line::from(spans);
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(Color::Yellow)),
            area,
        );
        return;
    }

    // Temporary status message
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
        vec![("↑↓ PgUp PgDn", "scroll"), ("any other key", "close help")]
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
                ("[ ]", "your prompts"),
                ("/", "find"),
                ("n N", "next/prev hit"),
                ("Enter", enter),
                ("T", "tools"),
                ("f", "files"),
                ("Tab", "list"),
                ("?", "help"),
            ],
            Focus::List if app.view_mode == ViewMode::Timeline => vec![
                ("←→ ↑↓", "pick a day"),
                ("Enter", "list its sessions"),
                ("t Esc", "back to list"),
                ("q", "quit"),
            ],
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

/// Compact metadata suffix for the preview title: whether it's open in a
/// running Claude Code, permission mode, skills, linked PRs. Returns "" when
/// there's nothing to show, otherwise " · plan · tdd · PR #12".
fn session_badges(s: &SessionMeta, live: Option<crate::live::LiveSession>) -> String {
    let mut parts: Vec<String> = Vec::new();
    match live {
        Some(l) if l.busy => parts.push("● working elsewhere".to_string()),
        Some(_) => parts.push("● open elsewhere".to_string()),
        None => {}
    }
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
fn draw_help(frame: &mut Frame, area: Rect, app: &mut App) {
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
                ("F", "resume as a fork (new session id)"),
                ("c", "copy the resume command"),
                ("p", "print session id and exit"),
                ("/", "search titles, files, tickets, full text"),
                ("⌘V  Ctrl+V", "search for the pasted text"),
                ("s", "sort: date · size · duration · messages"),
                ("a", "scope: this project ↔ all projects"),
                ("1-4  0", "size filter: quick…massive / clear"),
                ("b", "pin to top"),
                ("r", "rename (same as Claude Code's /rename)"),
                ("o", "open the linked pull request"),
                ("e", "export as markdown"),
                ("d  u", "delete / undo the last delete"),
                ("v", "compact / comfortable rows"),
                ("t", "timeline: arrows pick a day, Enter lists it"),
                ("●", "open in a running Claude Code (yellow: working)"),
            ],
        ),
        (
            "Preview",
            &[
                ("Tab", "switch between list and preview"),
                ("↑↓ PgUp PgDn", "scroll (also Ctrl+D / Ctrl+U)"),
                ("g G", "top / end of conversation"),
                ("[ ]", "previous / next message you wrote"),
                ("/  n N", "find in conversation / next, previous hit"),
                ("", "(a session search's words are marked already)"),
                ("Enter l F p", "resume / print, as in the list"),
                ("", "(Enter: next hit while a find is active)"),
                ("T", "show tool calls"),
                ("f", "show changed files"),
            ],
        ),
        (
            "Search filters (combine with words)",
            &[
                ("project:web", "project name contains \"web\""),
                ("branch:  file:", "branch / a changed file's path contains"),
                ("is:open", "open in a running Claude Code"),
                ("is:pinned  is:pr", "pinned / has a linked pull request"),
                ("after:7d", "active in the last 7 days (h d w m, or a date)"),
                ("before:2026-10-01", "last active before that day"),
                ("on:today", "active that day (today, yesterday, a date)"),
            ],
        ),
        (
            "Anywhere",
            &[
                ("Esc", "back: clear search → list → quit"),
                ("z", "zoom: one pane at a time, full width"),
                ("mouse", "with --mouse: wheel, click, double-click resumes"),
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
                Span::styled(format!("  {:<18}", k), key_style),
                Span::styled((*d).to_string(), desc_style),
            ]));
        }
    }

    let w = 72u16.min(area.width.saturating_sub(2));
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let popup = Rect::new(x, y, w, h);

    // Short terminals can't fit every key: scroll, and say there's more.
    let rows = h.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(rows);
    app.help_scroll = app.help_scroll.min(max_scroll);
    let more = match (app.help_scroll > 0, app.help_scroll < max_scroll) {
        (false, true) => " ↓ more ",
        (true, true) => " ↑↓ more ",
        (true, false) => " ↑ more ",
        (false, false) => "",
    };

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Keys — any other key closes ")
        .title(Line::from(more).right_aligned())
        .style(Style::default().bg(Color::Rgb(20, 20, 20)));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let visible: Vec<Line> = lines.into_iter().skip(app.help_scroll).collect();
    frame.render_widget(Paragraph::new(visible), inner);
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

/// Split text into spans, highlighting occurrences of any of `terms`
/// (lowercase; matched case-insensitively). Where two terms overlap, the
/// earlier one wins, then the longer.
fn highlight_spans(
    text: &str,
    terms: &[&str],
    base_style: Style,
    match_style: Style,
) -> Vec<Span<'static>> {
    let terms: Vec<&str> = terms.iter().copied().filter(|t| !t.is_empty()).collect();
    if terms.is_empty() {
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

    loop {
        let next = terms
            .iter()
            .filter_map(|t| text_lower[search_from..].find(t).map(|p| (search_from + p, t.len())))
            .min_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        let Some((start_lower, len)) = next else {
            break;
        };
        let end_lower = start_lower + len;
        let start_orig = lower_to_orig[start_lower];
        let end_orig = lower_to_orig[end_lower];
        if start_orig < last_end {
            search_from = end_lower;
            continue;
        }

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
    fn test_when_label() {
        use chrono::{Local, TimeZone};
        let at = |y, mo, d, h, mi| Local.with_ymd_and_hms(y, mo, d, h, mi, 0).single().unwrap();
        let now = at(2026, 11, 12, 15, 0); // a Thursday
        let ts = |y, mo, d, h, mi| at(y, mo, d, h, mi).timestamp();
        assert_eq!(when_label(ts(2026, 11, 12, 9, 5), now), "today 09:05 ");
        assert_eq!(when_label(ts(2026, 11, 11, 23, 59), now), "Wed 23:59   ");
        assert_eq!(when_label(ts(2026, 11, 6, 8, 0), now), "Fri 08:00   ");
        assert_eq!(when_label(ts(2026, 11, 5, 8, 0), now), "Nov  5 08:00", "a week back: the date");
        assert_eq!(when_label(ts(2025, 12, 24, 8, 0), now), "Dec 24  2025");
        assert!(when_label(ts(2026, 1, 2, 3, 4), now).width() == 12);
    }

    #[test]
    fn test_window_around_keeps_the_hit_visible() {
        let text = format!("{}needle{}", "a".repeat(100), "b".repeat(100));
        let w = window_around(&text, 100, 40);
        assert_eq!(w.width(), 40);
        assert!(w.starts_with('…') && w.ends_with('…'));
        assert!(w.contains("needle"));
        assert_eq!(window_around("short", 0, 40), "short");
        assert_eq!(window_around("needle at the start of a long line", 0, 12), "needle at t…");
    }

    #[test]
    fn test_highlight_spans_multiple_terms() {
        let base = Style::default();
        let hit = Style::default().fg(Color::Yellow);
        let spans = highlight_spans("Deploy Caddy, then deploy", &["caddy", "deploy"], base, hit);
        let marked: Vec<&str> = spans
            .iter()
            .filter(|s| s.style == hit)
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(marked, vec!["Deploy", "Caddy", "deploy"]);
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "Deploy Caddy, then deploy");
        // Overlapping terms: the longer one at the same spot wins.
        let spans = highlight_spans("caddyfile", &["caddy", "caddyfile"], base, hit);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "caddyfile");
    }

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn sessions(n: usize) -> App {
        let list = (0..n)
            .map(|i| SessionMeta {
                id: format!("id{}", i),
                project: "proj".into(),
                branch: "main".into(),
                name: format!("Session number {}", i),
                title: format!("first message {}", i),
                last_message: format!("last message {}", i),
                duration_secs: 600,
                timestamp: 1_700_000_000 - i as i64 * 60,
                file_size: 2048,
                file_mtime: 0,
                file_path: std::path::PathBuf::from(format!("/does/not/exist/{}.jsonl", i)),
                cwd: String::new(),
                message_count: 3,
                tickets: vec![],
                text_offset: 0,
                text_len: 0,
                name_lc: format!("session number {}", i),
                title_lc: format!("first message {}", i),
                project_lc: "proj".into(),
                branch_lc: "main".into(),
                permission_mode: String::new(),
                cc_version: String::new(),
                skills: vec![],
                changed_files: vec![],
                changed_files_lc: String::new(),
                prs: vec![],
                recap: String::new(),
            })
            .collect();
        let cache = crate::text_cache::TextCache::open(std::path::Path::new("/does/not/exist"));
        let mut app = App::new(list, false, Default::default(), cache);
        app.rebuild_view();
        app
    }

    #[test]
    fn test_compact_rows_fit_four_times_as_many_sessions() {
        let mut app = sessions(40);
        let screen = render(&mut app, 120, 30);
        let comfortable = screen.matches("Session number").count();
        app.density = Density::Compact;
        let screen = render(&mut app, 120, 30);
        let compact = screen.matches("Session number").count();
        assert!(compact >= comfortable * 3, "{} vs {}", compact, comfortable);
        assert!(!screen.contains("2.0 KB"), "a narrow list keeps the room for names");
        let screen = render(&mut app, 160, 30);
        assert!(screen.contains("2.0 KB"), "the size fits");
        assert!(!screen.contains("3 msgs"), "the count doesn't");
        app.zoom = true;
        let screen = render(&mut app, 120, 30);
        assert!(screen.contains("3 msgs") && screen.contains("2.0 KB"), "full width: both");
    }

    #[test]
    fn test_narrow_terminal_shows_one_pane_at_a_time() {
        let mut app = sessions(3);
        let screen = render(&mut app, 80, 24);
        assert!(screen.contains("Session number 0"));
        assert!(screen.contains("first message 0"), "the list gets the full width");
        assert!(!screen.contains("Loading conversation"), "no preview beside it");
        app.focus = Focus::Preview;
        let screen = render(&mut app, 80, 24);
        assert!(!screen.contains("left off"), "the preview replaces the list");
        let wide = render(&mut app, 140, 30);
        assert!(wide.contains("left off"), "wide terminals show both panes");
    }

    #[test]
    fn test_live_sessions_are_marked() {
        let mut app = sessions(2);
        app.live.insert("id0".into(), crate::live::LiveSession { pid: 1, busy: false });
        let screen = render(&mut app, 140, 20);
        let row = screen.lines().find(|l| l.contains("▸")).unwrap();
        assert!(row.contains("▸ ●"), "got {:?}", row);
        assert!(screen.contains("● open elsewhere"));
    }

    #[test]
    fn test_prompts_show_even_while_a_status_message_is_up() {
        let mut app = sessions(1);
        app.set_status("Pinned".to_string());
        app.confirm_resume = Some(crate::app::AppAction::Launch(0));
        let screen = render(&mut app, 140, 20);
        assert!(screen.contains("OPEN ELSEWHERE"), "{}", screen.lines().last().unwrap());
        app.confirm_resume = None;
        app.confirm_delete = true;
        let screen = render(&mut app, 140, 20);
        assert!(screen.lines().last().unwrap().contains("DELETE"));
    }

    #[test]
    fn test_timeline_picks_a_day_and_lists_it() {
        use chrono::TimeZone;
        let mut app = sessions(3);
        let today = chrono::Local::now().date_naive();
        let at = |d: chrono::NaiveDate, h| {
            chrono::Local
                .from_local_datetime(&d.and_hms_opt(h, 0, 0).unwrap())
                .earliest()
                .unwrap()
                .timestamp()
        };
        let yesterday = today.pred_opt().unwrap();
        app.sessions[0].timestamp = at(today, 9);
        app.sessions[1].timestamp = at(yesterday, 9);
        app.sessions[2].timestamp = at(yesterday, 15);
        app.rebuild_view();
        app.view_mode = ViewMode::Timeline;

        let screen = render(&mut app, 140, 30);
        assert!(screen.contains(&today.format("%a %b %e %Y").to_string()));
        assert!(screen.contains("1 session"), "today's count");
        app.timeline_move(7);
        assert_eq!(app.timeline_selected(), today, "never past today");
        app.timeline_move(-1);
        let screen = render(&mut app, 140, 30);
        assert!(screen.contains("2 sessions  Enter lists them"));
        assert!(app.timeline_cells.iter().any(|&(_, _, d)| d == yesterday));

        app.pick_timeline_day();
        assert_eq!(app.view_mode, ViewMode::Normal);
        assert_eq!(app.search_query.text(), format!("on:{}", yesterday.format("%Y-%m-%d")));
        let shown: Vec<&str> = app
            .filtered_indices
            .iter()
            .map(|&i| app.sessions[i].id.as_str())
            .collect();
        assert_eq!(shown, vec!["id2", "id1"], "that day only, newest first");

        app.timeline_move(-10_000);
        assert_eq!(app.timeline_selected(), app.timeline_start.unwrap(), "clamped to the grid");
    }

    #[test]
    fn test_day_rules_render_as_one_row_and_are_never_matched() {
        let mut app = sessions(1);
        app.focus = Focus::Preview;
        app.preview_lines = vec![
            ("Mon Nov 9, 2026".into(), "mon nov 9, 2026".into(), Speaker::Day),
            ("first".into(), "first".into(), Speaker::User),
            ("Wed Nov 11, 2026".into(), "wed nov 11, 2026".into(), Speaker::Day),
            ("nov plans".into(), "nov plans".into(), Speaker::User),
        ];
        app.preview_session_id = "id0".into();
        let screen = render(&mut app, 60, 20);
        let rule = screen.lines().find(|l| l.contains("Mon Nov 9, 2026")).unwrap();
        assert!(rule.contains("── Mon Nov 9, 2026 ────"), "{:?}", rule);
        assert_eq!(app.preview_line_offsets, vec![0, 2, 4, 6]);
        app.search_query = "nov".into();
        app.refresh_preview_matches(false);
        assert_eq!(app.preview_search_matches, vec![3], "only the message, not the rules");
    }

    #[test]
    fn test_help_overlay_scrolls_in_a_short_terminal() {
        let mut app = sessions(1);
        app.show_help = true;
        let top = render(&mut app, 80, 24);
        assert!(top.contains("↓ more"));
        assert!(!top.contains("q  Ctrl+C"));
        app.help_scroll = 999;
        let bottom = render(&mut app, 80, 24);
        assert!(bottom.contains("q  Ctrl+C"), "scrolls to the last key");
        assert!(bottom.contains("↑ more"));
        assert!(app.help_scroll < 999, "clamped to the content");
    }

    #[test]
    fn test_highlight_unicode_length_change() {
        // Turkish dotted-I (\u{0130}) lowercases to "i\u{0307}" (two code points),
        // which makes text_lower.len() differ from text.len().
        let text = "İstanbul project";
        let base = Style::default();
        let mat = Style::default();
        let spans = highlight_spans(text, &["istanbul"], base, mat);
        // Should produce at least 2 spans (matched + remainder), not bail out.
        assert!(spans.len() >= 2, "got spans: {}", spans.len());
    }
}
