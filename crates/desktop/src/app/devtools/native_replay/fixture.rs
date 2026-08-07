use coding_agent::api::client::CodingAgentFileChangeSnapshot;

pub(in crate::app::devtools) fn visual_change(
    path: &'static str,
    first_changed_line: usize,
    added_lines: usize,
    removed_lines: usize,
    diff: Option<&'static str>,
) -> CodingAgentFileChangeSnapshot {
    CodingAgentFileChangeSnapshot {
        path: path.into(),
        mutation_kind: "edit".into(),
        source: "agent_edit".into(),
        operation_id: "visual-operation".into(),
        tool_call_id: Some("visual-running-edit".into()),
        session_id: Some("desktop-native-visual".into()),
        turn_id: Some("visual-turn".into()),
        updated_sequence: 2,
        before_revision: Some("before".into()),
        after_revision: "after".into(),
        after_exists: true,
        first_changed_line: Some(first_changed_line),
        added_lines: Some(added_lines),
        removed_lines: Some(removed_lines),
        diff: diff.map(Into::into),
        hunks: Vec::new(),
    }
}
