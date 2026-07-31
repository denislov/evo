use super::{
    Arc, ClipboardFeedback, Context, ConversationFullMessageView, DesktopModalKind, MAX_COPY_BYTES,
    NativeShell, ScrollStrategy, UiChangeSet, UiRegion, Window, adjacent_conversation_index,
    conversation_copy_text, conversation_pane, message_conversation_block_id,
    tool_conversation_block_id,
};

impl NativeShell {
    pub(super) fn copy_selected_conversation(&mut self, cx: &mut Context<Self>) {
        let workspace = self.app.workspaces.active_mut();
        let Some(projection) = workspace.projection.as_ref() else {
            return;
        };
        let Some(text) = workspace
            .presentation
            .conversation_controller
            .copy_selected(projection.conversation())
        else {
            self.app.workspaces.active_mut().set_preference_notice(
                "Select a committed conversation block before copying.".into(),
            );
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        self.write_clipboard(
            Some(text),
            ClipboardFeedback::ConversationAnnouncement("Selected message copied.".into()),
            cx,
        );
    }

    pub(super) fn conversation_full_message_view(
        &self,
        block_id: &str,
    ) -> Option<ConversationFullMessageView> {
        let projection = self.app.workspaces.active().projection.as_ref()?;
        if let Some(block) = projection.conversation().block(block_id) {
            return Some(ConversationFullMessageView {
                block_id: block_id.to_owned(),
                title: Arc::from(block.title.as_str()),
                text: Arc::from(block.copy_text()),
                source_truncated: block.truncated
                    || block.text.len().saturating_add(block.detail.len()) > MAX_COPY_BYTES,
            });
        }
        if let Some(message) = self
            .app
            .workspaces
            .active()
            .projection
            .as_ref()?
            .messages()
            .iter()
            .find(|message| message_conversation_block_id(message) == block_id)
        {
            return Some(ConversationFullMessageView {
                block_id: block_id.to_owned(),
                title: Arc::from("Assistant · live"),
                text: Arc::from(conversation_copy_text(&message.text, &message.thinking)),
                source_truncated: message.truncated
                    || message.text.len().saturating_add(message.thinking.len()) > MAX_COPY_BYTES,
            });
        }
        if let Some(tool) = projection
            .tools()
            .iter()
            .find(|tool| tool_conversation_block_id(tool) == block_id)
        {
            return Some(ConversationFullMessageView {
                block_id: block_id.to_owned(),
                title: Arc::from(format!("Tool · {}", tool.name)),
                text: Arc::from(conversation_copy_text(&tool.detail, &tool.arguments)),
                source_truncated: tool.truncated
                    || tool.detail.len().saturating_add(tool.arguments.len()) > MAX_COPY_BYTES,
            });
        }
        self.app
            .workspaces
            .active()
            .presentation
            .conversation_controller
            .row_for_block(block_id)
            .map(|row| ConversationFullMessageView {
                block_id: block_id.to_owned(),
                title: Arc::clone(&row.title),
                text: Arc::from(conversation_copy_text(&row.text, &row.detail)),
                source_truncated: row.preview_truncated,
            })
    }

    pub(super) fn copy_conversation_row(&mut self, block_id: &str, cx: &mut Context<Self>) {
        let Some(message) = self.conversation_full_message_view(block_id) else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("Message is no longer available to copy.".into());
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            return;
        };
        self.write_clipboard(
            Some(message.text.to_string()),
            ClipboardFeedback::ConversationAnnouncement("Message copied.".into()),
            cx,
        );
    }

    pub(super) fn copy_tool_details(&mut self, block_id: &str, cx: &mut Context<Self>) {
        let Some(row) = self
            .app
            .workspaces
            .active_mut()
            .presentation
            .conversation_controller
            .row_for_block(block_id)
        else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("Tool details are no longer available to copy.".into());
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            return;
        };
        self.write_clipboard(
            Some(conversation_pane::tool_detail_copy_text(
                &row.title,
                &row.detail,
                &row.text,
            )),
            ClipboardFeedback::ConversationAnnouncement("Tool details copied.".into()),
            cx,
        );
    }

    pub(super) fn announce_conversation_copy(&mut self, message: &str, cx: &mut Context<Self>) {
        self.write_clipboard(
            None,
            ClipboardFeedback::ConversationAnnouncement(message.into()),
            cx,
        );
    }

    pub(super) fn write_clipboard(
        &mut self,
        text: Option<String>,
        feedback: ClipboardFeedback,
        cx: &mut Context<Self>,
    ) {
        let owner = self.app.workspaces.active_key().clone();
        match self
            .connection
            .controller
            .write_clipboard(owner, text, feedback)
        {
            Ok(transition) => self.apply_transition(transition, cx),
            Err(error) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error.to_string());
                self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            }
        }
    }

    pub(super) fn open_full_conversation_message(
        &mut self,
        block_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(message) = self.conversation_full_message_view(block_id) else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("Message is no longer available to open.".into());
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            return;
        };
        tracing::trace!(
            target: "desktop",
            event = "message_full_view_open",
            block_id = message.block_id,
            bytes = message.text.len(),
        );
        self.ui.conversation_full_message = Some(message);
        self.activate_modal(DesktopModalKind::FullMessage, window, cx);
    }

    pub(super) fn close_full_conversation_message(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ui.conversation_full_message = None;
        self.dismiss_modal(window, cx);
    }

    pub(super) fn toggle_conversation_details(&mut self, block_id: &str, cx: &mut Context<Self>) {
        self.app
            .workspaces
            .active_mut()
            .presentation
            .conversation_controller
            .toggle_details(block_id);
        if !self.refresh_conversation_rows_at_current_width(cx) {
            cx.notify();
        }
    }

    pub(in crate::app) fn select_adjacent_conversation(
        &mut self,
        reverse: bool,
        cx: &mut Context<Self>,
    ) {
        let workspace = &mut self.app.workspaces.active_mut();
        let Some(projection) = workspace.projection.as_ref() else {
            return;
        };
        let row_count = workspace.presentation.conversation_controller.row_count();
        if row_count == 0 {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("The conversation is empty.".into());
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            return;
        }
        let current_index = workspace
            .presentation
            .conversation_controller
            .selected_block_id()
            .map(str::to_owned)
            .and_then(|selected| {
                workspace
                    .presentation
                    .conversation_controller
                    .row_index(&selected)
            });
        let next_index = adjacent_conversation_index(row_count, current_index, reverse)
            .expect("non-empty conversation has an adjacent selection");
        let row = workspace
            .presentation
            .conversation_controller
            .row_at(next_index)
            .expect("adjacent index is inside the rendered rows");
        workspace.presentation.conversation_controller.select_row(
            row.item_key.row_id().to_owned(),
            row.durable,
            projection.conversation(),
        );
        workspace
            .presentation
            .conversation_controller
            .scroll_to_row(
                next_index,
                if reverse {
                    ScrollStrategy::Top
                } else {
                    ScrollStrategy::Bottom
                },
            );
        self.refresh_views(UiChangeSet::one(UiRegion::Conversation), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
    }

    pub(super) fn copy_keyboard_selected_conversation(&mut self, cx: &mut Context<Self>) {
        let Some(block_id) = self
            .app
            .workspaces
            .active_mut()
            .presentation
            .conversation_controller
            .selected_block_id()
            .map(str::to_owned)
        else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("Select a conversation message before copying.".into());
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            return;
        };
        self.copy_conversation_row(&block_id, cx);
    }

    pub(super) fn toggle_keyboard_selected_conversation_details(&mut self, cx: &mut Context<Self>) {
        let Some(block_id) = self
            .app
            .workspaces
            .active_mut()
            .presentation
            .conversation_controller
            .selected_block_id()
            .map(str::to_owned)
        else {
            return;
        };
        let has_details = self
            .app
            .workspaces
            .active_mut()
            .presentation
            .conversation_controller
            .row_for_block(&block_id)
            .is_some_and(|row| !row.detail.is_empty());
        if has_details {
            self.toggle_conversation_details(&block_id, cx);
        }
    }
}
