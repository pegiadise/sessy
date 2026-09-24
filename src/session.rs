use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    pub message_count: u32,
    pub tickets: Vec<String>,
    pub text_offset: u64,
    pub text_len: u32,
    pub name_lc: String,
    pub title_lc: String,
    pub project_lc: String,
    pub branch_lc: String,
    /// Last permission mode seen ("plan", "acceptEdits", "bypassPermissions", "default", or "").
    pub permission_mode: String,
    /// Claude Code version that produced the session.
    pub cc_version: String,
    /// Skills attributed during the session, sorted + deduped.
    pub skills: Vec<String>,
    /// Files Claude edited (union of tracked backups), sorted.
    pub changed_files: Vec<String>,
    /// `changed_files` newline-joined + lowercased, for substring search.
    pub changed_files_lc: String,
    /// Pull requests Claude Code linked to the session.
    pub prs: Vec<crate::parser::PrLink>,
    /// Latest "while you were away" recap, or empty.
    pub recap: String,
}

impl SessionMeta {
    /// Headline shown for the session: its name, else its first message.
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.title
        } else {
            &self.name
        }
    }
}

/// Name a session the way Claude Code's `/rename` does: append a
/// `custom-title` entry to its transcript. The file's modification time is
/// restored afterwards so renaming doesn't bump the session to the top of
/// date-sorted lists (sessy's and `claude --resume`'s).
pub fn write_custom_title(path: &Path, session_id: &str, title: &str) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut file = std::fs::OpenOptions::new().read(true).append(true).open(path)?;
    let meta = file.metadata()?;
    let mtime = meta.modified().ok();
    // Claude Code may be mid-write; never glue our entry onto a partial line.
    let mut needs_newline = false;
    if meta.len() > 0 {
        file.seek(SeekFrom::Start(meta.len() - 1))?;
        let mut last = [0u8; 1];
        file.read_exact(&mut last)?;
        needs_newline = last[0] != b'\n';
    }
    let entry = serde_json::json!({
        "type": "custom-title",
        "customTitle": title,
        "sessionId": session_id,
    });
    let mut line = String::new();
    if needs_newline {
        line.push('\n');
    }
    line.push_str(&entry.to_string());
    line.push('\n');
    file.write_all(line.as_bytes())?;
    if let Some(t) = mtime {
        let _ = file.set_modified(t);
    }
    Ok(())
}

/// Extract a human-readable project name from a `cwd` path: the path under
/// the home directory, minus a conventional source-root folder (`~/code/foo`,
/// `~/projects/foo`, … → `foo`).
pub fn extract_project_name(cwd: &str, home_dir: &str) -> String {
    const SOURCE_ROOTS: &[&str] = &[
        "code", "src", "dev", "projects", "Projects", "repos", "git", "work", "workspace",
        "Developer", "Code",
    ];
    if home_dir.is_empty() {
        return cwd.to_string();
    }
    if cwd == home_dir {
        return "~".to_string();
    }
    let Some(rest) = cwd.strip_prefix(&format!("{}/", home_dir)) else {
        return cwd.to_string();
    };
    for root in SOURCE_ROOTS {
        if let Some(inner) = rest.strip_prefix(&format!("{}/", root)) {
            if !inner.is_empty() {
                return inner.to_string();
            }
        }
    }
    rest.to_string()
}

/// Format file size into human-readable string: "1.2 KB", "34 MB"
pub fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Categorize a session by file size into a label.
pub fn size_category(bytes: u64) -> &'static str {
    if bytes < 1024 * 1024 {
        "quick"
    } else if bytes < 10 * 1024 * 1024 {
        "medium"
    } else if bytes < 30 * 1024 * 1024 {
        "deep"
    } else {
        "massive"
    }
}

/// Format seconds into human-readable duration: "3d4h", "2h12m", "5m", "< 1m"
pub fn format_duration(secs: u64) -> String {
    if secs < 60 {
        return "< 1m".to_string();
    }
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    if days > 0 {
        format!("{}d{}h", days, hours)
    } else if hours > 0 {
        format!("{}h{}m", hours, minutes)
    } else {
        format!("{}m", minutes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_name_from_cwd() {
        let home = "/Users/me";
        assert_eq!(extract_project_name("/Users/me/code/agile-turtles", home), "agile-turtles");
    }

    #[test]
    fn test_project_name_from_cwd_nested() {
        let home = "/Users/me";
        assert_eq!(extract_project_name("/Users/me/code/agile-turtles/side-income", home), "agile-turtles/side-income");
    }

    #[test]
    fn test_project_name_from_cwd_deep() {
        let home = "/Users/me";
        assert_eq!(extract_project_name("/Users/me/code/acme/web", home), "acme/web");
    }

    #[test]
    fn test_project_name_fallback() {
        let home = "/Users/me";
        assert_eq!(extract_project_name("/other/path/project", home), "/other/path/project");
    }

    #[test]
    fn test_format_duration_minutes() {
        assert_eq!(format_duration(300), "5m");
    }

    #[test]
    fn test_format_duration_hours_minutes() {
        assert_eq!(format_duration(7920), "2h12m");
    }

    #[test]
    fn test_format_duration_under_minute() {
        assert_eq!(format_duration(30), "< 1m");
    }

    #[test]
    fn test_format_duration_zero() {
        assert_eq!(format_duration(0), "< 1m");
    }

    #[test]
    fn test_format_duration_exact_hour() {
        assert_eq!(format_duration(3600), "1h0m");
    }

    #[test]
    fn test_write_custom_title_appends_entry_and_keeps_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        // No trailing newline: the entry must still land on its own line.
        std::fs::write(&path, r#"{"type":"user","message":{"content":"hi"}}"#).unwrap();
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_custom_title(&path, "abc", "Auth \"rework\"").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "got {:?}", content);
        let entry: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(entry["type"], "custom-title");
        assert_eq!(entry["customTitle"], "Auth \"rework\"");
        assert_eq!(entry["sessionId"], "abc");
        let after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(before, after, "rename must not bump the session's date");

        let scan = crate::parser::scan_session(&path).unwrap();
        assert_eq!(scan.tail.unwrap().rename, "Auth \"rework\"");
    }

    #[test]
    fn test_format_duration_days() {
        // Resumed sessions span days; "312h8m" is unreadable.
        assert_eq!(format_duration(13 * 86400 + 5 * 3600 + 120), "13d5h");
    }

    #[test]
    fn test_project_name_other_source_roots() {
        let home = "/home/ann";
        assert_eq!(extract_project_name("/home/ann/projects/shop", home), "shop");
        assert_eq!(extract_project_name("/home/ann/notes", home), "notes");
        assert_eq!(extract_project_name("/home/ann/code", home), "code");
        assert_eq!(extract_project_name("/home/ann", home), "~");
    }

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
            message_count: 0,
            tickets: vec!["PROJ-1".into()],
            text_offset: 100,
            text_len: 50,
            name_lc: String::new(),
            title_lc: "title".into(),
            project_lc: "p".into(),
            branch_lc: "main".into(),
            permission_mode: String::new(),
            cc_version: String::new(),
            skills: vec![],
            changed_files: vec![],
            changed_files_lc: String::new(),
            prs: vec![],
            recap: String::new(),
        };
        assert_eq!(m.tickets[0], "PROJ-1");
        assert_eq!(m.text_offset, 100);
        assert_eq!(m.title_lc, "title");
    }
}
