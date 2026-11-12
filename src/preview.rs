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
            // A transcript that trips a parser bug shows as empty rather than
            // leaving the preview loading forever.
            let empty = PreviewResult {
                session_id: req.session_id.clone(),
                lines: Vec::new(),
                message_count: 0,
                include_tools: req.include_tools,
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load(req)))
                .unwrap_or(empty);
            if results.send(result).is_err() {
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
        let lower = crate::fold::fold(&req.recap);
        lines.push((req.recap, lower, Speaker::Recap));
    }
    lines.extend(messages.into_iter().map(|(speaker, text)| {
        let lower = crate::fold::fold(&text);
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
        app.preview_lines = Default::default();
        app.preview_search_matches.clear();
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

    // Loading a different session: a find-in-conversation query (and its
    // match indices and scroll position) belonged to the old conversation.
    // A reload of the same one (`T`) keeps them.
    if app.preview_keep.as_ref().is_none_or(|k| k.session != session_id) {
        app.preview_keep = None;
        app.preview_search_query.clear();
    }
    app.preview_scroll = 0;

    if let Some(cached) = app.preview_cache.get(&session_id) {
        app.preview_lines = cached.lines.clone();
        app.preview_session_id = session_id;
        app.preview_loading = false;
        app.preview_loaded();
        return;
    }

    app.preview_loading = true;
    app.preview_session_id = session_id;
    app.preview_lines = Default::default();
    app.preview_search_matches.clear();
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
        let loaded_any = !result.lines.is_empty();
        let lines: crate::app::PreviewLines = std::sync::Arc::new(result.lines);
        app.cache_preview(result.session_id.clone(), lines.clone());

        // A second load of what's already shown (j then k over it queues
        // two) would reset where you are reading; the first one counts.
        if app.preview_session_id == result.session_id && app.preview_loading {
            app.preview_lines = lines;
            app.preview_loading = false;
            app.preview_loaded();
            changed = true;
        }

        if let Some(session) = app
            .sessions
            .iter_mut()
            .find(|s| s.id == result.session_id)
        {
            // (An empty result, from an unreadable transcript, says nothing.)
            if loaded_any && session.message_count != result.message_count {
                session.message_count = result.message_count;
                changed = true;
            }
        }
    }
    changed
}
