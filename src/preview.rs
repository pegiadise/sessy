use crate::app::App;
use crate::parser::{extract_conversation, Role};
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

    let session_id = session.id.clone();
    let file_path = session.file_path.clone();

    // Check FIFO cache
    if let Some(cached) = app.preview_cache.get(&session_id) {
        app.preview_lines = cached.clone();
        app.preview_session_id = session_id;
        app.preview_loading = false;
        return;
    }

    app.preview_loading = true;
    app.preview_session_id = session_id.clone();
    app.preview_lines.clear();

    let tx = app.preview_tx.clone();

    thread::spawn(move || {
        let messages = extract_conversation(&file_path);
        let lines: Vec<(String, bool)> = messages
            .into_iter()
            .map(|m| (m.text, m.role == Role::User))
            .collect();
        let message_count = lines.iter().filter(|(_, is_user)| *is_user).count() as u32;

        let _ = tx.send(crate::app::PreviewResult {
            session_id,
            lines,
            message_count,
        });
    });
}

/// Check for completed preview loads and update app state.
pub fn check_preview_updates(app: &mut App) {
    if let Ok(result) = app.preview_rx.try_recv() {
        // FIFO cache
        app.cache_preview(result.session_id.clone(), result.lines.clone());

        if app.preview_session_id == result.session_id {
            app.preview_lines = result.lines;
            app.preview_loading = false;
        }

        if let Some(session) = app
            .sessions
            .iter_mut()
            .find(|s| s.id == result.session_id)
        {
            session.message_count = Some(result.message_count);
        }
    }
}
