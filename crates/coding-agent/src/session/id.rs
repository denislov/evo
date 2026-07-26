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
        prefixed_id("sess")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_id_generator_uses_stable_prefixes() {
        let mut generator = DeterministicIdGenerator::new();

        assert_eq!(generator.next_session_id(), "sess_1");
        assert_eq!(generator.next_session_id(), "sess_2");
        assert_eq!(generator.next_event_id(), "evt_1");
        assert_eq!(generator.next_root_operation_id(), "op_1");
        assert_eq!(generator.next_child_operation_id(), "op_2");
        assert_eq!(generator.next_session_copy_id(), "copy_1");
        assert_eq!(generator.next_recovery_id(), "recovery_1");
        assert_eq!(generator.next_turn_id(), "turn_1");
        assert_eq!(generator.next_message_id(), "msg_1");
        assert_eq!(generator.next_tool_call_id(), "tool_1");
        assert_eq!(generator.next_leaf_id(), "leaf_1");
    }

    #[test]
    fn system_id_generator_uses_product_prefixes() {
        let mut generator = SystemIdGenerator;

        assert!(generator.next_session_id().starts_with("sess_"));
        assert!(generator.next_event_id().starts_with("evt_"));
        assert!(generator.next_root_operation_id().starts_with("op_"));
        assert!(generator.next_child_operation_id().starts_with("op_"));
        assert!(generator.next_session_copy_id().starts_with("copy_"));
        assert!(generator.next_recovery_id().starts_with("recovery_"));
        assert!(generator.next_turn_id().starts_with("turn_"));
        assert!(generator.next_message_id().starts_with("msg_"));
        assert!(generator.next_tool_call_id().starts_with("tool_"));
        assert!(generator.next_leaf_id().starts_with("leaf_"));
    }

    #[test]
    fn fixed_clock_returns_stable_timestamp() {
        let clock = FixedClock::new("2026-06-29T00:00:00Z");

        assert_eq!(clock.now_rfc3339(), "2026-06-29T00:00:00Z");
    }
}
