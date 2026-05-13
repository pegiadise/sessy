use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

/// Extract a human-readable project name from a `cwd` path.
pub fn extract_project_name(cwd: &str, home_dir: &str) -> String {
    let code_prefix = format!("{}/code/", home_dir);
    if let Some(rest) = cwd.strip_prefix(&code_prefix) {
        return rest.to_string();
    }
    if let Some(rest) = cwd.strip_prefix(&format!("{}/", home_dir)) {
        return rest.to_string();
    }
    cwd.to_string()
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

/// Format seconds into human-readable duration: "2h12m", "5m", "< 1m"
pub fn format_duration(secs: u64) -> String {
    if secs < 60 {
        return "< 1m".to_string();
    }
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    if hours > 0 {
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
        assert_eq!(extract_project_name("/Users/me/code/pitcher/web", home), "pitcher/web");
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
}
