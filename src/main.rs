use sessy::{app, bookmarks, config, index, preview, session, text_cache, ui};
use app::{App, AppAction, Focus, Scope, ViewMode};
use clap::Parser;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use std::io;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "sessy", version, about = "TUI session manager for Claude Code")]
struct Cli {
    /// Filter to sessions from a specific project (substring match)
    #[arg(long)]
    project: Option<String>,

    /// Print selected session ID to stdout and exit
    #[arg(long)]
    print: bool,

    /// Only show sessions from a recent time window (e.g. 1h, 7d, 2w, 1m)
    #[arg(long)]
    recent: Option<String>,

    /// Show sessions from all projects (default: current directory only)
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
    idx.sessions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    // Save index before applying runtime filters
    index::save_index(&idx);

    // Compute the encoded launch-directory prefix so the in-TUI scope toggle
    // (`a`) can switch between current-directory and all-projects live.
    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let cwd_encoded = index::encode_project_path(&cwd);

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
        let cutoff = now - secs as i64;
        idx.sessions.retain(|s| s.timestamp >= cutoff);
    }

    // Purge: delete tiny old sessions. Runs after the CLI filters so
    // `--project X --purge` only touches that project's sessions.
    if cli.purge {
        return run_purge(&idx);
    }

    // Load bookmarks
    let bookmarks = bookmarks::load_bookmarks();

    // Run TUI
    let cfg = config::load();
    let tc = text_cache::TextCache::open(&text_cache::text_cache_path());
    let mut app = App::new(idx.sessions, cli.print, bookmarks, tc);
    // An unreadable cwd yields an empty encoding; disable scope filtering
    // instead of silently matching nothing.
    app.cwd_encoded = if cwd_encoded.is_empty() {
        None
    } else {
        Some(cwd_encoded)
    };
    app.scope = if cli.all || cfg.scope_is_all() {
        Scope::All
    } else {
        Scope::Current
    };
    app.sort_mode = cfg.sort_mode();
    app.show_tools = cfg.show_tool_activity;
    app.rebuild_view(); // apply scope filter + bookmark floating on initial load

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
        let result = run_event_loop(&mut terminal, &mut app);
        pop_keyboard_enhancement(&mut io::stdout(), kbd_enhanced);
        ratatui::restore();
        result
    };

    // Handle post-TUI actions
    handle_post_tui_action(&app);

    result
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
    let result = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(io::stderr()))
        .and_then(|mut terminal| run_event_loop(&mut terminal, app));
    pop_keyboard_enhancement(&mut io::stderr(), kbd_enhanced);
    let _ = crossterm::execute!(io::stderr(), LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
    result
}

/// Enable the kitty keyboard protocol when the terminal supports it, so
/// modifier combinations like Cmd+Backspace and Alt+Backspace reach the app.
/// The support probe talks to /dev/tty directly, keeping stdout clean for
/// `--print` command substitution. Returns whether the flags were pushed.
fn push_keyboard_enhancement<W: io::Write>(out: &mut W) -> bool {
    if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
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

fn run_purge(idx: &index::SessionIndex) -> io::Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let two_days_ago = now - 2 * 86400;
    let size_limit = 15 * 1024;

    let to_purge: Vec<&session::SessionMeta> = idx
        .sessions
        .iter()
        .filter(|s| s.file_size < size_limit && s.timestamp < two_days_ago)
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

fn handle_post_tui_action(app: &App) {
    let resolve = |idx: usize| -> Option<&session::SessionMeta> {
        app.filtered_indices
            .get(idx)
            .and_then(|&real| app.sessions.get(real))
    };

    match app.action {
        AppAction::Launch(idx) | AppAction::LaunchDangerously(idx) => {
            if let Some(session) = resolve(idx) {
                if !session.cwd.is_empty() {
                    let cwd_path = std::path::Path::new(&session.cwd);
                    if cwd_path.is_dir() {
                        std::env::set_current_dir(cwd_path).ok();
                    }
                }
                let mut cmd = std::process::Command::new("claude");
                cmd.arg("--resume").arg(&session.id);
                if matches!(app.action, AppAction::LaunchDangerously(_)) {
                    cmd.arg("--dangerously-skip-permissions");
                }
                if let Err(e) = cmd.status() {
                    eprintln!("Failed to launch claude: {}", e);
                }
            }
        }
        AppAction::Yank(idx) => {
            if let Some(session) = resolve(idx) {
                let cmd = format!("claude --resume {}", session.id);
                match copypasta::ClipboardContext::new() {
                    Ok(mut ctx) => {
                        use copypasta::ClipboardProvider;
                        if let Err(e) = ctx.set_contents(cmd.clone()) {
                            eprintln!("Clipboard error: {}", e);
                        } else {
                            // stderr: keeps stdout clean for --print substitution.
                            eprintln!("Copied: {}", cmd);
                        }
                    }
                    Err(e) => eprintln!("Clipboard error: {}", e),
                }
            }
        }
        AppAction::Print(idx) => {
            if let Some(session) = resolve(idx) {
                println!("{}", session.id);
            }
        }
        _ => {}
    }
}

fn run_event_loop<B: ratatui::backend::Backend<Error = io::Error>>(
    terminal: &mut ratatui::Terminal<B>,
    app: &mut App,
) -> io::Result<()> {
    if !app.filtered_indices.is_empty() {
        preview::request_preview(app);
    }

    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;
        // terminal_height is updated inside draw()

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                // Delete confirmation
                if app.confirm_delete {
                    match key.code {
                        KeyCode::Char('d') | KeyCode::Char('y') => {
                            app.delete_selected();
                            preview::request_preview(app);
                        }
                        _ => {
                            app.confirm_delete = false;
                        }
                    }
                    continue;
                }

                match app.focus {
                    Focus::Search => handle_search_key(app, key),
                    Focus::PreviewSearch => handle_preview_search_key(app, key),
                    Focus::Preview => handle_preview_key(app, key.code),
                    Focus::List => handle_list_key(app, key.code),
                }
            }
        }

        preview::check_preview_updates(app);

        if app.action != AppAction::None {
            break;
        }
    }

    Ok(())
}

fn handle_search_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.handle_esc();
        }
        KeyCode::Enter => {
            app.focus = Focus::List;
        }
        _ => {
            if handle_text_input_key(&mut app.search_query, key) {
                app.apply_search();
                preview::request_preview(app);
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

/// Shared line editing for the search inputs: cursor movement (arrows, word
/// jumps, Home/End and their macOS/readline synonyms) and edits at the cursor.
/// Returns true when the text changed (cursor-only moves return false).
fn handle_text_input_key(input: &mut sessy::input::TextInput, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let cmd = key.modifiers.contains(KeyModifiers::SUPER);

    match key.code {
        KeyCode::Backspace if alt || ctrl => input.delete_word_backwards(),
        KeyCode::Backspace if cmd => input.delete_to_start(),
        KeyCode::Backspace => input.backspace(),
        KeyCode::Delete => input.delete_forward(),
        KeyCode::Char('w') if ctrl => input.delete_word_backwards(),
        KeyCode::Char('u') if ctrl => input.delete_to_start(),
        KeyCode::Char('k') if ctrl => input.delete_to_end(),
        KeyCode::Left if alt || ctrl => {
            input.move_word_left();
            return false;
        }
        KeyCode::Right if alt || ctrl => {
            input.move_word_right();
            return false;
        }
        KeyCode::Left if cmd => {
            input.move_home();
            return false;
        }
        KeyCode::Right if cmd => {
            input.move_end();
            return false;
        }
        KeyCode::Left => {
            input.move_left();
            return false;
        }
        KeyCode::Right => {
            input.move_right();
            return false;
        }
        KeyCode::Home => {
            input.move_home();
            return false;
        }
        KeyCode::End => {
            input.move_end();
            return false;
        }
        KeyCode::Char('a') if ctrl => {
            input.move_home();
            return false;
        }
        KeyCode::Char('e') if ctrl => {
            input.move_end();
            return false;
        }
        KeyCode::Char(c) if !ctrl && !alt && !cmd => input.insert(c),
        _ => return false,
    }
    true
}

fn handle_preview_key(app: &mut App, code: KeyCode) {
    match code {
        // Tab always returns to the list; Esc first clears an active search.
        KeyCode::Tab => app.focus = Focus::List,
        KeyCode::Esc => app.handle_esc(),
        KeyCode::Up | KeyCode::Char('k') => app.scroll_preview_up(),
        KeyCode::Down | KeyCode::Char('j') => app.scroll_preview_down(),
        KeyCode::PageUp => app.scroll_preview_page_up(app.terminal_height / 2),
        KeyCode::PageDown => app.scroll_preview_page_down(app.terminal_height / 2),
        KeyCode::Char('/') => app.start_preview_search(),
        KeyCode::Char('n') => app.next_preview_match(),
        KeyCode::Char('N') => app.prev_preview_match(),
        KeyCode::Char('T') => {
            app.toggle_tools();
            preview::request_preview(app);
        }
        KeyCode::Char('f') => app.toggle_files(),
        KeyCode::Char('q') => {
            app.action = AppAction::Quit;
        }
        _ => {}
    }
}

fn handle_list_key(app: &mut App, code: KeyCode) {
    // The help overlay captures the next keypress to dismiss itself.
    if app.show_help {
        app.show_help = false;
        return;
    }

    // In timeline view, only allow t/Esc/q
    if app.view_mode == ViewMode::Timeline {
        match code {
            KeyCode::Char('t') | KeyCode::Esc => app.handle_esc(),
            KeyCode::Char('q') => {
                app.action = AppAction::Quit;
            }
            _ => {}
        }
        return;
    }

    match code {
        KeyCode::Esc => app.handle_esc(),
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
        KeyCode::Tab => {
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
            let page = (app.terminal_height as usize / 4).max(1);
            app.page_up(page);
            preview::request_preview(app);
        }
        KeyCode::PageDown => {
            let page = (app.terminal_height as usize / 4).max(1);
            app.page_down(page);
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
        KeyCode::Enter => {
            if app.selected_session().is_some() {
                if app.print_mode {
                    app.action = AppAction::Print(app.selected);
                } else {
                    app.action = AppAction::LaunchDangerously(app.selected);
                }
            }
        }
        KeyCode::Char('l') => {
            if app.selected_session().is_some() {
                app.action = AppAction::Launch(app.selected);
            }
        }
        KeyCode::Char('c') => {
            if app.selected_session().is_some() {
                app.action = AppAction::Yank(app.selected);
            }
        }
        KeyCode::Char('p') => {
            if app.selected_session().is_some() {
                app.action = AppAction::Print(app.selected);
            }
        }
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
        KeyCode::Char('d') => {
            if !app.filtered_indices.is_empty() {
                app.confirm_delete = true;
            }
        }
        KeyCode::Char('1') => {
            app.toggle_size_filter("quick");
            preview::request_preview(app);
        }
        KeyCode::Char('2') => {
            app.toggle_size_filter("medium");
            preview::request_preview(app);
        }
        KeyCode::Char('3') => {
            app.toggle_size_filter("deep");
            preview::request_preview(app);
        }
        KeyCode::Char('4') => {
            app.toggle_size_filter("massive");
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
