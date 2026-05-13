use crate::session::SessionMeta;
use crate::text_cache::TextCache;
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
    pub lines: Vec<(String, String, bool)>,
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
    pub preview_lines: Vec<(String, String, bool)>,
    pub preview_loading: bool,
    pub preview_session_id: String,
    pub preview_tx: mpsc::Sender<PreviewResult>,
    pub preview_rx: mpsc::Receiver<PreviewResult>,
    pub preview_cache: std::collections::HashMap<String, Vec<(String, String, bool)>>,
    pub preview_cache_order: VecDeque<String>,
    pub confirm_delete: bool,
    pub sort_mode: SortMode,
    pub size_filter: Option<&'static str>,
    pub bookmarks: HashSet<String>,
    pub text_cache: TextCache,
    pub preview_search_query: String,
    pub preview_search_matches: Vec<usize>,
    pub preview_search_current: usize,
    pub view_mode: ViewMode,
    pub status_message: Option<(String, Instant)>,
    pub terminal_height: u16,
    pub preview_inner_width: u16,
    pub preview_line_offsets: Vec<u16>,
}

impl App {
    pub fn new(
        sessions: Vec<SessionMeta>,
        print_mode: bool,
        bookmarks: HashSet<String>,
        text_cache: TextCache,
    ) -> Self {
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
            text_cache,
            preview_search_query: String::new(),
            preview_search_matches: Vec::new(),
            preview_search_current: 0,
            view_mode: ViewMode::Normal,
            status_message: None,
            terminal_height: 40,
            preview_inner_width: 0,
            preview_line_offsets: Vec::new(),
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

    pub fn page_up(&mut self, page_size: usize) {
        if self.filtered_indices.is_empty() {
            return;
        }
        self.selected = self.selected.saturating_sub(page_size);
        self.preview_scroll = 0;
    }

    pub fn page_down(&mut self, page_size: usize) {
        if self.filtered_indices.is_empty() {
            return;
        }
        self.selected = (self.selected + page_size).min(self.filtered_indices.len() - 1);
        self.preview_scroll = 0;
    }

    pub fn scroll_preview_up(&mut self) {
        self.preview_scroll = self.preview_scroll.saturating_sub(3);
    }

    pub fn scroll_preview_down(&mut self) {
        self.preview_scroll = self.preview_scroll.saturating_add(3);
    }

    pub fn scroll_preview_page_up(&mut self, page_size: u16) {
        self.preview_scroll = self.preview_scroll.saturating_sub(page_size);
    }

    pub fn scroll_preview_page_down(&mut self, page_size: u16) {
        self.preview_scroll = self.preview_scroll.saturating_add(page_size);
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
            return;
        }

        use memchr::memmem::Finder;
        use rayon::prelude::*;

        let query_lc = self.search_query.to_lowercase();
        let query_upper = self.search_query.to_ascii_uppercase();
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

    pub fn apply_sort(&mut self) {
        let sessions = &self.sessions;
        let bookmarks = &self.bookmarks;
        let search_active = !self.search_query.is_empty();

        // When search is active, preserve the relevance ordering (score desc,
        // date tiebreak) computed by apply_search_inner. Only float bookmarks.
        if search_active {
            self.filtered_indices.sort_by(|&a, &b| {
                let a_pinned = bookmarks.contains(&sessions[a].id);
                let b_pinned = bookmarks.contains(&sessions[b].id);
                b_pinned.cmp(&a_pinned)
            });
            return;
        }

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
        let query_lc = self.preview_search_query.to_lowercase();
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
            .filter_map(|(i, (_orig, lower, _is_user))| {
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

    fn wrapped_line_count(text: &str, width: usize) -> u16 {
        if width == 0 {
            return 1;
        }
        let mut count = 0_u16;
        let mut remaining = text;
        while let Some((byte_limit, _)) = remaining.char_indices().nth(width) {
            let split_at = remaining[..byte_limit].rfind(' ').unwrap_or(0);
            let split_at = if split_at == 0 { byte_limit } else { split_at };
            count = count.saturating_add(1);
            remaining = remaining[split_at..].trim_start();
        }
        if !remaining.is_empty() {
            count = count.saturating_add(1);
        }
        if count == 0 { 1 } else { count }
    }

    pub fn recompute_preview_offsets(&mut self) {
        let width = self.preview_inner_width.saturating_sub(1) as usize;
        self.preview_line_offsets.clear();
        self.preview_line_offsets.reserve(self.preview_lines.len());
        let mut cursor: u16 = 0;
        for (text, _lower, is_user) in self.preview_lines.iter() {
            self.preview_line_offsets.push(cursor);
            let prefix = if *is_user { "USER: " } else { "ASST: " };
            let full = format!("{}{}", prefix, text);
            let lines = Self::wrapped_line_count(&full, width);
            cursor = cursor.saturating_add(lines).saturating_add(1); // +1 blank separator
        }
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
        let intra = self.intra_match_chunk_offset(line_idx);
        self.preview_scroll = base.saturating_add(intra).saturating_sub(2);
    }

    fn intra_match_chunk_offset(&self, line_idx: usize) -> u16 {
        let (text, _lower, is_user) = match self.preview_lines.get(line_idx) {
            Some(t) => t,
            None => return 0,
        };
        let query_lc = self.preview_search_query.to_lowercase();
        if query_lc.is_empty() {
            return 0;
        }
        let prefix = if *is_user { "USER: " } else { "ASST: " };
        let full = format!("{}{}", prefix, text);
        let width = self.preview_inner_width.saturating_sub(1) as usize;
        if width == 0 {
            return 0;
        }
        let finder = memchr::memmem::Finder::new(query_lc.as_bytes());
        let chunks = wrap_for_offsets(&full, width);
        for (idx, chunk) in chunks.iter().enumerate() {
            if finder.find(chunk.to_lowercase().as_bytes()).is_some() {
                return idx as u16;
            }
        }
        0
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

    pub fn cache_preview(&mut self, session_id: String, lines: Vec<(String, String, bool)>) {
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

fn wrap_for_offsets(text: &str, width: usize) -> Vec<String> {
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
    use std::path::PathBuf;

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
            message_count: None,
            tickets: tickets.into_iter().map(String::from).collect(),
            text_offset: 0,
            text_len: 0,
            name_lc: name.to_lowercase(),
            title_lc: title.to_lowercase(),
            project_lc: project.to_lowercase(),
            branch_lc: branch.to_lowercase(),
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
}
