use super::*;

impl SessionService {
    pub(crate) fn hydrate(
        options: &CodingAgentSessionOptions,
    ) -> Result<CodingAgentSessionHydration, CodingSessionError> {
        let (store, handle) = Self::open_hydration_handle(options)?;
        let summary = CodingAgentSessionSummary::from(SessionSummary::from_handle(&handle));
        bounded_hydration(&store, &handle, summary, &[])
    }

    pub(crate) fn tree_view(
        options: &CodingAgentSessionOptions,
    ) -> Result<CodingAgentSessionTree, CodingSessionError> {
        Self::open(options)?.leaf_tree_view()
    }

    pub(super) fn leaf_tree_view(&self) -> Result<CodingAgentSessionTree, CodingSessionError> {
        let events = self.store.read_events(&self.handle)?;
        let replay = fold_events(&events);
        Ok(build_leaf_tree(
            &events,
            self.current_active_leaf_id()?,
            &replay.tree_labels,
        ))
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.handle.manifest().session_id
    }

    pub(crate) fn current_active_leaf_id(&self) -> Result<Option<String>, CodingSessionError> {
        Ok(self.transaction_writer.manifest_snapshot()?.active_leaf_id)
    }

    pub(crate) fn current_default_agent_profile_id(&self) -> ProfileId {
        // The default profile is immutable after session creation, so the
        // repository-owned manifest is authoritative and needs no writer lock.
        self.handle.manifest().default_agent_profile_id.clone()
    }

    pub(crate) fn branch_summary_for(
        &self,
        source_leaf_id: &str,
        target_leaf_id: &str,
    ) -> Result<Option<String>, CodingSessionError> {
        let source_leaf_id = normalize_leaf_id(source_leaf_id)?;
        let target_leaf_id = normalize_leaf_id(target_leaf_id)?;
        Ok(self
            .replay()?
            .transcript
            .into_iter()
            .rev()
            .find_map(|item| match item {
                TranscriptItem::BranchSummary {
                    summary,
                    source_leaf_id: summary_source_leaf_id,
                    target_leaf_id: summary_target_leaf_id,
                } if summary_source_leaf_id == source_leaf_id
                    && summary_target_leaf_id == target_leaf_id =>
                {
                    Some(summary)
                }
                _ => None,
            }))
    }

    pub(crate) fn replay(&self) -> Result<SessionReplay, CodingSessionError> {
        self.store.replay_session(&self.handle)
    }

    pub(crate) fn committed_session_sequence(&self) -> u64 {
        self.committed_session_sequence.load(Ordering::Acquire)
    }

    pub(crate) fn view(&self) -> Result<CodingAgentSessionView, CodingSessionError> {
        Ok(CodingAgentSessionView {
            session_id: self.session_id().to_owned(),
            name: self.transaction_writer.manifest_snapshot()?.name,
            default_agent_profile_id: self.current_default_agent_profile_id(),
        })
    }

    pub(crate) fn hydrated_view(&self) -> Result<CodingAgentSessionHydration, CodingSessionError> {
        bounded_hydration(
            &self.store,
            &self.handle,
            self.summary()?,
            self.transaction_writer.startup_storage_recoveries(),
        )
    }

    pub(crate) fn session_export(
        &self,
        options: ExportOptions,
    ) -> Result<SessionExport, CodingSessionError> {
        Ok(SessionExport {
            options,
            summary: self.summary()?,
            replay: self.replay()?,
        })
    }

    pub(super) fn summary(&self) -> Result<CodingAgentSessionSummary, CodingSessionError> {
        let manifest = self.transaction_writer.manifest_snapshot()?;
        Ok(CodingAgentSessionSummary {
            storage: crate::session::view::SessionStorageHandle::new(
                manifest.session_id.clone(),
                self.handle.session_dir().to_path_buf(),
                manifest.event_log.clone(),
            ),
            session_id: manifest.session_id,
            name: manifest.name,
            created_at: manifest.created_at,
            updated_at: manifest.updated_at,
            active_leaf_id: manifest.active_leaf_id,
        })
    }
}

fn bounded_hydration(
    store: &SessionLogStore,
    handle: &SessionHandle,
    summary: CodingAgentSessionSummary,
    startup_storage_recoveries: &[String],
) -> Result<CodingAgentSessionHydration, CodingSessionError> {
    let bounded = store.read_events_bounded(
        handle,
        SessionEventReadBudget::new(MAX_HYDRATION_EVENT_ITEMS, MAX_HYDRATION_EVENT_BYTES),
    )?;
    let continuation = bounded.continuation.map(|cursor| {
        CodingAgentTranscriptContinuation::new(
            cursor.opaque_token(),
            cursor.before_session_sequence,
        )
    });
    let replay = fold_events(&bounded.events);
    let cwd = store.session_creation_workspace_for_handle(handle)?.cwd;
    let mut diagnostics = replay
        .diagnostics
        .into_iter()
        .map(|diagnostic| CodingAgentSessionDiagnostic {
            message: diagnostic.message,
        })
        .collect::<Vec<_>>();
    diagnostics.extend(
        startup_storage_recoveries
            .iter()
            .cloned()
            .map(|message| CodingAgentSessionDiagnostic { message }),
    );
    Ok(CodingAgentSessionHydration {
        summary,
        cwd,
        transcript: coding_transcript_from_replay(replay.transcript),
        omitted_items: bounded.omitted_items,
        continuation,
        diagnostics,
        usage: CodingAgentSessionUsageSummary {
            input: replay.usage.input,
            output: replay.usage.output,
            cache_read: replay.usage.cache_read,
            cache_write: replay.usage.cache_write,
            cost: replay.usage.cost,
            cost_known: replay.usage.cost_known,
            last_context_tokens: replay.usage.last_context_tokens,
        },
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LeafTreeEntry {
    leaf_id: String,
    parent_leaf_id: Option<String>,
    timestamp: String,
    text: String,
}

fn build_leaf_tree(
    events: &[SessionEventEnvelope],
    active_leaf_id: Option<String>,
    tree_labels: &HashMap<String, ReplayTreeLabel>,
) -> CodingAgentSessionTree {
    let mut operation_kinds = HashMap::new();
    let mut operation_inputs = HashMap::new();
    let mut leaves = Vec::new();
    let mut current_parent_leaf_id: Option<String> = None;

    for event in events {
        if let SessionEventData::ActiveLeafChanged { leaf_id } = &event.data {
            current_parent_leaf_id = Some(leaf_id.clone());
            continue;
        }
        let Some(operation_id) = event.operation_id.as_deref() else {
            continue;
        };
        match &event.data {
            SessionEventData::OperationStarted { operation, .. } => {
                operation_kinds.insert(operation_id.to_owned(), operation.clone());
            }
            SessionEventData::TurnInputRecorded { content } => {
                operation_inputs
                    .entry(operation_id.to_owned())
                    .or_insert_with(|| text_from_persisted_content(content));
            }
            SessionEventData::OperationCommitted {
                new_leaf_id: Some(leaf_id),
            } if operation_kinds.get(operation_id) == Some(&OperationKind::Prompt) => {
                leaves.push(LeafTreeEntry {
                    leaf_id: leaf_id.clone(),
                    parent_leaf_id: current_parent_leaf_id.clone(),
                    timestamp: event.created_at.clone(),
                    text: operation_inputs
                        .get(operation_id)
                        .filter(|text| !text.trim().is_empty())
                        .cloned()
                        .unwrap_or_else(|| leaf_id.clone()),
                });
                current_parent_leaf_id = Some(leaf_id.clone());
            }
            _ => {}
        }
    }

    CodingAgentSessionTree {
        tree: leaf_tree(leaves, tree_labels),
        active_leaf_id,
    }
}

fn text_from_persisted_content(content: &[PersistedContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            PersistedContentBlock::Text { text } => Some(text.trim()),
            PersistedContentBlock::Thinking { thinking, .. } => Some(thinking.trim()),
            PersistedContentBlock::Image { .. } => None,
            PersistedContentBlock::ProviderItem { .. } => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn leaf_tree(
    leaves: Vec<LeafTreeEntry>,
    tree_labels: &HashMap<String, ReplayTreeLabel>,
) -> Vec<SessionTreeNode> {
    let known_leaf_ids = leaves
        .iter()
        .map(|leaf| leaf.leaf_id.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut children_by_parent: HashMap<Option<String>, Vec<LeafTreeEntry>> = HashMap::new();
    for mut leaf in leaves {
        if leaf
            .parent_leaf_id
            .as_ref()
            .is_some_and(|parent| !known_leaf_ids.contains(parent))
        {
            leaf.parent_leaf_id = None;
        }
        children_by_parent
            .entry(leaf.parent_leaf_id.clone())
            .or_default()
            .push(leaf);
    }
    build_leaf_children(None, &mut children_by_parent, tree_labels)
}

fn build_leaf_children(
    parent_leaf_id: Option<&str>,
    children_by_parent: &mut HashMap<Option<String>, Vec<LeafTreeEntry>>,
    tree_labels: &HashMap<String, ReplayTreeLabel>,
) -> Vec<SessionTreeNode> {
    let key = parent_leaf_id.map(str::to_owned);
    let leaves = children_by_parent.remove(&key).unwrap_or_default();
    leaves
        .into_iter()
        .map(|leaf| {
            let leaf_id = leaf.leaf_id.clone();
            let label = tree_labels.get(&leaf_id);
            let mut node = SessionTreeNode {
                entry: SessionEntry::message(
                    leaf.leaf_id,
                    leaf.parent_leaf_id,
                    leaf.timestamp,
                    StoredAgentMessage::User {
                        content: vec![ContentBlock::Text {
                            text: leaf.text,
                            text_signature: None,
                        }],
                        timestamp: 0,
                    },
                ),
                children: Vec::new(),
                label: label.and_then(|label| label.label.clone()),
                label_timestamp: label
                    .filter(|label| label.label.is_some())
                    .map(|label| label.updated_at.clone()),
            };
            node.children = build_leaf_children(Some(&leaf_id), children_by_parent, tree_labels);
            node
        })
        .collect()
}

pub(crate) use crate::domain::projection::transcript::coding_transcript_from_replay;
#[cfg(test)]
use crate::domain::projection::transcript::coding_transcript_item_from_replay;

#[cfg(test)]
mod coding_transcript_item_from_replay_tests {
    use super::*;
    use crate::session::event::PersistedContentBlock;
    use crate::session::replay::{MessageStatus, ToolCallStatus, TranscriptItem};
    use crate::session::view::CodingAgentSessionTranscriptItem;

    #[test]
    fn queue_saturation_is_structured_in_the_session_write_failure_event() {
        let finalized = SessionService::failed_prompt_transaction(
            "operation-queue-saturated",
            &CodingSessionError::SessionWriteFailure {
                reason: SessionWriteFailureReason::QueueSaturated,
                message: "bounded queue timeout".into(),
            },
        );
        assert!(matches!(
            finalized.events.as_slice(),
            [
                SessionWriteEvent::Pending { .. },
                SessionWriteEvent::Failed {
                    failure_reason: Some(CodingAgentSessionWriteFailureReason::QueueSaturated),
                    status: CodingAgentSessionWriteFailureStatus::Definite,
                    ..
                }
            ]
        ));
    }

    #[test]
    fn assistant_message_model_id_flows_through_the_transcript_conversion() {
        let item = TranscriptItem::AssistantMessage {
            message_id: "message-1".into(),
            content: vec![PersistedContentBlock::Text {
                text: "answer".into(),
            }],
            status: MessageStatus::Completed,
            reasoning_duration_millis: None,
            model_id: Some("deepseek-v4-pro".into()),
            completed_at: Some("2026-01-01T00:00:01Z".into()),
        };
        let CodingAgentSessionTranscriptItem::Assistant { model_id, .. } =
            coding_transcript_item_from_replay(item)
        else {
            panic!("assistant message must convert to an assistant transcript item");
        };
        assert_eq!(model_id.as_deref(), Some("deepseek-v4-pro"));
    }

    #[test]
    fn assistant_message_without_model_id_stays_unattributed() {
        let item = TranscriptItem::AssistantMessage {
            message_id: "message-2".into(),
            content: Vec::new(),
            status: MessageStatus::Started,
            reasoning_duration_millis: None,
            model_id: None,
            completed_at: None,
        };
        let CodingAgentSessionTranscriptItem::Assistant { model_id, .. } =
            coding_transcript_item_from_replay(item)
        else {
            panic!("assistant message must convert to an assistant transcript item");
        };
        assert!(model_id.is_none());
    }

    #[test]
    fn provider_tool_before_text_is_restored_before_the_assistant_segment() {
        let projected = coding_transcript_from_replay(vec![
            assistant_message(
                "message-1",
                vec![provider_item("provider-web-1"), text("answer")],
            ),
            tool_call("session-tool-1", "provider-web-1"),
        ]);

        assert!(matches!(
            projected.as_slice(),
            [
                CodingAgentSessionTranscriptItem::Tool { call_id, name, .. },
                CodingAgentSessionTranscriptItem::Assistant {
                    id,
                    text,
                    done: true,
                    model_id: Some(model_id),
                    completed_at: Some(completed_at),
                    ..
                }
            ] if call_id == "session-tool-1"
                && name == "web_search"
                && id == "message-1:segment:1"
                && text == "answer"
                && model_id == "deepseek-v4-flash"
                && completed_at == "2026-01-01T00:00:01Z"
        ));
    }

    #[test]
    fn provider_tool_between_text_blocks_splits_the_assistant_at_its_actual_position() {
        let projected = coding_transcript_from_replay(vec![
            assistant_message(
                "message-2",
                vec![
                    text("before"),
                    provider_item("provider-web-2"),
                    text("after"),
                ],
            ),
            tool_call("session-tool-2", "provider-web-2"),
        ]);

        assert!(matches!(
            projected.as_slice(),
            [
                CodingAgentSessionTranscriptItem::Assistant {
                    id: before_id,
                    text: before,
                    done: true,
                    model_id: None,
                    completed_at: None,
                    ..
                },
                CodingAgentSessionTranscriptItem::Tool { call_id, .. },
                CodingAgentSessionTranscriptItem::Assistant {
                    id: after_id,
                    text: after,
                    done: true,
                    model_id: Some(model_id),
                    completed_at: Some(_),
                    ..
                }
            ] if before_id == "message-2"
                && before == "before"
                && call_id == "session-tool-2"
                && after_id == "message-2:segment:1"
                && after == "after"
                && model_id == "deepseek-v4-flash"
        ));
    }

    #[test]
    fn multiple_provider_tools_follow_content_block_order_without_duplicates() {
        let projected = coding_transcript_from_replay(vec![
            assistant_message(
                "message-3",
                vec![
                    text("first"),
                    provider_item("provider-web-3a"),
                    text("second"),
                    provider_item("provider-web-3b"),
                    text("third"),
                ],
            ),
            tool_call("session-tool-3a", "provider-web-3a"),
            tool_call("session-tool-3b", "provider-web-3b"),
        ]);

        let labels = projected
            .iter()
            .map(|item| match item {
                CodingAgentSessionTranscriptItem::Assistant { text, .. } => {
                    format!("assistant:{text}")
                }
                CodingAgentSessionTranscriptItem::Tool { call_id, .. } => {
                    format!("tool:{call_id}")
                }
                _ => "unexpected".into(),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            [
                "assistant:first",
                "tool:session-tool-3a",
                "assistant:second",
                "tool:session-tool-3b",
                "assistant:third",
            ]
        );
    }

    #[test]
    fn unmatched_provider_items_and_tools_keep_their_legacy_order() {
        let projected = coding_transcript_from_replay(vec![
            assistant_message(
                "message-4",
                vec![provider_item("missing-web"), text("answer")],
            ),
            tool_call("local-tool", "unrelated-provider-call"),
        ]);

        assert!(matches!(
            projected.as_slice(),
            [
                CodingAgentSessionTranscriptItem::Assistant { id, text, .. },
                CodingAgentSessionTranscriptItem::Tool { call_id, .. }
            ] if id == "message-4" && text == "answer" && call_id == "local-tool"
        ));
    }

    fn assistant_message(message_id: &str, content: Vec<PersistedContentBlock>) -> TranscriptItem {
        TranscriptItem::AssistantMessage {
            message_id: message_id.into(),
            content,
            status: MessageStatus::Completed,
            reasoning_duration_millis: Some(25),
            model_id: Some("deepseek-v4-flash".into()),
            completed_at: Some("2026-01-01T00:00:01Z".into()),
        }
    }

    fn text(value: &str) -> PersistedContentBlock {
        PersistedContentBlock::Text { text: value.into() }
    }

    fn provider_item(id: &str) -> PersistedContentBlock {
        PersistedContentBlock::ProviderItem {
            api: "deepseek-responses".into(),
            item: serde_json::json!({
                "type": "web_search_call",
                "id": id,
                "status": "completed"
            }),
        }
    }

    fn tool_call(tool_call_id: &str, provider_call_id: &str) -> TranscriptItem {
        TranscriptItem::ToolCall {
            tool_call_id: tool_call_id.into(),
            name: "web_search".into(),
            arguments: serde_json::json!({
                "type": "web_search_call",
                "id": provider_call_id
            }),
            status: ToolCallStatus::Completed,
            summary: "completed".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            duration_millis: Some(10),
        }
    }
}
