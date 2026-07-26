use crate::transcript::SessionEntry;
use serde_json::{Map, Value};

impl SessionEntry {
    #[allow(clippy::too_many_arguments)]
    pub fn compaction(
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        summary: String,
        first_kept_message_id: String,
        tokens_before: u32,
        details: Option<Value>,
        from_hook: bool,
    ) -> Self {
        let mut fields = Map::new();
        fields.insert("summary".into(), Value::String(summary));
        fields.insert(
            "firstKeptEntryId".into(),
            Value::String(first_kept_message_id),
        );
        fields.insert("tokensBefore".into(), Value::Number(tokens_before.into()));
        if let Some(d) = details {
            fields.insert("details".into(), d);
        }
        fields.insert("fromHook".into(), Value::Bool(from_hook));
        Self {
            entry_type: "compaction".into(),
            id,
            parent_id,
            timestamp,
            fields,
        }
    }

    pub fn thinking_level_change(
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        thinking_level: crate::agent::types::ThinkingLevel,
    ) -> Self {
        let mut fields = Map::new();
        fields.insert(
            "thinkingLevel".into(),
            Value::String(thinking_level.to_string()),
        );
        Self {
            entry_type: "thinking_level_change".into(),
            id,
            parent_id,
            timestamp,
            fields,
        }
    }

    pub fn active_tools_change(
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        tools: Vec<String>,
    ) -> Self {
        let mut fields = Map::new();
        fields.insert(
            "activeToolNames".into(),
            Value::Array(tools.into_iter().map(Value::String).collect()),
        );
        Self {
            entry_type: "active_tools_change".into(),
            id,
            parent_id,
            timestamp,
            fields,
        }
    }

    pub fn model_change(
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        provider: String,
        model_id: String,
    ) -> Self {
        let mut fields = Map::new();
        fields.insert("provider".into(), Value::String(provider));
        fields.insert("modelId".into(), Value::String(model_id));
        Self {
            entry_type: "model_change".into(),
            id,
            parent_id,
            timestamp,
            fields,
        }
    }

    pub fn branch_summary(
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        summary: String,
        from_id: String,
        details: Option<Value>,
        from_hook: bool,
    ) -> Self {
        let mut fields = Map::new();
        fields.insert("summary".into(), Value::String(summary));
        fields.insert("fromId".into(), Value::String(from_id));
        if let Some(details) = details {
            fields.insert("details".into(), details);
        }
        fields.insert("fromHook".into(), Value::Bool(from_hook));
        Self {
            entry_type: "branch_summary".into(),
            id,
            parent_id,
            timestamp,
            fields,
        }
    }

    pub fn leaf(
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        target_id: Option<String>,
    ) -> Self {
        let mut fields = Map::new();
        fields.insert(
            "targetId".into(),
            target_id.map(Value::String).unwrap_or(Value::Null),
        );
        Self {
            entry_type: "leaf".into(),
            id,
            parent_id,
            timestamp,
            fields,
        }
    }
}
