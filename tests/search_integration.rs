use std::path::PathBuf;
use std::collections::HashSet;
use sessy::app::App;
use sessy::session::SessionMeta;
use sessy::text_cache::TextCache;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn make_session(id: &str, name: &str, project: &str, tickets: Vec<&str>) -> SessionMeta {
    SessionMeta {
        id: id.into(),
        project: project.into(),
        branch: "main".into(),
        name: name.into(),
        title: name.into(),
        last_message: String::new(),
        duration_secs: 0,
        timestamp: 0,
        file_size: 0,
        file_mtime: 0,
        file_path: fixture("simple_session.jsonl"),
        cwd: String::new(),
        message_count: None,
        tickets: tickets.into_iter().map(String::from).collect(),
        text_offset: 0,
        text_len: 0,
        name_lc: name.to_lowercase(),
        title_lc: name.to_lowercase(),
        project_lc: project.to_lowercase(),
        branch_lc: "main".into(),
    }
}

#[test]
fn ticket_query_finds_ticketed_session_first() {
    let mut t = make_session("a", "other", "p", vec!["PROJ-123"]);
    t.tickets.sort();
    let sessions = vec![
        make_session("b", "kerveros", "p", vec![]),
        t,
    ];
    let cache = TextCache::open(std::path::Path::new("/does/not/exist"));
    let mut app = App::new(sessions, false, HashSet::new(), cache);
    app.search_query = "PROJ-123".into();
    app.apply_search();
    assert!(!app.filtered_indices.is_empty());
    assert_eq!(app.sessions[app.filtered_indices[0]].id, "a");
}

#[test]
fn multi_token_and_match() {
    let cache = TextCache::open(std::path::Path::new("/does/not/exist"));
    let sessions = vec![
        make_session("a", "kerveros encryption", "p", vec![]),
        make_session("b", "kerveros", "p", vec![]),
    ];
    let mut app = App::new(sessions, false, HashSet::new(), cache);
    app.search_query = "kerv encrypt".into();
    app.apply_search();
    let ids: Vec<String> = app
        .filtered_indices
        .iter()
        .map(|&i| app.sessions[i].id.clone())
        .collect();
    assert_eq!(ids, vec!["a".to_string()]);
}
