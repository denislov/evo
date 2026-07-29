use std::collections::VecDeque;
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use coding_agent::api::authorization::{
    ToolAuthorizationDecision, ToolAuthorizationIdentity, ToolAuthorizationPreview,
    ToolAuthorizationRequest, ToolAuthorizationRisk, ToolAuthorizationScope,
};
use coding_agent::api::client::{
    CodingAgentClientBootstrap, CodingAgentClientId, CodingAgentClientProjection,
    CodingAgentClientProjectionApply, CodingAgentFreshSnapshotRecovery,
    CodingAgentReconnectDelivery, CodingAgentRecoveryPending, CodingAgentRecoveryReason,
};
use coding_agent::api::embedding::{
    CodingAgentEmbeddingContext, CodingAgentEmbeddingOptions, CodingAgentThinkingLevel,
    CodingAgentWorkspaceSelection,
};
use coding_agent::api::error::{
    CodingAgentErrorCategory, CodingAgentErrorContext, CodingAgentPublicError,
};
use coding_agent::api::event::{
    CodingAgentProductEvent, CodingAgentProductEventDeliveryClass, CodingAgentRecoveryResolution,
};
use coding_agent::api::review::CodingAgentFileReviewRequest;
use coding_agent::api::view::CodingAgentSessionTranscriptItem;
use tokio::sync::{mpsc, watch};
use tokio::task;

use crate::conversation::{MAX_TRANSCRIPT_BLOCKS, MAX_TRANSCRIPT_BYTES};
use crate::projection::{
    ContextDirtyFlags, DesktopMessageStatus, DesktopProjection, DesktopProjectionApply,
    DesktopProjectionLifecycle, DesktopToolStatus, MAX_AUTHORIZATION_TEXT_BYTES,
    MAX_DESKTOP_MESSAGE_OVERLAYS,
};

use super::bridge::build_desktop_runtime;
use super::dispatch::{dispatch_active_command, dispatch_command};
use super::driver::*;
use super::protocol::*;
use super::*;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct ProcessEnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl ProcessEnvGuard {
    fn isolated(evo_dir: &std::path::Path) -> Self {
        const NAMES: &[&str] = &[
            "EVO_DIR",
            "ANTHROPIC_API_KEY",
            "CLAUDE_API_KEY",
            "ANTHROPIC_KEY",
        ];
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved = NAMES
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();
        unsafe {
            std::env::set_var("EVO_DIR", evo_dir);
            for name in &NAMES[1..] {
                std::env::remove_var(name);
            }
        }
        Self { _lock: lock, saved }
    }
}

impl Drop for ProcessEnvGuard {
    fn drop(&mut self) {
        for (name, previous) in self.saved.iter().rev() {
            unsafe {
                match previous {
                    Some(previous) => std::env::set_var(name, previous),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

fn isolated_options(temp: &tempfile::TempDir) -> (ProcessEnvGuard, CodingAgentEmbeddingOptions) {
    let global = temp.path().join("global");
    let project = temp.path().join("project");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let env = ProcessEnvGuard::isolated(&global);
    let options = CodingAgentEmbeddingOptions::for_workspace(
        CodingAgentWorkspaceSelection::project(&project),
    )
    .unwrap()
    .with_session_dir(&sessions)
    .with_model_id("claude-sonnet-4-5");
    (env, options)
}

fn new_project_prompt_target(temp: &tempfile::TempDir) -> DesktopPromptTarget {
    DesktopPromptTarget::new(
        CodingAgentWorkspaceSelection::project(temp.path().join("project")),
        "claude-sonnet-4-5",
        "default",
    )
}

fn existing_prompt_target(session_id: impl Into<String>) -> DesktopPromptTarget {
    DesktopPromptTarget::existing(session_id)
}

fn home_owner_target() -> DesktopRuntimeOwnerTarget {
    DesktopRuntimeOwnerTarget::home()
}

fn session_owner_target(session_id: impl Into<String>) -> DesktopRuntimeOwnerTarget {
    DesktopRuntimeOwnerTarget::session(session_id)
}

fn write_workspace_fixture(project: &std::path::Path, id: &str, thinking: &str) {
    let skill_dir = project.join(".evo/skills").join(format!("{id}-skill"));
    let agents_dir = project.join(".evo/agents");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        project.join(".evo/settings.toml"),
        format!("default_thinking_level = \"{thinking}\"\n"),
    )
    .unwrap();
    std::fs::write(project.join("AGENTS.md"), format!("{id} context")).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!(
            "---\nname: {id}-skill\ndescription: {id} skill description\n---\n{id} skill body\n"
        ),
    )
    .unwrap();
    std::fs::write(
        agents_dir.join(format!("{id}.toml")),
        format!("schema_version = 1\nid = \"{id}\"\ndisplay_name = \"{id}\"\n"),
    )
    .unwrap();
}

async fn start_runtime(
    options: CodingAgentEmbeddingOptions,
) -> (DesktopRuntimeBridge, DesktopRuntimeHydratedSnapshot) {
    let (mut bridge, _) = DesktopRuntimeBridge::spawn(options)
        .unwrap()
        .wait_blocking()
        .unwrap();
    bridge.try_create_session(u64::MAX).unwrap();
    let DesktopRuntimeUpdate::SessionChanged { snapshot, .. } = bridge.next_update().await.unwrap()
    else {
        panic!("test runtime session creation should publish a hydrated snapshot");
    };
    (bridge, snapshot)
}

#[test]
fn desktop_runtime_enables_tcp_io() {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let client = std::net::TcpStream::connect(address).unwrap();
    let (_server, _) = listener.accept().unwrap();
    client.set_nonblocking(true).unwrap();

    build_desktop_runtime().unwrap().block_on(async move {
        let stream = tokio::net::TcpStream::from_std(client).unwrap();
        stream.writable().await.unwrap();
    });
}

#[tokio::test]
async fn bootstrap_can_be_polled_without_waiting_on_runtime_initialization() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let mut bootstrap = DesktopRuntimeBridge::spawn(options).unwrap();

    let (bridge, snapshot) = loop {
        if let Some(ready) = bootstrap.try_ready().unwrap() {
            break ready;
        }
        tokio::task::yield_now().await;
    };
    assert!(!snapshot.project.selected_model_id.is_empty());
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn sessionless_startup_supports_project_commands_and_rejects_session_commands() {
    use coding_agent::api::review::{CodingAgentFileChangeIdentity, CodingAgentFileRevision};

    let temp = tempfile::tempdir().unwrap();
    let sessions_dir = temp.path().join("sessions");
    let (_env, options) = isolated_options(&temp);
    let (mut bridge, ready) = DesktopRuntimeBridge::spawn(options)
        .unwrap()
        .wait_blocking()
        .unwrap();

    assert_eq!(ready.project.selected_model_id, "claude-sonnet-4-5");
    assert!(!sessions_dir.exists());

    bridge.try_reload(1, home_owner_target()).unwrap();
    let Some(DesktopRuntimeUpdate::Reloaded {
        command_id: 1,
        metadata,
    }) = bridge.next_update().await
    else {
        panic!("sessionless reload should return project metadata");
    };
    assert!(metadata.session.is_none());

    bridge.try_list_sessions(2).unwrap();
    let Some(DesktopRuntimeUpdate::SessionsListed {
        command_id: 2,
        sessions,
        omitted: 0,
    }) = bridge.next_update().await
    else {
        panic!("sessionless catalog query should return a typed empty catalog");
    };
    assert!(sessions.is_empty());

    bridge
        .try_select_model(3, home_owner_target(), "claude-haiku-4-5")
        .unwrap();
    let Some(DesktopRuntimeUpdate::SelectionChanged {
        command_id: 3,
        selection: DesktopRuntimeSelectionKind::Model,
        metadata,
    }) = bridge.next_update().await
    else {
        panic!("sessionless model selection should return project metadata");
    };
    assert_eq!(metadata.project.selected_model_id, "claude-haiku-4-5");
    assert!(metadata.session.is_none());

    bridge
        .try_select_session_profile(30, home_owner_target(), "review")
        .unwrap();
    let Some(DesktopRuntimeUpdate::SelectionChanged {
        command_id: 30,
        selection: DesktopRuntimeSelectionKind::SessionProfile,
        metadata,
    }) = bridge.next_update().await
    else {
        panic!("sessionless profile selection should return Home metadata");
    };
    assert_eq!(metadata.project.default_agent_profile_id.as_str(), "review");
    assert!(metadata.session.is_none());

    bridge.try_resync(4).unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 4,
            command: DesktopRuntimeCommandKind::Resync,
            code,
            message,
        }) if code == "session" && message == "desktop runtime has no idle session owner"
    ));

    let review = CodingAgentFileReviewRequest::new(
        CodingAgentFileChangeIdentity {
            operation_id: "operation-sessionless-review".into(),
            tool_call_id: Some("call-sessionless-review".into()),
            path: "src/lib.rs".into(),
        },
        CodingAgentFileRevision::new(1),
    );
    let (commands, mut events, shutdown) = bridge.into_parts();
    commands
        .try_review_changed_file(5, "missing-session", &review)
        .unwrap();
    assert!(matches!(
        events.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 5,
            command: DesktopRuntimeCommandKind::ReviewChangedFile,
            code,
            message,
        }) if code == "session_target" && message == "session missing-session is not open"
    ));

    let recovery = DesktopRecoveryIdentity {
        operation_id: "operation-sessionless-recovery".into(),
        recovery_id: "recovery-sessionless".into(),
        record_version: 1,
        descriptor_revision: 1,
        capability_generation: Some(1),
        attempt_count: 0,
    };
    commands.try_retry_recovery(6, &recovery).unwrap();
    assert!(matches!(
        events.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 6,
            command: DesktopRuntimeCommandKind::RetryRecovery,
            code,
            message,
        }) if code == "session" && message == "desktop runtime has no idle session owner"
    ));

    assert!(!sessions_dir.exists());
    drop(commands);
    shutdown.shutdown(&mut events).await.unwrap();
}

#[tokio::test]
async fn sessionless_runtime_opens_an_existing_session_without_an_intermediate_session() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (creator, created) = start_runtime(options.clone()).await;
    let session_id = created.session.session.session_id.clone();
    creator.shutdown().await.unwrap();

    let (mut bridge, ready) = DesktopRuntimeBridge::spawn(options)
        .unwrap()
        .wait_blocking()
        .unwrap();
    assert_eq!(ready.project.selected_model_id, "claude-sonnet-4-5");

    bridge.try_open_session(7, &session_id).unwrap();
    let Some(DesktopRuntimeUpdate::SessionChanged {
        command_id: 7,
        snapshot,
    }) = bridge.next_update().await
    else {
        panic!("sessionless open should install the requested existing session");
    };
    assert_eq!(snapshot.session.session.session_id, session_id);
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn runtime_owns_context_and_switches_sessions_over_bounded_queues() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (mut bridge, initial) = start_runtime(options).await;
    assert_eq!(
        initial.transcript.session_id,
        initial.session.session.session_id
    );
    let initial_session_id = initial.session.session.session_id.clone();

    bridge.try_create_session(1).unwrap();
    let DesktopRuntimeUpdate::SessionChanged {
        command_id,
        snapshot,
    } = bridge.next_update().await.unwrap()
    else {
        panic!("create session should publish a replacement snapshot");
    };
    assert_eq!(command_id, 1);
    assert_ne!(snapshot.session.session.session_id, initial_session_id);

    bridge.try_open_session(2, &initial_session_id).unwrap();
    let DesktopRuntimeUpdate::SessionChanged {
        command_id,
        snapshot,
    } = bridge.next_update().await.unwrap()
    else {
        panic!("open session should publish a replacement snapshot");
    };
    assert_eq!(command_id, 2);
    assert_eq!(snapshot.session.session.session_id, initial_session_id);

    bridge.try_open_session(3, "missing-session").unwrap();
    let DesktopRuntimeUpdate::CommandRejected {
        command_id,
        command,
        ..
    } = bridge.next_update().await.unwrap()
    else {
        panic!("missing session should be rejected");
    };
    assert_eq!(command_id, 3);
    assert_eq!(command, DesktopRuntimeCommandKind::OpenSession);

    bridge
        .try_reload(4, session_owner_target(&initial_session_id))
        .unwrap();
    let DesktopRuntimeUpdate::Reloaded {
        command_id,
        metadata,
    } = bridge.next_update().await.unwrap()
    else {
        panic!("reload should publish the retained current session");
    };
    assert_eq!(command_id, 4);
    assert_eq!(
        metadata.session.as_ref().unwrap().session.session_id,
        initial_session_id
    );

    bridge.try_resync(5).unwrap();
    let DesktopRuntimeUpdate::Resynced {
        command_id,
        replacement,
    } = bridge.next_update().await.unwrap()
    else {
        panic!("idle resync should publish a consistent runtime snapshot");
    };
    assert_eq!(command_id, 5);
    let DesktopRuntimeResyncSnapshot::Hydrated(snapshot) = replacement else {
        panic!("idle resync must hydrate durable state");
    };
    assert_eq!(snapshot.session.session.session_id, initial_session_id);

    bridge.try_list_sessions(6).unwrap();
    let DesktopRuntimeUpdate::SessionsListed {
        command_id,
        sessions,
        omitted,
    } = bridge.next_update().await.unwrap()
    else {
        panic!("session catalog should use a typed bounded update");
    };
    assert_eq!(command_id, 6);
    assert_eq!(omitted, 0);
    assert!(sessions.len() >= 2);
    assert!(sessions.len() <= MAX_DESKTOP_SESSION_CATALOG);
    assert!(
        sessions
            .iter()
            .any(|session| session.session_id == initial_session_id)
    );

    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn changed_file_review_command_is_typed_and_preserves_product_error_codes() {
    use coding_agent::api::review::{CodingAgentFileChangeIdentity, CodingAgentFileRevision};

    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (bridge, initial) = start_runtime(options).await;
    let session_id = initial.session.session.session_id;
    let (commands, mut events, shutdown) = bridge.into_parts();
    let request = CodingAgentFileReviewRequest::new(
        CodingAgentFileChangeIdentity {
            operation_id: "operation-review".into(),
            tool_call_id: Some("call-review".into()),
            path: "src/lib.rs".into(),
        },
        CodingAgentFileRevision::new(7),
    );

    commands
        .try_review_changed_file(41, &session_id, &request)
        .unwrap();
    let update = events.next_update().await.unwrap();
    assert!(matches!(
        update,
        DesktopRuntimeUpdate::CommandRejected {
            command_id: 41,
            command: DesktopRuntimeCommandKind::ReviewChangedFile,
            code,
            ..
        } if code == "file_review_change_unauthorized"
    ));

    let mut oversized = request;
    oversized.change.path = "x".repeat(MAX_FILE_REVIEW_PATH_BYTES + 1);
    assert!(matches!(
        commands.try_review_changed_file(42, &session_id, &oversized),
        Err(DesktopCommandAdmissionError::InvalidFileReview { .. })
    ));

    drop(commands);
    shutdown.shutdown(&mut events).await.unwrap();
}

#[tokio::test]
async fn failed_reload_retains_the_previous_runtime_context() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, _) = isolated_options(&temp);
    let options = CodingAgentEmbeddingOptions::for_workspace(
        CodingAgentWorkspaceSelection::project(temp.path().join("project")),
    )
    .unwrap()
    .with_session_dir(temp.path().join("sessions"));
    let (mut bridge, initial) = start_runtime(options).await;
    std::fs::write(
        temp.path().join("global").join("settings.toml"),
        "default_model = \"missing-desktop-reload-model\"\n",
    )
    .unwrap();

    bridge
        .try_reload(6, session_owner_target(&initial.session.session.session_id))
        .unwrap();
    let reload_update = bridge.next_update().await;
    assert!(
        matches!(
            &reload_update,
            Some(DesktopRuntimeUpdate::CommandRejected {
                command_id: 6,
                command: DesktopRuntimeCommandKind::Reload,
                code,
                ..
            }) if code == "config"
        ),
        "unexpected reload result: {reload_update:?}"
    );

    bridge.try_resync(7).unwrap();
    let Some(DesktopRuntimeUpdate::Resynced {
        command_id: 7,
        replacement,
    }) = bridge.next_update().await
    else {
        panic!("resync after a failed reload must return the retained context");
    };
    let DesktopRuntimeResyncSnapshot::Hydrated(snapshot) = replacement else {
        panic!("idle resync must hydrate durable state");
    };
    assert_eq!(snapshot.project, initial.project);
    assert_eq!(
        snapshot.session.session.session_id,
        initial.session.session.session_id
    );
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn idle_model_and_session_profile_selection_are_typed_and_transactional() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (mut bridge, initial) = start_runtime(options).await;
    let session_id = initial.session.session.session_id.clone();
    let mut projection = DesktopProjection::new(initial).unwrap();
    let conversation = projection.conversation().clone();
    let product_snapshot = projection.snapshot().clone();
    assert!(
        projection
            .apply(DesktopRuntimeUpdate::Reloaded {
                command_id: 7,
                metadata: DesktopRuntimeMetadataSnapshot {
                    project: projection.project().clone(),
                    session: None,
                },
            })
            .is_replaced()
    );
    assert_eq!(projection.snapshot(), &product_snapshot);
    assert_eq!(projection.conversation(), &conversation);

    bridge
        .try_select_model(8, session_owner_target(&session_id), "claude-haiku-4-5")
        .unwrap();
    let update = bridge.next_update().await.unwrap();
    let DesktopRuntimeUpdate::SelectionChanged {
        command_id: 8,
        selection: DesktopRuntimeSelectionKind::Model,
        metadata,
    } = &update
    else {
        panic!("idle model selection must return a typed replacement snapshot");
    };
    assert_eq!(metadata.project.selected_model_id, "claude-haiku-4-5");
    assert_eq!(
        metadata.session.as_ref().unwrap().session.session_id,
        session_id
    );
    assert!(projection.apply(update).is_replaced());
    assert_eq!(projection.conversation(), &conversation);

    bridge
        .try_select_session_profile(9, session_owner_target(&session_id), "review")
        .unwrap();
    let update = bridge.next_update().await.unwrap();
    let DesktopRuntimeUpdate::SelectionChanged {
        command_id: 9,
        selection: DesktopRuntimeSelectionKind::SessionProfile,
        metadata,
    } = &update
    else {
        panic!("idle profile selection must return a typed replacement snapshot");
    };
    assert_eq!(
        metadata
            .session
            .as_ref()
            .unwrap()
            .session
            .default_agent_profile_id
            .as_str(),
        "review"
    );
    assert_eq!(metadata.project.selected_model_id, "claude-haiku-4-5");
    assert!(projection.apply(update).is_replaced());
    assert_eq!(projection.conversation(), &conversation);

    bridge
        .try_select_model(
            10,
            session_owner_target(&session_id),
            "missing-desktop-model",
        )
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 10,
            command: DesktopRuntimeCommandKind::SelectModel,
            ..
        })
    ));
    bridge
        .try_select_session_profile(11, session_owner_target(&session_id), "missing-profile")
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 11,
            command: DesktopRuntimeCommandKind::SelectSessionProfile,
            ..
        })
    ));

    bridge.try_resync(12).unwrap();
    let Some(DesktopRuntimeUpdate::Resynced { replacement, .. }) = bridge.next_update().await
    else {
        panic!("resync must expose the last successful selector state");
    };
    let DesktopRuntimeResyncSnapshot::Hydrated(snapshot) = replacement else {
        panic!("idle resync must hydrate durable state");
    };
    assert_eq!(snapshot.project.selected_model_id, "claude-haiku-4-5");
    assert_eq!(
        snapshot.session.session.default_agent_profile_id.as_str(),
        "review"
    );
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn ten_mib_transcript_stays_single_hydration_across_metadata_commands() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (bridge, mut initial) = start_runtime(options).await;
    let payload = "x".repeat(1_280);
    initial.transcript.items = (0..MAX_TRANSCRIPT_BLOCKS)
        .map(|index| CodingAgentSessionTranscriptItem::User {
            text: format!("{index}:{payload}"),
        })
        .collect();
    let fixture_bytes = initial
        .transcript
        .items
        .iter()
        .map(|item| match item {
            CodingAgentSessionTranscriptItem::User { text } => text.len(),
            _ => 0,
        })
        .sum::<usize>();
    assert!(fixture_bytes >= 10 * 1024 * 1024);
    let metadata = DesktopRuntimeMetadataSnapshot {
        project: initial.project.clone(),
        session: Some(initial.session.clone()),
    };
    let recovery = DesktopRuntimeRecoverySnapshot {
        project: initial.project.clone(),
        session: initial.session.clone(),
        pending_recoveries: Vec::new(),
    };
    let mut projection = DesktopProjection::new(initial).unwrap();
    let initial_counters = projection.counters();
    assert_eq!(initial_counters.full_transcript_hydrations, 1);
    assert_eq!(
        initial_counters.transcript_items_hydrated,
        MAX_TRANSCRIPT_BLOCKS as u64
    );
    assert_eq!(
        initial_counters.conversation_blocks_allocated,
        MAX_TRANSCRIPT_BLOCKS as u64
    );
    assert!(projection.conversation().retained_bytes() <= MAX_TRANSCRIPT_BYTES);

    for command_id in 100..164 {
        let update = match command_id % 4 {
            0 => DesktopRuntimeUpdate::Reloaded {
                command_id,
                metadata: metadata.clone(),
            },
            1 => DesktopRuntimeUpdate::SelectionChanged {
                command_id,
                selection: DesktopRuntimeSelectionKind::Model,
                metadata: metadata.clone(),
            },
            2 => DesktopRuntimeUpdate::SelectionChanged {
                command_id,
                selection: DesktopRuntimeSelectionKind::SessionProfile,
                metadata: metadata.clone(),
            },
            _ => DesktopRuntimeUpdate::PromptStarted {
                command_id,
                operation_id: format!("metadata-operation-{command_id}"),
                metadata: metadata.clone(),
            },
        };
        assert!(projection.apply(update).is_replaced());
    }
    for command_id in 164..180 {
        assert!(
            projection
                .apply(DesktopRuntimeUpdate::RecoveryChanged {
                    command_id,
                    action: DesktopRecoveryAction::Retry,
                    recovery_id: format!("recovery-{command_id}"),
                    recovery: recovery.clone(),
                })
                .is_replaced()
        );
    }

    let counters = projection.counters();
    assert_eq!(counters.full_transcript_hydrations, 1);
    assert_eq!(
        counters.transcript_items_hydrated,
        MAX_TRANSCRIPT_BLOCKS as u64
    );
    assert_eq!(
        counters.conversation_blocks_allocated,
        MAX_TRANSCRIPT_BLOCKS as u64
    );
    assert_eq!(counters.metadata_replacements, 64);
    assert_eq!(counters.recovery_replacements, 16);
    assert_eq!(
        projection.conversation().blocks().len(),
        MAX_TRANSCRIPT_BLOCKS
    );
    assert!(
        projection
            .conversation()
            .blocks()
            .front()
            .unwrap()
            .text
            .starts_with("0:")
    );
    assert!(
        projection
            .conversation()
            .blocks()
            .back()
            .unwrap()
            .text
            .starts_with("9999:")
    );
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn prompt_submission_forwards_product_events_and_returns_the_session_owner() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (mut bridge, initial) = start_runtime(options).await;
    let session_id = initial.session.session.session_id;

    bridge
        .try_submit_prompt(
            10,
            existing_prompt_target(&session_id),
            "offline desktop prompt",
            None,
        )
        .unwrap();
    let mut started_operation_id = None;
    let mut saw_product_event = false;
    let mut last_product_event_sequence = None;
    let finished = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match bridge.next_update().await.unwrap() {
                DesktopRuntimeUpdate::PromptAccepted { command_id } => {
                    assert_eq!(command_id, 10);
                }
                DesktopRuntimeUpdate::PromptStarted {
                    command_id,
                    operation_id,
                    ..
                } => {
                    assert_eq!(command_id, 10);
                    started_operation_id = Some(operation_id);
                }
                DesktopRuntimeUpdate::ProductEvent { event, .. } => {
                    saw_product_event = true;
                    if let Some(previous) = last_product_event_sequence {
                        assert!(
                            event.sequence() > previous,
                            "desktop bridge reordered product event {} after {previous}",
                            event.sequence()
                        );
                    }
                    last_product_event_sequence = Some(event.sequence());
                    if let Some(started) = started_operation_id.as_deref()
                        && let Some(event_operation_id) = event.operation_id()
                    {
                        assert_eq!(event_operation_id, started);
                    }
                }
                DesktopRuntimeUpdate::PromptFinished {
                    command_id,
                    operation_id,
                    snapshot,
                    ..
                } => {
                    assert_eq!(command_id, 10);
                    assert_eq!(Some(operation_id.as_str()), started_operation_id.as_deref());
                    assert_eq!(snapshot.session.session.session_id, session_id);
                    let transcript = &snapshot.transcript;
                    assert_eq!(transcript.session_id, session_id);
                    assert!(transcript.items.iter().any(|item| matches!(
                        item,
                        coding_agent::api::view::CodingAgentSessionTranscriptItem::User {
                            text
                        } if text == "offline desktop prompt"
                    )));
                    break;
                }
                DesktopRuntimeUpdate::ResyncRequired { .. } => {}
                update => panic!("unexpected prompt update: {update:?}"),
            }
        }
    })
    .await;
    assert!(finished.is_ok(), "offline prompt did not finish promptly");
    assert!(saw_product_event);

    bridge.try_create_session(11).unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::SessionChanged { command_id: 11, .. })
    ));
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_prompts_route_events_and_completions_to_their_session() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (mut bridge, first) = start_runtime(options).await;
    let first_session = first.session.session.session_id;
    bridge.try_create_session(101).unwrap();
    let Some(DesktopRuntimeUpdate::SessionChanged {
        snapshot: second, ..
    }) = bridge.next_update().await
    else {
        panic!("second session should be created");
    };
    let second_session = second.session.session.session_id;

    bridge
        .try_submit_prompt(
            102,
            existing_prompt_target(&first_session),
            "first concurrent prompt",
            None,
        )
        .unwrap();
    bridge
        .try_submit_prompt(
            103,
            existing_prompt_target(&second_session),
            "second concurrent prompt",
            None,
        )
        .unwrap();

    let mut accepted = std::collections::BTreeSet::new();
    let mut started = std::collections::BTreeMap::new();
    let mut finished = std::collections::BTreeMap::new();
    let mut last_sequence = std::collections::BTreeMap::<String, u64>::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while finished.len() < 2 {
            match bridge.next_update().await.unwrap() {
                DesktopRuntimeUpdate::PromptAccepted { command_id } => {
                    assert!(matches!(command_id, 102 | 103));
                    accepted.insert(command_id);
                }
                DesktopRuntimeUpdate::PromptStarted {
                    command_id,
                    operation_id,
                    metadata,
                } => {
                    let session_id = metadata
                        .session
                        .expect("prompt start is session-scoped")
                        .session
                        .session_id;
                    started.insert(command_id, (session_id, operation_id));
                }
                DesktopRuntimeUpdate::ProductEvent { session_id, event } => {
                    assert!(session_id == first_session || session_id == second_session);
                    if let Some(previous) = last_sequence.insert(session_id, event.sequence()) {
                        assert!(event.sequence() > previous);
                    }
                }
                DesktopRuntimeUpdate::PromptFinished {
                    command_id,
                    operation_id,
                    snapshot,
                    ..
                } => {
                    let session_id = snapshot.session.session.session_id;
                    let (started_session, started_operation) = started
                        .get(&command_id)
                        .expect("each completion must match its own start");
                    assert_eq!(&session_id, started_session);
                    assert_eq!(&operation_id, started_operation);
                    finished.insert(command_id, session_id);
                }
                DesktopRuntimeUpdate::ResyncRequired { .. } => {}
                update => panic!("unexpected concurrent prompt update: {update:?}"),
            }
        }
    })
    .await
    .expect("both offline prompts should finish concurrently");

    assert_eq!(accepted, std::collections::BTreeSet::from([102, 103]));
    assert_eq!(finished.get(&102), Some(&first_session));
    assert_eq!(finished.get(&103), Some(&second_session));
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn project_workspace_owners_isolate_context_model_profile_and_events() {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let home = temp.path().join("home");
    let project_a = temp.path().join("project-a");
    let project_b = temp.path().join("project-b");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    write_workspace_fixture(&project_a, "project-a", "low");
    write_workspace_fixture(&project_b, "project-b", "high");
    let _env = ProcessEnvGuard::isolated(&global);
    let options =
        CodingAgentEmbeddingOptions::for_workspace(CodingAgentWorkspaceSelection::project(&home))
            .unwrap()
            .with_session_dir(&sessions)
            .with_model_id("claude-sonnet-4-5");
    let (mut bridge, _) = DesktopRuntimeBridge::spawn(options)
        .unwrap()
        .wait_blocking()
        .unwrap();

    bridge
        .try_submit_prompt(
            104,
            DesktopPromptTarget::new(
                CodingAgentWorkspaceSelection::project(&project_a),
                "claude-sonnet-4-5",
                "project-a",
            ),
            "project a prompt",
            None,
        )
        .unwrap();
    bridge
        .try_submit_prompt(
            105,
            DesktopPromptTarget::new(
                CodingAgentWorkspaceSelection::project(&project_b),
                "claude-haiku-4-5",
                "project-b",
            ),
            "project b prompt",
            None,
        )
        .unwrap();

    let mut accepted = std::collections::BTreeMap::new();
    let mut finished = std::collections::BTreeMap::new();
    let mut event_sessions = std::collections::BTreeSet::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while finished.len() < 2 {
            match bridge.next_update().await.unwrap() {
                DesktopRuntimeUpdate::PromptAcceptedWithSession {
                    command_id,
                    snapshot,
                } => {
                    accepted.insert(command_id, snapshot);
                }
                DesktopRuntimeUpdate::PromptStarted { .. }
                | DesktopRuntimeUpdate::ResyncRequired { .. } => {}
                DesktopRuntimeUpdate::ProductEvent { session_id, .. } => {
                    event_sessions.insert(session_id);
                }
                DesktopRuntimeUpdate::PromptFinished {
                    command_id,
                    snapshot,
                    ..
                } => {
                    finished.insert(command_id, snapshot.session.session.session_id);
                }
                update => panic!("unexpected multi-project prompt update: {update:?}"),
            }
        }
    })
    .await
    .expect("both project-scoped prompts should finish");

    let accepted_a = accepted.get(&104).expect("project A must be accepted");
    let accepted_b = accepted.get(&105).expect("project B must be accepted");
    let canonical_a = project_a.canonicalize().unwrap();
    let canonical_b = project_b.canonicalize().unwrap();
    assert_eq!(accepted_a.project.cwd, canonical_a);
    assert_eq!(accepted_b.project.cwd, canonical_b);
    assert_eq!(accepted_a.project.selected_model_id, "claude-sonnet-4-5");
    assert_eq!(accepted_b.project.selected_model_id, "claude-haiku-4-5");
    assert_eq!(
        accepted_a.project.default_agent_profile_id.as_str(),
        "project-a"
    );
    assert_eq!(
        accepted_b.project.default_agent_profile_id.as_str(),
        "project-b"
    );
    assert!(
        accepted_a
            .project
            .resources
            .skill_names
            .iter()
            .any(|name| name == "project-a-skill")
    );
    assert!(
        !accepted_a
            .project
            .resources
            .skill_names
            .iter()
            .any(|name| name == "project-b-skill")
    );
    assert!(
        accepted_b
            .project
            .resources
            .context_files
            .contains(&canonical_b.join("AGENTS.md"))
    );
    assert!(
        !accepted_b
            .project
            .resources
            .context_files
            .contains(&canonical_a.join("AGENTS.md"))
    );
    let session_a = accepted_a.session.session.session_id.clone();
    let session_b = accepted_b.session.session.session_id.clone();
    assert_eq!(finished.get(&104), Some(&session_a));
    assert_eq!(finished.get(&105), Some(&session_b));
    assert_eq!(
        event_sessions,
        std::collections::BTreeSet::from([session_a.clone(), session_b.clone()])
    );

    bridge.try_open_session(106, &session_a).unwrap();
    let Some(DesktopRuntimeUpdate::SessionChanged { snapshot, .. }) = bridge.next_update().await
    else {
        panic!("project A owner should be focusable after prompt completion");
    };
    assert_eq!(snapshot.project.cwd, canonical_a);
    bridge
        .try_select_model(107, session_owner_target(&session_a), "gpt-5")
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::SelectionChanged { metadata, .. })
            if metadata.project.cwd == canonical_a
                && metadata.project.selected_model_id == "gpt-5"
    ));

    bridge.try_open_session(108, &session_b).unwrap();
    let Some(DesktopRuntimeUpdate::SessionChanged { snapshot, .. }) = bridge.next_update().await
    else {
        panic!("project B owner should remain independently focusable");
    };
    assert_eq!(snapshot.project.cwd, canonical_b);
    assert_eq!(snapshot.project.selected_model_id, "claude-haiku-4-5");
    assert_eq!(
        snapshot.session.session.default_agent_profile_id.as_str(),
        "project-b"
    );

    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn fifth_open_session_is_rejected_without_disturbing_the_existing_four() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (mut bridge, first) = start_runtime(options).await;
    let first_session = first.session.session.session_id;
    for command_id in 111..114 {
        bridge.try_create_session(command_id).unwrap();
        assert!(matches!(
            bridge.next_update().await,
            Some(DesktopRuntimeUpdate::SessionChanged { command_id: completed, .. })
                if completed == command_id
        ));
    }

    bridge.try_create_session(114).unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 114,
            command: DesktopRuntimeCommandKind::CreateSession,
            ref code,
            ..
        }) if code == "session_limit_reached"
    ));

    bridge.try_close_session(115, &first_session).unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::SessionClosed {
            command_id: 115,
            session_id,
        }) if session_id == first_session
    ));
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn closing_one_active_session_does_not_interrupt_another_prompt() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (mut bridge, first) = start_runtime(options).await;
    let first_session = first.session.session.session_id;
    bridge.try_create_session(121).unwrap();
    let Some(DesktopRuntimeUpdate::SessionChanged {
        snapshot: second, ..
    }) = bridge.next_update().await
    else {
        panic!("second session should be created");
    };
    let second_session = second.session.session.session_id;
    bridge
        .try_submit_prompt(
            122,
            existing_prompt_target(&first_session),
            "close this prompt",
            None,
        )
        .unwrap();
    bridge
        .try_submit_prompt(
            123,
            existing_prompt_target(&second_session),
            "keep this prompt",
            None,
        )
        .unwrap();
    bridge.try_close_session(124, &first_session).unwrap();

    let mut closed = false;
    let mut second_finished = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !closed || !second_finished {
            match bridge.next_update().await.unwrap() {
                DesktopRuntimeUpdate::SessionClosed {
                    command_id: 124,
                    session_id,
                } => {
                    assert_eq!(session_id, first_session);
                    closed = true;
                }
                DesktopRuntimeUpdate::PromptFinished {
                    command_id: 123,
                    snapshot,
                    ..
                } => {
                    assert_eq!(snapshot.session.session.session_id, second_session);
                    second_finished = true;
                }
                DesktopRuntimeUpdate::RuntimeFailed { error } => {
                    panic!("closing one session failed the shared runtime: {error:?}")
                }
                _ => {}
            }
        }
    })
    .await
    .expect("the surviving prompt should finish after the other session closes");
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn sessionless_prompt_atomically_creates_and_accepts_one_session() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (mut bridge, _) = DesktopRuntimeBridge::spawn(options)
        .unwrap()
        .wait_blocking()
        .unwrap();

    bridge
        .try_submit_prompt(
            13,
            new_project_prompt_target(&temp),
            "first desktop prompt",
            None,
        )
        .unwrap();
    let Some(DesktopRuntimeUpdate::PromptAcceptedWithSession {
        command_id: 13,
        snapshot: created,
    }) = bridge.next_update().await
    else {
        panic!("first prompt should atomically publish its created session");
    };
    let session_id = created.session.session.session_id.clone();
    assert!(created.transcript.items.is_empty());
    let mut projection = DesktopProjection::new(created).unwrap();

    let finished = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let update = bridge.next_update().await.unwrap();
            assert!(!matches!(
                projection.apply(update.clone()),
                DesktopProjectionApply::NeedsResync
            ));
            if let DesktopRuntimeUpdate::PromptFinished {
                command_id: 13,
                snapshot,
                ..
            } = update
            {
                assert_eq!(snapshot.session.session.session_id, session_id);
                assert!(snapshot.transcript.items.iter().any(|item| matches!(
                    item,
                    CodingAgentSessionTranscriptItem::User { text }
                        if text == "first desktop prompt"
                )));
                break;
            }
        }
    })
    .await;
    assert!(finished.is_ok(), "first sessionless prompt did not finish");

    bridge.try_list_sessions(14).unwrap();
    let Some(DesktopRuntimeUpdate::SessionsListed { sessions, .. }) = bridge.next_update().await
    else {
        panic!("created prompt session should be visible in the catalog");
    };
    assert_eq!(
        sessions
            .iter()
            .filter(|session| session.session_id == session_id)
            .count(),
        1
    );
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn new_prompt_context_load_failure_creates_no_session_owner_or_manifest() {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let home = temp.path().join("home");
    let target = temp.path().join("target");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    let _env = ProcessEnvGuard::isolated(&global);
    let home_options =
        CodingAgentEmbeddingOptions::for_workspace(CodingAgentWorkspaceSelection::project(&home))
            .unwrap()
            .with_session_dir(&sessions)
            .with_model_id("claude-sonnet-4-5");
    let mut state = RuntimeState {
        home: HomeRuntimeContext::load(home_options).unwrap(),
        workspaces: std::collections::HashMap::new(),
        focused_session_id: None,
        fail_next_prompt_start: false,
    };
    let mut active = std::collections::HashMap::new();

    let update = dispatch_command(
        &mut state,
        &mut active,
        DesktopRuntimeCommand::SubmitPrompt {
            command_id: 131,
            target: DesktopPromptTarget::new(
                CodingAgentWorkspaceSelection::project(&target),
                "missing-desktop-context-model",
                "default",
            ),
            prompt: "context must load before persistence".into(),
            attachments: Vec::new(),
            thinking_level: None,
        },
    )
    .await;

    assert!(matches!(
        update,
        DesktopRuntimeUpdate::CommandRejected {
            command_id: 131,
            command: DesktopRuntimeCommandKind::SubmitPrompt,
            ..
        }
    ));
    assert!(state.workspaces.is_empty());
    assert!(active.is_empty());
    assert!(!sessions.exists());
}

#[tokio::test]
async fn workspace_deleted_after_admission_creates_no_session_owner_or_manifest() {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let home = temp.path().join("home");
    let target = temp.path().join("target");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    let _env = ProcessEnvGuard::isolated(&global);
    let home_options =
        CodingAgentEmbeddingOptions::for_workspace(CodingAgentWorkspaceSelection::project(&home))
            .unwrap()
            .with_session_dir(&sessions)
            .with_model_id("claude-sonnet-4-5");
    let mut state = RuntimeState {
        home: HomeRuntimeContext::load(home_options).unwrap(),
        workspaces: std::collections::HashMap::new(),
        focused_session_id: None,
        fail_next_prompt_start: false,
    };
    let prompt_target = DesktopPromptTarget::new(
        CodingAgentWorkspaceSelection::project(&target),
        "claude-sonnet-4-5",
        "default",
    );
    validate_prompt_target(&prompt_target).expect("the target is valid at admission time");
    std::fs::remove_dir(&target).unwrap();
    let mut active = std::collections::HashMap::new();

    let update = dispatch_command(
        &mut state,
        &mut active,
        DesktopRuntimeCommand::SubmitPrompt {
            command_id: 132,
            target: prompt_target,
            prompt: "the runtime must resolve the target again".into(),
            attachments: Vec::new(),
            thinking_level: None,
        },
    )
    .await;

    assert!(matches!(
        update,
        DesktopRuntimeUpdate::CommandRejected {
            command_id: 132,
            command: DesktopRuntimeCommandKind::SubmitPrompt,
            ..
        }
    ));
    assert!(state.workspaces.is_empty());
    assert!(active.is_empty());
    assert!(!sessions.exists());
}

#[tokio::test]
async fn new_prompt_binds_model_profile_and_sanitized_thinking_before_persistence() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let mut state = RuntimeState {
        home: HomeRuntimeContext::load(options).unwrap(),
        workspaces: std::collections::HashMap::new(),
        focused_session_id: None,
        fail_next_prompt_start: false,
    };

    let created = state
        .create_session_for_workspace(
            CodingAgentWorkspaceSelection::project(temp.path().join("project")),
            "gpt-5".into(),
            "review".into(),
            Some(CodingAgentThinkingLevel::Off),
            0,
        )
        .await
        .unwrap();

    assert_eq!(
        created.thinking_level, None,
        "unsupported Off must fall back to Auto"
    );
    let session_id = created.session_id;
    let owner = state.workspaces.get(&session_id).unwrap();
    assert_eq!(owner.context.snapshot().selected_model_id, "gpt-5");
    assert_eq!(
        owner.context.snapshot().default_agent_profile_id.as_str(),
        "review"
    );
    assert_eq!(
        owner.session.view().default_agent_profile_id.as_str(),
        "review"
    );
    state.close_idle_session(&session_id).await.unwrap();
}

#[tokio::test]
async fn prompt_prepare_failure_retains_the_persisted_scoped_session() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let attachment = temp.path().join("project/deleted-attachment.txt");
    std::fs::write(&attachment, "prepare me").unwrap();
    let mut state = RuntimeState {
        home: HomeRuntimeContext::load(options).unwrap(),
        workspaces: std::collections::HashMap::new(),
        focused_session_id: None,
        fail_next_prompt_start: false,
    };
    validate_prompt_with_attachments("prepare failure", std::slice::from_ref(&attachment))
        .expect("the attachment path is admitted before it disappears");
    std::fs::remove_file(&attachment).unwrap();
    let mut active = std::collections::HashMap::new();

    let update = dispatch_command(
        &mut state,
        &mut active,
        DesktopRuntimeCommand::SubmitPrompt {
            command_id: 133,
            target: new_project_prompt_target(&temp),
            prompt: "prepare failure".into(),
            attachments: vec![attachment],
            thinking_level: None,
        },
    )
    .await;
    let DesktopRuntimeUpdate::PromptRejectedWithSession {
        command_id: 133,
        snapshot,
        ..
    } = update
    else {
        panic!("a post-persistence prepare failure must install the created session");
    };

    let session_id = snapshot.session.session.session_id.clone();
    let owner = state.workspaces.get(&session_id).unwrap();
    let resolved = snapshot.project.workspace.as_ref().unwrap();
    assert_eq!(&snapshot.project, owner.context.snapshot());
    assert_eq!(resolved.scope, owner.scope);
    assert_eq!(resolved.execution_cwd, snapshot.project.cwd);
    assert!(snapshot.transcript.items.is_empty());
    assert!(active.is_empty());
    let overview = state
        .session_catalog()
        .unwrap()
        .0
        .into_iter()
        .find(|overview| overview.session_id == session_id)
        .expect("the rejected prompt session remains durable");
    assert_eq!(overview.cwd.as_deref(), snapshot.project.cwd.to_str());
    state.close_idle_session(&session_id).await.unwrap();
}

#[tokio::test]
async fn explicit_new_prompt_target_creates_a_session_when_another_is_open() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (mut bridge, first) = start_runtime(options).await;
    let first_session_id = first.session.session.session_id;

    bridge
        .try_submit_prompt(
            14,
            new_project_prompt_target(&temp),
            "start a distinct conversation",
            None,
        )
        .unwrap();
    let Some(DesktopRuntimeUpdate::PromptAcceptedWithSession {
        command_id: 14,
        snapshot: second,
    }) = bridge.next_update().await
    else {
        panic!("an explicit New target must publish the newly created session");
    };
    assert_ne!(second.session.session.session_id, first_session_id);

    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn projectless_first_prompt_records_the_global_only_scratch_cwd() {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let scratch = global.join("scratch/workspace-runtime-test");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir_all(scratch.join(".evo")).unwrap();
    std::fs::write(
        scratch.join(".evo/settings.toml"),
        "default_thinking_level = \"high\"\n",
    )
    .unwrap();
    let _env = ProcessEnvGuard::isolated(&global);
    let options = CodingAgentEmbeddingOptions::for_workspace(
        CodingAgentWorkspaceSelection::projectless("workspace-runtime-test"),
    )
    .unwrap()
    .with_session_dir(&sessions)
    .with_model_id("claude-sonnet-4-5");
    let (mut bridge, ready) = DesktopRuntimeBridge::spawn(options)
        .unwrap()
        .wait_blocking()
        .unwrap();
    assert_eq!(ready.project.cwd, scratch);
    assert_ne!(
        ready.project.settings.default_thinking_level.as_deref(),
        Some("high"),
        "scratch-local project settings must not enter a global-only context"
    );

    bridge
        .try_submit_prompt(
            16,
            DesktopPromptTarget::new(
                CodingAgentWorkspaceSelection::projectless("workspace-runtime-test"),
                "claude-sonnet-4-5",
                "default",
            ),
            "scratch workspace prompt",
            None,
        )
        .unwrap();
    let Some(DesktopRuntimeUpdate::PromptAcceptedWithSession { snapshot, .. }) =
        bridge.next_update().await
    else {
        panic!("the first scratch prompt should atomically create its session");
    };
    let session_id = snapshot.session.session.session_id;
    let catalog =
        coding_agent::api::embedding::CodingAgentSessionQuery::from_session_root(&sessions)
            .overviews()
            .unwrap();
    let overview = catalog
        .overviews
        .iter()
        .find(|overview| overview.session_id == session_id)
        .expect("the scratch session should be visible in the durable overview");
    assert_eq!(
        overview.workspace.kind,
        coding_agent::api::view::CodingAgentWorkspaceKind::Projectless
    );
    assert_eq!(overview.workspace.display_path, None);
    assert_eq!(overview.cwd, None);

    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn session_creation_failure_rejects_the_first_prompt_without_an_active_owner() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, _) = isolated_options(&temp);
    let blocked_session_root = temp.path().join("blocked-session-root");
    std::fs::write(&blocked_session_root, "not a directory").unwrap();
    let options = CodingAgentEmbeddingOptions::for_workspace(
        CodingAgentWorkspaceSelection::project(temp.path().join("project")),
    )
    .unwrap()
    .with_session_dir(&blocked_session_root)
    .with_model_id("claude-sonnet-4-5");
    let (mut bridge, _) = DesktopRuntimeBridge::spawn(options)
        .unwrap()
        .wait_blocking()
        .unwrap();

    bridge
        .try_submit_prompt(
            15,
            new_project_prompt_target(&temp),
            "cannot create this session",
            None,
        )
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 15,
            command: DesktopRuntimeCommandKind::SubmitPrompt,
            ..
        })
    ));
    bridge.try_resync(151).unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 151,
            command: DesktopRuntimeCommandKind::Resync,
            code,
            ..
        }) if code == "session"
    ));
    assert!(blocked_session_root.is_file());
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn prompt_start_failure_reports_the_session_that_was_already_created() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    std::fs::write(
        temp.path().join("project/scope-proof.txt"),
        "the selected context resolved this relative attachment",
    )
    .unwrap();
    let mut state = RuntimeState {
        home: HomeRuntimeContext::load(options).unwrap(),
        workspaces: std::collections::HashMap::new(),
        focused_session_id: None,
        fail_next_prompt_start: true,
    };
    let mut active = std::collections::HashMap::new();

    let update = dispatch_command(
        &mut state,
        &mut active,
        DesktopRuntimeCommand::SubmitPrompt {
            command_id: 16,
            target: new_project_prompt_target(&temp),
            prompt: "prompt start failure".into(),
            attachments: vec![std::path::PathBuf::from("scope-proof.txt")],
            thinking_level: None,
        },
    )
    .await;
    let DesktopRuntimeUpdate::PromptRejectedWithSession {
        command_id: 16,
        snapshot,
        error,
    } = update
    else {
        panic!("post-creation failure must report the retained session atomically");
    };
    assert_eq!(error.code, "session");
    assert_eq!(error.message, "injected desktop prompt start failure");
    assert!(active.is_empty());
    let retained_session_id = snapshot.session.session.session_id.clone();
    let owner = state.workspaces.get(&retained_session_id).unwrap();
    let resolved = snapshot.project.workspace.as_ref().unwrap();
    assert_eq!(&snapshot.project, owner.context.snapshot());
    assert_eq!(resolved.scope, owner.scope);
    assert_eq!(resolved.execution_cwd, snapshot.project.cwd);
    assert_eq!(
        state
            .workspaces
            .get(&retained_session_id)
            .unwrap()
            .session
            .view()
            .session_id,
        snapshot.session.session.session_id
    );
    assert_eq!(state.session_catalog().unwrap().0.len(), 1);

    let mut workspace = state.workspaces.remove(&retained_session_id).unwrap();
    workspace.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn desktop_projection_rejects_gaps_and_association_mismatches_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (mut bridge, initial) = start_runtime(options).await;
    let session_id = initial.session.session.session_id.clone();
    let mut wrong_transcript = initial.clone();
    wrong_transcript.transcript.session_id = "wrong-session".into();
    assert_eq!(
        DesktopProjection::new(wrong_transcript).unwrap_err().code,
        "transcript_session_mismatch"
    );
    let mut projection = DesktopProjection::new(initial).unwrap();
    bridge
        .try_submit_prompt(
            40,
            existing_prompt_target(session_id),
            "projection cursor fixture",
            None,
        )
        .unwrap();

    let mut exercised_strict_reducer = false;
    let mut requested_active_resync = false;
    let mut saw_active_resync = false;
    let mut saw_finished = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let update = bridge.next_update().await.unwrap();
            if matches!(update, DesktopRuntimeUpdate::PromptStarted { .. })
                && !requested_active_resync
            {
                bridge.try_resync(41).unwrap();
                requested_active_resync = true;
            }
            if let DesktopRuntimeUpdate::Resynced { command_id: 41, .. } = &update {
                saw_active_resync = true;
            }
            if let DesktopRuntimeUpdate::ProductEvent { event, .. } = &update
                && !exercised_strict_reducer
            {
                let mut baseline = projection.clone();
                let expected = baseline.cursor().last_event_sequence + 1;
                let submitted_operation = baseline
                    .snapshot()
                    .submitted_operation
                    .as_ref()
                    .map(|operation| operation.operation_id.clone());

                let valid = rewritten_event(
                    event,
                    expected,
                    baseline.cursor().stream_id.as_str(),
                    Some(baseline.snapshot().session.session_id.as_str()),
                    submitted_operation.as_deref(),
                );
                assert!(
                    baseline
                        .apply(DesktopRuntimeUpdate::product_event(valid.clone()))
                        .is_applied()
                );
                assert_eq!(
                    baseline.apply(DesktopRuntimeUpdate::product_event(valid)),
                    DesktopProjectionApply::IgnoredDuplicate
                );

                let mut gap_projection = projection.clone();
                let original_cursor = gap_projection.cursor().clone();
                let gap = rewritten_event(
                    event,
                    expected + 1,
                    gap_projection.cursor().stream_id.as_str(),
                    Some(gap_projection.snapshot().session.session_id.as_str()),
                    submitted_operation.as_deref(),
                );
                assert_eq!(
                    gap_projection.apply(DesktopRuntimeUpdate::product_event(gap)),
                    DesktopProjectionApply::NeedsResync
                );
                assert_eq!(gap_projection.cursor(), &original_cursor);
                assert_eq!(
                    gap_projection.lifecycle(),
                    DesktopProjectionLifecycle::NeedsResync
                );
                assert!(
                    gap_projection
                        .apply(DesktopRuntimeUpdate::ResyncRequired {
                            reason: DesktopRuntimeError {
                                code: "test_resync".into(),
                                message: "replace after an injected cursor gap".into(),
                            },
                            snapshot: projection.snapshot().clone(),
                        })
                        .is_replaced()
                );
                assert_eq!(
                    gap_projection.lifecycle(),
                    DesktopProjectionLifecycle::Running
                );
                assert!(gap_projection.recent_events().is_empty());

                let mut wrong_session = projection.clone();
                let mismatched = rewritten_event(
                    event,
                    expected,
                    wrong_session.cursor().stream_id.as_str(),
                    Some("another-session"),
                    submitted_operation.as_deref(),
                );
                assert_eq!(
                    wrong_session.apply(DesktopRuntimeUpdate::product_event(mismatched)),
                    DesktopProjectionApply::NeedsResync
                );
                assert_eq!(
                    wrong_session.issues().back().unwrap().code,
                    "product_event_session_mismatch"
                );

                let mut wrong_stream = projection.clone();
                let mismatched = rewritten_event(
                    event,
                    expected,
                    "another-stream",
                    Some(wrong_stream.snapshot().session.session_id.as_str()),
                    submitted_operation.as_deref(),
                );
                assert_eq!(
                    wrong_stream.apply(DesktopRuntimeUpdate::product_event(mismatched)),
                    DesktopProjectionApply::NeedsResync
                );
                assert_eq!(
                    wrong_stream.issues().back().unwrap().code,
                    "product_event_stream_mismatch"
                );

                let mut wrong_generation = projection.clone();
                let mut value = serde_json::to_value(rewritten_event(
                    event,
                    expected,
                    wrong_generation.cursor().stream_id.as_str(),
                    Some(wrong_generation.snapshot().session.session_id.as_str()),
                    submitted_operation.as_deref(),
                ))
                .unwrap();
                value["capability_generation"] = serde_json::json!(
                    wrong_generation
                        .cursor()
                        .capability_generation
                        .saturating_add(2)
                );
                let mismatched = serde_json::from_value(value).unwrap();
                assert_eq!(
                    wrong_generation.apply(DesktopRuntimeUpdate::product_event(mismatched)),
                    DesktopProjectionApply::NeedsResync
                );
                assert_eq!(
                    wrong_generation.issues().back().unwrap().code,
                    "product_event_capability_generation_mismatch"
                );

                if submitted_operation.is_some() {
                    let mut wrong_operation = projection.clone();
                    let mismatched = rewritten_event(
                        event,
                        expected,
                        wrong_operation.cursor().stream_id.as_str(),
                        Some(wrong_operation.snapshot().session.session_id.as_str()),
                        Some("unrelated-operation"),
                    );
                    assert_eq!(
                        wrong_operation.apply(DesktopRuntimeUpdate::product_event(mismatched)),
                        DesktopProjectionApply::NeedsResync
                    );
                    assert_eq!(
                        wrong_operation.issues().back().unwrap().code,
                        "product_event_operation_mismatch"
                    );
                }
                assert_bounded_streaming_overlays(
                    &projection,
                    event,
                    submitted_operation.as_deref(),
                );
                exercised_strict_reducer = true;
            }

            saw_finished |= matches!(update, DesktopRuntimeUpdate::PromptFinished { .. });
            let outcome = projection.apply(update);
            assert_ne!(
                outcome,
                DesktopProjectionApply::NeedsResync,
                "real runtime updates must satisfy the desktop projection contract: {:?}",
                projection.issues().back()
            );
            if saw_finished && saw_active_resync {
                break;
            }
        }
    })
    .await
    .expect("projection fixture prompt must finish");
    assert!(exercised_strict_reducer);
    assert!(saw_active_resync);
    assert_eq!(projection.lifecycle(), DesktopProjectionLifecycle::Running);
    assert!(
        projection
            .conversation()
            .blocks()
            .iter()
            .any(|block| block.text == "projection cursor fixture")
    );
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_cross_adapter_fixture_matches_desktop_product_state_exactly() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (bridge, initial) = start_runtime(options).await;
    let transcript = initial.transcript.clone();
    let mut shared = CodingAgentClientProjection::from_bootstrap(CodingAgentClientBootstrap {
        snapshot: initial.session.clone(),
        transcript,
        pending_recoveries: initial.pending_recoveries.clone(),
    })
    .unwrap();
    let mut desktop = DesktopProjection::new(initial).unwrap();
    let base_sequence = desktop.cursor().last_event_sequence;
    let stream_id = desktop.cursor().stream_id.clone();
    let session_id = desktop.snapshot().session.session_id.clone();

    for fixture in cross_adapter_fixture_events() {
        let event = rewritten_event(
            &fixture,
            base_sequence + fixture.sequence(),
            &stream_id,
            Some(&session_id),
            fixture.operation_id(),
        );
        assert!(matches!(
            shared.apply(&event),
            CodingAgentClientProjectionApply::Applied(_)
        ));
        let terminal = event.terminal_operation().is_some();
        let outcome = desktop.apply(DesktopRuntimeUpdate::product_event(event));
        assert!(outcome.is_applied());
        assert_eq!(outcome.delta().unwrap().terminal, terminal);
    }

    assert_eq!(desktop.product_for_tests(), &shared);
    assert_eq!(
        desktop
            .messages()
            .front()
            .map(|message| message.text.as_str()),
        Some("hello world")
    );
    assert_eq!(
        desktop.tools().front().map(|tool| tool.detail.as_str()),
        Some("read complete")
    );
    assert_eq!(
        desktop.snapshot().context.delegations[0].status,
        "completed"
    );
    assert_eq!(
        desktop.snapshot().session.default_agent_profile_id.as_str(),
        "reviewer"
    );
    bridge.shutdown().await.unwrap();
}

fn rewritten_event(
    event: &CodingAgentProductEvent,
    sequence: u64,
    stream_id: &str,
    session_id: Option<&str>,
    operation_id: Option<&str>,
) -> CodingAgentProductEvent {
    let mut value = serde_json::to_value(event).unwrap();
    value["sequence"] = serde_json::json!(sequence);
    value["stream_id"] = serde_json::json!(stream_id);
    value["session_id"] = session_id.map_or(serde_json::Value::Null, |session_id| {
        serde_json::json!(session_id)
    });
    value["operation_id"] = operation_id.map_or(serde_json::Value::Null, |operation_id| {
        serde_json::json!(operation_id)
    });
    value["parent_operation_id"] = serde_json::Value::Null;
    value["root_operation_id"] = serde_json::Value::Null;
    serde_json::from_value(value).unwrap()
}

fn cross_adapter_fixture_events() -> Vec<CodingAgentProductEvent> {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../coding-agent/tests/fixtures/client_projection/cross-adapter-events.json"
    )))
    .expect("the shared client-projection fixture must deserialize")
}

fn rewritten_event_kind(
    event: &CodingAgentProductEvent,
    sequence: u64,
    stream_id: &str,
    session_id: &str,
    operation_id: &str,
    kind: serde_json::Value,
) -> CodingAgentProductEvent {
    let rewritten = rewritten_event(
        event,
        sequence,
        stream_id,
        Some(session_id),
        Some(operation_id),
    );
    let mut value = serde_json::to_value(rewritten).unwrap();
    value["event"] = kind;
    value["terminal_status"] = serde_json::Value::Null;
    value["terminal_operation"] = serde_json::Value::Null;
    serde_json::from_value(value).unwrap()
}

fn assert_bounded_streaming_overlays(
    projection: &DesktopProjection,
    base_event: &CodingAgentProductEvent,
    submitted_operation: Option<&str>,
) {
    let Some(operation_id) = submitted_operation else {
        return;
    };
    let mut overlays = projection.clone();
    let stream_id = overlays.cursor().stream_id.clone();
    let session_id = overlays.snapshot().session.session_id.clone();
    let initial_usage_input = overlays.snapshot().context.usage.input;
    let initial_usage_output = overlays.snapshot().context.usage.output;
    let initial_view_rebuilds = overlays.counters().product_view_rebuilds;
    let mut sequence = overlays.cursor().last_event_sequence;

    sequence += 1;
    let started = rewritten_event_kind(
        base_event,
        sequence,
        &stream_id,
        &session_id,
        operation_id,
        serde_json::json!({
            "family": "message",
            "payload": {
                "kind": "started",
                "operation_id": operation_id,
                "turn_id": "turn-overlay",
                "message_id": "message-overlay"
            }
        }),
    );
    let outcome = overlays.apply(DesktopRuntimeUpdate::product_event(started));
    assert!(outcome.is_applied());
    let delta = outcome.delta().unwrap();
    assert!(delta.cursor);
    assert!(delta.conversation);
    assert!(!delta.tools);
    assert!(!delta.context.contains(ContextDirtyFlags::USAGE));

    sequence += 1;
    let delta = rewritten_event_kind(
        base_event,
        sequence,
        &stream_id,
        &session_id,
        operation_id,
        serde_json::json!({
            "family": "message",
            "payload": {
                "kind": "delta",
                "operation_id": operation_id,
                "turn_id": "turn-overlay",
                "message_id": "message-overlay",
                "text": "streaming text"
            }
        }),
    );
    assert!(
        overlays
            .apply(DesktopRuntimeUpdate::product_event(delta))
            .is_applied()
    );

    sequence += 1;
    let completed = rewritten_event_kind(
        base_event,
        sequence,
        &stream_id,
        &session_id,
        operation_id,
        serde_json::json!({
            "family": "message",
            "payload": {
                "kind": "completed",
                "operation_id": operation_id,
                "turn_id": "turn-overlay",
                "message_id": "message-overlay",
                "final_text": "final text",
                "images": [],
                "usage": {
                    "input": 1,
                    "output": 2,
                    "cache_read": 0,
                    "cache_write": 0,
                    "total_tokens": 3,
                    "cost_known": false,
                    "input_cost": 0.0,
                    "output_cost": 0.0,
                    "cache_read_cost": 0.0,
                    "cache_write_cost": 0.0
                }
            }
        }),
    );
    let outcome = overlays.apply(DesktopRuntimeUpdate::product_event(completed));
    assert!(outcome.is_applied());
    let delta = outcome.delta().unwrap();
    assert!(delta.conversation);
    assert!(delta.context.contains(ContextDirtyFlags::USAGE));
    let message = overlays.messages().back().unwrap();
    assert_eq!(message.text, "final text");
    assert_eq!(message.status, DesktopMessageStatus::Completed);
    assert_eq!(
        overlays.snapshot().context.usage.input,
        initial_usage_input + 1
    );
    assert_eq!(
        overlays.snapshot().context.usage.output,
        initial_usage_output + 2
    );

    for index in 0..=MAX_DESKTOP_MESSAGE_OVERLAYS {
        sequence += 1;
        let completed = rewritten_event_kind(
            base_event,
            sequence,
            &stream_id,
            &session_id,
            operation_id,
            serde_json::json!({
                "family": "message",
                "payload": {
                    "kind": "completed",
                    "operation_id": operation_id,
                    "turn_id": format!("turn-{index}"),
                    "message_id": format!("message-{index}"),
                    "final_text": "bounded",
                    "images": [],
                    "usage": {
                        "input": 0,
                        "output": 0,
                        "cache_read": 0,
                        "cache_write": 0,
                        "total_tokens": 0,
                        "cost_known": false,
                        "input_cost": 0.0,
                        "output_cost": 0.0,
                        "cache_read_cost": 0.0,
                        "cache_write_cost": 0.0
                    }
                }
            }),
        );
        assert!(
            overlays
                .apply(DesktopRuntimeUpdate::product_event(completed))
                .is_applied()
        );
    }
    assert_eq!(overlays.messages().len(), MAX_DESKTOP_MESSAGE_OVERLAYS);

    sequence += 1;
    let tool_started = rewritten_event_kind(
        base_event,
        sequence,
        &stream_id,
        &session_id,
        operation_id,
        serde_json::json!({
            "family": "tool",
            "payload": {
                "kind": "started",
                "operation_id": operation_id,
                "turn_id": "turn-tool",
                "tool_call_id": "tool-overlay",
                "name": "edit",
                "arguments_json": "{\"path\":\"README.md\"}"
            }
        }),
    );
    assert!(
        overlays
            .apply(DesktopRuntimeUpdate::product_event(tool_started))
            .is_applied()
    );
    sequence += 1;
    let tool_completed = rewritten_event_kind(
        base_event,
        sequence,
        &stream_id,
        &session_id,
        operation_id,
        serde_json::json!({
            "family": "tool",
            "payload": {
                "kind": "completed",
                "operation_id": operation_id,
                "turn_id": "turn-tool",
                "tool_call_id": "tool-overlay",
                "name": "edit",
                "summary": "edited README.md"
            }
        }),
    );
    let outcome = overlays.apply(DesktopRuntimeUpdate::product_event(tool_completed));
    assert!(outcome.is_applied());
    let delta = outcome.delta().unwrap();
    assert!(delta.tools);
    assert!(delta.context.contains(ContextDirtyFlags::CHANGES));
    assert!(!delta.conversation);
    assert_eq!(
        overlays.tools().back().unwrap().status,
        DesktopToolStatus::Completed
    );
    assert_eq!(
        overlays.snapshot().context.changes.first().unwrap().path,
        "README.md"
    );

    sequence += 1;
    let delegation = rewritten_event_kind(
        base_event,
        sequence,
        &stream_id,
        &session_id,
        operation_id,
        serde_json::json!({
            "family": "delegation",
            "payload": {
                "kind": "started",
                "context": {
                    "operation_id": operation_id,
                    "turn_id": "turn-delegation",
                    "tool_call_id": "delegation-overlay",
                    "requesting_profile_id": "default",
                    "target_kind": "agent",
                    "target_id": "reviewer",
                    "task": "review projection"
                },
                "child_operation_id": "child-overlay"
            }
        }),
    );
    let outcome = overlays.apply(DesktopRuntimeUpdate::product_event(delegation));
    assert!(outcome.is_applied());
    let delta = outcome.delta().unwrap();
    assert!(delta.context.contains(ContextDirtyFlags::DELEGATIONS));
    assert!(!delta.conversation);
    assert!(!delta.tools);
    assert_eq!(
        overlays
            .snapshot()
            .context
            .delegations
            .first()
            .unwrap()
            .status,
        "running"
    );

    sequence += 1;
    let recovery = rewritten_event_kind(
        base_event,
        sequence,
        &stream_id,
        &session_id,
        operation_id,
        serde_json::json!({
            "family": "workflow",
            "payload": {
                "kind": "operation_recovery_pending",
                "operation_id": operation_id,
                "recovery_id": "recovery-overlay",
                "reason": "injected recovery",
                "record_version": 1,
                "descriptor_revision": 1,
                "capability_generation": null,
                "attempt_count": 0,
                "last_attempt_at": null,
                "next_attempt_at": null
            }
        }),
    );
    let outcome = overlays.apply(DesktopRuntimeUpdate::product_event(recovery));
    assert!(outcome.is_applied());
    assert!(outcome.delta().unwrap().recoveries);
    assert_eq!(
        overlays.recoveries().front().unwrap().status,
        crate::projection::DesktopRecoveryStatus::Pending
    );

    sequence += 1;
    let diagnostic = rewritten_event_kind(
        base_event,
        sequence,
        &stream_id,
        &session_id,
        operation_id,
        serde_json::json!({
            "family": "diagnostic",
            "payload": {
                "kind": "diagnostic",
                "diagnostic": {
                    "severity": "warning",
                    "code": "projection_diagnostic",
                    "summary": "projection diagnostic",
                    "origin": "runtime",
                    "operation_id": operation_id
                }
            }
        }),
    );
    let outcome = overlays.apply(DesktopRuntimeUpdate::product_event(diagnostic));
    assert!(outcome.is_applied());
    assert!(outcome.delta().unwrap().diagnostics);
    assert_eq!(
        overlays.diagnostics().back().unwrap().message,
        "projection diagnostic"
    );
    let incremental_counters = overlays.counters();
    assert_eq!(
        incremental_counters.product_view_rebuilds, initial_view_rebuilds,
        "product events must not rebuild every compatibility view"
    );
    assert!(incremental_counters.incremental_message_updates > 1);
    assert_eq!(incremental_counters.incremental_tool_updates, 2);
    assert_eq!(incremental_counters.incremental_recovery_updates, 1);
    assert_eq!(incremental_counters.incremental_diagnostic_updates, 1);

    let mut fresh = overlays.snapshot().clone();
    fresh.cursor = overlays.cursor().clone();
    assert!(
        overlays
            .apply(DesktopRuntimeUpdate::ResyncRequired {
                reason: DesktopRuntimeError {
                    code: "overlay_resync".into(),
                    message: "discard incomplete live overlays".into(),
                },
                snapshot: fresh,
            })
            .is_replaced()
    );
    assert!(overlays.messages().is_empty());
    assert!(overlays.tools().is_empty());
    assert_eq!(
        overlays.counters().product_view_rebuilds,
        initial_view_rebuilds + 1
    );
    assert_eq!(
        overlays
            .recoveries()
            .front()
            .map(|recovery| recovery.recovery_id.as_str()),
        Some("recovery-overlay")
    );
    assert!(!overlays.recoveries().front().unwrap().authoritative);
    assert!(overlays.diagnostics().is_empty());
}

#[tokio::test]
async fn command_queue_full_and_closed_are_typed_without_runtime_timing() {
    let (commands, _command_rx) = mpsc::channel(DESKTOP_COMMAND_QUEUE_CAPACITY);
    let (shutdown, _shutdown_rx) = watch::channel(false);
    let (_priority_updates_tx, priority_updates) =
        mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
    let (_data_updates_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
    let bridge = DesktopRuntimeBridge {
        shutdown: DesktopRuntimeShutdownGuard {
            shutdown,
            runtime_thread: None,
        },
        commands: Some(commands),
        events: DesktopRuntimeEventStream {
            priority_updates,
            data_updates,
            pending_priority_update: None,
            pending_data_update: None,
        },
    };

    for command_id in 0..DESKTOP_COMMAND_QUEUE_CAPACITY as u64 {
        bridge.try_reload(command_id, home_owner_target()).unwrap();
    }
    assert_eq!(
        bridge.try_reload(u64::MAX, home_owner_target()),
        Err(DesktopCommandAdmissionError::QueueFull)
    );
    drop(_command_rx);
    assert_eq!(
        bridge.try_reload(u64::MAX, home_owner_target()),
        Err(DesktopCommandAdmissionError::RuntimeClosed)
    );
}

#[tokio::test]
async fn streaming_batch_waits_only_for_data_and_flushes_on_priority_delivery() {
    let (commands, _command_rx) = mpsc::channel(DESKTOP_COMMAND_QUEUE_CAPACITY);
    let (shutdown, _shutdown_rx) = watch::channel(false);
    let (priority_tx, priority_updates) = mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
    let (data_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
    let mut bridge = DesktopRuntimeBridge {
        shutdown: DesktopRuntimeShutdownGuard {
            shutdown,
            runtime_thread: None,
        },
        commands: Some(commands),
        events: DesktopRuntimeEventStream {
            priority_updates,
            data_updates,
            pending_priority_update: None,
            pending_data_update: None,
        },
    };
    let fixture = cross_adapter_fixture_events();
    let data = fixture
        .iter()
        .find(|event| event.delivery_class() == CodingAgentProductEventDeliveryClass::Data)
        .cloned()
        .expect("fixture must contain a coalescible data event");
    let priority = fixture
        .iter()
        .find(|event| event.delivery_class() != CodingAgentProductEventDeliveryClass::Data)
        .cloned()
        .expect("fixture must contain an immediate event");

    data_tx
        .send(DesktopRuntimeUpdate::product_event(data.clone()))
        .await
        .unwrap();
    let priority_task = tokio::spawn(async move {
        tokio::task::yield_now().await;
        priority_tx
            .send(DesktopRuntimeUpdate::product_event(priority.clone()))
            .await
            .unwrap();
        priority
    });
    let batch = bridge.next_update_batch().await.unwrap();
    let priority = priority_task.await.unwrap();

    assert_eq!(batch.len(), 2);
    assert!(matches!(
        &batch[0],
        DesktopRuntimeUpdate::ProductEvent { event, .. } if event == &data
    ));
    assert!(matches!(
        &batch[1],
        DesktopRuntimeUpdate::ProductEvent { event, .. } if event == &priority
    ));
}

#[tokio::test]
async fn priority_and_data_merge_never_compares_sequences_across_sessions() {
    let (priority_tx, priority_updates) = mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
    let (data_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
    let fixture = cross_adapter_fixture_events();
    let data = fixture
        .iter()
        .find(|event| event.delivery_class() == CodingAgentProductEventDeliveryClass::Data)
        .cloned()
        .unwrap();
    let priority = fixture
        .iter()
        .find(|event| event.delivery_class() != CodingAgentProductEventDeliveryClass::Data)
        .cloned()
        .unwrap();
    data_tx
        .send(DesktopRuntimeUpdate::ProductEvent {
            session_id: "session-data".into(),
            event: data,
        })
        .await
        .unwrap();
    priority_tx
        .send(DesktopRuntimeUpdate::ProductEvent {
            session_id: "session-priority".into(),
            event: priority,
        })
        .await
        .unwrap();
    let mut events = DesktopRuntimeEventStream {
        priority_updates,
        data_updates,
        pending_priority_update: None,
        pending_data_update: None,
    };

    assert!(matches!(
        events.next_update().await,
        Some(DesktopRuntimeUpdate::ProductEvent { session_id, .. })
            if session_id == "session-priority"
    ));
    assert!(matches!(
        events.next_update().await,
        Some(DesktopRuntimeUpdate::ProductEvent { session_id, .. })
            if session_id == "session-data"
    ));
}

#[test]
fn streaming_batch_timer_does_not_require_a_tokio_reactor() {
    let (_priority_tx, priority_updates) = mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
    let (data_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
    let data = cross_adapter_fixture_events()
        .into_iter()
        .find(|event| event.delivery_class() == CodingAgentProductEventDeliveryClass::Data)
        .expect("fixture must contain a coalescible data event");
    data_tx
        .try_send(DesktopRuntimeUpdate::product_event(data))
        .unwrap();
    let mut events = DesktopRuntimeEventStream {
        priority_updates,
        data_updates,
        pending_priority_update: None,
        pending_data_update: None,
    };

    let mut future = std::pin::pin!(events.next_update_batch());
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    let batch = loop {
        match std::future::Future::poll(future.as_mut(), &mut context) {
            std::task::Poll::Ready(batch) => break batch.expect("data update should be ready"),
            std::task::Poll::Pending => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "executor-neutral coalescing timer did not complete"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    };

    assert_eq!(batch.len(), 1);
}

#[tokio::test]
async fn data_queue_overflow_emits_a_priority_resync_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (bridge, initial) = start_runtime(options).await;
    let (priority_updates, mut priority_rx) = mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
    let (data_updates, _data_rx) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
    for command_id in 0..DESKTOP_UPDATE_QUEUE_CAPACITY as u64 {
        data_updates
            .try_send(DesktopRuntimeUpdate::PromptAccepted { command_id })
            .unwrap();
    }

    assert!(
        publish_data_update(
            DesktopRuntimeUpdate::PromptAccepted {
                command_id: u64::MAX,
            },
            || Ok::<_, DesktopBridgeError>(initial.session.clone()),
            &priority_updates,
            &data_updates,
        )
        .await
    );
    let DesktopRuntimeUpdate::ResyncRequired { reason, snapshot } =
        priority_rx.recv().await.unwrap()
    else {
        panic!("data overflow must publish a priority resync request");
    };
    assert_eq!(reason.code, "desktop_data_queue_full");
    assert_eq!(
        snapshot.session.session_id,
        initial.session.session.session_id
    );
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn typed_recovery_reasons_replace_the_projection_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (bridge, initial) = start_runtime(options).await;
    let mut projection = DesktopProjection::new(initial.clone()).unwrap();
    let cursor = initial.session.cursor.clone();

    let live_lag = recovery_update(CodingAgentFreshSnapshotRecovery {
        requested_sequence: cursor.last_event_sequence.saturating_sub(1),
        oldest_available_sequence: cursor.last_event_sequence,
        fresh_cursor: cursor.clone(),
        reason: CodingAgentRecoveryReason::LiveReceiverLag,
        snapshot: Box::new(initial.session.clone()),
    });
    let DesktopRuntimeUpdate::ResyncRequired { reason, .. } = &live_lag else {
        panic!("live lag must become a typed resync update");
    };
    assert_eq!(reason.code, "product_event_live_receiver_lag");
    assert!(projection.apply(live_lag).is_replaced());
    assert_eq!(
        projection
            .last_resync_reason()
            .expect("live lag reason should be retained")
            .code,
        "product_event_live_receiver_lag"
    );

    let retained_gap = recovery_update(CodingAgentFreshSnapshotRecovery {
        requested_sequence: 0,
        oldest_available_sequence: cursor.last_event_sequence.saturating_add(1),
        fresh_cursor: cursor,
        reason: CodingAgentRecoveryReason::RetainedHistoryGap,
        snapshot: Box::new(initial.session),
    });
    let DesktopRuntimeUpdate::ResyncRequired { reason, .. } = &retained_gap else {
        panic!("retained gap must become a typed resync update");
    };
    assert_eq!(reason.code, "product_event_retained_history_gap");
    assert!(projection.apply(retained_gap).is_replaced());
    assert!(projection.recent_events().is_empty());
    assert_eq!(
        projection.apply(DesktopRuntimeUpdate::Stopped),
        DesktopProjectionApply::NoChange
    );
    assert_eq!(projection.lifecycle(), DesktopProjectionLifecycle::Stopped);
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn reconnect_state_machine_handles_gap_lag_and_exhaustion_deterministically() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (bridge, initial) = start_runtime(options).await;
    let cursor = initial.session.cursor.clone();

    let retained = CodingAgentFreshSnapshotRecovery {
        requested_sequence: 0,
        oldest_available_sequence: cursor.last_event_sequence.saturating_add(1),
        fresh_cursor: cursor.clone(),
        reason: CodingAgentRecoveryReason::RetainedHistoryGap,
        snapshot: Box::new(initial.session.clone()),
    };
    let mut attempts = VecDeque::from([
        DesktopReconnectAttempt::FreshSnapshotRequired(retained),
        DesktopReconnectAttempt::Replayed {
            events: Vec::new(),
            receiver: (),
        },
    ]);
    let mut requested = Vec::new();
    let (events, (), recovery) = establish_reconnect(0, |sequence| {
        requested.push(sequence);
        Ok(attempts
            .pop_front()
            .expect("two reconnect attempts should be consumed"))
    })
    .unwrap();
    assert!(events.is_empty());
    assert_eq!(
        requested,
        vec![0, cursor.last_event_sequence],
        "fresh snapshot cursor must anchor the second reconnect"
    );
    assert_eq!(
        recovery.unwrap().reason,
        CodingAgentRecoveryReason::RetainedHistoryGap
    );

    let first = CodingAgentFreshSnapshotRecovery {
        requested_sequence: 0,
        oldest_available_sequence: 1,
        fresh_cursor: cursor.clone(),
        reason: CodingAgentRecoveryReason::RetainedHistoryGap,
        snapshot: Box::new(initial.session.clone()),
    };
    let second = CodingAgentFreshSnapshotRecovery {
        requested_sequence: cursor.last_event_sequence,
        oldest_available_sequence: cursor.last_event_sequence.saturating_add(1),
        fresh_cursor: cursor.clone(),
        reason: CodingAgentRecoveryReason::RetainedHistoryGap,
        snapshot: Box::new(initial.session.clone()),
    };
    let mut attempts = VecDeque::from([
        DesktopReconnectAttempt::<()>::FreshSnapshotRequired(first),
        DesktopReconnectAttempt::<()>::FreshSnapshotRequired(second),
    ]);
    let error = establish_reconnect(0, |_| {
        Ok(attempts
            .pop_front()
            .expect("exhaustion should consume two fresh snapshots"))
    })
    .unwrap_err();
    assert!(error.to_string().contains("reconnect exhausted"));

    let live_lag = CodingAgentFreshSnapshotRecovery {
        requested_sequence: cursor.last_event_sequence.saturating_sub(1),
        oldest_available_sequence: cursor.last_event_sequence,
        fresh_cursor: cursor,
        reason: CodingAgentRecoveryReason::LiveReceiverLag,
        snapshot: Box::new(initial.session),
    };
    let (delivery_tx, delivery_rx) = mpsc::channel(1);
    let mut source = DesktopProductEventSource {
        replay: VecDeque::new(),
        receiver: DesktopProductEventReceiver::Injected(delivery_rx),
    };
    delivery_tx
        .send(Ok(CodingAgentReconnectDelivery::FreshSnapshotRequired(
            live_lag,
        )))
        .await
        .unwrap();
    let CodingAgentReconnectDelivery::FreshSnapshotRequired(recovery) =
        source.recv().await.unwrap()
    else {
        panic!("injected live lag must reach the desktop recovery branch");
    };
    let DesktopRuntimeUpdate::ResyncRequired { reason, .. } = recovery_update(recovery) else {
        panic!("live lag delivery must become a typed resync update");
    };
    assert_eq!(reason.code, "product_event_live_receiver_lag");

    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn command_sender_loss_stops_and_joins_the_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (mut bridge, _) = start_runtime(options).await;
    drop(bridge.commands.take());

    let stopped = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(update) = bridge.next_update().await {
            if matches!(update, DesktopRuntimeUpdate::Stopped) {
                return;
            }
        }
        panic!("runtime closed without publishing Stopped");
    })
    .await;
    assert!(stopped.is_ok(), "command sender loss did not stop runtime");
    bridge.join_runtime_thread().unwrap();
}

#[tokio::test]
async fn split_runtime_owners_deliver_commands_then_shutdown_and_join() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (bridge, initial) = start_runtime(options).await;
    let initial_session_id = initial.session.session.session_id;
    let (commands, mut events, shutdown) = bridge.into_parts();

    commands
        .try_reload(60, session_owner_target(&initial_session_id))
        .unwrap();
    let DesktopRuntimeUpdate::Reloaded {
        command_id,
        metadata,
    } = events.next_update().await.unwrap()
    else {
        panic!("the split event owner must deliver the command result");
    };
    assert_eq!(command_id, 60);
    assert_eq!(
        metadata.session.as_ref().unwrap().session.session_id,
        initial_session_id
    );

    shutdown.shutdown(&mut events).await.unwrap();
    assert_eq!(
        commands.try_reload(61, session_owner_target(&initial_session_id)),
        Err(DesktopCommandAdmissionError::RuntimeClosed),
        "a successful shutdown join must close the independently held command sender"
    );
}

#[tokio::test]
async fn shutdown_deadline_aborts_a_stuck_prompt_task() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let context = CodingAgentEmbeddingContext::load(options).unwrap();
    let mut session = context.create_session().await.unwrap();
    let connection = session
        .connect(CodingAgentClientId::new(DESKTOP_CLIENT_ID))
        .unwrap();
    let requested_after = connection.state().unwrap().cursor.last_event_sequence;
    let (events, pending_recovery) = reconnect_event_source(&connection, requested_after).unwrap();
    let task = task::spawn(std::future::pending::<PromptTaskOutput>());
    let scope = context.snapshot().workspace.as_ref().unwrap().scope.clone();
    let active = ActivePrompt {
        session_id: session.view().session_id.clone(),
        command_id: 30,
        operation_id: Some("stuck-operation".into()),
        scope,
        context,
        connection,
        events,
        pending_recovery,
        last_forwarded_sequence: requested_after,
        task,
    };
    let switch = dispatch_active_command(
        &active,
        DesktopRuntimeCommand::CreateSession { command_id: 31 },
    );
    assert!(matches!(
        switch,
        DesktopRuntimeUpdate::CommandRejected {
            command_id: 31,
            command: DesktopRuntimeCommandKind::CreateSession,
            ref code,
            ..
        } if code == "busy"
    ));
    for (command, expected_kind) in [
        (
            DesktopRuntimeCommand::SelectModel {
                command_id: 32,
                target: session_owner_target(active.session_id.clone()),
                model_id: "claude-haiku-4-5".into(),
            },
            DesktopRuntimeCommandKind::SelectModel,
        ),
        (
            DesktopRuntimeCommand::SelectSessionProfile {
                command_id: 33,
                target: session_owner_target(active.session_id.clone()),
                profile_id: "review".into(),
            },
            DesktopRuntimeCommandKind::SelectSessionProfile,
        ),
    ] {
        assert!(matches!(
            dispatch_active_command(&active, command),
            DesktopRuntimeUpdate::CommandRejected {
                command,
                ref code,
                ..
            } if command == expected_kind && code == "busy"
        ));
    }
    let stale_authorization = dispatch_active_command(
        &active,
        DesktopRuntimeCommand::DecideToolAuthorization {
            command_id: 34,
            session_id: None,
            identity: ToolAuthorizationIdentity {
                authorization_id: "already-resolved".into(),
                operation_id: "stuck-operation".into(),
                turn_id: "turn-34".into(),
                tool_call_id: "tool-call-34".into(),
                capability_generation: 1,
            },
            decision: ToolAuthorizationDecision::Deny { reason: None },
        },
    );
    assert!(matches!(
        stale_authorization,
        DesktopRuntimeUpdate::CommandRejected {
            command_id: 34,
            command: DesktopRuntimeCommandKind::DecideToolAuthorization,
            ref code,
            ..
        } if code == "input"
    ));
    let (priority_updates, mut priority_rx) = mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);

    shutdown_active_prompt_with_deadline(Some(active), &priority_updates, Duration::ZERO).await;
    let DesktopRuntimeUpdate::RuntimeFailed { error } = priority_rx.recv().await.unwrap() else {
        panic!("deadline expiry must publish a runtime failure");
    };
    assert_eq!(error.code, "shutdown_deadline_exceeded");
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn runtime_thread_panic_is_reported_during_join() {
    let (commands, command_rx) = mpsc::channel(DESKTOP_COMMAND_QUEUE_CAPACITY);
    drop(command_rx);
    let (shutdown, shutdown_rx) = watch::channel(false);
    drop(shutdown_rx);
    let (priority_updates_tx, priority_updates) =
        mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
    drop(priority_updates_tx);
    let (data_updates_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
    drop(data_updates_tx);
    let runtime_thread = thread::spawn(|| panic!("injected desktop runtime panic"));
    let bridge = DesktopRuntimeBridge {
        shutdown: DesktopRuntimeShutdownGuard {
            shutdown,
            runtime_thread: Some(runtime_thread),
        },
        commands: Some(commands),
        events: DesktopRuntimeEventStream {
            priority_updates,
            data_updates,
            pending_priority_update: None,
            pending_data_update: None,
        },
    };

    assert!(matches!(
        bridge.shutdown().await,
        Err(DesktopRuntimeShutdownError::RuntimePanicked)
    ));
}

#[tokio::test]
async fn abort_race_is_typed_and_window_close_is_non_blocking() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (mut bridge, initial) = start_runtime(options).await;
    let session_id = initial.session.session.session_id;
    bridge
        .try_submit_prompt(20, existing_prompt_target(&session_id), "abort race", None)
        .unwrap();

    let mut saw_control_result = false;
    let mut saw_prompt_finished = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match bridge.next_update().await.unwrap() {
                DesktopRuntimeUpdate::PromptStarted { .. } => {
                    bridge.try_abort(21).unwrap();
                }
                DesktopRuntimeUpdate::ControlAccepted { command_id: 21, .. }
                | DesktopRuntimeUpdate::CommandRejected { command_id: 21, .. } => {
                    saw_control_result = true
                }
                DesktopRuntimeUpdate::PromptFinished { command_id: 20, .. } => {
                    saw_prompt_finished = true
                }
                _ => {}
            }
            if saw_control_result && saw_prompt_finished {
                break;
            }
        }
    })
    .await
    .expect("abort race must converge to a prompt terminal");
    assert!(
        saw_control_result,
        "abort command must receive a typed result"
    );
    assert!(saw_prompt_finished);

    bridge.try_abort(24).unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 24,
            command: DesktopRuntimeCommandKind::Abort,
            ..
        })
    ));

    bridge
        .try_submit_prompt(
            22,
            existing_prompt_target(session_id),
            "close during prompt",
            None,
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                bridge.next_update().await,
                Some(DesktopRuntimeUpdate::PromptAccepted { command_id: 22 })
            ) {
                break;
            }
        }
    })
    .await
    .expect("terminal ProductEvent acknowledgement must release the next submission slot");
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(move || drop(bridge)),
    )
    .await
    .expect("dropping the desktop window bridge must return promptly")
    .unwrap();
}

#[tokio::test]
async fn steer_and_follow_up_races_keep_typed_command_association() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (mut bridge, initial) = start_runtime(options).await;
    let session_id = initial.session.session.session_id;
    bridge
        .try_submit_prompt(
            25,
            existing_prompt_target(session_id),
            "control association race",
            None,
        )
        .unwrap();

    let mut controls_sent = false;
    let mut steer_result = false;
    let mut follow_up_result = false;
    let mut prompt_finished = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match bridge.next_update().await.unwrap() {
                DesktopRuntimeUpdate::PromptStarted { .. } if !controls_sent => {
                    bridge.try_steer(26, "steer exactly").unwrap();
                    bridge.try_follow_up(27, "follow up exactly").unwrap();
                    controls_sent = true;
                }
                DesktopRuntimeUpdate::ControlAccepted {
                    command_id: 26,
                    command: DesktopRuntimeCommandKind::Steer,
                    ..
                }
                | DesktopRuntimeUpdate::CommandRejected {
                    command_id: 26,
                    command: DesktopRuntimeCommandKind::Steer,
                    ..
                } => steer_result = true,
                DesktopRuntimeUpdate::ControlAccepted {
                    command_id: 27,
                    command: DesktopRuntimeCommandKind::FollowUp,
                    ..
                }
                | DesktopRuntimeUpdate::CommandRejected {
                    command_id: 27,
                    command: DesktopRuntimeCommandKind::FollowUp,
                    ..
                } => follow_up_result = true,
                DesktopRuntimeUpdate::PromptFinished { command_id: 25, .. } => {
                    prompt_finished = true
                }
                _ => {}
            }
            if steer_result && follow_up_result && prompt_finished {
                break;
            }
        }
    })
    .await
    .expect("control races must converge to typed results and a prompt terminal");

    assert!(controls_sent, "controls must be sent after PromptStarted");
    assert!(steer_result, "steer must receive its typed command result");
    assert!(
        follow_up_result,
        "follow-up must receive its typed command result"
    );
    assert!(prompt_finished);
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn authorization_decision_is_typed_and_rejected_without_an_active_prompt() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (mut bridge, _) = start_runtime(options).await;
    let identity = ToolAuthorizationIdentity {
        authorization_id: "authorization-31".into(),
        operation_id: "operation-31".into(),
        turn_id: "turn-31".into(),
        tool_call_id: "tool-call-31".into(),
        capability_generation: 1,
    };
    bridge
        .try_decide_tool_authorization(
            31,
            &identity,
            ToolAuthorizationDecision::Deny {
                reason: Some("test denial".into()),
            },
        )
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 31,
            command: DesktopRuntimeCommandKind::DecideToolAuthorization,
            ..
        })
    ));
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn recovery_actions_are_identity_bound_and_stale_facts_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (mut bridge, initial) = start_runtime(options).await;
    let pending = CodingAgentRecoveryPending {
        operation_id: "operation-recovery".into(),
        recovery_id: "recovery-id".into(),
        operation_kind: Some("prompt".into()),
        record_version: 3,
        descriptor_revision: 2,
        capability_generation: Some(initial.session.cursor.capability_generation),
        attempt_count: 1,
        last_attempt_at: Some("2026-07-24T00:00:00Z".into()),
        next_attempt_at: None,
    };
    let identity = DesktopRecoveryIdentity::from(&pending);
    let mut projected = initial;
    projected.pending_recoveries = vec![pending];
    let projection = DesktopProjection::new(projected).unwrap();
    let recovery = projection.recoveries().front().unwrap();
    assert!(recovery.authoritative);
    assert_eq!(recovery.identity.as_ref(), Some(&identity));
    assert_eq!(recovery.attempt_count, 1);

    bridge.try_retry_recovery(32, &identity).unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 32,
            command: DesktopRuntimeCommandKind::RetryRecovery,
            ..
        })
    ));
    bridge
        .try_resolve_recovery(33, &identity, CodingAgentRecoveryResolution::Aborted)
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 33,
            command: DesktopRuntimeCommandKind::ResolveRecovery,
            ..
        })
    ));
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn authorization_projection_preserves_identity_and_bounds_display_payloads() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (bridge, initial) = start_runtime(options).await;
    let request = ToolAuthorizationRequest {
        authorization_id: "authorization-exact".into(),
        operation_id: "operation-exact".into(),
        turn_id: "turn-exact".into(),
        tool_call_id: "tool-call-exact".into(),
        tool_name: "bash".into(),
        risk: ToolAuthorizationRisk::ShellExecution,
        scope: ToolAuthorizationScope::Shell {
            cwd: "x".repeat(MAX_AUTHORIZATION_TEXT_BYTES + 100),
            command_fingerprint: "fingerprint".into(),
        },
        preview: ToolAuthorizationPreview {
            summary: "x".repeat(MAX_AUTHORIZATION_TEXT_BYTES + 100),
            path: None,
            command: Some("x".repeat(MAX_AUTHORIZATION_TEXT_BYTES + 100)),
            cwd: Some("x".repeat(MAX_AUTHORIZATION_TEXT_BYTES + 100)),
            content_preview: None,
        },
        capability_generation: initial.session.cursor.capability_generation,
        requested_at: "2026-07-24T00:00:00Z".into(),
    };

    let mut bounded = initial.clone();
    bounded.session.pending_authorizations.push(request.clone());
    let projection = DesktopProjection::new(bounded).unwrap();
    let retained = projection
        .snapshot()
        .pending_authorizations
        .first()
        .unwrap();
    assert_eq!(retained.authorization_id, "authorization-exact");
    assert_eq!(retained.operation_id, "operation-exact");
    assert!(retained.preview.summary.len() <= MAX_AUTHORIZATION_TEXT_BYTES);
    assert!(retained.preview.command.as_ref().unwrap().len() <= MAX_AUTHORIZATION_TEXT_BYTES);

    let mut invalid = initial.clone();
    let mut invalid_request = request.clone();
    invalid_request.authorization_id = "x".repeat(MAX_AUTHORIZATION_ID_BYTES + 1);
    invalid.session.pending_authorizations.push(invalid_request);
    assert_eq!(
        DesktopProjection::new(invalid).unwrap_err().code,
        "authorization_identity_invalid"
    );

    let mut stale = initial;
    let mut stale_request = request.clone();
    stale_request.capability_generation =
        stale_request.capability_generation.checked_add(1).unwrap();
    stale.session.pending_authorizations.push(stale_request);
    assert_eq!(
        DesktopProjection::new(stale).unwrap_err().code,
        "authorization_capability_generation_mismatch"
    );

    let identity = request.identity();
    assert_eq!(request.identity(), identity);
    let mut stale_identity = identity.clone();
    stale_identity.capability_generation =
        stale_identity.capability_generation.checked_add(1).unwrap();
    assert_ne!(request.identity(), stale_identity);
    stale_identity = identity;
    stale_identity.operation_id = "another-operation".into();
    assert_ne!(request.identity(), stale_identity);
    bridge.shutdown().await.unwrap();
}

#[test]
fn command_inputs_and_queue_capacities_are_bounded() {
    assert!((1..=128).contains(&DESKTOP_COMMAND_QUEUE_CAPACITY));
    assert!((1..=256).contains(&DESKTOP_UPDATE_QUEUE_CAPACITY));
    assert!((1..=128).contains(&DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY));
    assert!(validate_session_id("").is_err());
    assert!(validate_session_id(&"x".repeat(MAX_SESSION_ID_BYTES + 1)).is_err());
    assert!(validate_session_id("session-ok").is_ok());
    assert!(validate_prompt("").is_err());
    assert!(validate_prompt(&"x".repeat(MAX_PROMPT_BYTES + 1)).is_err());
    assert!(validate_prompt("prompt").is_ok());
    let attachments = vec![std::path::PathBuf::from("fixture.png"); MAX_PROMPT_ATTACHMENTS];
    assert!(validate_prompt_with_attachments("", &attachments).is_ok());
    let over_limit = vec![std::path::PathBuf::from("fixture.png"); MAX_PROMPT_ATTACHMENTS + 1];
    assert!(validate_prompt_with_attachments("draft remains", &over_limit).is_err());
    assert!(validate_control_text("").is_err());
    assert!(validate_control_text(&"x".repeat(MAX_CONTROL_TEXT_BYTES + 1)).is_err());
    assert!(validate_control_text("steer").is_ok());
    let mut identity = ToolAuthorizationIdentity {
        authorization_id: "authorization-ok".into(),
        operation_id: "operation-ok".into(),
        turn_id: "turn-ok".into(),
        tool_call_id: "tool-call-ok".into(),
        capability_generation: 1,
    };
    assert!(validate_authorization_identity(&identity).is_ok());
    identity.authorization_id.clear();
    assert!(validate_authorization_identity(&identity).is_err());
    identity.authorization_id = "authorization-ok".into();
    identity.tool_call_id = "x".repeat(MAX_AUTHORIZATION_ID_BYTES + 1);
    assert!(validate_authorization_identity(&identity).is_err());
    let mut recovery = DesktopRecoveryIdentity {
        operation_id: "operation-ok".into(),
        recovery_id: "recovery-ok".into(),
        record_version: 1,
        descriptor_revision: 1,
        capability_generation: Some(1),
        attempt_count: 0,
    };
    assert!(validate_recovery_identity(&recovery).is_ok());
    recovery.recovery_id.clear();
    assert!(validate_recovery_identity(&recovery).is_err());
    recovery.recovery_id = "x".repeat(MAX_RECOVERY_ID_BYTES + 1);
    assert!(validate_recovery_identity(&recovery).is_err());
    assert!(validate_selection_id("model", "").is_err());
    assert!(validate_selection_id("profile", &"x".repeat(MAX_SELECTION_ID_BYTES + 1)).is_err());
    assert!(validate_selection_id("model", "claude-haiku-4-5").is_ok());
}

#[test]
fn prompt_target_admission_is_typed_bounded_and_debug_redacted() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("private-project-name");
    std::fs::create_dir_all(&project).unwrap();
    let valid = DesktopPromptTarget::new(
        CodingAgentWorkspaceSelection::project(&project),
        "private-model-id",
        "private-profile-id",
    );
    assert!(validate_prompt_target(&valid).is_ok());
    assert!(matches!(
        validate_prompt_target(&DesktopPromptTarget::existing("")),
        Err(DesktopCommandAdmissionError::InvalidSessionId { .. })
    ));
    assert!(matches!(
        validate_prompt_target(&DesktopPromptTarget::new(
            CodingAgentWorkspaceSelection::project(temp.path().join("missing-project")),
            "model",
            "profile",
        )),
        Err(DesktopCommandAdmissionError::InvalidPromptTarget { .. })
    ));
    let file = temp.path().join("not-a-project-directory");
    std::fs::write(&file, "file").unwrap();
    assert!(matches!(
        validate_prompt_target(&DesktopPromptTarget::new(
            CodingAgentWorkspaceSelection::project(file),
            "model",
            "profile",
        )),
        Err(DesktopCommandAdmissionError::InvalidPromptTarget { .. })
    ));
    assert!(matches!(
        validate_prompt_target(&DesktopPromptTarget::new(
            CodingAgentWorkspaceSelection::project("bad\0project"),
            "model",
            "profile",
        )),
        Err(DesktopCommandAdmissionError::InvalidPromptTarget { .. })
    ));
    assert!(matches!(
        validate_prompt_target(&DesktopPromptTarget::new(
            CodingAgentWorkspaceSelection::project(
                std::path::PathBuf::from("x").join("y".repeat(MAX_WORKSPACE_PATH_BYTES))
            ),
            "model",
            "profile",
        )),
        Err(DesktopCommandAdmissionError::InvalidPromptTarget { .. })
    ));
    assert!(matches!(
        validate_prompt_target(&DesktopPromptTarget::new(
            CodingAgentWorkspaceSelection::project(&project),
            "",
            "profile",
        )),
        Err(DesktopCommandAdmissionError::InvalidSelectionId { .. })
    ));

    let command = DesktopRuntimeCommand::SubmitPrompt {
        command_id: 902,
        target: valid,
        prompt: "private prompt body".into(),
        attachments: vec![std::path::PathBuf::from("private-attachment-name")],
        thinking_level: None,
    };
    let debug = format!("{command:?}");
    assert!(debug.contains("SubmitPrompt"));
    assert!(debug.contains("new"));
    for secret in [
        "private-project-name",
        "private-model-id",
        "private-profile-id",
        "private prompt body",
        "private-attachment-name",
    ] {
        assert!(!debug.contains(secret));
    }
}

#[test]
fn attachment_commands_preserve_bounded_paths_and_session_target() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("project")).unwrap();
    let (bridge, mut harness) = DesktopRuntimeBridge::instrumented_for_test();
    let attachments = [
        std::path::PathBuf::from("screenshots/one.png"),
        std::path::PathBuf::from("notes/two.txt"),
    ];
    bridge
        .try_submit_prompt_with_attachments(
            900,
            existing_prompt_target("session-attachment-test"),
            "inspect these",
            &attachments,
            None,
        )
        .unwrap();
    bridge
        .try_submit_prompt_with_attachments(
            901,
            new_project_prompt_target(&temp),
            "inspect once more",
            &attachments[..1],
            None,
        )
        .unwrap();
    assert_eq!(
        harness.drain_prompt_attachments(),
        [
            (
                existing_prompt_target("session-attachment-test"),
                "inspect these".into(),
                attachments.to_vec(),
            ),
            (
                new_project_prompt_target(&temp),
                "inspect once more".into(),
                attachments[..1].to_vec(),
            ),
        ]
    );
}

#[test]
fn rename_command_is_bounded_trimmed_and_identity_preserving() {
    let (bridge, mut harness) = DesktopRuntimeBridge::instrumented_for_test();
    bridge
        .try_rename_session(902, "session-to-rename", Some("  Release plan  "))
        .unwrap();
    assert_eq!(
        harness.drain_session_renames(),
        [("session-to-rename".into(), Some("Release plan".into()))]
    );
    assert!(
        bridge
            .try_rename_session(
                903,
                "session-to-rename",
                Some(&"x".repeat(MAX_SESSION_NAME_BYTES + 1)),
            )
            .is_err()
    );
}

#[test]
fn runtime_error_preserves_only_the_product_safe_error_projection() {
    let product_error = CodingAgentPublicError {
        category: CodingAgentErrorCategory::Provider,
        code: "provider".into(),
        retryable: true,
        summary: "The model provider request failed.".into(),
        context: CodingAgentErrorContext::None,
    };
    let error = runtime_error(&product_error);
    let rendered = format!("{}: {}", error.code, error.message);

    assert_eq!(error.code, "provider");
    assert_eq!(error.message, "The model provider request failed.");
    assert_eq!(rendered, "provider: The model provider request failed.");
}
