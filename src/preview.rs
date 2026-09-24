use crate::app::{App, PreviewResult};
use crate::parser::{extract_conversation_ext, Speaker};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

pub struct PreviewRequest {
    pub session_id: String,
    pub file_path: PathBuf,
    pub include_tools: bool,
    /// Pinned to the top of the preview when non-empty.
    pub recap: String,
}

/// Start the single background loader. Requests queue up while the user
/// scrolls through the list; the worker always skips ahead to the newest
/// one, so holding `j` over large sessions parses one file at a time instead
/// of spawning a parse per keypress.
pub fn spawn_worker(results: mpsc::Sender<PreviewResult>) -> mpsc::Sender<PreviewRequest> {
    let (tx, rx) = mpsc::channel::<PreviewRequest>();
    thread::spawn(move || {
        while let Ok(mut req) = rx.recv() {
            while let Ok(newer) = rx.try_recv() {
                req = newer;
            }
            if results.send(load(req)).is_err() {
                break;
            }
        }
    });
    tx
}

fn load(req: PreviewRequest) -> PreviewResult {
    let messages = extract_conversation_ext(&req.file_path, req.include_tools);
    let message_count = messages
        .iter()
        .filter(|(sp, _)| *sp == Speaker::User)
        .count() as u32;
    let mut lines: Vec<(String, String, Speaker)> = Vec::with_capacity(messages.len() + 1);
    if !req.recap.is_empty() {
        let lower = req.recap.to_lowercase();
        lines.push((req.recap, lower, Speaker::Recap));
    }
    lines.extend(messages.into_iter().map(|(speaker, text)| {
        let lower = text.to_lowercase();
        (text, lower, speaker)
    }));
    PreviewResult {
        session_id: req.session_id,
        lines,
        message_count,
        include_tools: req.include_tools,
    }
}

/// Request a preview for the currently selected session.
pub fn request_preview(app: &mut App) {
    let Some(session) = app.selected_session() else {
        app.preview_lines.clear();
        app.preview_session_id.clear();
        app.preview_loading = false;
        app.recompute_preview_offsets();
        return;
    };

    // Already shown or on its way. Anything that invalidates the lines (tool
    // toggle, delete) clears `preview_session_id` to force a reload.
    if app.preview_session_id == session.id {
        return;
    }

    let session_id = session.id.clone();
    let request = PreviewRequest {
        session_id: session_id.clone(),
        file_path: session.file_path.clone(),
        include_tools: app.show_tools,
        recap: session.recap.clone(),
    };

    // Loading a different session: committed search matches point into the
    // old conversation's lines.
    app.clear_preview_search();

    if let Some(cached) = app.preview_cache.get(&session_id) {
        app.preview_lines = cached.clone();
        app.preview_session_id = session_id;
        app.preview_loading = false;
        app.recompute_preview_offsets();
        return;
    }

    app.preview_loading = true;
    app.preview_session_id = session_id;
    app.preview_lines.clear();
    app.recompute_preview_offsets();
    let _ = app.preview_req_tx.send(request);
}

/// Apply completed preview loads. Returns whether anything visible changed.
pub fn check_preview_updates(app: &mut App) -> bool {
    let mut changed = false;
    while let Ok(result) = app.preview_rx.try_recv() {
        // Extracted under a tool-activity setting that has since been toggled:
        // caching or applying it would show stale lines. A fresh request was
        // already sent by the toggle handler, so just drop it.
        if result.include_tools != app.show_tools {
            continue;
        }
        app.cache_preview(result.session_id.clone(), result.lines.clone());

        if app.preview_session_id == result.session_id {
            app.preview_lines = result.lines;
            app.preview_loading = false;
            app.recompute_preview_offsets();
            changed = true;
        }

        if let Some(session) = app
            .sessions
            .iter_mut()
            .find(|s| s.id == result.session_id)
        {
            if session.message_count != result.message_count {
                session.message_count = result.message_count;
                changed = true;
            }
        }
    }
    changed
}
