use crate::parser::{extract_conversation, Role};
use crate::session::SessionMeta;
use std::io;
use std::path::PathBuf;

pub fn export_session(session: &SessionMeta) -> io::Result<PathBuf> {
    let messages = extract_conversation(&session.file_path);

    let display_name = if !session.name.is_empty() {
        &session.name
    } else {
        &session.title
    };

    let mut md = String::with_capacity(messages.len() * 200);
    md.push_str(&format!("# {}\n\n", display_name));
    md.push_str(&format!("- **Project**: {}\n", session.project));
    if !session.branch.is_empty() {
        md.push_str(&format!("- **Branch**: {}\n", session.branch));
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

    let filename = if !session.name.is_empty() {
        sanitize_filename(&session.name)
    } else {
        sanitize_filename(&session.id)
    };
    let path = PathBuf::from(format!("{}.md", filename));
    std::fs::write(&path, &md)?;
    Ok(path)
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
}
