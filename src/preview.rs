use crate::app::App;
use crate::parser::{extract_conversation_ext, Speaker};
use std::thread;

/// Request a preview for the currently selected session.
pub fn request_preview(app: &mut App) {
    let session = match app.selected_session() {
        Some(s) => s,
        None => {
            app.preview_lines.clear();
            app.preview_loading = false;
            return;
        }
    };

    if app.preview_session_id == session.id && !app.preview_lines.is_empty() {
        return;
    }

    // Loading a different session (or reloading after a tools toggle): any
    // committed search matches point into the old conversation's lines.
    app.clear_preview_search();

    let session = match app.selected_session() {
        Some(s) => s,
        None => return,
    };
    let session_id = session.id.clone();
    let file_path = session.file_path.clone();
    let include_tools = app.show_tools;

    // Check FIFO cache
    if let Some(cached) = app.preview_cache.get(&session_id) {
        app.preview_lines = cached.clone();
        app.recompute_preview_offsets();
        app.preview_session_id = session_id;
        app.preview_loading = false;
        return;
    }

    app.preview_loading = true;
    app.preview_session_id = session_id.clone();
    app.preview_lines.clear();

    let tx = app.preview_tx.clone();

    thread::spawn(move || {
        let messages = extract_conversation_ext(&file_path, include_tools);
        let lines: Vec<(String, String, Speaker)> = messages
            .into_iter()
            .map(|(speaker, text)| {
                let lower = text.to_lowercase();
                (text, lower, speaker)
            })
            .collect();
        let message_count = lines
            .iter()
            .filter(|(_, _, sp)| *sp == Speaker::User)
            .count() as u32;

        let _ = tx.send(crate::app::PreviewResult {
            session_id,
            lines,
            message_count,
            include_tools,
        });
    });
}

/// Check for completed preview loads and update app state.
pub fn check_preview_updates(app: &mut App) {
    while let Ok(result) = app.preview_rx.try_recv() {
        // Extracted under a tool-activity setting that has since been toggled:
        // caching or applying it would show stale lines. A fresh request was
        // already spawned by the toggle handler, so just drop it.
        if result.include_tools != app.show_tools {
            continue;
        }
        // FIFO cache
        app.cache_preview(result.session_id.clone(), result.lines.clone());

        if app.preview_session_id == result.session_id {
            app.preview_lines = result.lines;
            app.recompute_preview_offsets();
            app.preview_loading = false;
        }

        if let Some(session) = app
            .sessions
            .iter_mut()
            .find(|s| s.id == result.session_id)
        {
            session.message_count = result.message_count;
        }
    }
}
