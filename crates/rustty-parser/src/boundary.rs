use crate::{BatchEvent, Event, Parser, State};

/// VT stream boundaries to stop after. Combine flags with `|`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Boundaries(u8);

impl Boundaries {
    pub const NONE: Self = Self(0);
    /// Already-ground parsers stop immediately, consuming no input.
    pub const GROUND: Self = Self(1);
    pub const APC_START: Self = Self(1 << 1);
    pub const APC_END: Self = Self(1 << 2);
    /// DCS starts at its introducer, before the header and payload.
    pub const DCS_START: Self = Self(1 << 3);
    /// Includes a DCS cancelled while its header is still incomplete.
    pub const DCS_END: Self = Self(1 << 4);
    pub const OSC_START: Self = Self(1 << 5);
    pub const OSC_END: Self = Self(1 << 6);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for Boundaries {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringKind {
    /// Includes SOS and PM, which share the APC parser state.
    Apc,
    Dcs,
    Osc,
}

impl StringKind {
    fn at(state: State) -> Option<Self> {
        match state {
            State::SosPmApcString => Some(Self::Apc),
            State::DcsEntry
            | State::DcsParam
            | State::DcsIntermediate
            | State::DcsPassthrough
            | State::DcsIgnore => Some(Self::Dcs),
            State::OscString => Some(Self::Osc),
            _ => None,
        }
    }

    fn start(self) -> Boundaries {
        match self {
            Self::Apc => Boundaries::APC_START,
            Self::Dcs => Boundaries::DCS_START,
            Self::Osc => Boundaries::OSC_START,
        }
    }

    fn end(self) -> Boundaries {
        match self {
            Self::Apc => Boundaries::APC_END,
            Self::Dcs => Boundaries::DCS_END,
            Self::Osc => Boundaries::OSC_END,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringIntroducer {
    Esc,
    C1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringEnd {
    /// ESC ended the string; the following backslash is still unconsumed.
    StEsc,
    StC1,
    Cancelled,
    Aborted,
    Bel,
}

/// A stop includes only requested boundaries and their applicable metadata.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BoundaryStop {
    /// Includes the byte reaching the boundary, or all input when none matched.
    pub consumed: usize,
    pub reached: Boundaries,
    /// If a byte ends one requested string and starts another, names the old one.
    pub kind: Option<StringKind>,
    pub introducer: Option<StringIntroducer>,
    pub end: Option<StringEnd>,
    /// The start/end byte; absent for ground-only stops or exhausted input.
    pub byte: Option<u8>,
}

impl Parser {
    /// Process input until a requested boundary is reached, including its byte.
    /// Ground is a state: requesting it while already ground consumes nothing.
    /// Use `NONE` or [`Self::advance`] to make progress after such a stop.
    ///
    /// Start/end boundaries follow parser transitions, including cancellations,
    /// header-only DCS strings, and C1 bytes that end one string and start another.
    /// End stops at ESC leave the backslash of ST for the next call. In tmux DCS
    /// passthrough mode, the escaped terminator instead completes at the backslash.
    pub fn advance_until(
        &mut self,
        bytes: &[u8],
        boundaries: Boundaries,
        mut handler: impl FnMut(Event<'_>),
    ) -> BoundaryStop {
        if boundaries.contains(Boundaries::GROUND) && self.is_ground() {
            return BoundaryStop {
                reached: Boundaries::GROUND,
                ..Default::default()
            };
        }
        for (index, &byte) in bytes.iter().enumerate() {
            let previous = self.state;
            let dcs_escape = self.tmux_dcs_escape;
            let before = StringKind::at(previous);
            self.advance_byte(byte, &mut handler);
            let after = StringKind::at(self.state);
            let mut stop = BoundaryStop {
                consumed: index + 1,
                ..Default::default()
            };
            if boundaries.contains(Boundaries::GROUND) && self.is_ground() {
                stop.reached = Boundaries::GROUND;
            }
            if before != after {
                if let Some(kind) = before.filter(|kind| boundaries.contains(kind.end())) {
                    stop.reached = stop.reached | kind.end();
                    stop.kind = Some(kind);
                    stop.byte = Some(byte);
                    stop.end = Some(match byte {
                        0x1b => StringEnd::StEsc,
                        b'\\' if dcs_escape => StringEnd::StEsc,
                        0x9c => StringEnd::StC1,
                        0x18 | 0x1a => StringEnd::Cancelled,
                        7 => StringEnd::Bel,
                        _ => StringEnd::Aborted,
                    });
                }
                if let Some(kind) = after.filter(|kind| boundaries.contains(kind.start())) {
                    stop.reached = stop.reached | kind.start();
                    stop.kind.get_or_insert(kind);
                    stop.byte = Some(byte);
                    stop.introducer = Some(if byte >= 0x80 {
                        StringIntroducer::C1
                    } else {
                        StringIntroducer::Esc
                    });
                }
            }
            if !stop.reached.is_empty() {
                self.retain_continuation(&bytes[..stop.consumed]);
                return stop;
            }
        }
        self.retain_continuation(bytes);
        BoundaryStop {
            consumed: bytes.len(),
            ..Default::default()
        }
    }

    /// Like [`Self::advance_until`], with the batched event interface.
    /// `NONE` retains the full-input batched path; boundary writes emit scalar events.
    pub fn advance_batched_until(
        &mut self,
        bytes: &[u8],
        boundaries: Boundaries,
        mut handler: impl FnMut(BatchEvent<'_>),
    ) -> BoundaryStop {
        if boundaries.is_empty() {
            self.advance_batched(bytes, handler);
            BoundaryStop {
                consumed: bytes.len(),
                ..Default::default()
            }
        } else {
            self.advance_until(bytes, boundaries, |event| handler(BatchEvent::Event(event)))
        }
    }
}
