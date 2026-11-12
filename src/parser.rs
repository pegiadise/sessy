use regex::Regex;
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::OnceLock;

fn ticket_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // ASCII word boundaries: Unicode `\b` sends every line with a
        // non-ASCII char to the slow engine (seconds on a big transcript).
        Regex::new(r"(?-u:\b)[A-Z][A-Z0-9]{1,9}-\d{1,7}(?-u:\b)|(?:^|[^A-Za-z0-9_])#\d{1,7}(?-u:\b)")
            .unwrap()
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
/// activity is requested; `Recap` is Claude Code's "while you were away"
/// summary, pinned to the top of the preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    User,
    Assistant,
    Tool,
    Recap,
    /// A date rule ("Tue Nov 10, 2026") where a multi-day session moves on
    /// to the next day. Not a message: never searched, counted or exported.
    Day,
}

impl Speaker {
    /// Render prefix shown before the message text.
    pub fn prefix(self) -> &'static str {
        match self {
            Speaker::User => "USER: ",
            Speaker::Assistant => "ASST: ",
            Speaker::Tool => "TOOL: ",
            Speaker::Recap => "RECAP ",
            Speaker::Day => "      ",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConversationMessage {
    pub role: Role,
    pub text: String,
}

/// A pull request Claude Code linked to the session (`type: "pr-link"`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrLink {
    pub repo: String,
    pub number: u64,
    pub url: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
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

fn flag(entry: &Value, key: &str) -> bool {
    entry.get(key).and_then(|v| v.as_bool()) == Some(true)
}

/// Text of a turn the person actually typed, or `None` for everything else
/// stored as `type:"user"`: tool results, injected/meta turns, compaction
/// summaries, messages from other sessions or background tasks, interrupt
/// markers, and command noise. Handles both content forms — a plain string,
/// and a block array (text + pasted images). A slash command typed with
/// arguments counts as a human turn ("/loop fix the tests"). A prompt typed
/// while Claude was busy is stored as a queued-command attachment instead,
/// and counts too.
pub fn human_text(entry: &Value) -> Option<String> {
    if let Some(prompt) = queued_prompt(entry) {
        return typed_text(prompt);
    }
    if entry.get("type").and_then(|t| t.as_str()) != Some("user")
        || flag(entry, "isMeta")
        || flag(entry, "isCompactSummary")
        || flag(entry, "isVisibleInTranscriptOnly")
        || entry.get("toolUseResult").is_some()
    {
        return None;
    }
    // Newer Claude Code tags every turn with its origin; only "human" is typed.
    if let Some(kind) = entry
        .get("origin")
        .and_then(|o| o.get("kind"))
        .and_then(|k| k.as_str())
    {
        if kind != "human" {
            return None;
        }
    }
    typed_text(entry.get("message")?.get("content")?)
}

/// The `prompt` of a queued-command attachment the person typed (Claude
/// Code stores prompts sent while it's busy this way, often never as a
/// `type:"user"` turn). Not task notifications or messages from other
/// sessions, which are queued the same way.
fn queued_prompt(entry: &Value) -> Option<&Value> {
    if entry.get("type").and_then(|t| t.as_str()) != Some("attachment") {
        return None;
    }
    let attachment = entry.get("attachment")?;
    if attachment.get("type").and_then(|t| t.as_str()) != Some("queued_command")
        || attachment.get("commandMode").and_then(|m| m.as_str()) != Some("prompt")
    {
        return None;
    }
    let origin = attachment
        .get("origin")
        .and_then(|o| o.get("kind"))
        .and_then(|k| k.as_str());
    if origin.is_some_and(|kind| kind != "human") {
        return None;
    }
    attachment.get("prompt")
}

/// What a person typed, from message content (a string, or text and image
/// blocks). Blocks the client injects (IDE context, reminders) are skipped,
/// so they don't become the title or hide the words after them.
fn typed_text(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(s) => s.trim().to_string(),
        Value::Array(blocks) => {
            let mut parts: Vec<&str> = Vec::new();
            let mut noise: Option<&str> = None;
            let mut has_image = false;
            for block in blocks {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                            if is_command_noise(t) || is_injected_context(t) {
                                noise.get_or_insert(t);
                            } else {
                                parts.push(t);
                            }
                        }
                    }
                    Some("image") => has_image = true,
                    Some("tool_result") => return None,
                    _ => {}
                }
            }
            let joined = parts.join("\n").trim().to_string();
            match (joined.is_empty(), has_image, noise) {
                (false, _, _) => joined,
                (true, true, _) => "[Image]".to_string(),
                (true, false, Some(noise)) => return command_invocation(noise),
                (true, false, None) => return None,
            }
        }
        _ => return None,
    };
    if text.is_empty() || text.starts_with("[Request interrupted by user") {
        return None;
    }
    if is_command_noise(&text) {
        return command_invocation(&text);
    }
    Some(text)
}

/// A block the editor integration adds to a typed turn (`<ide_opened_file>`,
/// `<ide_selection>`, …).
fn is_injected_context(text: &str) -> bool {
    text.trim_start().starts_with("<ide_")
}

/// Prompts typed while Claude was busy are logged as queued-command
/// attachments; a few also come back as a `type:"user"` turn later. This
/// counts each prompt once.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct QueuedPrompts(std::collections::HashSet<String>);

impl QueuedPrompts {
    /// Whether `text`, typed in `entry`, is a prompt not seen before.
    fn is_new(&mut self, entry: &Value, text: &str) -> bool {
        if queued_prompt(entry).is_some() {
            self.0.insert(text.to_string());
            true
        } else {
            !self.0.remove(text)
        }
    }
}

/// Machine-generated content stored as `type:"user"` turns — slash-command
/// invocations, local command output, bash-mode I/O, background-task
/// notifications. These must not drive titles, "left off", message counts,
/// search text, or preview lines (slash commands with arguments excepted, see
/// `command_invocation`).
fn is_command_noise(text: &str) -> bool {
    const NOISE: &[&str] = &[
        "<command-name>",
        "<command-message>",
        "<local-command-stdout>",
        "<local-command-stderr>",
        "<local-command-caveat>",
        "<task-notification>",
        "<system-reminder>",
        "<bash-input>",
        "<bash-stdout>",
        "<bash-stderr>",
        "<user-memory-input>",
        "<user-prompt-submit-hook>",
    ];
    let t = text.trim_start();
    NOISE.iter().any(|p| t.starts_with(p))
}

/// Content between `<tag>` and the *following* `</tag>`. Returns `None` when
/// either tag is missing or the closing tag only appears before the opening
/// one (malformed/truncated content must not panic).
fn extract_tag<'a>(content: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = content.find(&open)? + open.len();
    let end_rel = content[start..].find(&close)?;
    Some(&content[start..start + end_rel])
}

fn extract_command_args(content: &str) -> Option<&str> {
    extract_tag(content, "command-args")
}

/// "/name args" for a slash command typed with arguments — that's what the
/// person wrote. `None` for argument-less commands (/clear, /model, …).
fn command_invocation(content: &str) -> Option<String> {
    let args = extract_command_args(content)?.trim();
    if args.is_empty() {
        return None;
    }
    let name = extract_tag(content, "command-name")?.trim();
    if name.is_empty() {
        return None;
    }
    Some(format!("{} {}", name, args))
}

/// Fallback headline for sessions containing only command noise: the slash
/// command that was run, or a generic label.
fn command_noise_title(content: &str) -> String {
    match extract_tag(content, "command-name").map(str::trim) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => "(command output)".to_string(),
    }
}

/// Drop control characters (ANSI escapes in pasted logs, stray `\r`) that
/// would corrupt the terminal or shift columns. Tabs become spaces.
pub fn sanitize_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\t' => out.push_str("    "),
            // Skip CSI sequences whole (ESC [ … final byte) so no residue like
            // "[31m" is left behind.
            '\u{1b}' => {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for n in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
            }
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Collapse a message to a single display line for titles and "left off":
/// `<pasted_content …>` wrapper tags removed (their text kept), all whitespace
/// runs (including newlines) folded to one space, control characters dropped,
/// and capped at `max_chars` with an ellipsis.
pub fn one_line(text: &str, max_chars: usize) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("<pasted_content") {
        stripped.push_str(&rest[..pos]);
        stripped.push(' ');
        rest = match rest[pos..].find('>') {
            Some(end) => &rest[pos + end + 1..],
            None => "",
        };
    }
    stripped.push_str(rest);
    let stripped = stripped.replace("</pasted_content>", " ");

    let stripped = sanitize_line(&stripped.replace(['\n', '\t'], " "));

    let mut out = String::with_capacity(stripped.len().min(max_chars * 4));
    let mut count = 0;
    for word in stripped.split_whitespace() {
        if count > 0 {
            out.push(' ');
            count += 1;
        }
        for c in word.chars() {
            if count >= max_chars {
                let kept = out.trim_end().len();
                out.truncate(kept);
                out.push('…');
                return out;
            }
            out.push(c);
            count += 1;
        }
    }
    out
}

fn push_search_text(out: &mut String, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    let folded = crate::fold::fold(trimmed);
    // The byte that marks where a subagent's text starts never occurs in
    // the text itself.
    if folded.as_bytes().contains(&crate::subagents::MARK) {
        out.push_str(&folded.replace(crate::subagents::MARK as char, " "));
    } else {
        out.push_str(&folded);
    }
    out.push('\n');
}

/// One entry's searchable text (see `index_entry_text`), without tickets.
pub fn append_search_text(entry: &Value, out: &mut String) {
    for_each_search_piece(entry, &mut |piece| push_search_text(out, piece));
}

/// Accumulate every piece of human-readable text in an entry into the search
/// text: user messages (string or block form), assistant text and thinking,
/// tool-use string inputs (commands, paths, …), and tool-result output.
/// Command noise is filtered (a slash command's arguments are kept); images
/// and JSON structure are not indexed. Ticket IDs are read from the
/// text as written (not the raw JSON line, where "\nPROJ-1" reads as
/// "nPROJ-1" and the system prompt adds noise) and from the branch name.
fn index_entry_text(
    entry: &Value,
    out: &mut String,
    tickets: &mut std::collections::HashSet<String>,
) {
    for_each_search_piece(entry, &mut |piece| {
        extract_tickets_into(piece, tickets);
        push_search_text(out, piece);
    });
}

/// The pieces of an entry's text the search text is made of, as written:
/// each is trimmed, lowercased and ended with a newline there.
pub fn for_each_search_piece(entry: &Value, f: &mut dyn FnMut(&str)) {
    if let Some(prompt) = queued_prompt(entry) {
        if let Some(text) = typed_text(prompt) {
            f(&text);
        }
        return;
    }
    // Only the conversation: not the system prompt, tool listings and
    // CLAUDE.md that Claude Code also logs (`api-request-blob`), which would
    // make every session match words like "docx".
    if !matches!(
        entry.get("type").and_then(|t| t.as_str()),
        Some("user" | "assistant")
    ) {
        return;
    }
    let Some(content) = entry.get("message").and_then(|m| m.get("content")) else {
        return;
    };
    match content {
        Value::String(s) => {
            if !is_command_noise(s) {
                f(s);
            } else if let Some(cmd) = command_invocation(s) {
                f(&cmd);
            }
        }
        Value::Array(blocks) => {
            for block in blocks {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                            if !is_command_noise(t) {
                                f(t);
                            }
                        }
                    }
                    Some("thinking") => {
                        if let Some(t) = block.get("thinking").and_then(|t| t.as_str()) {
                            f(t);
                        }
                    }
                    Some("tool_use") => {
                        if let Some(input) = block.get("input").and_then(|i| i.as_object()) {
                            for value in input.values() {
                                if let Some(s) = value.as_str() {
                                    f(s);
                                }
                            }
                        }
                    }
                    Some("tool_result") => match block.get("content") {
                        Some(Value::String(s)) => f(s),
                        Some(Value::Array(parts)) => {
                            for part in parts {
                                if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                                    f(t);
                                }
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// Claude Code's "while you were away" recap text, without the settings hint
/// it appends.
fn recap_text(entry: &Value) -> Option<String> {
    if entry.get("type").and_then(|t| t.as_str()) != Some("system")
        || entry.get("subtype").and_then(|s| s.as_str()) != Some("away_summary")
    {
        return None;
    }
    let content = entry.get("content").and_then(|c| c.as_str())?.trim();
    let content = content
        .strip_suffix("(disable recaps in /config)")
        .unwrap_or(content)
        .trim();
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}

pub struct ScanResult {
    pub head: HeadMeta,
    pub tail: Option<TailMeta>,
    /// Lowercased searchable text: user messages, assistant text + thinking,
    /// tool-use string inputs, and tool-result text — everything a person
    /// could have read in the session, minus JSON structure and images.
    pub search_text_lc: String,
    pub tickets: Vec<String>,
    /// Claude-generated session title (`type: "ai-title"`), if present.
    pub ai_title: String,
    /// Last `permissionMode` seen (e.g. "plan", "acceptEdits", "bypassPermissions").
    pub permission_mode: String,
    /// Claude Code `version` field (first non-empty seen).
    pub cc_version: String,
    /// Distinct `attributionSkill` values, sorted.
    pub skills: Vec<String>,
    /// Sorted union of tracked file paths across file-history snapshots and deltas.
    pub changed_files: Vec<String>,
    /// Count of human messages outside sidechains.
    pub message_count: u32,
    /// Linked pull requests, in the order they were first linked.
    pub prs: Vec<PrLink>,
    /// Latest "while you were away" recap, if any.
    pub recap: String,
}

/// Reads a transcript a line at a time, keeping what the index needs.
/// Serializable, so a scan can carry on from where it stopped when the
/// transcript has only grown (`index` keeps it between runs).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Scanner {
    head: Option<HeadMeta>,
    working_head: HeadMeta,
    last_human_message: String,
    last_timestamp: String,
    custom_title: String,
    legacy_rename: String,
    fallback_title: String,
    tickets_set: std::collections::HashSet<String>,
    ai_title: String,
    permission_mode: String,
    cc_version: String,
    skills_set: std::collections::HashSet<String>,
    changed_files_set: std::collections::HashSet<String>,
    message_count: u32,
    prs: Vec<PrLink>,
    recap: String,
    last_branch: String,
    /// Prompts queued while Claude was busy, not yet seen again as a turn.
    queued: QueuedPrompts,
}

impl Scanner {
    /// Take in one line of the transcript; its searchable text goes on the
    /// end of `search_text_lc`.
    pub fn feed(&mut self, line: &str, search_text_lc: &mut String) {
        if line.trim().is_empty() {
            return;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            return;
        };

        index_entry_text(&entry, search_text_lc, &mut self.tickets_set);
        if let Some(b) = entry.get("gitBranch").and_then(|b| b.as_str()) {
            if b != self.last_branch {
                extract_tickets_into(b, &mut self.tickets_set);
                self.last_branch = b.to_string();
            }
        }

        // Modern-format metadata, harvested in the same single pass.
        match entry.get("type").and_then(|t| t.as_str()) {
            Some("ai-title") => {
                if let Some(t) = entry.get("aiTitle").and_then(|t| t.as_str()) {
                    self.ai_title = one_line(t, 200);
                }
            }
            // `/rename` (and Claude Code's own naming) persists the title here.
            Some("custom-title") => {
                if let Some(t) = entry.get("customTitle").and_then(|t| t.as_str()) {
                    self.custom_title = one_line(t, 200);
                }
            }
            Some("permission-mode") => {
                if let Some(m) = entry.get("permissionMode").and_then(|m| m.as_str()) {
                    self.permission_mode = m.to_string();
                }
            }
            Some("file-history-snapshot") => {
                if let Some(backups) = entry
                    .get("snapshot")
                    .and_then(|s| s.get("trackedFileBackups"))
                    .and_then(|b| b.as_object())
                {
                    for path in backups.keys() {
                        self.changed_files_set.insert(path.clone());
                    }
                }
            }
            // Newer Claude Code records edits one file at a time.
            Some("file-history-delta") => {
                if let Some(p) = entry.get("trackingPath").and_then(|p| p.as_str()) {
                    self.changed_files_set.insert(p.to_string());
                }
            }
            Some("pr-link") => {
                let number = entry.get("prNumber").and_then(|n| n.as_u64());
                let url = entry.get("prUrl").and_then(|u| u.as_str()).unwrap_or("");
                if let Some(number) = number {
                    if !self.prs.iter().any(|p| p.number == number && p.url == url) {
                        self.prs.push(PrLink {
                            repo: entry
                                .get("prRepository")
                                .and_then(|r| r.as_str())
                                .unwrap_or("")
                                .to_string(),
                            number,
                            url: url.to_string(),
                        });
                    }
                    // `#123` finds the session through ticket search.
                    self.tickets_set.insert(format!("#{}", number));
                }
            }
            _ => {}
        }
        if let Some(r) = recap_text(&entry) {
            self.recap = one_line(&r, 1000);
        }
        if self.cc_version.is_empty() {
            if let Some(v) = entry.get("version").and_then(|v| v.as_str()) {
                self.cc_version = v.to_string();
            }
        }
        if let Some(skill) = entry.get("attributionSkill").and_then(|s| s.as_str()) {
            if !skill.is_empty() {
                self.skills_set.insert(skill.to_string());
            }
        }

        if self.working_head.branch.is_empty() {
            if let Some(b) = entry.get("gitBranch").and_then(|b| b.as_str()) {
                self.working_head.branch = b.to_string();
            }
        }
        if self.working_head.slug.is_empty() {
            if let Some(s) = entry.get("slug").and_then(|s| s.as_str()) {
                self.working_head.slug = s.to_string();
            }
        }
        if self.working_head.first_timestamp.is_empty() {
            if let Some(ts) = entry.get("timestamp").and_then(|t| t.as_str()) {
                self.working_head.first_timestamp = ts.to_string();
            }
        }
        if self.working_head.cwd.is_empty() {
            if let Some(c) = entry.get("cwd").and_then(|c| c.as_str()) {
                self.working_head.cwd = c.to_string();
            }
        }

        if let Some(ts) = entry.get("timestamp").and_then(|t| t.as_str()) {
            self.last_timestamp = ts.to_string();
        }

        // Remember the first command-noise turn so command-only sessions
        // (someone opened Claude and ran /exit) still get a listable title.
        if self.fallback_title.is_empty()
            && entry.get("type").and_then(|t| t.as_str()) == Some("user")
        {
            if let Some(c) = entry
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
            {
                if is_command_noise(c) {
                    self.fallback_title = command_noise_title(c);
                }
            }
        }

        // Sidechain turns belong to subagents, not the person; the preview
        // skips them, so titles and counts must too.
        if !flag(&entry, "isSidechain") {
            if let Some(text) = human_text(&entry).filter(|t| self.queued.is_new(&entry, t)) {
                let line = one_line(&text, 200);
                self.message_count += 1;
                if self.head.is_none() {
                    self.head = Some(HeadMeta {
                        title: line.clone(),
                        branch: self.working_head.branch.clone(),
                        slug: self.working_head.slug.clone(),
                        first_timestamp: self.working_head.first_timestamp.clone(),
                        cwd: self.working_head.cwd.clone(),
                    });
                }
                self.last_human_message = line;
            }
        }

        // Pre-`custom-title` Claude Code only recorded /rename as a local
        // command. A bare `/rename` (auto-generated name) has empty args and
        // must not clear a name set earlier.
        if entry.get("subtype").and_then(|s| s.as_str()) == Some("local_command") {
            if let Some(content) = entry.get("content").and_then(|c| c.as_str()) {
                if content.contains("<command-name>/rename</command-name>") {
                    if let Some(args) = extract_command_args(content) {
                        let args = one_line(args, 200);
                        if !args.is_empty() {
                            self.legacy_rename = args;
                        }
                    }
                }
            }
        }
    }

    /// What the scan found so far, `None` when nothing listable (no prompt,
    /// not even a command). The search text isn't part of it.
    pub fn result(&self) -> Option<ScanResult> {
        let rename = if self.custom_title.is_empty() {
            self.legacy_rename.clone()
        } else {
            self.custom_title.clone()
        };
        let mut head = match &self.head {
            Some(h) => h.clone(),
            None if !self.fallback_title.is_empty() => HeadMeta {
                title: self.fallback_title.clone(),
                ..self.working_head.clone()
            },
            None => return None,
        };
        // The slug is first written after the first prompt, so the head taken
        // there rarely has it.
        if head.slug.is_empty() {
            head.slug = self.working_head.slug.clone();
        }
        let tail = if self.last_human_message.is_empty()
            && self.last_timestamp.is_empty()
            && rename.is_empty()
        {
            None
        } else {
            Some(TailMeta {
                last_human_message: self.last_human_message.clone(),
                last_timestamp: self.last_timestamp.clone(),
                rename,
            })
        };
        let sorted = |set: &std::collections::HashSet<String>| {
            let mut v: Vec<String> = set.iter().cloned().collect();
            v.sort();
            v
        };
        Some(ScanResult {
            head,
            tail,
            search_text_lc: String::new(),
            tickets: sorted(&self.tickets_set),
            ai_title: self.ai_title.clone(),
            permission_mode: self.permission_mode.clone(),
            cc_version: self.cc_version.clone(),
            skills: sorted(&self.skills_set),
            changed_files: sorted(&self.changed_files_set),
            message_count: self.message_count,
            prs: self.prs.clone(),
            recap: self.recap.clone(),
        })
    }
}

/// A transcript read from `from` (a line start) to its last complete line.
/// A last line still being written (no newline yet) is fed too, so the
/// result is current, but not counted in the returned scanner and offset:
/// carrying on from there reads it again, whole.
pub struct ScanPart {
    /// The scan up to `scanned`, to carry on from later.
    pub resumable: Scanner,
    /// Byte offset just past the last complete line.
    pub scanned: u64,
    /// Search text of the complete lines, then of the unfinished one.
    pub text: String,
    pub text_complete: usize,
    /// The scan including the unfinished line.
    pub current: Scanner,
}

pub fn scan_from(path: &Path, mut scanner: Scanner, from: u64) -> Option<ScanPart> {
    use std::io::{BufRead, Seek, SeekFrom};
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut reader = BufReader::new(file);
    let mut text = String::new();
    let mut scanned = from;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf).ok()?;
        if n == 0 {
            break;
        }
        if buf.last() != Some(&b'\n') {
            // Unfinished: count it in the current result only.
            let text_complete = text.len();
            let resumable = scanner.clone();
            if let Ok(line) = std::str::from_utf8(&buf) {
                scanner.feed(line, &mut text);
            }
            return Some(ScanPart { resumable, scanned, text, text_complete, current: scanner });
        }
        scanned += n as u64;
        // Lines that aren't UTF-8 can't be JSON; skip them as before.
        if let Ok(line) = std::str::from_utf8(&buf) {
            scanner.feed(line.trim_end_matches(['\n', '\r']), &mut text);
        }
    }
    let text_complete = text.len();
    Some(ScanPart { resumable: scanner.clone(), scanned, text, text_complete, current: scanner })
}

pub fn scan_session(path: &Path) -> Option<ScanResult> {
    let part = scan_from(path, Scanner::default(), 0)?;
    let mut result = part.current.result()?;
    result.search_text_lc = part.text;
    Some(result)
}

/// User + assistant text messages, with tool use filtered out. Used by export.
pub fn extract_conversation(path: &Path) -> Vec<ConversationMessage> {
    extract_conversation_ext(path, false)
        .into_iter()
        .filter_map(|(speaker, text)| {
            let role = match speaker {
                Speaker::User => Role::User,
                Speaker::Assistant => Role::Assistant,
                Speaker::Tool | Speaker::Recap | Speaker::Day => return None,
            };
            Some(ConversationMessage { role, text })
        })
        .collect()
}

/// Extract the conversation as `(speaker, text)` pairs. When `include_tools` is
/// true, assistant `tool_use` blocks are surfaced as `Speaker::Tool` lines with
/// a compact summary (e.g. "Edit src/auth.rs", "Bash cargo test").
pub fn extract_conversation_ext(path: &Path, include_tools: bool) -> Vec<(Speaker, String)> {
    let mut tail = ConversationTail::new(include_tools);
    read_conversation(path, &mut tail).lines
}

/// How far a conversation has been read, to read on when its transcript
/// grows (a session open in Claude Code), with what that needs to carry
/// on as one whole read would.
#[derive(Debug, Clone)]
pub struct ConversationTail {
    /// Bytes read: whole lines only.
    pub scanned: u64,
    pub include_tools: bool,
    queued: QueuedPrompts,
    /// Lines given out so far (day rules included).
    emitted: usize,
    /// The first dated message's day and line, where its day rule goes
    /// once a second day comes.
    first_day: Option<(chrono::NaiveDate, usize)>,
    last_day: Option<chrono::NaiveDate>,
    /// Day rules are in: more than one day so far.
    ruled: bool,
}

impl ConversationTail {
    pub fn new(include_tools: bool) -> Self {
        Self {
            scanned: 0,
            include_tools,
            queued: QueuedPrompts::default(),
            emitted: 0,
            first_day: None,
            last_day: None,
            ruled: false,
        }
    }
}

/// What reading on found.
pub struct MoreConversation {
    pub lines: Vec<(Speaker, String)>,
    /// The conversation came to span a second day just now: the first
    /// day's rule, to go in at this line of what was read before.
    pub first_rule: Option<(usize, String)>,
}

/// Read the conversation on from `tail` (as `(speaker, text)` lines, see
/// `extract_conversation_ext`), up to its last whole line, and move `tail`
/// past it. Sidechains are skipped; a day rule goes before the first
/// message of each day once there are two.
pub fn read_conversation(path: &Path, tail: &mut ConversationTail) -> MoreConversation {
    use std::io::{Seek, SeekFrom};
    let mut more = MoreConversation { lines: Vec::new(), first_rule: None };
    let Ok(mut file) = File::open(path) else {
        return more;
    };
    if file.seek(SeekFrom::Start(tail.scanned)).is_err() {
        return more;
    }
    let start = tail.emitted;
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    let mut messages: Vec<(Speaker, String)> = Vec::new();
    loop {
        buf.clear();
        let n = match reader.read_until(b'\n', &mut buf) {
            Ok(n) if n > 0 => n,
            _ => break,
        };
        let entry = std::str::from_utf8(&buf)
            .ok()
            .and_then(|line| serde_json::from_str::<Value>(line.trim_end()).ok());
        // A last line with no newline yet is still being written, unless it's
        // whole JSON already (a file that just doesn't end in a newline).
        if buf.last() != Some(&b'\n') && entry.is_none() {
            break;
        }
        tail.scanned += n as u64;
        let Some(entry) = entry else {
            continue;
        };
        if flag(&entry, "isSidechain") {
            continue;
        }
        messages.clear();
        conversation_lines(&entry, tail, &mut messages);
        if messages.is_empty() {
            continue;
        }
        if let Some(day) = entry_day(&entry) {
            match tail.last_day {
                None => tail.first_day = Some((day, tail.emitted)),
                Some(prev) if prev != day => {
                    if !tail.ruled {
                        tail.ruled = true;
                        if let Some((first, at)) = tail.first_day {
                            let rule = (at, day_rule(first));
                            // In this read's lines, or in what came before.
                            if at >= start {
                                more.lines.insert(at - start, (Speaker::Day, rule.1));
                            } else {
                                more.first_rule = Some(rule);
                            }
                            tail.emitted += 1;
                        }
                    }
                    more.lines.push((Speaker::Day, day_rule(day)));
                    tail.emitted += 1;
                }
                Some(_) => {}
            }
            tail.last_day = Some(day);
        }
        tail.emitted += messages.len();
        more.lines.append(&mut messages);
    }
    more
}

fn day_rule(day: chrono::NaiveDate) -> String {
    day.format("%a %b %-d, %Y").to_string()
}

/// The preview lines of one transcript entry.
fn conversation_lines(entry: &Value, tail: &mut ConversationTail, out: &mut Vec<(Speaker, String)>) {
    match entry.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "user" | "attachment" => {
            if let Some(text) = human_text(entry).filter(|t| tail.queued.is_new(entry, t)) {
                out.push((Speaker::User, text));
            }
        }
        "assistant" => {
            let Some(content) = entry
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            else {
                return;
            };
            for block in content {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                out.push((Speaker::Assistant, trimmed.to_string()));
                            }
                        }
                    }
                    Some("tool_use") if tail.include_tools => {
                        let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                        out.push((Speaker::Tool, summarize_tool_use(name, block.get("input"))));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// Local day an entry was written, from its `timestamp`.
fn entry_day(entry: &Value) -> Option<chrono::NaiveDate> {
    let ts = entry.get("timestamp")?.as_str()?;
    let dt = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    Some(dt.with_timezone(&chrono::Local).date_naive())
}

/// Build a one-line summary of a tool call: the tool name plus its most telling
/// argument (file path, command, pattern, …), truncated.
fn summarize_tool_use(name: &str, input: Option<&Value>) -> String {
    let detail = input
        .and_then(|inp| {
            [
                "file_path",
                "path",
                "command",
                "pattern",
                "query",
                "url",
                "skill",
                "description",
                "prompt",
            ]
            .iter()
            .find_map(|k| inp.get(*k).and_then(|v| v.as_str()))
        })
        .unwrap_or("");
    let detail = one_line(detail, 120);
    if detail.is_empty() {
        name.to_string()
    } else {
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
    fn test_reading_on_gives_what_one_whole_read_does() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let line = |ts: &str, text: &str| {
            serde_json::json!({"type": "user", "timestamp": ts, "message": {"content": text}})
                .to_string()
                + "\n"
        };
        let add = |text: &str| {
            std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(text.as_bytes()).unwrap();
        };
        std::fs::write(&path, line("2026-11-09T12:00:00Z", "first") + &line("2026-11-09T12:30:00Z", "same day")).unwrap();
        let mut tail = ConversationTail::new(false);
        let mut shown = read_conversation(&path, &mut tail).lines;
        assert_eq!(shown.len(), 2, "one day: no rules");

        // A line half written: nothing yet.
        let later = line("2026-11-11T12:00:00Z", "two days later");
        let (head, rest) = later.split_at(20);
        add(head);
        let before = tail.scanned;
        assert!(read_conversation(&path, &mut tail).lines.is_empty());
        assert_eq!(tail.scanned, before, "read again once whole");

        // Whole, on another day: its rule, and the first day's for before.
        add(rest);
        let more = read_conversation(&path, &mut tail);
        if let Some((at, rule)) = more.first_rule {
            shown.insert(at, (Speaker::Day, rule));
        }
        shown.extend(more.lines);
        assert_eq!(shown, extract_conversation_ext(&path, false));
        let speakers: Vec<Speaker> = shown.iter().map(|(s, _)| *s).collect();
        assert_eq!(speakers, [Speaker::Day, Speaker::User, Speaker::User, Speaker::Day, Speaker::User]);
    }

    #[test]
    fn test_multi_day_sessions_get_day_rules() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let line = |ts: &str, text: &str| {
            serde_json::json!({"type": "user", "timestamp": ts, "message": {"content": text}})
                .to_string()
        };
        // Midday UTC keeps the local day the same in any timezone.
        let body = [
            line("2026-11-09T12:00:00Z", "first"),
            line("2026-11-09T12:30:00Z", "same day"),
            line("2026-11-11T12:00:00Z", "two days later"),
        ]
        .join("\n");
        std::fs::write(&path, body).unwrap();
        let got = extract_conversation_ext(&path, false);
        let speakers: Vec<Speaker> = got.iter().map(|(s, _)| *s).collect();
        assert_eq!(
            speakers,
            vec![Speaker::Day, Speaker::User, Speaker::User, Speaker::Day, Speaker::User]
        );
        assert_eq!(got[0].1, "Mon Nov 9, 2026");
        assert_eq!(got[3].1, "Wed Nov 11, 2026");
        // Export leaves the rules out.
        assert_eq!(extract_conversation(&path).len(), 3);

        // One day: no rules at all.
        std::fs::write(&path, line("2026-11-09T12:00:00Z", "only")).unwrap();
        assert_eq!(extract_conversation_ext(&path, false).len(), 1);
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
            result.search_text_lc.contains("build a cool thing"),
            "got: {:?}",
            result.search_text_lc
        );
        assert!(
            result.search_text_lc.contains("looks good, ship it"),
            "got: {:?}",
            result.search_text_lc
        );
        assert!(
            result.search_text_lc.chars().all(|c: char| !c.is_uppercase()),
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
    fn test_extract_command_args_well_formed() {
        assert_eq!(
            extract_command_args("<command-args>new name</command-args>"),
            Some("new name")
        );
    }

    #[test]
    fn test_extract_command_args_close_before_open_no_panic() {
        // A closing tag before the opening tag used to slice with start > end.
        let content = "<command-name>/rename</command-name></command-args>junk<command-args>";
        assert_eq!(extract_command_args(content), None);
    }

    #[test]
    fn test_extract_command_args_missing_close() {
        assert_eq!(extract_command_args("<command-args>truncated"), None);
    }

    #[test]
    fn test_scan_session_malformed_rename_line_no_panic() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.jsonl");
        let mut f = std::fs::File::create(&path).expect("create");
        // Valid human message so the scan yields a result…
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"hello"}},"timestamp":"2026-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        // …then a local_command whose </command-args> precedes <command-args>.
        writeln!(
            f,
            r#"{{"type":"user","subtype":"local_command","content":"<command-name>/rename</command-name></command-args>junk<command-args>","toolUseResult":{{}}}}"#
        )
        .unwrap();
        let result = scan_session(&path).expect("should still scan");
        assert_eq!(result.head.title, "hello");
        let tail = result.tail.expect("tail");
        assert_eq!(tail.rename, "", "malformed rename must be ignored");
    }

    fn transcript(dir: &tempfile::TempDir, lines: &[serde_json::Value]) -> std::path::PathBuf {
        use std::io::Write;
        let path = dir.path().join("s.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{}", line).unwrap();
        }
        path
    }

    fn queued(prompt: serde_json::Value, origin: &str, mode: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "attachment", "timestamp": "2026-11-10T10:05:00Z",
            "attachment": {"type": "queued_command", "prompt": prompt, "commandMode": mode,
                           "origin": {"kind": origin}}
        })
    }

    #[test]
    fn test_prompts_typed_while_claude_was_busy_count_and_show() {
        let dir = tempfile::tempdir().unwrap();
        let path = transcript(&dir, &[
            serde_json::json!({"type": "user", "timestamp": "2026-11-10T10:00:00Z",
                               "message": {"role": "user", "content": "fix the panic"}}),
            serde_json::json!({"type": "assistant", "timestamp": "2026-11-10T10:01:00Z",
                               "message": {"role": "assistant", "content": [{"type": "text", "text": "Looking."}]}}),
            queued(serde_json::json!("also check the Logger"), "human", "prompt"),
            queued(serde_json::json!([{"type": "text", "text": "and the tests"}]), "human", "prompt"),
            queued(serde_json::json!("<task-notification>done</task-notification>"), "task-notification", "task-notification"),
            queued(serde_json::json!("hi from another session"), "peer", "prompt"),
            // Delivered again as a turn later: still one prompt.
            serde_json::json!({"type": "user", "timestamp": "2026-11-10T10:06:00Z",
                               "message": {"role": "user", "content": "and the tests"}}),
        ]);
        let scan = scan_session(&path).unwrap();
        assert_eq!(scan.message_count, 3, "typed: the first turn and two queued prompts");
        assert_eq!(scan.tail.unwrap().last_human_message, "and the tests");
        assert!(scan.search_text_lc.contains("also check the logger"));
        assert!(!scan.search_text_lc.contains("another session"));

        let users: Vec<String> = extract_conversation_ext(&path, false)
            .into_iter()
            .filter(|(sp, _)| *sp == Speaker::User)
            .map(|(_, t)| t)
            .collect();
        assert_eq!(users, ["fix the panic", "also check the Logger", "and the tests"]);
    }

    #[test]
    fn test_logged_system_prompts_are_not_searched() {
        let dir = tempfile::tempdir().unwrap();
        let path = transcript(&dir, &[
            serde_json::json!({"type": "api-request-blob", "hash": "x",
                               "message": {"role": "system", "content": [{"type": "text", "text": "skills: docx, PROJ-77"}]}}),
            serde_json::json!({"type": "user", "message": {"role": "user", "content": "hello"}}),
        ]);
        let scan = scan_session(&path).unwrap();
        assert!(!scan.search_text_lc.contains("docx"));
        assert!(scan.tickets.is_empty(), "nor its ticket-like words");
    }

    #[test]
    fn test_editor_context_blocks_do_not_become_the_title() {
        let dir = tempfile::tempdir().unwrap();
        let path = transcript(&dir, &[serde_json::json!({"type": "user", "message": {"role": "user", "content": [
            {"type": "text", "text": "<ide_opened_file>The user opened src/main.rs</ide_opened_file>"},
            {"type": "text", "text": "<system-reminder>be brief</system-reminder>"},
            {"type": "text", "text": "fix the panic"},
        ]}})]);
        let scan = scan_session(&path).unwrap();
        assert_eq!(scan.head.title, "fix the panic");
        assert_eq!(scan.message_count, 1);
    }

    #[test]
    fn test_tickets_come_from_the_text_and_the_branch() {
        let dir = tempfile::tempdir().unwrap();
        let path = transcript(&dir, &[
            serde_json::json!({"type": "user", "gitBranch": "feat/WEB-9-login",
                               "message": {"role": "user", "content": "see:\nPROJ-123 and\n#42"}}),
        ]);
        let tickets = scan_session(&path).unwrap().tickets;
        assert_eq!(tickets, ["#42", "PROJ-123", "WEB-9"]);
    }

    #[test]
    fn test_slug_written_after_the_first_prompt_still_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = transcript(&dir, &[
            serde_json::json!({"type": "user", "message": {"role": "user", "content": "hello"}}),
            serde_json::json!({"type": "assistant", "slug": "brave-otter",
                               "message": {"role": "assistant", "content": [{"type": "text", "text": "hi"}]}}),
        ]);
        assert_eq!(scan_session(&path).unwrap().head.slug, "brave-otter");
    }

    #[test]
    fn test_scan_session_non_utf8_and_truncated_lines_no_panic() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("garbage.jsonl");
        let mut f = std::fs::File::create(&path).expect("create");
        // Invalid UTF-8 bytes, truncated JSON, unknown types with odd shapes.
        f.write_all(&[0xff, 0xfe, 0x80, b'\n']).unwrap();
        f.write_all(b"{\"type\":\"user\",\"message\":{\"content\":\"tr\n").unwrap();
        f.write_all(b"{\"type\":\"future-thing\",\"message\":42}\n").unwrap();
        f.write_all(b"{\"type\":\"user\",\"message\":{\"content\":[\"array form\"]}}\n").unwrap();
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"survivor"}},"timestamp":"2026-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        let result = scan_session(&path).expect("should scan despite garbage");
        assert_eq!(result.head.title, "survivor");
        assert_eq!(result.message_count, 1);
    }

    #[test]
    fn test_command_noise_skipped_for_title_and_count() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("noisy.jsonl");
        let mut f = std::fs::File::create(&path).expect("create");
        // Slash command + its output arrive before the real first message.
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"<command-name>/model</command-name><command-args></command-args>"}},"timestamp":"2026-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"<local-command-stdout>Set model to opus</local-command-stdout>"}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"fix the login bug"}},"timestamp":"2026-01-01T00:01:00Z"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"<task-notification><task-id>x1</task-id>done</task-notification>"}},"timestamp":"2026-01-01T00:02:00Z"}}"#
        )
        .unwrap();
        let result = scan_session(&path).expect("should scan");
        assert_eq!(result.head.title, "fix the login bug");
        assert_eq!(result.message_count, 1, "noise turns must not count");
        let tail = result.tail.expect("tail");
        assert_eq!(
            tail.last_human_message, "fix the login bug",
            "task notification must not become the left-off line"
        );
        assert!(
            !result.search_text_lc.contains("command-name"),
            "noise must not enter the search text"
        );
    }

    #[test]
    fn test_command_only_session_gets_fallback_title() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cmd_only.jsonl");
        let mut f = std::fs::File::create(&path).expect("create");
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"<command-name>/exit</command-name>"}},"timestamp":"2026-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"<local-command-stdout>Bye!</local-command-stdout>"}}}}"#
        )
        .unwrap();
        let result = scan_session(&path).expect("command-only session must stay listable");
        assert_eq!(result.head.title, "/exit");
        assert_eq!(result.message_count, 0);
    }

    #[test]
    fn test_search_text_includes_assistant_text() {
        let result = scan_session(&fixture_path("complex_session.jsonl")).expect("should scan");
        assert!(
            result.search_text_lc.contains("i'll set up jwt auth."),
            "assistant text must be searchable, got: {:?}",
            result.search_text_lc
        );
    }

    #[test]
    fn test_search_text_includes_array_form_user_text() {
        let result = scan_session(&fixture_path("complex_session.jsonl")).expect("should scan");
        assert!(
            result.search_text_lc.contains("skill loaded: auth-helper"),
            "array-form user text must be searchable, got: {:?}",
            result.search_text_lc
        );
    }

    #[test]
    fn test_search_text_includes_tool_results_inputs_and_thinking() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("deep.jsonl");
        let mut f = std::fs::File::create(&path).expect("create");
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"start"}},"timestamp":"2026-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        // Assistant turn: thinking + tool_use with string inputs.
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"content":[{{"type":"thinking","thinking":"maybe the flag is inverted"}},{{"type":"tool_use","id":"t1","name":"Bash","input":{{"command":"cargo build --release"}}}}]}}}}"#
        )
        .unwrap();
        // Tool result in string form…
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"t1","content":"error[E0502]: cannot borrow"}}]}},"toolUseResult":{{}}}}"#
        )
        .unwrap();
        // …and in block-array form.
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"t2","content":[{{"type":"text","text":"warning: unused variable `zeta`"}}]}}]}},"toolUseResult":{{}}}}"#
        )
        .unwrap();
        let result = scan_session(&path).expect("should scan");
        for needle in [
            "maybe the flag is inverted",
            "cargo build --release",
            "error[e0502]: cannot borrow",
            "warning: unused variable `zeta`",
        ] {
            assert!(
                result.search_text_lc.contains(needle),
                "search text must contain {:?}, got: {:?}",
                needle,
                result.search_text_lc
            );
        }
        // Deep content must not affect the human message count.
        assert_eq!(result.message_count, 1);
    }

    #[test]
    fn test_extract_conversation_missing_file_returns_empty() {
        let messages = extract_conversation(Path::new("/does/not/exist.jsonl"));
        assert!(messages.is_empty());
    }

    #[test]
    fn test_scan_current_format() {
        let r = scan_session(&fixture_path("current_format_session.jsonl")).expect("should scan");
        // A slash command typed with arguments is what the person wrote.
        assert_eq!(r.head.title, "/loop fix the flaky tests");
        // /loop, the image message, the pasted message. Not: the meta
        // expansion, the interrupt marker, the compaction summary, the task
        // notification, or the message from another session.
        assert_eq!(r.message_count, 3);
        let tail = r.tail.expect("tail");
        assert_eq!(
            tail.last_human_message, "FAILED spec/cart_spec.rb:12 this one too",
            "pasted_content tags stripped, newlines folded"
        );
        // Latest custom-title wins; a bare /rename afterwards doesn't clear it.
        assert_eq!(tail.rename, "auth-rework");
        assert_eq!(r.ai_title, "Fix flaky specs");
        assert!(r.changed_files.contains(&"spec/auth_spec.rb".to_string()));
        assert_eq!(
            r.prs,
            vec![PrLink {
                repo: "acme/shop".into(),
                number: 42,
                url: "https://github.com/acme/shop/pull/42".into(),
            }]
        );
        assert!(r.tickets.contains(&"#42".to_string()), "PR number is ticket-searchable");
        assert_eq!(
            r.recap,
            "Both flaky specs are fixed and PR #42 is open. Next: merge after CI."
        );
        assert!(r.search_text_lc.contains("/loop fix the flaky tests"));
    }

    #[test]
    fn test_extract_current_format_conversation() {
        let lines = extract_conversation_ext(&fixture_path("current_format_session.jsonl"), false);
        let users: Vec<&str> = lines
            .iter()
            .filter(|(sp, _)| *sp == Speaker::User)
            .map(|(_, t)| t.as_str())
            .collect();
        assert_eq!(users.len(), 3, "got {:?}", users);
        assert_eq!(users[0], "/loop fix the flaky tests");
        assert_eq!(users[1], "why is this red? [Image #1]");
        assert!(!users.iter().any(|u| u.contains("being continued")));
        assert!(!users.iter().any(|u| u.contains("another session")));
        // Multi-line assistant text keeps its line breaks for the preview.
        assert!(lines.iter().any(|(sp, t)| *sp == Speaker::Assistant && t.contains("\n\n1. `auth_spec`")));
    }

    #[test]
    fn test_image_only_message_is_a_turn() {
        let entry: Value = serde_json::from_str(
            r#"{"type":"user","message":{"content":[{"type":"image","source":{}}]}}"#,
        )
        .unwrap();
        assert_eq!(human_text(&entry).as_deref(), Some("[Image]"));
    }

    #[test]
    fn test_sanitize_strips_ansi_and_controls() {
        assert_eq!(sanitize_line("\u{1b}[31mred\u{1b}[0m\r"), "red");
        assert_eq!(sanitize_line("a\tb"), "a    b");
        assert_eq!(sanitize_line("bell\u{7}"), "bell");
    }

    #[test]
    fn test_one_line_cleanup() {
        assert_eq!(one_line("a\n\n  b\tc", 200), "a b c");
        assert_eq!(one_line("abc def", 4), "abc…");
        assert_eq!(one_line("x\u{1b}[31my\u{1b}[0m", 200), "xy");
        assert_eq!(one_line("<pasted_content id=\"1\">log</pasted_content> fix", 200), "log fix");
        assert_eq!(one_line("<pasted_content id=\"1\" truncated", 200), "");
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
