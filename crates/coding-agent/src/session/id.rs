use agent_core::api::transcript::{create_session_id, create_timestamp};

pub(crate) trait IdGenerator {
    fn next_session_id(&mut self) -> String;
    fn next_event_id(&mut self) -> String;
    fn next_root_operation_id(&mut self) -> String;
    fn next_child_operation_id(&mut self) -> String;
    fn next_session_copy_id(&mut self) -> String;
    #[cfg(test)]
    fn next_recovery_id(&mut self) -> String;
    fn next_turn_id(&mut self) -> String;
    fn next_message_id(&mut self) -> String;
    fn next_tool_call_id(&mut self) -> String;
    fn next_leaf_id(&mut self) -> String;
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

    #[cfg(test)]
    fn next_recovery_id(&mut self) -> String {
        prefixed_id("recovery")
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

#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub(crate) struct DeterministicIdGenerator {
    session: u64,
    event: u64,
    operation: u64,
    session_copy: u64,
    recovery: u64,
    turn: u64,
    message: u64,
    tool_call: u64,
    leaf: u64,
}

#[cfg(test)]
impl DeterministicIdGenerator {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
impl IdGenerator for DeterministicIdGenerator {
    fn next_session_id(&mut self) -> String {
        next_deterministic_id("sess", &mut self.session)
    }

    fn next_event_id(&mut self) -> String {
        next_deterministic_id("evt", &mut self.event)
    }

    fn next_root_operation_id(&mut self) -> String {
        next_deterministic_id("op", &mut self.operation)
    }

    fn next_child_operation_id(&mut self) -> String {
        next_deterministic_id("op", &mut self.operation)
    }

    fn next_session_copy_id(&mut self) -> String {
        next_deterministic_id("copy", &mut self.session_copy)
    }

    #[cfg(test)]
    fn next_recovery_id(&mut self) -> String {
        next_deterministic_id("recovery", &mut self.recovery)
    }

    fn next_turn_id(&mut self) -> String {
        next_deterministic_id("turn", &mut self.turn)
    }

    fn next_message_id(&mut self) -> String {
        next_deterministic_id("msg", &mut self.message)
    }

    fn next_tool_call_id(&mut self) -> String {
        next_deterministic_id("tool", &mut self.tool_call)
    }

    fn next_leaf_id(&mut self) -> String {
        next_deterministic_id("leaf", &mut self.leaf)
    }
}

#[cfg(test)]
fn next_deterministic_id(prefix: &str, counter: &mut u64) -> String {
    *counter += 1;
    format!("{prefix}_{counter}")
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct FixedClock {
    timestamp: String,
}

#[cfg(test)]
impl FixedClock {
    pub(crate) fn new(timestamp: impl Into<String>) -> Self {
        Self {
            timestamp: timestamp.into(),
        }
    }
}

#[cfg(test)]
impl Clock for FixedClock {
    fn now_rfc3339(&self) -> String {
        self.timestamp.clone()
    }
}
