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
sessy              # Browse sessions from current directory
sessy --all        # Browse all sessions across all projects
sessy --project X  # Filter to project name (substring match)
sessy --recent 7d  # Only show last 7 days (supports: 1h, 7d, 2w, 1m)
sessy --print      # Select and print session ID to stdout
sessy --purge      # Delete all sessions < 15 KB and older than 2 days
```

Scripting: `claude --resume $(sessy --print)`

## Keybindings

| Key | Action |
|-----|--------|
| `j` / `k` or `Up` / `Down` | Navigate sessions (wraps around) |
| `g` / `G` or `Home` / `End` | Jump to top / bottom |
| `PgUp` / `PgDn` | Jump a full page |
| `/` | Search (project, title, branch, name, changed files, tickets, message text) |
| `s` | Cycle sort: date → size → duration → messages |
| `1` `2` `3` `4` | Filter by size: quick / medium / deep / massive (`0` clears) |
| `a` | Toggle scope: current directory ↔ all projects |
| `b` | Bookmark/unpin session (pinned sort to top) |
| `e` | Export session as markdown |
| `t` | Toggle timeline heatmap view |
| `f` | Show files changed in the selected session |
| `T` | Toggle tool-use activity in the preview |
| `?` | Keybinding help overlay |
| `Enter` | Launch with `--dangerously-skip-permissions` (yolo mode) |
| `l` | Launch `claude --resume` (safe mode) |
| `c` | Copy `claude --resume <id>` to clipboard |
| `p` | Print session ID to stdout and exit |
| `d` | Delete session (with confirmation) |
| `Tab` | Switch focus to preview pane |
| `/` (in preview) | Search within conversation (`n`/`N` for next/prev match) |
| `Esc` | Clear search / exit search / exit preview / quit |
| `q` | Quit |

## Session List

Each session shows three lines:

```
▸★ Jun 10 14:32  my-app  feat/auth  Add JWT login endpoint
   1h45m  4.2 MB [medium] · 42 msgs  "add login endpoint with JWT"
   └ left off: "looks good, ship it"
```

- **Line 1**: Selection/bookmark indicator, timestamp, project, branch, session name
- **Line 2**: Duration, file size with color-coded category, message count, first message
- **Line 3**: Last human message — where you left off

The session name prefers Claude Code's AI-generated title, falling back to a `/rename` value or the session slug.

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

Scrollable conversation showing `USER:` and `ASST:` messages. System events and sidechain entries are filtered out. Loaded in background with a 10-entry FIFO cache. The pane title shows a scroll position percentage, plus badges for the session's permission mode (`plan` / `accept` / `yolo`) and any skills it used.

- Press `T` to fold tool-use activity into the preview (`TOOL:` lines summarising each Edit/Bash/etc. call).
- Press `f` to list the files the session changed.
- Press `/` while in the preview pane to search within the conversation. Matching messages are highlighted; use `n`/`N` to jump between matches.

## Export

Press `e` to export the selected session as a markdown file in the current directory. Includes metadata header (project, branch, duration, size) followed by the full conversation.

## Config

Optional config at `~/.config/sessy/config.toml`. All keys are optional:

```toml
scope = "current"          # "current" (launch dir) or "all" (every project)
sort = "date"              # date | size | duration | messages
show_tool_activity = false # start with tool-use lines shown in the preview
```

A missing or malformed file falls back to these defaults. The `--all` flag overrides `scope`.

## Performance

- **Warm start** (cached index): < 100 ms
- **Cold start** (first run): a couple of seconds for a few thousand files
- Incremental index updates — only changed files are re-scanned; unchanged sessions reuse their cached text
- Parallel scanning with rayon

## How It Works

Reads JSONL session files from `~/.claude/projects/`. A single-pass scanner extracts per-session metadata — title, branch, slug, cwd, AI title, permission mode, skills, changed files, tickets, message count, the "left off" message, and the full human-message text used for search — into a cached index (`~/.cache/sessy/index.bin`), with the searchable text stored in an mmap'd companion (`~/.cache/sessy/text.bin`). Full conversations are only parsed when you open a preview.

Bookmarks are persisted at `~/.cache/sessy/bookmarks.json`.

## License

MIT
