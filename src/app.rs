use crate::session::SessionMeta;
use std::collections::{HashSet, VecDeque};
use std::sync::mpsc;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Focus {
    List,
    Search,
    Preview,
    PreviewSearch,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortMode {
    Date,
    Size,
    Duration,
}

impl SortMode {
    pub fn label(self) -> &'static str {
        match self {
            SortMode::Date => "date",
            SortMode::Size => "size",
            SortMode::Duration => "duration",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewMode {
    Normal,
    Timeline,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppAction {
    None,
    Launch(usize),
    LaunchDangerously(usize),
    Yank(usize),
    Print(usize),
    Quit,
}

pub struct PreviewResult {
    pub session_id: String,
    pub lines: Vec<(String, bool)>,
    pub message_count: u32,
}

pub struct App {
    pub sessions: Vec<SessionMeta>,
    pub filtered_indices: Vec<usize>,
    pub selected: usize,
    pub preview_scroll: u16,
    pub search_query: String,
    pub focus: Focus,
    pub action: AppAction,
    pub print_mode: bool,
    pub preview_lines: Vec<(String, bool)>,
    pub preview_loading: bool,
    pub preview_session_id: String,
    pub preview_tx: mpsc::Sender<PreviewResult>,
    pub preview_rx: mpsc::Receiver<PreviewResult>,
    pub preview_cache: std::collections::HashMap<String, Vec<(String, bool)>>,
    pub preview_cache_order: VecDeque<String>,
    pub confirm_delete: bool,
    pub sort_mode: SortMode,
    pub size_filter: Option<&'static str>,
    pub bookmarks: HashSet<String>,
    pub preview_search_query: String,
    pub preview_search_matches: Vec<usize>,
    pub preview_search_current: usize,
    pub view_mode: ViewMode,
    pub status_message: Option<(String, Instant)>,
}

impl App {
    pub fn new(sessions: Vec<SessionMeta>, print_mode: bool, bookmarks: HashSet<String>) -> Self {
        let filtered_indices: Vec<usize> = (0..sessions.len()).collect();
        let (preview_tx, preview_rx) = mpsc::channel();
        Self {
            sessions,
            filtered_indices,
            selected: 0,
            preview_scroll: 0,
            search_query: String::new(),
            focus: Focus::List,
            action: AppAction::None,
            print_mode,
            preview_lines: Vec::new(),
            preview_loading: false,
            preview_session_id: String::new(),
            preview_tx,
            preview_rx,
            preview_cache: std::collections::HashMap::new(),
            preview_cache_order: VecDeque::new(),
            confirm_delete: false,
            sort_mode: SortMode::Date,
            size_filter: None,
            bookmarks,
            preview_search_query: String::new(),
            preview_search_matches: Vec::new(),
            preview_search_current: 0,
            view_mode: ViewMode::Normal,
            status_message: None,
        }
    }

    pub fn selected_session(&self) -> Option<&SessionMeta> {
        self.filtered_indices
            .get(self.selected)
            .and_then(|&idx| self.sessions.get(idx))
    }

    pub fn move_up(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        if self.selected > 0 {
            self.selected -= 1;
        } else {
            self.selected = self.filtered_indices.len() - 1;
        }
        self.preview_scroll = 0;
    }

    pub fn move_down(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        if self.selected + 1 < self.filtered_indices.len() {
            self.selected += 1;
        } else {
            self.selected = 0;
        }
        self.preview_scroll = 0;
    }

    pub fn scroll_preview_up(&mut self) {
        self.preview_scroll = self.preview_scroll.saturating_sub(3);
    }

    pub fn scroll_preview_down(&mut self) {
        self.preview_scroll = self.preview_scroll.saturating_add(3);
    }

    /// Rebuild filtered_indices from scratch: search → size filter → sort.
    pub fn rebuild_view(&mut self) {
        self.apply_search_inner();
        self.apply_size_filter();
        self.apply_sort();
        self.selected = 0;
        self.preview_scroll = 0;
    }

    fn apply_search_inner(&mut self) {
        if self.search_query.is_empty() {
            self.filtered_indices = (0..self.sessions.len()).collect();
        } else {
            use fuzzy_matcher::skim::SkimMatcherV2;
            use fuzzy_matcher::FuzzyMatcher;
            let matcher = SkimMatcherV2::default();
            let query = &self.search_query;
            let mut scored: Vec<(usize, i64)> = self
                .sessions
                .iter()
                .enumerate()
                .filter_map(|(i, s)| {
                    let searchable = format!(
                        "{} {} {} {} {}",
                        s.project, s.branch, s.title, s.last_message, s.name
                    );
                    matcher
                        .fuzzy_match(&searchable, query)
                        .map(|score| (i, score))
                })
                .collect();
            scored.sort_by(|a, b| b.1.cmp(&a.1));
            self.filtered_indices = scored.into_iter().map(|(i, _)| i).collect();
        }
    }

    fn apply_size_filter(&mut self) {
        if let Some(category) = self.size_filter {
            let sessions = &self.sessions;
            self.filtered_indices
                .retain(|&i| crate::session::size_category(sessions[i].file_size) == category);
        }
    }

    pub fn apply_sort(&mut self) {
        let sessions = &self.sessions;
        let bookmarks = &self.bookmarks;

        // Primary: bookmarked first. Secondary: current sort mode.
        self.filtered_indices.sort_by(|&a, &b| {
            let a_pinned = bookmarks.contains(&sessions[a].id);
            let b_pinned = bookmarks.contains(&sessions[b].id);
            b_pinned
                .cmp(&a_pinned)
                .then_with(|| match self.sort_mode {
                    SortMode::Date => sessions[b].timestamp.cmp(&sessions[a].timestamp),
                    SortMode::Size => sessions[b].file_size.cmp(&sessions[a].file_size),
                    SortMode::Duration => sessions[b].duration_secs.cmp(&sessions[a].duration_secs),
                })
        });
    }

    /// Called when search query changes.
    pub fn apply_search(&mut self) {
        self.rebuild_view();
    }

    pub fn cycle_sort(&mut self) {
        self.sort_mode = match self.sort_mode {
            SortMode::Date => SortMode::Size,
            SortMode::Size => SortMode::Duration,
            SortMode::Duration => SortMode::Date,
        };
        self.apply_sort();
        self.selected = 0;
        self.preview_scroll = 0;
    }

    pub fn toggle_size_filter(&mut self, category: &'static str) {
        if self.size_filter == Some(category) {
            self.size_filter = None;
        } else {
            self.size_filter = Some(category);
        }
        self.rebuild_view();
    }

    pub fn clear_size_filter(&mut self) {
        self.size_filter = None;
        self.rebuild_view();
    }

    pub fn toggle_bookmark(&mut self) {
        if let Some(session) = self.selected_session() {
            let id = session.id.clone();
            if self.bookmarks.contains(&id) {
                self.bookmarks.remove(&id);
            } else {
                self.bookmarks.insert(id);
            }
            crate::bookmarks::save_bookmarks(&self.bookmarks);
            // Re-sort to float bookmarks to top
            self.apply_sort();
        }
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

    pub fn toggle_timeline(&mut self) {
        self.view_mode = match self.view_mode {
            ViewMode::Normal => ViewMode::Timeline,
            ViewMode::Timeline => ViewMode::Normal,
        };
    }

    // Preview search

    pub fn start_preview_search(&mut self) {
        self.focus = Focus::PreviewSearch;
        self.preview_search_query.clear();
        self.preview_search_matches.clear();
        self.preview_search_current = 0;
    }

    pub fn update_preview_search(&mut self) {
        let query = self.preview_search_query.to_lowercase();
        if query.is_empty() {
            self.preview_search_matches.clear();
            self.preview_search_current = 0;
            return;
        }
        self.preview_search_matches = self
            .preview_lines
            .iter()
            .enumerate()
            .filter(|(_, (text, _))| text.to_lowercase().contains(&query))
            .map(|(i, _)| i)
            .collect();
        self.preview_search_current = 0;
        // Scroll to first match
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

    fn scroll_to_current_match(&mut self) {
        if let Some(&line_idx) = self.preview_search_matches.get(self.preview_search_current) {
            // Estimate display line: each message ≈ 2 display lines + 1 blank
            let estimated_line = (line_idx * 3) as u16;
            self.preview_scroll = estimated_line.saturating_sub(2);
        }
    }

    pub fn exit_preview_search(&mut self) {
        self.focus = Focus::Preview;
        self.preview_search_query.clear();
        self.preview_search_matches.clear();
        self.preview_search_current = 0;
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

    pub fn cache_preview(&mut self, session_id: String, lines: Vec<(String, bool)>) {
        if self.preview_cache.contains_key(&session_id) {
            *self.preview_cache.get_mut(&session_id).unwrap() = lines;
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
        if let Some(&real_idx) = self.filtered_indices.get(self.selected) {
            let path = self.sessions[real_idx].file_path.clone();
            if std::fs::remove_file(&path).is_ok() {
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
                self.preview_lines.clear();
                self.preview_session_id.clear();
                self.preview_loading = false;
            }
        }
        self.confirm_delete = false;
    }

    pub fn handle_esc(&mut self) {
        match self.focus {
            Focus::PreviewSearch => {
                self.exit_preview_search();
            }
            Focus::Search if !self.search_query.is_empty() => {
                self.search_query.clear();
                self.apply_search();
            }
            Focus::Search => {
                self.focus = Focus::List;
            }
            Focus::Preview => {
                self.focus = Focus::List;
            }
            Focus::List if self.view_mode == ViewMode::Timeline => {
                self.view_mode = ViewMode::Normal;
            }
            Focus::List => {
                self.action = AppAction::Quit;
            }
        }
    }
}
