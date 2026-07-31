mod model;

pub use model::DesktopPreferences;
pub(crate) use model::{
    DesktopThinkingLevel, ExternalEditorPreference, PREFERENCES_SCHEMA_VERSION,
    valid_scratch_workspace_id,
};
#[cfg(test)]
pub(crate) use model::{
    MAX_PERSISTED_SESSION_ID_BYTES, MAX_PERSISTED_SESSION_THINKING_LEVELS, WindowGeometry,
};
