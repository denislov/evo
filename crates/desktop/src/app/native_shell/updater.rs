use gpui::Context;

#[cfg(not(test))]
use super::DesktopUpdateAvailable;
use super::NativeShell;
use crate::application::change_set::{UiChangeSet, UiRegion};

impl NativeShell {
    pub(super) fn start_update_check(&mut self, cx: &mut Context<Self>) {
        #[cfg(test)]
        {
            let _ = cx;
        }

        #[cfg(not(test))]
        {
            cx.spawn(async move |this, cx| {
                let Some(version) = crate::update::check_for_update().await else {
                    return;
                };
                let _ = this.update(cx, |this, cx| {
                    if this.ui.available_update.is_some() {
                        return;
                    }
                    this.ui.available_update = Some(DesktopUpdateAvailable {
                        version,
                        installing: false,
                        installed: false,
                        status: None,
                    });
                    this.refresh_views(UiChangeSet::one(UiRegion::Modal), cx);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    pub(super) fn install_available_update(&mut self, cx: &mut Context<Self>) {
        let Some(update) = self.ui.available_update.as_mut() else {
            return;
        };
        if update.installing || update.installed {
            return;
        }
        update.installing = true;
        update.status = None;
        self.refresh_views(UiChangeSet::one(UiRegion::Modal), cx);
        cx.notify();

        cx.spawn(async move |this, cx| {
            let outcome = crate::update::install_latest().await;
            let _ = this.update(cx, |this, cx| {
                let Some(update) = this.ui.available_update.as_mut() else {
                    return;
                };
                update.installing = false;
                match outcome {
                    Ok(message) => {
                        update.installed = true;
                        update.status = Some(message);
                    }
                    Err(error) => {
                        update.status = Some(format!("Update failed: {error}"));
                    }
                }
                this.refresh_views(UiChangeSet::one(UiRegion::Modal), cx);
                cx.notify();
            });
        })
        .detach();
    }
}
