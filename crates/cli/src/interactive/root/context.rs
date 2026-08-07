use super::*;

impl InteractiveRoot {
    pub(super) fn render_context_region(&mut self, width: usize, height: usize) -> Vec<String> {
        if width == 0 || height == 0 {
            return Vec::new();
        }
        let active_tab_style = self
            .semantic_style(CodingAgentThemeForeground::Accent, USER)
            .bold();
        let inactive_tab_style = self.semantic_style(CodingAgentThemeForeground::Muted, SYSTEM);
        let tabs = visible_context_tabs(width, self.local.context_tab)
            .into_iter()
            .map(|(tab, label)| {
                if tab == self.local.context_tab {
                    paint_with(&format!("[{label}]"), &active_tab_style, color_enabled())
                } else {
                    paint_with(label, &inactive_tab_style, color_enabled())
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let mut lines = vec![self.panel_header(
            &format!("Context {tabs}"),
            InteractiveRegion::Context,
            width,
        )];
        self.local.context_viewport_height = height.saturating_sub(1).max(1);
        let body = if self.local.context_tab == ContextTab::Usage {
            self.context_usage_lines(width)
        } else {
            self.context_list_lines()
        }
        .into_iter()
        .map(|line| self.style_context_body_line(line))
        .collect::<Vec<_>>();
        let scroll = self.local.context_scroll[self.local.context_tab.index()].min(
            body.len()
                .saturating_sub(self.local.context_viewport_height),
        );
        self.local.context_scroll[self.local.context_tab.index()] = scroll;
        lines.extend(
            body.into_iter()
                .skip(scroll)
                .take(self.local.context_viewport_height),
        );
        lines.truncate(height.max(1));
        lines
            .into_iter()
            .map(|line| fit_line(&line, width))
            .collect()
    }

    pub(super) fn style_context_body_line(&self, line: String) -> String {
        if line.is_empty() {
            return line;
        }
        let (token, fallback, bold) = if matches!(
            line.as_str(),
            "session totals" | "latest turn" | "context window"
        ) {
            (CodingAgentThemeForeground::Accent, USER, true)
        } else if line.starts_with('›') {
            (CodingAgentThemeForeground::Accent, USER, false)
        } else if line.starts_with("no ") || line.contains("unavailable") {
            (CodingAgentThemeForeground::Muted, SYSTEM, false)
        } else {
            (CodingAgentThemeForeground::Text, Style::default(), false)
        };
        let mut style = self.semantic_style(token, fallback);
        style.bold = bold;
        paint_with(&line, &style, color_enabled())
    }

    pub(super) fn context_list_lines(&mut self) -> Vec<String> {
        let items = self.context_items(self.local.context_tab);
        if items.is_empty() {
            return vec![
                match self.local.context_tab {
                    ContextTab::Ops => "no operations yet",
                    ContextTab::Changes => "no successful file changes yet",
                    ContextTab::Agents => "no agent inventory available",
                    ContextTab::Usage => "usage unavailable",
                }
                .into(),
            ];
        }
        let index = self.local.context_tab.index();
        self.local.context_selection[index] =
            self.local.context_selection[index].min(items.len() - 1);
        let selected = self.local.context_selection[index];
        let viewport = self.local.context_viewport_height.max(1);
        if selected < self.local.context_scroll[index] {
            self.local.context_scroll[index] = selected;
        } else if selected >= self.local.context_scroll[index].saturating_add(viewport) {
            self.local.context_scroll[index] = selected.saturating_add(1).saturating_sub(viewport);
        }
        items
            .into_iter()
            .enumerate()
            .map(|(item_index, item)| {
                let marker = if item_index == selected { "›" } else { " " };
                format!("{marker} {}", item.summary)
            })
            .collect()
    }

    pub(super) fn context_items(&self, tab: ContextTab) -> Vec<ContextListItem> {
        match tab {
            ContextTab::Ops => self
                .shared_projection
                .context()
                .operations
                .iter()
                .map(|operation| self.operation_context_item(operation))
                .collect(),
            ContextTab::Changes => self
                .shared_projection
                .context()
                .changes
                .iter()
                .map(|change| self.change_context_item(change))
                .collect(),
            ContextTab::Agents => self.agent_context_items(),
            ContextTab::Usage => Vec::new(),
        }
    }

    pub(super) fn operation_context_item(
        &self,
        operation: &CodingAgentOperationSnapshot,
    ) -> ContextListItem {
        let elapsed = self.operation_elapsed(operation);
        let cancellable = operation_status_is_running(operation.status)
            && self
                .shared_projection
                .context()
                .operations
                .iter()
                .find(|candidate| operation_status_is_running(candidate.status))
                .is_some_and(|candidate| candidate.operation_id == operation.operation_id)
            && self
                .shared_projection
                .capabilities()
                .is_some_and(|capabilities| {
                    matches!(capabilities.abort, CapabilityStatus::Available)
                });
        let cancel = if cancellable { " cancel" } else { "" };
        let summary = format!(
            "{:<9} {} {}{cancel}",
            operation_status_as_str(operation.status),
            operation.kind,
            elapsed
        );
        let mut detail_lines = vec![
            format!("kind: {}", operation.kind),
            format!("operation: {}", operation.operation_id),
            format!("status: {}", operation_status_as_str(operation.status)),
            format!("elapsed: {elapsed}"),
            format!(
                "cancel: {}",
                if cancellable {
                    "available"
                } else {
                    "unavailable"
                }
            ),
            format!(
                "parent: {}",
                operation.parent_operation_id.as_deref().unwrap_or("none")
            ),
            format!(
                "root: {}",
                operation.root_operation_id.as_deref().unwrap_or("none")
            ),
        ];
        if let Some(failure) = &operation.failure {
            detail_lines.push(format!("failure: {failure}"));
        }
        if operation.diagnostics.is_empty() {
            detail_lines.push("diagnostics: none".into());
        } else {
            detail_lines.push("diagnostics:".into());
            detail_lines.extend(
                operation
                    .diagnostics
                    .iter()
                    .map(|diagnostic| format!("- {diagnostic}")),
            );
        }
        ContextListItem {
            summary,
            detail_title: format!("Operation {}", short_id(&operation.operation_id)),
            detail_lines,
        }
    }

    pub(super) fn change_context_item(
        &self,
        change: &CodingAgentFileChangeSnapshot,
    ) -> ContextListItem {
        let stats = match (change.added_lines, change.removed_lines) {
            (Some(added), Some(removed)) => format!(" +{added}/-{removed}"),
            (Some(added), None) => format!(" +{added}"),
            (None, Some(removed)) => format!(" -{removed}"),
            (None, None) => String::new(),
        };
        let age = self
            .local
            .context_change_timing
            .get(&change.path)
            .map(|(_, seen_at)| Instant::now().saturating_duration_since(*seen_at));
        let updated = age.map_or_else(
            || format!("event #{}", change.updated_sequence),
            |age| {
                if age.as_secs() == 0 {
                    format!("event #{} · now", change.updated_sequence)
                } else {
                    format!(
                        "event #{} · {} ago",
                        change.updated_sequence,
                        format_duration(age)
                    )
                }
            },
        );
        let mut detail_lines = vec![
            format!("path: {}", change.path),
            format!("mutation: {}", change.mutation_kind),
            format!("operation: {}", change.operation_id),
            format!(
                "tool call: {}",
                change.tool_call_id.as_deref().unwrap_or("unavailable")
            ),
            format!("updated: {updated}"),
            format!(
                "first changed line: {}",
                change
                    .first_changed_line
                    .map(|line| line.to_string())
                    .unwrap_or_else(|| "unavailable".into())
            ),
            format!(
                "diff stats: {}",
                if stats.is_empty() {
                    "unavailable"
                } else {
                    stats.trim()
                }
            ),
        ];
        if let Some(diff) = &change.diff {
            detail_lines.push("diff:".into());
            detail_lines.extend(diff.lines().map(ToOwned::to_owned));
        } else {
            detail_lines.push("diff: unavailable".into());
        }
        ContextListItem {
            summary: format!(
                "{:<8} {}{} · {}",
                change.mutation_kind,
                abbreviate_path(&change.path, 18),
                stats,
                if age.is_some_and(|age| age.as_secs() == 0) {
                    "now".into()
                } else {
                    age.map(format_duration).unwrap_or_else(|| "--".into())
                }
            ),
            detail_title: format!("Change {}", abbreviate_path(&change.path, 40)),
            detail_lines,
        }
    }

    pub(super) fn operation_elapsed(&self, operation: &CodingAgentOperationSnapshot) -> String {
        self.shared_projection
            .operation_elapsed(operation)
            .map(format_duration)
            .unwrap_or_else(|| "--".into())
    }

    pub(super) fn agent_context_items(&self) -> Vec<ContextListItem> {
        let mut items = Vec::new();
        let default_agent_profile_id = self.display_default_agent_profile_id();
        let active = self
            .profile_catalog
            .agent(default_agent_profile_id.as_str());
        if let Some(profile) = active {
            let mut details = vec![
                format!("id: {}", profile.id),
                format!("name: {}", profile.display_name),
                format!(
                    "description: {}",
                    profile.description.as_deref().unwrap_or("unavailable")
                ),
                format!(
                    "model: {}",
                    profile.model_id.as_deref().unwrap_or("session default")
                ),
                format!(
                    "tools: {}",
                    nonempty_join(&profile.tools, "session defaults")
                ),
                format!("skills: {}", nonempty_join(&profile.skills, "none")),
                format!("max delegation depth: {}", profile.delegation.max_depth),
                format!(
                    "max parallel children: {}",
                    profile.delegation.max_parallel_children
                ),
            ];
            details.push(format!(
                "delegation: agents={} teams={}",
                profile.delegation.allow_agents, profile.delegation.allow_teams
            ));
            items.push(ContextListItem {
                summary: format!("active  {} · {}", profile.id, profile.display_name),
                detail_title: format!("Agent profile {}", profile.id),
                detail_lines: details,
            });

            if profile.delegation.allow_agents {
                for profile_id in &profile.delegation.agent_targets {
                    if let Some(target) = self.profile_catalog.agent(profile_id.as_str()) {
                        items.push(ContextListItem {
                            summary: format!("agent   {} · {}", target.id, target.display_name),
                            detail_title: format!("Delegation target {}", target.id),
                            detail_lines: vec![
                                "kind: agent".into(),
                                format!("id: {}", target.id),
                                format!("name: {}", target.display_name),
                                format!(
                                    "description: {}",
                                    target.description.as_deref().unwrap_or("unavailable")
                                ),
                                format!(
                                    "tools: {}",
                                    nonempty_join(&target.tools, "session defaults")
                                ),
                                format!("skills: {}", nonempty_join(&target.skills, "none")),
                            ],
                        });
                    }
                }
            }
            if profile.delegation.allow_teams {
                for profile_id in &profile.delegation.team_targets {
                    if let Some(target) = self.profile_catalog.team(profile_id.as_str()) {
                        items.push(ContextListItem {
                            summary: format!("team    {} · {}", target.id, target.display_name),
                            detail_title: format!("Delegation team {}", target.id),
                            detail_lines: vec![
                                "kind: team".into(),
                                format!("id: {}", target.id),
                                format!("name: {}", target.display_name),
                                format!(
                                    "description: {}",
                                    target.description.as_deref().unwrap_or("unavailable")
                                ),
                                format!(
                                    "members: {}",
                                    target
                                        .members
                                        .iter()
                                        .map(ToString::to_string)
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ),
                            ],
                        });
                    }
                }
            }
        } else {
            items.push(ContextListItem {
                summary: format!("active  {default_agent_profile_id} · unavailable"),
                detail_title: "Active agent profile unavailable".into(),
                detail_lines: vec![format!("id: {default_agent_profile_id}")],
            });
        }

        items.extend(
            self.shared_projection
                .context()
                .delegations
                .iter()
                .map(|delegation| {
                    let mut detail_lines = vec![
                        format!("kind: {}", delegation.target_kind),
                        format!("target: {}", delegation.target_id),
                        format!("status: {}", delegation.status),
                        format!("tool call: {}", delegation.tool_call_id),
                        format!(
                            "child operation: {}",
                            delegation
                                .child_operation_id
                                .as_deref()
                                .unwrap_or("unavailable")
                        ),
                        format!("task: {}", delegation.task),
                    ];
                    if let Some(summary) = &delegation.summary {
                        detail_lines.push(format!("summary: {summary}"));
                    }
                    if let Some(failure) = &delegation.failure {
                        detail_lines.push(format!("failure: {failure}"));
                    }
                    ContextListItem {
                        summary: format!(
                            "child   {} {} · {}",
                            delegation.target_id, delegation.target_kind, delegation.status
                        ),
                        detail_title: format!(
                            "Delegated {} {}",
                            delegation.target_kind, delegation.target_id
                        ),
                        detail_lines,
                    }
                }),
        );
        items
    }

    pub(super) fn context_usage_lines(&self, width: usize) -> Vec<String> {
        let usage = &self.shared_projection.context().usage;
        let mut lines = vec![
            "session totals".into(),
            format!("input       {}", format_token_total(usage.input)),
            format!("output      {}", format_token_total(usage.output)),
            format!("cache read  {}", format_token_total(usage.cache_read)),
            format!("cache write {}", format_token_total(usage.cache_write)),
            format!(
                "cost         {}",
                usage
                    .cost
                    .map(|cost| format!("${cost:.4}"))
                    .unwrap_or_else(|| "unavailable".into())
            ),
            String::new(),
            "latest turn".into(),
        ];
        if let Some(turn) = &usage.latest_turn {
            lines.extend([
                format!("turn         {}", short_id(&turn.turn_id)),
                format!("input        {}", format_tokens(turn.input)),
                format!("output       {}", format_tokens(turn.output)),
                format!("cache read   {}", format_tokens(turn.cache_read)),
                format!("cache write  {}", format_tokens(turn.cache_write)),
                format!(
                    "cost          {}",
                    turn.cost
                        .map(|cost| format!("${cost:.4}"))
                        .unwrap_or_else(|| "unavailable".into())
                ),
            ]);
        } else {
            lines.push("unavailable".into());
        }
        lines.push(String::new());
        lines.push("context window".into());
        let context_tokens = usage
            .latest_turn
            .as_ref()
            .and_then(|turn| turn.context_tokens);
        let context_window = usage.context_window;
        lines.push(match (context_tokens, context_window) {
            (Some(tokens), Some(window)) if window > 0 => {
                let exact = format!("{}/{}", format_tokens(tokens), format_tokens(window));
                let percent = format!("{}%", context_percentage(tokens, window));
                let fixed_width = visible_width("used          ")
                    .saturating_add(2)
                    .saturating_add(1 + visible_width(&percent))
                    .saturating_add(1 + visible_width(&exact));
                let gauge_width = width.saturating_sub(fixed_width).min(12);
                let gauge_width = usize::from(gauge_width >= 3) * gauge_width;
                format!(
                    "used          {} {exact}",
                    context_gauge(tokens, window, gauge_width, !color_enabled()),
                )
            }
            (Some(tokens), Some(0)) => {
                format!("used          unavailable ({})", format_tokens(tokens))
            }
            _ => "used          unavailable".into(),
        });
        lines.push(format!(
            "model         {}",
            usage.model_id.as_deref().unwrap_or("unavailable")
        ));
        lines
    }
}
