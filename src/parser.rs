use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone)]
pub struct ConversationMessage {
    pub role: Role,
    pub text: String,
}

pub struct HeadMeta {
    pub title: String,
    pub branch: String,
    pub slug: String,
    pub first_timestamp: String,
    pub cwd: String,
}

pub struct TailMeta {
    pub last_human_message: String,
    pub last_timestamp: String,
    pub rename: String,
}

fn is_human_message(entry: &Value) -> bool {
    entry.get("type").and_then(|t| t.as_str()) == Some("user")
        && entry.get("isMeta").and_then(|m| m.as_bool()) != Some(true)
        && entry.get("toolUseResult").is_none()
        && entry
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .is_some()
}

fn human_message_text(entry: &Value) -> Option<String> {
    entry
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| {
            let trimmed = s.trim();
            if trimmed.chars().count() > 200 {
                let end = trimmed
                    .char_indices()
                    .nth(200)
                    .map(|(i, _)| i)
                    .unwrap_or(trimmed.len());
                format!("{}…", &trimmed[..end])
            } else {
                trimmed.to_string()
            }
        })
}

pub fn extract_head_meta(path: &Path) -> Option<HeadMeta> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut branch = String::new();
    let mut slug = String::new();
    let mut first_timestamp = String::new();
    let mut cwd = String::new();

    for line in reader.lines() {
        let line = line.ok()?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: Value = serde_json::from_str(&line).ok()?;

        if branch.is_empty() {
            if let Some(b) = entry.get("gitBranch").and_then(|b| b.as_str()) {
                branch = b.to_string();
            }
        }
        if slug.is_empty() {
            if let Some(s) = entry.get("slug").and_then(|s| s.as_str()) {
                slug = s.to_string();
            }
        }
        if first_timestamp.is_empty() {
            if let Some(ts) = entry.get("timestamp").and_then(|t| t.as_str()) {
                first_timestamp = ts.to_string();
            }
        }
        if cwd.is_empty() {
            if let Some(c) = entry.get("cwd").and_then(|c| c.as_str()) {
                cwd = c.to_string();
            }
        }

        if is_human_message(&entry) {
            let title = human_message_text(&entry).unwrap_or_default();
            return Some(HeadMeta {
                title,
                branch,
                slug,
                first_timestamp,
                cwd,
            });
        }
    }
    None
}

pub fn extract_tail_meta(path: &Path) -> Option<TailMeta> {
    let mut file = File::open(path).ok()?;
    let file_size = file.metadata().ok()?.len();
    let seek_pos = file_size.saturating_sub(8192);
    file.seek(SeekFrom::Start(seek_pos)).ok()?;

    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;

    let mut last_human_message = String::new();
    let mut last_timestamp = String::new();
    let mut rename = String::new();

    for line in buf.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<Value>(line) {
            if let Some(ts) = entry.get("timestamp").and_then(|t| t.as_str()) {
                last_timestamp = ts.to_string();
            }
            if is_human_message(&entry) {
                if let Some(text) = human_message_text(&entry) {
                    last_human_message = text;
                }
            }
            // Check for /rename command
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
    }

    if last_human_message.is_empty() && last_timestamp.is_empty() {
        return None;
    }
    Some(TailMeta {
        last_human_message,
        last_timestamp,
        rename,
    })
}

pub fn extract_conversation(path: &Path) -> Vec<ConversationMessage> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return vec![],
    };
    let reader = BufReader::new(file);
    let mut messages = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if entry
            .get("isSidechain")
            .and_then(|s| s.as_bool())
            == Some(true)
        {
            continue;
        }

        let entry_type = entry
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("");

        match entry_type {
            "user" if is_human_message(&entry) => {
                if let Some(text) = entry
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        messages.push(ConversationMessage {
                            role: Role::User,
                            text: trimmed.to_string(),
                        });
                    }
                }
            }
            "assistant" => {
                if let Some(content) = entry
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for block in content {
                        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                let trimmed = text.trim();
                                if !trimmed.is_empty() {
                                    messages.push(ConversationMessage {
                                        role: Role::Assistant,
                                        text: trimmed.to_string(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    messages
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
    fn test_extract_head_meta_simple() {
        let meta = extract_head_meta(&fixture_path("simple_session.jsonl"));
        let meta = meta.expect("should parse successfully");
        assert_eq!(meta.title, "build a cool thing");
        assert_eq!(meta.branch, "main");
    }

    #[test]
    fn test_extract_head_meta_empty() {
        let meta = extract_head_meta(&fixture_path("empty_session.jsonl"));
        assert!(meta.is_none(), "empty session should return None");
    }

    #[test]
    fn test_extract_head_meta_complex_skips_meta() {
        let meta = extract_head_meta(&fixture_path("complex_session.jsonl"));
        let meta = meta.expect("should parse successfully");
        assert_eq!(meta.title, "implement auth middleware");
        assert_eq!(meta.branch, "feat/auth");
    }

    #[test]
    fn test_extract_tail_meta() {
        let meta = extract_tail_meta(&fixture_path("simple_session.jsonl"));
        let meta = meta.expect("should parse successfully");
        assert_eq!(meta.last_human_message, "looks good, ship it");
        assert!(meta.last_timestamp.contains("2026-03-13T01:30:30"));
    }

    #[test]
    fn test_extract_tail_meta_complex() {
        let meta = extract_tail_meta(&fixture_path("complex_session.jsonl"));
        let meta = meta.expect("should parse successfully");
        assert_eq!(meta.last_human_message, "add rate limiting too");
    }

    #[test]
    fn test_extract_conversation_filters_correctly() {
        let messages = extract_conversation(&fixture_path("simple_session.jsonl"));
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].text, "build a cool thing");
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[1].text, "Sure, let me help you build that.");
        assert_eq!(messages[2].role, Role::User);
        assert_eq!(messages[2].text, "looks good, ship it");
        assert_eq!(messages[3].role, Role::Assistant);
        assert_eq!(messages[3].text, "Done! Everything is deployed.");
    }

    #[test]
    fn test_extract_conversation_skips_sidechain() {
        let messages = extract_conversation(&fixture_path("complex_session.jsonl"));
        let texts: Vec<&str> = messages.iter().map(|m| m.text.as_str()).collect();
        assert!(!texts.contains(&"This is a sidechain message."));
    }

    #[test]
    fn test_extract_conversation_skips_meta_user() {
        let messages = extract_conversation(&fixture_path("complex_session.jsonl"));
        let texts: Vec<&str> = messages.iter().map(|m| m.text.as_str()).collect();
        assert!(!texts.contains(&"skill loaded: auth-helper"));
        assert!(!texts.iter().any(|t| t.contains("local-command-caveat")));
    }
}
