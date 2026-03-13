use crate::session::SessionMeta;
use std::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Focus {
    List,
    Search,
    Preview,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortMode {
    Date,
    Size,
}

impl SortMode {
    pub fn label(self) -> &'static str {
        match self {
            SortMode::Date => "date",
            SortMode::Size => "size",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppAction {
    None,
    Launch(usize),         // index into filtered sessions
    LaunchDangerously(usize), // --dangerously-skip-permissions
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
    pub preview_lines: Vec<(String, bool)>, // (text, is_user)
    pub preview_loading: bool,
    pub preview_session_id: String,
    pub preview_tx: mpsc::Sender<PreviewResult>,
    pub preview_rx: mpsc::Receiver<PreviewResult>,
    pub preview_cache: std::collections::HashMap<String, Vec<(String, bool)>>,
    pub confirm_delete: bool,
    pub sort_mode: SortMode,
}

impl App {
    pub fn new(sessions: Vec<SessionMeta>, print_mode: bool) -> Self {
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
            confirm_delete: false,
            sort_mode: SortMode::Date,
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

    pub fn apply_search(&mut self) {
        use fuzzy_matcher::skim::SkimMatcherV2;
        use fuzzy_matcher::FuzzyMatcher;

        if self.search_query.is_empty() {
            self.filtered_indices = (0..self.sessions.len()).collect();
            self.apply_sort();
        } else {
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
                    matcher.fuzzy_match(&searchable, query).map(|score| (i, score))
                })
                .collect();
            scored.sort_by(|a, b| b.1.cmp(&a.1));
            self.filtered_indices = scored.into_iter().map(|(i, _)| i).collect();
        }
        self.selected = 0;
        self.preview_scroll = 0;
    }

    pub fn delete_selected(&mut self) {
        if let Some(&real_idx) = self.filtered_indices.get(self.selected) {
            let path = self.sessions[real_idx].file_path.clone();
            if std::fs::remove_file(&path).is_ok() {
                // Also remove companion UUID directory (subagents/, tool-results/)
                let companion_dir = path.with_extension("");
                if companion_dir.is_dir() {
                    std::fs::remove_dir_all(&companion_dir).ok();
                }
                // Remove from sessions and rebuild filtered indices
                self.sessions.remove(real_idx);
                // Fix filtered_indices: remove the entry and adjust indices > real_idx
                self.filtered_indices.retain(|&i| i != real_idx);
                for idx in &mut self.filtered_indices {
                    if *idx > real_idx {
                        *idx -= 1;
                    }
                }
                // Adjust selection
                if self.selected >= self.filtered_indices.len() && self.selected > 0 {
                    self.selected -= 1;
                }
                // Clear preview for deleted session
                self.preview_lines.clear();
                self.preview_session_id.clear();
                self.preview_loading = false;
            }
        }
        self.confirm_delete = false;
    }

    pub fn cycle_sort(&mut self) {
        self.sort_mode = match self.sort_mode {
            SortMode::Date => SortMode::Size,
            SortMode::Size => SortMode::Date,
        };
        self.apply_sort();
        self.selected = 0;
        self.preview_scroll = 0;
    }

    pub fn apply_sort(&mut self) {
        let sessions = &self.sessions;
        match self.sort_mode {
            SortMode::Date => {
                self.filtered_indices
                    .sort_by(|&a, &b| sessions[b].timestamp.cmp(&sessions[a].timestamp));
            }
            SortMode::Size => {
                self.filtered_indices
                    .sort_by(|&a, &b| sessions[b].file_size.cmp(&sessions[a].file_size));
            }
        }
    }

    pub fn handle_esc(&mut self) {
        match self.focus {
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
            Focus::List => {
                self.action = AppAction::Quit;
            }
        }
    }
}
