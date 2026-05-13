# sessy search overhaul — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace fuzzy search with precise substring + ticket-aware ranked search backed by an mmap'd text cache; fix five adjacent bugs.

**Architecture:** Two-file cache: small `index.bin` (metadata + ticket lists + offsets) plus mmap'd `text.bin` containing concatenated lowercased human messages. Search runs in parallel via rayon, using `memchr::memmem` SIMD substring scan. Ticket regex (`[A-Z][A-Z0-9]{1,9}-\d{1,7}` and `#\d{1,7}`) is extracted once at index build. Relevance overrides user sort when query active.

**Tech Stack:** Rust 2024, ratatui, rayon, memchr, memmap2, regex, bincode v1.

**Spec:** `docs/superpowers/specs/2026-05-13-search-overhaul-design.md`

---

## File-by-file responsibilities

| File | Change | Responsibility |
|------|--------|---------------|
| `Cargo.toml` | modify | add `memchr`, `memmap2`, `regex` deps; bump version 0.3.0 → 0.4.0 |
| `src/session.rs` | modify | add `tickets`, `text_offset`, `text_len`, `name_lc`, `title_lc`, `project_lc`, `branch_lc` to `SessionMeta` |
| `src/parser.rs` | modify | add `scan_session()` single-pass scanner producing head + tail + human_text_lc + tickets; keep legacy `extract_*` for preview |
| `src/index.rs` | modify | bump `INDEX_VERSION` to 3; new build pipeline writing both `index.bin` and `text.bin`; cache invalidation guard for missing/short `text.bin` |
| `src/text_cache.rs` | create | thin wrapper around `memmap2::Mmap` exposing per-session `&[u8]` slices |
| `src/app.rs` | modify | rewrite `apply_search_inner` with ticket detection + scoring + parallel scan; restore sort on empty query; add `preview_inner_width` + `preview_line_offsets`; fix `update_preview_search` perf; fix `delete_selected` bookmark orphan; change `preview_lines` tuple shape |
| `src/preview.rs` | modify | lowercase each preview message once on the worker thread; update `PreviewResult` tuple shape |
| `src/ui.rs` | modify | write `app.preview_inner_width` per frame; fix `highlight_spans` Unicode bug; show `Sort: relevance` while query active |
| `src/main.rs` | modify | wire mmap into `App`; load `text.bin` after `build_index` |
| `tests/fixtures/session_with_tickets.jsonl` | create | JSONL fixture with ticket strings in various roles |
| `tests/search_integration.rs` | create | integration test over fixture sessions |

---

## Task 1: Add dependencies and bump version

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Edit `Cargo.toml`**

Replace the existing `[package]` `version = "0.3.0"` line with `version = "0.4.0"`.

Replace the `[dependencies]` block with:

```toml
[dependencies]
ratatui = "0.30"
crossterm = "0.29"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
bincode = "1"
rayon = "1.10"
clap = { version = "4", features = ["derive"] }
copypasta = "0.10"
fuzzy-matcher = "0.3"
dirs = "6"
chrono = "0.4"
memchr = "2"
memmap2 = "0.9"
regex = "1"
```

(`fuzzy-matcher` is kept until Task 7 removes it.)

- [ ] **Step 2: Verify build still compiles with new deps**

Run: `cargo check`
Expected: PASS with new deps fetched.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore(sessy): add memchr+memmap2+regex deps, bump to 0.4.0 (search)"
```

---

## Task 2: Extend `SessionMeta` with new fields

**Files:**
- Modify: `src/session.rs`
- Modify: `src/index.rs` (constructor sites)
- Test: `src/session.rs` (existing test module)

- [ ] **Step 1: Write a failing test verifying the new fields exist and serialize**

Append to `#[cfg(test)] mod tests` in `src/session.rs`:

```rust
#[test]
fn test_session_meta_has_search_fields() {
    let m = SessionMeta {
        id: "x".into(),
        project: "P".into(),
        branch: "main".into(),
        name: String::new(),
        title: "Title".into(),
        last_message: String::new(),
        duration_secs: 0,
        timestamp: 0,
        file_size: 0,
        file_mtime: 0,
        file_path: std::path::PathBuf::from("/tmp/x"),
        cwd: String::new(),
        message_count: None,
        tickets: vec!["PROJ-1".into()],
        text_offset: 100,
        text_len: 50,
        name_lc: String::new(),
        title_lc: "title".into(),
        project_lc: "p".into(),
        branch_lc: "main".into(),
    };
    assert_eq!(m.tickets[0], "PROJ-1");
    assert_eq!(m.text_offset, 100);
    assert_eq!(m.title_lc, "title");
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test --lib session::tests::test_session_meta_has_search_fields`
Expected: FAIL with "no field `tickets` on type `SessionMeta`".

- [ ] **Step 3: Add fields to `SessionMeta`**

Edit `src/session.rs`, replace the `SessionMeta` struct with:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub project: String,
    pub branch: String,
    pub name: String,
    pub title: String,
    pub last_message: String,
    pub duration_secs: u64,
    pub timestamp: i64,
    pub file_size: u64,
    pub file_mtime: i64,
    pub file_path: PathBuf,
    pub cwd: String,
    pub message_count: Option<u32>,
    pub tickets: Vec<String>,
    pub text_offset: u64,
    pub text_len: u32,
    pub name_lc: String,
    pub title_lc: String,
    pub project_lc: String,
    pub branch_lc: String,
}
```

- [ ] **Step 4: Fix `SessionMeta` construction sites**

Edit `src/index.rs` `scan_session_file` (line 86-100) to fill the new fields:

```rust
let name_lc = name.to_lowercase();
let title_lc = head.title.to_lowercase();
let project_lc = project.to_lowercase();
let branch_lc = branch.to_lowercase();
Some(SessionMeta {
    id,
    project,
    branch,
    name,
    title: head.title,
    last_message,
    duration_secs,
    timestamp: file_mtime,
    file_size,
    file_mtime,
    file_path: path.to_path_buf(),
    cwd: head.cwd,
    message_count: None,
    tickets: Vec::new(),
    text_offset: 0,
    text_len: 0,
    name_lc,
    title_lc,
    project_lc,
    branch_lc,
})
```

Edit the existing index serialization test in `src/index.rs` (line 240-254): add the new fields to the literal `SessionMeta { ... }`:

```rust
let sessions = vec![SessionMeta {
    id: "abc-123".to_string(),
    project: "test".to_string(),
    branch: "main".to_string(),
    name: String::new(),
    title: "hello world".to_string(),
    last_message: "goodbye".to_string(),
    duration_secs: 300,
    timestamp: 1710300000,
    file_size: 1024,
    file_mtime: 1710300000,
    file_path: PathBuf::from("/tmp/test.jsonl"),
    cwd: "/Users/me/code/test".to_string(),
    message_count: None,
    tickets: vec![],
    text_offset: 0,
    text_len: 0,
    name_lc: String::new(),
    title_lc: "hello world".to_string(),
    project_lc: "test".to_string(),
    branch_lc: "main".to_string(),
}];
```

- [ ] **Step 5: Run tests**

Run: `cargo test --lib`
Expected: PASS (all existing tests + the new field test). The index serialization roundtrip test still passes.

- [ ] **Step 6: Commit**

```bash
git add src/session.rs src/index.rs
git commit -m "feat(sessy): extend SessionMeta with search fields (tickets, text offsets, lc copies)"
```

---

## Task 3: Add ticket regex helper and unit-test it

**Files:**
- Modify: `src/parser.rs`
- Test: `src/parser.rs` (test module)

- [ ] **Step 1: Write failing tests for ticket extraction**

Append to `#[cfg(test)] mod tests` at the bottom of `src/parser.rs`:

```rust
#[test]
fn test_extract_tickets_positive() {
    let mut out = std::collections::HashSet::new();
    extract_tickets_into("see PROJ-123 and ABC-78 please", &mut out);
    assert!(out.contains("PROJ-123"));
    assert!(out.contains("ABC-78"));
}

#[test]
fn test_extract_tickets_hash_form() {
    let mut out = std::collections::HashSet::new();
    extract_tickets_into("fixes #456 and refs #7", &mut out);
    assert!(out.contains("#456"));
    assert!(out.contains("#7"));
}

#[test]
fn test_extract_tickets_negative() {
    let mut out = std::collections::HashSet::new();
    extract_tickets_into("lowercase-99 and A-99 and proj-123", &mut out);
    assert!(out.is_empty(), "got: {:?}", out);
}

#[test]
fn test_extract_tickets_dedupes() {
    let mut out = std::collections::HashSet::new();
    extract_tickets_into("PROJ-1 PROJ-1 PROJ-1", &mut out);
    assert_eq!(out.len(), 1);
}

#[test]
fn test_extract_tickets_word_boundaries() {
    let mut out = std::collections::HashSet::new();
    extract_tickets_into("xPROJ-1y and (PROJ-2)", &mut out);
    assert!(!out.contains("PROJ-1"));
    assert!(out.contains("PROJ-2"));
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test --lib parser::tests::test_extract_tickets`
Expected: FAIL with "cannot find function `extract_tickets_into`".

- [ ] **Step 3: Implement `extract_tickets_into`**

Add to the top of `src/parser.rs`, below the existing `use` statements:

```rust
use regex::Regex;
use std::sync::OnceLock;

fn ticket_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b[A-Z][A-Z0-9]{1,9}-\d{1,7}\b|(?:^|[^A-Za-z0-9_])#\d{1,7}\b").unwrap()
    })
}

pub fn extract_tickets_into(text: &str, out: &mut std::collections::HashSet<String>) {
    for m in ticket_regex().find_iter(text) {
        let s = m.as_str();
        let trimmed = if let Some(pos) = s.find('#') { &s[pos..] } else { s };
        out.insert(trimmed.to_string());
    }
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test --lib parser::tests::test_extract_tickets`
Expected: PASS (all 5 ticket tests).

- [ ] **Step 5: Commit**

```bash
git add src/parser.rs
git commit -m "feat(sessy): ticket extraction regex (PROJ-123, #456 forms)"
```

---

## Task 4: Add `scan_session` single-pass scanner

**Files:**
- Modify: `src/parser.rs`
- Test: `src/parser.rs` (test module)

- [ ] **Step 1: Write failing test**

Append to `src/parser.rs` test module:

```rust
#[test]
fn test_scan_session_simple() {
    let result = scan_session(&fixture_path("simple_session.jsonl"));
    let result = result.expect("should scan");
    assert_eq!(result.head.title, "build a cool thing");
    assert_eq!(result.head.branch, "main");
    let tail = result.tail.expect("should have tail");
    assert_eq!(tail.last_human_message, "looks good, ship it");
    assert!(
        result.human_text_lc.contains("build a cool thing"),
        "got: {:?}",
        result.human_text_lc
    );
    assert!(
        result.human_text_lc.contains("looks good, ship it"),
        "got: {:?}",
        result.human_text_lc
    );
    assert!(
        result.human_text_lc.chars().all(|c| !c.is_uppercase()),
        "should be lowercased"
    );
}

#[test]
fn test_scan_session_empty_returns_none() {
    let result = scan_session(&fixture_path("empty_session.jsonl"));
    assert!(result.is_none());
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test --lib parser::tests::test_scan_session`
Expected: FAIL with "cannot find function `scan_session`".

- [ ] **Step 3: Implement `scan_session`**

Add to `src/parser.rs`, after the existing `extract_*` functions:

```rust
pub struct ScanResult {
    pub head: HeadMeta,
    pub tail: Option<TailMeta>,
    pub human_text_lc: String,
    pub tickets: Vec<String>,
}

pub fn scan_session(path: &Path) -> Option<ScanResult> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut head: Option<HeadMeta> = None;
    let mut working_head = HeadMeta {
        title: String::new(),
        branch: String::new(),
        slug: String::new(),
        first_timestamp: String::new(),
        cwd: String::new(),
    };
    let mut last_human_message = String::new();
    let mut last_timestamp = String::new();
    let mut rename = String::new();
    let mut human_text_lc = String::new();
    let mut tickets_set: std::collections::HashSet<String> = std::collections::HashSet::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }

        extract_tickets_into(&line, &mut tickets_set);

        let entry: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if working_head.branch.is_empty() {
            if let Some(b) = entry.get("gitBranch").and_then(|b| b.as_str()) {
                working_head.branch = b.to_string();
            }
        }
        if working_head.slug.is_empty() {
            if let Some(s) = entry.get("slug").and_then(|s| s.as_str()) {
                working_head.slug = s.to_string();
            }
        }
        if working_head.first_timestamp.is_empty() {
            if let Some(ts) = entry.get("timestamp").and_then(|t| t.as_str()) {
                working_head.first_timestamp = ts.to_string();
            }
        }
        if working_head.cwd.is_empty() {
            if let Some(c) = entry.get("cwd").and_then(|c| c.as_str()) {
                working_head.cwd = c.to_string();
            }
        }

        if let Some(ts) = entry.get("timestamp").and_then(|t| t.as_str()) {
            last_timestamp = ts.to_string();
        }

        if is_human_message(&entry) {
            if let Some(full) = entry
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
            {
                let trimmed = full.trim();
                if !trimmed.is_empty() {
                    if head.is_none() {
                        let title = human_message_text(&entry).unwrap_or_default();
                        head = Some(HeadMeta {
                            title,
                            branch: working_head.branch.clone(),
                            slug: working_head.slug.clone(),
                            first_timestamp: working_head.first_timestamp.clone(),
                            cwd: working_head.cwd.clone(),
                        });
                    }
                    human_text_lc.push_str(&trimmed.to_lowercase());
                    human_text_lc.push('\n');
                    if let Some(text) = human_message_text(&entry) {
                        last_human_message = text;
                    }
                }
            }
        }

        if entry.get("subtype").and_then(|s| s.as_str()) == Some("local_command") {
            if let Some(content) = entry.get("content").and_then(|c| c.as_str()) {
                if content.contains("<command-name>/rename</command-name>") {
                    if let Some(start) = content.find("<command-args>") {
                        if let Some(end) = content.find("</command-args>") {
                            rename = content[start + 14..end].to_string();
                        }
                    }
                }
            }
        }
    }

    let head = head?;
    let tail = if last_human_message.is_empty() && last_timestamp.is_empty() && rename.is_empty() {
        None
    } else {
        Some(TailMeta {
            last_human_message,
            last_timestamp,
            rename,
        })
    };

    let mut tickets: Vec<String> = tickets_set.into_iter().collect();
    tickets.sort();

    Some(ScanResult {
        head,
        tail,
        human_text_lc,
        tickets,
    })
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test --lib parser::tests`
Expected: PASS (existing tests + 2 new scan_session tests).

- [ ] **Step 5: Commit**

```bash
git add src/parser.rs
git commit -m "feat(sessy): scan_session single-pass scanner with human text + tickets"
```

---

## Task 5: Create text-cache module (mmap'd `text.bin`)

**Files:**
- Create: `src/text_cache.rs`
- Modify: `src/main.rs` (declare module)

- [ ] **Step 1: Create `src/text_cache.rs`**

```rust
use memmap2::Mmap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn text_cache_path() -> PathBuf {
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("sessy");
    fs::create_dir_all(&cache_dir).ok();
    cache_dir.join("text.bin")
}

/// Mmap of the text cache. Empty (`len() == 0`) when the cache file is missing
/// or empty; callers must check before slicing.
pub struct TextCache {
    mmap: Option<Mmap>,
}

impl TextCache {
    pub fn open(path: &Path) -> Self {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return Self { mmap: None },
        };
        let metadata = match file.metadata() {
            Ok(m) => m,
            Err(_) => return Self { mmap: None },
        };
        if metadata.len() == 0 {
            return Self { mmap: None };
        }
        let mmap = unsafe { Mmap::map(&file).ok() };
        Self { mmap }
    }

    pub fn len(&self) -> usize {
        self.mmap.as_ref().map(|m| m.len()).unwrap_or(0)
    }

    pub fn slice(&self, offset: u64, len: u32) -> &[u8] {
        let mmap = match &self.mmap {
            Some(m) => m,
            None => return &[],
        };
        let start = offset as usize;
        let end = start.saturating_add(len as usize);
        if end > mmap.len() {
            return &[];
        }
        &mmap[start..end]
    }
}

/// Writes `text.bin` atomically. Returns the per-session (offset, len) pairs
/// in the order of the input.
pub fn write_text_cache(path: &Path, chunks: &[&[u8]]) -> std::io::Result<Vec<(u64, u32)>> {
    let tmp = path.with_extension("bin.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp)?;

    let mut out = Vec::with_capacity(chunks.len());
    let mut offset: u64 = 0;
    for chunk in chunks {
        file.write_all(chunk)?;
        out.push((offset, chunk.len() as u32));
        offset += chunk.len() as u64;
    }
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)?;
    Ok(out)
}
```

- [ ] **Step 2: Register the module in `src/main.rs`**

Find the `mod` declarations near the top of `src/main.rs` and add:

```rust
mod text_cache;
```

- [ ] **Step 3: Write a unit test for the round trip**

Append to `src/text_cache.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_text_cache_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("text.bin");
        let a = b"hello".as_slice();
        let b = b"world!".as_slice();
        let pairs = write_text_cache(&path, &[a, b]).unwrap();
        assert_eq!(pairs[0], (0, 5));
        assert_eq!(pairs[1], (5, 6));

        let cache = TextCache::open(&path);
        assert_eq!(cache.slice(0, 5), b"hello");
        assert_eq!(cache.slice(5, 6), b"world!");
        assert_eq!(cache.slice(11, 1), b""); // out of range
    }

    #[test]
    fn test_text_cache_missing_file_safe() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("does_not_exist.bin");
        let cache = TextCache::open(&path);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.slice(0, 5), b"");
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib text_cache`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/text_cache.rs src/main.rs
git commit -m "feat(sessy): mmap'd text cache with atomic write"
```

---

## Task 6: Wire `scan_session` into index build and emit `text.bin`

**Files:**
- Modify: `src/index.rs`
- Test: `src/index.rs` (test module)

- [ ] **Step 1: Bump `INDEX_VERSION`**

Edit `src/index.rs` line 9:

```rust
pub const INDEX_VERSION: u32 = 3;
```

- [ ] **Step 2: Replace `scan_session_file` to use the new scanner and return text bytes**

Replace the body of `scan_session_file` in `src/index.rs` (lines 44-101) with:

```rust
pub fn scan_session_file(path: &Path) -> Option<(SessionMeta, Vec<u8>)> {
    let metadata = fs::metadata(path).ok()?;
    let file_size = metadata.len();
    let file_mtime = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let id = path.file_stem()?.to_str()?.to_string();

    let scan = crate::parser::scan_session(path)?;

    let home_dir = dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let project = crate::session::extract_project_name(&scan.head.cwd, &home_dir);

    let (last_message, duration_secs, rename) = if let Some(tail) = &scan.tail {
        let duration = compute_duration(&scan.head.first_timestamp, &tail.last_timestamp);
        (tail.last_human_message.clone(), duration, tail.rename.clone())
    } else {
        (String::new(), 0, String::new())
    };

    let name = if !rename.is_empty() {
        rename
    } else if !scan.head.slug.is_empty() {
        scan.head.slug.clone()
    } else {
        String::new()
    };

    let branch = if scan.head.branch == "HEAD" {
        String::new()
    } else {
        scan.head.branch.clone()
    };

    let name_lc = name.to_lowercase();
    let title_lc = scan.head.title.to_lowercase();
    let project_lc = project.to_lowercase();
    let branch_lc = branch.to_lowercase();

    let text_bytes = scan.human_text_lc.into_bytes();

    let meta = SessionMeta {
        id,
        project,
        branch,
        name,
        title: scan.head.title,
        last_message,
        duration_secs,
        timestamp: file_mtime,
        file_size,
        file_mtime,
        file_path: path.to_path_buf(),
        cwd: scan.head.cwd,
        message_count: None,
        tickets: scan.tickets,
        text_offset: 0, // filled in finalize step
        text_len: text_bytes.len() as u32,
        name_lc,
        title_lc,
        project_lc,
        branch_lc,
    };
    Some((meta, text_bytes))
}
```

Replace the imports at top of file (line 1):

```rust
use crate::session::SessionMeta;
use crate::text_cache::{text_cache_path, write_text_cache, TextCache};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
```

(Removed: `use crate::parser::{extract_head_meta, extract_tail_meta};` — they are no longer called from index.)

- [ ] **Step 3: Rewrite `build_index` to emit `text.bin`**

Replace the body of `build_index` (lines 116-183) with:

```rust
pub fn build_index(cached: Option<SessionIndex>, force_rebuild: bool) -> SessionIndex {
    let projects_dir = claude_projects_dir();
    if !projects_dir.exists() {
        return SessionIndex {
            version: INDEX_VERSION,
            sessions: vec![],
        };
    }

    // Open the previous text.bin so we can reuse bytes for unchanged sessions.
    let prev_text = TextCache::open(&text_cache_path());

    let cache_map: std::collections::HashMap<PathBuf, &SessionMeta> = if force_rebuild {
        std::collections::HashMap::new()
    } else {
        cached
            .as_ref()
            .map(|idx| idx.sessions.iter().map(|s| (s.file_path.clone(), s)).collect())
            .unwrap_or_default()
    };

    let mut file_entries: Vec<PathBuf> = Vec::new();
    if let Ok(project_dirs) = fs::read_dir(&projects_dir) {
        for proj_entry in project_dirs.flatten() {
            if !proj_entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let proj_dir = proj_entry.path();
            if let Ok(files) = fs::read_dir(&proj_dir) {
                for file_entry in files.flatten() {
                    let path = file_entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("jsonl")
                        && path.is_file()
                    {
                        file_entries.push(path);
                    }
                }
            }
        }
    }

    // For each file: either reuse cached meta + old text bytes, or rescan.
    let scanned: Vec<(SessionMeta, Vec<u8>)> = file_entries
        .par_iter()
        .filter_map(|path| {
            if let Some(cached_entry) = cache_map.get(path) {
                let meta = match fs::metadata(path) {
                    Ok(m) => m,
                    Err(_) => return scan_session_file(path),
                };
                let current_mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let current_size = meta.len();
                if cached_entry.file_mtime == current_mtime
                    && cached_entry.file_size == current_size
                {
                    let bytes = prev_text
                        .slice(cached_entry.text_offset, cached_entry.text_len)
                        .to_vec();
                    let mut meta = (*cached_entry).clone();
                    // offsets will be reassigned in the finalize step
                    meta.text_offset = 0;
                    return Some((meta, bytes));
                }
            }
            scan_session_file(path)
        })
        .collect();

    // Serial finalize: write text.bin and patch offsets onto each SessionMeta.
    let chunks: Vec<&[u8]> = scanned.iter().map(|(_, b)| b.as_slice()).collect();
    let offsets = match write_text_cache(&text_cache_path(), &chunks) {
        Ok(o) => o,
        Err(_) => {
            // If we can't write text.bin, return empty so the next launch retries.
            return SessionIndex {
                version: INDEX_VERSION,
                sessions: vec![],
            };
        }
    };

    let sessions: Vec<SessionMeta> = scanned
        .into_iter()
        .zip(offsets.into_iter())
        .map(|((mut meta, _), (offset, len))| {
            meta.text_offset = offset;
            meta.text_len = len;
            meta
        })
        .collect();

    SessionIndex {
        version: INDEX_VERSION,
        sessions,
    }
}
```

- [ ] **Step 4: Add cache-validity guard to `load_cached_index`**

Replace `load_cached_index` (line 185-189):

```rust
pub fn load_cached_index() -> Option<SessionIndex> {
    let path = index_cache_path();
    let bytes = fs::read(&path).ok()?;
    let index = deserialize_index(&bytes)?;
    let text_cache = TextCache::open(&text_cache_path());
    let max_end: u64 = index
        .sessions
        .iter()
        .map(|s| s.text_offset + s.text_len as u64)
        .max()
        .unwrap_or(0);
    if (text_cache.len() as u64) < max_end {
        return None;
    }
    Some(index)
}
```

- [ ] **Step 5: Fix the existing `test_scan_session_file_simple` test**

The signature of `scan_session_file` changed from `Option<SessionMeta>` to `Option<(SessionMeta, Vec<u8>)>`. Update lines 222-230 of `src/index.rs`:

```rust
#[test]
fn test_scan_session_file_simple() {
    let result = scan_session_file(&fixture_path("simple_session.jsonl"));
    let (meta, text) = result.expect("should produce SessionMeta + text");
    assert_eq!(meta.title, "build a cool thing");
    assert_eq!(meta.last_message, "looks good, ship it");
    assert_eq!(meta.branch, "main");
    assert!(meta.duration_secs > 0);
    assert_eq!(meta.title_lc, "build a cool thing");
    assert_eq!(meta.branch_lc, "main");
    assert!(!text.is_empty());
    assert!(text.iter().all(|b| !(*b as char).is_uppercase()));
}
```

And line 233-236:

```rust
#[test]
fn test_scan_session_file_empty_returns_none() {
    let result = scan_session_file(&fixture_path("empty_session.jsonl"));
    assert!(result.is_none(), "empty session should be filtered out");
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test --lib index::tests`
Expected: PASS (all index tests).

- [ ] **Step 7: Commit**

```bash
git add src/index.rs
git commit -m "feat(sessy): index pipeline emits text.bin; INDEX_VERSION=3; cache validity guard"
```

---

## Task 7: Rewrite `apply_search_inner` with ticket-aware scoring + parallel scan

**Files:**
- Modify: `src/app.rs`
- Test: `src/app.rs` (new test module)

- [ ] **Step 1: Add `text_cache` field to `App`**

Edit `src/app.rs`:

Add use line near top:
```rust
use crate::text_cache::TextCache;
```

Add field to `App` struct (after `sessions`):
```rust
pub text_cache: TextCache,
```

Add to `App::new` signature: take `text_cache: TextCache`, store it. Update the call site in `main.rs` later.

Replace `pub fn new(sessions: ..., print_mode: bool, bookmarks: HashSet<String>)` with:

```rust
pub fn new(
    sessions: Vec<SessionMeta>,
    print_mode: bool,
    bookmarks: HashSet<String>,
    text_cache: TextCache,
) -> Self {
```

In the struct init body, add `text_cache,` alongside the existing fields.

- [ ] **Step 2: Write failing tests for the new search logic**

Add a new test module at the bottom of `src/app.rs`:

```rust
#[cfg(test)]
mod search_tests {
    use super::*;
    use crate::text_cache::TextCache;
    use std::path::PathBuf;

    fn make_session(
        id: &str,
        name: &str,
        title: &str,
        project: &str,
        branch: &str,
        tickets: Vec<&str>,
    ) -> SessionMeta {
        SessionMeta {
            id: id.into(),
            project: project.into(),
            branch: branch.into(),
            name: name.into(),
            title: title.into(),
            last_message: String::new(),
            duration_secs: 0,
            timestamp: 0,
            file_size: 0,
            file_mtime: 0,
            file_path: PathBuf::from(format!("/tmp/{}.jsonl", id)),
            cwd: String::new(),
            message_count: None,
            tickets: tickets.into_iter().map(String::from).collect(),
            text_offset: 0,
            text_len: 0,
            name_lc: name.to_lowercase(),
            title_lc: title.to_lowercase(),
            project_lc: project.to_lowercase(),
            branch_lc: branch.to_lowercase(),
        }
    }

    fn empty_cache() -> TextCache {
        TextCache::open(std::path::Path::new("/does/not/exist"))
    }

    #[test]
    fn test_empty_query_restores_all() {
        let sessions = vec![
            make_session("a", "", "", "p1", "main", vec![]),
            make_session("b", "", "", "p2", "main", vec![]),
        ];
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.search_query.clear();
        app.apply_search();
        assert_eq!(app.filtered_indices.len(), 2);
    }

    #[test]
    fn test_ticket_exact_beats_name() {
        let sessions = vec![
            make_session("a", "PROJ-123", "", "p", "main", vec![]),
            make_session("b", "", "", "p", "main", vec!["PROJ-123"]),
        ];
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.search_query = "PROJ-123".into();
        app.apply_search();
        assert_eq!(app.filtered_indices.len(), 2);
        // session "b" (ticket hit) ranks above "a" (name hit)
        assert_eq!(app.sessions[app.filtered_indices[0]].id, "b");
    }

    #[test]
    fn test_name_beats_project() {
        let sessions = vec![
            make_session("a", "kerveros", "", "other", "main", vec![]),
            make_session("b", "", "", "kerveros", "main", vec![]),
        ];
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.search_query = "kerveros".into();
        app.apply_search();
        assert_eq!(app.sessions[app.filtered_indices[0]].id, "a");
    }

    #[test]
    fn test_and_across_tokens_field_or() {
        let sessions = vec![
            make_session("a", "kerveros encrypt", "", "p", "main", vec![]),
            make_session("b", "kerveros", "", "p", "main", vec![]),
            make_session("c", "kerveros", "", "encrypt-stuff", "main", vec![]),
        ];
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.search_query = "kerveros encrypt".into();
        app.apply_search();
        let ids: Vec<&str> = app
            .filtered_indices
            .iter()
            .map(|&i| app.sessions[i].id.as_str())
            .collect();
        assert!(ids.contains(&"a"), "got {:?}", ids);
        assert!(ids.contains(&"c"), "got {:?}", ids);
        assert!(!ids.contains(&"b"), "session b should not match: {:?}", ids);
    }

    #[test]
    fn test_no_match_returns_empty() {
        let sessions = vec![make_session("a", "kerveros", "", "p", "main", vec![])];
        let mut app = App::new(sessions, false, HashSet::new(), empty_cache());
        app.search_query = "zzzzznevermatch".into();
        app.apply_search();
        assert!(app.filtered_indices.is_empty());
    }

    #[test]
    fn test_bookmarks_float_to_top_under_relevance() {
        let sessions = vec![
            make_session("a", "kerveros", "", "p", "main", vec![]),
            make_session("b", "kerveros", "", "p", "main", vec![]),
        ];
        let mut bookmarks = HashSet::new();
        bookmarks.insert("b".to_string());
        let mut app = App::new(sessions, false, bookmarks, empty_cache());
        app.search_query = "kerveros".into();
        app.apply_search();
        assert_eq!(app.sessions[app.filtered_indices[0]].id, "b");
    }
}
```

- [ ] **Step 3: Run tests to verify failure**

Run: `cargo test --lib app::search_tests`
Expected: FAIL — `App::new` arity changed (text_cache is still being added) and/or search logic is unchanged.

- [ ] **Step 4: Replace `apply_search_inner` with the new scoring logic**

In `src/app.rs`, replace `apply_search_inner` (lines 185-210) with:

```rust
fn apply_search_inner(&mut self) {
    if self.search_query.is_empty() {
        self.filtered_indices = (0..self.sessions.len()).collect();
        return;
    }

    use memchr::memmem::Finder;
    use rayon::prelude::*;

    let query_lc = self.search_query.to_lowercase();
    let query_upper = self.search_query.to_ascii_uppercase();
    let is_ticket_form = is_ticket_query(&query_upper);
    let tokens: Vec<&str> = query_lc.split_whitespace().collect();
    if tokens.is_empty() {
        self.filtered_indices = (0..self.sessions.len()).collect();
        return;
    }
    let finders: Vec<Finder> = tokens.iter().map(|t| Finder::new(t.as_bytes())).collect();

    let text_cache = &self.text_cache;

    let mut scored: Vec<(usize, i64)> = self
        .sessions
        .par_iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let mut score: i64 = 0;
            if is_ticket_form && s.tickets.binary_search(&query_upper).is_ok() {
                score += 1000;
            }
            for (token, finder) in tokens.iter().zip(finders.iter()) {
                let hit = if finder.find(s.name_lc.as_bytes()).is_some()
                    || finder.find(s.title_lc.as_bytes()).is_some()
                {
                    score += 500;
                    if starts_at_word_boundary(s.name_lc.as_bytes(), token.as_bytes())
                        || starts_at_word_boundary(s.title_lc.as_bytes(), token.as_bytes())
                    {
                        score += 50;
                    }
                    true
                } else if finder.find(s.project_lc.as_bytes()).is_some() {
                    score += 400;
                    true
                } else if finder.find(s.branch_lc.as_bytes()).is_some() {
                    score += 300;
                    true
                } else {
                    let slice = text_cache.slice(s.text_offset, s.text_len);
                    if finder.find(slice).is_some() {
                        score += 100;
                        true
                    } else {
                        false
                    }
                };
                if !hit {
                    return None;
                }
            }
            Some((i, score))
        })
        .collect();

    // score desc, timestamp desc tiebreak
    scored.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| self.sessions[b.0].timestamp.cmp(&self.sessions[a.0].timestamp))
    });

    self.filtered_indices = scored.into_iter().map(|(i, _)| i).collect();
}

fn is_ticket_query(q_upper: &str) -> bool {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"^[A-Z][A-Z0-9]{1,9}-\d{1,7}$|^#\d{1,7}$").unwrap()
    });
    re.is_match(q_upper)
}

fn starts_at_word_boundary(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return false;
    }
    for idx in memchr::memmem::find_iter(haystack, needle) {
        if idx == 0 || !haystack[idx - 1].is_ascii_alphanumeric() {
            return true;
        }
    }
    false
}
```

- [ ] **Step 5: Make `apply_sort` skip the relevance-ordered list when search is active**

Replace `apply_sort` (lines 220-236):

```rust
pub fn apply_sort(&mut self) {
    let sessions = &self.sessions;
    let bookmarks = &self.bookmarks;
    let search_active = !self.search_query.is_empty();

    // When search is active, preserve the relevance ordering (score desc,
    // date tiebreak) computed by apply_search_inner. Only float bookmarks.
    if search_active {
        self.filtered_indices.sort_by(|&a, &b| {
            let a_pinned = bookmarks.contains(&sessions[a].id);
            let b_pinned = bookmarks.contains(&sessions[b].id);
            b_pinned.cmp(&a_pinned)
        });
        return;
    }

    self.filtered_indices.sort_by(|&a, &b| {
        let a_pinned = bookmarks.contains(&sessions[a].id);
        let b_pinned = bookmarks.contains(&sessions[b].id);
        b_pinned
            .cmp(&a_pinned)
            .then_with(|| match self.sort_mode {
                SortMode::Date => sessions[b].timestamp.cmp(&sessions[a].timestamp),
                SortMode::Size => sessions[b].file_size.cmp(&sessions[a].file_size),
                SortMode::Duration => sessions[b].duration_secs.cmp(&sessions[a].duration_secs),
            })
    });
}
```

Important: `sort_by` is **stable**, so the bookmark-float pass preserves the relevance ordering among non-bookmarks and among bookmarks.

- [ ] **Step 6: Run tests to verify pass**

Run: `cargo test --lib app::search_tests`
Expected: PASS (6 tests).

- [ ] **Step 7: Remove the now-unused `fuzzy-matcher` dependency**

Edit `Cargo.toml` and remove the line `fuzzy-matcher = "0.3"`.

Verify there are no more references: `grep -r "fuzzy_matcher\|fuzzy-matcher" src/ Cargo.toml`
Expected: no matches.

Run: `cargo build`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/app.rs Cargo.toml Cargo.lock
git commit -m "feat(sessy): ticket-aware substring search with relevance ranking; drop fuzzy-matcher"
```

---

## Task 8: Change `preview_lines` tuple shape (3-tuple with lowercased text)

**Files:**
- Modify: `src/app.rs`
- Modify: `src/preview.rs`
- Modify: `src/ui.rs`

- [ ] **Step 1: Update `App` fields**

Edit `src/app.rs`:

```rust
pub preview_lines: Vec<(String, String, bool)>,  // (text, text_lc, is_user)
pub preview_cache: std::collections::HashMap<String, Vec<(String, String, bool)>>,
```

Update `PreviewResult`:

```rust
pub struct PreviewResult {
    pub session_id: String,
    pub lines: Vec<(String, String, bool)>,
    pub message_count: u32,
}
```

Update `cache_preview` signature (line 384):

```rust
pub fn cache_preview(&mut self, session_id: String, lines: Vec<(String, String, bool)>) {
```

- [ ] **Step 2: Update `update_preview_search` to use the lowercased copy with memmem**

Replace `update_preview_search` (lines 311-328) of `src/app.rs`:

```rust
pub fn update_preview_search(&mut self) {
    let query_lc = self.preview_search_query.to_lowercase();
    if query_lc.is_empty() {
        self.preview_search_matches.clear();
        self.preview_search_current = 0;
        return;
    }
    let finder = memchr::memmem::Finder::new(query_lc.as_bytes());
    self.preview_search_matches = self
        .preview_lines
        .iter()
        .enumerate()
        .filter_map(|(i, (_orig, lower, _is_user))| {
            if finder.find(lower.as_bytes()).is_some() {
                Some(i)
            } else {
                None
            }
        })
        .collect();
    self.preview_search_current = 0;
    self.scroll_to_current_match();
}
```

- [ ] **Step 3: Update `preview.rs` to fill the lowercased field**

Edit `src/preview.rs`:

In `request_preview` (line 25 and 38-44), replace the cached `clone` flow and the `thread::spawn` body so they produce 3-tuples:

```rust
// at the cache hit:
if let Some(cached) = app.preview_cache.get(&session_id) {
    app.preview_lines = cached.clone();
    app.preview_session_id = session_id;
    app.preview_loading = false;
    return;
}

// in thread::spawn:
thread::spawn(move || {
    let messages = extract_conversation(&file_path);
    let lines: Vec<(String, String, bool)> = messages
        .into_iter()
        .map(|m| {
            let lower = m.text.to_lowercase();
            (m.text, lower, m.role == Role::User)
        })
        .collect();
    let message_count = lines.iter().filter(|(_, _, is_user)| *is_user).count() as u32;

    let _ = tx.send(crate::app::PreviewResult {
        session_id,
        lines,
        message_count,
    });
});
```

- [ ] **Step 4: Update `ui.rs` to read the new tuple**

Find the iteration in `draw_preview` around line 476:

```rust
for (msg_idx, (text, is_user)) in app.preview_lines.iter().enumerate() {
```

Replace with:

```rust
for (msg_idx, (text, _text_lc, is_user)) in app.preview_lines.iter().enumerate() {
```

- [ ] **Step 5: Verify build**

Run: `cargo build`
Expected: PASS.

Run: `cargo test --lib`
Expected: PASS (all previous tests still green).

- [ ] **Step 6: Commit**

```bash
git add src/app.rs src/preview.rs src/ui.rs
git commit -m "perf(sessy): pre-lowercase preview lines once on worker thread"
```

---

## Task 9: Fix preview-search scroll positioning

**Files:**
- Modify: `src/app.rs`
- Modify: `src/ui.rs`

- [ ] **Step 1: Add new fields to `App`**

Edit the `App` struct in `src/app.rs`:

```rust
pub preview_inner_width: u16,
pub preview_line_offsets: Vec<u16>,
```

Initialize in `App::new`:

```rust
preview_inner_width: 0,
preview_line_offsets: Vec::new(),
```

- [ ] **Step 2: Add helpers for wrap math (mirrors `ui::wrap_text` logic, used by app)**

Add to `src/app.rs`, near the bottom of `impl App`:

```rust
fn wrapped_line_count(text: &str, width: usize) -> u16 {
    if width == 0 {
        return 1;
    }
    let mut count = 0_u16;
    let mut remaining = text;
    while let Some((byte_limit, _)) = remaining.char_indices().nth(width) {
        let split_at = remaining[..byte_limit].rfind(' ').unwrap_or(0);
        let split_at = if split_at == 0 { byte_limit } else { split_at };
        count = count.saturating_add(1);
        remaining = remaining[split_at..].trim_start();
    }
    if !remaining.is_empty() {
        count = count.saturating_add(1);
    }
    if count == 0 { 1 } else { count }
}

pub fn recompute_preview_offsets(&mut self) {
    let width = self.preview_inner_width.saturating_sub(1) as usize;
    self.preview_line_offsets.clear();
    self.preview_line_offsets.reserve(self.preview_lines.len());
    let mut cursor: u16 = 0;
    for (text, _lower, is_user) in self.preview_lines.iter() {
        self.preview_line_offsets.push(cursor);
        let prefix = if *is_user { "USER: " } else { "ASST: " };
        let full = format!("{}{}", prefix, text);
        let lines = Self::wrapped_line_count(&full, width);
        cursor = cursor.saturating_add(lines).saturating_add(1); // +1 blank separator
    }
}
```

- [ ] **Step 3: Recompute offsets when needed**

In `preview::check_preview_updates`, after `app.preview_lines = result.lines;`, call:

```rust
app.recompute_preview_offsets();
```

Same when the cached preview is loaded in `request_preview` (after `app.preview_lines = cached.clone();`).

Edit `src/ui.rs` `draw_preview` to track width changes. Near the top of `draw_preview`, after `inner` is computed (line 441-442):

```rust
let new_width = inner.width;
if app.preview_inner_width != new_width {
    app.preview_inner_width = new_width;
    app.recompute_preview_offsets();
}
```

Note: this requires `draw_preview` to take `&mut App` instead of `&App`. Update the signature and the caller in `draw_content` accordingly. `draw_session_list` and `draw_timeline` stay `&App`; only `draw_preview` becomes `&mut App`.

In `draw` (line 9), `app: &mut App` is already passed. In `draw_content` (line 69), change the signature to `&mut App` and have it pass `&*app` to the `&App` siblings and `app` to `draw_preview`. Concretely:

```rust
fn draw_content(frame: &mut Frame, app: &mut App, area: Rect) {
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    match app.view_mode {
        ViewMode::Normal => draw_session_list(frame, app, panes[0]),
        ViewMode::Timeline => draw_timeline(frame, app, panes[0]),
    }
    draw_preview(frame, app, panes[1]);
}

fn draw_preview(frame: &mut Frame, app: &mut App, area: Rect) {
    // ... existing body, with the new width-update block ...
}
```

And in `draw` (line 22), pass `app` (mutable) into `draw_content`:

```rust
draw_content(frame, app, main_chunks[1]);
```

- [ ] **Step 4: Replace `scroll_to_current_match` with the offset-based version**

Replace `scroll_to_current_match` in `src/app.rs` (lines 351-357):

```rust
fn scroll_to_current_match(&mut self) {
    let line_idx = match self.preview_search_matches.get(self.preview_search_current) {
        Some(&i) => i,
        None => return,
    };
    let base = self
        .preview_line_offsets
        .get(line_idx)
        .copied()
        .unwrap_or(0);
    let intra = self.intra_match_chunk_offset(line_idx);
    self.preview_scroll = base.saturating_add(intra).saturating_sub(2);
}

fn intra_match_chunk_offset(&self, line_idx: usize) -> u16 {
    let (text, _lower, is_user) = match self.preview_lines.get(line_idx) {
        Some(t) => t,
        None => return 0,
    };
    let query_lc = self.preview_search_query.to_lowercase();
    if query_lc.is_empty() {
        return 0;
    }
    let prefix = if *is_user { "USER: " } else { "ASST: " };
    let full = format!("{}{}", prefix, text);
    let width = self.preview_inner_width.saturating_sub(1) as usize;
    if width == 0 {
        return 0;
    }
    let finder = memchr::memmem::Finder::new(query_lc.as_bytes());
    let chunks = wrap_for_offsets(&full, width);
    for (idx, chunk) in chunks.iter().enumerate() {
        if finder.find(chunk.to_lowercase().as_bytes()).is_some() {
            return idx as u16;
        }
    }
    0
}
```

Add helper alongside `wrapped_line_count`:

```rust
fn wrap_for_offsets(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut result = Vec::new();
    let mut remaining = text;
    while let Some((byte_limit, _)) = remaining.char_indices().nth(width) {
        let split_at = remaining[..byte_limit].rfind(' ').unwrap_or(0);
        let split_at = if split_at == 0 { byte_limit } else { split_at };
        result.push(remaining[..split_at].to_string());
        remaining = remaining[split_at..].trim_start();
    }
    if !remaining.is_empty() {
        result.push(remaining.to_string());
    }
    result
}
```

Note: function above is intentionally a duplicate of `ui::wrap_text` to keep the wrap logic colocated with the offset math. Cheap acceptable duplication; both share identical behavior.

- [ ] **Step 5: Verify build**

Run: `cargo build`
Expected: PASS.

Run: `cargo test --lib`
Expected: PASS (all existing tests).

- [ ] **Step 6: Commit**

```bash
git add src/app.rs src/ui.rs src/preview.rs
git commit -m "fix(sessy): preview-search scrolls to actual wrapped-row of match"
```

---

## Task 10: Fix `highlight_spans` Unicode bug

**Files:**
- Modify: `src/ui.rs`
- Test: `src/ui.rs` (test module — add if missing)

- [ ] **Step 1: Write failing test**

Append a test module at the bottom of `src/ui.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_highlight_unicode_length_change() {
        // Turkish dotted-I (\u{0130}) lowercases to "i\u{0307}" (two code points),
        // which makes text_lower.len() differ from text.len().
        let text = "İstanbul project";
        let base = Style::default();
        let mat = Style::default();
        let spans = highlight_spans(text, "istanbul", base, mat);
        // Should produce at least 2 spans (matched + remainder), not bail out.
        assert!(spans.len() >= 2, "got spans: {}", spans.len());
    }
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test --lib ui::tests::test_highlight_unicode_length_change`
Expected: FAIL (current behavior returns one span when lengths differ).

- [ ] **Step 3: Rewrite `highlight_spans` to handle length-changing lowercase**

Replace `highlight_spans` in `src/ui.rs` (lines 697-745):

```rust
fn highlight_spans(
    text: &str,
    query: &str,
    base_style: Style,
    match_style: Style,
) -> Vec<Span<'static>> {
    if query.is_empty() {
        return vec![Span::styled(text.to_string(), base_style)];
    }

    let mut text_lower = String::with_capacity(text.len());
    let mut lower_to_orig: Vec<usize> = Vec::with_capacity(text.len() + 1);
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
    lower_to_orig.push(text.len());

    let mut spans = Vec::new();
    let mut last_end = 0;
    let mut search_from = 0;

    while let Some(pos) = text_lower[search_from..].find(query) {
        let start_lower = search_from + pos;
        let end_lower = start_lower + query.len();
        let start_orig = lower_to_orig[start_lower];
        let end_orig = lower_to_orig[end_lower];

        if start_orig > last_end {
            spans.push(Span::styled(text[last_end..start_orig].to_string(), base_style));
        }
        spans.push(Span::styled(text[start_orig..end_orig].to_string(), match_style));
        last_end = end_orig;
        search_from = end_lower;
        if search_from >= text_lower.len() {
            break;
        }
    }

    if last_end < text.len() {
        spans.push(Span::styled(text[last_end..].to_string(), base_style));
    }

    if spans.is_empty() {
        vec![Span::styled(text.to_string(), base_style)]
    } else {
        spans
    }
}
```

- [ ] **Step 4: Run test to verify pass**

Run: `cargo test --lib ui::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/ui.rs
git commit -m "fix(sessy): highlight_spans handles Unicode case-length changes (Greek/Turkish)"
```

---

## Task 11: Fix orphan bookmarks on delete

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Write failing test**

Append to `src/app.rs` `search_tests` module:

```rust
#[test]
fn test_delete_removes_bookmark() {
    let sessions = vec![make_session("a", "", "t", "p", "main", vec![])];
    let mut bookmarks = HashSet::new();
    bookmarks.insert("a".to_string());
    let mut app = App::new(sessions, false, bookmarks, empty_cache());
    app.selected = 0;
    // Skip actual file deletion in delete_selected by pre-removing the path?
    // Instead, test the bookmark-cleanup helper directly.
    app.cleanup_bookmark_for_deleted("a");
    assert!(!app.bookmarks.contains("a"));
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test --lib app::search_tests::test_delete_removes_bookmark`
Expected: FAIL — `cleanup_bookmark_for_deleted` does not exist.

- [ ] **Step 3: Add helper and call it from `delete_selected`**

Add to `impl App` in `src/app.rs`, near the bottom:

```rust
pub fn cleanup_bookmark_for_deleted(&mut self, id: &str) {
    if self.bookmarks.remove(id) {
        crate::bookmarks::save_bookmarks(&self.bookmarks);
    }
}
```

Edit `delete_selected` (line 398). Insert near the top, before `std::fs::remove_file`:

```rust
let id = self.sessions[real_idx].id.clone();
```

After the file is successfully removed and `self.sessions.remove(real_idx);` runs, add:

```rust
self.cleanup_bookmark_for_deleted(&id);
```

So the full updated `delete_selected` looks like:

```rust
pub fn delete_selected(&mut self) {
    if let Some(&real_idx) = self.filtered_indices.get(self.selected) {
        let id = self.sessions[real_idx].id.clone();
        let path = self.sessions[real_idx].file_path.clone();
        if std::fs::remove_file(&path).is_ok() {
            let companion_dir = path.with_extension("");
            if companion_dir.is_dir() {
                std::fs::remove_dir_all(&companion_dir).ok();
            }
            self.sessions.remove(real_idx);
            self.filtered_indices.retain(|&i| i != real_idx);
            for idx in &mut self.filtered_indices {
                if *idx > real_idx {
                    *idx -= 1;
                }
            }
            if self.selected >= self.filtered_indices.len() && self.selected > 0 {
                self.selected -= 1;
            }
            self.preview_lines.clear();
            self.preview_session_id.clear();
            self.preview_loading = false;
            self.cleanup_bookmark_for_deleted(&id);
        }
    }
    self.confirm_delete = false;
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib app::search_tests::test_delete_removes_bookmark`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "fix(sessy): remove orphan bookmark when session is deleted"
```

---

## Task 12: Show "Sort: relevance" when search active

**Files:**
- Modify: `src/ui.rs`

- [ ] **Step 1: Edit the sort label in `draw_status_bar`**

Edit `src/ui.rs` line 600. Replace:

```rust
let sort_label = format!("sort:{}  ", app.sort_mode.label());
```

with:

```rust
let sort_label = if app.search_query.is_empty() {
    format!("sort:{}  ", app.sort_mode.label())
} else {
    "sort:relevance  ".to_string()
};
```

- [ ] **Step 2: Build and verify**

Run: `cargo build`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src/ui.rs
git commit -m "fix(sessy): status bar shows sort:relevance while query active"
```

---

## Task 13: Wire mmap'd `TextCache` into `App` at startup

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Find and update the `App::new` call site**

Search: `grep -n "App::new" src/main.rs`

For each call to `App::new(sessions, print_mode, bookmarks)`, change to `App::new(sessions, print_mode, bookmarks, text_cache)` where `text_cache` is constructed via:

```rust
let text_cache = crate::text_cache::TextCache::open(&crate::text_cache::text_cache_path());
```

Place this construction immediately after `build_index` returns and before `App::new`. If there are multiple call sites (e.g., main path + print-only path), each gets its own `TextCache::open` (cheap — no I/O).

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: PASS.

- [ ] **Step 3: Run all tests**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat(sessy): open mmap'd text cache and pass to App"
```

---

## Task 14: Add ticket fixture and integration test

**Files:**
- Create: `tests/fixtures/session_with_tickets.jsonl`
- Create: `tests/search_integration.rs`

- [ ] **Step 1: Create the fixture**

Inspect an existing fixture to match the JSONL shape:

Run: `head -3 tests/fixtures/simple_session.jsonl`

Then create `tests/fixtures/session_with_tickets.jsonl` with three lines of the same shape. Use ISO timestamps and these contents:

- Line 1: user message containing `"working on PROJ-123 today"`.
- Line 2: assistant message containing `"ABC-78 is also relevant, #456"`.
- Line 3: user message containing `"final question on PROJ-123"`.

Concrete:

```jsonl
{"type":"user","timestamp":"2026-05-13T10:00:00Z","cwd":"/Users/me/code/agile-turtles/sessy","gitBranch":"main","slug":"ticket-session","message":{"content":"working on PROJ-123 today"}}
{"type":"assistant","timestamp":"2026-05-13T10:00:10Z","message":{"content":[{"type":"text","text":"ABC-78 is also relevant, #456"}]}}
{"type":"user","timestamp":"2026-05-13T10:01:00Z","message":{"content":"final question on PROJ-123"}}
```

- [ ] **Step 2: Add a unit test that scans the fixture**

Append to `src/parser.rs` test module:

```rust
#[test]
fn test_scan_session_extracts_tickets() {
    let result = scan_session(&fixture_path("session_with_tickets.jsonl"));
    let result = result.expect("should scan");
    assert!(result.tickets.contains(&"PROJ-123".to_string()), "got {:?}", result.tickets);
    assert!(result.tickets.contains(&"ABC-78".to_string()), "got {:?}", result.tickets);
    assert!(result.tickets.contains(&"#456".to_string()), "got {:?}", result.tickets);
    // sorted + deduped
    let mut sorted = result.tickets.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted, result.tickets);
}
```

- [ ] **Step 3: Run the parser test**

Run: `cargo test --lib parser::tests::test_scan_session_extracts_tickets`
Expected: PASS.

- [ ] **Step 4: Create integration test**

Create `tests/search_integration.rs`:

```rust
use std::path::PathBuf;
use std::collections::HashSet;
use sessy::app::App;
use sessy::session::SessionMeta;
use sessy::text_cache::TextCache;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn make_session(id: &str, name: &str, project: &str, tickets: Vec<&str>) -> SessionMeta {
    SessionMeta {
        id: id.into(),
        project: project.into(),
        branch: "main".into(),
        name: name.into(),
        title: name.into(),
        last_message: String::new(),
        duration_secs: 0,
        timestamp: 0,
        file_size: 0,
        file_mtime: 0,
        file_path: fixture("simple_session.jsonl"),
        cwd: String::new(),
        message_count: None,
        tickets: tickets.into_iter().map(String::from).collect(),
        text_offset: 0,
        text_len: 0,
        name_lc: name.to_lowercase(),
        title_lc: name.to_lowercase(),
        project_lc: project.to_lowercase(),
        branch_lc: "main".into(),
    }
}

#[test]
fn ticket_query_finds_ticketed_session_first() {
    let mut t = make_session("a", "other", "p", vec!["PROJ-123"]);
    t.tickets.sort();
    let sessions = vec![
        make_session("b", "kerveros", "p", vec![]),
        t,
    ];
    let cache = TextCache::open(std::path::Path::new("/does/not/exist"));
    let mut app = App::new(sessions, false, HashSet::new(), cache);
    app.search_query = "PROJ-123".into();
    app.apply_search();
    assert!(!app.filtered_indices.is_empty());
    assert_eq!(app.sessions[app.filtered_indices[0]].id, "a");
}

#[test]
fn multi_token_and_match() {
    let cache = TextCache::open(std::path::Path::new("/does/not/exist"));
    let sessions = vec![
        make_session("a", "kerveros encryption", "p", vec![]),
        make_session("b", "kerveros", "p", vec![]),
    ];
    let mut app = App::new(sessions, false, HashSet::new(), cache);
    app.search_query = "kerv encrypt".into();
    app.apply_search();
    let ids: Vec<String> = app
        .filtered_indices
        .iter()
        .map(|&i| app.sessions[i].id.clone())
        .collect();
    assert_eq!(ids, vec!["a".to_string()]);
}
```

- [ ] **Step 5: Make crate types publicly reachable for the integration test**

Add `pub` exports to `src/main.rs` (or extract to a `lib.rs`). Simplest: convert sessy into a library + binary. Add `src/lib.rs`:

```rust
pub mod app;
pub mod bookmarks;
pub mod export;
pub mod index;
pub mod parser;
pub mod preview;
pub mod session;
pub mod text_cache;
pub mod ui;
```

Remove the corresponding `mod` declarations from `src/main.rs` (they would now duplicate). Replace usages in `src/main.rs` that were like `use crate::app::App` to `use sessy::app::App` (or similar).

Add to `Cargo.toml` under `[package]`:

```toml
[lib]
name = "sessy"
path = "src/lib.rs"

[[bin]]
name = "sessy"
path = "src/main.rs"
```

- [ ] **Step 6: Run integration test**

Run: `cargo test --test search_integration`
Expected: PASS.

- [ ] **Step 7: Run full test suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add tests/ src/lib.rs src/main.rs Cargo.toml
git commit -m "test(sessy): ticket fixture + search integration suite; expose lib"
```

---

## Task 15: Manual smoke test + version polish

**Files:**
- Modify: none, runtime smoke only
- Verify: `Cargo.toml` version bump still 0.4.0

- [ ] **Step 1: Build release binary**

Run: `cargo build --release`
Expected: PASS, no warnings.

- [ ] **Step 2: Run against real session library**

Run: `./target/release/sessy`

Manually verify:
- App opens and lists sessions.
- Typing into the search bar filters; results re-order by relevance (top result feels like the best match).
- Typing a known ticket ID (if you have one in any session) puts that session at top.
- Multi-word query like `agile encryption` works (AND across tokens).
- Clearing the query restores the date-sorted list.
- Status bar shows `sort:relevance` while typing, then reverts.
- Pressing `/` inside the preview, typing text, then `n`/`N` between matches: scroll lands on the actual line, not estimated.
- Sessions containing Greek characters highlight correctly when preview-searched.

- [ ] **Step 3: Time the per-keystroke cost (informal)**

In the search bar, type rapidly. Confirm there is no visible lag. If lag is felt, run with:

```bash
cargo flamegraph --bin sessy
```

and inspect — but expect it to be fine on typical libraries.

- [ ] **Step 4: Final commit (if any polish needed)**

If you found anything to fix during smoke test, commit it. Otherwise, no commit.

---

## Self-review checklist

- [x] Spec coverage:
  - Two-file cache (index.bin + text.bin) → Tasks 5, 6
  - SessionMeta new fields → Task 2
  - INDEX_VERSION bump → Task 6
  - Cache-invalidity guard → Task 6
  - Single-pass `scan_session` → Task 4
  - Ticket regex + extraction → Task 3
  - Ranking algorithm + parallel scan → Task 7
  - Sort behavior with query active → Task 7
  - Bug 1 preview-search scroll → Task 9
  - Bug 2 preview-search perf → Task 8
  - Bug 3 Unicode highlight → Task 10
  - Bug 4 orphan bookmark → Task 11
  - Bug 5 stale sort indicator → Task 12
  - Mmap wiring → Tasks 5, 13
  - Ticket fixture + integration test → Task 14
  - Version bump 0.3.0 → 0.4.0 → Task 1
  - Drop `fuzzy-matcher` dep → Task 7
- [x] No placeholders / TBDs scanned.
- [x] Type consistency: `cleanup_bookmark_for_deleted`, `recompute_preview_offsets`, `intra_match_chunk_offset`, `wrapped_line_count`, `wrap_for_offsets`, `is_ticket_query`, `starts_at_word_boundary`, `ScanResult`, `TextCache`, `text_cache_path`, `write_text_cache` — all referenced consistently across tasks.
- [x] Task ordering — types/data model first (1, 2), then helpers (3, 4, 5), then pipeline (6), then app behavior (7-12), then wiring (13), then tests + verification (14, 15).
