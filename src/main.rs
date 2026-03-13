mod app;
mod index;
mod parser;
mod preview;
mod session;
mod ui;

use app::{App, AppAction, Focus};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
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

    // Purge: delete tiny old sessions
    if cli.purge {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let two_days_ago = now - 2 * 86400;
        let size_limit = 15 * 1024; // 15 KB

        let to_purge: Vec<usize> = idx
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| s.file_size < size_limit && s.timestamp < two_days_ago)
            .map(|(i, _)| i)
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
            for &i in to_purge.iter().rev() {
                let path = &idx.sessions[i].file_path;
                if std::fs::remove_file(path).is_ok() {
                    // Also remove companion UUID directory
                    let companion = path.with_extension("");
                    if companion.is_dir() {
                        std::fs::remove_dir_all(&companion).ok();
                    }
                    deleted += 1;
                }
                idx.sessions.remove(i);
            }
            index::save_index(&idx);
            println!("Purged {} sessions.", deleted);
        } else {
            println!("Aborted.");
        }
        return Ok(());
    }

    // Default: filter to current directory's sessions
    if !cli.all {
        let cwd = std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        // Claude encodes project paths by replacing / with -
        let encoded = format!("-{}", cwd.trim_start_matches('/').replace('/', "-"));
        idx.sessions.retain(|s| {
            s.file_path
                .to_string_lossy()
                .contains(&format!("/{}/", encoded))
        });
    }

    // Apply filters
    if let Some(ref project_filter) = cli.project {
        let filter_lower = project_filter.to_lowercase();
        idx.sessions
            .retain(|s| s.project.to_lowercase().contains(&filter_lower));
    }

    if let Some(ref recent) = cli.recent {
        if let Some(secs) = index::parse_recent_filter(recent) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let cutoff = now - secs as i64;
            idx.sessions.retain(|s| s.timestamp >= cutoff);
        }
    }

    // Run TUI
    let mut app = App::new(idx.sessions, cli.print);

    let mut terminal = ratatui::init();
    let result = run_event_loop(&mut terminal, &mut app);
    ratatui::restore();

    // Handle post-TUI actions
    match app.action {
        AppAction::Launch(idx) | AppAction::LaunchDangerously(idx) => {
            if let Some(session) = app.sessions.get(
                *app.filtered_indices.get(idx).unwrap_or(&0),
            ) {
                // cd to session's original working directory before resuming
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
            if let Some(session) = app.sessions.get(
                *app.filtered_indices.get(idx).unwrap_or(&0),
            ) {
                let cmd = format!("claude --resume {}", session.id);
                match copypasta::ClipboardContext::new() {
                    Ok(mut ctx) => {
                        use copypasta::ClipboardProvider;
                        if let Err(e) = ctx.set_contents(cmd.clone()) {
                            eprintln!("Clipboard error: {}", e);
                        } else {
                            println!("Copied: {}", cmd);
                        }
                    }
                    Err(e) => eprintln!("Clipboard error: {}", e),
                }
            }
        }
        AppAction::Print(idx) => {
            if let Some(session) = app.sessions.get(
                *app.filtered_indices.get(idx).unwrap_or(&0),
            ) {
                println!("{}", session.id);
            }
        }
        _ => {}
    }

    result
}

fn run_event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
) -> io::Result<()> {
    // Trigger initial preview load
    if !app.filtered_indices.is_empty() {
        preview::request_preview(app);
    }

    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        // Poll with timeout to allow background preview updates
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                // Handle delete confirmation mode
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
                    Focus::Search => match key.code {
                        KeyCode::Esc => app.handle_esc(),
                        KeyCode::Enter => {
                            app.focus = Focus::List;
                        }
                        KeyCode::Backspace => {
                            app.search_query.pop();
                            app.apply_search();
                            preview::request_preview(app);
                        }
                        KeyCode::Char(c) => {
                            app.search_query.push(c);
                            app.apply_search();
                            preview::request_preview(app);
                        }
                        _ => {}
                    },
                    Focus::Preview => match key.code {
                        KeyCode::Esc | KeyCode::Tab => app.handle_esc(),
                        KeyCode::Up | KeyCode::Char('k') => app.scroll_preview_up(),
                        KeyCode::Down | KeyCode::Char('j') => app.scroll_preview_down(),
                        KeyCode::Char('q') => {
                            app.action = AppAction::Quit;
                        }
                        _ => {}
                    },
                    Focus::List => match key.code {
                        KeyCode::Esc => app.handle_esc(),
                        KeyCode::Char('q') => {
                            app.action = AppAction::Quit;
                        }
                        KeyCode::Char('/') => {
                            app.focus = Focus::Search;
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
                        KeyCode::Enter => {
                            if app.print_mode {
                                app.action = AppAction::Print(app.selected);
                            } else {
                                app.action = AppAction::Launch(app.selected);
                            }
                        }
                        KeyCode::Char('y') => {
                            app.action = AppAction::LaunchDangerously(app.selected);
                        }
                        KeyCode::Char('c') => {
                            app.action = AppAction::Yank(app.selected);
                        }
                        KeyCode::Char('p') => {
                            app.action = AppAction::Print(app.selected);
                        }
                        KeyCode::Char('s') => {
                            app.cycle_sort();
                            preview::request_preview(app);
                        }
                        KeyCode::Char('d') => {
                            if !app.filtered_indices.is_empty() {
                                app.confirm_delete = true;
                            }
                        }
                        _ => {}
                    },
                }
            }
        }

        // Check for preview updates
        preview::check_preview_updates(app);

        if app.action != AppAction::None {
            break;
        }
    }

    Ok(())
}
