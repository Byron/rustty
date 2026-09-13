//! Headless terminal state and protocol handling.

pub mod graphics;
pub mod input;
pub mod modes;
pub mod screen;
pub mod search;
pub mod snapshot;
mod terminal;
pub mod unicode;

pub use input::{Key, KeyAction, KeyEvent, Modifiers, MouseAction, MouseButton, MouseEvent};
pub use screen::{
    Cell, Color, Cursor, CursorShape, GridPoint, HyperlinkId, Row, Screen, ScrollbackLimits,
    Selection, SemanticContent, Style, TrackedPoint, Underline,
};
pub use terminal::{Effect, Margins, Terminal, default_palette, parse_color};
