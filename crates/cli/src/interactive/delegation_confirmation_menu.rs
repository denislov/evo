use tui::api::input::{InputEvent, Key, KeyEventKind, KeyModifiers, KeybindingsManager};
use tui::api::render::{SYSTEM, USER, color_enabled, paint_with};

use crate::interactive::render::fit_line;
use coding_agent::api::operation::PendingDelegationConfirmation;
use coding_agent::api::view::ProfileKind;

const MAX_VISIBLE_CONFIRMATIONS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DelegationConfirmationMenuRenderState {
    selected: usize,
    pending_len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DelegationConfirmationMenuState {
    pending: Vec<PendingDelegationConfirmation>,
    selected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DelegationConfirmationMenuOutcome {
    None,
    Close,
    Approve {
        operation_id: String,
        tool_call_id: String,
    },
    Reject {
        operation_id: String,
        tool_call_id: String,
    },
    RejectWithReason {
        operation_id: String,
        tool_call_id: String,
    },
}

impl DelegationConfirmationMenuState {
    pub(super) fn new(pending: Vec<PendingDelegationConfirmation>) -> Self {
        Self {
            pending,
            selected: 0,
        }
    }

    pub(super) fn render_state(&self) -> DelegationConfirmationMenuRenderState {
        DelegationConfirmationMenuRenderState {
            selected: self.selected,
            pending_len: self.pending.len(),
        }
    }

    pub(super) fn upsert(&mut self, pending: PendingDelegationConfirmation) {
        if let Some(existing) = self.pending.iter_mut().find(|existing| {
            existing.operation_id == pending.operation_id
                && existing.tool_call_id == pending.tool_call_id
        }) {
            *existing = pending;
        } else {
            self.pending.push(pending);
        }
        self.selected = self.selected.min(self.pending.len().saturating_sub(1));
    }

    pub(super) fn remove(&mut self, operation_id: &str, tool_call_id: &str) {
        self.pending.retain(|pending| {
            pending.operation_id != operation_id || pending.tool_call_id != tool_call_id
        });
        self.selected = self.selected.min(self.pending.len().saturating_sub(1));
    }

    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub(super) fn render(&mut self, width: usize) -> Vec<String> {
        let color = color_enabled();
        self.selected = self.selected.min(self.pending.len().saturating_sub(1));
        let mut lines = vec![fit_line("Delegation confirmations", width)];
        if self.pending.is_empty() {
            lines.push(fit_line(
                &paint_with("  No pending delegation confirmations", &SYSTEM, color),
                width,
            ));
            lines.push(fit_line(&paint_with("Esc close", &SYSTEM, color), width));
            return lines;
        }

        let window_start = self
            .selected
            .saturating_add(1)
            .saturating_sub(MAX_VISIBLE_CONFIRMATIONS);
        for (visible_offset, item) in self
            .pending
            .iter()
            .skip(window_start)
            .take(MAX_VISIBLE_CONFIRMATIONS)
            .enumerate()
        {
            let absolute_index = window_start + visible_offset;
            let marker = if absolute_index == self.selected {
                "->"
            } else {
                "  "
            };
            let summary = format!(
                "{marker} {} {} requested by {}",
                profile_kind_label(item.target_kind),
                item.target_id,
                item.requesting_profile_id
            );
            if absolute_index == self.selected {
                lines.push(fit_line(&paint_with(&summary, &USER, color), width));
            } else {
                lines.push(fit_line(&summary, width));
            }
            lines.push(fit_line(&format!("   task: {}", item.task), width));
            lines.push(fit_line(&format!("   reason: {}", item.reason), width));
            lines.push(fit_line(
                &format!("   ids: {} {}", item.operation_id, item.tool_call_id),
                width,
            ));
        }
        lines.push(fit_line(
            &paint_with(
                &format!(
                    "({}/{}) Enter/a approve · r reject · R reject with reason · Esc close",
                    self.selected + 1,
                    self.pending.len()
                ),
                &SYSTEM,
                color,
            ),
            width,
        ));
        lines
    }

    pub(super) fn handle_input(
        &mut self,
        keybindings: &KeybindingsManager,
        event: &InputEvent,
    ) -> DelegationConfirmationMenuOutcome {
        let InputEvent::Key(key_event) = event else {
            return DelegationConfirmationMenuOutcome::None;
        };
        if key_event.kind == KeyEventKind::Release {
            return DelegationConfirmationMenuOutcome::None;
        }

        if keybindings.matches(event, "tui.select.cancel") || keybindings.matches(event, "ctrl+c") {
            return DelegationConfirmationMenuOutcome::Close;
        }
        if keybindings.matches(event, "tui.select.up") {
            if !self.pending.is_empty() {
                self.selected = (self.selected + self.pending.len() - 1) % self.pending.len();
            }
            return DelegationConfirmationMenuOutcome::None;
        }
        if keybindings.matches(event, "tui.select.down") {
            if !self.pending.is_empty() {
                self.selected = (self.selected + 1) % self.pending.len();
            }
            return DelegationConfirmationMenuOutcome::None;
        }
        if keybindings.matches(event, "tui.select.pageUp") {
            self.selected = self.selected.saturating_sub(MAX_VISIBLE_CONFIRMATIONS);
            return DelegationConfirmationMenuOutcome::None;
        }
        if keybindings.matches(event, "tui.select.pageDown") {
            self.selected = (self.selected + MAX_VISIBLE_CONFIRMATIONS)
                .min(self.pending.len().saturating_sub(1));
            return DelegationConfirmationMenuOutcome::None;
        }
        if keybindings.matches(event, "tui.select.confirm") {
            return self.selected_outcome(true);
        }

        match key_from_event(event) {
            Some(Key::Char(text)) if text.eq_ignore_ascii_case("a") => self.selected_outcome(true),
            Some(Key::Char(text)) if text == "R" => self.selected_reject_with_reason_outcome(),
            Some(Key::Char(text)) if text == "r" => self.selected_outcome(false),
            _ => DelegationConfirmationMenuOutcome::None,
        }
    }

    fn selected_outcome(&self, approve: bool) -> DelegationConfirmationMenuOutcome {
        let Some(item) = self.pending.get(self.selected) else {
            return DelegationConfirmationMenuOutcome::None;
        };
        if approve {
            DelegationConfirmationMenuOutcome::Approve {
                operation_id: item.operation_id.clone(),
                tool_call_id: item.tool_call_id.clone(),
            }
        } else {
            DelegationConfirmationMenuOutcome::Reject {
                operation_id: item.operation_id.clone(),
                tool_call_id: item.tool_call_id.clone(),
            }
        }
    }

    fn selected_reject_with_reason_outcome(&self) -> DelegationConfirmationMenuOutcome {
        let Some(item) = self.pending.get(self.selected) else {
            return DelegationConfirmationMenuOutcome::None;
        };
        DelegationConfirmationMenuOutcome::RejectWithReason {
            operation_id: item.operation_id.clone(),
            tool_call_id: item.tool_call_id.clone(),
        }
    }
}

fn key_from_event(event: &InputEvent) -> Option<&Key> {
    let InputEvent::Key(key_event) = event else {
        return None;
    };
    if key_event
        .modifiers
        .intersects(KeyModifiers::CTRL | KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        return None;
    }
    Some(&key_event.key)
}

fn profile_kind_label(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Agent => "agent",
        ProfileKind::Team => "team",
    }
}
