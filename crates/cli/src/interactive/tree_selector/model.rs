use super::*;

/// Build the set of entry ids on the active path (root -> current leaf).
pub(super) fn build_active_path_ids(
    tree: &[CodingAgentSessionTreeNode],
    current_leaf_id: Option<&str>,
) -> BTreeSet<String> {
    let Some(leaf_id) = current_leaf_id else {
        return BTreeSet::new();
    };

    // Recursively find the path.
    fn find_path(
        nodes: &[CodingAgentSessionTreeNode],
        target: &str,
        path: &mut Vec<String>,
    ) -> bool {
        for node in nodes {
            path.push(node.entry_id.clone());
            if node.entry_id == target {
                return true;
            }
            if find_path(&node.children, target, path) {
                return true;
            }
            path.pop();
        }
        false
    }

    let mut path = Vec::new();
    find_path(tree, leaf_id, &mut path);
    path.into_iter().collect()
}

/// Flatten the tree into a Vec<FlatTreeNode> with indent and connector info.
pub(super) fn flatten_tree(
    tree: &[CodingAgentSessionTreeNode],
    current_leaf_id: Option<&str>,
    active_path_ids: &BTreeSet<String>,
) -> Vec<FlatTreeNode> {
    let mut result = Vec::new();
    let contains_active = build_contains_active_map(tree, current_leaf_id);

    fn flatten_recursive(
        nodes: &[CodingAgentSessionTreeNode],
        current_leaf_id: Option<&str>,
        active_path_ids: &BTreeSet<String>,
        contains_active: &BTreeMap<String, bool>,
        result: &mut Vec<FlatTreeNode>,
    ) {
        let mut ordered_nodes: Vec<&CodingAgentSessionTreeNode> = nodes.iter().collect();
        ordered_nodes.sort_by_key(|node| {
            if contains_active
                .get(&node.entry_id)
                .copied()
                .unwrap_or(false)
            {
                0
            } else {
                1
            }
        });

        for node in ordered_nodes {
            let is_active = active_path_ids.contains(&node.entry_id);
            let role = node.message_role.map(|role| match role {
                CodingAgentSessionTreeRole::User => "user",
                CodingAgentSessionTreeRole::Assistant => "assistant",
                CodingAgentSessionTreeRole::ToolResult => "toolResult",
                CodingAgentSessionTreeRole::Other => "other",
            });

            result.push(FlatTreeNode {
                entry_id: node.entry_id.clone(),
                entry_type: node.entry_type.clone(),
                parent_id: node.parent_id.clone(),
                indent: 0,
                show_connector: false,
                is_last: false,
                gutters: Vec::new(),
                is_virtual_root_child: false,
                label: node.label.clone(),
                label_timestamp: node.label_timestamp.clone(),
                is_active,
                is_foldable: false,
                is_folded: false,
                display_text: node.display_text.clone(),
                message_role: role.map(str::to_owned),
                assistant_has_text: node.assistant_has_text,
                assistant_stop_reason: node.assistant_stop_reason.clone(),
                assistant_error_message: node.assistant_error_message.clone(),
                is_current_leaf: current_leaf_id == Some(node.entry_id.as_str()),
            });

            flatten_recursive(
                &node.children,
                current_leaf_id,
                active_path_ids,
                contains_active,
                result,
            );
        }
    }

    flatten_recursive(
        tree,
        current_leaf_id,
        active_path_ids,
        &contains_active,
        &mut result,
    );
    result
}

fn build_contains_active_map(
    tree: &[CodingAgentSessionTreeNode],
    current_leaf_id: Option<&str>,
) -> BTreeMap<String, bool> {
    fn walk(
        node: &CodingAgentSessionTreeNode,
        current_leaf_id: Option<&str>,
        result: &mut BTreeMap<String, bool>,
    ) -> bool {
        let mut contains = current_leaf_id == Some(node.entry_id.as_str());
        for child in &node.children {
            if walk(child, current_leaf_id, result) {
                contains = true;
            }
        }
        result.insert(node.entry_id.clone(), contains);
        contains
    }

    let mut result = BTreeMap::new();
    for node in tree {
        walk(node, current_leaf_id, &mut result);
    }
    result
}

pub(super) fn folded_descendant_ids(
    flat_nodes: &[FlatTreeNode],
    folded_nodes: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut skipped = BTreeSet::new();
    for node in flat_nodes {
        if let Some(parent_id) = &node.parent_id
            && (folded_nodes.contains(parent_id) || skipped.contains(parent_id))
        {
            skipped.insert(node.entry_id.clone());
        }
    }
    skipped
}

/// Apply filter and search to the flat nodes.
pub(super) fn apply_filter_and_search(
    nodes: &[FlatTreeNode],
    filter: TreeFilterMode,
    search: &str,
) -> Vec<FlatTreeNode> {
    let filtered: Vec<&FlatTreeNode> = nodes
        .iter()
        .filter(|n| match filter {
            TreeFilterMode::Default => passes_default_filter(n),
            TreeFilterMode::NoTools => passes_default_filter(n) && !is_tool_result(n),
            TreeFilterMode::UserOnly => n.message_role.as_deref() == Some("user"),
            TreeFilterMode::LabeledOnly => n.label.is_some() && n.label.as_deref() != Some(""),
            TreeFilterMode::All => true,
        })
        .collect();

    if search.is_empty() {
        return filtered.into_iter().cloned().collect();
    }

    let query_lower = search.to_lowercase();
    let search_terms: Vec<&str> = query_lower.split_whitespace().collect();

    filtered
        .into_iter()
        .filter(|n| {
            let text = searchable_text(n).to_lowercase();
            search_terms.iter().all(|term| text.contains(term))
        })
        .cloned()
        .collect()
}

fn passes_default_filter(n: &FlatTreeNode) -> bool {
    if n.message_role.as_deref() == Some("assistant") && !n.is_current_leaf {
        let is_error_or_aborted = n
            .assistant_stop_reason
            .as_deref()
            .is_some_and(|reason| reason != "stop" && reason != "toolUse")
            || n.assistant_error_message.is_some();
        if !n.assistant_has_text && !is_error_or_aborted {
            return false;
        }
    }

    !matches!(
        n.entry_type.as_str(),
        "label" | "custom" | "model_change" | "thinking_level_change" | "session_info"
    )
}

fn is_tool_result(n: &FlatTreeNode) -> bool {
    n.message_role.as_deref() == Some("toolResult")
}

pub(super) fn matches_key(event: &InputEvent, key: &str) -> bool {
    tui::api::input::matches_key(event, key)
}

pub(super) fn is_plain_char(event: &InputEvent, expected: &str) -> bool {
    matches!(
        event,
        InputEvent::Key(tui::api::input::KeyEvent {
            key: Key::Char(ch),
            modifiers,
            ..
        }) if modifiers.is_empty() && ch == expected
    )
}

/// Build searchable text for a flat node.
fn searchable_text(n: &FlatTreeNode) -> String {
    let mut parts = vec![
        n.display_text.clone(),
        n.entry_type.clone(),
        n.entry_id.clone(),
    ];
    if let Some(label) = &n.label {
        parts.push(label.clone());
    }
    parts.join(" ")
}

/// Render a single tree row.
pub(super) fn render_tree_row(
    node: &FlatTreeNode,
    is_selected: bool,
    multiple_roots: bool,
    width: usize,
    show_timestamps: bool,
) -> String {
    let cursor = if is_selected { "› " } else { "  " };
    let display_indent = if multiple_roots {
        node.indent.saturating_sub(1)
    } else {
        node.indent
    };
    let connector_position = if node.show_connector && !node.is_virtual_root_child {
        display_indent.checked_sub(1)
    } else {
        None
    };

    let mut prefix = String::new();
    for i in 0..display_indent * 3 {
        let level = i / 3;
        let pos_in_level = i % 3;
        if let Some(gutter) = node.gutters.iter().find(|gutter| gutter.position == level) {
            if pos_in_level == 0 && gutter.show {
                prefix.push('│');
            } else {
                prefix.push(' ');
            }
        } else if connector_position == Some(level) {
            match pos_in_level {
                0 => prefix.push(if node.is_last { '└' } else { '├' }),
                1 => {
                    if node.is_folded {
                        prefix.push('⊞');
                    } else if node.is_foldable {
                        prefix.push('⊟');
                    } else {
                        prefix.push('─');
                    }
                }
                _ => prefix.push(' '),
            }
        } else {
            prefix.push(' ');
        }
    }

    let shows_fold_in_connector = node.show_connector && !node.is_virtual_root_child;
    let fold_marker = if node.is_folded && !shows_fold_in_connector {
        "⊞ "
    } else {
        ""
    };
    let active_marker = if node.is_active { "• " } else { "" };
    let mut text = format!("{cursor}{prefix}{fold_marker}{active_marker}");

    if let Some(label) = &node.label
        && !label.is_empty()
    {
        text.push_str(&format!("[{}] ", label));
    }

    if show_timestamps && let Some(ts) = &node.label_timestamp {
        let formatted = format_label_timestamp(ts);
        text.push_str(&format!("{formatted} "));
    }

    text.push_str(&node.display_text);

    truncate_to_width_with_ellipsis(&text, width)
}

/// Format a label timestamp.
pub(super) fn format_label_timestamp(timestamp: &str) -> String {
    // Simple formatting: take just the time portion.
    // ISO format: "2026-06-05T00:00:02.000Z"
    if let Some(t_part) = timestamp.split('T').nth(1) {
        let t = t_part.trim_end_matches('Z');
        if let Some(hhmm) = t.split('.').next() {
            return hhmm[..5].to_string();
        }
        t[..5].to_string()
    } else {
        timestamp.to_string()
    }
}
