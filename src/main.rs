use sessy::{app, bookmarks, config, index, preview, session, text_cache, ui};
use app::{App, AppAction, Focus, Scope, ViewMode};
use clap::Parser;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "sessy", version, about = "TUI session manager for Claude Code")]
struct Cli {
    /// Only sessions whose project name contains this (case-insensitive);
    /// searches across all projects
    #[arg(long)]
    project: Option<String>,

    /// Print the selected session ID to stdout instead of resuming it
    #[arg(long)]
    print: bool,

    /// Only show sessions from a recent time window (e.g. 1h, 7d, 2w, 1m)
    #[arg(long)]
    recent: Option<String>,

    /// Show sessions from all projects (default: the current project)
    #[arg(long, short)]
    all: bool,

    /// Force full re-index, ignoring cache
    #[arg(long)]
    rebuild_index: bool,

    /// Delete all sessions smaller than 15 KB and older than 2 days
    #[arg(long)]
    purge: bool,
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();

    // Validate flags before the (potentially slow) index build.
    let recent_secs = match cli.recent.as_deref() {
        Some(recent) => match index::parse_recent_filter(recent) {
            Some(secs) => Some(secs),
            None => {
                eprintln!(
                    "sessy: invalid --recent value '{}' (expected a number followed by h, d, w, or m — e.g. 1h, 7d, 2w, 1m)",
                    recent
                );
                std::process::exit(2);
            }
        },
        None => None,
    };

    // Build index
    let cached = if cli.rebuild_index {
        None
    } else {
        index::load_cached_index()
    };

    let mut idx = index::build_index(cached, cli.rebuild_index);
    idx.sessions.sort_by_key(|s| std::cmp::Reverse(s.timestamp));

    // Save index before applying runtime filters
    index::save_index(&idx);

    // Apply filters
    if let Some(ref project_filter) = cli.project {
        let filter_lower = project_filter.to_lowercase();
        idx.sessions
            .retain(|s| s.project.to_lowercase().contains(&filter_lower));
    }

    if let Some(secs) = recent_secs {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let cutoff = now.saturating_sub(secs as i64);
        idx.sessions.retain(|s| s.timestamp >= cutoff);
    }

    // Load bookmarks
    let bookmarks = bookmarks::load_bookmarks();

    // Purge: delete tiny old sessions. Runs after the CLI filters so
    // `--project X --purge` only touches that project's sessions.
    if cli.purge {
        return run_purge(&idx, &bookmarks);
    }

    // Run TUI
    let (cfg, cfg_warning) = config::load();
    let tc = text_cache::TextCache::open(&text_cache::text_cache_path());
    let mut app = App::new(idx.sessions, cli.print, bookmarks, tc);
    app.bookmarks_file = Some(bookmarks::bookmarks_path());
    app.scope_root = std::env::current_dir().ok().map(|cwd| project_root(&cwd));
    // `--project` names the project explicitly; restricting it further to
    // the launch directory would usually leave nothing.
    app.scope = if cli.all || cli.project.is_some() || cfg.scope_is_all() {
        Scope::All
    } else {
        Scope::Current
    };
    app.sort_mode = cfg.sort_mode();
    app.show_tools = cfg.show_tool_activity;
    app.enter_yolo = cfg.enter_is_yolo();
    app.rebuild_view(); // apply scope filter + bookmark floating on initial load
    if let Some(warning) = cfg_warning {
        app.set_status(warning);
    }

    // In --print mode stdout is typically captured by a command substitution
    // (`claude --resume $(sessy --print)`), so the TUI must render on stderr,
    // keeping stdout clean for the selected session ID.
    let result = if cli.print {
        run_tui_on_stderr(&mut app)
    } else {
        let mut terminal = ratatui::init();
        // Kitty keyboard protocol: without it, terminals send legacy codes in
        // which Cmd/Alt+Backspace are indistinguishable from plain Backspace
        // (or never delivered), so the modifier-aware search-input bindings
        // can't fire. Push after entering the alternate screen (the flag stack
        // is per-screen), pop before leaving it.
        let kbd_enhanced = push_keyboard_enhancement(&mut io::stdout());
        let _ = crossterm::execute!(io::stdout(), EnableBracketedPaste);
        let result = run_event_loop(&mut terminal, &mut app);
        let _ = crossterm::execute!(io::stdout(), DisableBracketedPaste);
        pop_keyboard_enhancement(&mut io::stdout(), kbd_enhanced);
        ratatui::restore();
        result
    };
    result?;

    // Handle post-TUI actions
    let code = handle_post_tui_action(&app);
    std::process::exit(code);
}

/// The directory `Scope::Current` covers: the enclosing git checkout, so
/// launching from `repo/src` still finds sessions started at `repo/`.
/// Outside a repo (or when the nearest `.git` is the home directory itself,
/// e.g. a dotfiles repo) it's the launch directory.
fn project_root(cwd: &Path) -> PathBuf {
    let home = dirs::home_dir();
    for dir in cwd.ancestors() {
        if home.as_deref() == Some(dir) {
            break;
        }
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
    }
    cwd.to_path_buf()
}

/// Set up and tear down a terminal on stderr (mirror of `ratatui::init()`/
/// `restore()`, which are hardwired to stdout).
fn run_tui_on_stderr(app: &mut App) -> io::Result<()> {
    use crossterm::cursor::Show;
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };

    enable_raw_mode()?;
    if let Err(e) = crossterm::execute!(io::stderr(), EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(e);
    }
    let kbd_enhanced = push_keyboard_enhancement(&mut io::stderr());
    let _ = crossterm::execute!(io::stderr(), EnableBracketedPaste);
    let result = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(io::stderr()))
        .and_then(|mut terminal| run_event_loop(&mut terminal, app));
    let _ = crossterm::execute!(io::stderr(), DisableBracketedPaste);
    pop_keyboard_enhancement(&mut io::stderr(), kbd_enhanced);
    let _ = crossterm::execute!(io::stderr(), LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
    result
}

/// Enable the kitty keyboard protocol when the terminal supports it, so
/// modifier combinations like Cmd+Backspace and Alt+Backspace reach the app.
/// Returns whether the flags were pushed.
///
/// Skipped when stdout isn't a terminal: crossterm's support probe opens
/// /dev/tty read-only, fails to write its query there, and falls back to
/// stdout — which under `$(sessy --print)` would prepend `ESC[?u ESC[c` to
/// the captured session ID.
fn push_keyboard_enhancement<W: io::Write>(out: &mut W) -> bool {
    use std::io::IsTerminal;
    if io::stdout().is_terminal()
        && crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false)
    {
        crossterm::execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok()
    } else {
        false
    }
}

fn pop_keyboard_enhancement<W: io::Write>(out: &mut W, pushed: bool) {
    if pushed {
        let _ = crossterm::execute!(out, PopKeyboardEnhancementFlags);
    }
}

fn run_purge(
    idx: &index::SessionIndex,
    bookmarks: &std::collections::HashSet<String>,
) -> io::Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let two_days_ago = now - 2 * 86400;
    let size_limit = 15 * 1024;

    // Pinned sessions are kept no matter how small: pinning is an explicit
    // "keep this".
    let to_purge: Vec<&session::SessionMeta> = idx
        .sessions
        .iter()
        .filter(|s| {
            s.file_size < size_limit && s.timestamp < two_days_ago && !bookmarks.contains(&s.id)
        })
        .collect();

    if to_purge.is_empty() {
        println!("Nothing to purge.");
        return Ok(());
    }

    println!(
        "Found {} sessions < 15 KB and older than 2 days. Delete all? [y/N]",
        to_purge.len()
    );
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if answer.trim().eq_ignore_ascii_case("y") {
        let mut deleted = 0;
        for s in &to_purge {
            if std::fs::remove_file(&s.file_path).is_ok() {
                let companion = s.file_path.with_extension("");
                if companion.is_dir() {
                    std::fs::remove_dir_all(&companion).ok();
                }
                deleted += 1;
            }
        }
        // No index save needed: the next launch simply won't find the deleted
        // files on disk, and stale cache entries keyed by missing paths are
        // ignored by the incremental rebuild.
        println!("Purged {} sessions.", deleted);
    } else {
        println!("Aborted.");
    }
    Ok(())
}

/// Run the action chosen in the TUI. Returns the process exit code: claude's
/// own when resuming, 1 when `--print` ends without a selection (so
/// `id=$(sessy --print) && claude --resume "$id"` stops cleanly).
fn handle_post_tui_action(app: &App) -> i32 {
    let resolve = |idx: usize| -> Option<&session::SessionMeta> {
        app.filtered_indices
            .get(idx)
            .and_then(|&real| app.sessions.get(real))
    };

    match app.action {
        AppAction::Launch(idx) | AppAction::LaunchDangerously(idx) => {
            let Some(session) = resolve(idx) else {
                return 1;
            };
            if !session.cwd.is_empty() {
                let cwd_path = Path::new(&session.cwd);
                if cwd_path.is_dir() {
                    std::env::set_current_dir(cwd_path).ok();
                } else {
                    eprintln!(
                        "sessy: {} no longer exists; resuming from the current directory",
                        session.cwd
                    );
                }
            }
            let mut cmd = std::process::Command::new("claude");
            cmd.arg("--resume").arg(&session.id);
            if matches!(app.action, AppAction::LaunchDangerously(_)) {
                cmd.arg("--dangerously-skip-permissions");
            }
            match cmd.status() {
                Ok(status) => status.code().unwrap_or(1),
                Err(e) => {
                    eprintln!(
                        "sessy: couldn't run `claude` ({}). Is Claude Code installed and on your PATH?",
                        e
                    );
                    127
                }
            }
        }
        AppAction::Print(idx) => match resolve(idx) {
            Some(session) => {
                println!("{}", session.id);
                0
            }
            None => 1,
        },
        _ if app.print_mode => 1,
        _ => 0,
    }
}

fn run_event_loop<B: ratatui::backend::Backend<Error = io::Error>>(
    terminal: &mut ratatui::Terminal<B>,
    app: &mut App,
) -> io::Result<()> {
    preview::request_preview(app);

    let mut dirty = true;
    let mut last_draw = Instant::now();
    loop {
        // Redraw on change, plus a slow tick so the loading indicator and
        // status-message expiry still update when idle.
        if dirty || last_draw.elapsed() >= Duration::from_millis(250) {
            terminal.draw(|frame| ui::draw(frame, app))?;
            last_draw = Instant::now();
            dirty = false;
        }

        if event::poll(Duration::from_millis(50))? {
            // Drain everything already queued (fast typing, key repeat) before
            // searching and redrawing once.
            loop {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => handle_key(app, key),
                    Event::Paste(text) => handle_paste(app, &text),
                    _ => {}
                }
                dirty = true;
                if app.action != AppAction::None || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
            if app.search_dirty {
                app.apply_search();
                preview::request_preview(app);
            }
        }

        if preview::check_preview_updates(app) {
            dirty = true;
        }

        if app.action != AppAction::None {
            break;
        }
    }

    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent) {
    // Ctrl+C quits from anywhere, as in every other terminal program.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.action = AppAction::Quit;
        return;
    }

    // Delete confirmation: only an explicit `y` deletes.
    if app.confirm_delete {
        app.confirm_delete = false;
        if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
            app.delete_selected();
            preview::request_preview(app);
        }
        return;
    }

    // The help overlay captures the next keypress to dismiss itself.
    if app.show_help {
        app.show_help = false;
        return;
    }

    // Terminals that don't paste on Ctrl+V (most Linux ones, Windows consoles
    // with the binding off) send the key through; read the clipboard instead.
    if is_paste_key(&key) {
        match sessy::clipboard::paste() {
            Ok(text) if !text.trim().is_empty() => handle_paste(app, &text),
            Ok(_) => app.set_status("Clipboard is empty".to_string()),
            Err(e) => app.set_status(format!("Paste failed: {}", e)),
        }
        return;
    }

    // Typed a query and left the input within one burst of keys: the view
    // must be current before any list action reads the selection.
    if app.search_dirty && app.focus != Focus::Search {
        app.apply_search();
        preview::request_preview(app);
    }

    match app.focus {
        Focus::Search => handle_search_key(app, key),
        Focus::PreviewSearch => handle_preview_search_key(app, key),
        Focus::Rename => handle_rename_key(app, key),
        Focus::Preview => handle_preview_key(app, key),
        Focus::List => handle_list_key(app, key),
    }
}

/// Ctrl+V, or ⌘V when the terminal forwards it as a key.
fn is_paste_key(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V'))
        && key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

fn handle_paste(app: &mut App, text: &str) {
    // An open prompt takes the paste as its next input, like any key: it
    // isn't `y`, so a pending delete is cancelled.
    if app.confirm_delete || app.show_help {
        app.confirm_delete = false;
        app.show_help = false;
        return;
    }
    match app.focus {
        Focus::Search => {
            app.search_query.insert_str(text);
            app.search_dirty = true;
        }
        Focus::PreviewSearch => {
            app.preview_search_query.insert_str(text);
            app.update_preview_search();
        }
        Focus::Rename => app.rename_input.insert_str(text),
        // Outside an input, pasting searches for the pasted text right away.
        Focus::List | Focus::Preview => {
            let text = text.trim();
            if text.is_empty() {
                return;
            }
            app.view_mode = ViewMode::Normal;
            app.focus = Focus::Search;
            app.search_query.clear();
            app.search_query.insert_str(text);
            app.search_dirty = true;
        }
    }
}

fn handle_search_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // Navigation moves through the results (like fzf) without leaving the input.
    let nav = match key.code {
        KeyCode::Up => Some(App::move_up as fn(&mut App)),
        KeyCode::Down => Some(App::move_down as fn(&mut App)),
        KeyCode::Char('p') if ctrl => Some(App::move_up as fn(&mut App)),
        KeyCode::Char('n') if ctrl => Some(App::move_down as fn(&mut App)),
        KeyCode::PageUp => Some(App::page_up as fn(&mut App)),
        KeyCode::PageDown => Some(App::page_down as fn(&mut App)),
        _ => None,
    };
    if let Some(nav) = nav {
        if app.search_dirty {
            app.apply_search();
        }
        nav(app);
        preview::request_preview(app);
        return;
    }

    match key.code {
        KeyCode::Esc => {
            app.handle_esc();
            preview::request_preview(app);
        }
        KeyCode::Enter => {
            app.focus = Focus::List;
        }
        KeyCode::Tab => {
            app.focus = Focus::Preview;
        }
        _ => {
            if handle_text_input_key(&mut app.search_query, key) {
                app.search_dirty = true;
            }
        }
    }
}

fn handle_preview_search_key(app: &mut App, key: KeyEvent) {
    match key.code {
        // Esc cancels; Enter commits the search so n/N can walk the matches.
        KeyCode::Esc => {
            app.exit_preview_search();
        }
        KeyCode::Enter => {
            app.commit_preview_search();
        }
        _ => {
            if handle_text_input_key(&mut app.preview_search_query, key) {
                app.update_preview_search();
            }
        }
    }
}

fn handle_rename_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.cancel_rename(),
        KeyCode::Enter => app.commit_rename(),
        _ => {
            handle_text_input_key(&mut app.rename_input, key);
        }
    }
}

/// Shared line editing for the text inputs: cursor movement (arrows, word
/// jumps, Home/End and their macOS/readline synonyms) and edits at the cursor.
/// Returns true only when the text actually changed — cursor moves and no-op
/// edits (Backspace on an empty query) must not re-run the search, which
/// would reset the selection.
fn handle_text_input_key(input: &mut sessy::input::TextInput, key: KeyEvent) -> bool {
    let before = input.text().to_string();
    edit_text_input(input, key);
    input.text() != before
}

fn edit_text_input(input: &mut sessy::input::TextInput, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let cmd = key.modifiers.contains(KeyModifiers::SUPER);

    match key.code {
        KeyCode::Backspace if alt || ctrl => input.delete_word_backwards(),
        KeyCode::Backspace if cmd => input.delete_to_start(),
        KeyCode::Backspace => input.backspace(),
        KeyCode::Delete => input.delete_forward(),
        KeyCode::Char('h') if ctrl => input.backspace(),
        KeyCode::Char('w') if ctrl => input.delete_word_backwards(),
        KeyCode::Char('u') if ctrl => input.delete_to_start(),
        KeyCode::Char('k') if ctrl => input.delete_to_end(),
        KeyCode::Left if alt || ctrl => input.move_word_left(),
        KeyCode::Right if alt || ctrl => input.move_word_right(),
        KeyCode::Char('b') if alt => input.move_word_left(),
        KeyCode::Char('f') if alt => input.move_word_right(),
        KeyCode::Left if cmd => input.move_home(),
        KeyCode::Right if cmd => input.move_end(),
        KeyCode::Left => input.move_left(),
        KeyCode::Right => input.move_right(),
        KeyCode::Home => input.move_home(),
        KeyCode::End => input.move_end(),
        KeyCode::Char('a') if ctrl => input.move_home(),
        KeyCode::Char('e') if ctrl => input.move_end(),
        KeyCode::Char(c) if !ctrl && !alt && !cmd => input.insert(c),
        _ => {}
    }
}

/// Modifier combos other than Shift. Keys are bound on their bare letter; a
/// stray Ctrl+D must not reach the `d` (delete) binding.
fn has_command_modifier(key: &KeyEvent) -> bool {
    key.modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
}

fn handle_preview_key(app: &mut App, key: KeyEvent) {
    if has_command_modifier(&key) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('d') | KeyCode::Char('f') => app.scroll_preview_page_down(),
                KeyCode::Char('u') | KeyCode::Char('b') => app.scroll_preview_page_up(),
                KeyCode::Char('n') => app.scroll_preview_down(),
                KeyCode::Char('p') => app.scroll_preview_up(),
                _ => {}
            }
        }
        return;
    }
    match key.code {
        // Tab/← always return to the list; Esc first clears an active search.
        KeyCode::Tab | KeyCode::BackTab | KeyCode::Left => app.focus = Focus::List,
        KeyCode::Esc => app.handle_esc(),
        KeyCode::Up | KeyCode::Char('k') => app.scroll_preview_up(),
        KeyCode::Down | KeyCode::Char('j') => app.scroll_preview_down(),
        KeyCode::PageUp => app.scroll_preview_page_up(),
        KeyCode::PageDown | KeyCode::Char(' ') => app.scroll_preview_page_down(),
        KeyCode::Home | KeyCode::Char('g') => app.scroll_preview_top(),
        KeyCode::End | KeyCode::Char('G') => app.scroll_preview_bottom(),
        KeyCode::Char('/') => app.start_preview_search(),
        KeyCode::Char('n') => app.next_preview_match(),
        KeyCode::Char('N') => app.prev_preview_match(),
        KeyCode::Char('T') => {
            app.toggle_tools();
            preview::request_preview(app);
        }
        KeyCode::Char('f') => app.toggle_files(),
        KeyCode::Char('?') => app.show_help = true,
        KeyCode::Char('q') => {
            app.action = AppAction::Quit;
        }
        _ => {}
    }
}

fn handle_list_key(app: &mut App, key: KeyEvent) {
    if has_command_modifier(&key) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && app.view_mode == ViewMode::Normal {
            match key.code {
                KeyCode::Char('n') => app.move_down(),
                KeyCode::Char('p') => app.move_up(),
                KeyCode::Char('d') | KeyCode::Char('f') => app.page_down(),
                KeyCode::Char('u') | KeyCode::Char('b') => app.page_up(),
                _ => return,
            }
            preview::request_preview(app);
        }
        return;
    }

    // In timeline view, only allow t/Esc/q/?
    if app.view_mode == ViewMode::Timeline {
        match key.code {
            KeyCode::Char('t') | KeyCode::Esc => app.handle_esc(),
            KeyCode::Char('?') => app.show_help = true,
            KeyCode::Char('q') => {
                app.action = AppAction::Quit;
            }
            _ => {}
        }
        return;
    }

    let has_selection = app.selected_session().is_some();
    match key.code {
        KeyCode::Esc => {
            app.handle_esc();
            preview::request_preview(app);
        }
        KeyCode::Char('q') => {
            app.action = AppAction::Quit;
        }
        KeyCode::Char('/') => {
            app.focus = Focus::Search;
        }
        KeyCode::Char('a') => {
            app.toggle_scope();
            preview::request_preview(app);
        }
        KeyCode::Char('?') => {
            app.show_help = true;
        }
        KeyCode::Tab | KeyCode::BackTab | KeyCode::Right => {
            app.focus = Focus::Preview;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.move_up();
            preview::request_preview(app);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.move_down();
            preview::request_preview(app);
        }
        KeyCode::PageUp => {
            app.page_up();
            preview::request_preview(app);
        }
        KeyCode::PageDown => {
            app.page_down();
            preview::request_preview(app);
        }
        KeyCode::Char('g') | KeyCode::Home => {
            app.move_to_top();
            preview::request_preview(app);
        }
        KeyCode::Char('G') | KeyCode::End => {
            app.move_to_bottom();
            preview::request_preview(app);
        }
        // In --print mode every "pick this one" key prints: launching claude
        // inside `$(sessy --print)` would capture its output.
        KeyCode::Enter | KeyCode::Char('l') | KeyCode::Char('p') if has_selection && app.print_mode => {
            app.action = AppAction::Print(app.selected);
        }
        KeyCode::Enter if has_selection => {
            app.action = if app.enter_yolo {
                AppAction::LaunchDangerously(app.selected)
            } else {
                AppAction::Launch(app.selected)
            };
        }
        KeyCode::Char('l') if has_selection => {
            app.action = AppAction::Launch(app.selected);
        }
        KeyCode::Char('p') if has_selection => {
            app.action = AppAction::Print(app.selected);
        }
        KeyCode::Char('c') => app.copy_selected(),
        KeyCode::Char('o') => app.open_pr(),
        KeyCode::Char('r') => app.start_rename(),
        KeyCode::Char('s') => {
            app.cycle_sort();
            preview::request_preview(app);
        }
        KeyCode::Char('e') => {
            app.export_selected();
        }
        KeyCode::Char('b') => {
            app.toggle_bookmark();
        }
        KeyCode::Char('t') => {
            app.toggle_timeline();
        }
        KeyCode::Char('T') => {
            app.toggle_tools();
            preview::request_preview(app);
        }
        KeyCode::Char('f') => {
            app.toggle_files();
        }
        KeyCode::Char('d') if has_selection => {
            app.confirm_delete = true;
        }
        KeyCode::Char(c @ '1'..='4') => {
            let category = match c {
                '1' => "quick",
                '2' => "medium",
                '3' => "deep",
                _ => "massive",
            };
            app.toggle_size_filter(category);
            preview::request_preview(app);
        }
        KeyCode::Char('0') => {
            app.clear_size_filter();
            preview::request_preview(app);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn test_app() -> App {
        let cache = sessy::text_cache::TextCache::open(std::path::Path::new("/does/not/exist"));
        App::new(vec![], false, HashSet::new(), cache)
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn app_with(ids: &[&str]) -> App {
        let sessions = ids
            .iter()
            .enumerate()
            .map(|(i, id)| session::SessionMeta {
                id: id.to_string(),
                project: "p".into(),
                branch: String::new(),
                name: format!("session {}", id),
                title: String::new(),
                last_message: String::new(),
                duration_secs: 0,
                timestamp: 100 - i as i64,
                file_size: 0,
                file_mtime: 0,
                file_path: PathBuf::from(format!("/does/not/exist/{}.jsonl", id)),
                cwd: String::new(),
                message_count: 0,
                tickets: vec![],
                text_offset: 0,
                text_len: 0,
                name_lc: format!("session {}", id),
                title_lc: String::new(),
                project_lc: "p".into(),
                branch_lc: String::new(),
                permission_mode: String::new(),
                cc_version: String::new(),
                skills: vec![],
                changed_files: vec![],
                changed_files_lc: String::new(),
                prs: vec![],
                recap: String::new(),
            })
            .collect();
        let cache = sessy::text_cache::TextCache::open(Path::new("/does/not/exist"));
        let mut app = App::new(sessions, false, HashSet::new(), cache);
        app.rebuild_view();
        app
    }

    fn press(app: &mut App, code: KeyCode) {
        handle_key(app, key(code, KeyModifiers::NONE));
    }

    #[test]
    fn ctrl_c_quits_from_the_search_input() {
        let mut app = app_with(&["a"]);
        app.focus = Focus::Search;
        handle_key(&mut app, key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(app.action, AppAction::Quit);
        assert_eq!(app.search_query.text(), "");
    }

    #[test]
    fn ctrl_letters_do_not_trigger_list_actions() {
        let mut app = app_with(&["a", "b"]);
        // Ctrl+D is a page-down habit, not "delete"; Ctrl+L must not launch.
        handle_key(&mut app, key(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert!(!app.confirm_delete);
        assert_eq!(app.selected, 1, "Ctrl+D pages down");
        handle_key(&mut app, key(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert_eq!(app.action, AppAction::None);
        handle_key(&mut app, key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(app.selected, 0, "Ctrl+P moves up");
    }

    #[test]
    fn delete_needs_an_explicit_y() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char('d'));
        assert!(app.confirm_delete);
        // A second `d` (double tap) cancels instead of deleting.
        press(&mut app, KeyCode::Char('d'));
        assert!(!app.confirm_delete);
        assert_eq!(app.sessions.len(), 1);
    }

    #[test]
    fn arrows_in_search_move_through_fresh_results() {
        let mut app = app_with(&["a", "b", "c"]);
        press(&mut app, KeyCode::Char('/'));
        for c in "session".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        assert!(app.search_dirty, "typing defers the search to the event loop");
        press(&mut app, KeyCode::Down);
        assert!(!app.search_dirty, "navigation runs the pending search first");
        assert_eq!(app.focus, Focus::Search, "focus stays in the input");
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn list_keys_in_the_same_burst_see_the_typed_query() {
        let mut app = app_with(&["a", "b", "c"]);
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Char('c'));
        press(&mut app, KeyCode::Enter); // back to the list, search still pending
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.action, AppAction::Print(0));
        assert_eq!(app.selected_session().map(|s| s.id.as_str()), Some("c"));
    }

    #[test]
    fn print_mode_makes_every_pick_key_print() {
        let mut app = app_with(&["a"]);
        app.print_mode = true;
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.action, AppAction::Print(0));
    }

    #[test]
    fn enter_follows_the_configured_action() {
        let mut app = app_with(&["a"]);
        app.enter_yolo = false;
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.action, AppAction::Launch(0));
    }

    #[test]
    fn help_opens_from_preview_and_any_key_closes_it() {
        let mut app = app_with(&["a"]);
        app.focus = Focus::Preview;
        press(&mut app, KeyCode::Char('?'));
        assert!(app.show_help);
        press(&mut app, KeyCode::Char('q'));
        assert!(!app.show_help);
        assert_eq!(app.action, AppAction::None, "the closing key is swallowed");
    }

    #[test]
    fn paste_into_search_is_one_edit() {
        let mut app = app_with(&["a"]);
        app.focus = Focus::Search;
        handle_paste(&mut app, "PROJ-123\n");
        assert_eq!(app.search_query.text(), "PROJ-123");
        assert!(app.search_dirty);
    }

    #[test]
    fn paste_in_the_list_starts_a_fresh_search() {
        let mut app = app_with(&["a", "b"]);
        app.search_query = "old".into();
        handle_paste(&mut app, "  PROJ-123\n");
        assert_eq!(app.focus, Focus::Search);
        assert_eq!(app.search_query.text(), "PROJ-123", "replaces the query, trimmed");
        assert!(app.search_dirty, "the event loop searches right after the paste");
    }

    #[test]
    fn paste_in_the_preview_or_timeline_searches_sessions() {
        let mut app = app_with(&["a"]);
        app.focus = Focus::Preview;
        handle_paste(&mut app, "b");
        assert_eq!(app.focus, Focus::Search);
        assert_eq!(app.search_query.text(), "b");

        let mut app = app_with(&["a"]);
        app.view_mode = ViewMode::Timeline;
        handle_paste(&mut app, "b");
        assert_eq!(app.view_mode, ViewMode::Normal, "results need the list view");
        assert_eq!(app.search_query.text(), "b");
    }

    #[test]
    fn blank_paste_in_the_list_keeps_the_current_search() {
        let mut app = app_with(&["a"]);
        app.search_query = "keep".into();
        handle_paste(&mut app, " \n");
        assert_eq!(app.focus, Focus::List);
        assert_eq!(app.search_query.text(), "keep");
        assert!(!app.search_dirty);
    }

    #[test]
    fn paste_cancels_a_pending_delete() {
        let mut app = app_with(&["a"]);
        press(&mut app, KeyCode::Char('d'));
        handle_paste(&mut app, "yes");
        assert!(!app.confirm_delete);
        assert_eq!(app.focus, Focus::List, "the prompt swallows the paste");
        // The next `y` is a plain key again, not a delete confirmation.
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.sessions.len(), 1);
    }

    #[test]
    fn ctrl_v_and_cmd_v_are_paste_keys() {
        assert!(is_paste_key(&key(KeyCode::Char('v'), KeyModifiers::CONTROL)));
        assert!(is_paste_key(&key(KeyCode::Char('v'), KeyModifiers::SUPER)));
        assert!(is_paste_key(&key(
            KeyCode::Char('V'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
        assert!(!is_paste_key(&key(KeyCode::Char('v'), KeyModifiers::NONE)));
        assert!(!is_paste_key(&key(KeyCode::Char('v'), KeyModifiers::ALT)));
        assert!(!is_paste_key(&key(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL | KeyModifiers::ALT
        )));
    }

    #[test]
    fn project_root_is_the_enclosing_git_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        assert_eq!(project_root(&repo.join("src/deep")), repo);
        let loose = dir.path().join("loose");
        std::fs::create_dir_all(&loose).unwrap();
        assert_eq!(project_root(&loose), loose);
    }

    #[test]
    fn cmd_backspace_clears_search_query() {
        let mut app = test_app();
        app.focus = Focus::Search;
        app.search_query = "hello world".into();
        handle_search_key(&mut app, key(KeyCode::Backspace, KeyModifiers::SUPER));
        assert_eq!(app.search_query.text(), "");
    }

    #[test]
    fn alt_backspace_deletes_word_in_search_query() {
        let mut app = test_app();
        app.focus = Focus::Search;
        app.search_query = "hello world".into();
        handle_search_key(&mut app, key(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(app.search_query.text(), "hello ");
    }

    #[test]
    fn shifted_chars_still_type_into_search_query() {
        let mut app = test_app();
        app.focus = Focus::Search;
        handle_search_key(&mut app, key(KeyCode::Char('A'), KeyModifiers::SHIFT));
        assert_eq!(app.search_query.text(), "A");
    }

    #[test]
    fn arrows_move_cursor_and_edit_mid_string() {
        let mut app = test_app();
        app.focus = Focus::Search;
        app.search_query = "helo world".into();
        // ⌘← to start, then → →, then insert the missing 'l'.
        handle_search_key(&mut app, key(KeyCode::Left, KeyModifiers::SUPER));
        handle_search_key(&mut app, key(KeyCode::Right, KeyModifiers::NONE));
        handle_search_key(&mut app, key(KeyCode::Right, KeyModifiers::NONE));
        handle_search_key(&mut app, key(KeyCode::Char('l'), KeyModifiers::NONE));
        assert_eq!(app.search_query.text(), "hello world");
    }

    #[test]
    fn alt_arrows_jump_words_and_backspace_deletes_before_cursor() {
        let mut app = test_app();
        app.focus = Focus::Search;
        app.search_query = "hello brave world".into();
        // ⌥← twice → cursor at start of "brave"; ⌥⌫ deletes "hello ".
        handle_search_key(&mut app, key(KeyCode::Left, KeyModifiers::ALT));
        handle_search_key(&mut app, key(KeyCode::Left, KeyModifiers::ALT));
        handle_search_key(&mut app, key(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(app.search_query.text(), "brave world");
    }

    #[test]
    fn cmd_backspace_deletes_to_start_keeping_tail() {
        let mut app = test_app();
        app.focus = Focus::Search;
        app.search_query = "hello world".into();
        handle_search_key(&mut app, key(KeyCode::Left, KeyModifiers::ALT));
        handle_search_key(&mut app, key(KeyCode::Backspace, KeyModifiers::SUPER));
        assert_eq!(app.search_query.text(), "world");
    }

    #[test]
    fn cursor_moves_do_not_reset_selection() {
        // Cursor-only keys must not rebuild the view (which resets selection).
        let mut app = test_app();
        app.focus = Focus::Search;
        app.search_query = "abc".into();
        app.selected = 3;
        handle_search_key(&mut app, key(KeyCode::Left, KeyModifiers::NONE));
        handle_search_key(&mut app, key(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.selected, 3);
    }

    #[test]
    fn noop_edits_do_not_reset_the_selection() {
        let mut app = app_with(&["a", "b", "c"]);
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        // Stray Backspace / Delete on an empty query changes nothing.
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Delete);
        assert!(!app.search_dirty);
        assert_eq!(app.selected, 2);
    }

    #[test]
    fn cmd_backspace_clears_preview_search_query() {
        let mut app = test_app();
        app.focus = Focus::PreviewSearch;
        app.preview_search_query = "hello world".into();
        handle_preview_search_key(&mut app, key(KeyCode::Backspace, KeyModifiers::SUPER));
        assert_eq!(app.preview_search_query.text(), "");
    }

    #[test]
    fn alt_backspace_deletes_word_in_preview_search_query() {
        let mut app = test_app();
        app.focus = Focus::PreviewSearch;
        app.preview_search_query = "hello world".into();
        handle_preview_search_key(&mut app, key(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(app.preview_search_query.text(), "hello ");
    }
}
