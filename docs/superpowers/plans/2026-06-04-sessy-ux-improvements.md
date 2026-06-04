# sessy UX Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Modernize sessy to parse the current Claude Code session format (aiTitle, permission mode, skills, changed files) and add high-value UX features: in-TUI scope toggle, help overlay, message-count column/sort, files-changed surfacing, optional tool activity in preview, a config file, and navigation/scroll polish.

**Architecture:** A single-pass scanner (`parser::scan_session`) already reads every JSONL line per session. Extend it to harvest the new metadata in that same pass, widen `SessionMeta`, bump `INDEX_VERSION` so old caches invalidate, then surface the new data through the existing app-state + ratatui rendering layers. The cwd-scope filter moves from `main.rs` startup into the App's view pipeline so it can be toggled live.

**Tech Stack:** Rust 2024, ratatui 0.30, crossterm 0.29, serde/serde_json, bincode, rayon, regex, memmap2, clap. Add `toml` for config.

---

## Design decisions (locked)

- **aiTitle folds into `name`.** Name priority becomes `/rename` → `aiTitle` → `slug` → empty. This routes the clean AI title into the existing line-1 headline, preview-pane label, export filename, and `name_lc` search field with minimal surface change. The first human message stays as the line-2 `title` quote.
- **`Enter` = yolo is preserved** (explicit user request). No launch-binding changes.
- **`message_count` becomes `u32`** (was `Option<u32>`), populated at index time by counting human messages during the scan. Preview still overwrites it (no-op since equal).
- **New `SessionMeta` fields:** `permission_mode: String`, `cc_version: String`, `skills: Vec<String>`, `changed_files: Vec<String>`, `changed_files_lc: String` (newline-joined, for search).
- **`INDEX_VERSION` 3 → 4.** Adding fields changes the bincode layout.
- **Preview line role** changes from `bool is_user` to enum `Speaker { User, Assistant, Tool }` so tool activity can render distinctly.
- **Config** is minimal TOML at `~/.config/sessy/config.toml`: `scope`, `sort`, `show_tool_activity`. Missing/malformed → defaults, never crash. CLI `--all` overrides `scope`.
- **New keys:** `a` scope toggle, `?` help overlay, `f` files-changed view, `T` (shift+t) tool-activity toggle, `g`/`Home` top, `G`/`End` bottom. Sort cycle gains `messages`.
- **Deferred (noted to user, not in this plan):** NO_COLOR/16-color fallback, multi-select/trash delete, regex & field-scoped (`project:`) search, assistant-text in list search.

## File map

- `src/parser.rs` — extend `ScanResult` + `scan_session`; add `extract_conversation_ext(include_tools)` + `Speaker`/tool-call summarization.
- `src/session.rs` — widen `SessionMeta`; `message_count: u32`.
- `src/index.rs` — populate new fields; name priority; `INDEX_VERSION=4`.
- `src/app.rs` — `Scope`, `show_tools`, `show_help`, files view state; scope filter in pipeline; scroll clamp; `SortMode::Messages`; top/bottom nav; search over `changed_files_lc`; preview tuple → `(String,String,Speaker)`.
- `src/preview.rs` — thread role enum; honor `show_tools`.
- `src/ui.rs` — message-count + badges in list/preview; scroll %; help overlay; status-bar wrap; empty-state hint; files view; tool styling.
- `src/config.rs` — NEW. Load/parse config with defaults.
- `src/lib.rs` — export `config`.
- `src/main.rs` — load config, seed App, move cwd filter into App, wire new keys.
- `Cargo.toml` — add `toml`.
- `tests/fixtures/modern_session.jsonl` — NEW fixture with ai-title, permission-mode, version, attributionSkill, file-history-snapshot.
- `README.md`, `CLAUDE.md` — update keybindings, fields, and correct the stale "head/tail reads" claim (scan is now full-file).

---

## Task 1: Parse modern session metadata

**Files:**
- Modify: `src/parser.rs` (`ScanResult`, `scan_session`)
- Test: `src/parser.rs` (tests mod), `tests/fixtures/modern_session.jsonl`

- [ ] **Step 1:** Create `tests/fixtures/modern_session.jsonl` containing (one JSON object per line): a `user` human message ("add login endpoint"), an `assistant` text block, an `ai-title` entry `{"type":"ai-title","aiTitle":"Add JWT login endpoint"}`, a `permission-mode` entry `{"type":"permission-mode","permissionMode":"plan"}`, an entry carrying `"version":"2.1.0"` and `"gitBranch":"feat/auth"` and `"cwd":"/Users/me/code/demo"`, a `user` entry with `attributionSkill":"brainstorming"`, a `file-history-snapshot` whose `snapshot.trackedFileBackups` has keys `src/auth.rs` and `src/lib.rs`, and a final `user` human message ("ship it").
- [ ] **Step 2:** Extend `ScanResult` with `ai_title: String`, `permission_mode: String`, `cc_version: String`, `skills: Vec<String>`, `changed_files: Vec<String>`, `message_count: u32`.
- [ ] **Step 3:** In `scan_session`, during the existing per-line loop, capture: last `aiTitle`; last `permissionMode`; first non-empty `version`; insert each `attributionSkill` string into a set; for `file-history-snapshot` union `snapshot.trackedFileBackups` object keys into a set; increment a counter on each `is_human_message`. Sort skills + changed_files; return them.
- [ ] **Step 4:** Test `test_scan_modern_extracts_metadata`: scan the fixture; assert `ai_title=="Add JWT login endpoint"`, `permission_mode=="plan"`, `cc_version=="2.1.0"`, `skills` contains "brainstorming", `changed_files` contains "src/auth.rs" & "src/lib.rs" (sorted), `message_count==2`.
- [ ] **Step 5:** `cargo test parser::` → PASS. Ensure existing parser tests still pass (old fixtures lack new fields → empty/zero defaults).

## Task 2: Widen SessionMeta + index population (INDEX_VERSION 4)

**Files:**
- Modify: `src/session.rs` (struct + every literal in tests), `src/index.rs`
- Test: `src/index.rs`, `src/session.rs` test literals

- [ ] **Step 1:** Add to `SessionMeta`: `permission_mode: String`, `cc_version: String`, `skills: Vec<String>`, `changed_files: Vec<String>`, `changed_files_lc: String`; change `message_count: Option<u32>` → `message_count: u32`.
- [ ] **Step 2:** Update all `SessionMeta { .. }` literals (session.rs tests, app.rs `make_session`, index.rs tests) to set the new fields (`String::new()`, `vec![]`, `0`).
- [ ] **Step 3:** In `index::scan_session_file`: set `name = rename` else `ai_title` else `slug`; set `message_count = scan.message_count`; copy `permission_mode/cc_version/skills/changed_files`; build `changed_files_lc = changed_files.join("\n").to_lowercase()`.
- [ ] **Step 4:** Bump `INDEX_VERSION` to `4`.
- [ ] **Step 5:** Test `test_scan_session_file_uses_ai_title`: scan modern fixture → `meta.name=="Add JWT login endpoint"`, `meta.message_count==2`, `meta.permission_mode=="plan"`, `meta.changed_files_lc.contains("src/auth.rs")`.
- [ ] **Step 6:** `cargo test` → PASS (all modules compile with new literals).

## Task 3: message-count display + sort

**Files:**
- Modify: `src/app.rs` (`SortMode`, `cycle_sort`, `apply_sort`), `src/ui.rs` (list line 2), `src/preview.rs` / `check_preview_updates` (set `u32`)
- Test: `src/app.rs`

- [ ] **Step 1:** Add `SortMode::Messages` with label `"messages"`; `cycle_sort` order date→size→duration→messages→date; `apply_sort` Messages arm sorts by `message_count` desc.
- [ ] **Step 2:** Fix `preview.rs`/`check_preview_updates` assignment to `session.message_count = result.message_count;` (now `u32`).
- [ ] **Step 3:** In `ui.rs` list line 2, append `· {n} msgs` after the size category (only if `message_count>0`), within the title budget calc.
- [ ] **Step 4:** Test `test_sort_by_messages_orders_desc`: two sessions msg counts 3 and 9; set `sort_mode=Messages`; `apply_sort`; assert higher count first.
- [ ] **Step 5:** `cargo test app::` → PASS.

## Task 4: search over changed files

**Files:** Modify `src/app.rs` (`apply_search_inner`); Test `src/app.rs`

- [ ] **Step 1:** In `apply_search_inner`, after the branch check and before the text-cache fallback, add a check: `finder.find(s.changed_files_lc.as_bytes())` → `score += 200; true`. (Order: name/title 500 → project 400 → branch 300 → changed_files 200 → human text 100.)
- [ ] **Step 2:** Test `test_search_matches_changed_file`: session with `changed_files_lc="src/auth.rs"` and empty name/title/project; query `"auth.rs"`; `apply_search`; assert it matches.
- [ ] **Step 3:** `cargo test app::` → PASS.

## Task 5: in-TUI scope toggle + empty-state hint

**Files:** Modify `src/app.rs` (Scope, filter, key), `src/main.rs` (move cwd filter out, pass cwd+scope), `src/ui.rs` (empty hint, title); Test `src/app.rs`

- [ ] **Step 1:** Add `enum Scope { Current, All }`; App fields `scope: Scope`, `cwd_encoded: Option<String>`. `App::new` gains `scope` + `cwd_encoded` params (or a setter; keep `new` signature stable by adding a `with_scope` setter to limit test churn — chosen: add params, update tests).
- [ ] **Step 2:** New `apply_scope_filter(&mut self)` retaining sessions whose `file_path` contains `/{cwd_encoded}/` when `scope==Current` and `cwd_encoded` is Some. Call it first in `rebuild_view` (before search).
- [ ] **Step 3:** `toggle_scope()` flips Current/All, `rebuild_view()`. Bind `a` in `handle_list_key`.
- [ ] **Step 4:** In `main.rs`, delete the startup cwd `retain` block (lines ~57-69); compute `cwd_encoded`; seed App `scope` from `cli.all` (All if `--all` else config/Current). Keep `--project`/`--recent` pre-filters.
- [ ] **Step 5:** `ui.rs` empty-state: when `filtered_indices` empty and `scope==Current`, show "No sessions in this directory — press a for all projects." List title shows `[all]` or `[cwd]`.
- [ ] **Step 6:** Test `test_scope_filter_current_limits_to_cwd`: two sessions, paths under different encoded dirs; `cwd_encoded=Some(dir1)`, `scope=Current`; `rebuild_view`; assert only dir1 session; `toggle_scope`; assert both.
- [ ] **Step 7:** `cargo test` → PASS.

## Task 6: preview scroll clamp + position indicator

**Files:** Modify `src/app.rs` (clamp + viewport height), `src/ui.rs` (store viewport height, title %); Test `src/app.rs`

- [ ] **Step 1:** App field `preview_viewport_height: u16` (set in `draw_preview`). Add `max_preview_scroll()` = `total_offset.saturating_sub(viewport)` where `total_offset` = last entry offset+lines (track total in `recompute_preview_offsets` as `preview_total_rows`).
- [ ] **Step 2:** Clamp in `scroll_preview_down`/`scroll_preview_page_down` to `max_preview_scroll()`.
- [ ] **Step 3:** `ui.rs` preview title: when not loading/searching and `preview_total_rows>viewport`, append `{pct}%` from `scroll/max`.
- [ ] **Step 4:** Test `test_preview_scroll_clamped`: set `preview_total_rows=10`, `preview_viewport_height=20`; many `scroll_preview_down`; assert `preview_scroll==0` (content shorter than viewport → max 0).
- [ ] **Step 5:** `cargo test app::` → PASS.

## Task 7: list nav top/bottom

**Files:** Modify `src/app.rs` (`move_to_top`/`move_to_bottom`), `src/main.rs` (keys); Test `src/app.rs`

- [ ] **Step 1:** `move_to_top` → selected=0; `move_to_bottom` → selected=len-1 (guard empty); both reset `preview_scroll`.
- [ ] **Step 2:** Bind `g`/`Home` → top, `G`/`End` → bottom in `handle_list_key`; request preview.
- [ ] **Step 3:** Test `test_move_to_bottom`: 5 sessions; `move_to_bottom`; assert selected==4.
- [ ] **Step 4:** `cargo test app::` → PASS.

## Task 8: tool activity in preview (Speaker enum + toggle)

**Files:** Modify `src/parser.rs` (`Speaker`, `extract_conversation_ext`), `src/app.rs` (tuple type, `show_tools`), `src/preview.rs`, `src/ui.rs`, `src/main.rs` (key `T`); Test `src/parser.rs`

- [ ] **Step 1:** Add `pub enum Speaker { User, Assistant, Tool }`. Add `extract_conversation_ext(path, include_tools) -> Vec<(Speaker,String)>`; existing `extract_conversation` delegates with `include_tools=false` mapping Role→Speaker. When `include_tools`, for assistant `tool_use` blocks push `Speaker::Tool` with `"⚙ {name} {short-input}"` (e.g. file_path or command, truncated 120).
- [ ] **Step 2:** Change preview line tuple everywhere from `(String,String,bool)` to `(String,String,Speaker)`; update `PreviewResult.lines`, `App.preview_lines`, `preview_cache`, `recompute_preview_offsets` (prefix per speaker), `update_preview_search`, `intra_match_chunk_offset`, ui render (Tool → dim cyan, prefix "TOOL: ").
- [ ] **Step 3:** App field `show_tools: bool`; `toggle_tools()` flips it, clears `preview_session_id`+`preview_lines`+cache for selected so preview re-extracts. `preview::request_preview` passes `app.show_tools` to the thread.
- [ ] **Step 4:** Bind `T` (Char('T')) in `handle_list_key` and `handle_preview_key` → `toggle_tools()` + request_preview.
- [ ] **Step 5:** Test `test_extract_with_tools_includes_tool_lines`: fixture/complex_session with a tool_use; `extract_conversation_ext(.., true)` yields ≥1 `Speaker::Tool`; with `false` yields none.
- [ ] **Step 6:** `cargo test` → PASS.

## Task 9: files-changed view

**Files:** Modify `src/app.rs` (view state), `src/ui.rs` (render), `src/main.rs` (key `f`)

- [ ] **Step 1:** App `show_files: bool`; `toggle_files()`. Bind `f` in `handle_list_key`/`handle_preview_key`.
- [ ] **Step 2:** In `draw_preview`, when `show_files`, render selected session's `changed_files` as a list (title " Files changed (n) "); empty → "No tracked file changes." Conversation rendering skipped while active.
- [ ] **Step 3:** Manual check via run; no unit test (pure render). `cargo build` → OK.

## Task 10: metadata badges

**Files:** Modify `src/ui.rs` (preview header + list)

- [ ] **Step 1:** Preview title: append badges from selected session — `permission_mode` (mapped: bypassPermissions→"yolo", acceptEdits→"accept", plan→"plan", default→omit), and `skills` joined (max 3, "+N"). Keep within width.
- [ ] **Step 2:** `cargo build` → OK. Manual visual check.

## Task 11: help overlay + status-bar fix

**Files:** Modify `src/app.rs` (`show_help`), `src/ui.rs` (overlay + wrap), `src/main.rs` (`?`)

- [ ] **Step 1:** App `show_help: bool`; `?` toggles (any key/Esc closes); when open, `handle_list_key` swallows other keys.
- [ ] **Step 2:** `draw` renders a centered help panel listing all keybindings when `show_help`.
- [ ] **Step 3:** Status bar: enable wrapping (`Paragraph::wrap`) or trim to two compact rows so nothing clips at 80 cols; add `? help` hint.
- [ ] **Step 4:** `cargo build` → OK. Manual visual check at 80 cols.

## Task 12: config file

**Files:** Create `src/config.rs`; Modify `src/lib.rs`, `src/main.rs`, `Cargo.toml`; Test `src/config.rs`

- [ ] **Step 1:** `Cargo.toml`: add `toml = "<latest>"` (verify via `cargo search toml`).
- [ ] **Step 2:** `config.rs`: `#[derive(Deserialize)] struct Config { scope, sort, show_tool_activity }` with serde defaults (`scope="current"`, `sort="date"`, `show_tool_activity=false`); `load() -> Config` reads `~/.config/sessy/config.toml`, returns default on missing/parse error; helpers `scope_is_all()`, `sort_mode() -> SortMode`.
- [ ] **Step 3:** `lib.rs`: `pub mod config;`. `main.rs`: `let cfg = config::load();` seed App `scope` (cli.all || cfg all), `sort_mode`, `show_tools`.
- [ ] **Step 4:** Test `test_config_defaults` (parse empty string → current/date/false) and `test_config_parse` (a TOML snippet → all/messages/true).
- [ ] **Step 5:** `cargo test config::` → PASS.

## Task 13: docs + final verification

**Files:** Modify `README.md`, `CLAUDE.md`

- [ ] **Step 1:** README: add keys (`a`,`?`,`f`,`T`,`g`/`G`); note aiTitle headline, message count, permission/skill badges, files-changed + file search, config file; correct "How It Works" — scan is single-pass full-file (not head/tail).
- [ ] **Step 2:** CLAUDE.md: update Key Concepts (full-file scan, aiTitle in name priority, INDEX_VERSION=4, new fields), add `config.rs` to architecture.
- [ ] **Step 3:** `cargo test` (all) + `cargo build --release` → PASS. `cargo clippy` clean.

---

## Self-review notes
- Spec coverage: approved items 1,3,4,5 → Tasks 1-3,5,11; missing-features (files, badges, tools, config) → Tasks 1,4,8,9,10,12; polish (clamp/%, nav, msg-sort, docs) → Tasks 3,6,7,13. Enter=yolo intentionally untouched.
- Type consistency: preview tuple `(String,String,Speaker)` applied in Tasks 8 across app/preview/ui together (single compile unit). `message_count: u32` set in Tasks 2,3. `INDEX_VERSION=4` once (Task 2).
- Ordering: data (1,2) before consumers (3-12) so each `cargo test` stays green.
