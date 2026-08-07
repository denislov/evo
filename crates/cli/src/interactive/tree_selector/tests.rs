use super::*;

fn tree_node(
    id: &str,
    parent: Option<&str>,
    entry_type: &str,
    display_text: String,
    message_role: Option<CodingAgentSessionTreeRole>,
    assistant_has_text: bool,
    assistant_stop_reason: Option<&str>,
) -> CodingAgentSessionTreeNode {
    CodingAgentSessionTreeNode {
        entry_id: id.into(),
        entry_type: entry_type.into(),
        parent_id: parent.map(str::to_owned),
        children: Vec::new(),
        label: None,
        label_timestamp: None,
        display_text,
        message_role,
        assistant_has_text,
        assistant_stop_reason: assistant_stop_reason.map(str::to_owned),
        assistant_error_message: None,
    }
}

fn user_node(id: &str, parent: Option<&str>, text: &str) -> CodingAgentSessionTreeNode {
    tree_node(
        id,
        parent,
        "message",
        format!("user: {text}"),
        Some(CodingAgentSessionTreeRole::User),
        false,
        None,
    )
}

fn assistant_node(id: &str, parent: Option<&str>, text: &str) -> CodingAgentSessionTreeNode {
    tree_node(
        id,
        parent,
        "message",
        format!("assistant: {text}"),
        Some(CodingAgentSessionTreeRole::Assistant),
        true,
        Some("stop"),
    )
}

fn tool_result_node(id: &str, parent: Option<&str>, text: &str) -> CodingAgentSessionTreeNode {
    tree_node(
        id,
        parent,
        "message",
        format!("[read] {text}"),
        Some(CodingAgentSessionTreeRole::ToolResult),
        false,
        None,
    )
}

fn session_info_node(id: &str, parent: Option<&str>, name: &str) -> CodingAgentSessionTreeNode {
    tree_node(
        id,
        parent,
        "session_info",
        format!("[title: {name}]"),
        None,
        false,
        None,
    )
}

fn model_change_node(id: &str, parent: Option<&str>) -> CodingAgentSessionTreeNode {
    tree_node(
        id,
        parent,
        "model_change",
        "[model: claude-sonnet-4]".into(),
        None,
        false,
        None,
    )
}

fn thinking_level_node(id: &str, parent: Option<&str>) -> CodingAgentSessionTreeNode {
    tree_node(
        id,
        parent,
        "thinking_level_change",
        "[thinking: high]".into(),
        None,
        false,
        None,
    )
}

fn tool_call_only_assistant_node(id: &str, parent: Option<&str>) -> CodingAgentSessionTreeNode {
    tree_node(
        id,
        parent,
        "message",
        "assistant: (no content)".into(),
        Some(CodingAgentSessionTreeRole::Assistant),
        false,
        Some("toolUse"),
    )
}

fn key_event(key: Key, modifiers: tui::api::input::KeyModifiers) -> InputEvent {
    InputEvent::Key(tui::api::input::KeyEvent {
        key,
        modifiers,
        kind: tui::api::input::KeyEventKind::Press,
    })
}

#[test]
fn default_filter_hides_session_info() {
    let tree = vec![
        user_node("u1", None, "hello"),
        session_info_node("s1", Some("u1"), "test"),
        user_node("u2", Some("s1"), "world"),
    ];
    let state = TreeSelectorState::new(tree, Some("u2".into()), TreeFilterMode::Default, 80);
    assert_eq!(
        state.visible_nodes.len(),
        2,
        "session_info should be hidden in default filter"
    );
}

#[test]
fn user_only_filter_shows_only_users() {
    let tree = vec![
        user_node("u1", None, "hello"),
        assistant_node("a1", Some("u1"), "hi"),
    ];
    let state = TreeSelectorState::new(tree, Some("a1".into()), TreeFilterMode::UserOnly, 80);
    assert_eq!(state.visible_nodes.len(), 1);
    assert!(state.visible_nodes[0].display_text.starts_with("user:"));
}

#[test]
fn labeled_only_filter_shows_only_labeled() {
    let mut n1 = user_node("u1", None, "hello");
    n1.label = Some("important".to_string());
    let n2 = user_node("u2", Some("u1"), "world");
    let tree = vec![n1, n2];
    let state = TreeSelectorState::new(tree, Some("u2".into()), TreeFilterMode::LabeledOnly, 80);
    assert_eq!(state.visible_nodes.len(), 1);
    assert_eq!(state.visible_nodes[0].entry_id, "u1");
}

#[test]
fn saving_label_emits_intent_without_optimistic_tree_mutation() {
    let mut state = TreeSelectorState::new(
        vec![user_node("u1", None, "hello")],
        Some("u1".into()),
        TreeFilterMode::Default,
        80,
    );
    state.editing_label = true;
    state.editing_label_entry_id = Some("u1".into());
    state.label_input = "checkpoint".into();
    let kbm = KeybindingsManager::new(
        crate::interactive::keybindings::default_keybindings(),
        Default::default(),
    );
    let enter = key_event(Key::Enter, tui::api::input::KeyModifiers::empty());

    let action = state.handle_input(&kbm, &enter);

    assert_eq!(
        action,
        TreeSelectorInput::SaveLabel {
            entry_id: "u1".into(),
            label: Some("checkpoint".into()),
        }
    );
    assert_eq!(state.tree[0].label, None);
    assert_eq!(state.tree[0].label_timestamp, None);
}

#[test]
fn all_filter_shows_everything() {
    let tree = vec![
        user_node("u1", None, "hello"),
        session_info_node("s1", Some("u1"), "test"),
    ];
    let state = TreeSelectorState::new(tree, Some("s1".into()), TreeFilterMode::All, 80);
    assert_eq!(state.visible_nodes.len(), 2);
}

#[test]
fn initial_selection_walks_to_nearest_visible_metadata_parent() {
    for (hidden_node, hidden_id) in [
        (
            model_change_node as fn(&str, Option<&str>) -> CodingAgentSessionTreeNode,
            "model-1",
        ),
        (
            thinking_level_node as fn(&str, Option<&str>) -> CodingAgentSessionTreeNode,
            "thinking-1",
        ),
    ] {
        let mut u2 = user_node("u2", Some("a1"), "active branch");
        u2.children.push(hidden_node(hidden_id, Some("u2")));
        let mut a1 = assistant_node("a1", Some("u1"), "hi");
        a1.children.push(u2);
        a1.children
            .push(user_node("u3", Some("a1"), "sibling branch"));
        let mut u1 = user_node("u1", None, "hello");
        u1.children.push(a1);

        let state = TreeSelectorState::new(
            vec![u1],
            Some(hidden_id.into()),
            TreeFilterMode::Default,
            80,
        );

        assert_eq!(state.selected_entry_id().as_deref(), Some("u2"));
    }
}

#[test]
fn active_branch_is_ordered_before_sibling_branches() {
    let mut branch_b = user_node("u3b", Some("a2"), "branch B");
    branch_b
        .children
        .push(assistant_node("a3b", Some("u3b"), "branch B response"));
    let mut branch_a = user_node("u3a", Some("a2"), "branch A");
    branch_a
        .children
        .push(assistant_node("a3a", Some("u3a"), "branch A response"));
    let mut a2 = assistant_node("a2", Some("u2"), "branch point");
    a2.children.push(branch_b);
    a2.children.push(branch_a);
    let mut u2 = user_node("u2", None, "root");
    u2.children.push(a2);

    let state = TreeSelectorState::new(vec![u2], Some("a3a".into()), TreeFilterMode::Default, 80);
    let ids: Vec<&str> = state
        .visible_nodes
        .iter()
        .map(|node| node.entry_id.as_str())
        .collect();

    let a_index = ids.iter().position(|id| *id == "u3a").unwrap();
    let b_index = ids.iter().position(|id| *id == "u3b").unwrap();
    assert!(a_index < b_index, "{ids:?}");
}

#[test]
fn render_shows_branch_connectors() {
    let state = TreeSelectorState::new(
        branching_tree(),
        Some("a4a".into()),
        TreeFilterMode::Default,
        120,
    );

    let rendered = state.render(120).join("\n");

    assert!(rendered.contains('├'), "{rendered}");
    assert!(rendered.contains('└'), "{rendered}");
}

#[test]
fn search_filters_by_text() {
    let tree = vec![
        user_node("u1", None, "hello world"),
        user_node("u2", Some("u1"), "foo bar"),
    ];
    let mut state = TreeSelectorState::new(tree, Some("u2".into()), TreeFilterMode::All, 80);
    state.search_query = "hello".to_string();
    state.rebuild();
    assert_eq!(state.visible_nodes.len(), 1);
    assert_eq!(state.visible_nodes[0].entry_id, "u1");
}

#[test]
fn fold_and_unfold_node() {
    let child = user_node("u2", Some("u1"), "child");
    let mut parent = user_node("u1", None, "parent");
    parent.children.push(child);
    let tree = vec![parent];
    let mut state = TreeSelectorState::new(tree, Some("u2".into()), TreeFilterMode::All, 80);
    // Initially both visible
    assert_eq!(state.visible_nodes.len(), 2);

    // Fold parent
    state.folded_nodes.insert("u1".to_string());
    state.rebuild();
    assert_eq!(state.visible_nodes.len(), 1);
    assert!(state.visible_nodes[0].is_folded);

    // Unfold
    state.folded_nodes.remove("u1");
    state.rebuild();
    assert_eq!(state.visible_nodes.len(), 2);
}

#[test]
fn initial_selection_uses_current_leaf() {
    let child = assistant_node("a1", Some("u1"), "response");
    let mut parent = user_node("u1", None, "prompt");
    parent.children.push(child);
    let tree = vec![parent];

    let state = TreeSelectorState::new(tree, Some("a1".into()), TreeFilterMode::Default, 80);

    assert_eq!(state.selected_entry_id().as_deref(), Some("a1"));
}

#[test]
fn visual_indent_keeps_single_child_chain_flat() {
    let tool = tool_result_node("t1", Some("a1"), "tool output");
    let mut assistant = assistant_node("a1", Some("u1"), "response");
    assistant.children.push(tool);
    let mut user = user_node("u1", None, "prompt");
    user.children.push(assistant);
    let tree = vec![user];

    let state = TreeSelectorState::new(tree, Some("t1".into()), TreeFilterMode::All, 80);

    let rows: Vec<_> = state
        .visible_nodes
        .iter()
        .map(|node| (node.entry_id.as_str(), node.indent))
        .collect();
    assert_eq!(rows, vec![("u1", 0), ("a1", 0), ("t1", 0)]);
}

#[test]
fn branch_up_uses_parent_chain_when_messages_share_visual_indent() {
    let child = assistant_node("a1", Some("u1"), "response");
    let mut parent = user_node("u1", None, "prompt");
    parent.children.push(child);
    let tree = vec![parent];
    let mut state = TreeSelectorState::new(tree, Some("a1".into()), TreeFilterMode::Default, 80);
    let kbm = KeybindingsManager::new(
        crate::interactive::keybindings::default_keybindings(),
        Default::default(),
    );
    let event = key_event(Key::Left, tui::api::input::KeyModifiers::CTRL);

    state.handle_input(&kbm, &event);

    assert_eq!(state.selected_entry_id().as_deref(), Some("u1"));
}

fn branching_tree() -> Vec<CodingAgentSessionTreeNode> {
    let mut u3a = user_node("u3a", Some("a2"), "branch A start");
    let mut a3a = assistant_node("a3a", Some("u3a"), "branch A response");
    let mut u4a = user_node("u4a", Some("a3a"), "branch A deep");
    u4a.children
        .push(assistant_node("a4a", Some("u4a"), "branch A leaf"));
    a3a.children.push(u4a);
    u3a.children.push(a3a);

    let mut u3b = user_node("u3b", Some("a2"), "branch B start");
    let mut a3b = assistant_node("a3b", Some("u3b"), "branch B response");
    a3b.children
        .push(user_node("u4b", Some("a3b"), "branch B deep"));
    u3b.children.push(a3b);

    let mut a2 = assistant_node("a2", Some("u2"), "response 2");
    a2.children.push(u3a);
    a2.children.push(u3b);
    let mut u2 = user_node("u2", Some("a1"), "second message");
    u2.children.push(a2);
    let mut a1 = assistant_node("a1", Some("u1"), "response 1");
    a1.children.push(u2);
    let mut u1 = user_node("u1", None, "first message");
    u1.children.push(a1);
    vec![u1]
}

#[test]
fn ctrl_left_folds_and_ctrl_right_unfolds_branch_segment() {
    let mut state = TreeSelectorState::new(
        branching_tree(),
        Some("a4a".into()),
        TreeFilterMode::Default,
        80,
    );
    let kbm = KeybindingsManager::new(
        crate::interactive::keybindings::default_keybindings(),
        Default::default(),
    );
    let ctrl_left = key_event(Key::Left, tui::api::input::KeyModifiers::CTRL);
    let ctrl_right = key_event(Key::Right, tui::api::input::KeyModifiers::CTRL);
    let down = key_event(Key::Down, tui::api::input::KeyModifiers::empty());

    state.handle_input(&kbm, &ctrl_left);
    assert_eq!(state.selected_entry_id().as_deref(), Some("u3a"));

    state.handle_input(&kbm, &ctrl_left);
    assert_eq!(state.selected_entry_id().as_deref(), Some("u3a"));

    state.handle_input(&kbm, &down);
    assert_eq!(state.selected_entry_id().as_deref(), Some("u3b"));

    state.handle_input(&kbm, &ctrl_right);
    assert_eq!(state.selected_entry_id().as_deref(), Some("u4b"));

    state.handle_input(&kbm, &ctrl_left);
    assert_eq!(state.selected_entry_id().as_deref(), Some("u3b"));
}

#[test]
fn folding_visible_root_hides_descendants_through_filtered_parent() {
    let mut hidden = session_info_node("s1", Some("u1"), "title");
    let mut u2 = user_node("u2", Some("s1"), "follow up");
    u2.children
        .push(assistant_node("a2", Some("u2"), "response"));
    hidden.children.push(u2);
    let mut u1 = user_node("u1", None, "hello");
    u1.children.push(hidden);
    let mut state =
        TreeSelectorState::new(vec![u1], Some("a2".into()), TreeFilterMode::Default, 80);
    let kbm = KeybindingsManager::new(
        crate::interactive::keybindings::default_keybindings(),
        Default::default(),
    );
    let ctrl_left = key_event(Key::Left, tui::api::input::KeyModifiers::CTRL);
    let down = key_event(Key::Down, tui::api::input::KeyModifiers::empty());

    state.handle_input(&kbm, &ctrl_left);
    assert_eq!(state.selected_entry_id().as_deref(), Some("u1"));

    state.handle_input(&kbm, &ctrl_left);
    assert_eq!(state.visible_nodes.len(), 1);

    state.handle_input(&kbm, &down);
    assert_eq!(state.selected_entry_id().as_deref(), Some("u1"));
}

#[test]
fn default_filter_hides_tool_call_only_assistant_except_current_leaf() {
    let mut tool_only = tool_call_only_assistant_node("tool-a1", Some("u1"));
    tool_only
        .children
        .push(user_node("u2", Some("tool-a1"), "follow up"));
    let mut u1 = user_node("u1", None, "hello");
    u1.children.push(tool_only);

    let state = TreeSelectorState::new(
        vec![u1.clone()],
        Some("u2".into()),
        TreeFilterMode::Default,
        80,
    );
    let ids: Vec<&str> = state
        .visible_nodes
        .iter()
        .map(|node| node.entry_id.as_str())
        .collect();
    assert_eq!(ids, vec!["u1", "u2"]);

    let state = TreeSelectorState::new(
        vec![u1],
        Some("tool-a1".into()),
        TreeFilterMode::Default,
        80,
    );
    let ids: Vec<&str> = state
        .visible_nodes
        .iter()
        .map(|node| node.entry_id.as_str())
        .collect();
    assert_eq!(ids, vec!["u1", "tool-a1", "u2"]);
}

#[test]
fn arrow_down_changes_selection() {
    let child = assistant_node("a1", Some("u1"), "response");
    let mut parent = user_node("u1", None, "prompt");
    parent.children.push(child);
    let tree = vec![parent];
    let mut state = TreeSelectorState::new(tree, Some("u1".into()), TreeFilterMode::Default, 80);
    let kbm = KeybindingsManager::new(
        crate::interactive::keybindings::default_keybindings(),
        Default::default(),
    );
    let event = InputEvent::Key(tui::api::input::KeyEvent {
        key: Key::Down,
        modifiers: tui::api::input::KeyModifiers::empty(),
        kind: tui::api::input::KeyEventKind::Press,
    });

    state.handle_input(&kbm, &event);

    assert_eq!(state.selected_entry_id().as_deref(), Some("a1"));
}

#[test]
fn cycle_filter_through_all_modes() {
    let tree = vec![user_node("u1", None, "hello")];
    let mut state = TreeSelectorState::new(tree, Some("u1".into()), TreeFilterMode::Default, 80);
    assert_eq!(state.filter_mode, TreeFilterMode::Default);

    state.cycle_filter(true);
    assert_eq!(state.filter_mode, TreeFilterMode::NoTools);

    state.cycle_filter(true);
    assert_eq!(state.filter_mode, TreeFilterMode::UserOnly);

    state.cycle_filter(true);
    assert_eq!(state.filter_mode, TreeFilterMode::LabeledOnly);

    state.cycle_filter(true);
    assert_eq!(state.filter_mode, TreeFilterMode::All);

    state.cycle_filter(true);
    assert_eq!(state.filter_mode, TreeFilterMode::Default);

    // Backwards
    state.cycle_filter(false);
    assert_eq!(state.filter_mode, TreeFilterMode::All);
}

#[test]
fn selection_persists_across_filter_change() {
    let mut u1 = user_node("u1", None, "first");
    u1.children.push(user_node("u2", Some("u1"), "second"));
    let tree = vec![u1];
    let mut state = TreeSelectorState::new(tree, Some("u2".into()), TreeFilterMode::All, 80);
    state.selected = 1; // select u2
    state.last_selected_id = Some("u2".into());

    // Switch to user-only (still shows both since both are users)
    state.set_filter(TreeFilterMode::Default);
    // Selection should persist (u2 still visible)
    assert_eq!(state.visible_nodes[state.selected].entry_id, "u2");
}

#[test]
fn format_timestamp_shows_hhmm() {
    let ts = "2026-06-05T14:30:00.000Z";
    let formatted = format_label_timestamp(ts);
    assert_eq!(formatted, "14:30");
}
