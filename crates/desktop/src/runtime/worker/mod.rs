//! Dedicated runtime-thread state, session ownership, and ProductEvent pump.

use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use coding_agent::api::client::{
    CodingAgentClientConnection, CodingAgentFreshSnapshotRecovery, CodingAgentReconnectDelivery,
};
use coding_agent::api::embedding::{
    CodingAgentEmbeddingContext, CodingAgentEmbeddingOptions, CodingAgentThinkingLevel,
    CodingAgentThinkingLevelSanitization, CodingAgentWorkspaceScope, sanitize_thinking_level,
};
use coding_agent::api::operation::CodingAgentOperationOutcome;
use coding_agent::api::runtime::{CodingAgentSession, CodingAgentSessionNameUpdateReceiver};
use futures::stream::{FuturesUnordered, StreamExt as _};
use tokio::sync::{mpsc, watch};
use tokio::task;

pub(super) mod dispatch;
mod product_events;
mod session;
mod shutdown;

use self::dispatch::dispatch_command_with_updates;
use self::product_events::{
    acknowledge_product_event, drain_product_events, ensure_operation_started,
    publish_product_event, recover_product_event_source, recv_product_event,
};
#[allow(unused_imports)]
pub(super) use self::{
    product_events::{
        DesktopProductEventReceiver, DesktopProductEventSource, DesktopReconnectAttempt,
        establish_reconnect, publish_data_update, reconnect_event_source, recovery_update,
    },
    session::RuntimeState,
    shutdown::{close_active_prompt, shutdown_active_prompt, shutdown_active_prompt_with_deadline},
};
use super::protocol::{
    DesktopBridgeError, DesktopRuntimeCommand, DesktopRuntimeError, DesktopRuntimeHydratedSnapshot,
    DesktopRuntimeReadySnapshot, DesktopRuntimeUpdate, local_runtime_error, runtime_error,
};

const RUNTIME_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(10);
pub(super) const DESKTOP_CLIENT_ID: &str = "evo-desktop";

pub(super) struct HomeRuntimeContext {
    pub(super) context: CodingAgentEmbeddingContext,
    options: CodingAgentEmbeddingOptions,
}

impl HomeRuntimeContext {
    pub(super) fn load(options: CodingAgentEmbeddingOptions) -> Result<Self, DesktopBridgeError> {
        let context = CodingAgentEmbeddingContext::load(options.clone())?;
        if context.snapshot().workspace.is_none() {
            return Err(DesktopBridgeError::Session {
                message: "desktop runtime requires typed workspace embedding options".into(),
            });
        }
        Ok(Self { context, options })
    }

    fn load_session_context(
        &self,
    ) -> Result<(String, CodingAgentEmbeddingContext), DesktopBridgeError> {
        let (session_id, options) =
            self.options
                .clone()
                .into_new_session()
                .map_err(|error| DesktopBridgeError::Input {
                    message: format!("desktop session workspace could not be resolved: {error}"),
                })?;
        let context = CodingAgentEmbeddingContext::load(options)?;
        Ok((session_id, context))
    }

    pub(super) fn select_model(
        &mut self,
        model_id: String,
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) -> Result<(Option<CodingAgentThinkingLevel>, bool), DesktopBridgeError> {
        let thinking = admitted_model_thinking(&self.context, &model_id, thinking_level)?;
        self.context.select_model(model_id.clone())?;
        self.options = self.options.clone().with_model_id(model_id);
        Ok(thinking)
    }

    pub(super) fn select_profile(&mut self, profile_id: String) -> Result<(), DesktopBridgeError> {
        if !self
            .context
            .snapshot()
            .profiles
            .iter()
            .any(|profile| profile.id.as_str() == profile_id)
        {
            return Err(DesktopBridgeError::Input {
                message: format!("unknown desktop Home profile {profile_id}"),
            });
        }
        let options = self
            .options
            .clone()
            .with_default_agent_profile_id(profile_id);
        let context = CodingAgentEmbeddingContext::load(options.clone())?;
        self.options = options;
        self.context = context;
        Ok(())
    }
}

pub(super) fn admitted_model_thinking(
    context: &CodingAgentEmbeddingContext,
    model_id: &str,
    requested: Option<CodingAgentThinkingLevel>,
) -> Result<(Option<CodingAgentThinkingLevel>, bool), DesktopBridgeError> {
    let model = context
        .snapshot()
        .models
        .iter()
        .find(|model| model.id == model_id)
        .ok_or_else(|| DesktopBridgeError::Input {
            message: format!("unknown desktop model {model_id}"),
        })?;
    Ok(match requested {
        Some(requested) => match sanitize_thinking_level(model, requested) {
            CodingAgentThinkingLevelSanitization::Explicit(level) => (Some(level), false),
            CodingAgentThinkingLevelSanitization::AutoFallback => (None, true),
        },
        None => (None, false),
    })
}

pub(super) struct RuntimeSessionWorkspace {
    pub(super) scope: CodingAgentWorkspaceScope,
    pub(super) context: CodingAgentEmbeddingContext,
    pub(super) session: CodingAgentSession,
}

impl RuntimeSessionWorkspace {
    fn scope_for_context(
        context: &CodingAgentEmbeddingContext,
    ) -> Result<CodingAgentWorkspaceScope, DesktopBridgeError> {
        context
            .snapshot()
            .workspace
            .as_ref()
            .map(|workspace| workspace.scope.clone())
            .ok_or_else(|| DesktopBridgeError::Session {
                message: "desktop session context has no typed workspace scope".into(),
            })
    }

    fn new(
        context: CodingAgentEmbeddingContext,
        session: CodingAgentSession,
    ) -> Result<Self, DesktopBridgeError> {
        let scope = Self::scope_for_context(&context)?;
        Ok(Self {
            scope,
            context,
            session,
        })
    }
}

pub(super) struct NewPromptSession {
    pub(super) session_id: String,
    pub(super) snapshot: DesktopRuntimeHydratedSnapshot,
    pub(super) thinking_level: Option<CodingAgentThinkingLevel>,
}

pub(super) type PromptTaskOutput = (
    CodingAgentSession,
    Result<CodingAgentOperationOutcome, DesktopBridgeError>,
);

pub(super) struct ActivePrompt {
    pub(super) session_id: String,
    pub(super) command_id: u64,
    pub(super) operation_id: Option<String>,
    pub(super) scope: CodingAgentWorkspaceScope,
    pub(super) context: CodingAgentEmbeddingContext,
    pub(super) connection: CodingAgentClientConnection,
    pub(super) events: DesktopProductEventSource,
    pub(super) pending_recovery: Option<CodingAgentFreshSnapshotRecovery>,
    pub(super) last_forwarded_sequence: u64,
    pub(super) session_name_updates: Option<CodingAgentSessionNameUpdateReceiver>,
    pub(super) task: task::JoinHandle<PromptTaskOutput>,
}

enum ActivePromptSignal {
    Event(Box<Result<CodingAgentReconnectDelivery, DesktopBridgeError>>),
    Finished(Box<Result<PromptTaskOutput, task::JoinError>>),
}

enum RuntimeSignal {
    Command(Option<DesktopRuntimeCommand>),
    Active {
        session_id: String,
        signal: ActivePromptSignal,
    },
    Shutdown,
}

pub(super) async fn run_runtime(
    options: CodingAgentEmbeddingOptions,
    mut commands: mpsc::Receiver<DesktopRuntimeCommand>,
    mut shutdown: watch::Receiver<bool>,
    priority_updates: mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: mpsc::Sender<DesktopRuntimeUpdate>,
    ready: std_mpsc::SyncSender<Result<DesktopRuntimeReadySnapshot, DesktopRuntimeError>>,
) {
    let home = match HomeRuntimeContext::load(options) {
        Ok(home) => home,
        Err(error) => {
            let _ = ready.send(Err(runtime_error(&error)));
            return;
        }
    };
    let mut state = RuntimeState {
        home,
        workspaces: HashMap::new(),
        focused_session_id: None,
        #[cfg(test)]
        fail_next_prompt_start: false,
    };
    let initial = DesktopRuntimeReadySnapshot {
        project: state.home.context.snapshot().clone(),
    };
    if ready.send(Ok(initial)).is_err() {
        let _ = state.shutdown_idle_sessions().await;
        return;
    }

    let mut active = HashMap::<String, ActivePrompt>::new();
    loop {
        let mut active_ids = active.keys().cloned().collect::<Vec<_>>();
        active_ids.sort();
        for session_id in active_ids {
            let active_prompt = active
                .get_mut(&session_id)
                .expect("collected active session must remain present");
            if let Some(recovery) = active_prompt.pending_recovery.take() {
                active_prompt.last_forwarded_sequence = recovery.fresh_cursor.last_event_sequence;
                if priority_updates
                    .send(recovery_update(recovery))
                    .await
                    .is_err()
                {
                    shutdown_all_active_prompts(&mut active, &priority_updates).await;
                    return;
                }
            }
        }

        let signal = if active.is_empty() {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    let _ = changed;
                    RuntimeSignal::Shutdown
                }
                command = commands.recv() => RuntimeSignal::Command(command),
            }
        } else {
            let next_active = next_active_signal(&mut active);
            tokio::pin!(next_active);
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    let _ = changed;
                    RuntimeSignal::Shutdown
                }
                command = commands.recv() => RuntimeSignal::Command(command),
                active_signal = &mut next_active => {
                    let (session_id, signal) = active_signal
                        .expect("non-empty active prompt set must produce a signal");
                    RuntimeSignal::Active { session_id, signal }
                }
            }
        };
        match signal {
            RuntimeSignal::Shutdown | RuntimeSignal::Command(None) => break,
            RuntimeSignal::Command(Some(command)) => {
                let update = dispatch_command_with_updates(
                    &mut state,
                    &mut active,
                    Some(&priority_updates),
                    Some(&data_updates),
                    command,
                )
                .await;
                if priority_updates.send(update).await.is_err() {
                    break;
                }
            }
            RuntimeSignal::Active { session_id, signal } => match signal {
                ActivePromptSignal::Event(event) => match *event {
                    Ok(CodingAgentReconnectDelivery::Event(event)) => {
                        let active_prompt = active
                            .get_mut(&session_id)
                            .expect("signaled active prompt must remain present");
                        let sequence = event.sequence();
                        let candidate_operation_id = event.operation_id().map(str::to_owned);
                        if !ensure_operation_started(
                            active_prompt,
                            candidate_operation_id.as_deref(),
                            &priority_updates,
                        )
                        .await
                        {
                            shutdown_active_prompt(active.remove(&session_id), &priority_updates)
                                .await;
                            continue;
                        }
                        if !publish_product_event(
                            event,
                            active_prompt,
                            &priority_updates,
                            &data_updates,
                        )
                        .await
                        {
                            shutdown_active_prompt(active.remove(&session_id), &priority_updates)
                                .await;
                            continue;
                        }
                        if !acknowledge_product_event(active_prompt, sequence, &priority_updates)
                            .await
                        {
                            shutdown_active_prompt(active.remove(&session_id), &priority_updates)
                                .await;
                            continue;
                        }
                        active_prompt.last_forwarded_sequence = sequence;
                    }
                    Ok(CodingAgentReconnectDelivery::FreshSnapshotRequired(recovery)) => {
                        let active_prompt = active
                            .get_mut(&session_id)
                            .expect("signaled active prompt must remain present");
                        active_prompt.last_forwarded_sequence =
                            recovery.fresh_cursor.last_event_sequence;
                        if priority_updates
                            .send(recovery_update(recovery))
                            .await
                            .is_err()
                        {
                            shutdown_active_prompt(active.remove(&session_id), &priority_updates)
                                .await;
                            continue;
                        }
                    }
                    Err(error) => {
                        let active_prompt = active
                            .get_mut(&session_id)
                            .expect("signaled active prompt must remain present");
                        if !recover_product_event_source(active_prompt, error, &priority_updates)
                            .await
                        {
                            shutdown_active_prompt(active.remove(&session_id), &priority_updates)
                                .await;
                        }
                    }
                },
                ActivePromptSignal::Finished(result) => {
                    let result = *result;
                    let mut completed = active
                        .remove(&session_id)
                        .expect("signaled active prompt must remain present");
                    if !drain_product_events(&mut completed, &priority_updates, &data_updates).await
                    {
                        shutdown_active_prompt(Some(completed), &priority_updates).await;
                        continue;
                    }
                    let _ = completed.connection.detach();
                    match result {
                        Ok((session, operation_result)) => {
                            let prompt_succeeded = operation_result.is_ok();
                            let session_name_updates = completed.session_name_updates.take();
                            let operation_started =
                                ensure_operation_started(&mut completed, None, &priority_updates)
                                    .await;
                            let operation_id = completed.operation_id.take();
                            let command_id = completed.command_id;
                            state.insert_idle_workspace(
                                session_id.clone(),
                                completed.scope,
                                completed.context,
                                session,
                            );
                            if !operation_started {
                                continue;
                            }
                            let Some(operation_id) = operation_id else {
                                let _ = priority_updates
                                    .send(DesktopRuntimeUpdate::RuntimeFailed {
                                        error: DesktopRuntimeError {
                                            code: "operation_association_missing".into(),
                                            message: "completed desktop prompt has no product operation id"
                                                .into(),
                                        },
                                    })
                                    .await;
                                continue;
                            };
                            let snapshot = match state.snapshot(&session_id) {
                                Ok(snapshot) => snapshot,
                                Err(error) => {
                                    let _ = priority_updates
                                        .send(DesktopRuntimeUpdate::RuntimeFailed {
                                            error: runtime_error(&error),
                                        })
                                        .await;
                                    continue;
                                }
                            };
                            let error = operation_result.err().map(|error| runtime_error(&error));
                            if priority_updates
                                .send(DesktopRuntimeUpdate::PromptFinished {
                                    command_id,
                                    operation_id,
                                    snapshot,
                                    error,
                                })
                                .await
                                .is_err()
                            {
                                continue;
                            }
                            if prompt_succeeded && let Some(mut name_updates) = session_name_updates
                            {
                                let name_updates_sender = priority_updates.clone();
                                let named_session_id = session_id.clone();
                                task::spawn(async move {
                                    if let Some(update) = name_updates.changed().await {
                                        let _ = name_updates_sender
                                            .send(DesktopRuntimeUpdate::SessionNameObserved {
                                                session_id: named_session_id,
                                                name: update.name,
                                                updated_at: update.updated_at,
                                            })
                                            .await;
                                    }
                                });
                            }
                        }
                        Err(_) => {
                            let _ = priority_updates
                                .send(DesktopRuntimeUpdate::RuntimeFailed {
                                    error: local_runtime_error(
                                        "runtime_task_panicked",
                                        "A desktop runtime task stopped unexpectedly.",
                                    ),
                                })
                                .await;
                            continue;
                        }
                    }
                }
            },
        }
    }

    shutdown_all_active_prompts(&mut active, &priority_updates).await;
    let _ = state.shutdown_idle_sessions().await;
    let _ = priority_updates.send(DesktopRuntimeUpdate::Stopped).await;
}

async fn next_active_signal(
    active: &mut HashMap<String, ActivePrompt>,
) -> Option<(String, ActivePromptSignal)> {
    let pending = active
        .iter_mut()
        .map(|(session_id, prompt)| {
            let session_id = session_id.clone();
            async move {
                let signal = tokio::select! {
                    biased;
                    result = &mut prompt.task => {
                        ActivePromptSignal::Finished(Box::new(result))
                    }
                    event = recv_product_event(&mut prompt.events) => {
                        ActivePromptSignal::Event(Box::new(event))
                    }
                };
                (session_id, signal)
            }
        })
        .collect::<FuturesUnordered<_>>();
    pending.into_future().await.0
}

async fn shutdown_all_active_prompts(
    active: &mut HashMap<String, ActivePrompt>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) {
    let mut session_ids = active.keys().cloned().collect::<Vec<_>>();
    session_ids.sort();
    for session_id in session_ids {
        shutdown_active_prompt(active.remove(&session_id), priority_updates).await;
    }
}
