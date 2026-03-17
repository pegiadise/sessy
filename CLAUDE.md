# sessy

TUI session manager for Claude Code — browse, search, preview, and resume conversations.

## Architecture

```
src/
  main.rs       — CLI (clap), event loop, post-TUI actions (launch/yank/print/purge)
  app.rs        — App state, focus/view modes, sort/filter/bookmark/search logic
  ui.rs         — Two-pane ratatui rendering: session list + conversation preview + timeline
  index.rs      — Filesystem scanner, bincode cache (~/.cache/sessy/index.bin), incremental rebuild
  parser.rs     — JSONL parser: head/tail reads, human message detection, conversation extraction
  session.rs    — SessionMeta struct, formatting helpers (duration, file size, size category)
  preview.rs    — Background thread preview loader with mpsc channel and FIFO cache
  bookmarks.rs  — Bookmark persistence (~/.cache/sessy/bookmarks.json)
  export.rs     — Markdown export of session conversations
```

## Key Concepts

- **Claude Code sessions** are JSONL files at `~/.claude/projects/<encoded-path>/<uuid>.jsonl`
- **Path encoding**: Claude replaces `/` with `-`, so `/Users/me/code/foo` → `-Users-me-code-foo`
- **Head read**: First ~10 lines for title, branch, slug, cwd, first timestamp
- **Tail read**: Last 8KB for last human message, last timestamp, `/rename` command
- **Human message detection**: `type=="user"` AND `message.content` is string AND no `toolUseResult` AND `isMeta` is not true
- **Index cache**: bincode v1 serialized with version header (bump `INDEX_VERSION` when `SessionMeta` changes)
- **Session name priority**: `/rename` value > `slug` field > empty
- **View pipeline**: search → size filter → sort (bookmarked first, then by current sort mode)
- **Preview cache**: FIFO-ordered HashMap, max 10 entries

## Building

```
cargo build           # dev build
cargo test            # unit tests
cargo build --release # optimized build
```

## Conventions

- Rust 2024 edition, MSRV 1.86
- No `unwrap()` in non-test code — use `ok()?`, `unwrap_or_default()`, or `unwrap_or_else()`
- Parallel scanning with rayon, background preview with std::sync::mpsc
- Status bar keybinding style: Cyan bold key + Rgb(180,180,180) description on Rgb(40,40,40) bg
- Size categories: quick <1MB (green), medium 1-10MB (yellow), deep 10-30MB (magenta), massive >30MB (red)
- Filter out `gitBranch: "HEAD"` — it's noise from detached HEAD states
- Timeline heatmap uses GitHub-style green color scale
- Bookmarked sessions float to top of any sort order

## Publishing

```
cargo publish  # from clean git state
```

Package name is `sessy` on crates.io. Bump version in `Cargo.toml` before publishing.
