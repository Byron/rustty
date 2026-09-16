//! Opt-in, thread-local observations; the normal library has no probe calls.
use std::cell::Cell;

#[derive(Clone, Copy, Default, serde::Serialize)]
pub struct Counts {
    pub hyperlink_admissions: usize,
    pub string_reservations: usize,
    pub grapheme_admissions: usize,
    pub grapheme_appends: usize,
    pub rebuilds: usize,
    pub resource_growths: usize,
    pub rebuild_allocations: usize,
    pub rebuild_requested_bytes: usize,
    /// Other (including graphemes), temporary hyperlink keys/payloads,
    /// owned hyperlink payloads, page cell/header/identity buffers.
    pub allocations: [usize; 4],
    pub requested_bytes: [usize; 4],
}

#[derive(Clone, Copy, Default)]
pub(crate) enum Kind {
    #[default]
    Other,
    TemporaryPayload,
    OwnedPayload,
    PageBuffer,
}

#[derive(Clone, Copy, Default)]
struct Context {
    kind: Kind,
    rebuilding: bool,
}

thread_local! {
    static COUNTS: Cell<Counts> = Cell::new(Counts::default());
    static CONTEXT: Cell<Context> = Cell::new(Context::default());
}

pub fn reset() {
    COUNTS.set(Counts::default());
}

pub fn counts() -> Counts {
    COUNTS.get()
}

pub(crate) fn event(f: impl FnOnce(&mut Counts)) {
    let mut counts = COUNTS.get();
    f(&mut counts);
    COUNTS.set(counts);
}

/// Called by the standalone allocator only while its measured region is active.
pub fn allocation(bytes: usize) {
    let context = CONTEXT.get();
    event(|counts| {
        counts.allocations[context.kind as usize] += 1;
        counts.requested_bytes[context.kind as usize] += bytes;
        if context.rebuilding {
            counts.rebuild_allocations += 1;
            counts.rebuild_requested_bytes += bytes;
        }
    });
}

pub(crate) struct Scope(Context);

impl Scope {
    pub fn enter(kind: Kind) -> Self {
        Self(CONTEXT.replace(Context {
            kind,
            ..CONTEXT.get()
        }))
    }

    pub fn rebuild(growing: bool) -> Self {
        event(|counts| {
            counts.rebuilds += 1;
            counts.resource_growths += usize::from(growing);
        });
        Self(CONTEXT.replace(Context {
            rebuilding: true,
            ..CONTEXT.get()
        }))
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        CONTEXT.set(self.0);
    }
}
