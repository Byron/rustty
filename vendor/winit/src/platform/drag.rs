//! Incoming native drag callbacks used by hosts with asynchronous drop negotiation.

use std::{ffi::c_void, marker::PhantomData, ptr::NonNull, sync::Arc};

use crate::dpi::LogicalPosition;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragPhase {
    Enter,
    Update,
    Leave,
    Drop,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DragOperations {
    pub copy: bool,
    pub move_: bool,
}

/// Borrowed NSPasteboard (macOS) or IDataObject (Windows), valid only during the callback.
/// Inspect format metadata on Enter; read data only on an accepted Drop.
#[derive(Clone, Copy, Debug)]
pub struct DragSource<'a> {
    pointer: NonNull<c_void>,
    lifetime: PhantomData<&'a c_void>,
}

impl<'a> DragSource<'a> {
    pub(crate) unsafe fn new(pointer: *mut c_void) -> Option<Self> {
        NonNull::new(pointer).map(|pointer| Self {
            pointer,
            lifetime: PhantomData,
        })
    }

    pub fn as_ptr(self) -> *mut c_void {
        self.pointer.as_ptr()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DragEvent<'a> {
    pub phase: DragPhase,
    /// Top-left-relative logical window content coordinates.
    pub position: LogicalPosition<f64>,
    pub operations: DragOperations,
    /// Enter/Drop only on Windows; available for every non-Leave callback on macOS.
    pub source: Option<DragSource<'a>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragResult {
    Default,
    Reject,
    Copy,
}

pub type DragHandler = Arc<dyn for<'a> Fn(DragEvent<'a>) -> DragResult + Send + Sync>;
