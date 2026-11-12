use crate::as_written::Piece;
use crate::input::TextInput;
use crate::parser::Speaker;
use crate::session::SessionMeta;
use crate::text_cache::TextCache;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
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

/// How much of each session the list shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Density {
    /// Three lines: headline, stats + first message, where you left off.
    Comfortable,
    /// One line per session.
    Compact,
}

impl Density {
    /// Screen rows per session, including the gap after it.
    pub fn rows(self) -> usize {
        match self {
            Density::Comfortable => 4,
            Density::Compact => 1,
        }
    }
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

/// What to do once the TUI closes. The payload is the session's index in
/// `App::sessions`, which (unlike its row) doesn't move when the view does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppAction {
    None,
    Launch(usize),
    LaunchDangerously(usize),
    /// Resume into a new session ID (`--fork-session`), leaving the original as is.
    Fork(usize),
    Print(usize),
    Quit,
}

/// Where a session's search hits are: the text around them, for the list
/// row's `└ match:` line.
#[derive(Debug, Clone, PartialEq)]
pub struct Snippet {
    /// The hit is in a changed file's path rather than the conversation.
    pub in_file: bool,
    /// The places shown: one when the search words occur together, up to
    /// `MAX_SNIPPET_PARTS` when they're far apart.
    pub parts: Vec<SnippetPart>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SnippetPart {
    /// The text around the hit, on one line with whitespace runs folded.
    pub text: String,
    /// Char offset of the first hit within `text`.
    pub hit: usize,
    /// Chars from `hit` to the end of the last search word found with it.
    pub span: usize,
    /// The text as the search text has it (lowercased), before cleaning.
    pub source: Piece,
    /// How far into the session's search text it is (0.0–1.0).
    pub at: f64,
    /// `text` is as written in the transcript, not lowercased.
    pub as_written: bool,
}

impl SnippetPart {
    /// The part showing `piece`: on one line, control chars dropped,
    /// whitespace runs folded.
    fn new(piece: Piece, at: f64, as_written: bool) -> Self {
        let clean = |s: &str| fold_whitespace(&crate::parser::sanitize_line(s));
        let (hit_from, hit_to) = piece.hit;
        let before = clean(&piece.text[..hit_from]);
        let before = before.trim_start();
        let hit = clean(&piece.text[hit_from..hit_to]);
        let after = clean(&piece.text[hit_to..]);
        SnippetPart {
            hit: before.chars().count(),
            span: hit.chars().count(),
            text: format!("{}{}{}", before, hit, after.trim_end()),
            source: piece,
            at,
            as_written,
        }
    }
}

const MAX_SNIPPET_PARTS: usize = 3;

/// A delete that `u` can still undo.
pub struct DeletedSession {
    pub meta: SessionMeta,
    pub pinned: bool,
    /// Trash folder holding the transcript.
    pub slot: PathBuf,
}

/// A conversation as the preview shows it: (text, lowercased, speaker) per
/// message. Shared, not copied: a big session's is tens of MB.
pub type PreviewLines = Arc<Vec<(String, String, Speaker)>>;

/// A loaded conversation kept for when its session is selected again, with
/// its row layout for the last width it was shown at.
pub struct CachedPreview {
    pub lines: PreviewLines,
    /// (body width, first row of each message, total rows).
    layout: Option<(usize, Vec<usize>, usize)>,
}

/// The preview cache's budget: this many conversations, or this many bytes
/// of text, whichever comes first.
const PREVIEW_CACHE_ENTRIES: usize = 10;
const PREVIEW_CACHE_BYTES: usize = 256 << 20;

/// Where to put the preview when its session's lines load again (`T`).
pub struct PreviewKeep {
    pub session: String,
    /// The message that was at the top.
    pub top: Option<(String, Speaker)>,
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
    /// Per session (indexed like `sessions`): the last search found the whole
    /// query as whole words. Exact matches list above partial ones. Empty
    /// while not searching.
    pub exact_matches: Vec<bool>,
    pub focus: Focus,
    pub action: AppAction,
    pub print_mode: bool,
    /// What Enter does: resume with `--dangerously-skip-permissions` (true)
    /// or a plain resume.
    pub enter_yolo: bool,
    pub preview_lines: PreviewLines,
    pub preview_loading: bool,
    pub preview_session_id: String,
    pub preview_req_tx: mpsc::Sender<crate::preview::PreviewRequest>,
    pub preview_rx: mpsc::Receiver<PreviewResult>,
    pub preview_cache: std::collections::HashMap<String, CachedPreview>,
    pub preview_cache_order: VecDeque<String>,
    pub confirm_delete: bool,
    /// The session (ID) an open delete prompt or rename is for. Checked when
    /// it's confirmed, so a list that changed meanwhile can't redirect it.
    pub prompt_target: Option<String>,
    /// The open prompt has been drawn. A `y` that arrives before (in the
    /// same burst of keys as the `d`, e.g. pasted text) doesn't confirm it.
    pub prompt_seen: bool,
    /// Which sessions are open changed while the view was frozen (a prompt
    /// or rename open); `is:open` catches up once it closes.
    pub live_stale: bool,
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
    /// Set by `T`: the reload of this session keeps its find and place.
    pub preview_keep: Option<PreviewKeep>,
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
    /// First visible row of the help overlay.
    pub help_scroll: usize,
    pub density: Density,
    /// Show only the focused pane, full width.
    pub zoom: bool,
    /// Sessions open in a running Claude Code, by session ID.
    pub live: HashMap<String, crate::live::LiveSession>,
    /// Where Claude Code's per-process files live; `None` disables live
    /// detection (tests).
    pub live_dir: Option<PathBuf>,
    /// A resume waiting for `y` because the session is open elsewhere.
    pub confirm_resume: Option<AppAction>,
    /// Where deleted sessions go so they can be restored; `None` deletes
    /// for good (tests).
    pub trash_dir: Option<PathBuf>,
    /// Deletes `u` can undo, most recent last.
    pub undo_stack: Vec<DeletedSession>,
    /// Per session (real index): the hit snippet for the current search,
    /// computed when the row is first drawn.
    pub snippets: HashMap<usize, Option<Snippet>>,
    /// Counts searches, so lookups of a snippet's original text that
    /// finish after the next search are dropped.
    pub snippet_generation: Arc<AtomicU64>,
    /// The background lookup of snippets' original text; `None` shows them
    /// lowercased (tests).
    pub as_written: Option<(
        mpsc::Sender<crate::as_written::Request>,
        mpsc::Receiver<crate::as_written::Found>,
    )>,
    /// The timeline's selected day (`None`: today).
    pub timeline_day: Option<chrono::NaiveDate>,
    /// First day the timeline shows (set by the renderer each frame).
    pub timeline_start: Option<chrono::NaiveDate>,
    /// Where each timeline day was drawn, for mouse clicks: (x, y, day).
    pub timeline_cells: Vec<(u16, u16, chrono::NaiveDate)>,
    /// Mouse support is on (config `mouse`, `--mouse`).
    pub mouse: bool,
    /// Where the panes were drawn last frame (empty when not shown), and
    /// which session each list row belongs to: (y, visual index).
    pub search_area: ratatui::layout::Rect,
    pub list_area: ratatui::layout::Rect,
    pub list_rows: Vec<(u16, usize)>,
    pub preview_area: ratatui::layout::Rect,
    /// The last click on a list row, to spot a double-click.
    pub last_click: Option<(Instant, usize)>,
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
            exact_matches: Vec::new(),
            focus: Focus::List,
            action: AppAction::None,
            print_mode,
            enter_yolo: true,
            preview_lines: Default::default(),
            preview_loading: false,
            preview_session_id: String::new(),
            preview_req_tx,
            preview_rx,
            preview_cache: std::collections::HashMap::new(),
            preview_cache_order: VecDeque::new(),
            confirm_delete: false,
            prompt_target: None,
            prompt_seen: false,
            live_stale: false,
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
            preview_keep: None,
            preview_total_rows: 0,
            preview_viewport_height: 0,
            show_tools: false,
            show_files: false,
            show_help: false,
            help_scroll: 0,
            density: Density::Comfortable,
            zoom: false,
            live: HashMap::new(),
            live_dir: None,
            confirm_resume: None,
            trash_dir: None,
            undo_stack: Vec::new(),
            snippets: HashMap::new(),
            snippet_generation: Arc::new(AtomicU64::new(0)),
            as_written: None,
            timeline_day: None,
            timeline_start: None,
            timeline_cells: Vec::new(),
            mouse: false,
            search_area: Default::default(),
            list_area: Default::default(),
            list_rows: Vec::new(),
            preview_area: Default::default(),
            last_click: None,
        }
    }

    pub fn toggle_tools(&mut self) {
        self.show_tools = !self.show_tools;
        // The same conversation comes back with tool lines added or gone:
        // return to the same message, with the same find.
        let top = self.preview_place().and_then(|(line, _)| {
            self.preview_lines[line..]
                .iter()
                .find(|(_, _, sp)| !matches!(sp, Speaker::Tool | Speaker::Day))
                .map(|(text, _, sp)| (text.clone(), *sp))
        });
        self.preview_keep = Some(PreviewKeep {
            session: self.preview_session_id.clone(),
            top,
        });
        // Cached previews were built under the previous setting; drop them so
        // the current (and future) sessions re-extract with/without tool lines.
        self.preview_cache.clear();
        self.preview_cache_order.clear();
        self.preview_lines = Default::default();
        self.preview_search_matches.clear();
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

    /// One step through the list without wrapping (mouse wheel).
    pub fn scroll_list(&mut self, down: bool) {
        let last = self.filtered_indices.len().saturating_sub(1);
        let target = if down {
            (self.selected + 1).min(last)
        } else {
            self.selected.saturating_sub(1)
        };
        self.select(target);
    }

    /// Sessions opened or closed elsewhere: an `is:open` search must follow.
    /// A prompt or rename is open, aimed at the selected session: nothing
    /// may reshuffle the list under it.
    pub fn view_frozen(&self) -> bool {
        self.confirm_delete || self.confirm_resume.is_some() || self.focus == Focus::Rename
    }

    pub fn live_changed(&mut self) {
        if self.view_frozen() {
            self.live_stale = true;
            return;
        }
        self.live_stale = false;
        let filters_on_open = self
            .query()
            .filters
            .contains(&crate::query::Filter::Open);
        if filters_on_open {
            self.rebuild_view_keep_selection();
        }
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

    /// Launched somewhere without sessions of its own (a new repo, /tmp): an
    /// empty list helps nobody, so show every project instead.
    pub fn widen_scope_if_empty(&mut self) {
        if self.scope == Scope::Current
            && self.filtered_indices.is_empty()
            && !self.sessions.is_empty()
        {
            self.scope = Scope::All;
            self.rebuild_view();
            self.set_status(
                "No sessions for this directory yet — showing all projects (a toggles)"
                    .to_string(),
            );
        }
    }

    pub fn toggle_scope(&mut self) {
        self.scope = match self.scope {
            Scope::Current => Scope::All,
            Scope::All => Scope::Current,
        };
        self.rebuild_view_keep_selection();
    }

    /// The search box parsed into text and `key:value` filters.
    pub fn query(&self) -> crate::query::Query {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        crate::query::parse(self.search_query.text(), now)
    }

    /// The search has words to match (not only filters): results are ranked
    /// by relevance and their hits marked.
    pub fn text_search_active(&self) -> bool {
        !self.query().text.is_empty()
    }

    fn apply_search_inner(&mut self) {
        self.exact_matches.clear();
        self.snippets.clear();
        self.snippet_generation.fetch_add(1, Ordering::Relaxed);
        let query = self.query();
        self.apply_text_search(&query.text);
        if !query.filters.is_empty() {
            let keep: Vec<bool> = self
                .sessions
                .iter()
                .map(|s| {
                    let pinned = self.bookmarks.contains(&s.id);
                    let open = self.live.contains_key(&s.id);
                    query.filters.iter().all(|f| f.matches(s, pinned, open))
                })
                .collect();
            self.filtered_indices.retain(|&i| keep[i]);
        }
    }

    /// Rank sessions against the search words (`query_lc`, lowercased);
    /// everything when there are none.
    fn apply_text_search(&mut self, query_lc: &str) {
        use memchr::memmem::Finder;
        use rayon::prelude::*;

        let query_upper = query_lc.trim().to_ascii_uppercase();
        let is_ticket_form = is_ticket_query(&query_upper);
        let tokens: Vec<&str> = query_lc.split_whitespace().collect();
        if tokens.is_empty() {
            self.filtered_indices = (0..self.sessions.len()).collect();
            return;
        }
        let finders: Vec<Finder> = tokens.iter().map(|t| Finder::new(t.as_bytes())).collect();
        let phrase = tokens.join(" ");
        let phrase_finder = Finder::new(phrase.as_bytes());

        let text_cache = &self.text_cache;

        let ticket_matched: Vec<bool> = self
            .sessions
            .iter()
            .map(|s| is_ticket_form && s.tickets.binary_search(&query_upper).is_ok())
            .collect();

        let mut scored: Vec<(usize, i64, bool)> = self
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
                let exact = ticket_hit
                    || [
                        &s.name_lc,
                        &s.title_lc,
                        &s.project_lc,
                        &s.branch_lc,
                        &s.changed_files_lc,
                    ]
                    .iter()
                    .any(|field| contains_whole_words(field.as_bytes(), &phrase_finder))
                    || contains_whole_words(
                        text_cache.slice(s.text_offset, s.text_len),
                        &phrase_finder,
                    );
                Some((i, score, exact))
            })
            .collect();

        // exact first, then score desc, timestamp desc tiebreak
        scored.sort_by(|a, b| {
            b.2.cmp(&a.2).then_with(|| b.1.cmp(&a.1)).then_with(|| {
                self.sessions[b.0]
                    .timestamp
                    .cmp(&self.sessions[a.0].timestamp)
            })
        });

        self.exact_matches = vec![false; self.sessions.len()];
        for &(i, _, exact) in &scored {
            self.exact_matches[i] = exact;
        }
        self.filtered_indices = scored.into_iter().map(|(i, _, _)| i).collect();
    }

    /// Whether the last search matched session `real_idx` exactly.
    pub fn is_exact_match(&self, real_idx: usize) -> bool {
        self.exact_matches.get(real_idx).copied().unwrap_or(false)
    }

    /// Number of listed sessions that match the search exactly.
    pub fn exact_match_count(&self) -> usize {
        self.filtered_indices
            .iter()
            .filter(|&&i| self.is_exact_match(i))
            .count()
    }

    /// The search words as the exact tier compares them: lowercased, with
    /// whitespace runs folded to single spaces, filters left out.
    pub fn search_phrase(&self) -> String {
        self.query().text
    }

    fn apply_size_filter(&mut self) {
        if let Some(category) = self.size_filter {
            let sessions = &self.sessions;
            self.filtered_indices
                .retain(|&i| crate::session::size_category(sessions[i].file_size) == category);
        }
    }

    /// Order the view: exact search matches first, then bookmarks, then the
    /// active sort. While a search is active and no explicit sort was chosen
    /// (`search_sort` is `None`), the relevance order from
    /// `apply_search_inner` is kept — the sort is stable, so this relies on
    /// `filtered_indices` already being in score order (i.e. the search ran
    /// since the last query change).
    pub fn apply_sort(&mut self) {
        let sessions = &self.sessions;
        let bookmarks = &self.bookmarks;
        let exact = |i: usize| self.exact_matches.get(i).copied().unwrap_or(false);
        // Relevance needs words; filters alone keep the browse sort.
        let mode = if self.text_search_active() {
            self.search_sort
        } else {
            Some(self.sort_mode)
        };

        self.filtered_indices.sort_by(|&a, &b| {
            let a_pinned = bookmarks.contains(&sessions[a].id);
            let b_pinned = bookmarks.contains(&sessions[b].id);
            exact(b)
                .cmp(&exact(a))
                .then(b_pinned.cmp(&a_pinned))
                .then_with(|| match mode {
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
        if self.text_search_active() {
            self.search_sort.map(SortMode::label).unwrap_or("relevance")
        } else {
            self.sort_mode.label()
        }
    }

    /// Called when search query changes.
    pub fn apply_search(&mut self) {
        self.search_dirty = false;
        self.rebuild_view();
        // Still previewing the same session: mark the new words. (Otherwise
        // the lines belong to another session; its load does this.)
        if self.selected_session().is_some_and(|s| s.id == self.preview_session_id) {
            self.refresh_preview_matches(true);
        }
    }

    /// Cycle the sort. Search results cycle relevance → date → … → messages
    /// → relevance; the browse sort is remembered separately.
    pub fn cycle_sort(&mut self) {
        if !self.text_search_active() {
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
        let pinned = !self.bookmarks.contains(&id);
        self.change_bookmarks(|set| {
            if pinned {
                set.insert(id.clone());
            } else {
                set.remove(&id);
            }
        });
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
        self.prompt_target = self.selected_id();
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
        let target = self.prompt_target.take();
        let Some(real) = target.and_then(|id| self.sessions.iter().position(|s| s.id == id)) else {
            self.set_status("That session is gone; nothing renamed".to_string());
            return;
        };
        if name.is_empty() || name == self.sessions[real].name {
            return;
        }
        let session = &self.sessions[real];
        match crate::session::write_custom_title(&session.file_path, &session.id, &name) {
            Ok(()) => {
                let open = self.live_status(&self.sessions[real]).is_some();
                let session = &mut self.sessions[real];
                session.name_lc = crate::fold::fold(&name);
                session.name = name.clone();
                // Claude Code rewrites the title it has in memory after its
                // turns, so a rename from outside may not last.
                self.set_status(if open {
                    format!("Renamed → {} (it's open in Claude Code, which may name it back; /rename there sticks)", name)
                } else {
                    format!("Renamed → {}", name)
                });
            }
            Err(e) => self.set_status(format!("Rename failed: {}", e)),
        }
    }

    /// The timeline's selected day.
    pub fn timeline_selected(&self) -> chrono::NaiveDate {
        self.timeline_day
            .unwrap_or_else(|| chrono::Local::now().date_naive())
    }

    /// Move the timeline cursor by `days`, staying between the first day
    /// shown and today.
    pub fn timeline_move(&mut self, days: i64) {
        let today = chrono::Local::now().date_naive();
        let mut day = self.timeline_selected() + chrono::Duration::days(days);
        if day > today {
            day = today;
        }
        if let Some(start) = self.timeline_start {
            if day < start {
                day = start;
            }
        }
        self.timeline_day = Some(day);
    }

    /// List the sessions of the timeline's selected day (an `on:` search).
    /// List the timeline's selected day: an `on:` filter in place of any
    /// date filter the search had, its words and other filters kept.
    pub fn pick_timeline_day(&mut self) {
        let day = self.timeline_selected();
        let mut query: Vec<String> = self
            .search_query
            .text()
            .split_whitespace()
            .filter(|t| !crate::query::is_date_filter(t))
            .map(String::from)
            .collect();
        query.push(format!("on:{}", day.format("%Y-%m-%d")));
        self.search_query = TextInput::from(query.join(" ").as_str());
        self.view_mode = ViewMode::Normal;
        self.focus = Focus::List;
        self.apply_search();
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
        self.refresh_preview_matches(true);
    }

    /// What the preview highlights and `n`/`N` walk: the find-in-conversation
    /// query when one is typed, otherwise the words of the session search, so
    /// a search result opens with its hits marked.
    pub fn preview_terms(&self) -> Vec<String> {
        let find = crate::fold::fold(self.preview_search_query.text());
        if !find.is_empty() {
            return vec![find];
        }
        self.query().text.split_whitespace().map(String::from).collect()
    }

    /// Recompute which preview messages contain a term; with `scroll`, move
    /// to the first of them.
    pub fn refresh_preview_matches(&mut self, scroll: bool) {
        let terms = self.preview_terms();
        self.preview_search_current = 0;
        if terms.is_empty() {
            self.preview_search_matches.clear();
            return;
        }
        let finders: Vec<memchr::memmem::Finder> = terms
            .iter()
            .map(|t| memchr::memmem::Finder::new(t.as_bytes()))
            .collect();
        self.preview_search_matches = self
            .preview_lines
            .iter()
            .enumerate()
            .filter(|(_, (_orig, lower, speaker))| {
                *speaker != Speaker::Day
                    && finders.iter().any(|f| f.find(lower.as_bytes()).is_some())
            })
            .map(|(i, _)| i)
            .collect();
        // A session search opens at the first message with all its words
        // (where the list's match line points), passing over the recap
        // pinned at the top; `N` still goes back to earlier hits.
        if self.preview_search_query.is_empty() {
            let lines = &self.preview_lines;
            let has_all = |i: usize| finders.iter().all(|f| f.find(lines[i].1.as_bytes()).is_some());
            let in_conversation = |i: usize| lines[i].2 != Speaker::Recap;
            let matches = &self.preview_search_matches;
            self.preview_search_current = matches
                .iter()
                .position(|&i| in_conversation(i) && has_all(i))
                .or_else(|| matches.iter().position(|&i| in_conversation(i)))
                .unwrap_or(0);
        }
        if scroll {
            self.scroll_to_current_match();
        }
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
        // Laid out at this width before (selected again): no re-wrapping,
        // which for a big session is most of the cost of showing it.
        let cached = self
            .preview_cache
            .get(&self.preview_session_id)
            .filter(|c| Arc::ptr_eq(&c.lines, &self.preview_lines));
        if let Some((_, offsets, total)) = cached.and_then(|c| c.layout.as_ref()).filter(|l| l.0 == width) {
            self.preview_line_offsets.clone_from(offsets);
            self.preview_total_rows = *total;
            return;
        }
        self.preview_line_offsets.clear();
        self.preview_line_offsets.reserve(self.preview_lines.len());
        let mut cursor: usize = 0;
        for (text, _lower, speaker) in self.preview_lines.iter() {
            self.preview_line_offsets.push(cursor);
            // A day rule is always one row, however narrow the pane.
            let rows = if *speaker == Speaker::Day {
                1
            } else {
                crate::ui::message_rows(text, width).len()
            };
            cursor += rows + 1; // +1 blank separator
        }
        self.preview_total_rows = cursor;
        if let Some(c) = self.preview_cache.get_mut(&self.preview_session_id) {
            if Arc::ptr_eq(&c.lines, &self.preview_lines) {
                c.layout = Some((width, self.preview_line_offsets.clone(), cursor));
            }
        }
    }

    /// The message at the top of the preview, and how far down it the view
    /// starts (0.0–1.0).
    pub fn preview_place(&self) -> Option<(usize, f64)> {
        if self.preview_line_offsets.is_empty() {
            return None;
        }
        let line = self
            .preview_line_offsets
            .partition_point(|&o| o <= self.preview_scroll)
            .saturating_sub(1);
        let (start, end) = self.preview_line_rows(line);
        let into = self.preview_scroll.saturating_sub(start) as f64;
        Some((line, (into / (end - start).max(1) as f64).min(1.0)))
    }

    /// Wrapped rows `start..end` of preview message `line` (its gap row
    /// included).
    fn preview_line_rows(&self, line: usize) -> (usize, usize) {
        let start = self.preview_line_offsets.get(line).copied().unwrap_or(0);
        let end = self
            .preview_line_offsets
            .get(line + 1)
            .copied()
            .unwrap_or(self.preview_total_rows);
        (start, end.max(start))
    }

    fn restore_preview_place(&mut self, (line, into): (usize, f64)) {
        let (start, end) = self.preview_line_rows(line);
        let row = start + ((end - start) as f64 * into) as usize;
        self.preview_scroll = row.min(self.max_preview_scroll());
    }

    /// The preview pane changed width: re-wrap, keeping the current match
    /// in view if it was, else the message at the top.
    pub fn rewrap_preview(&mut self) {
        let match_line = self.preview_search_matches.get(self.preview_search_current).copied();
        let match_shown = match_line.is_some_and(|line| {
            let (start, end) = self.preview_line_rows(line);
            start < self.preview_scroll + self.preview_viewport_height.max(1)
                && end > self.preview_scroll
        });
        let place = self.preview_place();
        self.recompute_preview_offsets();
        if match_shown {
            self.scroll_to_current_match();
        } else if let Some(place) = place {
            self.restore_preview_place(place);
        }
    }

    /// The selected session's lines are in `preview_lines`: lay them out,
    /// and open at the first search hit, or (after `T`) where you were.
    pub fn preview_loaded(&mut self) {
        self.recompute_preview_offsets();
        let keep = self.preview_keep.take_if(|k| k.session == self.preview_session_id);
        let Some(keep) = keep else {
            // While searching, open at the first hit.
            self.refresh_preview_matches(true);
            return;
        };
        let line = keep.top.and_then(|(text, speaker)| {
            self.preview_lines
                .iter()
                .position(|(t, _, sp)| *sp == speaker && *t == text)
        });
        self.refresh_preview_matches(false);
        if let Some(line) = line {
            self.restore_preview_place((line, 0.0));
            // Matches walk on from here.
            if let Some(i) = self.preview_search_matches.iter().position(|&m| m >= line) {
                self.preview_search_current = i;
            }
        } else {
            self.scroll_to_current_match();
        }
    }

    fn scroll_to_current_match(&mut self) {
        // The files view scrolls its own list, not conversation rows.
        if self.show_files {
            return;
        }
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
        let Some((text, _lower, _speaker)) = self.preview_lines.get(line_idx) else {
            return 0;
        };
        let terms = self.preview_terms();
        let terms: Vec<&str> = terms.iter().map(String::as_str).collect();
        crate::ui::message_rows_marked(text, self.preview_body_width(), &terms)
            .iter()
            .position(|(_, marks)| !marks.is_empty())
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

    /// Drop the find-in-conversation query without touching focus or the
    /// scroll position. The session search's words (if any) take over the
    /// highlighting again.
    pub fn clear_preview_search(&mut self) {
        self.preview_search_query.clear();
        self.refresh_preview_matches(false);
    }

    /// A find-in-conversation query is in effect (typed or committed).
    pub fn preview_search_active(&self) -> bool {
        !self.preview_search_query.is_empty()
    }

    /// Scroll the preview to the next (`forward`) or previous message you
    /// wrote, so long sessions can be skimmed prompt by prompt.
    pub fn jump_to_prompt(&mut self, forward: bool) {
        if self.show_files {
            return;
        }
        let user_rows = self
            .preview_lines
            .iter()
            .zip(&self.preview_line_offsets)
            .filter(|((_, _, speaker), _)| *speaker == Speaker::User)
            .map(|(_, &offset)| offset);
        let user_rows: Vec<usize> = user_rows.collect();
        let target = if forward {
            user_rows.iter().copied().find(|&o| o > self.preview_scroll)
        } else {
            user_rows.iter().copied().rfind(|&o| o < self.preview_scroll)
        };
        match target {
            Some(row) => self.preview_scroll = row.min(self.max_preview_scroll()),
            None if forward => self.preview_scroll = self.max_preview_scroll(),
            None => self.preview_scroll = 0,
        }
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

    pub fn cache_preview(&mut self, session_id: String, lines: PreviewLines) {
        self.preview_cache_order.retain(|id| *id != session_id);
        self.preview_cache_order.push_back(session_id.clone());
        self.preview_cache.insert(session_id, CachedPreview { lines, layout: None });
        self.trim_preview_cache(PREVIEW_CACHE_ENTRIES, PREVIEW_CACHE_BYTES);
    }

    /// Drop the oldest cached conversations until the cache is within
    /// budget, keeping at least the newest.
    fn trim_preview_cache(&mut self, max_entries: usize, max_bytes: usize) {
        let size = |c: &CachedPreview| c.lines.iter().map(|(t, l, _)| t.len() + l.len()).sum::<usize>();
        let mut bytes: usize = self.preview_cache.values().map(size).sum();
        while self.preview_cache_order.len() > 1
            && (self.preview_cache_order.len() > max_entries || bytes > max_bytes)
        {
            let Some(oldest) = self.preview_cache_order.pop_front() else {
                break;
            };
            if let Some(gone) = self.preview_cache.remove(&oldest) {
                bytes -= size(&gone);
            }
        }
    }

    /// Ask before deleting the selected session.
    pub fn open_delete_prompt(&mut self) {
        if self.selected_session().is_some() {
            self.confirm_delete = true;
            self.prompt_target = self.selected_id();
            self.prompt_seen = false;
        }
    }

    /// The delete prompt was answered `y`: delete the session it named, if
    /// that's still the one selected.
    pub fn confirm_delete_prompt(&mut self) {
        self.confirm_delete = false;
        let target = self.prompt_target.take();
        if !self.prompt_seen {
            return;
        }
        if target.is_some() && target == self.selected_id() {
            self.delete_selected();
        } else {
            self.set_status("The list changed; nothing deleted".to_string());
        }
    }

    pub fn delete_selected(&mut self) {
        self.confirm_delete = false;
        let Some(&real_idx) = self.filtered_indices.get(self.selected) else {
            return;
        };
        let id = self.sessions[real_idx].id.clone();
        let path = self.sessions[real_idx].file_path.clone();
        // Into the trash when possible; when it can't take the file (another
        // disk, no cache dir), delete for good as before.
        let trashed = self
            .trash_dir
            .as_ref()
            .map(|root| crate::trash::move_to_trash(&path, root, &id));
        let slot = match &trashed {
            Some(Ok(slot)) => Some(slot.clone()),
            _ => None,
        };
        if slot.is_none() {
            if let Err(e) = std::fs::remove_file(&path) {
                self.set_status(format!("Delete failed: {}", e));
                return;
            }
            let companion_dir = path.with_extension("");
            if companion_dir.is_dir() {
                std::fs::remove_dir_all(&companion_dir).ok();
            }
        }
        let undoable = slot.is_some();
        let pinned = self.bookmarks.contains(&id);
        let meta = self.sessions.remove(real_idx);
        if let Some(slot) = slot {
            self.undo_stack.push(DeletedSession { meta, pinned, slot });
        }
        self.snippets.clear();
        if real_idx < self.exact_matches.len() {
            self.exact_matches.remove(real_idx);
        }
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
        self.preview_lines = Default::default();
        self.preview_search_matches.clear();
        self.preview_session_id.clear();
        self.preview_loading = false;
        self.recompute_preview_offsets();
        self.preview_cache.remove(&id);
        self.preview_cache_order.retain(|c| c != &id);
        self.cleanup_bookmark_for_deleted(&id);
        self.set_status(match trashed {
            _ if undoable => "Session deleted — u undoes".to_string(),
            Some(Err(e)) => format!("Session deleted for good (no copy kept: {})", e),
            _ => "Session deleted".to_string(),
        });
    }

    /// Bring back the most recently deleted session.
    pub fn undo_delete(&mut self) {
        let Some(deleted) = self.undo_stack.pop() else {
            self.set_status("Nothing to undo".to_string());
            return;
        };
        if let Err(e) = crate::trash::restore(&deleted.slot, &deleted.meta.file_path) {
            // Kept, at the bottom: the next `u` goes on to earlier deletes
            // instead of failing on this one again.
            let more = if self.undo_stack.is_empty() { "" } else { "; u undoes the one before" };
            self.set_status(format!(
                "Undo failed: {} (kept in {}){}",
                e,
                deleted.slot.display(),
                more
            ));
            self.undo_stack.insert(0, deleted);
            return;
        }
        let id = deleted.meta.id.clone();
        let name = deleted.meta.display_name().to_string();
        if deleted.pinned {
            self.change_bookmarks(|set| {
                set.insert(id.clone());
            });
        }
        self.sessions.push(deleted.meta);
        self.view_mode = ViewMode::Normal;
        self.recompute_view();
        self.reselect(Some(id));
        self.set_status(format!("Restored \"{}\"", name));
    }

    pub fn cleanup_bookmark_for_deleted(&mut self, id: &str) {
        if self.bookmarks.contains(id) {
            self.change_bookmarks(|set| {
                set.remove(id);
            });
        }
    }

    /// Where session `real_idx` matched the search, for its row's match
    /// line: the conversation text around the hits (or, when the words are
    /// only in a changed file's path, that path). `None` when the words are
    /// only in the name, project or branch, or there's no text search.
    /// Cached until the next search.
    pub fn snippet(&mut self, real_idx: usize) -> Option<Snippet> {
        if let Some(cached) = self.snippets.get(&real_idx) {
            return cached.clone();
        }
        let snippet = self.compute_snippet(real_idx);
        self.snippets.insert(real_idx, snippet.clone());
        if let (Some(snip), Some((tx, _)), Some(s)) =
            (&snippet, &self.as_written, self.sessions.get(real_idx))
        {
            if !snip.in_file {
                let generation = self.snippet_generation.load(Ordering::Relaxed);
                for (part, p) in snip.parts.iter().enumerate() {
                    let _ = tx.send(crate::as_written::Request {
                        generation,
                        session: real_idx,
                        part,
                        path: s.file_path.clone(),
                        piece: p.source.clone(),
                        at: p.at,
                    });
                }
            }
        }
        snippet
    }

    /// Look snippets' text up in the transcripts, to show it as written.
    pub fn enable_as_written(&mut self) {
        let (found_tx, found_rx) = mpsc::channel();
        let tx = crate::as_written::spawn_worker(found_tx, self.snippet_generation.clone());
        self.as_written = Some((tx, found_rx));
    }

    /// Put finished lookups into their snippets. Returns whether any changed.
    pub fn apply_as_written(&mut self) -> bool {
        let Some((_, rx)) = &self.as_written else {
            return false;
        };
        let generation = self.snippet_generation.load(Ordering::Relaxed);
        let mut changed = false;
        while let Ok(found) = rx.try_recv() {
            if found.generation != generation {
                continue;
            }
            let part = self
                .snippets
                .get_mut(&found.session)
                .and_then(|s| s.as_mut())
                .and_then(|s| s.parts.get_mut(found.part));
            if let Some(part) = part {
                *part = SnippetPart::new(found.piece, part.at, true);
                changed = true;
            }
        }
        changed
    }

    fn compute_snippet(&self, real_idx: usize) -> Option<Snippet> {
        let s = self.sessions.get(real_idx)?;
        let phrase = self.search_phrase();
        let mut words: Vec<&str> = Vec::new();
        for word in phrase.split(' ').filter(|w| !w.is_empty()) {
            if !words.contains(&word) {
                words.push(word);
            }
        }
        if words.is_empty() {
            return None;
        }
        // Words the row already shows; when the words are far apart, the
        // others get the room.
        let shown = [
            s.name_lc.as_str(),
            s.title_lc.as_str(),
            s.project_lc.as_str(),
            s.branch_lc.as_str(),
        ]
        .join("\n");
        let text = self.text_cache.slice(s.text_offset, s.text_len);
        if let Some(parts) = text_snippet_parts(text, &phrase, &words, &shown) {
            return Some(Snippet { in_file: false, parts });
        }
        let hidden = words.iter().filter(|w| !shown.contains(**w));
        hidden.chain(words.iter()).find_map(|word| {
            s.changed_files.iter().find_map(|f| {
                let lower = crate::fold::fold(f);
                let pos = lower.find(word)?;
                // Paths are shown as written: the original is at hand.
                let written = Piece {
                    hit: (pos, pos + word.len()),
                    text: lower.clone(),
                };
                let written = crate::as_written::find_in(f, &written).unwrap_or(written);
                Some(Snippet {
                    in_file: true,
                    parts: vec![SnippetPart::new(written, 0.0, true)],
                })
            })
        })
    }

    /// Re-read which sessions are open in a running Claude Code. Returns
    /// whether that changed.
    pub fn refresh_live(&mut self) -> bool {
        let Some(dir) = &self.live_dir else {
            return false;
        };
        let live = crate::live::scan(dir);
        if live == self.live {
            return false;
        }
        self.live = live;
        true
    }

    pub fn live_status(&self, s: &SessionMeta) -> Option<crate::live::LiveSession> {
        self.live.get(&s.id).copied()
    }

    /// Resume the selected session, or ask first when it's already open in
    /// another Claude Code: two processes appending to one transcript
    /// interleave their turns.
    pub fn request_resume(&mut self, action: AppAction) {
        let target = match action {
            AppAction::Launch(i) | AppAction::LaunchDangerously(i) => self.sessions.get(i),
            // `--print` hands the ID to a script that resumes it.
            AppAction::Print(i) if self.print_mode => self.sessions.get(i),
            // A fork gets a session ID of its own; nothing to collide with.
            _ => None,
        };
        if target.is_some_and(|s| self.live_status(s).is_some()) {
            self.confirm_resume = Some(action);
            self.prompt_seen = false;
        } else {
            self.action = action;
        }
    }

    /// The session an action is for.
    pub fn action_target(&self, action: AppAction) -> Option<&SessionMeta> {
        match action {
            AppAction::Launch(i)
            | AppAction::LaunchDangerously(i)
            | AppAction::Fork(i)
            | AppAction::Print(i) => self.sessions.get(i),
            AppAction::None | AppAction::Quit => None,
        }
    }

    pub fn open_help(&mut self) {
        self.show_help = true;
        self.help_scroll = 0;
    }

    pub fn toggle_density(&mut self) {
        self.density = match self.density {
            Density::Comfortable => Density::Compact,
            Density::Compact => Density::Comfortable,
        };
    }

    pub fn toggle_zoom(&mut self) {
        self.zoom = !self.zoom;
        self.set_status(if self.zoom {
            "Zoomed: one pane at a time (Tab switches, z restores)".to_string()
        } else {
            "Both panes".to_string()
        });
    }

    /// Change the pins, in memory and (merged with what another sessy may
    /// have saved since) on disk.
    fn change_bookmarks(&mut self, change: impl Fn(&mut HashSet<String>)) {
        change(&mut self.bookmarks);
        if let Some(path) = &self.bookmarks_file {
            self.bookmarks = crate::bookmarks::update(path, change);
        }
    }

    fn clear_search(&mut self) {
        self.search_query.clear();
        let id = self.selected_id();
        self.apply_search();
        // Leaving a search keeps the session you were on, in its browse position.
        self.reselect(id);
        // Still previewing it: its search hits are no longer hits.
        if self.selected_session().is_some_and(|s| s.id == self.preview_session_id) {
            self.refresh_preview_matches(false);
        }
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
            Focus::List if self.size_filter.is_some() => {
                self.clear_size_filter();
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

/// The places in a session's search text (`text`) to show for the search
/// words: where the whole phrase occurs, else where all the words that
/// occur at all are close together, else the first hit of each, the words
/// the row doesn't already show (`shown`) first. `None` when no word occurs.
fn text_snippet_parts(
    text: &[u8],
    phrase: &str,
    words: &[&str],
    shown: &str,
) -> Option<Vec<SnippetPart>> {
    /// How close (in bytes) words must be to count as together.
    const NEAR: usize = 60;
    if words.len() > 1 {
        if let Some(pos) = find_preferring_whole_words(text, phrase) {
            return Some(vec![snippet_part(text, pos, pos + phrase.len())]);
        }
    }
    let found: Vec<(&str, usize)> = words
        .iter()
        .filter_map(|w| find_preferring_whole_words(text, w).map(|pos| (*w, pos)))
        .collect();
    match found.as_slice() {
        [] => return None,
        [(word, pos)] => return Some(vec![snippet_part(text, *pos, pos + word.len())]),
        _ => {}
    }
    let present: Vec<&str> = found.iter().map(|(w, _)| *w).collect();
    if let Some((start, end)) = find_cluster(text, &present, NEAR) {
        return Some(vec![snippet_part(text, start, end)]);
    }
    let mut picked: Vec<(&str, usize)> = found
        .iter()
        .filter(|(w, _)| !shown.contains(*w))
        .copied()
        .collect();
    if picked.is_empty() {
        picked = found;
    }
    picked.truncate(MAX_SNIPPET_PARTS);
    picked.sort_by_key(|&(_, pos)| pos);
    Some(
        picked
            .into_iter()
            .map(|(w, pos)| snippet_part(text, pos, pos + w.len()))
            .collect(),
    )
}

/// The first occurrence of `needle` as whole words (see
/// `contains_whole_words`), or else its first occurrence at all.
fn find_preferring_whole_words(text: &[u8], needle: &str) -> Option<usize> {
    /// Occurrences checked for a whole-word one before settling.
    const LOOK: usize = 500;
    let finder = memchr::memmem::Finder::new(needle.as_bytes());
    let first = finder.find(text)?;
    let whole = finder
        .find_iter(&text[first..])
        .take(LOOK)
        .map(|i| first + i)
        .find(|&i| is_whole_word_at(text, i, needle.len()));
    Some(whole.unwrap_or(first))
}

/// Whether `text[pos..pos + len]` isn't glued to a word char on either side
/// (where the needle's own edge is a word char).
fn is_whole_word_at(text: &[u8], pos: usize, len: usize) -> bool {
    let needle = &text[pos..pos + len];
    let left_ok = !first_char(needle).is_some_and(is_word_char)
        || last_char(&text[..pos]).is_none_or(|c| !is_word_char(c));
    let right_ok = !last_char(needle).is_some_and(is_word_char)
        || first_char(&text[pos + len..]).is_none_or(|c| !is_word_char(c));
    left_ok && right_ok
}

/// The first place where every word in `words` occurs within `near` bytes
/// of the longest one, on one line: (start of the first, end of the last).
fn find_cluster(text: &[u8], words: &[&str], near: usize) -> Option<(usize, usize)> {
    /// Occurrences of the anchor word tried before giving up.
    const ANCHORS: usize = 2000;
    let anchor = *words.iter().max_by_key(|w| w.len())?;
    for pos in memchr::memmem::find_iter(text, anchor.as_bytes()).take(ANCHORS) {
        let lo = pos.saturating_sub(near);
        let hi = (pos + anchor.len() + near).min(text.len());
        let window = &text[lo..hi];
        let (mut start, mut end) = (pos, pos + anchor.len());
        let all_near = words.iter().filter(|w| **w != anchor).all(|w| {
            // The occurrence nearest the anchor.
            let nearest = memchr::memmem::find_iter(window, w.as_bytes())
                .map(|i| lo + i)
                .min_by_key(|&i| i.abs_diff(pos));
            nearest.is_some_and(|i| {
                start = start.min(i);
                end = end.max(i + w.len());
                true
            })
        });
        if all_near && memchr::memchr(b'\n', &text[start..end]).is_none() {
            return Some((start, end));
        }
    }
    None
}

/// The search-text line around `text[start..end]` (a hit, or several close
/// together), cut to a few hundred bytes either side, cleaned for display.
fn snippet_part(text: &[u8], start: usize, end: usize) -> SnippetPart {
    const REACH: usize = 240;
    let floor = start.saturating_sub(REACH);
    let mut from = memchr::memrchr(b'\n', &text[floor..start]).map_or(floor, |i| floor + i + 1);
    let ceil = (end + REACH).min(text.len());
    let mut to = memchr::memchr(b'\n', &text[end..ceil]).map_or(ceil, |i| end + i);
    // Never cut a UTF-8 sequence in half.
    while from < start && (text[from] & 0xC0) == 0x80 {
        from += 1;
    }
    while to > end && to < text.len() && (text[to] & 0xC0) == 0x80 {
        to -= 1;
    }
    let piece = match std::str::from_utf8(&text[from..to]) {
        Ok(line) => Piece {
            text: line.to_string(),
            hit: (start - from, end - from),
        },
        // The cache is written from strings, so this is only a damaged file.
        Err(_) => {
            let lossy = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
            let (before, hit) = (lossy(&text[from..start]), lossy(&text[start..end]));
            Piece {
                hit: (before.len(), before.len() + hit.len()),
                text: before + &hit + &lossy(&text[end..to]),
            }
        }
    };
    SnippetPart::new(piece, start as f64 / text.len().max(1) as f64, false)
}

/// `s` with each run of whitespace (code indentation, tables) as one space.
fn fold_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}

fn is_ticket_query(q_upper: &str) -> bool {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"^[A-Z][A-Z0-9]{1,9}-\d{1,7}$|^#\d{1,7}$").unwrap()
    });
    re.is_match(q_upper)
}

/// Letters, digits, `_` and `/`: a slash doesn't end a word, so a piece of
/// a URL or path ("github.com/foo" in "github.com/foo/bar") isn't exact.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '/'
}

/// First char of UTF-8 `bytes` (`None` when empty or not valid UTF-8).
fn first_char(bytes: &[u8]) -> Option<char> {
    let head = &bytes[..bytes.len().min(4)];
    let valid = match std::str::from_utf8(head) {
        Ok(s) => s,
        Err(e) => std::str::from_utf8(&head[..e.valid_up_to()]).ok()?,
    };
    valid.chars().next()
}

/// Last char of UTF-8 `bytes` (`None` when empty or not valid UTF-8).
fn last_char(bytes: &[u8]) -> Option<char> {
    let tail = &bytes[bytes.len().saturating_sub(4)..];
    // The 4-byte window may start mid-char; skip to the first char start.
    (0..tail.len())
        .find_map(|i| std::str::from_utf8(&tail[i..]).ok())
        .and_then(|s| s.chars().next_back())
}

/// Whether `finder`'s needle occurs in `haystack` as whole words, i.e. not
/// glued to a word char (`is_word_char`) on either side: "deploy" is whole
/// in "deploy the app" but not in "redeployment". A needle edge that is
/// itself punctuation ("#12", ".rs") needs no boundary there.
fn contains_whole_words(haystack: &[u8], finder: &memchr::memmem::Finder) -> bool {
    let needle = finder.needle();
    if first_char(needle).is_none() || last_char(needle).is_none() {
        return false;
    }
    finder
        .find_iter(haystack)
        .any(|idx| is_whole_word_at(haystack, idx, needle.len()))
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

    fn ids(app: &App) -> Vec<&str> {
        app.filtered_indices
            .iter()
            .map(|&i| app.sessions[i].id.as_str())
            .collect()
    }

    #[test]
    fn test_exact_phrase_beats_higher_scoring_partial() {
        // "a" hits both tokens in name/title (higher score) but never as the
        // phrase; "b" has the phrase in its project only.
        let sessions = vec![
            make_session("a", "kerveros", "encrypt files", "kerveros", "main", vec![]),
            make_session("b", "", "", "kerveros encrypt", "main", vec![]),
        ];
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.search_query = "kerveros   encrypt".into();
        app.apply_search();
        assert_eq!(ids(&app), vec!["b", "a"]);
        assert!(app.is_exact_match(1));
        assert!(!app.is_exact_match(0));
        assert_eq!(app.exact_match_count(), 1);
    }

    #[test]
    fn test_exact_needs_whole_words() {
        let sessions = vec![
            make_session("a", "redeployment notes", "", "p", "main", vec![]),
            make_session("b", "", "", "deploy", "main", vec![]),
            make_session("c", "deploy_script", "", "p", "main", vec![]),
        ];
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.search_query = "deploy".into();
        app.apply_search();
        assert_eq!(ids(&app)[0], "b");
        assert_eq!(app.exact_match_count(), 1);
    }

    #[test]
    fn test_ticket_query_is_exact() {
        let sessions = vec![
            make_session("a", "", "", "p", "main", vec!["PROJ-12"]),
            make_session("b", "proj-123", "", "p", "main", vec![]),
        ];
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.search_query = "PROJ-12".into();
        app.apply_search();
        assert_eq!(ids(&app), vec!["a", "b"]);
        assert_eq!(app.exact_match_count(), 1);
    }

    #[test]
    fn test_exact_beats_bookmarked_partial() {
        let sessions = vec![
            make_session("a", "auth bug fix", "", "p", "main", vec![]),
            make_session("b", "auth", "", "p", "bug-123", vec![]),
        ];
        let mut bookmarks = HashSet::new();
        bookmarks.insert("b".to_string());
        let mut app = App::new(sessions, false, bookmarks, empty_cache());
        app.search_query = "auth bug".into();
        app.apply_search();
        assert_eq!(ids(&app), vec!["a", "b"]);
    }

    #[test]
    fn test_exact_tier_kept_under_sort() {
        let mut a = make_session("a", "auth flow", "", "p", "main", vec![]);
        a.timestamp = 1;
        let mut b = make_session("b", "authentication", "", "p", "main", vec![]);
        b.timestamp = 2;
        let mut c = make_session("c", "auth", "", "p", "main", vec![]);
        c.timestamp = 3;
        let mut app = App::new(vec![a, b, c], false, HashSet::new(), empty_cache());
        app.search_query = "auth".into();
        app.apply_search();
        app.cycle_sort(); // relevance → date
        assert_eq!(app.sort_label(), SortMode::Date.label());
        // Exact ("c", "a") by date, then the partial "b".
        assert_eq!(ids(&app), vec!["c", "a", "b"]);
    }

    #[test]
    fn test_clearing_search_drops_exact_flags() {
        let sessions = vec![make_session("a", "auth", "", "p", "main", vec![])];
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.search_query = "auth".into();
        app.apply_search();
        assert_eq!(app.exact_match_count(), 1);
        app.search_query.clear();
        app.apply_search();
        assert_eq!(app.exact_match_count(), 0);
    }

    #[test]
    fn test_contains_whole_words() {
        use memchr::memmem::Finder;
        let whole = |hay: &str, needle: &str| {
            contains_whole_words(hay.as_bytes(), &Finder::new(needle.as_bytes()))
        };
        assert!(whole("deploy the app", "deploy"));
        assert!(whole("re-deploy", "deploy"));
        assert!(!whole("redeploy", "deploy"));
        assert!(!whole("deployment", "deploy"));
        assert!(whole("deployment, then deploy", "deploy"));
        assert!(!whole("deploy_all", "deploy"));
        // Non-ASCII letters count as word chars on both sides.
        assert!(!whole("λογαριασμός", "λογ"));
        assert!(whole("ο λογαριασμός μου", "λογαριασμός"));
        assert!(!whole("éauth", "auth"));
        // Punctuation at the needle's edge needs no boundary.
        assert!(whole("pr#12 merged", "#12"));
        assert!(!whole("pr #123", "#12"));
        assert!(!whole("anything", ""));
        // A slash doesn't end a word: part of a URL or path is partial.
        let url = "see https://github.com/pegiadise/sessy/pull/7.";
        assert!(whole(url, "https://github.com/pegiadise/sessy/pull/7"));
        assert!(!whole(url, "github.com/pegiadise"));
        assert!(!whole(url, "pegiadise/sessy"));
        assert!(!whole(url, "sessy"));
        assert!(!whole("GET /api/users/123", "/api/users"));
        assert!(whole("GET /api/users?page=2", "/api/users"));
        assert!(whole("edit src/app.rs now", "src/app.rs"));
        assert!(!whole("edit src/app.rs now", "/app.rs"));
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
    fn test_clearing_the_search_drops_its_marks_from_the_preview() {
        // b (not the first row once the search is gone) is the hit.
        let mut app = three_sessions();
        app.sessions[1].name_lc = "caddy box".into();
        preview_of(&mut app, &[("set up caddy", Speaker::Assistant)]);
        app.preview_session_id = "b".into();
        app.search_query = "caddy".into();
        app.apply_search();
        assert_eq!(selected_id(&app), "b");
        assert_eq!(app.preview_search_matches, vec![0]);
        app.focus = Focus::List;
        app.handle_esc();
        assert!(app.search_query.is_empty());
        assert_eq!(selected_id(&app), "b", "still on b");
        assert!(app.preview_search_matches.is_empty(), "no stale marks or match count");
    }

    #[test]
    fn test_print_mode_asks_before_picking_an_open_session() {
        let mut app = three_sessions();
        app.print_mode = true;
        let first = app.filtered_indices[0];
        let id = app.sessions[first].id.clone();
        app.live.insert(id, crate::live::LiveSession { pid: 1, busy: true });
        app.request_resume(AppAction::Print(first));
        assert_eq!(app.confirm_resume, Some(AppAction::Print(first)));
        app.print_mode = false;
        app.confirm_resume = None;
        app.request_resume(AppAction::Print(first));
        assert_eq!(app.action, AppAction::Print(first), "p outside --print just prints");
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
        ].into();
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
        ].into();
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
        app.preview_lines = vec![("hello".into(), "hello".into(), Speaker::User)].into();
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
        app.preview_lines = vec![("hello".into(), "hello".into(), Speaker::User)].into();
        app.start_preview_search();
        app.preview_search_query = "hello".into();
        app.update_preview_search();
        app.exit_preview_search();
        assert_eq!(app.focus, Focus::Preview);
        assert!(!app.preview_search_active());
    }

    fn preview_of(app: &mut App, lines: &[(&str, Speaker)]) {
        app.preview_inner_width = 60;
        app.preview_viewport_height = 4;
        app.preview_lines = lines
            .iter()
            .map(|(t, sp)| (t.to_string(), t.to_lowercase(), *sp))
            .collect::<Vec<_>>()
            .into();
        app.recompute_preview_offsets();
    }

    fn long_preview(app: &mut App) {
        let lines: Vec<(String, Speaker)> = (0..40)
            .map(|i| (format!("message {} {}", i, "word ".repeat(30)), Speaker::Assistant))
            .collect();
        let refs: Vec<(&str, Speaker)> = lines.iter().map(|(t, s)| (t.as_str(), *s)).collect();
        preview_of(app, &refs);
        app.preview_viewport_height = 10;
    }

    #[test]
    fn test_rewrap_keeps_the_message_at_the_top() {
        let mut app = three_sessions();
        long_preview(&mut app);
        app.preview_scroll = app.preview_line_offsets[20];
        app.preview_inner_width = 30; // narrower: every message takes more rows
        app.rewrap_preview();
        assert_eq!(app.preview_scroll, app.preview_line_offsets[20], "still at message 20");
    }

    #[test]
    fn test_rewrap_keeps_a_shown_match_in_view() {
        let mut app = three_sessions();
        long_preview(&mut app);
        Arc::make_mut(&mut app.preview_lines)[25].1.push_str(" needle");
        app.preview_search_query = "needle".into();
        app.refresh_preview_matches(true);
        assert_eq!(app.preview_search_matches, vec![25]);
        app.preview_inner_width = 25;
        app.rewrap_preview();
        let (start, end) = app.preview_line_rows(25);
        let top = app.preview_scroll;
        assert!(start < top + 10 && end > top, "match rows {}..{} vs top {}", start, end, top);
    }

    #[test]
    fn test_toggling_tools_keeps_the_find_and_the_place() {
        let mut app = three_sessions();
        let id = app.selected_session().unwrap().id.clone();
        long_preview(&mut app);
        app.preview_session_id = id.clone();
        app.preview_search_query = "message 3".into();
        app.refresh_preview_matches(true);
        app.preview_scroll = app.preview_line_offsets[20];
        let top_text = app.preview_lines[20].0.clone();

        app.toggle_tools();
        crate::preview::request_preview(&mut app);
        assert_eq!(app.preview_search_query.text(), "message 3", "the find survives");
        // The reload comes back with tool lines in between.
        let mut reloaded = Vec::new();
        for i in 0..40 {
            let text = format!("message {} {}", i, "word ".repeat(30));
            reloaded.push((text.clone(), text.to_lowercase(), Speaker::Assistant));
            reloaded.push(("TOOL: Bash ls".into(), "tool: bash ls".into(), Speaker::Tool));
        }
        app.preview_lines = reloaded.into();
        app.preview_loading = false;
        app.preview_loaded();
        let (line, _) = app.preview_place().unwrap();
        assert_eq!(app.preview_lines[line].0, top_text, "back at the same message");
        assert_eq!(app.preview_search_query.text(), "message 3");
        assert!(app.preview_search_matches.len() > 1);
        assert!(
            app.preview_search_matches[app.preview_search_current] >= line,
            "n walks on from here"
        );
    }

    #[test]
    fn test_session_search_opens_where_all_words_are_past_the_recap() {
        let mut app = three_sessions();
        preview_of(
            &mut app,
            &[
                ("recap: caddy and deploy", Speaker::Recap),
                ("set up Caddy", Speaker::Assistant),
                ("more", Speaker::User),
                ("now deploy Caddy", Speaker::Assistant),
            ],
        );
        app.search_query = "caddy deploy".into();
        app.refresh_preview_matches(true);
        assert_eq!(app.preview_search_matches, vec![0, 1, 3]);
        assert_eq!(app.preview_search_current, 2, "the first message with both words");
        assert_eq!(app.preview_scroll, app.preview_line_offsets[3].saturating_sub(2).min(app.max_preview_scroll()));
        app.prev_preview_match();
        assert_eq!(app.preview_search_current, 1, "N still reaches earlier hits");

        app.search_query = "deploy".into();
        app.refresh_preview_matches(true);
        assert_eq!(app.preview_search_current, 1, "one word: the first message, not the recap");
    }

    #[test]
    fn test_session_search_words_mark_the_preview_and_scroll_to_the_first() {
        let mut app = three_sessions();
        preview_of(
            &mut app,
            &[
                ("intro", Speaker::User),
                ("one\ntwo\nthree", Speaker::Assistant),
                ("set up Caddy", Speaker::Assistant),
                ("more", Speaker::User),
                ("deploy it", Speaker::Assistant),
                ("tail", Speaker::User),
            ],
        );
        app.search_query = "caddy deploy".into();
        app.refresh_preview_matches(true);
        assert_eq!(app.preview_search_matches, vec![2, 4], "any of the words");
        assert!(app.preview_scroll > 0, "opens at the first hit");
        assert_eq!(app.preview_scroll, app.preview_line_offsets[2].saturating_sub(2));
        app.next_preview_match();
        assert_eq!(app.preview_search_current, 1);
    }

    #[test]
    fn test_find_query_takes_over_and_clearing_it_restores_search_words() {
        let mut app = three_sessions();
        preview_of(&mut app, &[("caddy", Speaker::User), ("nginx", Speaker::Assistant)]);
        app.search_query = "caddy".into();
        app.start_preview_search();
        app.preview_search_query = "nginx".into();
        app.update_preview_search();
        assert_eq!(app.preview_search_matches, vec![1]);
        assert!(app.preview_search_active());
        app.exit_preview_search();
        assert!(!app.preview_search_active());
        assert_eq!(app.preview_search_matches, vec![0], "back to the session search's hits");
        assert_eq!(app.focus, Focus::Preview);
    }

    #[test]
    fn test_esc_in_preview_with_only_search_hits_returns_to_list() {
        let mut app = three_sessions();
        preview_of(&mut app, &[("caddy", Speaker::User)]);
        app.search_query = "caddy".into();
        app.refresh_preview_matches(false);
        app.focus = Focus::Preview;
        app.handle_esc();
        assert_eq!(app.focus, Focus::List, "nothing of the preview's own to clear");
    }

    #[test]
    fn test_jump_between_prompts() {
        let mut app = three_sessions();
        preview_of(
            &mut app,
            &[
                ("q1", Speaker::User),
                ("a\nb\nc", Speaker::Assistant),
                ("q2", Speaker::User),
                ("d\ne\nf\ng\nh", Speaker::Assistant),
                ("q3", Speaker::User),
                ("i\nj\nk\nl\nm\nn", Speaker::Assistant),
            ],
        );
        let q2 = app.preview_line_offsets[2];
        let q3 = app.preview_line_offsets[4];
        app.jump_to_prompt(true);
        assert_eq!(app.preview_scroll, q2);
        app.jump_to_prompt(true);
        assert_eq!(app.preview_scroll, q3);
        app.jump_to_prompt(false);
        assert_eq!(app.preview_scroll, q2);
        app.jump_to_prompt(false);
        assert_eq!(app.preview_scroll, 0);
    }

    fn cache_with(text: &str) -> (tempfile::TempDir, TextCache) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("text.bin");
        std::fs::write(&path, text).unwrap();
        let cache = TextCache::open(&path);
        (dir, cache)
    }

    fn one_part(snip: &Snippet) -> (String, usize, usize) {
        assert_eq!(snip.parts.len(), 1, "{:?}", snip.parts);
        let p = &snip.parts[0];
        (p.text.clone(), p.hit, p.span)
    }

    fn searched(text: &str, name: &str, query: &str) -> (tempfile::TempDir, App) {
        let (dir, cache) = cache_with(text);
        let mut s = make_session("a", name, "fix the box", "p", "main", vec![]);
        s.text_len = text.len() as u32;
        let mut app = App::new(vec![s], false, HashSet::new(), cache);
        app.search_query = query.into();
        app.apply_search();
        (dir, app)
    }

    #[test]
    fn test_snippet_shows_the_text_around_the_hit() {
        let text = "first line\nwe reload caddy after the deploy step\nlast";
        let (_dir, mut app) = searched(text, "server work", "caddy");
        let snip = app.snippet(0).unwrap();
        assert!(!snip.in_file);
        let (body, hit, span) = one_part(&snip);
        assert_eq!(body, "we reload caddy after the deploy step");
        assert_eq!(&body[hit..hit + span], "caddy");

        // The whole phrase together is preferred over the first word alone.
        app.search_query = "the deploy".into();
        app.apply_search();
        let (_, hit, span) = one_part(&app.snippet(0).unwrap());
        assert_eq!((hit, span), ("we reload caddy after ".len(), "the deploy".len()));

        // Only in the name: nothing in the conversation to show.
        app.search_query = "server".into();
        app.apply_search();
        assert_eq!(app.snippet(0), None);
    }

    #[test]
    fn test_snippet_shows_hits_even_when_the_name_has_the_word() {
        let (_dir, mut app) = searched("we reload caddy nightly", "caddy work", "caddy");
        let (body, _, _) = one_part(&app.snippet(0).unwrap());
        assert_eq!(body, "we reload caddy nightly");
    }

    #[test]
    fn test_snippet_finds_words_close_together() {
        let text = "release notes are drafted\n\
                    later we cut the release and publish the crates to crates.io\n";
        let (_dir, mut app) = searched(text, "x", "crates release");
        let (body, hit, span) = one_part(&app.snippet(0).unwrap());
        assert_eq!(body, "later we cut the release and publish the crates to crates.io");
        assert_eq!(
            body.chars().skip(hit).take(span).collect::<String>(),
            "release and publish the crates"
        );
    }

    #[test]
    fn test_snippet_shows_each_word_when_they_are_far_apart() {
        let filler = "x".repeat(300);
        let text = format!("tokio runtime here\n{}\nwe tag the release\n", filler);
        let (_dir, mut app) = searched(&text, "x", "release tokio");
        let snip = app.snippet(0).unwrap();
        let bodies: Vec<&str> = snip.parts.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(bodies, ["tokio runtime here", "we tag the release"], "in reading order");

        // A word the row already shows gives its room to the others.
        let (_dir, mut app) = searched(&text, "tokio app", "release tokio");
        let (body, _, _) = one_part(&app.snippet(0).unwrap());
        assert_eq!(body, "we tag the release");
    }

    #[test]
    fn test_snippet_prefers_a_whole_word_hit() {
        let (_dir, mut app) = searched("we redeploy often\nthen deploy it", "x", "deploy");
        let (body, _, _) = one_part(&app.snippet(0).unwrap());
        assert_eq!(body, "then deploy it");
    }

    #[test]
    fn test_snippet_folds_indentation_and_tabs() {
        let (_dir, mut app) = searched("        let  x =\tflock(fd);   ", "x", "flock");
        let (body, hit, _) = one_part(&app.snippet(0).unwrap());
        assert_eq!(body, "let x = flock(fd);");
        assert_eq!(&body[hit..hit + 5], "flock");
    }

    #[test]
    fn test_search_ignores_accents_and_case() {
        let text = crate::fold::fold("Ραντεβού στην Οδό Πανεπιστημίου\nmeet at the Café");
        let (_dir, cache) = cache_with(&text);
        let mut s = make_session("a", "Οδηγός", "", "p", "main", vec![]);
        s.name_lc = crate::fold::fold("Οδηγός");
        s.text_len = text.len() as u32;
        let mut app = App::new(vec![s], false, HashSet::new(), cache);
        for query in ["οδο πανεπιστημιου", "ΟΔΌ", "cafe", "CAFÉ", "οδηγος"] {
            app.search_query = query.into();
            app.apply_search();
            assert_eq!(app.filtered_indices, vec![0], "{:?}", query);
        }
        app.search_query = "οδο πανεπιστημιου".into();
        app.apply_search();
        assert!(app.is_exact_match(0), "whole words, accents aside");
    }

    #[test]
    fn test_snippet_falls_back_to_changed_files() {
        let mut s = make_session("a", "x", "", "p", "main", vec![]);
        s.changed_files = vec!["/repo/src/Auth.rs".into()];
        s.changed_files_lc = "/repo/src/auth.rs".into();
        let mut app = App::new(vec![s], false, HashSet::new(), empty_cache());
        app.search_query = "auth.rs".into();
        app.apply_search();
        let snip = app.snippet(0).unwrap();
        assert!(snip.in_file);
        let (body, hit, _) = one_part(&snip);
        assert_eq!(body, "/repo/src/Auth.rs", "as written");
        assert_eq!(hit, "/repo/src/".len());
    }

    #[test]
    fn test_snippet_never_splits_a_utf8_char() {
        let text = format!("{}λογαριασμός caddy", "ά".repeat(200));
        let (_dir, mut app) = searched(&text, "x", "caddy");
        let (body, hit, _) = one_part(&app.snippet(0).unwrap());
        assert!(body.ends_with("λογαριασμός caddy"));
        assert_eq!(body.chars().skip(hit).collect::<String>(), "caddy");
    }

    #[test]
    fn test_delete_goes_to_trash_and_undo_restores_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut sessions = Vec::new();
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            let path = dir.path().join(format!("{}.jsonl", id));
            std::fs::write(&path, "{}\n").unwrap();
            let mut s = make_session(id, &format!("name {}", id), "", "p", "main", vec![]);
            s.timestamp = 100 - i as i64;
            s.file_path = path;
            sessions.push(s);
        }
        let mut bookmarks = HashSet::new();
        bookmarks.insert("b".to_string());
        let mut app = App::new(sessions, false, bookmarks, empty_cache());
        app.trash_dir = Some(dir.path().join("trash"));
        app.rebuild_view();
        assert_eq!(selected_id(&app), "b", "pinned first");

        app.delete_selected();
        assert_eq!(app.sessions.len(), 2);
        assert!(!dir.path().join("b.jsonl").exists());
        assert!(!app.bookmarks.contains("b"));
        assert_eq!(app.active_status(), Some("Session deleted — u undoes"));

        app.move_to_bottom();
        app.undo_delete();
        assert_eq!(app.sessions.len(), 3);
        assert!(dir.path().join("b.jsonl").is_file());
        assert!(app.bookmarks.contains("b"), "the pin comes back too");
        assert_eq!(selected_id(&app), "b", "the restored session is selected");

        app.undo_delete();
        assert_eq!(app.active_status(), Some("Nothing to undo"));
    }

    #[test]
    fn test_filters_narrow_and_combine_with_words() {
        let mut a = make_session("a", "auth fix", "", "web", "main", vec![]);
        a.timestamp = 300;
        let mut b = make_session("b", "auth rework", "", "api", "main", vec![]);
        b.timestamp = 200;
        let mut c = make_session("c", "docs", "", "web", "main", vec![]);
        c.timestamp = 100;
        let mut bookmarks = HashSet::new();
        bookmarks.insert("c".to_string());
        let mut app = App::new(vec![a, b, c], false, bookmarks, empty_cache());

        app.search_query = "project:web".into();
        app.apply_search();
        assert_eq!(ids(&app), vec!["c", "a"], "filters alone: pinned first, then by date");
        assert_eq!(app.sort_label(), "date", "no words, no relevance");
        assert_eq!(app.exact_match_count(), 0);
        assert_eq!(app.search_phrase(), "");

        app.search_query = "auth project:web".into();
        app.apply_search();
        assert_eq!(ids(&app), vec!["a"]);
        assert_eq!(app.search_phrase(), "auth", "filters are never highlighted");
        assert_eq!(app.preview_terms(), vec!["auth".to_string()]);

        app.search_query = "is:pinned".into();
        app.apply_search();
        assert_eq!(ids(&app), vec!["c"]);

        app.live.insert("b".into(), crate::live::LiveSession { pid: 1, busy: false });
        app.search_query = "is:open auth".into();
        app.apply_search();
        assert_eq!(ids(&app), vec!["b"]);

        app.search_query = "before:1970-01-02".into();
        app.apply_search();
        assert_eq!(ids(&app).len(), 3, "timestamps 100-300 are on Jan 1 1970");
    }

    #[test]
    fn test_is_open_follows_sessions_opening_and_closing() {
        let mut app = three_sessions();
        app.search_query = "is:open".into();
        app.apply_search();
        assert!(app.filtered_indices.is_empty());
        app.live.insert("b".into(), crate::live::LiveSession { pid: 1, busy: false });
        app.live_changed();
        assert_eq!(ids(&app), vec!["b"]);
        app.live.clear();
        app.live_changed();
        assert!(app.filtered_indices.is_empty());
    }

    #[test]
    fn test_failed_undo_keeps_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        std::fs::write(&path, "{}\n").unwrap();
        let mut s = make_session("a", "x", "", "p", "main", vec![]);
        s.file_path = path.clone();
        let mut app = App::new(vec![s], false, HashSet::new(), empty_cache());
        app.trash_dir = Some(dir.path().join("trash"));
        app.delete_selected();
        std::fs::write(&path, "recreated\n").unwrap();
        app.undo_delete();
        assert!(app.active_status().unwrap().starts_with("Undo failed"));
        assert_eq!(app.undo_stack.len(), 1, "still undoable once the conflict is gone");
        std::fs::remove_file(&path).unwrap();
        app.undo_delete();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");
    }

    #[test]
    fn test_a_failed_undo_does_not_block_earlier_ones() {
        let dir = tempfile::tempdir().unwrap();
        let mut sessions = Vec::new();
        for id in ["a", "b"] {
            let path = dir.path().join(format!("{}.jsonl", id));
            std::fs::write(&path, format!("{}\n", id)).unwrap();
            let mut s = make_session(id, id, "", "p", "main", vec![]);
            s.file_path = path;
            sessions.push(s);
        }
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.trash_dir = Some(dir.path().join("trash"));
        app.delete_selected(); // a
        app.delete_selected(); // b, the most recent
        std::fs::write(dir.path().join("b.jsonl"), "recreated\n").unwrap();
        app.undo_delete();
        assert!(app.active_status().unwrap().contains("u undoes the one before"));
        app.undo_delete();
        assert_eq!(std::fs::read_to_string(dir.path().join("a.jsonl")).unwrap(), "a\n");
        assert_eq!(app.undo_stack.len(), 1, "b can still be retried");
    }

    #[test]
    fn test_delete_without_a_usable_trash_still_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        std::fs::write(&path, "{}\n").unwrap();
        let blocker = dir.path().join("not-a-dir");
        std::fs::write(&blocker, "").unwrap();
        let mut s = make_session("a", "x", "", "p", "main", vec![]);
        s.file_path = path.clone();
        let mut app = App::new(vec![s], false, HashSet::new(), empty_cache());
        app.trash_dir = Some(blocker.join("trash"));
        app.delete_selected();
        assert!(!path.exists());
        assert!(app.sessions.is_empty());
        assert!(
            app.active_status().is_some_and(|m| m.starts_with("Session deleted for good")),
            "no undo promised, and it says so"
        );
    }

    #[test]
    fn test_preview_cache_keeps_to_its_budget_and_reuses_layouts() {
        let mut app = three_sessions();
        let lines = |n: usize| -> PreviewLines {
            Arc::new(vec![("x".repeat(n), "x".repeat(n), Speaker::User)])
        };
        for (id, n) in [("a", 400), ("b", 400), ("c", 400)] {
            app.cache_preview(id.into(), lines(n));
        }
        app.trim_preview_cache(10, 1000);
        assert_eq!(Vec::from(app.preview_cache_order.clone()), ["c"], "oldest out, by bytes");
        app.cache_preview("d".into(), lines(5000));
        app.trim_preview_cache(10, 1000);
        assert!(app.preview_cache.contains_key("d"), "the newest stays even when too big");

        // Shown again at the same width: the stored layout is used.
        app.preview_session_id = "d".into();
        app.preview_lines = app.preview_cache["d"].lines.clone();
        app.preview_inner_width = 40;
        app.recompute_preview_offsets();
        let rows = app.preview_total_rows;
        assert!(app.preview_cache["d"].layout.as_ref().is_some_and(|l| l.2 == rows));
        app.preview_total_rows = 0;
        app.recompute_preview_offsets();
        assert_eq!(app.preview_total_rows, rows);
    }

    #[test]
    fn test_search_moving_to_a_cached_session_opens_it_at_the_top() {
        let mut app = three_sessions();
        app.preview_inner_width = 60;
        app.preview_viewport_height = 3;
        // Previewing "a", whose hit is far down; "b" is cached with no hit.
        let filler: Vec<(String, String, Speaker)> = (0..30)
            .map(|i| (format!("line {}", i), format!("line {}", i), Speaker::Assistant))
            .collect();
        let mut a_lines = filler.clone();
        a_lines.push(("needle".into(), "needle".into(), Speaker::User));
        app.preview_lines = a_lines.into();
        app.preview_session_id = "a".into();
        app.recompute_preview_offsets();
        app.cache_preview("b".into(), filler.into());
        app.sessions[1].name_lc = "needle".into();

        app.search_query = "needle".into();
        app.apply_search();
        crate::preview::request_preview(&mut app);
        assert_eq!(app.preview_session_id, "b");
        assert_eq!(app.preview_scroll, 0, "not a's hit position");
    }

    #[test]
    fn test_search_hits_do_not_scroll_the_files_view() {
        let mut app = three_sessions();
        preview_of(
            &mut app,
            &[("a\nb\nc\nd\ne", Speaker::User), ("caddy", Speaker::Assistant)],
        );
        app.show_files = true;
        app.search_query = "caddy".into();
        app.refresh_preview_matches(true);
        assert_eq!(app.preview_scroll, 0);
        app.jump_to_prompt(true);
        assert_eq!(app.preview_scroll, 0);
    }

    #[test]
    fn test_resuming_a_live_session_asks_first() {
        let mut app = three_sessions();
        app.live.insert(
            "a".into(),
            crate::live::LiveSession { pid: 42, busy: true },
        );
        app.request_resume(AppAction::Launch(0));
        assert_eq!(app.action, AppAction::None);
        assert_eq!(app.confirm_resume, Some(AppAction::Launch(0)));

        // Forking makes a new session, so it never collides.
        app.confirm_resume = None;
        app.request_resume(AppAction::Fork(0));
        assert_eq!(app.action, AppAction::Fork(0));

        let mut app = three_sessions();
        app.move_down();
        app.live.insert("a".into(), crate::live::LiveSession { pid: 42, busy: false });
        app.request_resume(AppAction::Launch(1));
        assert_eq!(app.action, AppAction::Launch(1), "only the open one asks");
    }

    #[test]
    fn test_empty_current_scope_widens_to_all_projects() {
        let mut a = make_session("a", "x", "", "p", "main", vec![]);
        a.cwd = "/elsewhere".into();
        let mut app = App::new(vec![a], false, HashSet::new(), empty_cache());
        app.scope_root = Some(PathBuf::from("/Users/me/new-repo"));
        app.scope = Scope::Current;
        app.rebuild_view();
        assert!(app.filtered_indices.is_empty());
        app.widen_scope_if_empty();
        assert_eq!(app.scope, Scope::All);
        assert_eq!(app.filtered_indices.len(), 1);
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
