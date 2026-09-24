# sessy

TUI session manager for Claude Code — browse, search, preview, and resume conversations.

FOSS Rust CLI, published on crates.io (`cargo install sessy`). Doubles as a studio lead-gen footprint: the crate `homepage`/`repository` metadata and README backlink point to agileturtles.gr (see `Cargo.toml`).

- **Crate / binary**: `sessy` — current published version **1.2.0** (matches `Cargo.toml`; verify there before assuming).
- **Repo**: github.com/pegiadise/sessy (also a git checkout here).
- License MIT, Rust 2024 edition, MSRV 1.86.

## Architecture

```
src/
  main.rs       — CLI (clap), event loop (drains queued events, then searches/redraws once), key routing per focus, post-TUI actions (launch/print/purge)
  app.rs        — App state; focus/view modes; sort/scope/size filter; bookmark/search/rename/copy; selection preservation
  ui.rs         — Two-pane ratatui rendering: session list + preview/files + timeline + help overlay + context-sensitive status bar; wrapping helpers
  clipboard.rs  — Copy (pbcopy / wl-copy / xclip / xsel / copypasta / OSC 52) and open-URL
  index.rs      — Filesystem scanner, bincode cache (~/.cache/sessy/index.bin), incremental rebuild
  input.rs      — TextInput: single-line editor with movable cursor (search bars)
  parser.rs     — JSONL single-pass scanner; human message detection; conversation extraction (with optional tool lines)
  session.rs    — SessionMeta struct, formatting helpers (duration, file size, size category)
  preview.rs    — Single background preview worker (skips to the newest request) + FIFO cache
  text_cache.rs — mmap'd companion (~/.cache/sessy/text.bin) holding searchable conversation text (user + assistant + thinking + tool I/O)
  config.rs     — Optional ~/.config/sessy/config.toml (scope, sort, show_tool_activity, enter); parse errors surface in the status bar
  bookmarks.rs  — Bookmark persistence (~/.cache/sessy/bookmarks.json)
  export.rs     — Markdown export of session conversations
```

## Key Concepts

- **Claude Code sessions** are JSONL files at `~/.claude/projects/<encoded-path>/<uuid>.jsonl` (`$CLAUDE_CONFIG_DIR/projects` when set). Top-level `agent-*.jsonl` (old subagent transcripts) are skipped
- **Path encoding**: Claude replaces every non-alphanumeric char with `-`, so `/Users/me/code/foo` → `-Users-me-code-foo`
- **Single-pass scan**: `parser::scan_session` reads the whole file once, extracting head meta (title/branch/slug/cwd/first ts), tail meta (last human message/ts/rename), AI title (`type:"ai-title"`), custom title (`type:"custom-title"` — what `/rename` writes today; legacy `/rename` local-command args are the fallback), permission mode, Claude Code version, skills (`attributionSkill`), changed files (`file-history-snapshot` → `trackedFileBackups` + `file-history-delta` → `trackingPath`), PRs (`type:"pr-link"`, also added to tickets as `#N`), recap (`system`/`away_summary`), tickets, and the human message count. Title/`left off` are derived from the human messages it finds — no separate head/tail seek
- **Human message detection** (`parser::human_text`, shared by scan + preview + export): `type=="user"`, not sidechain, no `toolUseResult`, not `isMeta`/`isCompactSummary`/`isVisibleInTranscriptOnly`, `origin.kind` absent or `"human"` (excludes task notifications and peer-session messages), content is a string **or** a block array (text blocks joined; image-only → `[Image]`), not `[Request interrupted by user…]`, and not command noise (`<command-name>`, `<local-command-stdout>`, `<task-notification>`, `<bash-input>`, …) — except a slash command **with arguments** counts as `"/name args"`. Command-only sessions fall back to the slash-command name as title. Titles/left-off go through `parser::one_line` (pasted_content tags stripped, whitespace folded, ANSI/control chars dropped)
- **Scope**: `Scope::Current` = sessions whose recorded `cwd` is under the launch project root (nearest `.git` ancestor below `$HOME`, else the launch dir); sessions without a cwd fall back to the encoded dir name. `--project` implies `Scope::All`
- **Rename** (`r`) appends a `custom-title` entry (Claude Code's own format) and restores the file mtime so the session keeps its date slot
- **Real format evolves**: before changing the parser, survey real files (`~/.claude/projects/*/*.jsonl`) for entry `type`s and user-content shapes; `tests/fixtures/current_format_session.jsonl` captures the 2026-09 shapes
- **Index cache**: bincode serialized with version header. `INDEX_VERSION` is **7** — bump it whenever `SessionMeta` *or scan semantics* change. `SessionIndex.text_cache_len` must equal text.bin's size on load, else full rescan (guards against a crash or two instances racing); both files are written temp+rename
- **Session name priority**: custom title (`/rename`) > `aiTitle` > `slug` field > empty
- **View pipeline**: search → scope (cwd vs all) → size filter → sort (bookmarked first, then by current sort mode: date/size/duration/messages)
- **Preview cache**: FIFO-ordered HashMap, max 10 entries. Toggling tool activity (`T`) clears it so lines re-extract. `preview_session_id` set ⇒ loaded or in flight; anything that invalidates lines must clear it
- **Preview line role**: `parser::Speaker` (User/Assistant/Tool/Recap); Tool lines only appear when tool activity is on; the latest recap is pinned as line 0
- **Preview rendering**: `ui::message_rows` (newline-preserving, display-width wrapping) is the single source of truth for row counts — `recompute_preview_offsets`, match scrolling, and `visible_preview_lines` all call it with `App::preview_body_width()`. Scroll/offsets are `usize` (long sessions exceed 65k rows); only on-screen rows are built each frame
- **Selection**: search edits reset to the top match; bookmark/scope/size-filter changes and clearing a search keep the selected session (`reselect`). Search runs once per drained event batch (`search_dirty`), so actions that read `selected` from the search input must apply a pending search first

## Build / test / run

```
cargo build              # dev build
cargo test               # unit tests + tests/search_integration.rs (fixtures in tests/fixtures/)
cargo build --release    # optimized build (lto + strip, see [profile.release])
cargo clippy             # lint (repo is kept clippy-clean)
cargo run -- --all       # run the TUI against all projects
```

CLI flags (see `src/main.rs` / README): default browses sessions for the current project; `--all` (every project), `--project X` (substring filter, implies all projects), `--recent 7d` (1h/7d/2w/1m; invalid values exit with an error), `--print` (emit selected session ID to stdout for `claude --resume $(sessy --print)` — the TUI renders on **stderr** in this mode so stdout stays clean; every pick key prints; quitting exits 1), `--purge` (delete sessions < 15 KB older than 2 days, pinned ones kept; respects `--project`/`--recent`). After a resume, sessy exits with claude's exit code.

Stdout hygiene in `--print` mode: crossterm's kitty-keyboard probe writes its query to stdout when it can't write /dev/tty, so the probe only runs when stdout is a terminal. Anything else that talks to the terminal (OSC 52) goes to /dev/tty.

## Conventions

- Rust 2024 edition, MSRV 1.86
- Tests must not touch the user's real cache/config: `App.bookmarks_file` is `None` in tests (persistence off); main sets it
- No `unwrap()` in non-test code — use `ok()?`, `unwrap_or_default()`, or `unwrap_or_else()`
- Parallel scanning with rayon, background preview with std::sync::mpsc
- Status bar keybinding style: Cyan bold key + Rgb(180,180,180) description on Rgb(40,40,40) bg
- Size categories: quick <1MB (green), medium 1-10MB (yellow), deep 10-30MB (magenta), massive >30MB (red)
- Filter out `gitBranch: "HEAD"` — it's noise from detached HEAD states
- Timeline heatmap uses GitHub-style green color scale
- Bookmarked sessions float to top of any sort order
- List/preview key handlers ignore Ctrl/Alt/Super combos except explicit ones (Ctrl+N/P/D/U/F/B); Ctrl+C quits from any focus; delete confirms only on `y`

## Release / publish

Releases are tagged `vX.Y.Z` (latest `v1.2.0`). Flow:

1. Bump `version` in `Cargo.toml`.
2. Commit (conventional commit, ticket at end) and tag: `git tag vX.Y.Z`.
3. `cargo publish` from clean git state. crates.io token lives in `~/.cargo/credentials.toml`.
4. Push commits + tags to `main` (github.com/pegiadise/sessy).

Package name is `sessy` on crates.io.
