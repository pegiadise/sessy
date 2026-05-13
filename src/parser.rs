use regex::Regex;
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
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
            result.human_text_lc.chars().all(|c: char| !c.is_uppercase()),
            "should be lowercased"
        );
    }

    #[test]
    fn test_scan_session_empty_returns_none() {
        let result = scan_session(&fixture_path("empty_session.jsonl"));
        assert!(result.is_none());
    }
}
