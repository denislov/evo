#[allow(unused_imports)]
use super::helpers::filtered_search_sessions;
use std::sync::Arc;

use super::GlobalSearchSession;

fn search_session(id: &str, name: &str, workspace: &str) -> GlobalSearchSession {
    GlobalSearchSession {
        session_id: Arc::from(id),
        name: Arc::from(name),
        workspace: Arc::from(workspace),
    }
}

#[test]
fn global_search_matches_every_session_field_case_insensitively() {
    let sessions = [
        search_session("session-alpha", "Fix Parser", "Compiler"),
        search_session("session-beta", "Write docs", "Website"),
    ];

    assert_eq!(filtered_search_sessions(&sessions, "parser").len(), 1);
    assert_eq!(filtered_search_sessions(&sessions, "SESSION-BETA").len(), 1);
    assert_eq!(filtered_search_sessions(&sessions, "compiler").len(), 1);
    assert_eq!(filtered_search_sessions(&sessions, "").len(), 2);
    assert!(filtered_search_sessions(&sessions, "settings").is_empty());
}
