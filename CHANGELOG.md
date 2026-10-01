# Changelog

All notable changes to sessy. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

Each version's section doubles as its GitHub release notes (`scripts/release-notes.sh vX.Y.Z`).

## [Unreleased]

### Added

- Pasting while browsing the list or preview (`⌘V`, `Ctrl+V`, or the terminal's own paste) searches for the pasted text right away, replacing the current query.
- `Ctrl+V` reads the system clipboard when the terminal sends it as a key instead of pasting, as most Linux terminals and some Windows consoles do. It also works in the search, find-in-preview and rename inputs.

## [1.3.0] - 2026-09-24

Catches up with the current Claude Code session format and makes day-to-day browsing smoother. The first launch rebuilds the index once (a few seconds).

### Added

- `r` renames a session, writing the same entry as Claude Code's `/rename`, so `claude --resume` shows the new name too. The session keeps its place in date order.
- Pull requests linked to a session show as a `PR #N` badge, are searchable (`#N`), and open with `o`.
- Claude Code's latest "while you were away" recap is pinned at the top of the preview.
- `↑`/`↓` (and `Ctrl+N`/`Ctrl+P`, `PgUp`/`PgDn`) move through search results without leaving the search input.
- `s` sorts search results too: relevance → date → size → duration → messages.
- Preview: `g`/`G` jump to the top/end, `Space` and `Ctrl+D`/`Ctrl+U` page, `←` returns to the list; `→` in the list opens the preview.
- `?` works from the preview pane; the help screen lists every key, grouped by pane.
- Pasting into search inputs is a single edit (bracketed paste).
- Config `enter = "safe"` makes Enter resume without `--dangerously-skip-permissions` (default stays `"yolo"`).
- `CLAUDE_CONFIG_DIR` is honoured when locating sessions.

### Changed

- The default scope is the current project: the enclosing git checkout (or the launch directory) and everything below it, so launching from `repo/src` finds sessions started at `repo/`, and worktrees inside the repo count.
- `c` copies a ready-to-run `cd <dir> && claude --resume <id>` and keeps sessy open (it used to exit).
- Deleting asks for `y`; pressing `d` twice no longer deletes.
- `Esc` in the list clears an active search before it quits, keeping the selected session.
- The status bar shows only the keys that work in the focused pane and never wraps.
- `--project` searches across all projects instead of only the launch directory.
- `--print` exits with status 1 when you quit without picking, and every pick key (`Enter`, `l`, `p`) prints instead of launching. After resuming, sessy exits with claude's exit code.
- `--purge` keeps pinned sessions.
- Durations over a day read `3d4h`; exports are named `<name>-<id>.md` so sessions with the same name don't overwrite each other, and include the session ID and PR links.
- The list title shows which project is in scope; the preview shows its scroll position on the right.

### Fixed

- Sessions renamed with a recent Claude Code showed no name (`custom-title` entries were ignored).
- Compaction summaries ("This session is being continued…") became titles and appeared as user messages.
- Messages with pasted images were dropped, and image-only sessions were hidden.
- Slash commands with arguments (`/loop fix the tests`) were discarded; messages from other sessions and background-task notifications counted as yours.
- Files changed in newer sessions were missing from the files view (`file-history-delta`).
- The preview joined all lines of a message into one block; paragraphs, lists, and code now keep their shape. Emoji and CJK text wrap by display width.
- `claude --resume $(sessy --print)` received terminal escape codes before the session ID (since 1.1.0).
- On macOS, `~/.config/sessy/config.toml` was ignored; a malformed config is now reported in the status bar instead of being silently ignored.
- `Ctrl`+letter combinations triggered the plain-letter actions (`Ctrl+D` opened delete, `Ctrl+L` resumed); `Ctrl+C` didn't quit while typing a search.
- Pinning or unpinning moved the cursor to a different session and left a stale preview.
- The list jumped to keep the selection on its bottom row when moving back up.
- Very long sessions couldn't scroll past about 65,000 rows.
- Very long pasted queries froze the search box, and long queries now scroll horizontally.
- Idle CPU use with a large session selected (the whole conversation was re-wrapped 20 times a second), and a background parse was started for every keypress while scrolling the list.
- Titles and previews could carry terminal control sequences from pasted logs.
- The index and its search-text cache could fall out of sync after a crash or when two instances ran at once.
- `--recent` with an absurdly large value overflowed; old `agent-*.jsonl` subagent transcripts were listed as sessions.
- Running the test suite overwrote the real bookmarks file.

## [1.2.0] - 2026-08-11

### Added

- Movable cursor in both search inputs: arrow, word (`⌥←`/`⌥→`), and line (`Home`/`End`, `⌘←`/`⌘→`) movement; insert and delete at the cursor; macOS `⌘⌫`/`⌥⌫` semantics and readline `Ctrl+A`/`E`/`W`/`U`/`K`.

## [1.1.0] - 2026-08-11

### Added

- Search covers the full conversation text: assistant replies, thinking, tool inputs and tool results, not only your own messages.
- The kitty keyboard protocol is enabled when the terminal supports it, so `⌘⌫` and `⌥⌫` reach the search inputs.

## [1.0.1] - 2026-08-03

No user-facing changes: test fixtures use generic example paths. (The `v1.0.0` tag sits on a release commit that didn't land on `main`; 1.0.1 carries the same code.)

## [1.0.0] - 2026-08-03

### Fixed

- `--print` rendered the TUI on stdout, so `claude --resume $(sessy --print)` captured escape sequences instead of the session ID. The TUI now renders on stderr in print mode.
- Preview search: `Enter` keeps the matches so `n`/`N` work; `Esc` cancels. `Esc` in the preview clears an active search first.
- Stale preview-search matches are cleared when the previewed session changes.
- `--purge` ran before `--project`/`--recent` were applied and deleted across all projects; it now respects both.
- Invalid `--recent` values are rejected instead of silently ignored.
- Slash-command turns no longer pollute titles, "left off" lines, message counts, search, or previews; command-only sessions are titled with the command.
- The scope filter encodes paths the way Claude Code does, so directories with dots (like `.worktrees`) match.
- Malformed `/rename` entries and garbage JSONL lines can no longer crash the scan.

### Changed

- The status bar says "Enter print" in `--print` mode; the "Copied:" confirmation goes to stderr.

## [0.5.1] - 2026-06-14

No code changes: crate metadata and README link to the project homepage.

## [0.5.0] - 2026-06-04

### Added

- Current Claude Code session format: AI-generated titles as the session headline (`/rename` > AI title > slug), permission mode, Claude Code version, skills, and changed files.
- Message counts and a "messages" sort mode.
- Changed-file paths are searchable ("which session touched X").
- `a` toggles scope between the current directory and all projects.
- `?` help overlay, `f` changed-files view, `T` tool activity in the preview.
- `g`/`G`/`Home`/`End` navigation; preview scroll clamping and position percentage.
- Optional config file at `~/.config/sessy/config.toml`.

## [0.4.1] - 2026-05-13

### Added

- Readline-style editing in the search inputs: `Ctrl+W`/`⌥⌫` delete a word, `Ctrl+U`/`⌘⌫` clear.

## [0.4.0] - 2026-05-13

### Changed

- Search rewritten: ticket-aware substring search (`PROJ-123`, `#456`) with relevance ranking across name, title, project, branch, and message text, scanned in parallel over a memory-mapped text cache. Replaces fuzzy matching.

### Fixed

- Preview search scrolls to the wrapped row that contains the match.
- Highlighting handles characters whose lowercase form changes length (Greek, Turkish).
- Deleting a session removes its bookmark.
- The status bar shows `sort:relevance` while a query is active.

## [0.3.0] - 2026-03-18

### Changed

- **Breaking:** `Enter` resumes with `--dangerously-skip-permissions`; use `l` for a normal resume (the old `Enter`).

### Added

- Distinct colours for timestamp, project, branch, and session name; the preview title shows the session name; the search bar shows the result count.
- `PgUp`/`PgDn` in the list and preview.

## [0.2.3] - 2026-03-17

Covers 0.2.0 – 0.2.3.

### Added

- `e` export as markdown, `t` timeline heatmap, `b` bookmarks (pinned to the top), `1`–`4`/`0` size filters, `/` search within the preview with `n`/`N`, and a duration sort.
- Matching text is highlighted inside preview lines.

### Fixed

- Preview cache eviction, an off-by-one in truncation, quadratic wrapping, a panic on multi-byte `--recent` input, and `--purge` removing sessions from the index when the file delete failed.

## [0.1.10] - 2026-03-13

First release (0.1.0 – 0.1.10): a two-pane TUI to browse, search, preview, and resume Claude Code sessions.

[Unreleased]: https://github.com/pegiadise/sessy/compare/v1.3.0...HEAD
[1.3.0]: https://github.com/pegiadise/sessy/compare/v1.2.0...v1.3.0
[1.2.0]: https://github.com/pegiadise/sessy/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/pegiadise/sessy/compare/v1.0.1...v1.1.0
[1.0.1]: https://github.com/pegiadise/sessy/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/pegiadise/sessy/compare/v0.5.1...v1.0.0
[0.5.1]: https://github.com/pegiadise/sessy/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/pegiadise/sessy/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/pegiadise/sessy/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/pegiadise/sessy/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/pegiadise/sessy/compare/v0.2.3...v0.3.0
[0.2.3]: https://github.com/pegiadise/sessy/compare/v0.1.10...v0.2.3
[0.1.10]: https://github.com/pegiadise/sessy/releases/tag/v0.1.10
