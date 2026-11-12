use crate::app::{App, Density, Focus, Scope, Snippet, SnippetPart, ViewMode};
use crate::input::TextInput;
use crate::parser::Speaker;
use crate::session::{format_duration, format_file_size, size_category, SessionMeta};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use unicode_segmentation::UnicodeSegmentation;
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
    // The last session on the page needs no gap row after it.
    let gap = usize::from(app.density.rows() > 1);
    let per_page = ((usable + gap) / app.density.rows()).max(1);
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
                let label = match &snippet.subagent {
                    // Found in what a subagent did, not the conversation.
                    Some((task, _)) if !task.is_empty() => {
                        let room = (max_width / 4).clamp(8, 24);
                        format!("└ subagent \"{}\": ", truncate_width(task, room))
                    }
                    Some(_) => "└ subagent: ".to_string(),
                    None if snippet.in_file => "└ file: ".to_string(),
                    None => "└ match: ".to_string(),
                };
                let room = max_width.saturating_sub(INDENT.width() + label.width());
                let body = snippet_body(&snippet.parts, room);
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
        .take(height)
        .enumerate()
        .filter_map(|(i, visual)| Some((inner.y + i as u16, *app.filtered_indices.get(visual)?)))
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

/// The match line's text in `room` columns: each part windowed around its
/// hit, short parts whole and the rest sharing what's left.
fn snippet_body(parts: &[SnippetPart], room: usize) -> String {
    const SEP: &str = " · ";
    let n = parts.len();
    if n == 0 {
        return String::new();
    }
    let mut left = room.saturating_sub(SEP.width() * (n - 1));
    let natural: Vec<usize> = parts.iter().map(|p| p.text.trim_end().width()).collect();
    let mut widths = vec![0; n];
    let mut open: Vec<usize> = (0..n).collect();
    while !open.is_empty() {
        let share = left / open.len();
        let (fit, rest): (Vec<usize>, Vec<usize>) =
            open.iter().partition(|&&i| natural[i] <= share);
        if fit.is_empty() {
            for &i in &rest {
                widths[i] = share;
            }
            break;
        }
        for &i in &fit {
            widths[i] = natural[i];
            left -= natural[i];
        }
        open = rest;
    }
    parts
        .iter()
        .zip(widths)
        .map(|(p, w)| window_around(&p.text, p.hit, p.span, w))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(SEP)
}

/// `width` columns of `text` around the `span` chars at `hit`: a little
/// context before them, `…` where the text was cut, and cuts at word
/// boundaries where that keeps the hit in view.
fn window_around(text: &str, hit: usize, span: usize, width: usize) -> String {
    /// Most chars dropped to cut at a word boundary instead of mid-word.
    const WORD_SLACK: usize = 15;
    let text = text.trim_end();
    if text.width() <= width {
        return text.to_string();
    }
    if width < 2 {
        return String::new();
    }
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let hit = hit.min(len);
    let span_end = (hit + span).min(len);
    let cw = |c: char| c.width().unwrap_or(0);
    let span_width: usize = chars[hit..span_end].iter().map(|&c| cw(c)).sum();
    // Up to a quarter of the room shows what came before the hit; less when
    // the words found with it need the room.
    let lead_room = (width / 4).min(width.saturating_sub(span_width + 2));
    let mut start = hit;
    let mut lead = 0;
    while start > 0 && lead + cw(chars[start - 1]) <= lead_room {
        start -= 1;
        lead += cw(chars[start]);
    }
    if start > 0 && !chars[start - 1].is_whitespace() {
        if let Some(i) = chars[start..hit].iter().position(|c| c.is_whitespace()) {
            start += i + 1;
        }
    }
    // Room for the text between the ellipses.
    let budget = width - usize::from(start > 0);
    let mut end = start;
    let mut used = 0;
    while end < len && used + cw(chars[end]) <= budget {
        used += cw(chars[end]);
        end += 1;
    }
    if end < len {
        while used + 1 > budget && end > start {
            end -= 1;
            used -= cw(chars[end]);
        }
        let keep = span_end.clamp(start, end);
        if !chars[end].is_whitespace() {
            if let Some(i) = chars[keep..end].iter().rposition(|c| c.is_whitespace()) {
                if end - (keep + i) <= WORD_SLACK {
                    end = keep + i;
                }
            }
        }
    }
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < len {
        out.truncate(out.trim_end().len());
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
    /// A branch cut narrower than this is dropped instead.
    const MIN_BRANCH: usize = 6;
    let total = |p: &str, b: &str, n: &str| {
        p.width()
            + if b.is_empty() { 0 } else { 2 + b.width() }
            + if n.is_empty() { 0 } else { 2 + n.width() }
    };
    let (mut p, mut b, mut n) = (project.to_string(), branch.to_string(), name.to_string());
    if total(&p, &b, &n) <= avail {
        return (p, b, n);
    }
    // The name says the most, so it keeps up to two thirds of the row (all
    // of it when shorter); project and branch share the rest, branch first
    // to give way.
    let name_keeps = if n.is_empty() { 0 } else { 2 + n.width().min((avail * 2 / 3).max(10)) };
    let room = avail.saturating_sub(name_keeps);
    b = truncate_width(&b, 20);
    p = truncate_width(&p, 20);
    if total(&p, &b, "") > room {
        let branch_room = room.saturating_sub(p.width() + 2);
        b = if branch_room >= MIN_BRANCH { truncate_width(&b, branch_room) } else { String::new() };
    }
    if total(&p, &b, "") > room {
        p = truncate_width(&p, room);
    }
    if !n.is_empty() {
        let name_room = avail.saturating_sub(total(&p, &b, "") + 2);
        n = truncate_width(&n, name_room);
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

    // Count sessions per date, honoring the scope and size filter so the
    // heatmap matches the list Enter opens.
    let mut counts: std::collections::HashMap<NaiveDate, u32> = std::collections::HashMap::new();
    let sized = |s: &SessionMeta| app.size_filter.is_none_or(|c| size_category(s.file_size) == c);
    for s in app.sessions.iter().filter(|s| app.in_scope(s) && sized(s)) {
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
            match picked {
                0 => "",
                1 => "  Enter lists it",
                _ => "  Enter lists them",
            },
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
        Span::raw(match total_sessions {
            1 => "1 session".to_string(),
            n => format!("{} sessions", n),
        }),
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
    Some(if snippet.subagent.is_some() {
        " hits in a subagent's work "
    } else if snippet.in_file {
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
        app.rewrap_preview();
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
            message_rows_marked(text, width, highlight_terms)
        };
        for (row_idx, (row, marks)) in rows.into_iter().enumerate() {
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
            spans.extend(spans_for_hits(&row, &marks, base_style, match_style));
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

fn draw_status_bar(frame: &mut Frame, app: &mut App, area: Rect) {
    let bar_bg = Color::Rgb(40, 40, 40);

    // A pending prompt outranks everything: it's waiting for the next key,
    // and only counts a `y` once it's been seen.
    if app.confirm_delete || app.confirm_resume.is_some() {
        app.prompt_seen = true;
    }
    let app: &App = app;
    if app.confirm_delete {
        let warn = Style::default()
            .fg(Color::White)
            .bg(Color::Red)
            .add_modifier(Modifier::BOLD);
        let hint = Style::default()
            .fg(Color::Rgb(200, 200, 200))
            .bg(Color::Red);
        let open = app.selected_session().and_then(|s| app.live_status(s)).is_some();
        // Keys first: on a narrow terminal the name is what gets cut.
        let mut spans = vec![
            Span::styled(" DELETE ", warn),
            Span::styled(" y ", warn),
            Span::styled("confirm  ", hint),
            Span::styled("any other key ", warn),
            Span::styled("cancel  ", hint),
        ];
        if open {
            spans.push(Span::styled("open in Claude Code right now  ", warn));
        }
        if let Some(s) = app.selected_session() {
            let used: usize = spans.iter().map(|s| s.content.width()).sum();
            let room = (area.width as usize).saturating_sub(used + 3);
            spans.push(Span::styled(
                format!("\"{}\"", truncate_width(s.display_name(), room.max(8))),
                hint,
            ));
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
            crate::app::AppAction::Print(_) => "pick anyway",
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
        if let Some(s) = app.action_target(action) {
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
    for g in s.graphemes(true) {
        let w = g.width();
        if used + w > max_cols - 1 {
            break;
        }
        out.push_str(g);
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
    message_rows_joined(text, width).into_iter().map(|(row, _)| row).collect()
}

/// `message_rows`, with how each row joins the one before: `None` where a
/// line of the message starts, else whether a space was between them.
fn message_rows_joined(text: &str, width: usize) -> Vec<(String, Option<bool>)> {
    let mut rows = Vec::new();
    let mut prev_blank = true; // also drops leading blank lines
    for line in text.split('\n') {
        let line = crate::parser::sanitize_line(line);
        let line = line.trim_end();
        if line.is_empty() {
            if !prev_blank {
                rows.push((String::new(), None));
            }
            prev_blank = true;
            continue;
        }
        prev_blank = false;
        for (i, (row, after_space)) in wrap_text_joined(line, width).into_iter().enumerate() {
            rows.push((row, (i > 0).then_some(after_space)));
        }
    }
    while rows.len() > 1 && rows.last().is_some_and(|(r, _)| r.is_empty()) {
        rows.pop();
    }
    if rows.is_empty() {
        rows.push((String::new(), None));
    }
    rows
}

/// A message's rows, each with the byte ranges in it where the terms
/// occur, found across row breaks: a phrase wrapped onto the next row is
/// marked on both.
pub fn message_rows_marked(text: &str, width: usize, terms: &[&str]) -> Vec<(String, Vec<(usize, usize)>)> {
    let rows = message_rows_joined(text, width);
    let mut out = Vec::with_capacity(rows.len());
    let mut i = 0;
    while i < rows.len() {
        // The rows of one line of the message, rejoined.
        let mut j = i + 1;
        while j < rows.len() && rows[j].1.is_some() {
            j += 1;
        }
        let mut joined = String::new();
        let mut starts = Vec::with_capacity(j - i);
        for (k, (row, join)) in rows[i..j].iter().enumerate() {
            if k > 0 && *join == Some(true) {
                joined.push(' ');
            }
            starts.push(joined.len());
            joined.push_str(row);
        }
        let hits = find_hits(&joined, terms);
        for (k, (row, _)) in rows[i..j].iter().enumerate() {
            let (from, to) = (starts[k], starts[k] + row.len());
            let marks = hits
                .iter()
                .filter_map(|&(a, b)| {
                    let (a, b) = (a.max(from), b.min(to));
                    (a < b).then(|| (a - from, b - from))
                })
                .collect();
            out.push((row.clone(), marks));
        }
        i = j;
    }
    out
}

/// Word-wrap one line to `width` display columns. Breaks at the last space
/// that fits (after any indentation), or mid-word when a word is too long.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    wrap_text_joined(text, width).into_iter().map(|(row, _)| row).collect()
}

/// `wrap_text`, with whether each row went on from the previous one after
/// a space (`false` for the first row, and where a word was cut).
fn wrap_text_joined(text: &str, width: usize) -> Vec<(String, bool)> {
    if width == 0 {
        return vec![(text.to_string(), false)];
    }
    let mut result = Vec::new();
    let mut remaining = text;
    let mut after_space = false;
    while !remaining.is_empty() {
        let mut used = 0;
        let mut cut = None;
        let mut last_space = None;
        let mut seen_word = false;
        // By grapheme, as the terminal draws them: "⚠\u{FE0F}" is two
        // columns though its chars add up to one.
        for (i, g) in remaining.grapheme_indices(true) {
            let w = g.width();
            if used + w > width {
                cut = Some(i);
                break;
            }
            if g == " " {
                if seen_word {
                    last_space = Some(i);
                }
            } else {
                seen_word = true;
            }
            used += w;
        }
        let Some(cut) = cut else {
            result.push((remaining.to_string(), after_space));
            break;
        };
        let split = match last_space {
            Some(s) if s > 0 => s,
            // A single character wider than the row still has to go somewhere.
            _ if cut == 0 => remaining.graphemes(true).next().map_or(remaining.len(), str::len),
            _ => cut,
        };
        let row = remaining[..split].trim_end();
        let rest = &remaining[split..];
        result.push((row.to_string(), after_space));
        after_space = row.len() < split || rest.starts_with(' ');
        remaining = rest.trim_start_matches(' ');
    }
    result
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
    spans_for_hits(text, &find_hits(text, terms), base_style, match_style)
}

/// Byte ranges of `text` where the terms (lowercase) occur, ignoring case;
/// the earliest first, the longest where two start together, none
/// overlapping.
fn find_hits(text: &str, terms: &[&str]) -> Vec<(usize, usize)> {
    let terms: Vec<&str> = terms.iter().copied().filter(|t| !t.is_empty()).collect();
    if terms.is_empty() {
        return Vec::new();
    }

    // Folded like the terms (case and accents), with the byte in `text`
    // each folded byte came from.
    let (text_lower, lower_to_orig) = crate::fold::fold_with_origin(text);

    let mut hits = Vec::new();
    let mut last_end = 0;
    let mut search_from = 0;
    while search_from < text_lower.len() {
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
        search_from = end_lower;
        if start_orig < last_end || start_orig == end_orig {
            continue;
        }
        hits.push((start_orig, end_orig));
        last_end = end_orig;
    }
    hits
}

/// `text` as spans, the byte ranges `hits` (sorted, not overlapping) in
/// `match_style`.
fn spans_for_hits(
    text: &str,
    hits: &[(usize, usize)],
    base_style: Style,
    match_style: Style,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut last_end = 0;
    for &(start, end) in hits {
        if start > last_end {
            spans.push(Span::styled(text[last_end..start].to_string(), base_style));
        }
        spans.push(Span::styled(text[start..end].to_string(), match_style));
        last_end = end;
    }
    if last_end < text.len() || spans.is_empty() {
        spans.push(Span::styled(text[last_end..].to_string(), base_style));
    }
    spans
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
        // Emoji drawn two columns wide by a variation selector.
        let warned: String = (0..40).map(|i| format!("\u{26a0}\u{fe0f}w{:02} ", i)).collect();
        let rows = wrap_text(&warned, 20);
        assert!(rows.iter().all(|r| r.width() <= 20), "got {:?}", rows);
        assert!(rows.concat().contains("w39"));
        assert!(truncate_width(&warned, 9).width() <= 9);
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
    fn test_fit_line1_gives_the_name_room_next_to_a_long_project_and_branch() {
        let total = |p: &str, b: &str, n: &str| {
            p.width() + if b.is_empty() { 0 } else { 2 + b.width() } + 2 + n.width()
        };
        // The two-pane list at 100–130 columns has about 40–55 columns here.
        for avail in [30, 40, 47, 55] {
            let (p, b, n) =
                fit_line1("acme-platform-api", "feature/PROJ-1234-login", "Fix login redirect", avail);
            assert!(n.starts_with("Fix login"), "{}: {:?}", avail, (p, b, n));
            assert!(total(&p, &b, &n) <= avail, "{}: {:?} overflows", avail, (p, b, n));
        }
        let (_, b, n) = fit_line1("acme-platform-api", "feature/PROJ-1234-login", "Fix login redirect", 30);
        assert!(b.is_empty(), "a branch with no room left is dropped, not cut to a stub");
        assert_eq!(n, "Fix login redirect");
        // Everything fits: untouched.
        assert_eq!(
            fit_line1("p", "main", "name", 80),
            ("p".into(), "main".into(), "name".into())
        );
    }

    #[test]
    fn test_a_phrase_wrapped_onto_the_next_row_is_marked_on_both() {
        // Width 12: "then deploy" fills row 1, "caddy now" is row 2.
        let rows = message_rows_marked("then deploy caddy now", 12, &["deploy caddy"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "then deploy");
        assert_eq!(&rows[0].0[rows[0].1[0].0..rows[0].1[0].1], "deploy");
        assert_eq!(&rows[1].0[rows[1].1[0].0..rows[1].1[0].1], "caddy");
        // Cut mid-word (no spaces, as in CJK): the halves join without one.
        let rows = message_rows_marked("日本語のテキスト", 8, &["語のテ"]);
        let marked: String = rows
            .iter()
            .flat_map(|(row, marks)| marks.iter().map(move |&(a, b)| row[a..b].to_string()))
            .collect();
        assert_eq!(marked, "語のテ");
        // Separate lines of a message never join.
        let rows = message_rows_marked("deploy\ncaddy", 40, &["deploy caddy"]);
        assert!(rows.iter().all(|(_, marks)| marks.is_empty()));
    }

    #[test]
    fn test_highlight_marks_accented_words_from_a_plain_query() {
        let base = Style::default();
        let hit = Style::default().fg(Color::Yellow);
        let spans = highlight_spans("Στην Οδό, ΟΔΟΣ και café", &["οδο", "cafe"], base, hit);
        let marked: Vec<&str> = spans.iter().filter(|s| s.style == hit).map(|s| s.content.as_ref()).collect();
        assert_eq!(marked, ["Οδό", "ΟΔΟ", "café"]);
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
        let w = window_around(&text, 100, 6, 40);
        assert_eq!(w.width(), 40);
        assert!(w.starts_with('…') && w.ends_with('…'));
        assert!(w.contains("needle"));
        assert_eq!(window_around("short", 0, 5, 40), "short");
        assert_eq!(
            window_around("needle at the start of a long line", 0, 6, 12),
            "needle at…",
            "cut between words"
        );
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

    fn part(text: &str, hit: &str) -> SnippetPart {
        let at = text.find(hit).unwrap();
        SnippetPart {
            text: text.into(),
            hit: text[..at].chars().count(),
            span: hit.chars().count(),
            source: crate::as_written::Piece { text: text.into(), hit: (at, at + hit.len()) },
            at: 0.0,
            as_written: false,
        }
    }

    #[test]
    fn test_snippet_body_shares_the_room_between_parts() {
        let short = part("tokio runtime", "tokio");
        let long = part(&format!("{} we tag the release here {}", "x ".repeat(40), "y ".repeat(40)), "release");
        let body = snippet_body(&[short.clone(), long.clone()], 50);
        assert!(body.starts_with("tokio runtime · …"), "{}", body);
        assert!(body.contains("release"), "{}", body);
        assert!(body.width() <= 50, "{}", body.width());
        // Too narrow for both in full: each keeps its hit.
        let body = snippet_body(&[long.clone(), long], 40);
        assert_eq!(body.matches("release").count(), 2, "{}", body);
        assert!(body.width() <= 40);
        assert_eq!(snippet_body(&[], 40), "");
    }

    #[test]
    fn test_window_around_keeps_the_words_found_together_in_view() {
        let text = format!("{}crates.io releases and more text after it", "lead ".repeat(30));
        let hit = text.find("crates").unwrap();
        let span = "crates.io releases".len();
        let w = window_around(&text, hit, span, 24);
        assert!(w.contains("crates.io releases"), "{}", w);
        assert!(w.width() <= 24);
    }

    #[test]
    fn test_search_row_shows_where_each_word_matched() {
        let filler = "x".repeat(300);
        let text = format!("tokio runtime here\n{}\nwe tag the release\n", filler);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("text.bin");
        std::fs::write(&path, &text).unwrap();
        let mut app = sessions(1);
        app.text_cache = crate::text_cache::TextCache::open(&path);
        app.sessions[0].text_len = text.len() as u32;
        app.search_query = "release tokio".into();
        app.apply_search();
        app.zoom = true;
        let screen = render(&mut app, 100, 12);
        assert!(
            screen.contains("└ match: tokio runtime here · we tag the release"),
            "{}",
            screen
        );
        assert!(!screen.contains("left off"), "the match line takes its place");
    }

    #[test]
    fn test_a_hit_in_a_subagent_says_so() {
        let text = "the session itself\n\u{1}check the locks\nfound a flock race\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("text.bin");
        std::fs::write(&path, text).unwrap();
        let mut app = sessions(1);
        app.text_cache = crate::text_cache::TextCache::open(&path);
        app.sessions[0].text_len = text.len() as u32;
        app.search_query = "flock".into();
        app.apply_search();
        app.zoom = true;
        let screen = render(&mut app, 100, 12);
        // No meta file to read the description as written: the folded one.
        assert!(
            screen.contains("└ subagent \"check the locks\": found a flock race"),
            "{}",
            screen
        );
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

        // Picking again keeps the words and other filters, swaps the date.
        app.search_query = format!("deploy is:pinned after:7d on:{}", yesterday).as_str().into();
        app.view_mode = ViewMode::Timeline;
        app.timeline_day = Some(today);
        app.pick_timeline_day();
        assert_eq!(
            app.search_query.text(),
            format!("deploy is:pinned on:{}", today.format("%Y-%m-%d"))
        );

        // The count follows the size filter, as the list does.
        app.search_query.clear();
        app.apply_search();
        app.view_mode = ViewMode::Timeline;
        app.timeline_day = Some(yesterday);
        app.sessions[1].file_size = 50 << 20;
        app.size_filter = Some("massive");
        let screen = render(&mut app, 140, 30);
        assert!(screen.contains("1 session  Enter lists it"), "{}", screen);
        app.size_filter = None;

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
        ].into();
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
