use crate::protocol::types::RpcCapabilities;
use crate::protocol::types::{RpcSessionNamePersistence, RpcSessionState};
use crate::protocol::types::{RpcSessionStats, RpcSessionTokenStats};
use crate::rpc::state::RpcState;
use coding_agent::api::authorization::ToolAuthorizationRequest;
use coding_agent::api::client::{ProtocolFamilyVersion, UI_SNAPSHOT_PROTOCOL_VERSION};
use coding_agent::api::embedding::CodingAgentModelCatalogEntry;
use coding_agent::api::error::CodingAgentPublicError;
use coding_agent::api::view::{CodingAgentCapabilities, CodingAgentSessionTranscriptItem};
impl RpcState {
    pub(super) fn session_state(&self) -> Result<RpcSessionState, CodingAgentPublicError> {
        let projection = self.session_projection()?;

        Ok(RpcSessionState {
            model: Some(rpc_model_projection(
                &self.model,
                self.application.model_thinking_level_map(),
            )),
            thinking_level: self.thinking_level,
            is_streaming: self.is_streaming(),
            is_compacting: self.is_compacting,
            steering_mode: self.steering_mode,
            follow_up_mode: self.follow_up_mode,
            session_file: self
                .active_session_storage
                .as_ref()
                .map(|storage| storage.export_path().display().to_string()),
            session_id: projection.session_id,
            event_stream_id: projection.event_stream_id,
            client_id: self
                .client_connection
                .as_ref()
                .map(|connection| connection.client_id.as_str().to_owned()),
            snapshot_sequence: projection.snapshot_sequence,
            capability_generation: projection.capability_generation,
            snapshot_version: projection.snapshot_version,
            negotiated_protocol: self.negotiated_protocol.clone(),
            session_name: self.session_name.clone(),
            session_name_persistence: RpcSessionNamePersistence::AdapterLocal,
            auto_compaction_enabled: self.auto_compaction_enabled,
            message_count: self
                .coding_session
                .as_ref()
                .and_then(|session| session.transcript_snapshot().ok())
                .map_or(0, |transcript| transcript.items.len()),
            pending_message_count: projection.pending_message_count,
            pending_tool_authorizations: projection.pending_tool_authorizations,
            capabilities: projection.capabilities,
        })
    }

    fn session_projection(&self) -> Result<RpcSessionProjection, CodingAgentPublicError> {
        if let Some(connection) = self.client_connection.as_ref() {
            let snapshot = connection.state()?;
            return Ok(RpcSessionProjection {
                session_id: snapshot.session.session_id,
                event_stream_id: Some(snapshot.cursor.stream_id),
                pending_message_count: snapshot.drafts.len(),
                pending_tool_authorizations: snapshot.pending_authorizations,
                capabilities: snapshot.capabilities.into(),
                snapshot_sequence: snapshot.cursor.last_event_sequence,
                capability_generation: snapshot.cursor.capability_generation,
                snapshot_version: snapshot.version,
            });
        }
        if let Some(session) = self.coding_session.as_ref() {
            let snapshot = session.snapshot()?;
            return Ok(RpcSessionProjection {
                session_id: snapshot.session.session_id,
                event_stream_id: Some(snapshot.cursor.stream_id),
                pending_message_count: snapshot.drafts.len(),
                pending_tool_authorizations: snapshot.pending_authorizations,
                capabilities: snapshot.capabilities.into(),
                snapshot_sequence: snapshot.cursor.last_event_sequence,
                capability_generation: snapshot.cursor.capability_generation,
                snapshot_version: snapshot.version,
            });
        }

        Ok(RpcSessionProjection {
            session_id: self.fallback_session_id(),
            event_stream_id: None,
            pending_message_count: self.steering.len() + self.follow_up.len(),
            pending_tool_authorizations: Vec::new(),
            capabilities: CodingAgentCapabilities::idle(self.active_session_storage.is_some())
                .into(),
            snapshot_sequence: 0,
            capability_generation: 1,
            snapshot_version: UI_SNAPSHOT_PROTOCOL_VERSION,
        })
    }

    fn fallback_session_id(&self) -> String {
        self.active_leaf_id
            .clone()
            .or_else(|| {
                self.active_session_storage
                    .as_ref()
                    .map(|storage| storage.session_id().to_owned())
            })
            .unwrap_or_else(|| "in-memory".into())
    }

    pub(super) fn session_stats(&self) -> Result<RpcSessionStats, CodingAgentPublicError> {
        let session_file = self
            .active_session_storage
            .as_ref()
            .map(|storage| storage.export_path().display().to_string());

        if let Some(snapshot) = self
            .coding_session
            .as_ref()
            .map(|session| session.current_session_snapshot())
            .transpose()?
            .flatten()
        {
            let counts = rpc_message_counts(&snapshot.transcript);
            let usage = snapshot.usage;
            return Ok(RpcSessionStats {
                session_file,
                session_id: snapshot.choice.id,
                active_leaf_id: snapshot.choice.active_leaf_id,
                user_messages: counts.user,
                assistant_messages: counts.assistant,
                tool_calls: counts.tool_calls,
                tool_results: counts.tool_results,
                total_messages: counts.total_messages(),
                tokens: token_stats(
                    usage.input.into(),
                    usage.output.into(),
                    usage.cache_read.into(),
                    usage.cache_write.into(),
                ),
                cost: usage.cost,
                cost_known: usage.cost_known,
            });
        }

        let transcript = self
            .coding_session
            .as_ref()
            .map(|session| session.transcript_snapshot())
            .transpose()?;
        let counts = transcript
            .as_ref()
            .map_or_else(RpcSessionMessageCounts::default, |value| {
                rpc_message_counts(&value.items)
            });
        let usage = self
            .coding_session
            .as_ref()
            .map(|session| session.snapshot().map(|snapshot| snapshot.context.usage))
            .transpose()?
            .unwrap_or_default();

        Ok(RpcSessionStats {
            session_file,
            session_id: transcript.as_ref().map_or_else(
                || self.fallback_session_id(),
                |value| value.session_id.clone(),
            ),
            active_leaf_id: transcript
                .and_then(|value| value.active_leaf_id)
                .or_else(|| self.active_leaf_id.clone()),
            user_messages: counts.user,
            assistant_messages: counts.assistant,
            tool_calls: counts.tool_calls,
            tool_results: counts.tool_results,
            total_messages: counts.total_messages(),
            tokens: token_stats(
                usage.input,
                usage.output,
                usage.cache_read,
                usage.cache_write,
            ),
            cost: usage.cost.unwrap_or(0.0),
            cost_known: usage.cost.is_some(),
        })
    }

    pub(super) fn transcript_items(
        &self,
    ) -> Result<Vec<CodingAgentSessionTranscriptItem>, CodingAgentPublicError> {
        self.coding_session
            .as_ref()
            .map(|session| {
                session
                    .transcript_snapshot()
                    .map(|transcript| transcript.items)
            })
            .transpose()
            .map(Option::unwrap_or_default)
    }

    pub(super) fn last_assistant_text(&self) -> Result<Option<String>, CodingAgentPublicError> {
        Ok(self
            .transcript_items()?
            .into_iter()
            .rev()
            .find_map(|item| match item {
                CodingAgentSessionTranscriptItem::Assistant { text, .. } => Some(text),
                _ => None,
            }))
    }
}

fn rpc_model_projection(
    model: &CodingAgentModelCatalogEntry,
    thinking_level_map: Option<[Option<Option<String>>; 5]>,
) -> serde_json::Value {
    let mut input = Vec::with_capacity(2);
    if model.supports_text {
        input.push("text");
    }
    if model.supports_images {
        input.push("image");
    }
    let mut projection = serde_json::json!({
        "id": model.id,
        "name": model.name,
        "api": model.api,
        "provider": model.provider,
        "baseUrl": "",
        "reasoning": model.reasoning,
        "input": input,
        "cost": {
            "input": 0.0,
            "output": 0.0,
            "cacheRead": 0.0,
            "cacheWrite": 0.0
        },
        "contextWindow": model.context_window,
        "maxTokens": model.max_output_tokens
    });
    if let Some(mapping) = thinking_level_map {
        let mut wire_mapping = serde_json::Map::new();
        for (name, value) in ["minimal", "low", "medium", "high", "xhigh"]
            .into_iter()
            .zip(mapping)
        {
            if let Some(value) = value {
                wire_mapping.insert(
                    name.to_owned(),
                    value.map_or(serde_json::Value::Null, serde_json::Value::String),
                );
            }
        }
        projection
            .as_object_mut()
            .expect("RPC model projection is an object")
            .insert(
                "thinkingLevelMap".into(),
                serde_json::Value::Object(wire_mapping),
            );
    }
    projection
}

#[derive(Debug, Default)]
struct RpcSessionMessageCounts {
    user: usize,
    assistant: usize,
    tool_calls: usize,
    tool_results: usize,
}

fn rpc_message_counts(transcript: &[CodingAgentSessionTranscriptItem]) -> RpcSessionMessageCounts {
    let mut counts = RpcSessionMessageCounts::default();
    for item in transcript {
        match item {
            CodingAgentSessionTranscriptItem::User { .. } => counts.user += 1,
            CodingAgentSessionTranscriptItem::Assistant { .. } => counts.assistant += 1,
            CodingAgentSessionTranscriptItem::Tool { result, .. } => {
                counts.tool_calls += 1;
                counts.tool_results += usize::from(result.is_some());
            }
            CodingAgentSessionTranscriptItem::Delegation { .. }
            | CodingAgentSessionTranscriptItem::CompactionSummary { .. }
            | CodingAgentSessionTranscriptItem::BranchSummary { .. }
            | CodingAgentSessionTranscriptItem::Diagnostic { .. } => {}
        }
    }
    counts
}

impl RpcSessionMessageCounts {
    fn total_messages(&self) -> usize {
        self.user + self.assistant + self.tool_results
    }
}

fn token_stats(input: u64, output: u64, cache_read: u64, cache_write: u64) -> RpcSessionTokenStats {
    RpcSessionTokenStats {
        input,
        output,
        cache_read,
        cache_write,
        total: input
            .saturating_add(output)
            .saturating_add(cache_read)
            .saturating_add(cache_write),
    }
}

struct RpcSessionProjection {
    session_id: String,
    event_stream_id: Option<String>,
    pending_message_count: usize,
    pending_tool_authorizations: Vec<ToolAuthorizationRequest>,
    capabilities: RpcCapabilities,
    snapshot_sequence: u64,
    capability_generation: u64,
    snapshot_version: ProtocolFamilyVersion,
}
