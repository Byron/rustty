//! Native desktop services. Winit owns windows; each backend owns OS policy.

use rustty::vt::dnd::{Item, Operations};
use std::sync::Arc;

pub use rustty::vt::dnd::{MAX_DROP_BYTES as MAX_DRAG_BYTES, MAX_ITEMS as MAX_DRAG_ITEMS};

/// Native sources never leave the callback; only names and completed drop bytes do.
#[derive(Clone, Debug)]
pub enum DragEvent {
    Enter {
        serial: u64,
        position: [f32; 2],
        source_ops: Operations,
        mimes: Vec<Vec<u8>>,
    },
    Update {
        serial: u64,
        position: [f32; 2],
        source_ops: Operations,
    },
    Leave {
        serial: u64,
    },
    Drop {
        serial: u64,
        position: [f32; 2],
        source_ops: Operations,
        items: Vec<Item>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DragResult {
    /// No registered terminal target; preserve ordinary file/text paste behavior.
    Default,
    Reject,
    Copy(Vec<Vec<u8>>),
}

pub type DragHandler = Arc<dyn Fn(DragEvent) -> DragResult + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuiNotificationEvent {
    Activated { button: u8 },
    Closed,
    Untracked,
    Failed,
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;
