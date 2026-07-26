use super::types::{
    RpcDetachLifecycleEvent, RpcDetachRequest, RpcDetachResponse, RpcDetachStatus,
    RpcShutdownLifecycleEvent, RpcShutdownRequest, RpcShutdownResponse, RpcShutdownStatus,
};
use serde_json::json;

#[test]
fn lifecycle_wire_values_are_additive_and_exact() {
    assert_eq!(
        serde_json::to_value(RpcDetachRequest {
            id: Some("detach-1".into()),
        })
        .unwrap(),
        json!({"id": "detach-1", "type": "detach"})
    );
    assert_eq!(
        serde_json::to_value(RpcDetachResponse {
            status: RpcDetachStatus::AlreadyDetached,
        })
        .unwrap(),
        json!({"status": "already_detached"})
    );
    assert_eq!(
        serde_json::to_value(RpcDetachLifecycleEvent {
            status: RpcDetachStatus::Detached,
        })
        .unwrap(),
        json!({"type": "client_detached", "status": "detached"})
    );
    assert_eq!(
        serde_json::to_value(RpcShutdownRequest {
            id: Some("shutdown-1".into()),
        })
        .unwrap(),
        json!({"id": "shutdown-1", "type": "shutdown"})
    );
    assert_eq!(
        serde_json::to_value(RpcShutdownResponse {
            status: RpcShutdownStatus::AlreadyShutDown,
        })
        .unwrap(),
        json!({"status": "already_shut_down"})
    );
    assert_eq!(
        serde_json::to_value(RpcShutdownLifecycleEvent {
            status: RpcShutdownStatus::ShutDown,
        })
        .unwrap(),
        json!({"type": "runtime_shut_down", "status": "shut_down"})
    );
    assert_eq!(
        serde_json::to_value(RpcDetachStatus::StaleGeneration).unwrap(),
        json!("stale_generation")
    );
    assert_eq!(
        serde_json::to_value(RpcShutdownStatus::ShutdownRequested).unwrap(),
        json!("shutdown_requested")
    );
}
