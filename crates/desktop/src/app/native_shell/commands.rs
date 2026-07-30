use super::NativeShell;
use crate::application::commands::DesktopCommandIntent;

pub(super) fn reserve_command(
    shell: &mut NativeShell,
    intent: DesktopCommandIntent,
) -> Option<u64> {
    let owner = shell.app.workspaces.active_key().clone();
    match shell.app.commands.reserve(owner, intent) {
        Ok(command_id) => Some(command_id),
        Err(error) => {
            shell
                .app
                .workspaces
                .active_mut()
                .set_preference_notice(error.to_string());
            None
        }
    }
}
