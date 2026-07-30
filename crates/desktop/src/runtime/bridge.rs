use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use coding_agent::api::authorization::{ToolAuthorizationDecision, ToolAuthorizationIdentity};
use coding_agent::api::embedding::{CodingAgentEmbeddingOptions, CodingAgentThinkingLevel};
use coding_agent::api::event::{
    CodingAgentProductEventDeliveryClass, CodingAgentRecoveryResolution,
};
use coding_agent::api::review::{CodingAgentExternalEditorTarget, CodingAgentFileReviewRequest};
use tokio::runtime;
use tokio::sync::{mpsc, watch};
use tracing::Instrument as _;

use crate::file_review::DesktopExternalEditorConfig;

#[cfg(test)]
use super::protocol::DesktopRuntimeCommandKind;
use super::protocol::{
    DESKTOP_COMMAND_QUEUE_CAPACITY, DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY,
    DESKTOP_UPDATE_QUEUE_CAPACITY, DesktopCommandAdmissionError, DesktopPromptTarget,
    DesktopRecoveryIdentity, DesktopRuntimeCommand, DesktopRuntimeError,
    DesktopRuntimeHydratedSnapshot, DesktopRuntimeOwnerTarget, DesktopRuntimeReadySnapshot,
    DesktopRuntimeShutdownError, DesktopRuntimeStartError, DesktopRuntimeUpdate,
    local_runtime_error, validate_authorization_identity, validate_control_text,
    validate_file_review_request, validate_prompt_target, validate_prompt_with_attachments,
    validate_recovery_identity, validate_runtime_owner_target, validate_selection_id,
    validate_session_id, validate_session_name,
};
use super::run_runtime;

const STREAMING_DELIVERY_COALESCE_WINDOW: Duration = Duration::from_millis(16);
const MAX_STREAMING_DELIVERIES_PER_BATCH: usize = 64;

fn admitted_prompt_command(
    command_id: u64,
    target: DesktopPromptTarget,
    prompt: &str,
    attachments: &[PathBuf],
    thinking_level: Option<CodingAgentThinkingLevel>,
) -> Result<DesktopRuntimeCommand, DesktopCommandAdmissionError> {
    validate_prompt_target(&target)?;
    validate_prompt_with_attachments(prompt, attachments)?;
    Ok(DesktopRuntimeCommand::SubmitPrompt {
        command_id,
        target,
        prompt: prompt.to_owned(),
        attachments: attachments.to_vec(),
        thinking_level,
    })
}

pub(super) fn build_desktop_runtime() -> std::io::Result<runtime::Runtime> {
    runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
}

pub struct DesktopRuntimeBridge {
    pub(super) shutdown: DesktopRuntimeShutdownGuard,
    pub(super) command_client: Option<RuntimeCommandClient>,
    pub(super) events: DesktopRuntimeEventStream,
}

/// Sole ordered delivery side of the desktop runtime.
pub struct DesktopRuntimeEventStream {
    pub(super) priority_updates: mpsc::Receiver<DesktopRuntimeUpdate>,
    pub(super) data_updates: mpsc::Receiver<DesktopRuntimeUpdate>,
    pub(super) pending_priority_update: Option<DesktopRuntimeUpdate>,
    pub(super) pending_data_update: Option<DesktopRuntimeUpdate>,
}

/// Sole shutdown signal and runtime-thread join owner.
pub struct DesktopRuntimeShutdownGuard {
    pub(super) shutdown: watch::Sender<bool>,
    pub(super) runtime_thread: Option<JoinHandle<()>>,
}

/// Cloneable, non-blocking command side of the desktop runtime bridge.
#[derive(Clone)]
pub struct RuntimeCommandClient {
    pub(super) commands: mpsc::Sender<DesktopRuntimeCommand>,
}

/// Non-blocking startup handle for a desktop runtime.
///
/// GPUI owns this value while project configuration loads on the dedicated
/// runtime thread. [`Self::try_ready`] never waits and startup creates no session.
pub struct DesktopRuntimeBootstrap {
    ready: std_mpsc::Receiver<Result<DesktopRuntimeReadySnapshot, DesktopRuntimeError>>,
    bridge: Option<DesktopRuntimeBridge>,
}

impl DesktopRuntimeBootstrap {
    pub fn try_ready(
        &mut self,
    ) -> Result<Option<(DesktopRuntimeBridge, DesktopRuntimeReadySnapshot)>, DesktopRuntimeStartError>
    {
        match self.ready.try_recv() {
            Ok(result) => self.finish(result).map(Some),
            Err(std_mpsc::TryRecvError::Empty) => Ok(None),
            Err(std_mpsc::TryRecvError::Disconnected) => self.finish_disconnected_initialization(),
        }
    }

    /// Blocking startup is reserved for tests and explicitly non-GPUI callers.
    #[cfg(test)]
    pub fn wait_blocking(
        mut self,
    ) -> Result<(DesktopRuntimeBridge, DesktopRuntimeReadySnapshot), DesktopRuntimeStartError> {
        match self.ready.recv() {
            Ok(result) => self.finish(result),
            Err(_) => self.finish_disconnected_initialization().and_then(|ready| {
                ready.ok_or(DesktopRuntimeStartError::InitializationChannelClosed)
            }),
        }
    }

    fn finish(
        &mut self,
        result: Result<DesktopRuntimeReadySnapshot, DesktopRuntimeError>,
    ) -> Result<(DesktopRuntimeBridge, DesktopRuntimeReadySnapshot), DesktopRuntimeStartError> {
        let mut bridge = self
            .bridge
            .take()
            .ok_or(DesktopRuntimeStartError::InitializationChannelClosed)?;
        match result {
            Ok(snapshot) => Ok((bridge, snapshot)),
            Err(error) => {
                let join = bridge.join_runtime_thread();
                if matches!(join, Err(DesktopRuntimeShutdownError::RuntimePanicked)) {
                    return Err(DesktopRuntimeStartError::InitializationThreadPanicked);
                }
                Err(DesktopRuntimeStartError::Initialization {
                    code: error.code,
                    message: error.message,
                })
            }
        }
    }

    fn finish_disconnected_initialization(
        &mut self,
    ) -> Result<Option<(DesktopRuntimeBridge, DesktopRuntimeReadySnapshot)>, DesktopRuntimeStartError>
    {
        let Some(mut bridge) = self.bridge.take() else {
            return Err(DesktopRuntimeStartError::InitializationChannelClosed);
        };
        match bridge.join_runtime_thread() {
            Ok(()) => Err(DesktopRuntimeStartError::InitializationChannelClosed),
            Err(DesktopRuntimeShutdownError::RuntimePanicked) => {
                Err(DesktopRuntimeStartError::InitializationThreadPanicked)
            }
        }
    }
}

impl Drop for DesktopRuntimeBootstrap {
    fn drop(&mut self) {
        drop(self.bridge.take());
    }
}

impl DesktopRuntimeBridge {
    /// Creates an inert bridge for deterministic native rendering replays.
    ///
    /// The command and update peers are deliberately closed so the shell can
    /// exercise its real window/render path without starting a product runtime.
    pub(crate) fn disconnected_for_replay() -> Self {
        let (commands, command_rx) = mpsc::channel(DESKTOP_COMMAND_QUEUE_CAPACITY);
        drop(command_rx);
        let (priority_update_tx, priority_updates) =
            mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
        drop(priority_update_tx);
        let (data_update_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
        drop(data_update_tx);
        let (shutdown, _) = watch::channel(false);
        Self {
            shutdown: DesktopRuntimeShutdownGuard {
                shutdown,
                runtime_thread: None,
            },
            command_client: Some(RuntimeCommandClient { commands }),
            events: DesktopRuntimeEventStream {
                priority_updates,
                data_updates,
                pending_priority_update: None,
                pending_data_update: None,
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn disconnected_for_test() -> Self {
        Self::disconnected_for_replay()
    }

    #[cfg(test)]
    pub(crate) fn instrumented_for_test() -> (Self, DesktopRuntimeTestHarness) {
        let (commands, command_rx) = mpsc::channel(DESKTOP_COMMAND_QUEUE_CAPACITY);
        let (priority_update_tx, priority_updates) =
            mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
        let (data_update_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
        let (shutdown, _) = watch::channel(false);
        (
            Self {
                shutdown: DesktopRuntimeShutdownGuard {
                    shutdown,
                    runtime_thread: None,
                },
                command_client: Some(RuntimeCommandClient { commands }),
                events: DesktopRuntimeEventStream {
                    priority_updates,
                    data_updates,
                    pending_priority_update: None,
                    pending_data_update: None,
                },
            },
            DesktopRuntimeTestHarness {
                protocol_commands: command_rx,
                _priority_update_tx: priority_update_tx,
                _data_update_tx: data_update_tx,
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn command_client_for_test(&self) -> &RuntimeCommandClient {
        self.command_client
            .as_ref()
            .expect("test bridge must retain its command client before splitting")
    }

    /// Spawn runtime initialization without waiting for configuration/session I/O.
    pub fn spawn(
        options: CodingAgentEmbeddingOptions,
    ) -> Result<DesktopRuntimeBootstrap, DesktopRuntimeStartError> {
        let (commands, command_rx) = mpsc::channel(DESKTOP_COMMAND_QUEUE_CAPACITY);
        let (priority_update_tx, priority_updates) =
            mpsc::channel(DESKTOP_PRIORITY_UPDATE_QUEUE_CAPACITY);
        let (data_update_tx, data_updates) = mpsc::channel(DESKTOP_UPDATE_QUEUE_CAPACITY);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);

        let runtime_thread = thread::Builder::new()
            .name("desktop-runtime".into())
            .spawn(move || {
                let runtime = match build_desktop_runtime() {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        let _ = ready_tx.send(Err(local_runtime_error(
                            "runtime_initialization",
                            "The desktop runtime could not be initialized.",
                        )));
                        return;
                    }
                };
                runtime.block_on(run_runtime(
                    options,
                    command_rx,
                    shutdown_rx,
                    priority_update_tx,
                    data_update_tx,
                    ready_tx,
                ));
            })
            .map_err(DesktopRuntimeStartError::Spawn)?;

        Ok(DesktopRuntimeBootstrap {
            ready: ready_rx,
            bridge: Some(Self {
                command_client: Some(RuntimeCommandClient { commands }),
                events: DesktopRuntimeEventStream {
                    priority_updates,
                    data_updates,
                    pending_priority_update: None,
                    pending_data_update: None,
                },
                shutdown: DesktopRuntimeShutdownGuard {
                    shutdown,
                    runtime_thread: Some(runtime_thread),
                },
            }),
        })
    }

    pub fn into_parts(
        mut self,
    ) -> (
        RuntimeCommandClient,
        DesktopRuntimeEventStream,
        DesktopRuntimeShutdownGuard,
    ) {
        let command_client = self
            .command_client
            .take()
            .expect("live desktop bridge must retain its command client");
        (command_client, self.events, self.shutdown)
    }

    pub(crate) async fn open_session_for_bootstrap(
        &mut self,
        command_id: u64,
        session_id: &str,
    ) -> Result<DesktopRuntimeHydratedSnapshot, String> {
        self.command_client
            .as_ref()
            .ok_or_else(|| DesktopCommandAdmissionError::RuntimeClosed.to_string())?
            .try_open_session(command_id, session_id)
            .map_err(|error| error.to_string())?;
        while let Some(update) = self.events.next_update().await {
            match update {
                DesktopRuntimeUpdate::SessionChanged {
                    command_id: completed_id,
                    snapshot,
                } if completed_id == command_id => return Ok(snapshot),
                DesktopRuntimeUpdate::CommandRejected {
                    command_id: rejected_id,
                    message,
                    ..
                } if rejected_id == command_id => return Err(message),
                DesktopRuntimeUpdate::RuntimeFailed { error } => return Err(error.message),
                DesktopRuntimeUpdate::Stopped => {
                    return Err(
                        "desktop runtime stopped while opening the requested session".into(),
                    );
                }
                _ => {}
            }
        }
        Err("desktop runtime closed while opening the requested session".into())
    }

    #[cfg(test)]
    pub async fn next_update(&mut self) -> Option<DesktopRuntimeUpdate> {
        self.events.next_update().await
    }

    #[cfg(test)]
    pub async fn next_update_batch(&mut self) -> Option<Vec<DesktopRuntimeUpdate>> {
        self.events.next_update_batch().await
    }

    #[cfg(test)]
    pub async fn shutdown(mut self) -> Result<(), DesktopRuntimeShutdownError> {
        self.shutdown.signal();
        drop(self.command_client.take());
        while let Some(update) = self.events.next_update().await {
            if matches!(update, DesktopRuntimeUpdate::Stopped) {
                break;
            }
        }
        self.shutdown.join()
    }

    pub(super) fn join_runtime_thread(&mut self) -> Result<(), DesktopRuntimeShutdownError> {
        self.shutdown.join()
    }
}

#[cfg(test)]
pub(crate) struct DesktopRuntimeTestHarness {
    protocol_commands: mpsc::Receiver<DesktopRuntimeCommand>,
    _priority_update_tx: mpsc::Sender<DesktopRuntimeUpdate>,
    _data_update_tx: mpsc::Sender<DesktopRuntimeUpdate>,
}

#[cfg(test)]
impl DesktopRuntimeTestHarness {
    pub(crate) fn drain_command_kinds(&mut self) -> Vec<DesktopRuntimeCommandKind> {
        let mut kinds = Vec::new();
        while let Ok(command) = self.protocol_commands.try_recv() {
            kinds.push(command.kind());
        }
        kinds
    }

    pub(crate) fn drain_selections(
        &mut self,
    ) -> Vec<(
        DesktopRuntimeCommandKind,
        DesktopRuntimeOwnerTarget,
        String,
        Option<CodingAgentThinkingLevel>,
    )> {
        let mut selections = Vec::new();
        while let Ok(command) = self.protocol_commands.try_recv() {
            match command {
                DesktopRuntimeCommand::SelectModel {
                    target,
                    model_id,
                    thinking_level,
                    ..
                } => {
                    selections.push((
                        DesktopRuntimeCommandKind::SelectModel,
                        target,
                        model_id,
                        thinking_level,
                    ));
                }
                DesktopRuntimeCommand::SelectSessionProfile {
                    target, profile_id, ..
                } => {
                    selections.push((
                        DesktopRuntimeCommandKind::SelectSessionProfile,
                        target,
                        profile_id,
                        None,
                    ));
                }
                _ => {}
            }
        }
        selections
    }

    pub(crate) fn drain_prompts(
        &mut self,
    ) -> Vec<(
        DesktopPromptTarget,
        String,
        Option<CodingAgentThinkingLevel>,
    )> {
        let mut prompts = Vec::new();
        while let Ok(command) = self.protocol_commands.try_recv() {
            if let DesktopRuntimeCommand::SubmitPrompt {
                target,
                prompt,
                thinking_level,
                ..
            } = command
            {
                prompts.push((target, prompt, thinking_level));
            }
        }
        prompts
    }

    pub(crate) fn drain_prompt_attachments(
        &mut self,
    ) -> Vec<(DesktopPromptTarget, String, Vec<PathBuf>)> {
        let mut prompts = Vec::new();
        while let Ok(command) = self.protocol_commands.try_recv() {
            if let DesktopRuntimeCommand::SubmitPrompt {
                target,
                prompt,
                attachments,
                ..
            } = command
            {
                prompts.push((target, prompt, attachments));
            }
        }
        prompts
    }

    pub(crate) fn drain_session_renames(&mut self) -> Vec<(String, Option<String>)> {
        let mut renames = Vec::new();
        while let Ok(command) = self.protocol_commands.try_recv() {
            if let DesktopRuntimeCommand::RenameSession {
                session_id, name, ..
            } = command
            {
                renames.push((session_id, name));
            }
        }
        renames
    }
}

impl DesktopRuntimeEventStream {
    pub async fn next_update(&mut self) -> Option<DesktopRuntimeUpdate> {
        loop {
            if let Some(update) = self.try_next_update() {
                return Some(update);
            }
            if self.priority_updates.is_closed() && self.data_updates.is_closed() {
                return None;
            }
            tokio::select! {
                biased;
                update = self.priority_updates.recv(), if !self.priority_updates.is_closed() => {
                    self.pending_priority_update = update;
                }
                update = self.data_updates.recv(), if !self.data_updates.is_closed() => {
                    self.pending_data_update = update;
                }
            }
        }
    }

    /// Await one lossless delivery batch.
    ///
    /// Only ProductEvent data updates open the bounded coalescing window.
    /// Control, recovery, terminal, failure, and shutdown updates return
    /// immediately, including when they interrupt an active data batch.
    pub async fn next_update_batch(&mut self) -> Option<Vec<DesktopRuntimeUpdate>> {
        let wait_started = std::time::Instant::now();
        let first = self
            .next_update()
            .instrument(tracing::trace_span!("desktop.runtime.batch_wait"))
            .await?;
        tracing::trace!(
            target: "desktop",
            wait_micros = wait_started.elapsed().as_micros() as u64,
            update_kind = first.kind_label(),
            "desktop.runtime.receive"
        );
        if !is_streaming_data_update(&first) {
            tracing::trace!(
                target: "desktop",
                batch_size = 1_u64,
                "desktop.runtime.batch_size"
            );
            return Some(vec![first]);
        }
        let mut updates = Vec::with_capacity(MAX_STREAMING_DELIVERIES_PER_BATCH);
        updates.push(first);
        // This future is polled by GPUI's executor in production, so it must
        // not assume that a Tokio reactor is entered on the UI thread.
        // Use the scheduler timer future directly. BackgroundExecutor::timer
        // wraps it in a single-consumption Task, which is not safe for the
        // manual executor-neutral polling contract exercised below.
        let deadline = gpui_platform::background_executor()
            .scheduler_executor()
            .timer(STREAMING_DELIVERY_COALESCE_WINDOW);
        tokio::pin!(deadline);
        while updates.len() < MAX_STREAMING_DELIVERIES_PER_BATCH {
            tokio::select! {
                biased;
                update = self.next_update() => {
                    let Some(update) = update else {
                        break;
                    };
                    let immediate = !is_streaming_data_update(&update);
                    updates.push(update);
                    if immediate {
                        break;
                    }
                }
                _ = &mut deadline => break,
            }
        }
        tracing::trace!(
            target: "desktop",
            batch_size = updates.len() as u64,
            "desktop.runtime.batch_size"
        );
        Some(updates)
    }

    pub fn try_next_update(&mut self) -> Option<DesktopRuntimeUpdate> {
        if self.pending_priority_update.is_none() {
            self.pending_priority_update = self.priority_updates.try_recv().ok();
        }
        if self.pending_data_update.is_none() {
            self.pending_data_update = self.data_updates.try_recv().ok();
        }
        self.take_next_pending_update()
    }

    fn take_next_pending_update(&mut self) -> Option<DesktopRuntimeUpdate> {
        match (
            self.pending_priority_update.as_ref(),
            self.pending_data_update.as_ref(),
        ) {
            (Some(priority), Some(data)) if data_precedes_priority(data, priority) => {
                self.pending_data_update.take()
            }
            (Some(_), _) => self.pending_priority_update.take(),
            (None, Some(_)) => self.pending_data_update.take(),
            (None, None) => None,
        }
    }
}

impl DesktopRuntimeShutdownGuard {
    fn signal(&self) {
        let _ = self.shutdown.send(true);
    }

    pub async fn shutdown(
        mut self,
        events: &mut DesktopRuntimeEventStream,
    ) -> Result<(), DesktopRuntimeShutdownError> {
        self.signal();
        while let Some(update) = events.next_update().await {
            if matches!(update, DesktopRuntimeUpdate::Stopped) {
                break;
            }
        }
        self.join()
    }

    fn join(&mut self) -> Result<(), DesktopRuntimeShutdownError> {
        self.runtime_thread.take().map_or(Ok(()), |thread| {
            thread
                .join()
                .map_err(|_| DesktopRuntimeShutdownError::RuntimePanicked)
        })
    }
}

impl RuntimeCommandClient {
    pub fn try_reload(
        &self,
        command_id: u64,
        target: DesktopRuntimeOwnerTarget,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_runtime_owner_target(&target)?;
        self.try_send(DesktopRuntimeCommand::Reload { command_id, target })
    }

    pub fn try_resync(&self, command_id: u64) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(DesktopRuntimeCommand::Resync {
            command_id,
            session_id: None,
        })
    }

    pub fn try_create_session(&self, command_id: u64) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(DesktopRuntimeCommand::CreateSession { command_id })
    }

    pub fn try_open_session(
        &self,
        command_id: u64,
        session_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        self.try_send(DesktopRuntimeCommand::OpenSession {
            command_id,
            session_id: session_id.to_owned(),
        })
    }

    pub fn try_close_session(
        &self,
        command_id: u64,
        session_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        self.try_send(DesktopRuntimeCommand::CloseSession {
            command_id,
            session_id: session_id.to_owned(),
        })
    }

    pub fn try_list_sessions(&self, command_id: u64) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(DesktopRuntimeCommand::ListSessions { command_id })
    }

    pub fn try_rename_session(
        &self,
        command_id: u64,
        session_id: &str,
        name: Option<&str>,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        validate_session_name(name)?;
        self.try_send(DesktopRuntimeCommand::RenameSession {
            command_id,
            session_id: session_id.to_owned(),
            name: name
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
        })
    }

    pub fn try_select_model(
        &self,
        command_id: u64,
        target: DesktopRuntimeOwnerTarget,
        model_id: &str,
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_runtime_owner_target(&target)?;
        validate_selection_id("model", model_id)?;
        self.try_send(DesktopRuntimeCommand::SelectModel {
            command_id,
            target,
            model_id: model_id.to_owned(),
            thinking_level,
        })
    }

    pub fn try_select_session_profile(
        &self,
        command_id: u64,
        target: DesktopRuntimeOwnerTarget,
        profile_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_runtime_owner_target(&target)?;
        validate_selection_id("profile", profile_id)?;
        self.try_send(DesktopRuntimeCommand::SelectSessionProfile {
            command_id,
            target,
            profile_id: profile_id.to_owned(),
        })
    }

    #[allow(dead_code, reason = "text-only typed desktop prompt API")]
    pub fn try_submit_prompt(
        &self,
        command_id: u64,
        target: DesktopPromptTarget,
        prompt: &str,
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(admitted_prompt_command(
            command_id,
            target,
            prompt,
            &[],
            thinking_level,
        )?)
    }

    pub fn try_submit_prompt_with_attachments(
        &self,
        command_id: u64,
        target: DesktopPromptTarget,
        prompt: &str,
        attachments: &[PathBuf],
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(admitted_prompt_command(
            command_id,
            target,
            prompt,
            attachments,
            thinking_level,
        )?)
    }

    #[allow(dead_code, reason = "single-session adapter compatibility")]
    pub fn try_abort(&self, command_id: u64) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(DesktopRuntimeCommand::Abort {
            command_id,
            session_id: None,
        })
    }

    pub fn try_abort_for_session(
        &self,
        command_id: u64,
        session_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        self.try_send(DesktopRuntimeCommand::Abort {
            command_id,
            session_id: Some(session_id.to_owned()),
        })
    }

    #[allow(dead_code, reason = "single-session adapter compatibility")]
    pub fn try_steer(
        &self,
        command_id: u64,
        text: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_control_text(text)?;
        self.try_send(DesktopRuntimeCommand::Steer {
            command_id,
            session_id: None,
            text: text.to_owned(),
        })
    }

    pub fn try_steer_for_session(
        &self,
        command_id: u64,
        session_id: &str,
        text: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        validate_control_text(text)?;
        self.try_send(DesktopRuntimeCommand::Steer {
            command_id,
            session_id: Some(session_id.to_owned()),
            text: text.to_owned(),
        })
    }

    #[allow(dead_code, reason = "single-session adapter compatibility")]
    pub fn try_follow_up(
        &self,
        command_id: u64,
        text: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_control_text(text)?;
        self.try_send(DesktopRuntimeCommand::FollowUp {
            command_id,
            session_id: None,
            text: text.to_owned(),
        })
    }

    pub fn try_follow_up_for_session(
        &self,
        command_id: u64,
        session_id: &str,
        text: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        validate_control_text(text)?;
        self.try_send(DesktopRuntimeCommand::FollowUp {
            command_id,
            session_id: Some(session_id.to_owned()),
            text: text.to_owned(),
        })
    }

    pub fn try_decide_tool_authorization(
        &self,
        command_id: u64,
        identity: &ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_authorization_identity(identity)?;
        self.try_send(DesktopRuntimeCommand::DecideToolAuthorization {
            command_id,
            session_id: None,
            identity: identity.clone(),
            decision,
        })
    }

    pub fn try_retry_recovery(
        &self,
        command_id: u64,
        identity: &DesktopRecoveryIdentity,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_recovery_identity(identity)?;
        self.try_send(DesktopRuntimeCommand::RetryRecovery {
            command_id,
            session_id: None,
            identity: identity.clone(),
        })
    }

    pub fn try_resolve_recovery(
        &self,
        command_id: u64,
        identity: &DesktopRecoveryIdentity,
        resolution: CodingAgentRecoveryResolution,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_recovery_identity(identity)?;
        self.try_send(DesktopRuntimeCommand::ResolveRecovery {
            command_id,
            session_id: None,
            identity: identity.clone(),
            resolution,
        })
    }

    pub fn try_review_changed_file(
        &self,
        command_id: u64,
        session_id: &str,
        request: &CodingAgentFileReviewRequest,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        validate_file_review_request(request)?;
        self.try_send(DesktopRuntimeCommand::ReviewChangedFile {
            command_id,
            session_id: session_id.to_owned(),
            request: request.clone(),
        })
    }

    pub fn try_open_external_editor(
        &self,
        command_id: u64,
        session_id: &str,
        target: &CodingAgentExternalEditorTarget,
        editor: &DesktopExternalEditorConfig,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        editor.validate().map_err(
            |error| DesktopCommandAdmissionError::InvalidExternalEditor {
                message: error.to_string(),
            },
        )?;
        self.try_send(DesktopRuntimeCommand::OpenExternalEditor {
            command_id,
            session_id: session_id.to_owned(),
            target: target.clone(),
            editor: editor.clone(),
        })
    }

    fn try_send(&self, command: DesktopRuntimeCommand) -> Result<(), DesktopCommandAdmissionError> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => DesktopCommandAdmissionError::QueueFull,
                mpsc::error::TrySendError::Closed(_) => DesktopCommandAdmissionError::RuntimeClosed,
            })
    }
}

fn data_precedes_priority(data: &DesktopRuntimeUpdate, priority: &DesktopRuntimeUpdate) -> bool {
    match (product_event_route(data), product_event_route(priority)) {
        (Some((data_session, data_sequence)), Some((priority_session, priority_sequence))) => {
            data_session == priority_session && data_sequence < priority_sequence
        }
        (Some((data_session, _)), None) => match priority {
            DesktopRuntimeUpdate::PromptFinished { snapshot, .. } => {
                data_session == snapshot.session.session.session_id
            }
            DesktopRuntimeUpdate::Stopped => true,
            _ => false,
        },
        _ => false,
    }
}

fn product_event_route(update: &DesktopRuntimeUpdate) -> Option<(&str, u64)> {
    match update {
        DesktopRuntimeUpdate::ProductEvent { session_id, event } => {
            Some((session_id, event.sequence()))
        }
        _ => None,
    }
}

fn is_streaming_data_update(update: &DesktopRuntimeUpdate) -> bool {
    matches!(
        update,
        DesktopRuntimeUpdate::ProductEvent { event, .. }
            if event.delivery_class() == CodingAgentProductEventDeliveryClass::Data
    )
}

impl Drop for DesktopRuntimeShutdownGuard {
    fn drop(&mut self) {
        self.signal();
        if let Some(runtime_thread) = self.runtime_thread.take() {
            spawn_runtime_reaper("desktop-runtime-reaper", runtime_thread);
        }
    }
}

fn spawn_runtime_reaper(name: &str, runtime_thread: JoinHandle<()>) {
    let _ = thread::Builder::new().name(name.into()).spawn(move || {
        let _ = runtime_thread.join();
    });
}
