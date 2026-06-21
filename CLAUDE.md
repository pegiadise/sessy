# sessy

TUI session manager for Claude Code — browse, search, preview, and resume conversations.

FOSS Rust CLI, published on crates.io (`cargo install sessy`). Doubles as a studio lead-gen footprint: the crate `homepage`/`repository` metadata and README backlink point to agileturtles.gr (see `Cargo.toml`).

- **Crate / binary**: `sessy` — current published version **0.5.1** (matches `Cargo.toml`; verify there before assuming).
- **Repo**: github.com/pegiadise/sessy (also a git checkout here).
- License MIT, Rust 2024 edition, MSRV 1.86.

## Architecture

```
src/
  main.rs       — CLI (clap), event loop, post-TUI actions (launch/yank/print/purge)
  app.rs        — App state; focus/view modes; sort/scope/size filter; bookmark/search; tools/files/help toggles
  ui.rs         — Two-pane ratatui rendering: session list + preview/files + timeline + help overlay + status bar
  index.rs      — Filesystem scanner, bincode cache (~/.cache/sessy/index.bin), incremental rebuild
  parser.rs     — JSONL single-pass scanner; human message detection; conversation extraction (with optional tool lines)
  session.rs    — SessionMeta struct, formatting helpers (duration, file size, size category)
  preview.rs    — Background thread preview loader with mpsc channel and FIFO cache
  text_cache.rs — mmap'd companion (~/.cache/sessy/text.bin) holding searchable human text
  config.rs     — Optional ~/.config/sessy/config.toml (scope, sort, show_tool_activity)
  bookmarks.rs  — Bookmark persistence (~/.cache/sessy/bookmarks.json)
  export.rs     — Markdown export of session conversations
```

## Key Concepts

- **Claude Code sessions** are JSONL files at `~/.claude/projects/<encoded-path>/<uuid>.jsonl`
- **Path encoding**: Claude replaces `/` with `-`, so `/Users/me/code/foo` → `-Users-me-code-foo`
- **Single-pass scan**: `parser::scan_session` reads the whole file once, extracting head meta (title/branch/slug/cwd/first ts), tail meta (last human message/ts/`/rename`), AI title (`type:"ai-title"`), permission mode, Claude Code version, skills (`attributionSkill`), changed files (`file-history-snapshot` → `trackedFileBackups`), tickets, and the human message count. Title/`left off` are derived from the human messages it finds — no separate head/tail seek
- **Human message detection**: `type=="user"` AND `message.content` is string AND no `toolUseResult` AND `isMeta` is not true
- **Index cache**: bincode serialized with version header. `INDEX_VERSION` is **4** — bump it whenever `SessionMeta` changes
- **Session name priority**: `/rename` value > `aiTitle` > `slug` field > empty
- **View pipeline**: search → scope (cwd vs all) → size filter → sort (bookmarked first, then by current sort mode: date/size/duration/messages)
- **Preview cache**: FIFO-ordered HashMap, max 10 entries. Toggling tool activity (`T`) clears it so lines re-extract
- **Preview line role**: `parser::Speaker` (User/Assistant/Tool); Tool lines only appear when tool activity is on

## Build / test / run

```
cargo build              # dev build
cargo test               # unit tests + tests/search_integration.rs (fixtures in tests/fixtures/)
cargo build --release    # optimized build (lto + strip, see [profile.release])
cargo clippy             # lint (repo is kept clippy-clean)
cargo run -- --all       # run the TUI against all projects
```

CLI flags (see `src/main.rs` / README): default browses sessions for cwd; `--all` (every project), `--project X` (substring filter), `--recent 7d` (1h/7d/2w/1m), `--print` (emit selected session ID to stdout for `claude --resume $(sessy --print)`), `--purge` (delete sessions < 15 KB older than 2 days).

## Conventions

- Rust 2024 edition, MSRV 1.86
- No `unwrap()` in non-test code — use `ok()?`, `unwrap_or_default()`, or `unwrap_or_else()`
- Parallel scanning with rayon, background preview with std::sync::mpsc
- Status bar keybinding style: Cyan bold key + Rgb(180,180,180) description on Rgb(40,40,40) bg
- Size categories: quick <1MB (green), medium 1-10MB (yellow), deep 10-30MB (magenta), massive >30MB (red)
- Filter out `gitBranch: "HEAD"` — it's noise from detached HEAD states
- Timeline heatmap uses GitHub-style green color scale
- Bookmarked sessions float to top of any sort order

## Release / publish

Releases are tagged `vX.Y.Z` (latest `v0.5.1`). Flow:

1. Bump `version` in `Cargo.toml`.
2. Commit (conventional commit, ticket at end) and tag: `git tag vX.Y.Z`.
3. `cargo publish` from clean git state. crates.io token lives in `~/.cargo/credentials.toml`.
4. Push commits + tags to `main` (github.com/pegiadise/sessy).

Package name is `sessy` on crates.io.
