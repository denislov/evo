use tui::api::input::{
    InputEvent, Key, KeyEventKind, KeyModifiers, KeybindingsManager, fuzzy_filter_indices,
};
use tui::api::render::{SYSTEM, USER, color_enabled, paint_with};

use crate::interactive::render::fit_line;
use coding_agent::api::embedding::CodingAgentProfileCatalog;
use coding_agent::api::view::ProfileId;

const MAX_PROFILE_CHOICES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProfileMenuScreen {
    AgentRoot,
    TeamRoot,
    AgentInfo,
    TeamInfo,
    AgentUse,
    AgentRun,
    TeamRun,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProfileMenuRenderState {
    screen: ProfileMenuScreen,
    selected: usize,
    query: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PendingProfileTask {
    Agent { profile_id: ProfileId },
    Team { team_id: ProfileId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProfileMenuState {
    screen: ProfileMenuScreen,
    selected: usize,
    query: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProfileMenuOutcome {
    None,
    Close,
    SetDefaultAgent(ProfileId),
    BeginAgentTask(ProfileId),
    BeginTeamTask(ProfileId),
}

#[derive(Debug, Clone)]
struct ProfileMenuItem {
    id: ProfileId,
    title: String,
    detail: String,
}

impl ProfileMenuState {
    pub(super) fn agent() -> Self {
        Self {
            screen: ProfileMenuScreen::AgentRoot,
            selected: 0,
            query: String::new(),
        }
    }

    pub(super) fn team() -> Self {
        Self {
            screen: ProfileMenuScreen::TeamRoot,
            selected: 0,
            query: String::new(),
        }
    }

    pub(super) fn render_state(&self) -> ProfileMenuRenderState {
        ProfileMenuRenderState {
            screen: self.screen,
            selected: self.selected,
            query: self.query.clone(),
        }
    }

    pub(super) fn render(
        &mut self,
        catalog: &CodingAgentProfileCatalog,
        default_agent_profile_id: &ProfileId,
        width: usize,
    ) -> Vec<String> {
        match self.screen {
            ProfileMenuScreen::AgentRoot => self.render_action_menu(
                "Agent",
                &[
                    ("Info", "show discovered agent profiles"),
                    ("Use", "set the default agent profile"),
                    ("Run", "run a one-off agent task"),
                ],
                width,
            ),
            ProfileMenuScreen::TeamRoot => self.render_action_menu(
                "Team",
                &[
                    ("Info", "show discovered team profiles"),
                    ("Run", "run a supervised team task"),
                ],
                width,
            ),
            ProfileMenuScreen::AgentInfo => {
                render_agent_info(catalog, default_agent_profile_id, width)
            }
            ProfileMenuScreen::TeamInfo => render_team_info(catalog, width),
            ProfileMenuScreen::AgentUse => self.render_selector(
                "Select agent to use",
                agent_items(catalog, default_agent_profile_id),
                width,
            ),
            ProfileMenuScreen::AgentRun => self.render_selector(
                "Select agent to run",
                agent_items(catalog, default_agent_profile_id),
                width,
            ),
            ProfileMenuScreen::TeamRun => {
                self.render_selector("Select team to run", team_items(catalog), width)
            }
        }
    }

    pub(super) fn handle_input(
        &mut self,
        keybindings: &KeybindingsManager,
        event: &InputEvent,
        catalog: &CodingAgentProfileCatalog,
        default_agent_profile_id: &ProfileId,
    ) -> ProfileMenuOutcome {
        let InputEvent::Key(key_event) = event else {
            return ProfileMenuOutcome::None;
        };
        if key_event.kind == KeyEventKind::Release {
            return ProfileMenuOutcome::None;
        }

        if keybindings.matches(event, "tui.select.cancel") || keybindings.matches(event, "ctrl+c") {
            return match self.screen {
                ProfileMenuScreen::AgentRoot | ProfileMenuScreen::TeamRoot => {
                    ProfileMenuOutcome::Close
                }
                ProfileMenuScreen::AgentInfo
                | ProfileMenuScreen::AgentUse
                | ProfileMenuScreen::AgentRun => {
                    self.open_screen(ProfileMenuScreen::AgentRoot);
                    ProfileMenuOutcome::None
                }
                ProfileMenuScreen::TeamInfo | ProfileMenuScreen::TeamRun => {
                    self.open_screen(ProfileMenuScreen::TeamRoot);
                    ProfileMenuOutcome::None
                }
            };
        }

        match self.screen {
            ProfileMenuScreen::AgentRoot => {
                self.handle_action_menu_input(keybindings, event, 3, |state, selected| {
                    match selected {
                        0 => {
                            state.open_screen(ProfileMenuScreen::AgentInfo);
                            ProfileMenuOutcome::None
                        }
                        1 => {
                            state.open_screen(ProfileMenuScreen::AgentUse);
                            ProfileMenuOutcome::None
                        }
                        2 => {
                            state.open_screen(ProfileMenuScreen::AgentRun);
                            ProfileMenuOutcome::None
                        }
                        _ => ProfileMenuOutcome::None,
                    }
                })
            }
            ProfileMenuScreen::TeamRoot => {
                self.handle_action_menu_input(keybindings, event, 2, |state, selected| {
                    match selected {
                        0 => {
                            state.open_screen(ProfileMenuScreen::TeamInfo);
                            ProfileMenuOutcome::None
                        }
                        1 => {
                            state.open_screen(ProfileMenuScreen::TeamRun);
                            ProfileMenuOutcome::None
                        }
                        _ => ProfileMenuOutcome::None,
                    }
                })
            }
            ProfileMenuScreen::AgentInfo | ProfileMenuScreen::TeamInfo => {
                if keybindings.matches(event, "tui.select.confirm") {
                    match self.screen {
                        ProfileMenuScreen::AgentInfo => {
                            self.open_screen(ProfileMenuScreen::AgentRoot)
                        }
                        ProfileMenuScreen::TeamInfo => {
                            self.open_screen(ProfileMenuScreen::TeamRoot)
                        }
                        _ => {}
                    }
                }
                ProfileMenuOutcome::None
            }
            ProfileMenuScreen::AgentUse => self.handle_selector_input(
                keybindings,
                event,
                agent_items(catalog, default_agent_profile_id),
                ProfileMenuOutcome::SetDefaultAgent,
            ),
            ProfileMenuScreen::AgentRun => self.handle_selector_input(
                keybindings,
                event,
                agent_items(catalog, default_agent_profile_id),
                ProfileMenuOutcome::BeginAgentTask,
            ),
            ProfileMenuScreen::TeamRun => self.handle_selector_input(
                keybindings,
                event,
                team_items(catalog),
                ProfileMenuOutcome::BeginTeamTask,
            ),
        }
    }

    fn render_action_menu(
        &mut self,
        title: &str,
        actions: &[(&str, &str)],
        width: usize,
    ) -> Vec<String> {
        self.selected = self.selected.min(actions.len().saturating_sub(1));
        let color = color_enabled();
        let mut lines = vec![fit_line(title, width)];
        for (index, (label, description)) in actions.iter().enumerate() {
            let marker = if index == self.selected { "->" } else { "  " };
            let line = format!("{marker} {label:<5} {description}");
            if index == self.selected {
                lines.push(fit_line(&paint_with(&line, &USER, color), width));
            } else {
                lines.push(fit_line(&line, width));
            }
        }
        lines.push(fit_line(
            &paint_with("Enter select · Esc close", &SYSTEM, color),
            width,
        ));
        lines
    }

    fn render_selector(
        &mut self,
        title: &str,
        items: Vec<ProfileMenuItem>,
        width: usize,
    ) -> Vec<String> {
        let indices = selection_indices(&items, &self.query);
        self.selected = self.selected.min(indices.len().saturating_sub(1));
        let color = color_enabled();
        let mut lines = vec![fit_line(title, width)];
        if !self.query.is_empty() {
            lines.push(fit_line(
                &paint_with(&format!("Filter: {}", self.query), &SYSTEM, color),
                width,
            ));
        }
        if indices.is_empty() {
            lines.push(fit_line(
                &paint_with("  No matching profiles", &SYSTEM, color),
                width,
            ));
            lines.push(fit_line(&paint_with("  Esc back", &SYSTEM, color), width));
            return lines;
        }

        let window_start = self
            .selected
            .saturating_add(1)
            .saturating_sub(MAX_PROFILE_CHOICES);
        for (visible_offset, item_index) in indices
            .iter()
            .copied()
            .skip(window_start)
            .take(MAX_PROFILE_CHOICES)
            .enumerate()
        {
            let absolute_index = window_start + visible_offset;
            let item = &items[item_index];
            let marker = if absolute_index == self.selected {
                "->"
            } else {
                "  "
            };
            let line = if item.detail.is_empty() {
                format!("{marker} {:<18} {}", item.id, item.title)
            } else {
                format!("{marker} {:<18} {} · {}", item.id, item.title, item.detail)
            };
            if absolute_index == self.selected {
                lines.push(fit_line(&paint_with(&line, &USER, color), width));
            } else {
                lines.push(fit_line(&line, width));
            }
        }
        lines.push(fit_line(
            &paint_with(
                &format!(
                    "({}/{}) Enter select · Esc back",
                    self.selected + 1,
                    indices.len()
                ),
                &SYSTEM,
                color,
            ),
            width,
        ));
        lines
    }

    fn handle_action_menu_input<F>(
        &mut self,
        keybindings: &KeybindingsManager,
        event: &InputEvent,
        len: usize,
        confirm: F,
    ) -> ProfileMenuOutcome
    where
        F: FnOnce(&mut Self, usize) -> ProfileMenuOutcome,
    {
        if len == 0 {
            return ProfileMenuOutcome::None;
        }
        if keybindings.matches(event, "tui.select.up") {
            self.selected = (self.selected + len - 1) % len;
            return ProfileMenuOutcome::None;
        }
        if keybindings.matches(event, "tui.select.down") {
            self.selected = (self.selected + 1) % len;
            return ProfileMenuOutcome::None;
        }
        if keybindings.matches(event, "tui.select.confirm") {
            return confirm(self, self.selected.min(len - 1));
        }
        ProfileMenuOutcome::None
    }

    fn handle_selector_input<F>(
        &mut self,
        keybindings: &KeybindingsManager,
        event: &InputEvent,
        items: Vec<ProfileMenuItem>,
        confirm: F,
    ) -> ProfileMenuOutcome
    where
        F: FnOnce(ProfileId) -> ProfileMenuOutcome,
    {
        let indices = selection_indices(&items, &self.query);
        if keybindings.matches(event, "tui.select.up") {
            if !indices.is_empty() {
                self.selected = (self.selected + indices.len() - 1) % indices.len();
            }
            return ProfileMenuOutcome::None;
        }
        if keybindings.matches(event, "tui.select.down") {
            if !indices.is_empty() {
                self.selected = (self.selected + 1) % indices.len();
            }
            return ProfileMenuOutcome::None;
        }
        if keybindings.matches(event, "tui.select.pageUp") {
            self.selected = self.selected.saturating_sub(MAX_PROFILE_CHOICES);
            return ProfileMenuOutcome::None;
        }
        if keybindings.matches(event, "tui.select.pageDown") {
            self.selected =
                (self.selected + MAX_PROFILE_CHOICES).min(indices.len().saturating_sub(1));
            return ProfileMenuOutcome::None;
        }
        if keybindings.matches(event, "tui.select.confirm") {
            if let Some(item_index) = indices.get(self.selected).copied() {
                return confirm(items[item_index].id.clone());
            }
            return ProfileMenuOutcome::None;
        }

        match key_from_event(event) {
            Some(Key::Backspace) => {
                self.query.pop();
                self.selected = 0;
            }
            Some(Key::Delete) => {
                self.query.clear();
                self.selected = 0;
            }
            Some(Key::Space) => {
                self.query.push(' ');
                self.selected = 0;
            }
            Some(Key::Char(text)) => {
                self.query.push_str(text);
                self.selected = 0;
            }
            _ => {}
        }
        ProfileMenuOutcome::None
    }

    fn open_screen(&mut self, screen: ProfileMenuScreen) {
        self.screen = screen;
        self.selected = 0;
        self.query.clear();
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

fn selection_indices(items: &[ProfileMenuItem], query: &str) -> Vec<usize> {
    fuzzy_filter_indices(items, query, |item| {
        format!("{} {} {}", item.id, item.title, item.detail)
    })
}

fn agent_items(
    catalog: &CodingAgentProfileCatalog,
    default_agent_profile_id: &ProfileId,
) -> Vec<ProfileMenuItem> {
    catalog
        .agents
        .iter()
        .map(|profile| {
            let mut detail_parts = Vec::new();
            if profile.id == *default_agent_profile_id {
                detail_parts.push("current".to_string());
            }
            if let Some(model) = &profile.model_id {
                detail_parts.push(format!("model {model}"));
            }
            if !profile.tools.is_empty() {
                detail_parts.push(format!("tools {}", profile.tools.join(",")));
            }
            ProfileMenuItem {
                id: profile.id.clone(),
                title: profile.display_name.clone(),
                detail: detail_parts.join(" · "),
            }
        })
        .collect()
}

fn team_items(catalog: &CodingAgentProfileCatalog) -> Vec<ProfileMenuItem> {
    catalog
        .teams
        .iter()
        .map(|profile| ProfileMenuItem {
            id: profile.id.clone(),
            title: profile.display_name.clone(),
            detail: format!(
                "{:?} · {} member(s)",
                profile.supervisor,
                profile.members.len()
            ),
        })
        .collect()
}

fn render_agent_info(
    catalog: &CodingAgentProfileCatalog,
    default_agent_profile_id: &ProfileId,
    width: usize,
) -> Vec<String> {
    let color = color_enabled();
    let mut lines = vec![fit_line("Agent profiles:", width)];
    let mut count = 0usize;
    for profile in &catalog.agents {
        count += 1;
        let current = if profile.id == *default_agent_profile_id {
            " (current)"
        } else {
            ""
        };
        let description = profile
            .description
            .as_deref()
            .unwrap_or(profile.display_name.as_str());
        lines.push(fit_line(
            &format!("  {}{current} - {description}", profile.id),
            width,
        ));
        lines.push(fit_line(&format!("    {:?}", profile.source), width));
    }
    if count == 0 {
        lines.push(fit_line("  (none)", width));
    }
    for diagnostic in &catalog.diagnostics {
        lines.push(fit_line(
            &format!("  diagnostic: {}", diagnostic.summary),
            width,
        ));
    }
    lines.push(fit_line(
        &paint_with("Enter/Esc back", &SYSTEM, color),
        width,
    ));
    lines
}

fn render_team_info(catalog: &CodingAgentProfileCatalog, width: usize) -> Vec<String> {
    let color = color_enabled();
    let mut lines = vec![fit_line("Team profiles:", width)];
    let mut count = 0usize;
    for profile in &catalog.teams {
        count += 1;
        let description = profile
            .description
            .as_deref()
            .unwrap_or(profile.display_name.as_str());
        let members = profile
            .members
            .iter()
            .map(ProfileId::as_str)
            .collect::<Vec<_>>()
            .join(",");
        lines.push(fit_line(
            &format!(
                "  {} - {description} ({:?})",
                profile.id, profile.supervisor
            ),
            width,
        ));
        lines.push(fit_line(&format!("    members: {members}"), width));
    }
    if count == 0 {
        lines.push(fit_line("  (none)", width));
    }
    for diagnostic in &catalog.diagnostics {
        lines.push(fit_line(
            &format!("  diagnostic: {}", diagnostic.summary),
            width,
        ));
    }
    lines.push(fit_line(
        &paint_with("Enter/Esc back", &SYSTEM, color),
        width,
    ));
    lines
}
