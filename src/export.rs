use crate::parser::{extract_conversation, Role};
use crate::session::SessionMeta;
use std::io;
use std::path::PathBuf;

pub fn export_session(session: &SessionMeta) -> io::Result<PathBuf> {
    let messages = extract_conversation(&session.file_path);

    let display_name = session.display_name();

    let mut md = String::with_capacity(messages.len() * 200);
    md.push_str(&format!("# {}\n\n", display_name));
    md.push_str(&format!("- **Session**: `{}`\n", session.id));
    md.push_str(&format!("- **Project**: {}\n", session.project));
    if !session.branch.is_empty() {
        md.push_str(&format!("- **Branch**: {}\n", session.branch));
    }
    for pr in &session.prs {
        md.push_str(&format!("- **PR**: [#{}]({})\n", pr.number, pr.url));
    }
    md.push_str(&format!(
        "- **Duration**: {}\n",
        crate::session::format_duration(session.duration_secs)
    ));
    md.push_str(&format!(
        "- **Size**: {}\n\n",
        crate::session::format_file_size(session.file_size)
    ));
    md.push_str("---\n\n");

    for msg in &messages {
        let heading = match msg.role {
            Role::User => "**User**",
            Role::Assistant => "**Assistant**",
        };
        md.push_str(heading);
        md.push_str("\n\n");
        md.push_str(&msg.text);
        md.push_str("\n\n");
    }

    let path = PathBuf::from(export_filename(session));
    std::fs::write(&path, &md)?;
    Ok(path)
}

/// `<name>-<id prefix>.md`: readable, and two sessions that share a name
/// (common with AI titles) never overwrite each other's export.
fn export_filename(session: &SessionMeta) -> String {
    let id_prefix: String = session.id.chars().take(8).collect();
    let name: String = sanitize_filename(&session.name).chars().take(60).collect();
    let name = name.trim_matches('-');
    if name.is_empty() {
        format!("{}.md", sanitize_filename(&session.id))
    } else {
        format!("{}-{}.md", name, id_prefix)
    }
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("hello world/foo"), "hello-world-foo");
        assert_eq!(sanitize_filename("my-session_v2"), "my-session_v2");
        assert_eq!(sanitize_filename("a.b.c"), "a-b-c");
    }

    #[test]
    fn test_export_filename_is_unique_per_session() {
        let mut s = crate::session::SessionMeta {
            id: "1b2c3d4e-aaaa-bbbb".into(),
            project: String::new(),
            branch: String::new(),
            name: "Fix login / auth".into(),
            title: String::new(),
            last_message: String::new(),
            duration_secs: 0,
            timestamp: 0,
            file_size: 0,
            file_mtime: 0,
            file_path: PathBuf::new(),
            cwd: String::new(),
            message_count: 0,
            tickets: vec![],
            text_offset: 0,
            text_len: 0,
            name_lc: String::new(),
            title_lc: String::new(),
            project_lc: String::new(),
            branch_lc: String::new(),
            permission_mode: String::new(),
            cc_version: String::new(),
            skills: vec![],
            changed_files: vec![],
            changed_files_lc: String::new(),
            prs: vec![],
            recap: String::new(),
        };
        assert_eq!(export_filename(&s), "Fix-login---auth-1b2c3d4e.md");
        s.name = String::new();
        assert_eq!(export_filename(&s), "1b2c3d4e-aaaa-bbbb.md");
    }
}
