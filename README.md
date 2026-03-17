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
| `/` | Search (fuzzy match across project, branch, title, name) |
| `s` | Cycle sort: date → size → duration |
| `1` `2` `3` `4` | Filter by size: quick / medium / deep / massive (`0` clears) |
| `b` | Bookmark/unpin session (pinned sort to top) |
| `e` | Export session as markdown |
| `t` | Toggle timeline heatmap view |
| `Enter` | Launch `claude --resume` in the session's original directory |
| `y` | Launch with `--dangerously-skip-permissions` (yolo mode) |
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
▸★ Jun 10 14:32  my-app  feat/auth  wiggly-forging-newell
   1h45m  4.2 MB [medium]  "add login endpoint with JWT"
   └ left off: "looks good, ship it"
```

- **Line 1**: Selection/bookmark indicator, timestamp, project, branch, session name
- **Line 2**: Duration, file size with color-coded category, first message
- **Line 3**: Last human message — where you left off

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

Scrollable conversation showing `USER:` and `ASST:` messages. Tool use, system events, and sidechain entries are filtered out. Loaded in background with a 10-entry FIFO cache.

Press `/` while in the preview pane to search within the conversation. Matching messages are highlighted. Use `n`/`N` to jump between matches.

## Export

Press `e` to export the selected session as a markdown file in the current directory. Includes metadata header (project, branch, duration, size) followed by the full conversation.

## Performance

- **Cold start** (first run, ~2,400 files): ~2 seconds
- **Warm start** (cached index): < 100 ms
- Incremental index updates — only re-scans changed files
- Parallel scanning with rayon

## How It Works

Reads JSONL session files from `~/.claude/projects/`. Builds a cached index (`~/.cache/sessy/index.bin`) with head/tail reads — first ~10 lines for title/branch, last 8KB for the "left off" message. No full-file parsing until you preview.

Bookmarks are persisted at `~/.cache/sessy/bookmarks.json`.

## License

MIT
