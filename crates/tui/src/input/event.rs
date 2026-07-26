use super::{KeyEvent, MouseEvent};
use crate::terminal::TerminalSize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Raw(String),
    Resize(TerminalSize),
}
