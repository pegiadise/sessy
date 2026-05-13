# sessy search overhaul — design

**Status:** approved
**Date:** 2026-05-13

## Goals

1. Replace today's fuzzy search (skim, no threshold, computed score discarded by date sort) with a precise, predictable substring search that ranks results by relevance.
2. Make ticket IDs (`PROJ-123`, `#456`, etc.) the top-ranked signal — the user's most reliable session-recall key.
3. Search the full conversation history (human messages), not just the last message.
4. Keep search interactive: <20 ms p99 per keystroke for a library of hundreds of multi-MB sessions.
5. Fix several latent bugs in adjacent code (preview-search scroll, perf, Unicode highlight, orphan bookmarks, stale sort indicator).

## Non-goals

- Fuzzy matching of any kind. Substring only.
- Tantivy or other full-text engine. Custom in-process index using `memchr` SIMD scan.
- Search operators (`project:foo`, `branch:bar`). Deferred.
- Indexing assistant or tool output. Human messages only. Tickets are extracted from all roles.
- Search history / saved queries.

## Data model

### `SessionMeta` (src/session.rs)

Add four fields, keep all existing fields:

```rust
pub struct SessionMeta {
    // existing: id, project, branch, name, title, last_message,
    // duration_secs, timestamp, file_size, file_mtime, file_path, cwd,
    // message_count

    pub tickets: Vec<String>,   // uppercased, deduped, sorted
    pub text_offset: u64,       // byte offset into text.bin
    pub text_len: u32,          // bytes of this session's human text in text.bin

    // For zero-allocation search of metadata fields:
    pub name_lc: String,        // lowercased copy of name
    pub title_lc: String,       // lowercased copy of title
    pub project_lc: String,     // lowercased copy of project
    pub branch_lc: String,      // lowercased copy of branch
}
```

`*_lc` fields are stored in the cache to eliminate per-keystroke `to_lowercase()` allocations. They are produced once at index time via `str::to_lowercase` (Unicode-aware).

### Two-file cache layout

```
~/.cache/sessy/index.bin   — bincode v1 of SessionIndex { version: u32, sessions: Vec<SessionMeta> }
~/.cache/sessy/text.bin    — concatenated lowercased human text, one slice per session
```

`text.bin` is mmap'd at startup via the `memmap2` crate. No upfront read; OS pages it in lazily as the search scan touches each region.

### `INDEX_VERSION`

Bumped from `2` to `3`. Older caches are discarded automatically by the existing version check.

### Cache invalidation guard

If `text.bin` does not exist on disk, or if `text.bin.len() < max(text_offset + text_len)` across all sessions, the cache is treated as invalid and a full rebuild runs. This protects against partial writes (disk full, killed mid-build).

## Indexing pipeline

### `parser::scan_session(path) -> Option<ScanResult>`

New single-pass scanner. Replaces the two-read combo (`extract_head_meta` + `extract_tail_meta`) for the indexing path.

```rust
pub struct ScanResult {
    pub head: HeadMeta,
    pub tail: Option<TailMeta>,
    pub human_text_lc: String,   // lowercased, '\n' separated
    pub tickets: Vec<String>,    // uppercased, sorted, deduped
}
```

Walks the JSONL once with `BufReader::lines()`. For each line:
- Apply existing head/tail extraction logic (track first/last timestamp, first human message, slug, branch, cwd, rename).
- If it is a human message: lowercase its text (`str::to_lowercase`) and append + `\n` to `human_text_lc`.
- Run the ticket regex against the raw line bytes (matches in any role); collect uppercased tokens into a `HashSet`.

`extract_head_meta` and `extract_tail_meta` stay in place for `preview.rs` and tests — they are read-paths for already-loaded sessions, unaffected by indexing changes.

### Ticket regex

```
\b([A-Z][A-Z0-9]{1,9})-(\d{1,7})\b | #(\d{1,7})\b
```

- ASCII uppercase prefix, 2–10 chars (covers `PROJ`, `ABC`, `FOO123` etc.)
- Numeric suffix 1–7 digits
- `#NNN` form (GitHub-style)
- Word-boundary anchored on both sides

Matches stored uppercased (`#456` stored as `#456`; ticket alphas already uppercase by regex). Deduped via `HashSet`, then sorted before being written to `SessionMeta::tickets` so equality comparisons and binary search are stable.

Compiled once per `build_index` call via `OnceLock<Regex>`.

### `index::build_index` flow

1. Discover all `*.jsonl` files (unchanged).
2. Build a `HashMap<PathBuf, (&SessionMeta, &[u8])>` from the cached index, where `&[u8]` is the session's slice of the **old** `text.bin` (mmap'd).
3. Parallel par_iter over files:
   - If the file's mtime matches the cached entry's `file_mtime`, reuse the cached `SessionMeta` plus its text slice.
   - Otherwise call `parser::scan_session`, build a fresh `SessionMeta`, and produce a fresh text byte slice.
   - Each task returns `(SessionMeta, Vec<u8> | &[u8])`.
4. Serial finalize:
   - Open `~/.cache/sessy/text.bin.tmp` for writing.
   - For each session in stable order: record `text_offset = writer.position()`, write text bytes, update `SessionMeta::text_offset` / `text_len`.
   - `fsync` and rename `text.bin.tmp` → `text.bin`.
   - Write `index.bin` (atomic rename via existing logic).
5. Re-mmap the new `text.bin` and hand to `App`.

### Storage cost

Typical session: 5–50 KB of human text (the bulk of JSONL is tool output, which we exclude). 500 sessions × ~25 KB average = ~12 MB `text.bin`. `index.bin` grows modestly (lowercased copies of name/title/project/branch + ticket vec).

## Search algorithm

### Per-keystroke flow (replaces `app::apply_search_inner`)

Inputs: `search_query: &str`, `sessions: &[SessionMeta]`, mmap'd `text.bin` as `&[u8]`.

```
1. If query empty:
       filtered_indices = (0..sessions.len()).collect()
       run existing apply_sort() with user's chosen mode
       return

2. query_lc = query.to_lowercase()  // single allocation
3. query_upper = query.to_ascii_uppercase()
4. is_ticket_form = regex_match(query_upper, ^[A-Z][A-Z0-9]+-\d+$|^#\d+$)
5. tokens = query_lc.split_whitespace().collect::<Vec<&str>>()
6. finders = tokens.iter().map(|t| memchr::memmem::Finder::new(t)).collect()

7. sessions.par_iter().enumerate().filter_map(|(i, s)| {
       let mut score = 0_i64;

       if is_ticket_form && s.tickets.binary_search(&query_upper).is_ok() {
           score += 1000;
       }

       for (token, finder) in tokens.iter().zip(finders.iter()) {
           let hit;
           if finder.find(s.name_lc.as_bytes()).is_some() || finder.find(s.title_lc.as_bytes()).is_some() {
               score += 500;
               // word-boundary bonus
               if starts_at_word_boundary(s.name_lc.as_bytes(), token)
                   || starts_at_word_boundary(s.title_lc.as_bytes(), token) {
                   score += 50;
               }
               hit = true;
           } else if finder.find(s.project_lc.as_bytes()).is_some() {
               score += 400; hit = true;
           } else if finder.find(s.branch_lc.as_bytes()).is_some() {
               score += 300; hit = true;
           } else {
               let slice = &text_bin[s.text_offset as usize .. s.text_offset as usize + s.text_len as usize];
               if finder.find(slice).is_some() {
                   score += 100; hit = true;
               } else {
                   hit = false;
               }
           }
           if !hit { return None; }   // AND across tokens
       }

       Some((i, score))
   }).collect::<Vec<_>>()
8. sort by (score desc, sessions[i].timestamp desc)  // date tiebreak
9. apply bookmark float-to-top (existing logic, unchanged)
10. filtered_indices = sorted (i, _).map(|(i, _)| i)
```

### Ranking weights

| Match location | Score |
|----------------|------:|
| Exact ticket hit (ticket-form query, present in `tickets`) | +1000 |
| Token substring in `name` or `title` | +500 |
| Token starts at word boundary in `name`/`title` | +50 bonus |
| Token substring in `project` | +400 |
| Token substring in `branch` | +300 |
| Token substring in `human_text` | +100 |

Per-token, only the **highest-weighted matching field** contributes (cascade, not sum). Prevents `human_text` repetitions from outranking a name match.

### Sort behavior

- **Query active:** relevance score desc, date desc tiebreak. Bookmarks still float to top.
- **Query empty:** restore the user's chosen `SortMode` (Date/Size/Duration). Bookmarks still float to top.
- Status bar shows `Sort: relevance` while query is active; reverts to the chosen mode when cleared.

### Word-boundary helper

```rust
fn starts_at_word_boundary(haystack: &[u8], needle: &str) -> bool {
    for idx in memchr::memmem::find_iter(haystack, needle.as_bytes()) {
        if idx == 0 || !haystack[idx - 1].is_ascii_alphanumeric() {
            return true;
        }
    }
    false
}
```

## Bug fixes bundled in this pass

### Bug 1: Preview-search scroll position wrong

`app::scroll_to_current_match` uses `line_idx * 3` as a display-row estimate.

**Fix:**
1. Add `preview_inner_width: u16` and `preview_line_offsets: Vec<u16>` to `App`.
2. `ui::draw_preview` writes the current preview pane inner width to `app.preview_inner_width` each frame.
3. When `preview_lines` is set (in `preview::check_preview_updates`) or when `preview_inner_width` changes between frames, recompute `preview_line_offsets`:
   ```
   offsets[0] = 0
   offsets[i] = offsets[i-1] + wrap_text(&line_text(i-1), width).len() as u16 + 1  // +1 for blank separator
   ```
4. `scroll_to_current_match` becomes:
   ```rust
   if let Some(&line_idx) = self.preview_search_matches.get(self.preview_search_current) {
       let base = self.preview_line_offsets.get(line_idx).copied().unwrap_or(0);
       let intra = compute_intra_match_offset(line_idx);  // see below
       self.preview_scroll = base.saturating_add(intra).saturating_sub(2);
   }
   ```
5. **Intra-message offset:** for matches inside long wrapped messages, find which wrapped chunk contains the first `memmem::find` hit, and add that chunk index to `base`. Implementation: re-run `wrap_text(&full_text, width)`, scan chunks until one contains the query, return that chunk's index. Cached separately is overkill; this runs once per match navigation key press.

### Bug 2: Preview-search burns CPU per keystroke

`app::update_preview_search` calls `text.to_lowercase()` on every preview message every keystroke.

**Fix:** change `preview_lines: Vec<(String, bool)>` to `Vec<(String, String, bool)>` where the second `String` is pre-lowercased. Same tuple shape applies to `PreviewResult::lines` (delivered by the background worker) and `preview_cache: HashMap<String, Vec<...>>`. Lowercasing happens once on the worker thread in `preview.rs` when the conversation is parsed.

`update_preview_search` uses the lowercased copy with `memchr::memmem::find` (zero allocation in the hot path).

### Bug 3: Highlight bails on Unicode case-length changes

`ui::highlight_spans` returns the whole string un-highlighted when `text.to_lowercase().len() != text.len()`.

**Fix:** build a `lower_to_orig: Vec<usize>` map once, mapping each byte of `text_lower` back to its source byte index in `text`. Use it to translate match positions from the lowercased view to original-text byte slices.

```rust
let mut text_lower = String::with_capacity(text.len());
let mut lower_to_orig: Vec<usize> = Vec::with_capacity(text.len());
for (orig_idx, ch) in text.char_indices() {
    for lc in ch.to_lowercase() {
        let mut buf = [0u8; 4];
        let s = lc.encode_utf8(&mut buf);
        for _ in 0..s.len() {
            lower_to_orig.push(orig_idx);
        }
        text_lower.push(lc);
    }
}
// add one past-the-end sentinel so end-of-match maps cleanly
lower_to_orig.push(text.len());
```

Then `start_orig = lower_to_orig[match_start_lower]`, `end_orig = lower_to_orig[match_end_lower]`. Splice on original boundaries.

### Bug 4: Orphan bookmarks after delete

`app::delete_selected` does not remove the deleted session's bookmark.

**Fix:** in `delete_selected`, before the session is removed from `self.sessions`:
```rust
let id = self.sessions[real_idx].id.clone();
if self.bookmarks.remove(&id) {
    crate::bookmarks::save_bookmarks(&self.bookmarks);
}
```

### Bug 5: Sort indicator stale during search

While search is active, the sort label still shows `date` / `size` / `duration` — confusing because relevance is what actually orders results.

**Fix:** in the status bar / search-bar title block, show `Sort: relevance` while `!search_query.is_empty()`. When query clears, revert to `SortMode::label()`.

## Dependency changes

Add to `Cargo.toml`:

```toml
memmap2 = "0.9"
memchr = "2"
regex = "1"
```

`memchr` is already a transitive dependency in many builds; explicit dep makes our use first-class. `regex` is added for ticket extraction (compiled-once). Total binary growth: negligible relative to existing deps.

## Testing

### Unit tests

`src/parser.rs`:
- `scan_session` produces same head/tail values as the legacy `extract_head_meta`/`extract_tail_meta` on `simple_session.jsonl` and `complex_session.jsonl`.
- `scan_session` correctly extracts human_text (concatenated, lowercased).
- Ticket regex positive: `PROJ-123`, `ABC-78`, `#456`, `FOO12-9` (digit-in-prefix), `XYZ-9999999`.
- Ticket regex negative: `lowercase-99`, `A-99` (1-char prefix), `PROJ-12345678` (8-digit number), `proj-123` (lowercase), embedded `github.com/foo/issues/123` (the `#` form only fires with leading `#`; bare `/123` should not match the alpha form).
- New fixture: `tests/fixtures/session_with_tickets.jsonl` covering all the above.

`src/app.rs`:
- `apply_search_inner` empty query restores all sessions.
- Ticket exact match scores +1000 and outranks all others.
- Name match outranks project outranks branch outranks human_text.
- AND across tokens: `kerveros encrypt` requires both; `kerveros zzznotthere` matches nothing.
- Word-boundary bonus fires for prefix in name (`kerv` in `kerveros`) but not for middle (`erv`).
- Bookmarks float to top of relevance-ordered results.
- Sort restores user's chosen mode when query clears.

`src/ui.rs`:
- `highlight_spans` correctly highlights matches in text containing characters whose lowercase form has different byte length (use Turkish dotted/dotless I or a synthetic case).

### Integration test (new file: `tests/search_integration.rs`)

Build an index over fixture sessions, run representative queries, assert top-K ordering.

### Performance check (manual, pre-publish)

Run `sessy` against the real `~/.claude/projects/` library on the user's machine. Type into the search box; visually confirm no input lag. If lag is perceptible, profile with `perf` / `cargo-flamegraph` and revisit.

## Rollout

- Bump `Cargo.toml` version `0.3.0` → `0.4.0`. Cache format change and search rewrite justify the minor bump.
- First launch after upgrade triggers a full reindex (existing version-mismatch behavior). Expected duration: same order as today's two-pass index — single-pass actually saves I/O per file, offset by lowercasing + regex work.
- No user-visible config changes. No new keybindings.
- `cargo publish` from clean git state per repo convention.

## Risk / open questions

- **Mmap on Windows:** `memmap2` works on Windows but file replacement (rename over an open mmap) is restricted. sessy's user base is overwhelmingly macOS/Linux today; if Windows support matters, the fix is to release the mmap before renaming `text.bin.tmp`. Doc the constraint and revisit if a Windows bug is filed.
- **Very large human-text per session:** if a user has a single session with >100 MB of human messages, mmap stays cheap but the substring scan over that session's slice could exceed our 20 ms target. Acceptable degraded case; not a launch blocker. A trigram prefilter is the future remedy if it ever comes up.
- **Ticket regex false negatives:** prefix length capped at 10. If a user has `LONGPROJECTNAME-123` tickets, they won't auto-detect. They still match as plain substrings via the human_text path. Adjusting the cap is a one-line change post-launch if needed.
