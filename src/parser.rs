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

/// Who produced a preview line. `Tool` lines are only emitted when tool
/// activity is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    User,
    Assistant,
    Tool,
}

impl Speaker {
    /// Render prefix shown before the message text.
    pub fn prefix(self) -> &'static str {
        match self {
            Speaker::User => "USER: ",
            Speaker::Assistant => "ASST: ",
            Speaker::Tool => "TOOL: ",
        }
    }

    /// Lowercased prefix, used when wrapping pre-lowercased text for search.
    pub fn prefix_lc(self) -> &'static str {
        match self {
            Speaker::User => "user: ",
            Speaker::Assistant => "asst: ",
            Speaker::Tool => "tool: ",
        }
    }
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
    /// Claude-generated session title (`type: "ai-title"`), if present.
    pub ai_title: String,
    /// Last `permissionMode` seen (e.g. "plan", "acceptEdits", "bypassPermissions").
    pub permission_mode: String,
    /// Claude Code `version` field (first non-empty seen).
    pub cc_version: String,
    /// Distinct `attributionSkill` values, sorted.
    pub skills: Vec<String>,
    /// Sorted union of `trackedFileBackups` paths across file-history snapshots.
    pub changed_files: Vec<String>,
    /// Count of non-empty human messages.
    pub message_count: u32,
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
    let mut ai_title = String::new();
    let mut permission_mode = String::new();
    let mut cc_version = String::new();
    let mut skills_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut changed_files_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut message_count: u32 = 0;

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

        // Modern-format metadata, harvested in the same single pass.
        match entry.get("type").and_then(|t| t.as_str()) {
            Some("ai-title") => {
                if let Some(t) = entry.get("aiTitle").and_then(|t| t.as_str()) {
                    ai_title = t.to_string();
                }
            }
            Some("permission-mode") => {
                if let Some(m) = entry.get("permissionMode").and_then(|m| m.as_str()) {
                    permission_mode = m.to_string();
                }
            }
            Some("file-history-snapshot") => {
                if let Some(backups) = entry
                    .get("snapshot")
                    .and_then(|s| s.get("trackedFileBackups"))
                    .and_then(|b| b.as_object())
                {
                    for path in backups.keys() {
                        changed_files_set.insert(path.clone());
                    }
                }
            }
            _ => {}
        }
        if cc_version.is_empty() {
            if let Some(v) = entry.get("version").and_then(|v| v.as_str()) {
                cc_version = v.to_string();
            }
        }
        if let Some(skill) = entry.get("attributionSkill").and_then(|s| s.as_str()) {
            if !skill.is_empty() {
                skills_set.insert(skill.to_string());
            }
        }

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
                    // Count only non-sidechain turns, to match the conversation
                    // preview (which skips sidechains) and the `· N msgs` label.
                    if entry.get("isSidechain").and_then(|s| s.as_bool()) != Some(true) {
                        message_count += 1;
                    }
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
    let mut skills: Vec<String> = skills_set.into_iter().collect();
    skills.sort();
    let mut changed_files: Vec<String> = changed_files_set.into_iter().collect();
    changed_files.sort();

    Some(ScanResult {
        head,
        tail,
        human_text_lc,
        tickets,
        ai_title,
        permission_mode,
        cc_version,
        skills,
        changed_files,
        message_count,
    })
}

/// User + assistant text messages, with tool use filtered out. Used by export.
pub fn extract_conversation(path: &Path) -> Vec<ConversationMessage> {
    extract_conversation_ext(path, false)
        .into_iter()
        .map(|(speaker, text)| ConversationMessage {
            role: match speaker {
                Speaker::User => Role::User,
                _ => Role::Assistant,
            },
            text,
        })
        .collect()
}

/// Extract the conversation as `(speaker, text)` pairs. When `include_tools` is
/// true, assistant `tool_use` blocks are surfaced as `Speaker::Tool` lines with
/// a compact summary (e.g. "Edit src/auth.rs", "Bash cargo test").
pub fn extract_conversation_ext(path: &Path, include_tools: bool) -> Vec<(Speaker, String)> {
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

        if entry.get("isSidechain").and_then(|s| s.as_bool()) == Some(true) {
            continue;
        }

        let entry_type = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match entry_type {
            "user" if is_human_message(&entry) => {
                if let Some(text) = entry
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        messages.push((Speaker::User, trimmed.to_string()));
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
                        match block.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    let trimmed = text.trim();
                                    if !trimmed.is_empty() {
                                        messages.push((Speaker::Assistant, trimmed.to_string()));
                                    }
                                }
                            }
                            Some("tool_use") if include_tools => {
                                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                                messages.push((Speaker::Tool, summarize_tool_use(name, block.get("input"))));
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    messages
}

/// Build a one-line summary of a tool call: the tool name plus its most telling
/// argument (file path, command, pattern, …), truncated.
fn summarize_tool_use(name: &str, input: Option<&Value>) -> String {
    let detail = input
        .and_then(|inp| {
            ["file_path", "path", "command", "pattern", "query", "url", "description"]
                .iter()
                .find_map(|k| inp.get(*k).and_then(|v| v.as_str()))
        })
        .unwrap_or("")
        .trim()
        .replace('\n', " ");
    if detail.is_empty() {
        name.to_string()
    } else {
        let detail: String = detail.chars().take(120).collect();
        format!("{} {}", name, detail)
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
    fn test_extract_with_tools_includes_tool_lines() {
        let with = extract_conversation_ext(&fixture_path("complex_session.jsonl"), true);
        let without = extract_conversation_ext(&fixture_path("complex_session.jsonl"), false);
        assert!(
            with.iter().any(|(sp, _)| *sp == Speaker::Tool),
            "include_tools=true should surface a Tool line"
        );
        assert!(
            !without.iter().any(|(sp, _)| *sp == Speaker::Tool),
            "include_tools=false must not surface Tool lines"
        );
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

    #[test]
    fn test_scan_modern_extracts_metadata() {
        let result = scan_session(&fixture_path("modern_session.jsonl")).expect("should scan");
        assert_eq!(result.ai_title, "Add JWT login endpoint");
        assert_eq!(result.permission_mode, "plan");
        assert_eq!(result.cc_version, "2.1.0");
        assert!(
            result.skills.contains(&"brainstorming".to_string()),
            "skills: {:?}",
            result.skills
        );
        assert!(
            result.skills.contains(&"test-driven-development".to_string()),
            "skills: {:?}",
            result.skills
        );
        // sorted union of trackedFileBackups keys
        assert_eq!(
            result.changed_files,
            vec!["src/auth.rs".to_string(), "src/lib.rs".to_string()]
        );
        // two human messages: "add login endpoint" and "ship it"
        assert_eq!(result.message_count, 2);
    }
}
