use crate::input::TextInput;
use crate::parser::Speaker;
use crate::session::SessionMeta;
use crate::text_cache::TextCache;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Focus {
    List,
    Search,
    Preview,
    PreviewSearch,
    Rename,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortMode {
    Date,
    Size,
    Duration,
    Messages,
}

impl SortMode {
    pub fn label(self) -> &'static str {
        match self {
            SortMode::Date => "date",
            SortMode::Size => "size",
            SortMode::Duration => "duration",
            SortMode::Messages => "messages",
        }
    }

    fn next(self) -> SortMode {
        match self {
            SortMode::Date => SortMode::Size,
            SortMode::Size => SortMode::Duration,
            SortMode::Duration => SortMode::Messages,
            SortMode::Messages => SortMode::Date,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewMode {
    Normal,
    Timeline,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scope {
    /// Only sessions started in the launch project (git root, or the launch
    /// directory outside a repo) or any directory below it.
    Current,
    /// Every project's sessions.
    All,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Current => "project",
            Scope::All => "all",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppAction {
    None,
    Launch(usize),
    LaunchDangerously(usize),
    Print(usize),
    Quit,
}

pub struct PreviewResult {
    pub session_id: String,
    pub lines: Vec<(String, String, Speaker)>,
    pub message_count: u32,
    /// Tool-activity setting the lines were extracted under; results from a
    /// stale setting are dropped instead of cached.
    pub include_tools: bool,
}

pub struct App {
    pub sessions: Vec<SessionMeta>,
    pub filtered_indices: Vec<usize>,
    pub selected: usize,
    /// First visible row of the session list (in sessions, not screen rows).
    pub list_offset: usize,
    /// Sessions that fit on one list page (set by the renderer each frame).
    pub list_page_size: usize,
    /// Preview scroll position, in wrapped rows. `usize` because long
    /// sessions easily exceed 65k rows.
    pub preview_scroll: usize,
    pub search_query: TextInput,
    /// The query changed since the last search; the event loop runs the
    /// search once after draining pending keys, so typing (or pasting) fast
    /// doesn't search the full text once per character.
    pub search_dirty: bool,
    pub focus: Focus,
    pub action: AppAction,
    pub print_mode: bool,
    /// What Enter does: resume with `--dangerously-skip-permissions` (true)
    /// or a plain resume.
    pub enter_yolo: bool,
    pub preview_lines: Vec<(String, String, Speaker)>,
    pub preview_loading: bool,
    pub preview_session_id: String,
    pub preview_req_tx: mpsc::Sender<crate::preview::PreviewRequest>,
    pub preview_rx: mpsc::Receiver<PreviewResult>,
    pub preview_cache: std::collections::HashMap<String, Vec<(String, String, Speaker)>>,
    pub preview_cache_order: VecDeque<String>,
    pub confirm_delete: bool,
    pub sort_mode: SortMode,
    /// Sort applied to search results; `None` keeps relevance order.
    pub search_sort: Option<SortMode>,
    pub size_filter: Option<&'static str>,
    pub bookmarks: HashSet<String>,
    /// Where bookmark changes are saved; `None` keeps them in memory (tests).
    pub bookmarks_file: Option<PathBuf>,
    pub text_cache: TextCache,
    pub preview_search_query: TextInput,
    pub preview_search_matches: Vec<usize>,
    pub preview_search_current: usize,
    pub rename_input: TextInput,
    pub view_mode: ViewMode,
    pub status_message: Option<(String, Instant)>,
    pub terminal_height: u16,
    pub preview_inner_width: u16,
    /// First wrapped row of each preview message.
    pub preview_line_offsets: Vec<usize>,
    pub scope: Scope,
    /// Directory `Scope::Current` covers (the launch project). `None`
    /// disables scope filtering entirely.
    pub scope_root: Option<PathBuf>,
    /// Total wrapped rows of the current preview (set by `recompute_preview_offsets`).
    pub preview_total_rows: usize,
    /// Height of the preview viewport (set by the renderer each frame).
    pub preview_viewport_height: usize,
    /// Whether the preview includes tool-use lines.
    pub show_tools: bool,
    /// Whether the preview pane shows the changed-files list instead of the conversation.
    pub show_files: bool,
    /// Whether the keybinding help overlay is open.
    pub show_help: bool,
}

impl App {
    pub fn new(
        sessions: Vec<SessionMeta>,
        print_mode: bool,
        bookmarks: HashSet<String>,
        text_cache: TextCache,
    ) -> Self {
        let filtered_indices: Vec<usize> = (0..sessions.len()).collect();
        let (result_tx, preview_rx) = mpsc::channel();
        let preview_req_tx = crate::preview::spawn_worker(result_tx);
        Self {
            sessions,
            filtered_indices,
            selected: 0,
            list_offset: 0,
            list_page_size: 1,
            preview_scroll: 0,
            search_query: TextInput::default(),
            search_dirty: false,
            focus: Focus::List,
            action: AppAction::None,
            print_mode,
            enter_yolo: true,
            preview_lines: Vec::new(),
            preview_loading: false,
            preview_session_id: String::new(),
            preview_req_tx,
            preview_rx,
            preview_cache: std::collections::HashMap::new(),
            preview_cache_order: VecDeque::new(),
            confirm_delete: false,
            sort_mode: SortMode::Date,
            search_sort: None,
            size_filter: None,
            bookmarks,
            bookmarks_file: None,
            text_cache,
            preview_search_query: TextInput::default(),
            preview_search_matches: Vec::new(),
            preview_search_current: 0,
            rename_input: TextInput::default(),
            view_mode: ViewMode::Normal,
            status_message: None,
            terminal_height: 40,
            preview_inner_width: 0,
            preview_line_offsets: Vec::new(),
            scope: Scope::Current,
            scope_root: None,
            preview_total_rows: 0,
            preview_viewport_height: 0,
            show_tools: false,
            show_files: false,
            show_help: false,
        }
    }

    pub fn toggle_tools(&mut self) {
        self.show_tools = !self.show_tools;
        // Cached previews were built under the previous setting; drop them so
        // the current (and future) sessions re-extract with/without tool lines.
        self.preview_cache.clear();
        self.preview_cache_order.clear();
        self.preview_lines.clear();
        self.preview_session_id.clear();
        self.preview_loading = false;
        self.preview_scroll = 0;
        self.recompute_preview_offsets();
        self.set_status(if self.show_tools {
            "Tool calls shown in preview".to_string()
        } else {
            "Tool calls hidden".to_string()
        });
    }

    pub fn toggle_files(&mut self) {
        self.show_files = !self.show_files;
        self.preview_scroll = 0;
    }

    pub fn selected_session(&self) -> Option<&SessionMeta> {
        self.filtered_indices
            .get(self.selected)
            .and_then(|&idx| self.sessions.get(idx))
    }

    fn selected_id(&self) -> Option<String> {
        self.selected_session().map(|s| s.id.clone())
    }

    /// Put the selection back on session `id` after the view was rebuilt, or
    /// on the first row when it's no longer visible.
    fn reselect(&mut self, id: Option<String>) {
        let pos = id.and_then(|id| {
            self.filtered_indices
                .iter()
                .position(|&i| self.sessions[i].id == id)
        });
        match pos {
            Some(p) => self.selected = p,
            None => {
                self.selected = 0;
                self.preview_scroll = 0;
            }
        }
    }

    fn select(&mut self, index: usize) {
        if index != self.selected {
            self.selected = index;
            self.preview_scroll = 0;
        }
    }

    pub fn move_up(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        let target = if self.selected > 0 {
            self.selected - 1
        } else {
            self.filtered_indices.len() - 1
        };
        self.select(target);
    }

    pub fn move_down(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        let target = if self.selected + 1 < self.filtered_indices.len() {
            self.selected + 1
        } else {
            0
        };
        self.select(target);
    }

    pub fn move_to_top(&mut self) {
        self.select(0);
    }

    pub fn move_to_bottom(&mut self) {
        if !self.filtered_indices.is_empty() {
            self.select(self.filtered_indices.len() - 1);
        }
    }

    pub fn page_up(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        self.select(self.selected.saturating_sub(self.list_page_size.max(1)));
    }

    pub fn page_down(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        let target = (self.selected + self.list_page_size.max(1)).min(self.filtered_indices.len() - 1);
        self.select(target);
    }

    pub fn scroll_preview_up(&mut self) {
        self.preview_scroll = self.preview_scroll.saturating_sub(3);
    }

    pub fn scroll_preview_down(&mut self) {
        self.preview_scroll = (self.preview_scroll + 3).min(self.max_preview_scroll());
    }

    pub fn scroll_preview_page_up(&mut self) {
        let page = (self.preview_viewport_height.saturating_sub(2)).max(1);
        self.preview_scroll = self.preview_scroll.saturating_sub(page);
    }

    pub fn scroll_preview_page_down(&mut self) {
        let page = (self.preview_viewport_height.saturating_sub(2)).max(1);
        self.preview_scroll = (self.preview_scroll + page).min(self.max_preview_scroll());
    }

    pub fn scroll_preview_top(&mut self) {
        self.preview_scroll = 0;
    }

    pub fn scroll_preview_bottom(&mut self) {
        self.preview_scroll = self.max_preview_scroll();
    }

    /// Largest scroll offset that still keeps content on screen. Zero when the
    /// content is shorter than the viewport. In the files view the content is
    /// the changed-files list, not the conversation rows.
    pub fn max_preview_scroll(&self) -> usize {
        let total = if self.show_files {
            self.selected_session()
                .map(|s| s.changed_files.len())
                .unwrap_or(0)
        } else {
            self.preview_total_rows
        };
        total.saturating_sub(self.preview_viewport_height)
    }

    /// Whether `s` belongs to the current scope. Sessions record the
    /// directory they started in; without one, fall back to the encoded
    /// project-dir name Claude Code stores the file under.
    pub fn in_scope(&self, s: &SessionMeta) -> bool {
        if self.scope == Scope::All {
            return true;
        }
        let Some(root) = &self.scope_root else {
            return true;
        };
        if !s.cwd.is_empty() {
            return Path::new(&s.cwd).starts_with(root);
        }
        let encoded = crate::index::encode_project_path(&root.to_string_lossy());
        s.file_path
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|name| name.to_string_lossy() == encoded)
    }

    /// Rebuild filtered_indices from scratch: search → scope → size filter →
    /// sort, with the selection reset to the top (the best match).
    pub fn rebuild_view(&mut self) {
        self.recompute_view();
        self.selected = 0;
        self.preview_scroll = 0;
    }

    /// Rebuild the view but keep the selected session selected when it's
    /// still visible.
    fn rebuild_view_keep_selection(&mut self) {
        let id = self.selected_id();
        self.recompute_view();
        self.reselect(id);
    }

    fn recompute_view(&mut self) {
        if self.search_query.text().trim().is_empty() {
            self.search_sort = None;
        }
        self.apply_search_inner();
        self.apply_scope_filter();
        self.apply_size_filter();
        self.apply_sort();
    }

    fn apply_scope_filter(&mut self) {
        if self.scope == Scope::All || self.scope_root.is_none() {
            return;
        }
        let keep: Vec<bool> = self.sessions.iter().map(|s| self.in_scope(s)).collect();
        self.filtered_indices.retain(|&i| keep[i]);
    }

    pub fn toggle_scope(&mut self) {
        self.scope = match self.scope {
            Scope::Current => Scope::All,
            Scope::All => Scope::Current,
        };
        self.rebuild_view_keep_selection();
    }

    fn apply_search_inner(&mut self) {
        if self.search_query.is_empty() {
            self.filtered_indices = (0..self.sessions.len()).collect();
            return;
        }

        use memchr::memmem::Finder;
        use rayon::prelude::*;

        let query_lc = self.search_query.text().to_lowercase();
        let query_upper = self.search_query.text().trim().to_ascii_uppercase();
        let is_ticket_form = is_ticket_query(&query_upper);
        let tokens: Vec<&str> = query_lc.split_whitespace().collect();
        if tokens.is_empty() {
            self.filtered_indices = (0..self.sessions.len()).collect();
            return;
        }
        let finders: Vec<Finder> = tokens.iter().map(|t| Finder::new(t.as_bytes())).collect();

        let text_cache = &self.text_cache;

        let ticket_matched: Vec<bool> = self
            .sessions
            .iter()
            .map(|s| is_ticket_form && s.tickets.binary_search(&query_upper).is_ok())
            .collect();

        let mut scored: Vec<(usize, i64)> = self
            .sessions
            .par_iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let mut score: i64 = 0;
                let ticket_hit = ticket_matched[i];
                if ticket_hit {
                    score += 1000;
                }
                for (token, finder) in tokens.iter().zip(finders.iter()) {
                    let hit = if ticket_hit {
                        // Ticket exact match satisfies this token; still add
                        // field scores if the token also hits other fields.
                        if finder.find(s.name_lc.as_bytes()).is_some()
                            || finder.find(s.title_lc.as_bytes()).is_some()
                        {
                            score += 500;
                            if starts_at_word_boundary(s.name_lc.as_bytes(), token.as_bytes())
                                || starts_at_word_boundary(s.title_lc.as_bytes(), token.as_bytes())
                            {
                                score += 50;
                            }
                        } else if finder.find(s.project_lc.as_bytes()).is_some() {
                            score += 400;
                        } else if finder.find(s.branch_lc.as_bytes()).is_some() {
                            score += 300;
                        }
                        true
                    } else if finder.find(s.name_lc.as_bytes()).is_some()
                        || finder.find(s.title_lc.as_bytes()).is_some()
                    {
                        score += 500;
                        if starts_at_word_boundary(s.name_lc.as_bytes(), token.as_bytes())
                            || starts_at_word_boundary(s.title_lc.as_bytes(), token.as_bytes())
                        {
                            score += 50;
                        }
                        true
                    } else if finder.find(s.project_lc.as_bytes()).is_some() {
                        score += 400;
                        true
                    } else if finder.find(s.branch_lc.as_bytes()).is_some() {
                        score += 300;
                        true
                    } else if finder.find(s.changed_files_lc.as_bytes()).is_some() {
                        score += 200;
                        true
                    } else {
                        let slice = text_cache.slice(s.text_offset, s.text_len);
                        if finder.find(slice).is_some() {
                            score += 100;
                            true
                        } else {
                            false
                        }
                    };
                    if !hit {
                        return None;
                    }
                }
                Some((i, score))
            })
            .collect();

        // score desc, timestamp desc tiebreak
        scored.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| self.sessions[b.0].timestamp.cmp(&self.sessions[a.0].timestamp))
        });

        self.filtered_indices = scored.into_iter().map(|(i, _)| i).collect();
    }

    fn apply_size_filter(&mut self) {
        if let Some(category) = self.size_filter {
            let sessions = &self.sessions;
            self.filtered_indices
                .retain(|&i| crate::session::size_category(sessions[i].file_size) == category);
        }
    }

    /// Order the view: bookmarks first, then the active sort. While a search
    /// is active and no explicit sort was chosen (`search_sort` is `None`),
    /// the relevance order from `apply_search_inner` is kept — the sort is
    /// stable, so this relies on `filtered_indices` already being in score
    /// order (i.e. the search ran since the last query change).
    pub fn apply_sort(&mut self) {
        let sessions = &self.sessions;
        let bookmarks = &self.bookmarks;
        let mode = if self.search_query.is_empty() {
            Some(self.sort_mode)
        } else {
            self.search_sort
        };

        self.filtered_indices.sort_by(|&a, &b| {
            let a_pinned = bookmarks.contains(&sessions[a].id);
            let b_pinned = bookmarks.contains(&sessions[b].id);
            b_pinned.cmp(&a_pinned).then_with(|| match mode {
                None => std::cmp::Ordering::Equal,
                Some(SortMode::Date) => sessions[b].timestamp.cmp(&sessions[a].timestamp),
                Some(SortMode::Size) => sessions[b].file_size.cmp(&sessions[a].file_size),
                Some(SortMode::Duration) => {
                    sessions[b].duration_secs.cmp(&sessions[a].duration_secs)
                }
                Some(SortMode::Messages) => {
                    sessions[b].message_count.cmp(&sessions[a].message_count)
                }
            })
        });
    }

    /// Label for the sort currently shaping the list.
    pub fn sort_label(&self) -> &'static str {
        if self.search_query.is_empty() {
            self.sort_mode.label()
        } else {
            self.search_sort.map(SortMode::label).unwrap_or("relevance")
        }
    }

    /// Called when search query changes.
    pub fn apply_search(&mut self) {
        self.search_dirty = false;
        self.rebuild_view();
    }

    /// Cycle the sort. Search results cycle relevance → date → … → messages
    /// → relevance; the browse sort is remembered separately.
    pub fn cycle_sort(&mut self) {
        if self.search_query.is_empty() {
            self.sort_mode = self.sort_mode.next();
        } else {
            self.search_sort = match self.search_sort {
                None => Some(SortMode::Date),
                Some(SortMode::Messages) => None,
                Some(mode) => Some(mode.next()),
            };
        }
        self.rebuild_view();
    }

    pub fn toggle_size_filter(&mut self, category: &'static str) {
        if self.size_filter == Some(category) {
            self.size_filter = None;
        } else {
            self.size_filter = Some(category);
        }
        self.rebuild_view_keep_selection();
    }

    pub fn clear_size_filter(&mut self) {
        self.size_filter = None;
        self.rebuild_view_keep_selection();
    }

    pub fn toggle_bookmark(&mut self) {
        let Some(id) = self.selected_id() else {
            return;
        };
        let pinned = if self.bookmarks.remove(&id) {
            false
        } else {
            self.bookmarks.insert(id.clone());
            true
        };
        self.save_bookmarks();
        // Re-sort to float bookmarks to top, keeping the cursor on this
        // session. A full rebuild (not just `apply_sort`) so relevance order
        // is recomputed rather than inherited from the pre-pin order.
        self.rebuild_view_keep_selection();
        self.set_status(if pinned { "Pinned".to_string() } else { "Unpinned".to_string() });
    }

    pub fn export_selected(&mut self) {
        if let Some(session) = self.selected_session().cloned() {
            match crate::export::export_session(&session) {
                Ok(path) => {
                    self.set_status(format!("Exported → {}", path.display()));
                }
                Err(e) => {
                    self.set_status(format!("Export failed: {}", e));
                }
            }
        }
    }

    /// Copy a ready-to-run resume command for the selected session. The TUI
    /// stays open.
    pub fn copy_selected(&mut self) {
        let Some(session) = self.selected_session() else {
            return;
        };
        let here = std::env::current_dir().ok();
        let cmd = resume_command(session, here.as_deref());
        match crate::clipboard::copy(&cmd) {
            Ok(()) => self.set_status(format!("Copied: {}", cmd)),
            Err(e) => self.set_status(format!("Copy failed ({}): {}", e, cmd)),
        }
    }

    /// Open the most recently linked pull request in the browser.
    pub fn open_pr(&mut self) {
        let Some(pr) = self.selected_session().and_then(|s| s.prs.last()).cloned() else {
            self.set_status("No pull request linked to this session".to_string());
            return;
        };
        match crate::clipboard::open_url(&pr.url) {
            Ok(()) => self.set_status(format!("Opened PR #{} {}", pr.number, pr.url)),
            Err(e) => self.set_status(format!("Couldn't open {}: {}", pr.url, e)),
        }
    }

    // Rename

    pub fn start_rename(&mut self) {
        let Some(name) = self.selected_session().map(|s| s.name.clone()) else {
            return;
        };
        self.rename_input = TextInput::from(name.as_str());
        self.focus = Focus::Rename;
    }

    pub fn cancel_rename(&mut self) {
        self.rename_input.clear();
        self.focus = Focus::List;
    }

    /// Persist the new name the way Claude Code's `/rename` does (a
    /// `custom-title` entry), so `claude --resume` shows it too.
    pub fn commit_rename(&mut self) {
        self.focus = Focus::List;
        let name = crate::parser::one_line(self.rename_input.text(), 200);
        self.rename_input.clear();
        let Some(&real) = self.filtered_indices.get(self.selected) else {
            return;
        };
        if name.is_empty() || name == self.sessions[real].name {
            return;
        }
        let session = &self.sessions[real];
        match crate::session::write_custom_title(&session.file_path, &session.id, &name) {
            Ok(()) => {
                let session = &mut self.sessions[real];
                session.name_lc = name.to_lowercase();
                session.name = name.clone();
                self.set_status(format!("Renamed → {}", name));
            }
            Err(e) => self.set_status(format!("Rename failed: {}", e)),
        }
    }

    pub fn toggle_timeline(&mut self) {
        self.view_mode = match self.view_mode {
            ViewMode::Normal => ViewMode::Timeline,
            ViewMode::Timeline => ViewMode::Normal,
        };
    }

    // Preview search

    pub fn start_preview_search(&mut self) {
        // Search always operates on the conversation, so leave the files view.
        self.show_files = false;
        self.focus = Focus::PreviewSearch;
        self.preview_search_query.clear();
        self.preview_search_matches.clear();
        self.preview_search_current = 0;
    }

    pub fn update_preview_search(&mut self) {
        let query_lc = self.preview_search_query.text().to_lowercase();
        if query_lc.is_empty() {
            self.preview_search_matches.clear();
            self.preview_search_current = 0;
            return;
        }
        let finder = memchr::memmem::Finder::new(query_lc.as_bytes());
        self.preview_search_matches = self
            .preview_lines
            .iter()
            .enumerate()
            .filter_map(|(i, (_orig, lower, _speaker))| {
                if finder.find(lower.as_bytes()).is_some() {
                    Some(i)
                } else {
                    None
                }
            })
            .collect();
        self.preview_search_current = 0;
        self.scroll_to_current_match();
    }

    pub fn next_preview_match(&mut self) {
        if self.preview_search_matches.is_empty() {
            return;
        }
        self.preview_search_current =
            (self.preview_search_current + 1) % self.preview_search_matches.len();
        self.scroll_to_current_match();
    }

    pub fn prev_preview_match(&mut self) {
        if self.preview_search_matches.is_empty() {
            return;
        }
        if self.preview_search_current == 0 {
            self.preview_search_current = self.preview_search_matches.len() - 1;
        } else {
            self.preview_search_current -= 1;
        }
        self.scroll_to_current_match();
    }

    /// Width available to message text: the pane minus the match-marker
    /// column and the speaker-prefix column.
    pub fn preview_body_width(&self) -> usize {
        (self.preview_inner_width as usize)
            .saturating_sub(1 + crate::ui::PREFIX_WIDTH)
            .max(10)
    }

    pub fn recompute_preview_offsets(&mut self) {
        let width = self.preview_body_width();
        self.preview_line_offsets.clear();
        self.preview_line_offsets.reserve(self.preview_lines.len());
        let mut cursor: usize = 0;
        for (text, _lower, _speaker) in self.preview_lines.iter() {
            self.preview_line_offsets.push(cursor);
            let rows = crate::ui::message_rows(text, width).len();
            cursor += rows + 1; // +1 blank separator
        }
        self.preview_total_rows = cursor;
    }

    fn scroll_to_current_match(&mut self) {
        let line_idx = match self.preview_search_matches.get(self.preview_search_current) {
            Some(&i) => i,
            None => return,
        };
        let base = self
            .preview_line_offsets
            .get(line_idx)
            .copied()
            .unwrap_or(0);
        let intra = self.intra_match_row(line_idx);
        self.preview_scroll = (base + intra).saturating_sub(2).min(self.max_preview_scroll());
    }

    /// Row within message `line_idx` holding the first match, so long
    /// messages scroll to the hit rather than their first line.
    fn intra_match_row(&self, line_idx: usize) -> usize {
        let Some((_text, lower, _speaker)) = self.preview_lines.get(line_idx) else {
            return 0;
        };
        let query_lc = self.preview_search_query.text().to_lowercase();
        if query_lc.is_empty() {
            return 0;
        }
        let finder = memchr::memmem::Finder::new(query_lc.as_bytes());
        crate::ui::message_rows(lower, self.preview_body_width())
            .iter()
            .position(|row| finder.find(row.as_bytes()).is_some())
            .unwrap_or(0)
    }

    pub fn exit_preview_search(&mut self) {
        self.focus = Focus::Preview;
        self.clear_preview_search();
    }

    /// Leave the search input but keep the query, matches, and highlights so
    /// `n`/`N` can walk between matches from the preview pane.
    pub fn commit_preview_search(&mut self) {
        self.focus = Focus::Preview;
        if self.preview_search_query.is_empty() {
            self.clear_preview_search();
        }
    }

    /// Reset search state without touching focus. Called when the previewed
    /// session changes: match indices refer to the old conversation's lines.
    pub fn clear_preview_search(&mut self) {
        self.preview_search_query.clear();
        self.preview_search_matches.clear();
        self.preview_search_current = 0;
    }

    pub fn preview_search_active(&self) -> bool {
        !self.preview_search_query.is_empty() || !self.preview_search_matches.is_empty()
    }

    // Status message

    pub fn set_status(&mut self, msg: String) {
        self.status_message = Some((msg, Instant::now()));
    }

    pub fn active_status(&self) -> Option<&str> {
        self.status_message.as_ref().and_then(|(msg, when)| {
            if when.elapsed().as_secs() < 3 {
                Some(msg.as_str())
            } else {
                None
            }
        })
    }

    // Cache management (FIFO eviction)

    pub fn cache_preview(&mut self, session_id: String, lines: Vec<(String, String, Speaker)>) {
        if let Some(existing) = self.preview_cache.get_mut(&session_id) {
            *existing = lines;
            return;
        }
        if self.preview_cache.len() >= 10 {
            if let Some(oldest) = self.preview_cache_order.pop_front() {
                self.preview_cache.remove(&oldest);
            }
        }
        self.preview_cache_order.push_back(session_id.clone());
        self.preview_cache.insert(session_id, lines);
    }

    pub fn delete_selected(&mut self) {
        self.confirm_delete = false;
        let Some(&real_idx) = self.filtered_indices.get(self.selected) else {
            return;
        };
        let id = self.sessions[real_idx].id.clone();
        let path = self.sessions[real_idx].file_path.clone();
        if let Err(e) = std::fs::remove_file(&path) {
            self.set_status(format!("Delete failed: {}", e));
            return;
        }
        let companion_dir = path.with_extension("");
        if companion_dir.is_dir() {
            std::fs::remove_dir_all(&companion_dir).ok();
        }
        self.sessions.remove(real_idx);
        self.filtered_indices.retain(|&i| i != real_idx);
        for idx in &mut self.filtered_indices {
            if *idx > real_idx {
                *idx -= 1;
            }
        }
        if self.selected >= self.filtered_indices.len() && self.selected > 0 {
            self.selected -= 1;
        }
        self.preview_scroll = 0;
        self.preview_lines.clear();
        self.preview_session_id.clear();
        self.preview_loading = false;
        self.recompute_preview_offsets();
        self.preview_cache.remove(&id);
        self.preview_cache_order.retain(|c| c != &id);
        self.cleanup_bookmark_for_deleted(&id);
        self.set_status("Session deleted".to_string());
    }

    pub fn cleanup_bookmark_for_deleted(&mut self, id: &str) {
        if self.bookmarks.remove(id) {
            self.save_bookmarks();
        }
    }

    fn save_bookmarks(&self) {
        if let Some(path) = &self.bookmarks_file {
            crate::bookmarks::save_bookmarks(path, &self.bookmarks);
        }
    }

    fn clear_search(&mut self) {
        self.search_query.clear();
        let id = self.selected_id();
        self.apply_search();
        // Leaving a search keeps the session you were on, in its browse position.
        self.reselect(id);
    }

    pub fn handle_esc(&mut self) {
        match self.focus {
            Focus::PreviewSearch => {
                self.exit_preview_search();
            }
            Focus::Rename => self.cancel_rename(),
            Focus::Search if !self.search_query.is_empty() => {
                self.clear_search();
            }
            Focus::Search => {
                self.focus = Focus::List;
            }
            Focus::Preview if self.preview_search_active() => {
                // First Esc clears a committed search (like vim's :noh);
                // a second Esc returns to the list.
                self.clear_preview_search();
            }
            Focus::Preview => {
                self.focus = Focus::List;
            }
            Focus::List if self.view_mode == ViewMode::Timeline => {
                self.view_mode = ViewMode::Normal;
            }
            // An active filter is the thing Esc should undo, not the whole app.
            Focus::List if !self.search_query.is_empty() => {
                self.clear_search();
            }
            Focus::List => {
                self.action = AppAction::Quit;
            }
        }
    }
}

/// Shell command that resumes `session`, prefixed with a `cd` into its
/// directory when that differs from `here` (Claude Code looks sessions up by
/// the directory it's started in).
pub fn resume_command(session: &SessionMeta, here: Option<&Path>) -> String {
    let resume = format!("claude --resume {}", session.id);
    let dir = Path::new(&session.cwd);
    if session.cwd.is_empty() || here == Some(dir) {
        return resume;
    }
    format!("cd {} && {}", shell_quote(&session.cwd), resume)
}

/// Quote `s` for POSIX shells when it contains anything beyond safe characters.
pub fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+=:@%,~".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn is_ticket_query(q_upper: &str) -> bool {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"^[A-Z][A-Z0-9]{1,9}-\d{1,7}$|^#\d{1,7}$").unwrap()
    });
    re.is_match(q_upper)
}

fn starts_at_word_boundary(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return false;
    }
    for idx in memchr::memmem::find_iter(haystack, needle) {
        if idx == 0 || !haystack[idx - 1].is_ascii_alphanumeric() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod search_tests {
    use super::*;
    use crate::text_cache::TextCache;

    fn make_session(
        id: &str,
        name: &str,
        title: &str,
        project: &str,
        branch: &str,
        tickets: Vec<&str>,
    ) -> SessionMeta {
        SessionMeta {
            id: id.into(),
            project: project.into(),
            branch: branch.into(),
            name: name.into(),
            title: title.into(),
            last_message: String::new(),
            duration_secs: 0,
            timestamp: 0,
            file_size: 0,
            file_mtime: 0,
            file_path: PathBuf::from(format!("/tmp/{}.jsonl", id)),
            cwd: String::new(),
            message_count: 0,
            tickets: tickets.into_iter().map(String::from).collect(),
            text_offset: 0,
            text_len: 0,
            name_lc: name.to_lowercase(),
            title_lc: title.to_lowercase(),
            project_lc: project.to_lowercase(),
            branch_lc: branch.to_lowercase(),
            permission_mode: String::new(),
            cc_version: String::new(),
            skills: vec![],
            changed_files: vec![],
            changed_files_lc: String::new(),
            prs: vec![],
            recap: String::new(),
        }
    }

    fn empty_cache() -> TextCache {
        TextCache::open(std::path::Path::new("/does/not/exist"))
    }

    #[test]
    fn test_empty_query_restores_all() {
        let sessions = vec![
            make_session("a", "", "", "p1", "main", vec![]),
            make_session("b", "", "", "p2", "main", vec![]),
        ];
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.search_query.clear();
        app.apply_search();
        assert_eq!(app.filtered_indices.len(), 2);
    }

    #[test]
    fn test_ticket_exact_beats_name() {
        let sessions = vec![
            make_session("a", "PROJ-123", "", "p", "main", vec![]),
            make_session("b", "", "", "p", "main", vec!["PROJ-123"]),
        ];
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.search_query = "PROJ-123".into();
        app.apply_search();
        assert_eq!(app.filtered_indices.len(), 2);
        // session "b" (ticket hit) ranks above "a" (name hit)
        assert_eq!(app.sessions[app.filtered_indices[0]].id, "b");
    }

    #[test]
    fn test_name_beats_project() {
        let sessions = vec![
            make_session("a", "kerveros", "", "other", "main", vec![]),
            make_session("b", "", "", "kerveros", "main", vec![]),
        ];
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.search_query = "kerveros".into();
        app.apply_search();
        assert_eq!(app.sessions[app.filtered_indices[0]].id, "a");
    }

    #[test]
    fn test_and_across_tokens_field_or() {
        let sessions = vec![
            make_session("a", "kerveros encrypt", "", "p", "main", vec![]),
            make_session("b", "kerveros", "", "p", "main", vec![]),
            make_session("c", "kerveros", "", "encrypt-stuff", "main", vec![]),
        ];
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.search_query = "kerveros encrypt".into();
        app.apply_search();
        let ids: Vec<&str> = app
            .filtered_indices
            .iter()
            .map(|&i| app.sessions[i].id.as_str())
            .collect();
        assert!(ids.contains(&"a"), "got {:?}", ids);
        assert!(ids.contains(&"c"), "got {:?}", ids);
        assert!(!ids.contains(&"b"), "session b should not match: {:?}", ids);
    }

    #[test]
    fn test_no_match_returns_empty() {
        let sessions = vec![make_session("a", "kerveros", "", "p", "main", vec![])];
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.search_query = "zzzzznevermatch".into();
        app.apply_search();
        assert!(app.filtered_indices.is_empty());
    }

    #[test]
    fn test_bookmarks_float_to_top_under_relevance() {
        let sessions = vec![
            make_session("a", "kerveros", "", "p", "main", vec![]),
            make_session("b", "kerveros", "", "p", "main", vec![]),
        ];
        let mut bookmarks = HashSet::new();
        bookmarks.insert("b".to_string());
        let mut app = App::new(sessions, false, bookmarks, empty_cache());
        app.search_query = "kerveros".into();
        app.apply_search();
        assert_eq!(app.sessions[app.filtered_indices[0]].id, "b");
    }

    #[test]
    fn test_max_preview_scroll_uses_file_count_in_files_view() {
        let mut s = make_session("a", "x", "", "p", "main", vec![]);
        s.changed_files = vec![
            "f1".into(),
            "f2".into(),
            "f3".into(),
            "f4".into(),
            "f5".into(),
        ];
        let mut app = App::new(vec![s], false, HashSet::new(), empty_cache());
        app.show_files = true;
        app.preview_viewport_height = 2;
        // 5 files, 2 visible → 3 max scroll (independent of conversation rows)
        assert_eq!(app.max_preview_scroll(), 3);
    }

    #[test]
    fn test_start_preview_search_exits_files_view() {
        let mut app = App::new(
            vec![make_session("a", "x", "", "p", "main", vec![])],
            false,
            HashSet::new(),
            empty_cache(),
        );
        app.show_files = true;
        app.start_preview_search();
        assert!(!app.show_files, "starting a search must leave the files view");
    }

    #[test]
    fn test_preview_scroll_clamped_to_content() {
        let sessions = vec![make_session("a", "x", "", "p", "main", vec![])];
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        // Content (10 rows) is shorter than the viewport (20) → no scrolling.
        app.preview_total_rows = 10;
        app.preview_viewport_height = 20;
        for _ in 0..50 {
            app.scroll_preview_down();
        }
        assert_eq!(app.preview_scroll, 0);
    }

    #[test]
    fn test_move_to_top_and_bottom() {
        let sessions = vec![
            make_session("a", "", "", "p", "main", vec![]),
            make_session("b", "", "", "p", "main", vec![]),
            make_session("c", "", "", "p", "main", vec![]),
        ];
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.move_to_bottom();
        assert_eq!(app.selected, 2);
        app.move_to_top();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn test_scope_filter_current_covers_project_and_subdirs() {
        let mut a = make_session("a", "x", "", "p", "main", vec![]);
        a.cwd = "/Users/me/code/foo".into();
        let mut b = make_session("b", "y", "", "p", "main", vec![]);
        b.cwd = "/Users/me/code/bar".into();
        let mut c = make_session("c", "z", "", "p", "main", vec![]);
        c.cwd = "/Users/me/code/foo/.worktrees/spike".into();
        // Shares the prefix but is a sibling directory, not a child.
        let mut d = make_session("d", "w", "", "p", "main", vec![]);
        d.cwd = "/Users/me/code/foobar".into();
        let mut app = App::new(vec![a, b, c, d], false, HashSet::new(), empty_cache());
        app.scope_root = Some(PathBuf::from("/Users/me/code/foo"));
        app.scope = Scope::Current;
        app.rebuild_view();
        let mut ids: Vec<&str> = app
            .filtered_indices
            .iter()
            .map(|&i| app.sessions[i].id.as_str())
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "c"]);
        // toggling to All shows everything
        app.toggle_scope();
        assert_eq!(app.filtered_indices.len(), 4);
    }

    #[test]
    fn test_scope_filter_without_cwd_falls_back_to_encoded_dir() {
        // Claude Code encodes `.` (any non-alphanumeric) as `-` in project dir
        // names; the launch-dir encoding must agree or scope shows nothing.
        let mut a = make_session("a", "x", "", "p", "main", vec![]);
        a.file_path = PathBuf::from(
            "/Users/me/.claude/projects/-Users-me-code-web--worktrees-spike/a.jsonl",
        );
        let mut app = App::new(vec![a], false, HashSet::new(), empty_cache());
        app.scope_root = Some(PathBuf::from("/Users/me/code/web/.worktrees/spike"));
        app.scope = Scope::Current;
        app.rebuild_view();
        assert_eq!(app.filtered_indices.len(), 1);
    }

    fn three_sessions() -> App {
        let mut sessions = Vec::new();
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            let mut s = make_session(id, &format!("name {}", id), "", "p", "main", vec![]);
            s.timestamp = 100 - i as i64; // a newest
            sessions.push(s);
        }
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.rebuild_view();
        app
    }

    fn selected_id(app: &App) -> &str {
        app.selected_session().map(|s| s.id.as_str()).unwrap_or("")
    }

    #[test]
    fn test_bookmark_keeps_cursor_on_the_same_session() {
        let mut app = three_sessions();
        app.move_to_bottom();
        assert_eq!(selected_id(&app), "c");
        // Pinning floats "c" to the top; the cursor must follow it rather than
        // stay on row 2 (now a different session, with a stale preview).
        app.toggle_bookmark();
        assert_eq!(app.selected, 0);
        assert_eq!(selected_id(&app), "c");
        app.toggle_bookmark();
        assert_eq!(selected_id(&app), "c");
        assert_eq!(app.selected, 2, "unpinning returns it to its date slot");
    }

    #[test]
    fn test_unpin_restores_relevance_order() {
        let sessions = vec![
            make_session("a", "kerveros kerveros", "", "p", "main", vec![]),
            make_session("b", "", "", "kerveros", "main", vec![]),
        ];
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.search_query = "kerveros".into();
        app.apply_search();
        app.move_down(); // "b", the weaker (project-only) match
        app.toggle_bookmark();
        assert_eq!(selected_id(&app), "b");
        assert_eq!(app.selected, 0, "pinned floats to the top");
        app.toggle_bookmark();
        assert_eq!(selected_id(&app), "b");
        assert_eq!(app.selected, 1, "unpinned drops back to its relevance slot");
    }

    #[test]
    fn test_esc_in_list_clears_active_search_before_quitting() {
        let mut app = three_sessions();
        app.search_query = "name b".into();
        app.apply_search();
        assert_eq!(app.filtered_indices.len(), 1);
        app.focus = Focus::List;

        app.handle_esc();
        assert_eq!(app.action, AppAction::None, "first Esc clears the search");
        assert!(app.search_query.is_empty());
        assert_eq!(app.filtered_indices.len(), 3);
        assert_eq!(selected_id(&app), "b", "selection stays on the session");

        app.handle_esc();
        assert_eq!(app.action, AppAction::Quit);
    }

    #[test]
    fn test_sort_cycles_through_relevance_while_searching() {
        let mut app = three_sessions();
        app.sessions[2].file_size = 999;
        app.search_query = "name".into();
        app.apply_search();
        assert_eq!(app.sort_label(), "relevance");
        app.cycle_sort();
        assert_eq!(app.sort_label(), "date");
        app.cycle_sort();
        assert_eq!(app.sort_label(), "size");
        assert_eq!(selected_id(&app), "c", "size sort applies to the results");
        app.cycle_sort();
        app.cycle_sort();
        app.cycle_sort();
        assert_eq!(app.sort_label(), "relevance");
        // The browse sort is untouched by search sorting.
        assert_eq!(app.sort_mode, SortMode::Date);
    }

    #[test]
    fn test_scope_toggle_keeps_selection_when_visible() {
        let mut app = three_sessions();
        app.move_down();
        app.toggle_scope();
        assert_eq!(selected_id(&app), "b");
    }

    #[test]
    fn test_resume_command_cds_only_when_needed() {
        let mut s = make_session("abc", "", "", "p", "main", vec![]);
        s.cwd = "/Users/me/My Projects/it's".into();
        assert_eq!(
            resume_command(&s, Some(Path::new("/tmp"))),
            r"cd '/Users/me/My Projects/it'\''s' && claude --resume abc"
        );
        assert_eq!(
            resume_command(&s, Some(Path::new("/Users/me/My Projects/it's"))),
            "claude --resume abc"
        );
        s.cwd = String::new();
        assert_eq!(resume_command(&s, None), "claude --resume abc");
    }

    #[test]
    fn test_shell_quote() {
        assert_eq!(shell_quote("/Users/me/code/foo-bar_1.2"), "/Users/me/code/foo-bar_1.2");
        assert_eq!(shell_quote("/a b"), "'/a b'");
        assert_eq!(shell_quote("$(rm -rf ~)"), "'$(rm -rf ~)'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn test_rename_writes_custom_title_and_updates_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc.jsonl");
        std::fs::write(&path, "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n").unwrap();
        let mut s = make_session("abc", "old name", "", "p", "main", vec![]);
        s.file_path = path.clone();
        let mut app = App::new(vec![s], false, HashSet::new(), empty_cache());

        app.start_rename();
        assert_eq!(app.focus, Focus::Rename);
        assert_eq!(app.rename_input.text(), "old name");
        app.rename_input = "Auth rework".into();
        app.commit_rename();

        assert_eq!(app.focus, Focus::List);
        assert_eq!(app.sessions[0].name, "Auth rework");
        assert_eq!(app.sessions[0].name_lc, "auth rework");
        let scan = crate::parser::scan_session(&path).unwrap();
        assert_eq!(scan.tail.unwrap().rename, "Auth rework");
    }

    #[test]
    fn test_rename_to_blank_is_a_no_op() {
        let mut s = make_session("abc", "keep", "", "p", "main", vec![]);
        s.file_path = PathBuf::from("/does/not/exist.jsonl");
        let mut app = App::new(vec![s], false, HashSet::new(), empty_cache());
        app.start_rename();
        app.rename_input = "   ".into();
        app.commit_rename();
        assert_eq!(app.sessions[0].name, "keep");
        assert!(app.active_status().is_none(), "nothing written, nothing reported");
    }

    #[test]
    fn test_preview_offsets_count_newlines_and_scroll_past_u16() {
        let mut app = three_sessions();
        app.preview_inner_width = 60;
        // One message of 70k lines: the old u16 row math capped scrolling at 65 535.
        let long = "line\n".repeat(70_000);
        app.preview_lines = vec![
            (long.clone(), long.to_lowercase(), Speaker::Assistant),
            ("tail".into(), "tail".into(), Speaker::User),
        ];
        app.recompute_preview_offsets();
        assert_eq!(app.preview_line_offsets, vec![0, 70_001]);
        app.preview_viewport_height = 10;
        app.scroll_preview_bottom();
        assert!(app.preview_scroll > u16::MAX as usize);
    }

    #[test]
    fn test_sort_by_messages_orders_desc() {
        let mut a = make_session("a", "x", "", "p", "main", vec![]);
        a.message_count = 3;
        let mut b = make_session("b", "y", "", "p", "main", vec![]);
        b.message_count = 9;
        let mut app = App::new(vec![a, b], false, HashSet::new(), empty_cache());
        app.sort_mode = SortMode::Messages;
        app.apply_sort();
        assert_eq!(app.sessions[app.filtered_indices[0]].id, "b");
    }

    #[test]
    fn test_search_matches_changed_file() {
        let mut s = make_session("a", "", "", "p", "main", vec![]);
        s.changed_files_lc = "src/auth.rs".into();
        let mut app = App::new(vec![s], false, HashSet::new(), empty_cache());
        app.search_query = "auth.rs".into();
        app.apply_search();
        assert_eq!(app.filtered_indices.len(), 1);
    }

    #[test]
    fn test_commit_preview_search_keeps_matches_for_n_navigation() {
        let mut app = App::new(
            vec![make_session("a", "x", "", "p", "main", vec![])],
            false,
            HashSet::new(),
            empty_cache(),
        );
        app.preview_lines = vec![
            ("hello world".into(), "hello world".into(), Speaker::User),
            ("nothing".into(), "nothing".into(), Speaker::Assistant),
            ("hello again".into(), "hello again".into(), Speaker::User),
        ];
        app.start_preview_search();
        app.preview_search_query = "hello".into();
        app.update_preview_search();
        assert_eq!(app.preview_search_matches, vec![0, 2]);

        // Enter commits: focus returns to Preview, matches survive, n advances.
        app.commit_preview_search();
        assert_eq!(app.focus, Focus::Preview);
        assert_eq!(app.preview_search_matches, vec![0, 2]);
        app.next_preview_match();
        assert_eq!(app.preview_search_current, 1);
    }

    #[test]
    fn test_esc_in_preview_clears_search_then_exits() {
        let mut app = App::new(
            vec![make_session("a", "x", "", "p", "main", vec![])],
            false,
            HashSet::new(),
            empty_cache(),
        );
        app.preview_lines = vec![("hello".into(), "hello".into(), Speaker::User)];
        app.start_preview_search();
        app.preview_search_query = "hello".into();
        app.update_preview_search();
        app.commit_preview_search();

        // First Esc clears the committed search but stays in the preview…
        app.handle_esc();
        assert_eq!(app.focus, Focus::Preview);
        assert!(!app.preview_search_active());
        // …second Esc returns to the list.
        app.handle_esc();
        assert_eq!(app.focus, Focus::List);
    }

    #[test]
    fn test_esc_in_search_input_cancels_completely() {
        let mut app = App::new(
            vec![make_session("a", "x", "", "p", "main", vec![])],
            false,
            HashSet::new(),
            empty_cache(),
        );
        app.preview_lines = vec![("hello".into(), "hello".into(), Speaker::User)];
        app.start_preview_search();
        app.preview_search_query = "hello".into();
        app.update_preview_search();
        app.exit_preview_search();
        assert_eq!(app.focus, Focus::Preview);
        assert!(!app.preview_search_active());
    }

    #[test]
    fn test_delete_removes_bookmark() {
        let sessions = vec![make_session("a", "", "t", "p", "main", vec![])];
        let mut bookmarks = HashSet::new();
        bookmarks.insert("a".to_string());
        let mut app = App::new(sessions, false, bookmarks, empty_cache());
        app.selected = 0;
        app.cleanup_bookmark_for_deleted("a");
        assert!(!app.bookmarks.contains("a"));
    }
}
