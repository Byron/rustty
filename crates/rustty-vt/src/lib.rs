//! Headless terminal state and protocol handling.

pub mod clipboard;
pub mod color;
pub mod dnd;
pub mod formatter;
pub mod glyph;
pub mod graphics;
pub mod input;
pub mod modes;
mod page_layout;
mod page_list;
mod page_resources;
pub mod paste;
pub mod query;
pub mod screen;
pub mod search;
pub mod selection;
pub mod selection_gesture;
pub mod snapshot;
mod terminal;
mod terminfo;
pub mod unicode;

pub use color::parse as parse_color;
pub use input::{
    Key, KeyAction, KeyEncodeOptions, KeyEvent, Modifiers, MouseAction, MouseButton,
    MouseEncodeOptions, MouseEvent,
};
pub use page_layout::PageCapacity;
pub use page_list::PageAllocationInfo;
pub use screen::{
    Cell, Color, Cursor, CursorShape, GridPoint, HyperlinkId, Row, Screen, ScrollbackLimits,
    Selection, SemanticContent, Style, TrackedPoint, Underline,
};
pub use terminal::{Effect, EffectHandler, Margins, Terminal, default_palette};
