use super::{
    Arc, ClipboardFeedback, CodingAgentFileReviewRequest, Context, DesktopCommandIntent,
    DesktopFileReviewState, NativeShell, UiChangeSet, UiRegion, truncate_label,
};

impl NativeShell {
    pub(super) fn request_file_review(
        &mut self,
        request: CodingAgentFileReviewRequest,
        cx: &mut Context<Self>,
    ) {
        let intent = DesktopCommandIntent::FileReview {
            request: request.clone(),
        };
        if self.active_command_contains_where(|pending| {
            matches!(pending, DesktopCommandIntent::FileReview { .. })
        }) {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("Another file review is already pending.".into());
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        }
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        let Some(session_id) = self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .map(|projection| projection.snapshot().session.session_id.clone())
        else {
            self.complete_active_command(command_id, &intent);
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("File review requires an open session.".into());
            self.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
            cx.notify();
            return;
        };
        let admission = self
            .connection
            .runtime_client
            .as_ref()
            .ok_or_else(|| "desktop runtime is unavailable".to_owned())
            .and_then(|runtime| {
                runtime
                    .try_review_changed_file(command_id, &session_id, &request)
                    .map_err(|error| error.to_string())
            });
        match admission {
            Ok(()) => {
                self.app.workspaces.active_mut().file_review =
                    Arc::new(DesktopFileReviewState::Loading(request));
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice("Loading changed-file review…".into());
            }
            Err(message) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
        cx.notify();
    }

    pub(super) fn copy_review_path(&mut self, cx: &mut Context<Self>) {
        let DesktopFileReviewState::Ready(document) =
            self.app.workspaces.active_mut().file_review.as_ref()
        else {
            self.app.workspaces.active_mut().set_preference_notice(
                "Load a changed-file review before copying its path.".into(),
            );
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        let export = document.path_clipboard_export();
        let notice = if export.truncated {
            "Bounded changed-file path copied (truncated).".into()
        } else {
            "Changed-file path copied.".into()
        };
        self.write_clipboard(Some(export.text), ClipboardFeedback::Notice(notice), cx);
    }

    pub(super) fn copy_file_review(&mut self, cx: &mut Context<Self>) {
        let DesktopFileReviewState::Ready(document) =
            self.app.workspaces.active_mut().file_review.as_ref()
        else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("Load a changed-file review before copying it.".into());
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        let export = document.clipboard_export();
        let notice = if export.truncated {
            "Bounded file review copied (truncated at the clipboard limit).".into()
        } else {
            "File review copied.".into()
        };
        self.write_clipboard(Some(export.text), ClipboardFeedback::Notice(notice), cx);
    }

    pub(super) fn open_review_in_external_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.app.preferences.external_editor.clone() else {
            self.app.workspaces.active_mut().set_preference_notice(
                "Configure desktop.external_editor with a program and literal argv first.".into(),
            );
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        let DesktopFileReviewState::Ready(document) =
            self.app.workspaces.active_mut().file_review.as_ref()
        else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("Load a changed-file review before opening it.".into());
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        let Some(target) = document.external_editor_target.clone() else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("This review has no external-editor target.".into());
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        let project_relative_path = target.project_relative_path().to_owned();
        let intent = DesktopCommandIntent::ExternalEditor {
            project_relative_path: project_relative_path.clone(),
        };
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        let Some(session_id) = self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .map(|projection| projection.snapshot().session.session_id.clone())
        else {
            self.complete_active_command(command_id, &intent);
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("External editor requires an open session.".into());
            self.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
            cx.notify();
            return;
        };
        let admission = self
            .connection
            .runtime_client
            .as_ref()
            .ok_or_else(|| "desktop runtime is unavailable".to_owned())
            .and_then(|runtime| {
                runtime
                    .try_open_external_editor(command_id, &session_id, &target, &editor)
                    .map_err(|error| error.to_string())
            });
        match admission {
            Ok(()) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(format!(
                        "Validating {} before editor launch…",
                        truncate_label(&project_relative_path, 48)
                    ));
            }
            Err(message) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
        cx.notify();
    }
}
