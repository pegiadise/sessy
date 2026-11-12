# sessy

A two-pane TUI for browsing, searching, and resuming [Claude Code](https://claude.ai/claude-code) sessions.

Claude Code stores thousands of session files as JSONL — `sessy` gives you instant search, conversation preview, and one-key resume instead of guessing from timestamps.

## Install

If you don't have Rust installed:

```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then:

```
cargo install sessy
```

Requires Rust 1.86+.

## Usage

```
sessy              # Browse sessions for the current project
sessy --all        # Browse all sessions across all projects
sessy --project X  # Only projects whose name contains X (searches all projects)
sessy --recent 7d  # Only show last 7 days (supports: 1h, 7d, 2w, 1m)
sessy --print      # Pick a session and print its ID to stdout
sessy --purge      # Delete all sessions < 15 KB and older than 2 days
                   # (pinned sessions are kept; combines with --project / --recent)
```

"The current project" is the enclosing git checkout (or the launch directory outside a repo) and everything below it, so launching from `repo/src` still finds sessions started at `repo/`, and a worktree under `repo/.worktrees/` counts too.

Scripting: `id=$(sessy --print) && claude --resume "$id"` — `--print` exits 1 when you quit without picking.

Sessions are read from `~/.claude/projects/` (or `$CLAUDE_CONFIG_DIR/projects`).

## Keybindings

Press `?` in the app for the full list.

### Session list

| Key | Action |
|-----|--------|
| `j` / `k` or `↑` / `↓` | Navigate sessions (wraps around; also `Ctrl+N` / `Ctrl+P`) |
| `g` / `G` or `Home` / `End` | Jump to first / last |
| `PgUp` / `PgDn` | Page (also `Ctrl+U` / `Ctrl+D`) |
| `Enter` | Resume with `--dangerously-skip-permissions` (yolo mode; see `enter` in [Config](#config)) |
| `l` | Resume with normal permissions |
| `F` | Resume as a fork (`--fork-session`: a new session ID, the original stays as it was; permissions follow `Enter`) |
| `c` | Copy a ready-to-run `cd <dir> && claude --resume <id>` command (stays open) |
| `p` | Print session ID to stdout and exit |
| `/` | Search (title, name, project, branch, changed files, tickets/PR numbers, full conversation text). Exact matches (the whole query as whole words; a `/` doesn't end a word, so part of a longer URL or path is partial) come first with the match highlighted; partial matches follow below a divider |
| `⌘V` / `Ctrl+V` | Search for the pasted text right away (replaces the current query) |
| `s` | Cycle sort: date → size → duration → messages (while searching: relevance → date → …) |
| `1` `2` `3` `4` | Filter by size: quick / medium / deep / massive (`0` clears) |
| `a` | Toggle scope: current project ↔ all projects |
| `b` | Pin / unpin (pinned sessions sort to the top) |
| `r` | Rename — writes the same entry as Claude Code's `/rename`, so `claude --resume` shows it too |
| `o` | Open the pull request linked to the session |
| `e` | Export session as markdown |
| `d` | Delete session (confirm with `y`) |
| `u` | Undo the last delete |
| `v` | Toggle compact rows (one line per session) |
| `z` | Zoom: show one pane at a time, full width (`Tab` switches) |
| `t` | Toggle timeline heatmap view |
| `f` | Show files changed in the selected session |
| `T` | Toggle tool-use activity in the preview |
| `Tab` / `→` | Focus the preview pane |
| `Esc` | Clear the search; with no search, quit |
| `q` / `Ctrl+C` | Quit |

### Search input

Type to filter; `↑` / `↓` move through the results without leaving the input, `Enter` moves focus to the list, `Esc` clears. Pasting works as one edit; `Ctrl+V` reads the system clipboard when the terminal doesn't paste on its own (pbpaste / wl-paste / xclip / xsel, or the Windows clipboard). Windows Terminal pastes `Ctrl+V` as keystrokes, so press `/` first there.

| Key | Action |
|-----|--------|
| `←`/`→`, `⌥←`/`⌥→`, `Home`/`End`/`⌘←`/`⌘→` | Move the cursor (char / word / line) |
| `⌫`, `⌥⌫`, `⌘⌫` | Delete char / word / to line start (also `Ctrl+W`, `Ctrl+U`, `Ctrl+K` to line end) |

### Preview pane

| Key | Action |
|-----|--------|
| `j` / `k` or `↑` / `↓` | Scroll |
| `PgUp` / `PgDn` / `Space` | Page (also `Ctrl+U` / `Ctrl+D`) |
| `g` / `G` | Top / end of the conversation |
| `[` / `]` | Previous / next message you wrote |
| `/` | Search within the conversation; `Enter` keeps the matches, `n` / `N` jump between them |
| `n` / `N` | While a session search is active: next / previous hit of its words |
| `Enter` / `l` / `F` / `p` | Resume, fork or print, as in the list (while a find is active, `Enter` goes to the next hit instead) |
| `T` / `f` | Tool activity / changed files |
| `Tab` / `←` / `Esc` | Back to the list (`Esc` first clears an active search) |

## Session List

Each session shows three lines:

```
▸★● today 14:32   my-app  feat/auth  Add JWT login endpoint
    1h45m  4.2 MB [medium] · 42 msgs · PR #128  "add login endpoint with JWT"
    └ left off: "looks good, ship it"
```

- **Line 1**: Selection, pin and live indicators, when it was last active (`today 14:32`, a weekday for the past week, then the date), project, branch, session name
- **Line 2**: Duration, file size with color-coded category, message count, linked PR, first message
- **Line 3**: Last message you sent — where you left off. While searching, results whose hit isn't visible in the row show it here instead: `└ match: …the text around the hit…` (or `└ file:` for a changed file's path)

`●` marks a session that's open in a running Claude Code right now (yellow while Claude is working; macOS and Linux). Resuming one asks first: two Claude Code processes on one session interleave their turns. `F` (fork) never asks.

Press `v` for compact rows (one line per session, about four times as many on screen), or set `density = "compact"`.

Terminals narrower than 100 columns show one pane at a time, full width; `Tab` switches between the list and the preview. `z` does the same at any width.

The session name prefers an explicit `/rename` value, falling back to Claude Code's AI-generated title or the session slug. Messages count what you typed: slash commands with arguments and messages with pasted images count; tool results, compaction summaries, background-task notifications and messages from other sessions don't.

### Size Categories

| Tag | Size | Color |
|-----|------|-------|
| `[quick]` | < 1 MB | Green |
| `[medium]` | 1 – 10 MB | Yellow |
| `[deep]` | 10 – 30 MB | Magenta |
| `[massive]` | > 30 MB | Red |

## Timeline

Press `t` to see a GitHub-style contribution heatmap of your Claude Code activity over the past weeks. Shows total sessions, active days, and peak day.

## Preview Pane

Scrollable conversation showing `USER:` and `ASST:` messages with their paragraphs and code intact. Claude Code's latest "while you were away" recap, when there is one, is pinned at the top. Sidechain (subagent) entries are filtered out. Loaded in the background with a 10-entry cache. The pane title shows the session's permission mode (`plan` / `accept` / `yolo`), skills it used, and linked PRs; the scroll position is on the right.

- Press `T` to fold tool-use activity into the preview (`TOOL:` lines summarising each Edit/Bash/etc. call).
- Press `f` to list the files the session changed.
- While a session search is active, the preview marks its words and opens at the first hit; `n`/`N` walk the hits. When the hits are only in text the preview doesn't show (tool output, thinking), the pane says so.
- Press `[` / `]` to jump between the messages you wrote.
- Press `/` while in the preview pane to search within the conversation. Matching messages are highlighted; `Enter` commits the search so `n`/`N` jump between matches, `Esc` clears it.

## Export

Press `e` to export the selected session as `<name>-<id>.md` in the current directory. Includes a metadata header (session ID, project, branch, PRs, duration, size) followed by the conversation.

## Config

Optional config at `~/.config/sessy/config.toml` (or `$XDG_CONFIG_HOME/sessy/config.toml`; the platform config dir, e.g. `~/Library/Application Support/sessy/` on macOS, also works). All keys are optional:

```toml
scope = "current"          # "current" (this project) or "all" (every project)
sort = "date"              # date | size | duration | messages
show_tool_activity = false # start with tool-use lines shown in the preview
enter = "yolo"             # "yolo" (--dangerously-skip-permissions) or "safe"
density = "comfortable"    # "comfortable" (three lines per session) or "compact" (one)
```

A missing file means defaults; a malformed one also falls back to defaults, with the parse error shown in the status bar at startup. `--all` and `--project` override `scope`. Launched in a directory with no sessions of its own, sessy shows all projects.

## Delete and undo

`d` then `y` deletes a session; `u` brings it back while sessy is still open. Deleted sessions are kept in sessy's cache folder (`trash/`) for 7 days before they're removed for good. `--purge` deletes permanently.

## Performance

- **Warm start** (cached index): < 100 ms
- **Cold start** (first run): a few seconds for a few thousand files, with a progress line while it indexes
- Incremental index updates — only changed files are re-scanned; unchanged sessions reuse their cached text
- Parallel scanning with rayon; the preview renders only the rows on screen, so huge sessions stay smooth

## How It Works

Reads JSONL session files from `~/.claude/projects/`. A single-pass scanner extracts per-session metadata — title, branch, slug, cwd, custom and AI titles, permission mode, skills, changed files, linked PRs, recap, tickets, message count, the "left off" message, and the full conversation text used for search (user + assistant messages, thinking, tool inputs and outputs) — into a cached index (`~/.cache/sessy/index.bin`), with the searchable text stored in an mmap'd companion (`~/.cache/sessy/text.bin`). Full conversations are only parsed when you open a preview.

Bookmarks are persisted at `~/.cache/sessy/bookmarks.json`. On macOS the cache folder is `~/Library/Caches/sessy/`.

Live sessions come from the files Claude Code keeps for each running process in `~/.claude/sessions/`.

## License

MIT

---

Built by [agile turtles](https://agileturtles.gr) — see [more tools](https://agileturtles.gr/en/products).
