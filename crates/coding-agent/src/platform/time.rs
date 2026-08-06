use agent_core::api::transcript::{create_session_id, create_timestamp};

pub(crate) trait IdGenerator {
    fn next_session_id(&mut self) -> String;
    fn next_event_id(&mut self) -> String;
    fn next_root_operation_id(&mut self) -> String;
    fn next_child_operation_id(&mut self) -> String;
    fn next_session_copy_id(&mut self) -> String;
    fn next_turn_id(&mut self) -> String;
    fn next_message_id(&mut self) -> String;
    fn next_tool_call_id(&mut self) -> String;
    fn next_leaf_id(&mut self) -> String;
    fn next_branch_id(&mut self) -> String;
}

pub(crate) trait Clock {
    fn now_rfc3339(&self) -> String;
}

#[derive(Debug, Default, Clone)]
pub(crate) struct SystemIdGenerator;

impl IdGenerator for SystemIdGenerator {
    fn next_session_id(&mut self) -> String {
        new_session_id()
    }

    fn next_event_id(&mut self) -> String {
        prefixed_id("evt")
    }

    fn next_root_operation_id(&mut self) -> String {
        prefixed_id("op")
    }

    fn next_child_operation_id(&mut self) -> String {
        prefixed_id("op")
    }

    fn next_session_copy_id(&mut self) -> String {
        prefixed_id("copy")
    }

    fn next_turn_id(&mut self) -> String {
        prefixed_id("turn")
    }

    fn next_message_id(&mut self) -> String {
        prefixed_id("msg")
    }

    fn next_tool_call_id(&mut self) -> String {
        prefixed_id("tool")
    }

    fn next_leaf_id(&mut self) -> String {
        prefixed_id("leaf")
    }

    fn next_branch_id(&mut self) -> String {
        prefixed_id("branch")
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now_rfc3339(&self) -> String {
        create_timestamp()
    }
}

fn prefixed_id(prefix: &str) -> String {
    format!("{prefix}_{}", create_session_id())
}

pub(crate) fn new_session_id() -> String {
    prefixed_id("sess")
}

pub(crate) fn new_product_event_stream_id() -> String {
    prefixed_id("stream")
}
