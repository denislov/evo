use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use tui::api::input::{InputEvent, Key, KeybindingsManager};
use tui::api::render::{
    SYSTEM, USER, color_enabled, paint_with, truncate_to_width_with_ellipsis, visible_width,
};

use crate::interactive::render::fit_line;
use coding_agent::api::view::{CodingAgentSessionTreeNode, CodingAgentSessionTreeRole};

/// Filter mode for the `/tree` selector. Product presentation policy owned by
/// `coding-agent`; transcript DTOs remain in `agent-core`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TreeFilterMode {
    Default,
    NoTools,
    UserOnly,
    LabeledOnly,
    All,
}

impl TreeFilterMode {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::NoTools => "no-tools",
            Self::UserOnly => "user-only",
            Self::LabeledOnly => "labeled-only",
            Self::All => "all",
        }
    }
}

impl fmt::Display for TreeFilterMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Maximum number of tree rows shown per page.
mod model;
#[cfg(test)]
mod tests;

#[cfg(test)]
use model::format_label_timestamp;
use model::{
    apply_filter_and_search, build_active_path_ids, flatten_tree, folded_descendant_ids,
    is_plain_char, matches_key, render_tree_row,
};

const PAGE_SIZE: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
struct GutterInfo {
    position: usize,
    show: bool,
}

/// Result of a tree selector input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TreeSelectorInput {
    /// Event was handled, no action needed.
    Handled,
    /// User cancelled (Esc).
    Cancel,
    /// User confirmed selection of a node id.
    Confirm(Option<String>),
    /// User requested to edit the label for an entry.
    EditLabel {
        entry_id: String,
        current_label: Option<String>,
    },
    /// User saved a label change.
    SaveLabel {
        entry_id: String,
        label: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchDirection {
    Up,
    Down,
}

/// A flattened, filtered, and projection-ready tree node for display.
#[derive(Debug, Clone)]
struct FlatTreeNode {
    entry_id: String,
    entry_type: String,
    parent_id: Option<String>,
    indent: usize,
    show_connector: bool,
    is_last: bool,
    gutters: Vec<GutterInfo>,
    is_virtual_root_child: bool,
    label: Option<String>,
    label_timestamp: Option<String>,
    is_active: bool,
    is_foldable: bool,
    is_folded: bool,
    display_text: String,
    message_role: Option<String>,
    assistant_has_text: bool,
    assistant_stop_reason: Option<String>,
    assistant_error_message: Option<String>,
    /// Whether this node is the current leaf.
    is_current_leaf: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TreeSelectorRenderState {
    selected_entry_id: Option<String>,
    selected_index: usize,
    visible_len: usize,
    filter_mode: TreeFilterMode,
    search_query: String,
    folded_nodes: Vec<String>,
    show_label_timestamps: bool,
    editing_label: bool,
    label_input: String,
}

/// State for the tree selector UI.
#[derive(Debug, Clone)]
pub(super) struct TreeSelectorState {
    /// The full session tree (forest).
    tree: Vec<CodingAgentSessionTreeNode>,
    /// Current leaf id.
    current_leaf_id: Option<String>,
    /// Flat projection of the visible tree.
    flat_nodes: Vec<FlatTreeNode>,
    /// Visible nodes after filter + search.
    visible_nodes: Vec<FlatTreeNode>,
    /// Index into visible_nodes.
    selected: usize,
    /// Last selected entry id for restoring when filter changes back.
    last_selected_id: Option<String>,
    /// Current filter mode.
    filter_mode: TreeFilterMode,
    /// Current search query.
    search_query: String,
    /// Entry ids that are folded (collapsed).
    folded_nodes: BTreeSet<String>,
    /// Visible parent relationships after filter/search/fold projection.
    visible_parent_map: BTreeMap<String, Option<String>>,
    /// Visible children relationships after filter/search/fold projection.
    visible_children_map: BTreeMap<Option<String>, Vec<String>>,
    /// Whether the current visible forest has multiple roots.
    multiple_roots: bool,
    /// Whether to show label timestamps.
    show_label_timestamps: bool,
    /// Ids on the active path (from root to current leaf).
    active_path_ids: BTreeSet<String>,
    /// Whether we are in label-editing mode.
    pub(crate) editing_label: bool,
    /// The entry id being label-edited.
    pub(crate) editing_label_entry_id: Option<String>,
    /// Current label input text.
    pub(crate) label_input: String,
}

impl TreeSelectorState {
    /// Create a new tree selector state from the session tree.
    pub(super) fn new(
        tree: Vec<CodingAgentSessionTreeNode>,
        current_leaf_id: Option<String>,
        filter_mode: TreeFilterMode,
        _width: usize,
    ) -> Self {
        let active_path_ids = build_active_path_ids(&tree, current_leaf_id.as_deref());
        let mut state = Self {
            tree,
            current_leaf_id,
            flat_nodes: Vec::new(),
            visible_nodes: Vec::new(),
            selected: 0,
            last_selected_id: None,
            filter_mode,
            search_query: String::new(),
            folded_nodes: BTreeSet::new(),
            visible_parent_map: BTreeMap::new(),
            visible_children_map: BTreeMap::new(),
            multiple_roots: false,
            show_label_timestamps: false,
            active_path_ids,
            editing_label: false,
            editing_label_entry_id: None,
            label_input: String::new(),
        };
        state.rebuild();
        state.selected = state
            .find_nearest_visible_index(state.current_leaf_id.as_deref())
            .unwrap_or(0);
        if let Some(node) = state.visible_nodes.get(state.selected) {
            state.last_selected_id = Some(node.entry_id.clone());
        }
        state
    }

    /// Rebuild flat_nodes and visible_nodes after a state change.
    fn rebuild(&mut self) {
        self.flat_nodes = flatten_tree(
            &self.tree,
            self.current_leaf_id.as_deref(),
            &self.active_path_ids,
        );
        let mut visible_nodes =
            apply_filter_and_search(&self.flat_nodes, self.filter_mode, &self.search_query);
        if !self.folded_nodes.is_empty() {
            let skipped = folded_descendant_ids(&self.flat_nodes, &self.folded_nodes);
            visible_nodes.retain(|node| !skipped.contains(&node.entry_id));
        }
        self.recalculate_visual_structure(&mut visible_nodes);
        self.visible_nodes = visible_nodes;

        if self.visible_nodes.is_empty() {
            self.selected = 0;
            // Preserve last_selected_id through empty filters/searches so it
            // can be restored when the result set becomes non-empty again.
            return;
        }

        if let Some(target_id) = self
            .last_selected_id
            .clone()
            .or_else(|| self.current_leaf_id.clone())
            && let Some(pos) = self.find_nearest_visible_index(Some(&target_id))
        {
            self.selected = pos;
        } else if self.selected >= self.visible_nodes.len() {
            self.selected = self.visible_nodes.len() - 1;
        }

        self.last_selected_id = self
            .visible_nodes
            .get(self.selected)
            .map(|node| node.entry_id.clone());
    }

    fn recalculate_visual_structure(&mut self, visible_nodes: &mut [FlatTreeNode]) {
        self.visible_parent_map.clear();
        self.visible_children_map.clear();
        self.multiple_roots = false;
        if visible_nodes.is_empty() {
            return;
        }

        let visible_ids: BTreeSet<String> = visible_nodes
            .iter()
            .map(|node| node.entry_id.clone())
            .collect();
        let full_parent_map: BTreeMap<String, Option<String>> = self
            .flat_nodes
            .iter()
            .map(|node| (node.entry_id.clone(), node.parent_id.clone()))
            .collect();

        let find_visible_ancestor = |node_id: &str| -> Option<String> {
            let mut current_id = full_parent_map.get(node_id).and_then(Clone::clone);
            while let Some(id) = current_id {
                if visible_ids.contains(&id) {
                    return Some(id);
                }
                current_id = full_parent_map.get(&id).and_then(Clone::clone);
            }
            None
        };

        self.visible_children_map.insert(None, Vec::new());
        for node in visible_nodes.iter() {
            let ancestor_id = find_visible_ancestor(&node.entry_id);
            self.visible_parent_map
                .insert(node.entry_id.clone(), ancestor_id.clone());
            self.visible_children_map
                .entry(ancestor_id)
                .or_default()
                .push(node.entry_id.clone());
        }

        let visible_root_ids = self
            .visible_children_map
            .get(&None)
            .cloned()
            .unwrap_or_default();
        self.multiple_roots = visible_root_ids.len() > 1;

        let index_by_id: BTreeMap<String, usize> = visible_nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.entry_id.clone(), index))
            .collect();

        type StackItem = (String, usize, bool, bool, bool, Vec<GutterInfo>, bool);
        let mut stack: Vec<StackItem> = Vec::new();
        for (i, root_id) in visible_root_ids.iter().enumerate().rev() {
            let is_last = i == visible_root_ids.len().saturating_sub(1);
            stack.push((
                root_id.clone(),
                if self.multiple_roots { 1 } else { 0 },
                self.multiple_roots,
                self.multiple_roots,
                is_last,
                Vec::new(),
                self.multiple_roots,
            ));
        }

        while let Some((
            node_id,
            indent,
            just_branched,
            show_connector,
            is_last,
            gutters,
            is_virtual_root_child,
        )) = stack.pop()
        {
            let Some(index) = index_by_id.get(&node_id).copied() else {
                continue;
            };
            visible_nodes[index].indent = indent;
            visible_nodes[index].show_connector = show_connector;
            visible_nodes[index].is_last = is_last;
            visible_nodes[index].gutters = gutters.clone();
            visible_nodes[index].is_virtual_root_child = is_virtual_root_child;
            visible_nodes[index].is_folded = self.folded_nodes.contains(&node_id);

            let children = self
                .visible_children_map
                .get(&Some(node_id.clone()))
                .cloned()
                .unwrap_or_default();
            let multiple_children = children.len() > 1;
            let child_indent = if multiple_children || (just_branched && indent > 0) {
                indent + 1
            } else {
                indent
            };

            let connector_displayed = show_connector && !is_virtual_root_child;
            let current_display_indent = if self.multiple_roots {
                indent.saturating_sub(1)
            } else {
                indent
            };
            let connector_position = current_display_indent.saturating_sub(1);
            let child_gutters = if connector_displayed {
                let mut next = gutters;
                next.push(GutterInfo {
                    position: connector_position,
                    show: !is_last,
                });
                next
            } else {
                gutters
            };

            for (i, child_id) in children.iter().enumerate().rev() {
                let child_is_last = i == children.len().saturating_sub(1);
                stack.push((
                    child_id.clone(),
                    child_indent,
                    multiple_children,
                    multiple_children,
                    child_is_last,
                    child_gutters.clone(),
                    false,
                ));
            }
        }

        for node in visible_nodes.iter_mut() {
            let children = self
                .visible_children_map
                .get(&Some(node.entry_id.clone()))
                .cloned()
                .unwrap_or_default();
            if children.is_empty() {
                node.is_foldable = false;
                continue;
            }
            let parent_id = self
                .visible_parent_map
                .get(&node.entry_id)
                .cloned()
                .flatten();
            node.is_foldable = match parent_id {
                None => true,
                Some(parent_id) => self
                    .visible_children_map
                    .get(&Some(parent_id))
                    .is_some_and(|siblings| siblings.len() > 1),
            };
        }
    }

    fn find_nearest_visible_index(&self, entry_id: Option<&str>) -> Option<usize> {
        let mut current_id = entry_id?;
        loop {
            if let Some(index) = self
                .visible_nodes
                .iter()
                .position(|node| node.entry_id == current_id)
            {
                return Some(index);
            }
            let parent_id = self
                .flat_nodes
                .iter()
                .find(|node| node.entry_id == current_id)
                .and_then(|node| node.parent_id.as_deref())?;
            current_id = parent_id;
        }
    }

    pub(super) fn render_state(&self) -> TreeSelectorRenderState {
        TreeSelectorRenderState {
            selected_entry_id: self.selected_entry_id(),
            selected_index: self.selected,
            visible_len: self.visible_nodes.len(),
            filter_mode: self.filter_mode,
            search_query: self.search_query.clone(),
            folded_nodes: self.folded_nodes.iter().cloned().collect(),
            show_label_timestamps: self.show_label_timestamps,
            editing_label: self.editing_label,
            label_input: self.label_input.clone(),
        }
    }

    /// Handle an input event. Returns what action should be taken.
    pub(super) fn handle_input(
        &mut self,
        kbm: &KeybindingsManager,
        event: &InputEvent,
    ) -> TreeSelectorInput {
        // Label editing mode captures all input.
        if self.editing_label {
            return self.handle_label_input(kbm, event);
        }

        if kbm.matches(event, "tui.select.down") || matches_key(event, "down") {
            if !self.visible_nodes.is_empty() {
                self.selected = (self.selected + 1) % self.visible_nodes.len();
                self.last_selected_id = Some(self.visible_nodes[self.selected].entry_id.clone());
            }
            return TreeSelectorInput::Handled;
        }

        if kbm.matches(event, "tui.select.up") || matches_key(event, "up") {
            if !self.visible_nodes.is_empty() {
                self.selected =
                    (self.selected + self.visible_nodes.len() - 1) % self.visible_nodes.len();
                self.last_selected_id = Some(self.visible_nodes[self.selected].entry_id.clone());
            }
            return TreeSelectorInput::Handled;
        }

        if kbm.matches(event, "tui.select.pageDown") || matches_key(event, "pageDown") {
            if !self.visible_nodes.is_empty() {
                self.selected = (self.selected + PAGE_SIZE).min(self.visible_nodes.len() - 1);
                self.last_selected_id = Some(self.visible_nodes[self.selected].entry_id.clone());
            }
            return TreeSelectorInput::Handled;
        }

        if kbm.matches(event, "tui.select.pageUp") || matches_key(event, "pageUp") {
            if !self.visible_nodes.is_empty() {
                self.selected = self.selected.saturating_sub(PAGE_SIZE);
                self.last_selected_id = Some(self.visible_nodes[self.selected].entry_id.clone());
            }
            return TreeSelectorInput::Handled;
        }

        if kbm.matches(event, "tui.select.confirm") || matches_key(event, "enter") {
            if self.visible_nodes.is_empty() {
                return TreeSelectorInput::Handled;
            }
            let entry_id = self.visible_nodes[self.selected].entry_id.clone();
            return TreeSelectorInput::Confirm(Some(entry_id));
        }

        // Cancel: Esc (if no search text) or Backspace (clear search char)
        if kbm.matches(event, "tui.select.cancel") || matches_key(event, "escape") {
            if !self.search_query.is_empty() {
                self.search_query.clear();
                self.rebuild();
                return TreeSelectorInput::Handled;
            }
            return TreeSelectorInput::Cancel;
        }

        // Backspace in search: remove last character
        if matches_key(event, "backspace") {
            if !self.search_query.is_empty() {
                self.search_query.pop();
                self.rebuild();
            }
            return TreeSelectorInput::Handled;
        }

        // Fold/unfold
        if kbm.matches(event, "app.tree.foldOrUp") {
            if !self.visible_nodes.is_empty() {
                let node = &self.visible_nodes[self.selected];
                if node.is_foldable && !node.is_folded {
                    self.folded_nodes.insert(node.entry_id.clone());
                    self.rebuild();
                    return TreeSelectorInput::Handled;
                }
            }
            // Otherwise: branch up (find parent)
            if !self.visible_nodes.is_empty() {
                self.selected = self.find_branch_segment_start(BranchDirection::Up);
                self.last_selected_id = Some(self.visible_nodes[self.selected].entry_id.clone());
            }
            return TreeSelectorInput::Handled;
        }

        if kbm.matches(event, "app.tree.unfoldOrDown") {
            if !self.visible_nodes.is_empty() {
                let node = &self.visible_nodes[self.selected];
                if node.is_folded {
                    self.folded_nodes.remove(&node.entry_id);
                    self.rebuild();
                    return TreeSelectorInput::Handled;
                }
            }
            // Branch down: go to first child
            if !self.visible_nodes.is_empty() {
                self.selected = self.find_branch_segment_start(BranchDirection::Down);
                self.last_selected_id = Some(self.visible_nodes[self.selected].entry_id.clone());
            }
            return TreeSelectorInput::Handled;
        }

        // Label editing
        if kbm.matches(event, "app.tree.editLabel") || is_plain_char(event, "L") {
            if !self.visible_nodes.is_empty() {
                let node = &self.visible_nodes[self.selected];
                let current_label = node.label.clone();
                self.editing_label = true;
                self.editing_label_entry_id = Some(node.entry_id.clone());
                self.label_input = current_label.clone().unwrap_or_default();
                return TreeSelectorInput::EditLabel {
                    entry_id: node.entry_id.clone(),
                    current_label,
                };
            }
            return TreeSelectorInput::Handled;
        }

        // Toggle label timestamp
        if kbm.matches(event, "app.tree.toggleLabelTimestamp") || is_plain_char(event, "T") {
            self.show_label_timestamps = !self.show_label_timestamps;
            return TreeSelectorInput::Handled;
        }

        // Filter switching
        if kbm.matches(event, "app.tree.filter.default") {
            self.filter_mode = TreeFilterMode::Default;
            self.folded_nodes.clear();
            self.rebuild();
            return TreeSelectorInput::Handled;
        }
        if kbm.matches(event, "app.tree.filter.noTools") {
            self.set_filter(TreeFilterMode::NoTools);
            return TreeSelectorInput::Handled;
        }
        if kbm.matches(event, "app.tree.filter.userOnly") {
            self.set_filter(TreeFilterMode::UserOnly);
            return TreeSelectorInput::Handled;
        }
        if kbm.matches(event, "app.tree.filter.labeledOnly") {
            self.set_filter(TreeFilterMode::LabeledOnly);
            return TreeSelectorInput::Handled;
        }
        if kbm.matches(event, "app.tree.filter.all") {
            self.set_filter(TreeFilterMode::All);
            return TreeSelectorInput::Handled;
        }
        if kbm.matches(event, "app.tree.filter.cycleForward") {
            self.cycle_filter(true);
            return TreeSelectorInput::Handled;
        }
        if kbm.matches(event, "app.tree.filter.cycleBackward") {
            self.cycle_filter(false);
            return TreeSelectorInput::Handled;
        }

        // Search mode: plain printable characters accumulate into the search
        // query. This must run after keybindings so Ctrl+D/Ctrl+U/Shift+L/etc.
        // are not swallowed as search text.
        if let InputEvent::Key(ke) = event {
            match &ke.key {
                Key::Char(ch) if ke.modifiers.is_empty() => {
                    self.search_query.push_str(ch);
                    self.folded_nodes.clear();
                    self.rebuild();
                    return TreeSelectorInput::Handled;
                }
                Key::Space if ke.modifiers.is_empty() => {
                    self.search_query.push(' ');
                    self.folded_nodes.clear();
                    self.rebuild();
                    return TreeSelectorInput::Handled;
                }
                _ => {}
            }
        }

        TreeSelectorInput::Handled
    }

    fn handle_label_input(
        &mut self,
        kbm: &KeybindingsManager,
        event: &InputEvent,
    ) -> TreeSelectorInput {
        if kbm.matches(event, "tui.select.confirm") || matches_key(event, "enter") {
            let entry_id = self.editing_label_entry_id.take();
            self.editing_label = false;
            let label = self.label_input.clone();
            self.label_input.clear();
            if let Some(eid) = entry_id {
                let label_value = if label.is_empty() { None } else { Some(label) };
                return TreeSelectorInput::SaveLabel {
                    entry_id: eid,
                    label: label_value,
                };
            }
            return TreeSelectorInput::Handled;
        }

        if kbm.matches(event, "tui.select.cancel") || matches_key(event, "escape") {
            self.editing_label = false;
            self.editing_label_entry_id = None;
            self.label_input.clear();
            return TreeSelectorInput::Handled;
        }

        if matches_key(event, "backspace") {
            self.label_input.pop();
            return TreeSelectorInput::Handled;
        }

        // Typing adds to label.
        if let InputEvent::Key(ke) = event
            && let Key::Char(ch) = &ke.key
        {
            self.label_input.push_str(ch);
            return TreeSelectorInput::Handled;
        }

        TreeSelectorInput::Handled
    }

    fn set_filter(&mut self, mode: TreeFilterMode) {
        self.filter_mode = mode;
        self.folded_nodes.clear();
        // Save current selection before rebuilding
        if !self.visible_nodes.is_empty() {
            self.last_selected_id = Some(self.visible_nodes[self.selected].entry_id.clone());
        }
        self.rebuild();
    }

    fn cycle_filter(&mut self, forward: bool) {
        let modes = [
            TreeFilterMode::Default,
            TreeFilterMode::NoTools,
            TreeFilterMode::UserOnly,
            TreeFilterMode::LabeledOnly,
            TreeFilterMode::All,
        ];
        let current = modes
            .iter()
            .position(|m| *m == self.filter_mode)
            .unwrap_or(0);
        let next = if forward {
            (current + 1) % modes.len()
        } else {
            (current + modes.len() - 1) % modes.len()
        };
        self.set_filter(modes[next]);
    }

    fn find_branch_segment_start(&self, direction: BranchDirection) -> usize {
        let Some(selected_node) = self.visible_nodes.get(self.selected) else {
            return self.selected;
        };
        let index_by_entry_id: BTreeMap<String, usize> = self
            .visible_nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.entry_id.clone(), index))
            .collect();

        let mut current_id = selected_node.entry_id.clone();
        match direction {
            BranchDirection::Down => loop {
                let children = self
                    .visible_children_map
                    .get(&Some(current_id.clone()))
                    .cloned()
                    .unwrap_or_default();
                if children.is_empty() {
                    return index_by_entry_id
                        .get(&current_id)
                        .copied()
                        .unwrap_or(self.selected);
                }
                if children.len() > 1 {
                    return index_by_entry_id
                        .get(&children[0])
                        .copied()
                        .unwrap_or(self.selected);
                }
                current_id = children[0].clone();
            },
            BranchDirection::Up => loop {
                let parent_id = self.visible_parent_map.get(&current_id).cloned().flatten();
                let Some(parent_id) = parent_id else {
                    return index_by_entry_id
                        .get(&current_id)
                        .copied()
                        .unwrap_or(self.selected);
                };
                let children = self
                    .visible_children_map
                    .get(&Some(parent_id.clone()))
                    .cloned()
                    .unwrap_or_default();
                if children.len() > 1 {
                    let segment_start = index_by_entry_id
                        .get(&current_id)
                        .copied()
                        .unwrap_or(self.selected);
                    if segment_start < self.selected {
                        return segment_start;
                    }
                }
                current_id = parent_id;
            },
        }
    }

    /// Get the selected node's entry id, if any.
    pub(super) fn selected_entry_id(&self) -> Option<String> {
        self.visible_nodes
            .get(self.selected)
            .map(|n| n.entry_id.clone())
    }

    /// Update a node's label in the flat representation.
    pub(super) fn update_node_label(
        &mut self,
        entry_id: &str,
        label: Option<String>,
        timestamp: Option<String>,
    ) {
        fn update_tree_node(
            nodes: &mut [CodingAgentSessionTreeNode],
            entry_id: &str,
            label: &Option<String>,
            timestamp: &Option<String>,
        ) -> bool {
            for node in nodes {
                if node.entry_id == entry_id {
                    node.label = label.clone();
                    node.label_timestamp = timestamp.clone();
                    return true;
                }
                if update_tree_node(&mut node.children, entry_id, label, timestamp) {
                    return true;
                }
            }
            false
        }

        update_tree_node(&mut self.tree, entry_id, &label, &timestamp);
        self.rebuild();
    }

    /// Render the tree selector as a vector of strings.
    pub(super) fn render(&self, width: usize) -> Vec<String> {
        if width < 10 {
            return vec!["Tree".to_string()];
        }
        let color = color_enabled();
        let mut lines: Vec<String> = Vec::new();

        // Title line
        lines.push(fit_line(&paint_with("Session Tree", &USER, color), width));

        // Help line
        let help = "Up/Down move · PgUp/PgDn page · Ctrl+Left/Right branch · Shift+L label · Ctrl+D/T/U/L/A filter · Ctrl+O cycle";
        let help_display = if visible_width(help) > width {
            truncate_to_width_with_ellipsis(help, width)
        } else {
            help.to_string()
        };
        lines.push(fit_line(&paint_with(&help_display, &SYSTEM, color), width));

        // Search line
        let search_display = if self.search_query.is_empty() {
            "Type to search:".to_string()
        } else {
            format!("Search: {}", self.search_query)
        };
        lines.push(fit_line(
            &paint_with(
                &truncate_to_width_with_ellipsis(&search_display, width),
                &SYSTEM,
                color,
            ),
            width,
        ));

        // Label editing input
        if self.editing_label {
            let label_prompt = format!("Label: {}", self.label_input);
            lines.push(fit_line(
                &paint_with(
                    &truncate_to_width_with_ellipsis(&label_prompt, width),
                    &USER,
                    color,
                ),
                width,
            ));
        }

        // Tree rows
        if self.visible_nodes.is_empty() {
            lines.push(fit_line(
                &paint_with("(no entries match filter)", &SYSTEM, color),
                width,
            ));
        } else {
            let page_start = self.selected.saturating_sub(PAGE_SIZE / 2);
            let page_end = (page_start + PAGE_SIZE).min(self.visible_nodes.len());
            for i in page_start..page_end {
                let node = &self.visible_nodes[i];
                let is_selected = i == self.selected;
                let row = render_tree_row(
                    node,
                    is_selected,
                    self.multiple_roots,
                    width,
                    self.show_label_timestamps,
                );
                if is_selected {
                    lines.push(fit_line(&paint_with(&row, &USER, color), width));
                } else {
                    lines.push(fit_line(&row, width));
                }
            }
        }

        // Status line
        let total = self.visible_nodes.len();
        let filter_name = self.filter_mode.to_string();
        let status = if total > 0 {
            format!(
                "({}/{}) [{}]{}",
                self.selected + 1,
                total,
                filter_name,
                if self.show_label_timestamps {
                    " [+label time]"
                } else {
                    ""
                }
            )
        } else {
            format!("(0/0) [{}]", filter_name)
        };
        lines.push(fit_line(
            &paint_with(
                &truncate_to_width_with_ellipsis(&status, width),
                &SYSTEM,
                color,
            ),
            width,
        ));

        lines
    }
}
