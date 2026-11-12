//! Subagent transcripts (`<session id>/subagents/agent-<id>.jsonl`, next to
//! an `agent-<id>.meta.json` holding the task's description) are searched as
//! part of their session. In the session's search text they come after its
//! own text, each after a marker line: `MARK`, its description (folded),
//! a newline. A hit after the first `MARK` was found in a subagent.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Starts a subagent's part of the search text; never in the text itself.
pub const MARK: u8 = 0x01;

#[derive(Debug, Clone, PartialEq)]
pub struct SubagentFile {
    pub path: PathBuf,
    pub size: u64,
    /// Nanoseconds since the epoch: a subagent often starts within the same
    /// second as the turn that launched it.
    pub mtime: i64,
    /// Its `.meta.json` (the description): size and mtime hashed, 0 when
    /// there's none. Can be written after the transcript.
    pub meta: i64,
}

pub fn mtime_nanos(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i64)
}

/// `<id>/subagents` next to the transcript `<id>.jsonl`.
fn dir(transcript: &Path) -> PathBuf {
    transcript.with_extension("").join("subagents")
}

/// The session's subagent transcripts, by file name, and their folder's
/// modification time (a new subagent changes it).
pub fn list(transcript: &Path) -> (Vec<SubagentFile>, i64) {
    let dir = dir(transcript);
    let Ok(entries) = fs::read_dir(&dir) else {
        return (Vec::new(), 0);
    };
    let dir_mtime = fs::metadata(&dir).map_or(0, |m| mtime_nanos(&m));
    let mut files: Vec<SubagentFile> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                return None;
            }
            let meta = e.metadata().ok()?;
            let description = fs::metadata(path.with_extension("meta.json"))
                .map_or(0, |m| (m.len() as i64).wrapping_mul(0x100000001b3) ^ mtime_nanos(&m));
            Some(SubagentFile {
                path,
                size: meta.len(),
                mtime: mtime_nanos(&meta),
                meta: description,
            })
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    (files, dir_mtime)
}

/// What the subagent was asked to do (its `.meta.json` `description`), on
/// one line; empty when there's none.
pub fn description(transcript: &Path) -> String {
    let meta = transcript.with_extension("meta.json");
    fs::read_to_string(meta)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("description")?.as_str().map(str::to_string))
        .map(|d| crate::parser::one_line(&d, 200))
        .unwrap_or_default()
}

/// The search text of `files`, each after its marker line.
pub fn search_text(files: &[SubagentFile]) -> String {
    use std::io::BufRead;
    let mut out = String::new();
    for file in files {
        out.push(MARK as char);
        out.push_str(&crate::fold::fold(&description(&file.path)));
        out.push('\n');
        let Ok(f) = fs::File::open(&file.path) else {
            continue;
        };
        // Line by line, past any that isn't UTF-8 or JSON.
        let mut reader = std::io::BufReader::new(f);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(n) if n > 0 => {}
                _ => break,
            }
            if let Ok(entry) = serde_json::from_slice::<serde_json::Value>(&line) {
                crate::parser::append_search_text(&entry, &mut out);
            }
        }
    }
    out
}

/// Where the subagent parts of a session's search text start (`text.len()`
/// when it has none).
pub fn split(text: &[u8]) -> usize {
    memchr::memchr(MARK, text).unwrap_or(text.len())
}

/// The subagents' parts of `sub` (a session's text from `split` on): each
/// one's folded description, and its text's byte range in `sub`.
pub fn parts(sub: &[u8]) -> Vec<(String, std::ops::Range<usize>)> {
    let marks: Vec<usize> = memchr::memchr_iter(MARK, sub).collect();
    marks
        .iter()
        .enumerate()
        .map(|(i, &at)| {
            let end = marks.get(i + 1).copied().unwrap_or(sub.len());
            let line_end = memchr::memchr(b'\n', &sub[at..end]).map_or(end, |n| at + n);
            let description = String::from_utf8_lossy(&sub[at + 1..line_end]).into_owned();
            (description, (line_end + 1).min(end)..end)
        })
        .collect()
}

/// The transcript of the session's `nth` subagent whose description folds
/// to `folded` (descriptions can repeat), and the description as written.
pub fn find(transcript: &Path, folded: &str, nth: usize) -> Option<(PathBuf, String)> {
    let (files, _) = list(transcript);
    files
        .into_iter()
        .map(|f| {
            let written = description(&f.path);
            (f.path, written)
        })
        .filter(|(_, written)| crate::fold::fold(written) == folded)
        .nth(nth)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(dir: &Path, id: &str, description: &str, said: &str) {
        let sub = dir.join("s1/subagents");
        fs::create_dir_all(&sub).unwrap();
        let line = serde_json::json!({"type": "assistant", "isSidechain": true,
            "message": {"role": "assistant", "content": [{"type": "text", "text": said}]}});
        fs::write(sub.join(format!("agent-{}.jsonl", id)), format!("{}\n", line)).unwrap();
        let meta = serde_json::json!({"agentType": "Explore", "description": description});
        fs::write(sub.join(format!("agent-{}.meta.json", id)), meta.to_string()).unwrap();
    }

    #[test]
    fn subagent_text_is_marked_with_each_description() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("s1.jsonl");
        fs::write(&transcript, "").unwrap();
        agent(dir.path(), "b", "Audit the Index", "found a FLOCK race");
        agent(dir.path(), "a", "Review PR #9", "looks fine");
        let (files, dir_mtime) = list(&transcript);
        assert_eq!(files.len(), 2, "the .meta.json files aren't transcripts");
        assert!(dir_mtime > 0);
        let text = search_text(&files);
        let sub = &text.as_bytes()[split(text.as_bytes())..];
        let parts = parts(sub);
        let descriptions: Vec<&str> = parts.iter().map(|(d, _)| d.as_str()).collect();
        assert_eq!(descriptions, ["review pr #9", "audit the index"], "by file name");
        assert_eq!(&sub[parts[1].1.clone()], b"found a flock race\n");
        let (path, written) = find(&transcript, "audit the index", 0).unwrap();
        assert!(path.ends_with("agent-b.jsonl"));
        assert_eq!(written, "Audit the Index");
    }

    #[test]
    fn a_bad_line_skips_only_itself() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("s1.jsonl");
        agent(dir.path(), "a", "Audit", "before");
        let path = dir.path().join("s1/subagents/agent-a.jsonl");
        let after = serde_json::json!({"type": "assistant",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "after the bad line"}]}});
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(b"\xff\xfe\n");
        bytes.extend_from_slice(format!("{}\n", after).as_bytes());
        fs::write(&path, bytes).unwrap();
        let text = search_text(&list(&transcript).0);
        assert!(text.contains("before") && text.contains("after the bad line"), "{:?}", text);
    }

    #[test]
    fn a_description_written_later_changes_the_listing() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("s1.jsonl");
        agent(dir.path(), "a", "Audit", "found it");
        let meta = dir.path().join("s1/subagents/agent-a.meta.json");
        fs::remove_file(&meta).unwrap();
        let before = list(&transcript).0;
        assert_eq!(before[0].meta, 0);
        fs::write(&meta, r#"{"description":"Check the locks"}"#).unwrap();
        assert_ne!(list(&transcript).0, before);
    }

    #[test]
    fn a_session_without_subagents_has_no_subagent_text() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("s1.jsonl");
        assert_eq!(list(&transcript), (Vec::new(), 0));
        assert_eq!(split(b"plain text\n"), 11);
        assert!(parts(b"").is_empty());
    }
}
