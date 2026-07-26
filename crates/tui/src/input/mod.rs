mod event;
mod key;
mod keybindings;
mod mouse;
mod stdin;

pub use event::InputEvent;
pub use key::{
    Key, KeyEvent, KeyEventKind, KeyModifiers, is_key_release, is_kitty_protocol_active,
    matches_key, parse_key, set_kitty_protocol_active,
};
pub use keybindings::{
    GENERIC_TUI_KEYBINDINGS, KeybindingConflict, KeybindingDefinition, KeybindingsConfig,
    KeybindingsManager, TUI_KEYBINDINGS,
};
pub use mouse::{MouseButton, MouseEvent, MouseEventKind, MouseModifiers, parse_sgr_mouse};
pub use stdin::StdinBuffer;
