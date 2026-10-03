use crate::session::SessionMeta;
use crate::text_cache::{text_cache_path, write_text_cache, TextCache};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const INDEX_VERSION: u32 = 7;

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionIndex {
    pub version: u32,
    pub sessions: Vec<SessionMeta>,
    /// Size of the `text.bin` written alongside this index. A mismatch on load
    /// means the pair is out of sync (crash, or two instances racing), and the
    /// recorded text offsets can't be trusted.
    pub text_cache_len: u64,
}

pub fn index_cache_path() -> PathBuf {
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("sessy");
    fs::create_dir_all(&cache_dir).ok();
    cache_dir.join("index.bin")
}

/// Encode an absolute directory path the way Claude Code names project dirs:
/// every non-alphanumeric character becomes `-` (not just `/` — dots,
/// underscores, spaces, and unicode too, e.g. `/a/.worktrees/x` → `-a--worktrees-x`).
pub fn encode_project_path(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// `~/.claude/projects`, or `$CLAUDE_CONFIG_DIR/projects` when Claude Code
/// has been pointed at another config directory.
pub fn claude_projects_dir() -> PathBuf {
    let config_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(".claude")
        });
    config_dir.join("projects")
}

/// Whether a `.jsonl` file in a project dir is a resumable session. Old
/// Claude Code versions wrote subagent transcripts (`agent-<id>.jsonl`) next
/// to the sessions; those can't be resumed.
fn is_session_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("jsonl")
        && !path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("agent-"))
}

/// bincode 2 with the `legacy` config writes the same bytes as bincode 1's
/// `serialize`/`deserialize`, so caches written by older sessy builds still load.
pub fn serialize_index(index: &SessionIndex) -> Vec<u8> {
    bincode::serde::encode_to_vec(index, bincode::config::legacy()).unwrap_or_default()
}

pub fn deserialize_index(bytes: &[u8]) -> Option<SessionIndex> {
    let (index, _): (SessionIndex, usize) =
        bincode::serde::decode_from_slice(bytes, bincode::config::legacy()).ok()?;
    if index.version != INDEX_VERSION {
        return None;
    }
    Some(index)
}

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

    // Headline priority: an explicit /rename wins, then Claude's AI title,
    // then the random word-slug, then nothing.
    let name = if !rename.is_empty() {
        rename
    } else if !scan.ai_title.is_empty() {
        scan.ai_title.clone()
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
    let changed_files_lc = scan.changed_files.join("\n").to_lowercase();

    let text_bytes = scan.search_text_lc.into_bytes();

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
        message_count: scan.message_count,
        tickets: scan.tickets,
        text_offset: 0, // filled in finalize step
        text_len: text_bytes.len() as u32,
        name_lc,
        title_lc,
        project_lc,
        branch_lc,
        permission_mode: scan.permission_mode,
        cc_version: scan.cc_version,
        skills: scan.skills,
        changed_files: scan.changed_files,
        changed_files_lc,
        prs: scan.prs,
        recap: scan.recap,
    };
    Some((meta, text_bytes))
}

fn compute_duration(first: &str, last: &str) -> u64 {
    use chrono::DateTime;
    let parse = |s: &str| -> Option<i64> {
        DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.timestamp())
    };
    match (parse(first), parse(last)) {
        (Some(a), Some(b)) if b >= a => (b - a) as u64,
        _ => 0,
    }
}

pub fn build_index(cached: Option<SessionIndex>, force_rebuild: bool) -> SessionIndex {
    let projects_dir = claude_projects_dir();
    if !projects_dir.exists() {
        return SessionIndex {
            version: INDEX_VERSION,
            sessions: vec![],
            text_cache_len: 0,
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
                    if is_session_file(&path) && path.is_file() {
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
        Err(e) => {
            eprintln!("sessy: failed to write text cache: {}", e);
            // Without text.bin, full-text search just finds nothing; keep the
            // sessions browsable. A zero cache length forces a rescan next time.
            let sessions = scanned
                .into_iter()
                .map(|(mut meta, _)| {
                    meta.text_offset = 0;
                    meta.text_len = 0;
                    meta
                })
                .collect();
            return SessionIndex {
                version: INDEX_VERSION,
                sessions,
                text_cache_len: u64::MAX,
            };
        }
    };

    let text_cache_len = offsets
        .last()
        .map(|&(offset, len)| offset + len as u64)
        .unwrap_or(0);
    let sessions: Vec<SessionMeta> = scanned
        .into_iter()
        .zip(offsets)
        .map(|((mut meta, _), (offset, len))| {
            meta.text_offset = offset;
            meta.text_len = len;
            meta
        })
        .collect();

    SessionIndex {
        version: INDEX_VERSION,
        sessions,
        text_cache_len,
    }
}

pub fn load_cached_index() -> Option<SessionIndex> {
    let path = index_cache_path();
    let bytes = fs::read(&path).ok()?;
    let index = deserialize_index(&bytes)?;
    let text_len = fs::metadata(text_cache_path()).map(|m| m.len()).unwrap_or(0);
    if text_len != index.text_cache_len {
        return None;
    }
    Some(index)
}

pub fn save_index(index: &SessionIndex) {
    let path = index_cache_path();
    let bytes = serialize_index(index);
    // Write-then-rename so a crash or a concurrent instance never leaves a
    // half-written index behind.
    let tmp = path.with_extension(format!("bin.{}.tmp", std::process::id()));
    if fs::write(&tmp, bytes).is_ok() && fs::rename(&tmp, &path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

pub fn parse_recent_filter(s: &str) -> Option<u64> {
    let s = s.trim();
    let unit = s.chars().last()?;
    let num_str = &s[..s.len() - unit.len_utf8()];
    let num: u64 = num_str.parse().ok()?;
    let unit_secs: u64 = match unit {
        'h' => 3600,
        'd' => 86400,
        'w' => 7 * 86400,
        'm' => 30 * 86400,
        _ => return None,
    };
    // Reject absurd windows instead of overflowing into a bogus cutoff.
    num.checked_mul(unit_secs).filter(|&secs| secs <= i64::MAX as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

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

    #[test]
    fn test_scan_session_file_empty_returns_none() {
        let result = scan_session_file(&fixture_path("empty_session.jsonl"));
        assert!(result.is_none(), "empty session should be filtered out");
    }

    #[test]
    fn test_scan_session_file_uses_ai_title() {
        let (meta, _text) = scan_session_file(&fixture_path("modern_session.jsonl"))
            .expect("should produce SessionMeta + text");
        // No /rename and no slug → aiTitle becomes the headline name.
        assert_eq!(meta.name, "Add JWT login endpoint");
        assert_eq!(meta.message_count, 2);
        assert_eq!(meta.permission_mode, "plan");
        assert_eq!(meta.cc_version, "2.1.0");
        assert!(
            meta.changed_files_lc.contains("src/auth.rs"),
            "changed_files_lc: {:?}",
            meta.changed_files_lc
        );
        assert!(meta.skills.contains(&"brainstorming".to_string()));
    }

    #[test]
    fn test_index_serialization_roundtrip() {
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
            message_count: 0,
            tickets: vec![],
            text_offset: 0,
            text_len: 0,
            name_lc: String::new(),
            title_lc: "hello world".to_string(),
            project_lc: "test".to_string(),
            branch_lc: "main".to_string(),
            permission_mode: String::new(),
            cc_version: String::new(),
            skills: vec![],
            changed_files: vec![],
            changed_files_lc: String::new(),
            prs: vec![],
            recap: String::new(),
        }];
        let index = SessionIndex {
            version: INDEX_VERSION,
            sessions,
            text_cache_len: 0,
        };
        let bytes = serialize_index(&index);
        let restored = deserialize_index(&bytes);
        assert!(restored.is_some());
        let restored = restored.unwrap();
        assert_eq!(restored.sessions.len(), 1);
        assert_eq!(restored.sessions[0].title, "hello world");
        assert_eq!(restored.sessions[0].last_message, "goodbye");
    }

    #[test]
    fn test_index_deserialization_wrong_version_returns_none() {
        let index = SessionIndex {
            version: 999,
            sessions: vec![],
            text_cache_len: 0,
        };
        let bytes = serialize_index(&index);
        let restored = deserialize_index(&bytes);
        assert!(restored.is_none());
    }

    #[test]
    fn test_encode_project_path_plain() {
        assert_eq!(
            encode_project_path("/Users/me/code/foo"),
            "-Users-me-code-foo"
        );
    }

    #[test]
    fn test_encode_project_path_dots_underscores_unicode() {
        // Claude Code replaces every non-alphanumeric char, not just `/`.
        assert_eq!(
            encode_project_path("/Users/me/code/acme/web/.worktrees/abc-6370"),
            "-Users-me-code-acme-web--worktrees-abc-6370"
        );
        assert_eq!(encode_project_path("/srv/my_app.v2"), "-srv-my-app-v2");
        assert_eq!(encode_project_path("/home/ρώτα με"), "-home--------");
    }

    #[test]
    fn test_parse_recent_filter() {
        assert_eq!(parse_recent_filter("7d"), Some(7 * 86400));
        assert_eq!(parse_recent_filter("1h"), Some(3600));
        assert_eq!(parse_recent_filter("2w"), Some(14 * 86400));
        assert_eq!(parse_recent_filter("1m"), Some(30 * 86400));
        assert_eq!(parse_recent_filter("garbage"), None);
        assert_eq!(parse_recent_filter("d"), None);
        assert_eq!(parse_recent_filter("99999999999999999999d"), None);
        assert_eq!(parse_recent_filter("999999999999999w"), None);
    }

    #[test]
    fn test_agent_transcripts_are_not_sessions() {
        assert!(is_session_file(Path::new("/p/-a/1b2c.jsonl")));
        assert!(!is_session_file(Path::new("/p/-a/agent-a5c3eb79.jsonl")));
        assert!(!is_session_file(Path::new("/p/-a/notes.txt")));
    }
}
