use std::io;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;

use tui::api::component::Component;
use tui::api::input::{InputEvent, matches_key};

use crate::interactive::root::{
    InteractiveAction, InteractiveRoot, InteractiveStatus, PendingInteractiveCommand,
};
use crate::interactive::slash::parse_slash_command;
use crate::interactive::{TranscriptItem, root::TranscriptScrollCommand};

pub(super) struct InputPump {
    rx: tokio::sync::mpsc::Receiver<String>,
    consumed_tx: Option<tokio::sync::mpsc::Sender<String>>,
    idle_tx: Option<tokio::sync::mpsc::Sender<u64>>,
    processed_generation: AtomicU64,
    cancel: Arc<AtomicBool>,
    reader: Option<std::thread::JoinHandle<io::Result<()>>>,
}

const INPUT_CHANNEL_CAPACITY: usize = 32;
const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(25);

impl InputPump {
    pub(super) fn from_stdin() -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(INPUT_CHANNEL_CAPACITY);
        let cancel = Arc::new(AtomicBool::new(false));
        let reader_cancel = Arc::clone(&cancel);
        let reader = std::thread::spawn(move || read_stdin(reader_cancel, tx));
        Self {
            rx,
            consumed_tx: None,
            idle_tx: None,
            processed_generation: AtomicU64::new(0),
            cancel,
            reader: Some(reader),
        }
    }

    #[cfg(test)]
    pub(super) fn from_receiver(rx: tokio::sync::mpsc::Receiver<String>) -> Self {
        Self {
            rx,
            consumed_tx: None,
            idle_tx: None,
            processed_generation: AtomicU64::new(0),
            cancel: Arc::new(AtomicBool::new(false)),
            reader: None,
        }
    }

    #[cfg(test)]
    pub(super) fn cancellable_reader_for_tests() -> (Self, std::sync::mpsc::Receiver<()>) {
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let reader_cancel = Arc::clone(&cancel);
        let (exited_tx, exited_rx) = std::sync::mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            while !reader_cancel.load(Ordering::Acquire) {
                std::thread::park_timeout(INPUT_POLL_INTERVAL);
            }
            let _ = exited_tx.send(());
            Ok(())
        });
        (
            Self {
                rx,
                consumed_tx: None,
                idle_tx: None,
                processed_generation: AtomicU64::new(0),
                cancel,
                reader: Some(reader),
            },
            exited_rx,
        )
    }

    pub(super) async fn recv(&mut self) -> Option<String> {
        self.rx.recv().await
    }

    pub(super) async fn shutdown(&mut self) -> io::Result<()> {
        self.cancel.store(true, Ordering::Release);
        self.rx.close();
        let Some(reader) = self.reader.take() else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || reader.join())
            .await
            .map_err(|error| io::Error::other(format!("stdin join task failed: {error}")))?
            .map_err(|_| io::Error::other("stdin reader thread panicked"))?
    }

    pub(super) fn mark_processed(&self, chunk: &str) {
        if let Some(consumed_tx) = &self.consumed_tx
            && let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) =
                consumed_tx.try_send(chunk.to_string())
        {
            panic!("scripted consumed observer exceeded its bounded handshake window");
        }
        self.processed_generation.fetch_add(1, Ordering::Release);
    }

    pub(super) fn mark_idle(&self) {
        if let Some(idle_tx) = &self.idle_tx
            && let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) =
                idle_tx.try_send(self.processed_generation.load(Ordering::Acquire))
        {
            panic!("scripted idle observer exceeded its bounded handshake window");
        }
    }
}

impl Drop for InputPump {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.rx.close();
    }
}

fn read_stdin(cancel: Arc<AtomicBool>, tx: tokio::sync::mpsc::Sender<String>) -> io::Result<()> {
    let mut buffer = [0_u8; 1024];
    while !cancel.load(Ordering::Acquire) {
        if !stdin_ready(INPUT_POLL_INTERVAL)? {
            continue;
        }
        let count = read_stdin_bytes(&mut buffer)?;
        if count == 0 {
            break;
        }
        let mut chunk = String::from_utf8_lossy(&buffer[..count]).to_string();
        loop {
            match tx.try_send(chunk) {
                Ok(()) => break,
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return Ok(()),
                Err(tokio::sync::mpsc::error::TrySendError::Full(returned)) => {
                    chunk = returned;
                    if cancel.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    std::thread::sleep(INPUT_POLL_INTERVAL);
                }
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn stdin_ready(timeout: Duration) -> io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout_ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    loop {
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if result >= 0 {
            return Ok(result > 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(unix)]
fn read_stdin_bytes(buffer: &mut [u8]) -> io::Result<usize> {
    loop {
        let result =
            unsafe { libc::read(libc::STDIN_FILENO, buffer.as_mut_ptr().cast(), buffer.len()) };
        if result >= 0 {
            return Ok(result as usize);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(windows)]
fn stdin_ready(timeout: Duration) -> io::Result<bool> {
    type Handle = *mut std::ffi::c_void;
    const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 258;
    unsafe extern "system" {
        fn GetStdHandle(kind: u32) -> Handle;
        fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    }

    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle.is_null() || handle as isize == -1 {
        return Err(io::Error::last_os_error());
    }
    let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
    match unsafe { WaitForSingleObject(handle, timeout_ms) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}

#[cfg(windows)]
fn read_stdin_bytes(buffer: &mut [u8]) -> io::Result<usize> {
    use std::io::Read;

    std::io::stdin().read(buffer)
}

pub(super) fn handle_root_input(root: &mut InteractiveRoot, event: &InputEvent) {
    // Tree selector has highest priority when active
    if root.local.selecting_tree {
        if matches_key(event, "ctrl+c") {
            root.local.selecting_tree = false;
            root.local.tree_selector = None;
            root.local.selected_tree_entry_id = None;
            root.local.editor.set_text("");
            return;
        }
        root.handle_tree_selection_input(event);
        return;
    }

    if root.has_pending_tool_authorization() && root.handle_tool_authorization_input(event) {
        return;
    }

    if root.status == InteractiveStatus::Idle
        && root.has_active_delegation_confirmation_menu()
        && root.handle_delegation_confirmation_menu_input(event)
    {
        return;
    }

    if root.status == InteractiveStatus::Idle
        && root.has_active_profile_menu()
        && root.handle_profile_menu_input(event)
    {
        return;
    }

    if root.status == InteractiveStatus::Idle
        && root.has_pending_delegation_rejection_reason()
        && root.handle_pending_delegation_rejection_reason_input(event)
    {
        return;
    }

    if root.status == InteractiveStatus::Idle
        && root.has_pending_profile_task()
        && root.handle_pending_profile_task_input(event)
    {
        return;
    }

    if root.has_context_detail() && root.handle_context_detail_input(event) {
        return;
    }

    if root.handle_shell_input(event) {
        return;
    }

    if matches_key(event, "ctrl+c") || matches_key(event, "escape") {
        match root.status {
            InteractiveStatus::Running => {
                root.action = InteractiveAction::AbortRunning;
                return;
            }
            InteractiveStatus::Idle => {
                if matches_key(event, "ctrl+c") {
                    if root.local.editor.text().is_empty() {
                        root.action = InteractiveAction::Exit;
                    } else {
                        root.local.editor.set_text("");
                    }
                    return;
                }
            }
        }
    }

    if matches_key(event, "ctrl+o") {
        if root.uses_per_block_transcript_view() {
            root.toggle_all_transcript_blocks();
        } else {
            root.tool_output_expanded = !root.tool_output_expanded;
        }
        return;
    }

    if root.status == InteractiveStatus::Running {
        if matches_key(event, "shift+enter") {
            let text = root.local.editor.expanded_text().trim().to_string();
            root.local.editor.set_text("");
            if !text.is_empty() {
                root.local.editor.add_to_history(&text);
                root.queue_command(PendingInteractiveCommand::FollowUp(text));
            }
            return;
        }

        let before_text = root.local.editor.text().to_string();
        root.local.editor.handle_input(event);
        if root.local.editor.text() != before_text {
            root.slash_suggestion_selected = 0;
            root.slash_suggestions_dismissed_for = None;
        }
        if let Some(command) = root.take_scroll_command() {
            let page_rows = root.viewport_height.saturating_sub(2).max(1);
            match command {
                TranscriptScrollCommand::PageUp => root.transcript.scroll_page_up(page_rows),
                TranscriptScrollCommand::PageDown => root.transcript.scroll_page_down(page_rows),
            }
        }
        if let Some(text) = root.take_submitted() {
            root.local.editor.add_to_history(&text);
            root.queue_command(PendingInteractiveCommand::Submit(text));
        }
        return;
    }

    if root.status == InteractiveStatus::Idle
        && !root.local.selecting_model
        && !root.local.selecting_session
        && !root.local.selecting_settings
    {
        if root.local.keybindings.matches(event, "app.model.next") {
            root.cycle_model_rotation(false);
            return;
        }
        if root.local.keybindings.matches(event, "app.model.previous") {
            root.cycle_model_rotation(true);
            return;
        }
    }

    if root.local.selecting_model && matches_key(event, "escape") {
        root.local.selecting_model = false;
        root.local.editor.set_text("");
        root.transcript.push(TranscriptItem::system(
            "Model selection canceled".to_string(),
        ));
        return;
    }

    if root.local.selecting_session && matches_key(event, "escape") {
        root.local.selecting_session = false;
        root.local.editor.set_text("");
        root.transcript.push(TranscriptItem::system(
            "Session selection canceled".to_string(),
        ));
        return;
    }

    if root.local.selecting_settings && matches_key(event, "escape") {
        root.local.selecting_settings = false;
        root.local.editor.set_text("");
        return;
    }

    if root.status == InteractiveStatus::Idle {
        let before_text = root.local.editor.text().to_string();
        if root.handle_slash_suggestion_input(event) {
            root.clear_empty_editor_escape();
            return;
        }
        if !root.local.selecting_model
            && !root.local.selecting_session
            && !root.local.selecting_settings
            && matches_key(event, "escape")
        {
            if root.local.editor.text().trim().is_empty() {
                root.handle_empty_editor_escape();
            } else {
                root.clear_empty_editor_escape();
            }
            return;
        }
        if !matches_key(event, "escape") {
            root.clear_empty_editor_escape();
        }
        if root.local.selecting_model {
            root.handle_model_selection_input(event);
            return;
        }
        if root.local.selecting_session {
            root.handle_session_selection_input(event);
            return;
        }
        if root.local.selecting_settings {
            root.handle_settings_input(event);
            return;
        }
        root.local.editor.handle_input(event);
        if root.local.editor.text() != before_text {
            root.slash_suggestion_selected = 0;
            root.slash_suggestions_dismissed_for = None;
        }
        if let Some(command) = root.take_scroll_command() {
            let page_rows = root.viewport_height.saturating_sub(2).max(1);
            match command {
                TranscriptScrollCommand::PageUp => root.transcript.scroll_page_up(page_rows),
                TranscriptScrollCommand::PageDown => root.transcript.scroll_page_down(page_rows),
            }
        }
        if let Some(text) = root.take_submitted() {
            if let Some(command) = parse_slash_command(&text) {
                root.handle_slash_command(command);
            } else {
                root.local.editor.add_to_history(&text);
                root.queue_command(PendingInteractiveCommand::Submit(text));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_channel_applies_bounded_pressure() {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let _pump = InputPump::from_receiver(rx);

        tx.try_send("first".to_string()).unwrap();
        assert!(matches!(
            tx.try_send("second".to_string()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(chunk)) if chunk == "second"
        ));
    }

    #[tokio::test]
    async fn shutdown_cancels_and_joins_reader() {
        let (mut pump, exited) = InputPump::cancellable_reader_for_tests();

        pump.shutdown().await.unwrap();

        exited.try_recv().unwrap();
        assert!(pump.reader.is_none());
        assert!(pump.cancel.load(Ordering::Acquire));
    }
}
