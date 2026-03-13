use crate::parser::{extract_head_meta, extract_tail_meta};
use crate::session::SessionMeta;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const INDEX_VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionIndex {
    pub version: u32,
    pub sessions: Vec<SessionMeta>,
}

pub fn index_cache_path() -> PathBuf {
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("sessy");
    fs::create_dir_all(&cache_dir).ok();
    cache_dir.join("index.bin")
}

pub fn claude_projects_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".claude")
        .join("projects")
}

pub fn serialize_index(index: &SessionIndex) -> Vec<u8> {
    bincode::serialize(index).unwrap_or_default()
}

pub fn deserialize_index(bytes: &[u8]) -> Option<SessionIndex> {
    let index: SessionIndex = bincode::deserialize(bytes).ok()?;
    if index.version != INDEX_VERSION {
        return None;
    }
    Some(index)
}

pub fn scan_session_file(path: &Path) -> Option<SessionMeta> {
    let metadata = fs::metadata(path).ok()?;
    let file_size = metadata.len();
    let file_mtime = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let id = path.file_stem()?.to_str()?.to_string();

    let head = extract_head_meta(path)?;

    let home_dir = dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let project = crate::session::extract_project_name(&head.cwd, &home_dir);

    let tail = extract_tail_meta(path);
    let (last_message, duration_secs, rename) = if let Some(tail) = tail {
        let duration = compute_duration(&head.first_timestamp, &tail.last_timestamp);
        (tail.last_human_message, duration, tail.rename)
    } else {
        (String::new(), 0, String::new())
    };

    // Session name: /rename > slug > empty
    let name = if !rename.is_empty() {
        rename
    } else if !head.slug.is_empty() {
        head.slug
    } else {
        String::new()
    };

    // Filter out "HEAD" — it's noise from detached HEAD or non-git dirs
    let branch = if head.branch == "HEAD" {
        String::new()
    } else {
        head.branch
    };

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
    })
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
        };
    }

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

    let sessions: Vec<SessionMeta> = file_entries
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
                    return Some((*cached_entry).clone());
                }
            }
            scan_session_file(path)
        })
        .collect();

    SessionIndex {
        version: INDEX_VERSION,
        sessions,
    }
}

pub fn load_cached_index() -> Option<SessionIndex> {
    let path = index_cache_path();
    let bytes = fs::read(&path).ok()?;
    deserialize_index(&bytes)
}

pub fn save_index(index: &SessionIndex) {
    let path = index_cache_path();
    let bytes = serialize_index(index);
    fs::write(&path, bytes).ok();
}

pub fn parse_recent_filter(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.len() < 2 {
        return None;
    }
    let (num_str, unit) = s.split_at(s.len() - 1);
    let num: u64 = num_str.parse().ok()?;
    match unit {
        "h" => Some(num * 3600),
        "d" => Some(num * 86400),
        "w" => Some(num * 7 * 86400),
        "m" => Some(num * 30 * 86400),
        _ => None,
    }
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
        let meta = scan_session_file(&fixture_path("simple_session.jsonl"));
        let meta = meta.expect("should produce SessionMeta");
        assert_eq!(meta.title, "build a cool thing");
        assert_eq!(meta.last_message, "looks good, ship it");
        assert_eq!(meta.branch, "main");
        assert!(meta.duration_secs > 0);
    }

    #[test]
    fn test_scan_session_file_empty_returns_none() {
        let meta = scan_session_file(&fixture_path("empty_session.jsonl"));
        assert!(meta.is_none(), "empty session should be filtered out");
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
            message_count: None,
        }];
        let index = SessionIndex {
            version: INDEX_VERSION,
            sessions,
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
        };
        let bytes = serialize_index(&index);
        let restored = deserialize_index(&bytes);
        assert!(restored.is_none());
    }

    #[test]
    fn test_parse_recent_filter() {
        assert_eq!(parse_recent_filter("7d"), Some(7 * 86400));
        assert_eq!(parse_recent_filter("1h"), Some(3600));
        assert_eq!(parse_recent_filter("2w"), Some(14 * 86400));
        assert_eq!(parse_recent_filter("1m"), Some(30 * 86400));
        assert_eq!(parse_recent_filter("garbage"), None);
    }
}
