use tui::api::render::Tui;
use tui::api::terminal::Terminal;

use crate::interactive::error::CliError;
use crate::interactive::root::InteractiveRoot;
use crate::interactive::{TranscriptItem, UiEvent};
use coding_agent::api::client::{
    CodingAgentClientConnection, CodingAgentClientId, CodingAgentReconnect,
    CodingAgentReconnectDelivery, CodingAgentSnapshot,
};
use coding_agent::api::error::CodingAgentPublicError;
use coding_agent::api::event::CodingAgentProductEvent as ProductEvent;
use coding_agent::api::runtime::CodingAgentSession;

use super::{InteractiveClientConnection, RenderRequest, detach_interactive_client, root_mut};

pub(super) fn apply_prompt_connection_handoff<T: Terminal>(
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

/// Apply a permission-mode switch requested through `/permission` to the live
/// runtime connection. Without a connection (before the first prompt finishes)
/// the pending mode stays queued on the root and applies once connected.
pub(super) fn apply_pending_permission_mode<T: Terminal>(
    tui: &mut Tui<T>,
    root_id: usize,
    connection: &mut Option<InteractiveClientConnection>,
) -> Result<RenderRequest, CliError> {
    let Some(current) = connection.as_ref() else {
        return Ok(RenderRequest::NONE);
    };
    let Some(mode) = root_mut(tui, root_id)?.take_pending_permission_mode() else {
        return Ok(RenderRequest::NONE);
    };
    apply_interactive_projection(tui, root_id, |root| {
        match current.connection.set_tool_authorization_mode(mode) {
            Ok(()) => {
                root.transcript.push(TranscriptItem::system(format!(
                    "Permission mode set: {mode}"
                )));
            }
            Err(error) => {
                root.transcript.push(TranscriptItem::system(format!(
                    "Failed to set permission mode: {error}"
                )));
            }
        }
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

pub(super) fn connect_interactive_client<T: Terminal>(
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

pub(super) fn apply_interactive_client_delivery<T: Terminal>(
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

pub(super) fn drain_interactive_client<T: Terminal>(
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
