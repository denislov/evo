use std::time::{Duration, Instant};

use tui::api::component::{Component, OverlayAnchor, OverlayMargin, OverlayOptions, SizeValue};
use tui::api::input::{InputEvent, StdinBuffer, is_key_release};
use tui::api::render::{RenderScheduler, Tui, TuiError};
use tui::api::terminal::{Terminal, TerminalSize, detect_terminal_capabilities_from_env};

use crate::interactive::app::{PromptContext, session_label};
use crate::interactive::error::CliError;
use crate::interactive::input::InputPump;
use crate::interactive::prompt_task::{
    PromptTask, PromptTaskCompletion, PromptTaskFailure, PromptTaskResult,
};
use crate::interactive::root::{
    InteractiveAction, InteractiveRoot, InteractiveStatus, PendingAgentInvocationRequest,
    PendingAgentTeamRequest, PendingBranchSummaryRequest, PendingDelegationConfirmationCommand,
    PendingDelegationConfirmationSelection, PendingForkRequest, PendingInteractiveCommand,
    PendingSelfHealingEditRequest, TransientOverlayRole,
};
use crate::interactive::session_actions::{
    SessionChoiceKind, hydrate_existing_session_target, hydrated_session_from_snapshot,
};
use crate::interactive::{TranscriptItem, UiEvent};
use coding_agent::api::client::{
    CodingAgentClientConnection, CodingAgentClientId, CodingAgentReconnect,
    CodingAgentReconnectDelivery, CodingAgentReconnectReceiver, CodingAgentSnapshot,
};
use coding_agent::api::embedding::{
    CodingAgentAuthMutation, CodingAgentInteractiveStartup, CodingAgentResourceCommandKind,
};
use coding_agent::api::error::CodingAgentPublicError;
use coding_agent::api::event::CodingAgentProductEvent as ProductEvent;
use coding_agent::api::operation::{
    BranchSummaryReusePolicy, PromptInvocation, PromptTurnOutcome,
    SelfHealingEditModelRepairOptions, SelfHealingEditRequest,
};
use coding_agent::api::runtime::{CodingAgentSession, CodingAgentSessionBootstrap};
use coding_agent::api::settings::CodingAgentThemeSnapshot;

const NORMAL_RENDER_INTERVAL: Duration = Duration::from_millis(16);
const SPINNER_INTERVAL: Duration = Duration::from_millis(120);
const RESIZE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const SHUTDOWN_DRAIN_MAX: Duration = Duration::from_millis(1000);
const SHUTDOWN_DRAIN_IDLE: Duration = Duration::from_millis(50);

struct ResizeSource {
    #[cfg(unix)]
    platform: Option<tokio::signal::unix::Signal>,
    fallback: tokio::time::Interval,
}

impl ResizeSource {
    fn new() -> Self {
        let mut fallback = tokio::time::interval(RESIZE_POLL_INTERVAL);
        fallback.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Self {
            #[cfg(unix)]
            platform: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
                .ok(),
            fallback,
        }
    }

    async fn recv(&mut self) {
        #[cfg(unix)]
        tokio::select! {
            _ = receive_platform_resize(&mut self.platform), if self.platform.is_some() => {}
            _ = self.fallback.tick() => {}
        }
        #[cfg(not(unix))]
        self.fallback.tick().await;
    }
}

#[cfg(unix)]
async fn receive_platform_resize(signal: &mut Option<tokio::signal::unix::Signal>) {
    match signal {
        Some(signal) => {
            let _ = signal.recv().await;
        }
        None => std::future::pending().await,
    }
}

/// Print startup resource summary to stderr before the TUI takes over.
/// Respects the `quiet_startup` setting.
fn print_startup_banner(prompt_context: &PromptContext) {
    if prompt_context
        .settings_snapshot()
        .presentation
        .quiet_startup
    {
        return;
    }
    let cwd = prompt_context.cwd.clone();
    let cwd = cwd.canonicalize().unwrap_or(cwd);

    // [Context]
    if !prompt_context.context_files.is_empty() {
        let names: Vec<String> = prompt_context
            .context_files
            .iter()
            .map(|path| {
                // If the file's parent directory equals cwd, show just the file name.
                if let Some(parent) = path.parent()
                    && parent == cwd
                {
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string())
                } else {
                    path.display().to_string()
                }
            })
            .collect();
        eprintln!("[Context] {}", names.join(", "));
    }

    // [Skills]
    let skill_names: Vec<&str> = prompt_context
        .resource_commands
        .iter()
        .filter(|resource| {
            resource.kind == CodingAgentResourceCommandKind::Skill && resource.model_invocable
        })
        .map(|resource| resource.name.as_str())
        .collect();
    if !skill_names.is_empty() {
        eprintln!("[Skills] {}", skill_names.join(", "));
    }
}

fn print_exit_resume_hint(active_session_id: Option<&str>) {
    if let Some(session_id) = active_session_id {
        eprintln!("To resume this session: evo --session {session_id}");
    }
}

pub(super) struct LoopResult<T: Terminal> {
    terminal_type: std::marker::PhantomData<fn() -> T>,
    pub(super) exit_code: i32,
    pub(super) coding_session: Option<CodingAgentSession>,
}

struct InteractiveClientConnection {
    connection: CodingAgentClientConnection,
    receiver: CodingAgentReconnectReceiver,
    session_id: String,
}

fn detach_interactive_client(connection: &mut Option<InteractiveClientConnection>) {
    if let Some(connection) = connection.take() {
        let _ = connection.connection.detach();
    }
}

async fn receive_interactive_client(
    connection: &mut Option<InteractiveClientConnection>,
) -> Result<CodingAgentReconnectDelivery, CodingAgentPublicError> {
    match connection {
        Some(connection) => connection.receiver.recv().await,
        None => std::future::pending().await,
    }
}

async fn receive_prompt_connection_handoff(
    handoff: &mut Option<
        tokio::sync::oneshot::Receiver<Result<Option<CodingAgentClientConnection>, CliError>>,
    >,
) -> Result<
    Result<Option<CodingAgentClientConnection>, CliError>,
    tokio::sync::oneshot::error::RecvError,
> {
    match handoff.as_mut() {
        Some(receiver) => receiver.await,
        None => std::future::pending().await,
    }
}

enum PromptSourceEvent {
    ConnectionHandoff(
        Box<
            Result<
                Result<Option<CodingAgentClientConnection>, CliError>,
                tokio::sync::oneshot::error::RecvError,
            >,
        >,
    ),
    Completed(Box<PromptTaskCompletion>),
}

enum InteractiveLoopEvent {
    RenderDeadline,
    RuntimeTick,
    StdinTimeout,
    ResizeWake,
    Input(Option<String>),
    Prompt(Box<PromptSourceEvent>),
    Client(Box<Result<CodingAgentReconnectDelivery, CodingAgentPublicError>>),
    Theme(Box<CodingAgentThemeSnapshot>),
}

async fn receive_prompt_source(task: Option<&mut PromptTask>) -> PromptSourceEvent {
    let Some(task) = task else {
        return std::future::pending().await;
    };
    tokio::select! {
        handoff = receive_prompt_connection_handoff(&mut task.connection_handoff), if task.connection_handoff.is_some() => {
            PromptSourceEvent::ConnectionHandoff(Box::new(handoff))
        }
        done = &mut task.done => {
            PromptSourceEvent::Completed(Box::new(done.unwrap_or_else(|_| {
                PromptTaskCompletion::SetupFailed(CliError::AgentFailure(
                    "prompt task dropped before completion".to_string(),
                ))
            })))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RenderRequest {
    requested: bool,
    force: bool,
}

impl RenderRequest {
    const NONE: Self = Self {
        requested: false,
        force: false,
    };
    const FORCE: Self = Self {
        requested: true,
        force: true,
    };

    fn changed(changed: bool) -> Self {
        if changed {
            Self {
                requested: true,
                force: false,
            }
        } else {
            Self::NONE
        }
    }

    fn merge(self, other: Self) -> Self {
        Self {
            requested: self.requested || other.requested,
            force: self.force || other.force,
        }
    }
}

enum LoopControl {
    Continue(RenderRequest),
    Exit,
}

pub(super) trait InteractiveClock {
    fn now(&self) -> Instant;
}

struct SystemInteractiveClock;

impl InteractiveClock for SystemInteractiveClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

pub(super) async fn run_interactive_loop_with_input<T, F>(
    startup: CodingAgentInteractiveStartup,
    terminal: T,
    make_input: F,
) -> Result<LoopResult<T>, CliError>
where
    T: Terminal,
    F: FnOnce() -> InputPump,
{
    for diagnostic in &startup.diagnostics {
        eprintln!("[{}] {}", diagnostic.code, diagnostic.summary);
    }
    let prompt_context = PromptContext::from_startup(startup);

    print_startup_banner(&prompt_context);

    let mut tui = Tui::start(terminal).map_err(tui_error)?;
    let root_id = initialize_started_tui(&mut tui, &prompt_context)?;
    let mut input = make_input();

    let clock = SystemInteractiveClock;
    let loop_result =
        run_started_interactive_loop(&mut tui, root_id, &mut input, prompt_context, &clock).await;
    // Drain in-flight Kitty key release events before stopping.
    let _ = tui
        .terminal_mut()
        .drain_input(SHUTDOWN_DRAIN_MAX, SHUTDOWN_DRAIN_IDLE);
    let input_shutdown = input.shutdown().await.map_err(to_cli_error);
    let stop_result = tui.stop().map_err(tui_error);

    // Print resume hint after terminal cleanup.
    if let Ok(root) = root_ref(&tui, root_id) {
        print_exit_resume_hint(
            root.active_session
                .as_ref()
                .map(|choice| choice.id.as_str()),
        );
    }

    match (loop_result, input_shutdown, stop_result) {
        (Ok((exit_code, session)), Ok(()), Ok(())) => Ok(LoopResult {
            terminal_type: std::marker::PhantomData,
            exit_code,
            coding_session: session,
        }),
        (Err(error), _, _) => Err(error),
        (Ok(_), Err(error), _) | (Ok(_), Ok(()), Err(error)) => Err(error),
    }
}

fn initialize_started_tui<T: Terminal>(
    tui: &mut Tui<T>,
    prompt_context: &PromptContext,
) -> Result<usize, CliError> {
    let cwd = prompt_context.cwd.clone();
    let session_label = session_label(prompt_context.session_bootstrap.is_persistent());
    let root_id = tui.add_child_with_id(Box::new(
        InteractiveRoot::new_with_theme_models_and_settings(
            cwd,
            prompt_context.model_summary.id.clone(),
            session_label,
            prompt_context.theme.clone(),
            prompt_context.model_choices.clone(),
            prompt_context.settings_snapshot(),
            prompt_context.auth_controller.snapshot(),
        )
        .with_resolved_theme(prompt_context.resolved_theme.clone()),
    ));
    {
        let root = root_mut(tui, root_id)?;
        root.set_terminal_capabilities(detect_terminal_capabilities_from_env(
            std::env::vars(),
            || false,
        ));
        root.model_rotation = prompt_context.model_rotation.clone();
        root.session_query = prompt_context.session_query.clone();
        root.session_choices = prompt_context.session_choices.clone();
        root.model = Some(prompt_context.model_summary.clone());
        root.thinking_level = prompt_context.thinking_level.unwrap_or_default();
        root.resource_commands = prompt_context.resource_commands.clone();
        root.profile_catalog = prompt_context.profile_catalog.clone();
        root.set_default_agent_profile_id(prompt_context.default_agent_profile_id.clone());
        if let Some(hydrated) = hydrate_existing_session_target(&prompt_context.session_bootstrap)?
        {
            root.apply_hydrated_session(hydrated, None);
        }
    }
    install_transient_overlays(tui, root_id)?;
    tui.set_clear_on_shrink(
        prompt_context
            .settings_snapshot()
            .presentation
            .clear_on_shrink,
    );
    tui.set_focus(Some(root_id));
    sync_transient_overlays(tui, root_id)?;
    Ok(root_id)
}

fn install_transient_overlays<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
) -> Result<(), CliError> {
    let (support_component, modal_component) =
        root_ref(tui, root_id)?.transient_overlay_components();
    let support = tui.show_overlay(
        Box::new(support_component),
        transient_overlay_options(TransientOverlayRole::ComposerAssistance, 0),
    );
    support.hide(tui);
    let modal = tui.show_overlay(
        Box::new(modal_component),
        transient_overlay_options(TransientOverlayRole::ModalDialog, 0),
    );
    modal.hide(tui);
    root_mut(tui, root_id)?.install_transient_overlay_handles(support, modal);
    Ok(())
}

fn transient_overlay_options(role: TransientOverlayRole, bottom_margin: usize) -> OverlayOptions {
    match role {
        TransientOverlayRole::ComposerAssistance | TransientOverlayRole::SupportPrompt => {
            let assistance = role == TransientOverlayRole::ComposerAssistance;
            OverlayOptions {
                width: Some(SizeValue::Columns(72)),
                anchor: OverlayAnchor::BottomLeft,
                margin: OverlayMargin {
                    right: usize::from(!assistance) * 2,
                    bottom: bottom_margin,
                    left: usize::from(!assistance) * 2,
                    ..Default::default()
                },
                non_capturing: true,
                ..Default::default()
            }
        }
        TransientOverlayRole::ModalDialog => OverlayOptions {
            width: Some(SizeValue::Columns(72)),
            anchor: OverlayAnchor::Center,
            margin: OverlayMargin {
                top: 1,
                right: 2,
                bottom: 1,
                left: 2,
            },
            ..Default::default()
        },
        TransientOverlayRole::ContextRailDetail => OverlayOptions {
            width: Some(SizeValue::Columns(38)),
            anchor: OverlayAnchor::RightCenter,
            margin: OverlayMargin {
                top: 1,
                right: 0,
                bottom: bottom_margin,
                left: 0,
            },
            ..Default::default()
        },
        TransientOverlayRole::ContextDrawerDetail => OverlayOptions {
            width: Some(SizeValue::Percent(40)),
            anchor: OverlayAnchor::RightCenter,
            margin: OverlayMargin {
                bottom: bottom_margin,
                ..Default::default()
            },
            ..Default::default()
        },
        TransientOverlayRole::ContextPageDetail => OverlayOptions {
            width: Some(SizeValue::Percent(100)),
            anchor: OverlayAnchor::TopLeft,
            margin: OverlayMargin {
                bottom: 1,
                ..Default::default()
            },
            ..Default::default()
        },
    }
}

fn sync_transient_overlays<T: Terminal>(tui: &mut Tui<T>, root_id: usize) -> Result<(), CliError> {
    let Some((support, modal)) = root_ref(tui, root_id)?.transient_overlay_handles() else {
        return Ok(());
    };
    let size = tui.terminal().size();
    let projection = {
        let root = root_mut(tui, root_id)?;
        root.set_viewport_size(size.columns, size.rows);
        root.prepare_transient_overlays(size.columns)
    };

    tui.set_overlay_options(
        support,
        transient_overlay_options(projection.support_role, projection.bottom_margin),
    );
    support.set_hidden(tui, !projection.support_visible);

    let modal_was_visible = tui.has_overlay(modal);
    tui.set_overlay_options(
        modal,
        transient_overlay_options(projection.modal_role, projection.bottom_margin),
    );
    if projection.modal_visible {
        modal.set_hidden(tui, false);
        if !modal_was_visible {
            modal.focus(tui);
        }
    } else if modal_was_visible {
        modal.hide(tui);
    }
    Ok(())
}

async fn run_started_interactive_loop<T, C>(
    tui: &mut Tui<T>,
    root_id: usize,
    input: &mut InputPump,
    mut prompt_context: PromptContext,
    clock: &C,
) -> Result<(i32, Option<CodingAgentSession>), CliError>
where
    T: Terminal,
    C: InteractiveClock + ?Sized,
{
    let mut stdin_buffer = StdinBuffer::new();
    let mut running: Option<PromptTask> = None;
    let mut coding_session: Option<CodingAgentSession> = None;
    let mut client_connection = None;
    let mut input_open = true;
    let mut terminal_size = tui.terminal().size();
    let mut resize_source = ResizeSource::new();
    let mut render_scheduler = RenderScheduler::new(NORMAL_RENDER_INTERVAL);
    let mut runtime_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + SPINNER_INTERVAL,
        SPINNER_INTERVAL,
    );
    runtime_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    render_scheduler.request(true);
    flush_render_if_ready(tui, root_id, &mut render_scheduler, clock.now())?;

    // Start the theme hot-reload watcher. Only custom themes (a name other
    // than dark/light) are watched; built-in themes return an idle watcher.
    let settings = prompt_context.settings_snapshot();
    let active_theme_name = settings.presentation.theme.as_deref().unwrap_or("dark");
    let (_theme_watcher, mut theme_reload) = prompt_context
        .theme_controller
        .watch(active_theme_name, Duration::from_millis(100))
        .map_err(CliError::from)?;

    loop {
        flush_render_if_ready(tui, root_id, &mut render_scheduler, clock.now())?;
        if !input_open && running.is_none() {
            flush_pending_render(tui, root_id, &mut render_scheduler, clock.now())?;
            detach_interactive_client(&mut client_connection);
            return Ok((0, coding_session));
        }

        let render_delay = pending_render_delay(&render_scheduler, clock.now());
        let stdin_delay = stdin_pending_delay(&stdin_buffer, clock.now());
        let event = tokio::select! {
            _ = sleep_render_delay(render_delay), if render_delay.is_some() => {
                InteractiveLoopEvent::RenderDeadline
            }
            _ = sleep_stdin_pending(stdin_delay), if stdin_delay.is_some() => {
                InteractiveLoopEvent::StdinTimeout
            }
            _ = runtime_tick.tick(), if running.is_some() => {
                InteractiveLoopEvent::RuntimeTick
            }
            _ = resize_source.recv() => InteractiveLoopEvent::ResizeWake,
            chunk = input.recv(), if input_open => InteractiveLoopEvent::Input(chunk),
            event = receive_prompt_source(running.as_mut()), if running.is_some() => {
                InteractiveLoopEvent::Prompt(Box::new(event))
            }
            delivery = receive_interactive_client(&mut client_connection), if client_connection.is_some() => {
                InteractiveLoopEvent::Client(Box::new(delivery))
            }
            Some(reload) = theme_reload.recv() => InteractiveLoopEvent::Theme(Box::new(reload)),
        };

        match event {
            InteractiveLoopEvent::RenderDeadline => {
                flush_render_if_ready(tui, root_id, &mut render_scheduler, clock.now())?;
            }
            InteractiveLoopEvent::RuntimeTick => {
                schedule_runtime_refresh(tui, root_id, &mut render_scheduler);
            }
            InteractiveLoopEvent::StdinTimeout => {
                let events = stdin_buffer.tick(clock.now());
                if !events.is_empty()
                    && matches!(
                        process_input_events(
                            tui,
                            root_id,
                            events,
                            &mut prompt_context,
                            &mut running,
                            &mut coding_session,
                            &mut render_scheduler,
                            clock.now(),
                        )
                        .await?,
                        LoopControl::Exit
                    )
                {
                    detach_interactive_client(&mut client_connection);
                    return Ok((0, coding_session));
                }
            }
            InteractiveLoopEvent::ResizeWake => {
                schedule_resize_render(tui, &mut terminal_size, &mut render_scheduler);
                flush_render_if_ready(tui, root_id, &mut render_scheduler, clock.now())?;
            }
            InteractiveLoopEvent::Input(chunk) => {
                let control = match chunk {
                    Some(chunk) => {
                        let control = process_input_events(
                            tui,
                            root_id,
                            stdin_buffer.process_at(&chunk, clock.now()),
                            &mut prompt_context,
                            &mut running,
                            &mut coding_session,
                            &mut render_scheduler,
                            clock.now(),
                        )
                        .await?;
                        if matches!(control, LoopControl::Continue(_)) {
                            input.mark_processed(&chunk);
                        }
                        control
                    }
                    None => {
                        input_open = false;
                        process_stdin_eof(
                            tui,
                            root_id,
                            &mut stdin_buffer,
                            &mut prompt_context,
                            &mut running,
                            &mut coding_session,
                            &mut render_scheduler,
                            clock.now(),
                        )
                        .await?
                    }
                };
                if matches!(control, LoopControl::Exit) {
                    detach_interactive_client(&mut client_connection);
                    return Ok((0, coding_session));
                }
                if running.is_some() {
                    tokio::task::yield_now().await;
                } else {
                    input.mark_idle();
                }
            }
            InteractiveLoopEvent::Prompt(event) => match *event {
                PromptSourceEvent::ConnectionHandoff(handoff) => {
                    if let Some(task) = running.as_mut() {
                        task.connection_handoff = None;
                    }
                    schedule_render(
                        &mut render_scheduler,
                        apply_prompt_connection_handoff(
                            tui,
                            root_id,
                            &mut client_connection,
                            *handoff,
                        )?,
                    );
                }
                PromptSourceEvent::Completed(result) => {
                    let mut task = running
                        .take()
                        .expect("prompt source requires a running task");
                    if let Some(mut handoff) = task.connection_handoff.take()
                        && let Ok(handoff) = handoff.try_recv()
                    {
                        schedule_render(
                            &mut render_scheduler,
                            apply_prompt_connection_handoff(
                                tui,
                                root_id,
                                &mut client_connection,
                                Ok(handoff),
                            )?,
                        );
                    }
                    schedule_render(
                        &mut render_scheduler,
                        drain_interactive_client(tui, root_id, &mut client_connection)?,
                    );
                    finish_prompt(
                        tui,
                        root_id,
                        *result,
                        &mut coding_session,
                        &mut prompt_context.session_bootstrap,
                    )?;
                    if let Some(session) = coding_session.as_ref() {
                        let session_view = session.view()?;
                        if client_connection.as_ref().is_some_and(|connection| {
                            connection.session_id != session_view.session_id
                        }) {
                            detach_interactive_client(&mut client_connection);
                        }
                        if client_connection.is_none() {
                            let (connection, request) =
                                connect_interactive_client(tui, root_id, session)?;
                            client_connection = Some(connection);
                            schedule_render(&mut render_scheduler, request);
                        }
                        prompt_context.default_agent_profile_id =
                            session_view.default_agent_profile_id.clone();
                        prompt_context
                            .profile_catalog
                            .sync_default_agent_profile(&session_view.default_agent_profile_id);
                        prompt_context.session_bootstrap = prompt_context
                            .session_bootstrap
                            .clone()
                            .with_default_agent_profile_id(
                                session_view.default_agent_profile_id.clone(),
                            );
                    }
                    schedule_render(&mut render_scheduler, RenderRequest::FORCE);
                    flush_render_if_ready(tui, root_id, &mut render_scheduler, clock.now())?;
                    input.mark_idle();
                }
            },
            InteractiveLoopEvent::Client(delivery) => {
                let request = apply_interactive_client_delivery(
                    tui,
                    root_id,
                    &mut client_connection,
                    *delivery,
                )?;
                schedule_render(&mut render_scheduler, request);
            }
            InteractiveLoopEvent::Theme(reload) => {
                apply_theme_reload(tui, root_id, *reload);
                render_scheduler.request(true);
            }
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "stdin EOF flushes through the same explicitly owned interactive reducer state"
)]
async fn process_stdin_eof<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    stdin_buffer: &mut StdinBuffer,
    prompt_context: &mut PromptContext,
    running: &mut Option<PromptTask>,
    coding_session: &mut Option<CodingAgentSession>,
    render_scheduler: &mut RenderScheduler,
    now: Instant,
) -> Result<LoopControl, CliError> {
    process_input_events(
        tui,
        root_id,
        stdin_buffer.flush(),
        prompt_context,
        running,
        coding_session,
        render_scheduler,
        now,
    )
    .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "interactive loop dependencies remain explicit and borrow-scoped"
)]
async fn process_input_events<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    events: Vec<InputEvent>,
    prompt_context: &mut PromptContext,
    running: &mut Option<PromptTask>,
    coding_session: &mut Option<CodingAgentSession>,
    render_scheduler: &mut RenderScheduler,
    now: Instant,
) -> Result<LoopControl, CliError> {
    for event in events {
        match handle_input_event(tui, root_id, event, prompt_context, running, coding_session)
            .await?
        {
            LoopControl::Continue(request) => {
                schedule_render(render_scheduler, request);
                flush_render_if_ready(tui, root_id, render_scheduler, now)?;
            }
            LoopControl::Exit => return Ok(LoopControl::Exit),
        }
    }
    Ok(LoopControl::Continue(RenderRequest::NONE))
}

fn schedule_render(render_scheduler: &mut RenderScheduler, request: RenderRequest) {
    if request.requested {
        render_scheduler.request(request.force);
    }
}

fn schedule_runtime_refresh<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    render_scheduler: &mut RenderScheduler,
) {
    if let Some(root) = tui.component_as_mut::<InteractiveRoot>(root_id) {
        root.spinner_frame = root.spinner_frame.wrapping_add(1);
    }
    render_scheduler.request(true);
}

fn schedule_resize_render<T: Terminal>(
    tui: &Tui<T>,
    previous_size: &mut TerminalSize,
    render_scheduler: &mut RenderScheduler,
) {
    let current_size = tui.terminal().size();
    if current_size != *previous_size {
        *previous_size = current_size;
        render_scheduler.request(true);
    }
}

fn pending_render_delay(render_scheduler: &RenderScheduler, now: Instant) -> Option<Duration> {
    render_scheduler
        .next_render_at(now)
        .map(|deadline| deadline.saturating_duration_since(now))
}

async fn sleep_render_delay(delay: Option<Duration>) {
    if let Some(delay) = delay {
        tokio::time::sleep(delay).await;
    }
}

fn stdin_pending_delay(stdin_buffer: &StdinBuffer, now: Instant) -> Option<Duration> {
    stdin_buffer.pending_timeout_at(now)
}

async fn sleep_stdin_pending(delay: Option<Duration>) {
    if let Some(delay) = delay {
        tokio::time::sleep(delay).await;
    }
}

fn flush_render_if_ready<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    render_scheduler: &mut RenderScheduler,
    now: Instant,
) -> Result<(), CliError> {
    if render_scheduler.should_render_now(now) {
        sync_transient_overlays(tui, root_id)?;
        render_tui(tui)?;
        render_scheduler.mark_rendered(now);
    }
    Ok(())
}

fn flush_pending_render<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    render_scheduler: &mut RenderScheduler,
    now: Instant,
) -> Result<(), CliError> {
    if render_scheduler.has_pending() {
        render_scheduler.request(true);
        flush_render_if_ready(tui, root_id, render_scheduler, now)?;
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "interactive input dispatch keeps mutable owners explicit and borrow-scoped"
)]
async fn handle_input_event<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    event: InputEvent,
    prompt_context: &mut PromptContext,
    running: &mut Option<PromptTask>,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<LoopControl, CliError> {
    if is_key_release(&event) {
        return Ok(LoopControl::Continue(RenderRequest::NONE));
    }

    let before = root_ref(tui, root_id)?.render_state();
    tui.dispatch_input(&event);
    root_mut(tui, root_id)?.drain_modal_overlay_input();

    let (
        action,
        prompt,
        prompt_invocation,
        selected_model,
        selected_thinking_level,
        selected_agent_profile_id,
        selected_session,
        selected_session_hydrate,
        settings_command,
        auth_command,
        compact_instructions,
        branch_summary_request,
        agent_invocation_request,
        agent_team_request,
        delegation_confirmation_command,
        tool_authorization_decision,
        self_healing_edit_request,
        fork_request,
        mut render_request,
    ) = {
        let root = root_mut(tui, root_id)?;
        let action = root.take_action();
        let mut prompt = None;
        let mut prompt_invocation = None;
        let mut selected_agent_profile_id = None;
        let mut compact_instructions = None;
        let mut branch_summary_request = None;
        let mut agent_invocation_request = None;
        let mut agent_team_request = None;
        let mut self_healing_edit_request = None;
        let mut fork_request = None;
        if let Some(command) = root.take_pending_command() {
            debug_assert_eq!(action, command.action());
            match command {
                PendingInteractiveCommand::Submit(text)
                | PendingInteractiveCommand::FollowUp(text) => prompt = Some(text),
                PendingInteractiveCommand::SubmitResource {
                    display_text,
                    invocation,
                } => {
                    prompt = Some(display_text);
                    prompt_invocation = Some(invocation);
                }
                PendingInteractiveCommand::Compact { instructions } => {
                    compact_instructions = instructions;
                }
                PendingInteractiveCommand::BranchSummary(request) => {
                    branch_summary_request = Some(request);
                }
                PendingInteractiveCommand::Fork(request) => fork_request = Some(request),
                PendingInteractiveCommand::AgentInvocation(request) => {
                    agent_invocation_request = Some(request);
                }
                PendingInteractiveCommand::AgentTeam(request) => {
                    agent_team_request = Some(request);
                }
                PendingInteractiveCommand::SelfHealingEdit(request) => {
                    self_healing_edit_request = Some(request);
                }
                PendingInteractiveCommand::UseAgentProfile(profile_id) => {
                    selected_agent_profile_id = Some(profile_id);
                }
            }
        }
        let selected_model = root.take_selected_model();
        let selected_thinking_level = root.take_selected_thinking_level();
        let selected_session = root.take_selected_session();
        let selected_session_hydrate = root.take_selected_session_hydrate();
        let settings_command = root.take_settings_command();
        let auth_command = root.take_auth_command();
        let delegation_confirmation_command = if action == InteractiveAction::DelegationConfirmation
        {
            root.take_pending_delegation_confirmation_command()
        } else {
            None
        };
        let tool_authorization_decision = if action == InteractiveAction::ToolAuthorization {
            root.take_pending_tool_authorization_decision()
        } else {
            None
        };
        let after = root.render_state();
        (
            action,
            prompt,
            prompt_invocation,
            selected_model,
            selected_thinking_level,
            selected_agent_profile_id,
            selected_session,
            selected_session_hydrate,
            settings_command,
            auth_command,
            compact_instructions,
            branch_summary_request,
            agent_invocation_request,
            agent_team_request,
            delegation_confirmation_command,
            tool_authorization_decision,
            self_healing_edit_request,
            fork_request,
            RenderRequest::changed(before != after),
        )
    };
    sync_transient_overlays(tui, root_id)?;

    if let Some(model) = selected_model {
        let diagnostic_text = prompt_context.select_model(&model)?;
        if !diagnostic_text.is_empty() {
            eprint!("{diagnostic_text}");
        }
        let root = root_mut(tui, root_id)?;
        root.available_models = prompt_context.model_choices.clone();
        root.auth_snapshot = prompt_context.auth_controller.snapshot();
    }
    if let Some(thinking_level) = selected_thinking_level {
        prompt_context.thinking_level = Some(thinking_level);
    }
    if let Some(profile_id) = selected_agent_profile_id {
        if coding_session.is_some() {
            let root = root_mut(tui, root_id)?;
            root.transcript.push(TranscriptItem::system(
                "The session profile is locked to the choice made at session creation. Start a new session to use a different agent profile.",
            ));
            return Ok(LoopControl::Continue(RenderRequest::FORCE));
        } else {
            prompt_context.default_agent_profile_id = profile_id.clone();
            prompt_context
                .profile_catalog
                .sync_default_agent_profile(&profile_id);
            prompt_context.session_bootstrap = prompt_context
                .session_bootstrap
                .clone()
                .with_default_agent_profile_id(profile_id);
        }
    }
    if let Some(session) = selected_session {
        *coding_session = None;
        prompt_context.session_bootstrap = prompt_context
            .session_bootstrap
            .clone()
            .with_session_id(session.id.clone());
        prompt_context
            .operation_factory
            .bind_session_bootstrap(&prompt_context.session_bootstrap);
        if selected_session_hydrate
            && let Some(hydrated) =
                hydrate_existing_session_target(&prompt_context.session_bootstrap)?
        {
            let root = root_mut(tui, root_id)?;
            root.apply_hydrated_session(
                hydrated,
                Some(format!("Session selected: {}", session.display_name())),
            );
        }
    }
    if let Some(command) = settings_command {
        match prompt_context.apply_settings_command(command) {
            Ok(outcome) => {
                let clear_on_shrink = outcome.snapshot.presentation.clear_on_shrink;
                let show_progress = outcome.snapshot.presentation.show_progress;
                root_mut(tui, root_id)?.settings = outcome.snapshot;
                tui.set_clear_on_shrink(clear_on_shrink);
                set_terminal_progress(tui, running.is_some() && show_progress)?;
            }
            Err(error) => {
                let root = root_mut(tui, root_id)?;
                root.apply_prompt_context(prompt_context);
                root.transcript.push(TranscriptItem::system(format!(
                    "Failed to update settings: {}",
                    error.summary
                )));
            }
        }
        render_request = RenderRequest::FORCE;
    }
    if let Some(command) = auth_command {
        let root = root_mut(tui, root_id)?;
        match prompt_context.apply_auth_command(command) {
            Ok(outcome) => {
                root.auth_snapshot = outcome.snapshot;
                root.available_models = prompt_context.model_choices.clone();
                let notice = match outcome.mutation {
                    CodingAgentAuthMutation::Stored => {
                        format!("Saved API key for {}", outcome.provider)
                    }
                    CodingAgentAuthMutation::Removed => {
                        format!("Removed stored auth for {}", outcome.provider)
                    }
                    CodingAgentAuthMutation::NotFound => {
                        format!("No stored auth found for {}", outcome.provider)
                    }
                };
                root.transcript.push(TranscriptItem::system(notice));
            }
            Err(error) => {
                root.transcript.push(TranscriptItem::system(format!(
                    "Failed to update provider authentication: {}",
                    error.summary
                )));
            }
        }
        render_request = RenderRequest::FORCE;
    }

    let tree_label_change = if running.is_none() {
        root_mut(tui, root_id)?.take_pending_tree_label_change()
    } else {
        None
    };
    if let Some((entry_id, label)) = tree_label_change {
        start_tree_label_task(
            tui,
            root_id,
            entry_id,
            label,
            prompt_context,
            running,
            coding_session,
        )?;
        return Ok(LoopControl::Continue(RenderRequest::FORCE));
    }

    // Process tree navigation.
    let mut tree_navigation_summary: Option<(String, String)> = None;
    let mut tree_navigation_fork: Option<String> = None;
    {
        let root = root_mut(tui, root_id)?;
        if let Some(target_id) = root.take_selected_tree_entry_id() {
            if let Some(choice) = root
                .active_session
                .as_ref()
                .filter(|choice| choice.kind == SessionChoiceKind::Persistent)
                .cloned()
            {
                let current_leaf_id = choice
                    .active_leaf_id
                    .clone()
                    .or_else(|| root.active_leaf_id.clone());
                if current_leaf_id.as_deref() == Some(target_id.as_str()) {
                    root.transcript
                        .push(TranscriptItem::system("Already at this point".to_string()));
                } else if let Some(source_leaf_id) = current_leaf_id {
                    tree_navigation_summary = Some((source_leaf_id, target_id));
                } else {
                    tree_navigation_fork = Some(target_id);
                }
            } else {
                root.transcript.push(TranscriptItem::system(
                    "No active Rust-native session for tree navigation".to_string(),
                ));
            }
        }
    }
    if let Some((source_leaf_id, target_leaf_id)) = tree_navigation_summary {
        if running.is_some() {
            let root = root_mut(tui, root_id)?;
            root.transcript.push(TranscriptItem::system(
                "Wait for the current run to finish before navigating the session tree.",
            ));
            return Ok(LoopControl::Continue(RenderRequest::FORCE));
        }
        *running = Some(start_branch_summary_navigation_task(
            tui,
            root_id,
            source_leaf_id,
            target_leaf_id,
            prompt_context,
            coding_session,
        )?);
        return Ok(LoopControl::Continue(RenderRequest::FORCE));
    }
    if let Some(target_id) = tree_navigation_fork {
        if running.is_some() {
            let root = root_mut(tui, root_id)?;
            root.transcript.push(TranscriptItem::system(
                "Wait for the current run to finish before navigating the session tree.",
            ));
            return Ok(LoopControl::Continue(RenderRequest::FORCE));
        }
        start_tree_navigation_fork_task(
            tui,
            root_id,
            target_id,
            prompt_context,
            running,
            coding_session,
        )?;
        return Ok(LoopControl::Continue(RenderRequest::FORCE));
    }

    match action {
        InteractiveAction::None => Ok(LoopControl::Continue(render_request)),
        InteractiveAction::Exit => {
            set_terminal_progress(tui, false)?;
            Ok(LoopControl::Exit)
        }
        InteractiveAction::AbortRunning => {
            if let Some(task) = running.as_mut() {
                task.abort_once().await;
            }
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::ToolAuthorization => {
            let Some((request, decision)) = tool_authorization_decision else {
                return Ok(LoopControl::Continue(render_request));
            };
            let accepted = match running.as_ref() {
                Some(task) => {
                    task.decide_tool_authorization(request.identity(), decision)
                        .await
                }
                None => false,
            };
            if !accepted {
                let root = root_mut(tui, root_id)?;
                root.restore_tool_authorization(request);
                root.transcript.push(TranscriptItem::system(
                    "Tool authorization decision could not be delivered to the active operation.",
                ));
            }
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::NewSession => {
            if prompt_context.session_bootstrap.is_persistent() {
                *coding_session = None;
                prompt_context.session_bootstrap = prompt_context
                    .session_bootstrap
                    .clone()
                    .with_fresh_session();
                prompt_context
                    .operation_factory
                    .bind_session_bootstrap(&prompt_context.session_bootstrap);
            }
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::ReloadResources => {
            if running.is_some() {
                let root = root_mut(tui, root_id)?;
                root.transcript.push(TranscriptItem::system(
                    "Wait for the current run to finish before reloading local resources.",
                ));
                return Ok(LoopControl::Continue(RenderRequest::FORCE));
            }
            match prompt_context.reload() {
                Ok(mut reloaded) => {
                    reloaded.default_agent_profile_id =
                        prompt_context.default_agent_profile_id.clone();
                    reloaded
                        .profile_catalog
                        .sync_default_agent_profile(&prompt_context.default_agent_profile_id);
                    reloaded.session_bootstrap = reloaded
                        .session_bootstrap
                        .inherit_initial_session_name_from(&prompt_context.session_bootstrap)
                        .with_default_agent_profile_id(
                            prompt_context.default_agent_profile_id.clone(),
                        );
                    reloaded
                        .operation_factory
                        .bind_session_bootstrap(&reloaded.session_bootstrap);
                    *prompt_context = reloaded;
                    let root = root_mut(tui, root_id)?;
                    root.apply_prompt_context(prompt_context);
                    root.transcript.push(TranscriptItem::system(
                        "Reloaded local configuration and resources",
                    ));
                }
                Err(error) => {
                    let root = root_mut(tui, root_id)?;
                    root.transcript
                        .push(TranscriptItem::system(format!("Reload failed: {error}")));
                }
            }
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::AgentProfileUse => Ok(LoopControl::Continue(RenderRequest::FORCE)),
        InteractiveAction::AgentInvocation => {
            if running.is_some() {
                return Ok(LoopControl::Continue(render_request));
            }
            let Some(request) = agent_invocation_request else {
                return Ok(LoopControl::Continue(render_request));
            };
            *running = Some(start_agent_invocation_task(
                tui,
                root_id,
                request,
                prompt_context,
                coding_session,
            )?);
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::AgentTeam => {
            if running.is_some() {
                return Ok(LoopControl::Continue(render_request));
            }
            let Some(request) = agent_team_request else {
                return Ok(LoopControl::Continue(render_request));
            };
            *running = Some(start_agent_team_task(
                tui,
                root_id,
                request,
                prompt_context,
                coding_session,
            )?);
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::DelegationConfirmation => {
            if running.is_some() {
                return Ok(LoopControl::Continue(render_request));
            }
            let Some(command) = delegation_confirmation_command else {
                return Ok(LoopControl::Continue(render_request));
            };
            handle_delegation_confirmation_command(
                tui,
                root_id,
                command,
                prompt_context,
                running,
                coding_session,
            )?;
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::Submit => {
            let Some(prompt) = prompt else {
                return Ok(LoopControl::Continue(render_request));
            };
            if prompt.trim().is_empty() {
                return Ok(LoopControl::Continue(render_request));
            }
            if let Some(task) = running.as_ref() {
                if prompt_invocation.is_some() {
                    let root = root_mut(tui, root_id)?;
                    root.transcript.push(TranscriptItem::system(
                        "Wait for the current run to finish before invoking a skill or prompt template.",
                    ));
                    return Ok(LoopControl::Continue(RenderRequest::FORCE));
                }
                if task.steer(prompt).await {
                    return Ok(LoopControl::Continue(RenderRequest::FORCE));
                }
                return Ok(LoopControl::Continue(render_request));
            }
            *running = Some(start_prompt_task(
                tui,
                root_id,
                prompt,
                prompt_invocation,
                prompt_context,
                coding_session,
            )?);
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::FollowUp => {
            let Some(prompt) = prompt else {
                return Ok(LoopControl::Continue(render_request));
            };
            if prompt.trim().is_empty() {
                return Ok(LoopControl::Continue(render_request));
            }
            if let Some(task) = running.as_ref()
                && task.follow_up(prompt).await
            {
                return Ok(LoopControl::Continue(RenderRequest::FORCE));
            }
            Ok(LoopControl::Continue(render_request))
        }
        InteractiveAction::CompactSession => {
            if running.is_some() {
                return Ok(LoopControl::Continue(render_request));
            }
            *running = Some(start_compact_task(
                tui,
                root_id,
                compact_instructions,
                prompt_context,
                coding_session,
            )?);
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::BranchSummary => {
            if running.is_some() {
                return Ok(LoopControl::Continue(render_request));
            }
            let Some(request) = branch_summary_request else {
                return Ok(LoopControl::Continue(render_request));
            };
            *running = Some(start_branch_summary_task(
                tui,
                root_id,
                request,
                prompt_context,
                coding_session,
            )?);
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::SelfHealingEdit => {
            if running.is_some() {
                return Ok(LoopControl::Continue(render_request));
            }
            let Some(request) = self_healing_edit_request else {
                return Ok(LoopControl::Continue(render_request));
            };
            *running = Some(start_self_healing_edit_task(
                tui,
                root_id,
                request,
                prompt_context,
                coding_session,
            )?);
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
        InteractiveAction::Fork => {
            if running.is_some() {
                return Ok(LoopControl::Continue(render_request));
            }
            let Some(request) = fork_request else {
                return Ok(LoopControl::Continue(render_request));
            };
            start_fork_task(
                tui,
                root_id,
                request,
                prompt_context,
                running,
                coding_session,
            )?;
            Ok(LoopControl::Continue(RenderRequest::FORCE))
        }
    }
}

fn handle_delegation_confirmation_command<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    command: PendingDelegationConfirmationCommand,
    prompt_context: &PromptContext,
    running: &mut Option<PromptTask>,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<(), CliError> {
    match command {
        PendingDelegationConfirmationCommand::List => {
            show_pending_delegation_confirmations(tui, root_id, coding_session.as_ref())
        }
        PendingDelegationConfirmationCommand::Approve { selection } => {
            start_delegation_approval_task(
                tui,
                root_id,
                selection,
                prompt_context,
                running,
                coding_session,
            )
        }
        PendingDelegationConfirmationCommand::Reject { selection, reason } => {
            reject_pending_delegation_confirmation(
                tui,
                root_id,
                selection,
                reason,
                prompt_context,
                running,
                coding_session,
            )
        }
    }
}

fn show_pending_delegation_confirmations<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    coding_session: Option<&CodingAgentSession>,
) -> Result<(), CliError> {
    let Some(session) = coding_session else {
        root_mut(tui, root_id)?
            .transcript
            .push(TranscriptItem::system("No active coding session."));
        return Ok(());
    };
    let pending = session.pending_delegation_confirmations();
    if pending.is_empty() {
        root_mut(tui, root_id)?
            .transcript
            .push(TranscriptItem::system(
                "No pending delegation confirmations.",
            ));
        return Ok(());
    }
    root_mut(tui, root_id)?.open_delegation_confirmation_menu(pending);
    Ok(())
}

fn start_delegation_approval_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    selection: PendingDelegationConfirmationSelection,
    prompt_context: &PromptContext,
    running: &mut Option<PromptTask>,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<(), CliError> {
    let Some(session) = coding_session.as_ref() else {
        root_mut(tui, root_id)?
            .transcript
            .push(TranscriptItem::system("No active coding session."));
        return Ok(());
    };
    let (operation_id, tool_call_id) =
        match resolve_pending_delegation_confirmation(session, &selection) {
            Ok(resolved) => resolved,
            Err(message) => {
                root_mut(tui, root_id)?
                    .transcript
                    .push(TranscriptItem::system(message));
                return Ok(());
            }
        };

    let session = coding_session
        .take()
        .expect("coding session was checked before starting delegation approval");
    {
        let root = root_mut(tui, root_id)?;
        root.transcript.push(TranscriptItem::system(format!(
            "Approving delegation: {operation_id} {tool_call_id}"
        )));
        root.set_status(InteractiveStatus::Running);
    }
    *running = Some(PromptTask::spawn_delegation_approval(
        session,
        operation_id,
        tool_call_id,
    )?);
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(())
}

fn start_tree_label_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    entry_id: String,
    label: Option<String>,
    prompt_context: &PromptContext,
    running: &mut Option<PromptTask>,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<(), CliError> {
    let is_rust_native = root_mut(tui, root_id)?
        .active_session
        .as_ref()
        .is_some_and(|choice| choice.kind == SessionChoiceKind::Persistent);
    if !is_rust_native || coding_session.is_none() {
        root_mut(tui, root_id)?
            .transcript
            .push(TranscriptItem::system(
                "No active Rust-native session for tree label changes.",
            ));
        return Ok(());
    }
    let session = coding_session
        .take()
        .expect("coding session was checked before starting tree label mutation");
    root_mut(tui, root_id)?.set_status(InteractiveStatus::Running);
    *running = Some(PromptTask::spawn_session_tree_label(
        session, entry_id, label,
    )?);
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(())
}

fn reject_pending_delegation_confirmation<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    selection: PendingDelegationConfirmationSelection,
    reason: Option<String>,
    prompt_context: &PromptContext,
    running: &mut Option<PromptTask>,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<(), CliError> {
    let Some(session) = coding_session.as_ref() else {
        root_mut(tui, root_id)?
            .transcript
            .push(TranscriptItem::system("No active coding session."));
        return Ok(());
    };
    let (operation_id, tool_call_id) =
        match resolve_pending_delegation_confirmation(session, &selection) {
            Ok(resolved) => resolved,
            Err(message) => {
                root_mut(tui, root_id)?
                    .transcript
                    .push(TranscriptItem::system(message));
                return Ok(());
            }
        };

    let session = coding_session
        .take()
        .expect("coding session was checked before starting delegation rejection");
    {
        let root = root_mut(tui, root_id)?;
        root.set_status(InteractiveStatus::Running);
    }
    *running = Some(PromptTask::spawn_delegation_rejection(
        session,
        operation_id,
        tool_call_id,
        reason.unwrap_or_else(|| "delegation rejected by user".to_string()),
    )?);
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(())
}

fn resolve_pending_delegation_confirmation(
    session: &CodingAgentSession,
    selection: &PendingDelegationConfirmationSelection,
) -> Result<(String, String), String> {
    let pending = session.pending_delegation_confirmations();
    if pending.is_empty() {
        return Err("No pending delegation confirmations.".to_string());
    }
    if let Some(operation_id) = selection.operation_id.as_deref() {
        return pending
            .iter()
            .find(|pending| {
                pending.operation_id == operation_id
                    && pending.tool_call_id == selection.tool_call_id
            })
            .map(|pending| (pending.operation_id.clone(), pending.tool_call_id.clone()))
            .ok_or_else(|| {
                format!(
                    "Pending delegation confirmation not found: operation_id={operation_id}, tool_call_id={}",
                    selection.tool_call_id
                )
            });
    }

    let matches = pending
        .iter()
        .filter(|pending| pending.tool_call_id == selection.tool_call_id)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [pending] => Ok((pending.operation_id.clone(), pending.tool_call_id.clone())),
        [] => Err(format!(
            "Pending delegation confirmation not found: tool_call_id={}",
            selection.tool_call_id
        )),
        _ => Err(format!(
            "Multiple pending delegation confirmations match tool_call_id={}; include the operation id.",
            selection.tool_call_id
        )),
    }
}

fn start_prompt_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    prompt: String,
    resource_invocation: Option<PromptInvocation>,
    prompt_context: &PromptContext,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<PromptTask, CliError> {
    let (operation, task_prompt) = match resource_invocation {
        Some(invocation) => (
            prompt_context.resource_prompt_operation(invocation),
            String::new(),
        ),
        None => {
            let prepared = prompt_context.prepare_prompt(&prompt)?;
            let task_prompt = prepared.display_text().to_string();
            (
                prompt_context.prepared_prompt_operation(prepared),
                task_prompt,
            )
        }
    };

    {
        let root = root_mut(tui, root_id)?;
        root.push_user(prompt.clone());
        root.set_status(InteractiveStatus::Running);
    }

    let bootstrap = prompt_context.session_bootstrap();
    let existing_session = coding_session.take();
    let task = PromptTask::spawn_prompt(operation, task_prompt, bootstrap, existing_session)?;
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(task)
}

fn start_agent_invocation_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    request: PendingAgentInvocationRequest,
    prompt_context: &PromptContext,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<PromptTask, CliError> {
    {
        let root = root_mut(tui, root_id)?;
        root.push_user(format!("/agent:{} {}", request.profile_id, request.task));
        root.set_status(InteractiveStatus::Running);
    }

    let operation = prompt_context.agent_invocation_operation(request.profile_id, request.task);
    let bootstrap = prompt_context.session_bootstrap();
    let task = PromptTask::spawn_agent_invocation(operation, bootstrap, coding_session.take())?;
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(task)
}

fn start_agent_team_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    request: PendingAgentTeamRequest,
    prompt_context: &PromptContext,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<PromptTask, CliError> {
    {
        let root = root_mut(tui, root_id)?;
        root.push_user(format!("/team:{} {}", request.team_id, request.task));
        root.set_status(InteractiveStatus::Running);
    }

    let operation = prompt_context.team_invocation_operation(request.team_id, request.task);
    let bootstrap = prompt_context.session_bootstrap();
    let task = PromptTask::spawn_agent_team(operation, bootstrap, coding_session.take())?;
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(task)
}

fn interactive_self_healing_model_repair_options(
    prompt_context: &PromptContext,
    max_attempts: usize,
) -> SelfHealingEditModelRepairOptions {
    prompt_context.model_repair_options(max_attempts)
}

fn start_self_healing_edit_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    request: PendingSelfHealingEditRequest,
    prompt_context: &PromptContext,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<PromptTask, CliError> {
    {
        let root = root_mut(tui, root_id)?;
        root.transcript.push(TranscriptItem::system(format!(
            "Applying self-healing edit: {}",
            request.path
        )));
        root.set_status(InteractiveStatus::Running);
    }

    let mut edit_request = SelfHealingEditRequest::new(request.path, request.replacements);
    if let Some(command) = request.check_command {
        edit_request = edit_request.with_check_command(command);
    }
    if let Some(model_repair) = request.model_repair {
        edit_request =
            edit_request.with_model_repair(interactive_self_healing_model_repair_options(
                prompt_context,
                model_repair.max_attempts,
            ));
    }
    let operation = prompt_context.self_healing_edit_operation(edit_request);
    let bootstrap = prompt_context.session_bootstrap();
    let task = PromptTask::spawn_self_healing_edit(operation, bootstrap, coding_session.take())?;
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(task)
}

fn start_compact_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    custom_instructions: Option<String>,
    prompt_context: &PromptContext,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<PromptTask, CliError> {
    let use_rust_native = {
        let root = root_mut(tui, root_id)?;
        matches!(
            root.active_session.as_ref().map(|choice| choice.kind),
            Some(SessionChoiceKind::Persistent)
        )
    };

    {
        let root = root_mut(tui, root_id)?;
        root.transcript
            .push(TranscriptItem::system("Compacting session..."));
        root.set_status(InteractiveStatus::Running);
    }

    if !use_rust_native {
        return Err(CliError::UnsupportedMode(
            "manual compaction requires an active Rust-native session".into(),
        ));
    }
    let operation = prompt_context.compact_operation(custom_instructions);
    let bootstrap = prompt_context.session_bootstrap();
    let task = PromptTask::spawn_compact(operation, bootstrap, coding_session.take())?;
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(task)
}

fn start_fork_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    request: PendingForkRequest,
    prompt_context: &PromptContext,
    running: &mut Option<PromptTask>,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<(), CliError> {
    if coding_session.is_none() {
        root_mut(tui, root_id)?
            .transcript
            .push(TranscriptItem::system("No active coding session."));
        return Ok(());
    }
    {
        let root = root_mut(tui, root_id)?;
        root.set_status(InteractiveStatus::Running);
    }
    let operation = prompt_context.fork_session_operation(request.target_leaf_id);
    let bootstrap = prompt_context.session_bootstrap();
    *running = Some(PromptTask::spawn_fork_session(
        operation,
        bootstrap,
        coding_session.take(),
        Some("Forked to new session".to_string()),
    )?);
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(())
}

fn start_tree_navigation_fork_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    target_leaf_id: String,
    prompt_context: &PromptContext,
    running: &mut Option<PromptTask>,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<(), CliError> {
    {
        let root = root_mut(tui, root_id)?;
        root.set_status(InteractiveStatus::Running);
    }
    let operation = prompt_context.fork_session_operation(Some(target_leaf_id));
    let bootstrap = prompt_context.session_bootstrap();
    *running = Some(PromptTask::spawn_fork_session(
        operation,
        bootstrap,
        coding_session.take(),
        Some("Navigated to selected point".to_string()),
    )?);
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(())
}

fn start_branch_summary_navigation_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    source_leaf_id: String,
    target_leaf_id: String,
    prompt_context: &PromptContext,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<PromptTask, CliError> {
    {
        let root = root_mut(tui, root_id)?;
        root.transcript.push(TranscriptItem::system(
            "Summarizing branch before navigation...",
        ));
        root.set_status(InteractiveStatus::Running);
    }

    let operation = prompt_context.branch_summary_operation(
        source_leaf_id,
        target_leaf_id.clone(),
        None,
        BranchSummaryReusePolicy::ReuseExisting,
    );
    let bootstrap = prompt_context.session_bootstrap();
    let task = PromptTask::spawn_branch_summary_navigation(
        operation,
        bootstrap,
        coding_session.take(),
        target_leaf_id,
    )?;
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(task)
}

fn start_branch_summary_task<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    request: PendingBranchSummaryRequest,
    prompt_context: &PromptContext,
    coding_session: &mut Option<CodingAgentSession>,
) -> Result<PromptTask, CliError> {
    let use_rust_native = {
        let root = root_mut(tui, root_id)?;
        matches!(
            root.active_session.as_ref().map(|choice| choice.kind),
            Some(SessionChoiceKind::Persistent)
        )
    };

    {
        let root = root_mut(tui, root_id)?;
        root.transcript
            .push(TranscriptItem::system("Summarizing branch..."));
        root.set_status(InteractiveStatus::Running);
    }

    if !use_rust_native {
        return Err(CliError::UnsupportedMode(
            "branch summary requires an active Rust-native session".into(),
        ));
    }
    let operation = prompt_context.branch_summary_operation(
        request.source_leaf_id,
        request.target_leaf_id,
        request.custom_instructions,
        BranchSummaryReusePolicy::AlwaysCreate,
    );
    let bootstrap = prompt_context.session_bootstrap();
    let task = PromptTask::spawn_branch_summary(operation, bootstrap, coding_session.take())?;
    if prompt_context.show_progress() {
        set_terminal_progress(tui, true)?;
    }
    Ok(task)
}

fn apply_prompt_connection_handoff<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    current: &mut Option<InteractiveClientConnection>,
    handoff: Result<
        Result<Option<CodingAgentClientConnection>, CliError>,
        tokio::sync::oneshot::error::RecvError,
    >,
) -> Result<RenderRequest, CliError> {
    match handoff {
        Ok(Ok(Some(connection))) => {
            detach_interactive_client(current);
            let (connection, render) = resume_interactive_client(tui, root_id, connection)?;
            *current = Some(connection);
            Ok(render)
        }
        Ok(Ok(None)) | Ok(Err(_)) | Err(_) => Ok(RenderRequest::NONE),
    }
}

fn apply_interactive_snapshot<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    snapshot: CodingAgentSnapshot,
) -> Result<RenderRequest, CliError> {
    apply_interactive_projection(tui, root_id, |root| {
        root.install_shared_snapshot(snapshot);
    })
}

fn apply_interactive_product_event<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    event: &ProductEvent,
) -> Result<RenderRequest, CliError> {
    apply_interactive_projection(tui, root_id, |root| {
        root.apply_shared_product_event(event);
    })
}

fn apply_interactive_projection<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    apply: impl FnOnce(&mut InteractiveRoot),
) -> Result<RenderRequest, CliError> {
    let root = root_mut(tui, root_id)?;
    let before = root.render_state();
    apply(root);
    let ui_events = root.drain_shared_ui_events();
    let force_render = ui_events.iter().any(ui_event_requires_immediate_render);
    root.apply_root_events(ui_events);
    root.apply_shared_child_ui_events();
    let after = root.render_state();
    let changed = before != after;
    Ok(if changed && force_render {
        RenderRequest::FORCE
    } else {
        RenderRequest::changed(changed)
    })
}

fn connect_interactive_client<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    session: &CodingAgentSession,
) -> Result<(InteractiveClientConnection, RenderRequest), CliError> {
    let connection = session
        .connect(CodingAgentClientId::new("interactive"))
        .map_err(CliError::from)?;
    resume_interactive_client(tui, root_id, connection)
}

fn resume_interactive_client<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    connection: CodingAgentClientConnection,
) -> Result<(InteractiveClientConnection, RenderRequest), CliError> {
    let snapshot = connection.snapshot.clone();
    resume_interactive_client_from_snapshot(tui, root_id, connection, snapshot)
}

fn resume_interactive_client_from_snapshot<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    connection: CodingAgentClientConnection,
    mut snapshot: CodingAgentSnapshot,
) -> Result<(InteractiveClientConnection, RenderRequest), CliError> {
    let mut render = RenderRequest::NONE;
    loop {
        let session_id = snapshot.session.session_id.clone();
        let requested_after = snapshot.cursor.last_event_sequence;
        connection
            .acknowledge(requested_after)
            .map_err(CliError::from)?;

        match connection
            .reconnect_from_cursor(&snapshot.cursor)
            .map_err(CliError::from)?
        {
            CodingAgentReconnect::Replayed {
                events, receiver, ..
            } => {
                render = render.merge(apply_interactive_snapshot(tui, root_id, snapshot)?);
                for event in events {
                    let sequence = event.sequence();
                    render = render.merge(apply_interactive_product_event(tui, root_id, &event)?);
                    connection.acknowledge(sequence).map_err(CliError::from)?;
                }
                return Ok((
                    InteractiveClientConnection {
                        connection,
                        receiver,
                        session_id,
                    },
                    render,
                ));
            }
            CodingAgentReconnect::FreshSnapshotRequired(recovery) => {
                snapshot = *recovery.snapshot;
            }
        }
    }
}

fn apply_interactive_client_delivery<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    connection: &mut Option<InteractiveClientConnection>,
    delivery: Result<CodingAgentReconnectDelivery, CodingAgentPublicError>,
) -> Result<RenderRequest, CliError> {
    match delivery {
        Ok(CodingAgentReconnectDelivery::Event(event)) => {
            let sequence = event.sequence();
            let render = apply_interactive_product_event(tui, root_id, &event)?;
            let Some(current_connection) = connection.as_ref() else {
                return Err(CliError::AgentFailure(
                    "interactive client receiver lost its owning connection".to_string(),
                ));
            };
            let acknowledgement = current_connection.connection.acknowledge(sequence);
            match acknowledgement {
                Ok(_) => Ok(render),
                Err(error) if is_terminal_client_error(&error) => {
                    detach_interactive_client(connection);
                    Ok(render)
                }
                Err(error) => Err(CliError::from(error)),
            }
        }
        Ok(CodingAgentReconnectDelivery::FreshSnapshotRequired(recovery)) => {
            let Some(current) = connection.take() else {
                return Err(CliError::AgentFailure(
                    "interactive client recovery lost its owning connection".to_string(),
                ));
            };
            let (resumed, render) = resume_interactive_client_from_snapshot(
                tui,
                root_id,
                current.connection,
                *recovery.snapshot,
            )?;
            *connection = Some(resumed);
            Ok(render)
        }
        Err(error) if is_terminal_client_error(&error) => {
            detach_interactive_client(connection);
            Ok(RenderRequest::NONE)
        }
        Err(error) => Err(CliError::from(error)),
    }
}

fn drain_interactive_client<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    connection: &mut Option<InteractiveClientConnection>,
) -> Result<RenderRequest, CliError> {
    let mut render = RenderRequest::NONE;
    loop {
        let delivery = match connection.as_mut() {
            Some(connection) => connection.receiver.try_recv(),
            None => return Ok(render),
        };
        match delivery {
            Ok(Some(delivery)) => {
                render = render.merge(apply_interactive_client_delivery(
                    tui,
                    root_id,
                    connection,
                    Ok(delivery),
                )?);
            }
            Ok(None) => return Ok(render),
            Err(error) if is_terminal_client_error(&error) => {
                detach_interactive_client(connection);
                return Ok(render);
            }
            Err(error) => return Err(CliError::from(error)),
        }
    }
}

fn is_terminal_client_error(error: &CodingAgentPublicError) -> bool {
    matches!(error.code(), "cancelled" | "stale_generation")
}

fn ui_event_requires_immediate_render(event: &UiEvent) -> bool {
    matches!(
        event,
        UiEvent::ToolAuthorizationRequired { .. } | UiEvent::DelegationConfirmationRequired { .. }
    )
}

fn finish_prompt<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    result: PromptTaskCompletion,
    coding_session: &mut Option<CodingAgentSession>,
    session_bootstrap: &mut CodingAgentSessionBootstrap,
) -> Result<(), CliError> {
    set_terminal_progress(tui, false)?;
    let root = root_mut(tui, root_id)?;
    match result {
        PromptTaskCompletion::Completed(PromptTaskResult::Coding(result)) => {
            if let Some(session_id) = result.replacement_session_id.clone() {
                *session_bootstrap = session_bootstrap.clone().with_session_id(session_id);
            }
            let completion_notice = result.completion_notice.clone();
            if result.hydrate_transcript {
                if let Ok(Some(hydration)) = result.session.current_session_snapshot() {
                    root.apply_hydrated_session(
                        hydrated_session_from_snapshot(hydration),
                        completion_notice,
                    );
                } else {
                    finish_coding_prompt(root, &result.session, result.outcome)?;
                    if let Some(notice) = completion_notice {
                        root.transcript.push(TranscriptItem::system(notice));
                    }
                }
            } else {
                finish_coding_prompt(root, &result.session, result.outcome)?;
                if let Some(notice) = completion_notice {
                    root.transcript.push(TranscriptItem::system(notice));
                }
            }
            *coding_session = Some(result.session);
        }
        PromptTaskCompletion::Completed(PromptTaskResult::AgentInvocation(result)) => {
            root.set_default_agent_profile_id(
                result.session.view()?.default_agent_profile_id.clone(),
            );
            if let Ok(Some(hydration)) = result.session.current_session_snapshot() {
                let hydrated = hydrated_session_from_snapshot(hydration);
                let mut choice = hydrated.choice;
                if choice.active_leaf_id.is_none() {
                    choice.active_leaf_id = root.active_leaf_id.clone();
                }
                root.set_active_session_choice(choice);
            }
            *coding_session = Some(result.session);
        }
        PromptTaskCompletion::Completed(PromptTaskResult::AgentTeam(result)) => {
            let _final_text = &result.outcome.final_text;
            root.set_default_agent_profile_id(
                result.session.view()?.default_agent_profile_id.clone(),
            );
            if let Ok(Some(hydration)) = result.session.current_session_snapshot() {
                let hydrated = hydrated_session_from_snapshot(hydration);
                let mut choice = hydrated.choice;
                if choice.active_leaf_id.is_none() {
                    choice.active_leaf_id = root.active_leaf_id.clone();
                }
                root.set_active_session_choice(choice);
            }
            *coding_session = Some(result.session);
        }
        PromptTaskCompletion::Completed(PromptTaskResult::DelegationApproval(result)) => {
            root.set_default_agent_profile_id(
                result.session.view()?.default_agent_profile_id.clone(),
            );
            if let Ok(Some(hydration)) = result.session.current_session_snapshot() {
                let hydrated = hydrated_session_from_snapshot(hydration);
                let mut choice = hydrated.choice;
                if choice.active_leaf_id.is_none() {
                    choice.active_leaf_id = root.active_leaf_id.clone();
                }
                root.set_active_session_choice(choice);
            }
            *coding_session = Some(result.session);
        }
        PromptTaskCompletion::Completed(PromptTaskResult::SessionTreeLabel(result)) => {
            let notice = match result.label.as_deref() {
                Some(label) => format!("Tree label updated: {label}"),
                None => "Tree label cleared".to_string(),
            };
            root.apply_tree_label_update(&result.entry_id, result.label, result.updated_at);
            root.transcript.push(TranscriptItem::system(notice));
            *coding_session = Some(result.session);
        }
        PromptTaskCompletion::Completed(PromptTaskResult::DelegationRejection(result)) => {
            root.set_default_agent_profile_id(
                result.session.view()?.default_agent_profile_id.clone(),
            );
            if let Ok(Some(hydration)) = result.session.current_session_snapshot() {
                let hydrated = hydrated_session_from_snapshot(hydration);
                let mut choice = hydrated.choice;
                if choice.active_leaf_id.is_none() {
                    choice.active_leaf_id = root.active_leaf_id.clone();
                }
                root.set_active_session_choice(choice);
            }
            *coding_session = Some(result.session);
        }
        PromptTaskCompletion::Completed(PromptTaskResult::SelfHealingEdit(result)) => {
            root.transcript
                .push(TranscriptItem::system(result.outcome.message.clone()));
            for diagnostic in &result.outcome.diagnostics {
                root.transcript
                    .push(TranscriptItem::system(diagnostic.message.clone()));
            }
            root.set_default_agent_profile_id(
                result.session.view()?.default_agent_profile_id.clone(),
            );
            if let Ok(Some(hydration)) = result.session.current_session_snapshot() {
                let hydrated = hydrated_session_from_snapshot(hydration);
                let mut choice = hydrated.choice;
                if choice.active_leaf_id.is_none() {
                    choice.active_leaf_id = root.active_leaf_id.clone();
                }
                root.set_active_session_choice(choice);
            }
            *coding_session = Some(result.session);
        }
        PromptTaskCompletion::Completed(PromptTaskResult::ForkSession(result)) => {
            *session_bootstrap = session_bootstrap
                .clone()
                .with_session_id(result.replacement_session_id.clone());
            let completion_notice = result.completion_notice.clone();
            if result.hydrate_transcript {
                if let Ok(Some(hydration)) = result.session.current_session_snapshot() {
                    root.apply_hydrated_session(
                        hydrated_session_from_snapshot(hydration),
                        completion_notice,
                    );
                } else if let Some(notice) = completion_notice {
                    root.transcript.push(TranscriptItem::system(notice));
                }
            } else if let Some(notice) = completion_notice {
                root.transcript.push(TranscriptItem::system(notice));
            }
            root.set_default_agent_profile_id(
                result.session.view()?.default_agent_profile_id.clone(),
            );
            *coding_session = Some(result.session);
        }
        PromptTaskCompletion::Failed(PromptTaskFailure { session, error }) => {
            *coding_session = Some(session);
            root.apply_events(vec![UiEvent::AgentError {
                error: public_cli_error_message(&error),
            }]);
        }
        PromptTaskCompletion::SetupFailed(error) => {
            root.apply_events(vec![UiEvent::AgentError {
                error: public_cli_error_message(&error),
            }]);
        }
    }
    root.set_status(InteractiveStatus::Idle);
    Ok(())
}

#[cfg(test)]
#[path = "loop_tests.rs"]
mod tests;
fn finish_coding_prompt(
    root: &mut InteractiveRoot,
    session: &CodingAgentSession,
    outcome: PromptTurnOutcome,
) -> Result<(), CliError> {
    root.set_default_agent_profile_id(session.view()?.default_agent_profile_id.clone());
    root.clear_active_session();
    match outcome {
        PromptTurnOutcome::Success {
            session_id,
            leaf_id,
            ..
        } => {
            if let Some(session_id) = session_id {
                root.session_label = session_id;
                root.active_leaf_id = leaf_id;
            }
        }
        PromptTurnOutcome::Aborted { session_id, .. } => {
            if let Some(session_id) = session_id {
                root.session_label = session_id;
            }
        }
        PromptTurnOutcome::Failed { .. } => {}
    }
    if let Ok(Some(hydration)) = session.current_session_snapshot() {
        let hydrated = hydrated_session_from_snapshot(hydration);
        let mut choice = hydrated.choice;
        if choice.active_leaf_id.is_none() {
            choice.active_leaf_id = root.active_leaf_id.clone();
        }
        root.set_active_session_choice(choice);
    }
    Ok(())
}

fn set_terminal_progress<T: Terminal>(tui: &mut Tui<T>, active: bool) -> Result<(), CliError> {
    tui.terminal_mut()
        .set_progress(active)
        .map_err(to_cli_error)
}

fn render_tui<T: Terminal>(tui: &mut Tui<T>) -> Result<(), CliError> {
    tui.render_once().map(drop).map_err(tui_error)
}

fn root_mut<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
) -> Result<&mut InteractiveRoot, CliError> {
    tui.component_as_mut::<InteractiveRoot>(root_id)
        .ok_or_else(|| CliError::AgentFailure("interactive root component missing".to_string()))
}

fn root_ref<T: Terminal>(tui: &Tui<T>, root_id: usize) -> Result<&InteractiveRoot, CliError> {
    tui.component_as::<InteractiveRoot>(root_id)
        .ok_or_else(|| CliError::AgentFailure("interactive root component missing".to_string()))
}

fn tui_error(error: TuiError) -> CliError {
    CliError::AgentFailure(error.to_string())
}

/// Apply a hot-reloaded theme to the root component, mirroring TS
/// `setGlobalTheme(reloadedTheme)` + `onThemeChange` (UI invalidate).
fn apply_theme_reload<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    reload: CodingAgentThemeSnapshot,
) {
    if let Some(root) = tui.component_as_mut::<InteractiveRoot>(root_id) {
        root.apply_theme_snapshot(reload);
    }
}

fn to_cli_error(error: std::io::Error) -> CliError {
    CliError::AgentFailure(error.to_string())
}

fn public_cli_error_message(error: &CliError) -> String {
    match error {
        CliError::Product(error) => error.summary.clone(),
        _ => error.to_string(),
    }
}
