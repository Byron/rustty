//! Headless terminal state and protocol handling.

pub mod clipboard;
pub mod color;
pub mod graphics;
pub mod input;
pub mod modes;
pub mod paste;
pub mod query;
pub mod screen;
pub mod search;
pub mod snapshot;
mod terminal;
mod terminfo;
pub mod unicode;

pub use color::parse as parse_color;
pub use input::{
    Key, KeyAction, KeyEncodeOptions, KeyEvent, Modifiers, MouseAction, MouseButton, MouseEvent,
};
pub use screen::{
    Cell, Color, Cursor, CursorShape, GridPoint, HyperlinkId, Row, Screen, ScrollbackLimits,
    Selection, SemanticContent, Style, TrackedPoint, Underline,
};
pub use terminal::{Effect, EffectHandler, Margins, Terminal, default_palette};
