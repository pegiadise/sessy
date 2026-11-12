# Changelog

All notable changes to sessy. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

Each version's section doubles as its GitHub release notes (`scripts/release-notes.sh vX.Y.Z`).

## [Unreleased]

### Added

- Sessions open in a running Claude Code are marked `●` (yellow while Claude is working; macOS and Linux), and the preview title says so. Resuming one asks first, since two Claude Code processes on one session interleave their turns.
- While searching, the preview marks the search words and opens at the first message that has them all (past the pinned recap); `n`/`N` walk the hits. If the hits are only in text the preview doesn't show (tool output, thinking), its title says so.
- Search results show where they matched on the row's third line: `└ match: …the text around it…`, as written (not lowercased), with the words marked. Words close together show as one piece, words far apart each get their own (`… · …`); a hit only in a changed file's path shows `└ file:` and the path.
- `v` toggles compact rows, one line per session (config `density = "compact"`).
- `F` resumes a session as a fork (`--fork-session`), leaving the original untouched.
- `u` undoes the last delete. Deleted sessions are kept in sessy's cache folder for 7 days (when it's on another disk than the sessions, deletes are permanent as before).
- `[` / `]` jump between the messages you wrote in the preview; `Enter`, `l`, `F` and `p` work from the preview as in the list (while a find is active, `Enter` goes to the next hit).
- `z` zooms to one pane at a time, full width.
- A progress line while the index builds (the first run reads every session).
- Search filters alongside words: `project:`, `branch:`, `file:`, `is:open` / `is:pinned` / `is:pr`, `after:` / `before:` (`7d` or a date) and `on:` (`today`, `yesterday`, a date), e.g. `auth project:web is:open after:7d`. Filters are coloured in the search box.
- The timeline is interactive: the arrows pick a day and `Enter` lists its sessions, keeping the rest of the search.
- Sessions that span several days get a date rule in the preview where each day starts.
- Mouse support, opt-in with `--mouse` or `mouse = true`: the wheel scrolls, a click selects, a double-click resumes.

### Changed

- Terminals narrower than 100 columns show one pane at a time, full width; `Tab` switches.
- Timestamps read `today 14:32`, a weekday for the past week, then the date; older years show the year.
- Launched in a directory with no sessions of its own, sessy shows all projects instead of an empty list.
- Search covers the conversation only, not the system prompt, tool listings and CLAUDE.md that newer Claude Code also logs in each session; words like "docx" no longer match nearly every session. The search cache is about half the size.
- "Last active" (the list's date, date sort, `--recent`, `on:`, the timeline) is the session's last timestamped entry, not the file's modification time, which Claude Code moves when it appends bookkeeping weeks later.
- A config value sessy doesn't know, or an unknown key, is reported in the status bar; `enter` set to anything but `yolo` (a typo included) means the safe resume.
- `--purge` keeps sessions open in Claude Code and says which projects the sessions it would delete are in.
- `Esc` clears an active size filter before it quits.
- Much faster start: the search cache is no longer rewritten on every launch. Unchanged sessions keep their place, changed ones are appended, and nothing is written when nothing changed (about 10 ms instead of 200 ms on a 4 GB history).

### Fixed

- Prompts typed while Claude was still working were missing from the preview, the message count, "left off" and search (Claude Code stores them differently).
- A delete or resume prompt, or a rename, could act on a different session than the one it named when the list changed under it (a session closing elsewhere while `is:open` was searched). They now stay on their session, and a `y` arriving before the prompt was even drawn (pasted text) doesn't confirm it.
- Text pasted as keystrokes (Windows Terminal, terminals without bracketed paste) ran list commands; "study" could delete a session. It's searched for now. Pasted line breaks no longer glue words together.
- On Windows, AltGr characters (`@`, `\`, `{`, `€` on European layouts) can be typed in the inputs.
- The session name vanished from rows with a long project and branch in the two-pane layout.
- In the preview, a phrase wrapped across two rows wasn't found by `n`/`N` or highlighted; resizing, `z` or `T` lost the reading position and `T` dropped an active find.
- Clearing a search left its marks in the preview; unpinning under `is:pinned` left the preview on the unpinned session; a second load of the same preview reset the position.
- Moving through huge sessions stalled for a fraction of a second each time (the cached conversation was copied and re-wrapped); the preview cache is also capped by size now.
- The timeline's count ignored the size filter; list paging was off by one in comfortable rows.
- A damaged index file crashed sessy on every start until `--rebuild-index`; now it's rebuilt. A config folder without a `projects/` folder no longer empties the cache.
- A failed undo (the session came back meanwhile) no longer blocks undoing earlier deletes.
- Pins are written atomically and merged with another running sessy's; a damaged pins file is set aside instead of overwritten.
- Tickets at the start of a line were missed, and the system prompt's IDs were taken for tickets. Indexing big sessions is about twice as fast.
- Editor context Claude Code adds to a prompt (an opened file, a selection) no longer becomes the session's title.
- Keys typed while the index was building ran as commands once the list appeared; they're dropped now.
- The help screen was cut off on short terminals; it scrolls now.
- On narrow panes a session's stats line was clipped mid-word; it now drops the size tag, PR badge and message count, in that order, to fit.
- List and preview text was set to white, which is unreadable on light terminal themes; it uses the terminal's own text color now.

## [1.5.0] - 2026-10-22

### Changed

- Search lists exact matches first: sessions containing the whole query as whole words (or the exact ticket/PR number), with the match highlighted. Partial matches follow below a "partial matches" divider. Exact matches stay on top of bookmarks and of any sort, and the search bar counts them.
- A `/` doesn't end a word, so searching part of a longer URL or path (`github.com/you` inside `github.com/you/repo`) gives a partial match, not an exact one.

## [1.4.0] - 2026-10-01

### Added

- Pasting while browsing the list or preview (`⌘V`, `Ctrl+V`, or the terminal's own paste) searches for the pasted text right away, replacing the current query.
- `Ctrl+V` reads the system clipboard when the terminal sends it as a key instead of pasting, as most Linux terminals and some Windows consoles do. It also works in the search, find-in-preview and rename inputs. Windows Terminal types `Ctrl+V` pastes as keystrokes, so there press `/` before pasting (or unbind `Ctrl+V` in its settings to let sessy read the clipboard).

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

[Unreleased]: https://github.com/pegiadise/sessy/compare/v1.5.0...HEAD
[1.5.0]: https://github.com/pegiadise/sessy/compare/v1.4.0...v1.5.0
[1.4.0]: https://github.com/pegiadise/sessy/compare/v1.3.0...v1.4.0
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
