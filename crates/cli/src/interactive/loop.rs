use std::time::{Duration, Instant};

use tui::api::component::{Component, OverlayAnchor, OverlayMargin, OverlayOptions, SizeValue};
use tui::api::input::{InputEvent, StdinBuffer};
use tui::api::render::{RenderScheduler, Tui, TuiError};
use tui::api::terminal::{Terminal, TerminalSize, detect_terminal_capabilities_from_env};

use crate::interactive::TranscriptItem;
use crate::interactive::app::{PromptContext, session_label};
use crate::interactive::error::CliError;
use crate::interactive::input::InputPump;
use crate::interactive::prompt_task::{PromptTask, PromptTaskCompletion};
#[cfg(test)]
use crate::interactive::root::InteractiveStatus;
use crate::interactive::root::{InteractiveRoot, TransientOverlayRole};
use crate::interactive::session_actions::hydrate_existing_session_target;
use coding_agent::api::client::{
    CodingAgentClientConnection, CodingAgentReconnectDelivery, CodingAgentReconnectReceiver,
};
use coding_agent::api::embedding::CodingAgentInteractiveStartup;
use coding_agent::api::error::CodingAgentPublicError;
use coding_agent::api::runtime::CodingAgentSession;
use coding_agent::api::settings::CodingAgentThemeSnapshot;

mod bootstrap;
mod client;
mod completion;
mod effects;
mod input;

use bootstrap::{ResizeSource, print_exit_resume_hint, print_startup_banner};
use client::{
    apply_interactive_client_delivery, apply_pending_permission_mode,
    apply_prompt_connection_handoff, connect_interactive_client, drain_interactive_client,
};
use completion::finish_prompt;
use input::handle_input_event;

#[cfg(test)]
#[path = "loop_tests.rs"]
mod tests;

const NORMAL_RENDER_INTERVAL: Duration = Duration::from_millis(16);
const SPINNER_INTERVAL: Duration = Duration::from_millis(120);
const RESIZE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const SHUTDOWN_DRAIN_MAX: Duration = Duration::from_millis(1000);
const SHUTDOWN_DRAIN_IDLE: Duration = Duration::from_millis(50);

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
    UpdateNotice(Option<String>),
}

async fn receive_update_notice(
    update_check: &mut Option<tokio::task::JoinHandle<Option<String>>>,
) -> Option<String> {
    match update_check.as_mut() {
        Some(check) => check.await.ok().flatten(),
        None => std::future::pending().await,
    }
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
    update_check: Option<tokio::task::JoinHandle<Option<String>>>,
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
    let loop_result = run_started_interactive_loop(
        &mut tui,
        root_id,
        &mut input,
        prompt_context,
        update_check,
        &clock,
    )
    .await;
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
        root.permission_mode = prompt_context.permission_mode;
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
    mut update_check: Option<tokio::task::JoinHandle<Option<String>>>,
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
        schedule_render(
            &mut render_scheduler,
            apply_pending_permission_mode(tui, root_id, &mut client_connection)?,
        );
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
            notice = receive_update_notice(&mut update_check), if update_check.is_some() => {
                InteractiveLoopEvent::UpdateNotice(notice)
            }
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
            InteractiveLoopEvent::UpdateNotice(notice) => {
                update_check = None;
                if let Some(notice) = notice {
                    root_mut(tui, root_id)?
                        .transcript
                        .push(TranscriptItem::system(notice));
                    render_scheduler.request(true);
                }
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
