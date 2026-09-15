//! Streaming UTF-8 and VT escape parsing.
//!
//! The transition machine follows Ghostty's parser. UTF-8 decoding only runs
//! in ground state; string payloads retain their original bytes. Events borrow
//! parser storage and must be consumed before the callback returns. APC and
//! DCS payloads are delivered incrementally without accumulating an image or
//! other potentially large payload in the parser.

#[rustfmt::skip]
mod table;

pub const MAX_PARAMS: usize = 24;
pub const MAX_INTERMEDIATES: usize = 4;
pub const MAX_OSC_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationError {
    LimitExceeded,
    NoPendingState,
    NonCanonical,
    ReplayWouldCommit,
}

impl std::fmt::Display for ContinuationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::LimitExceeded => "unfinished parser state exceeded the continuation budget",
            Self::NoPendingState => "continuation contains no unfinished parser state",
            Self::NonCanonical => "continuation contains bytes before its effective replay start",
            Self::ReplayWouldCommit => "continuation would repeat a completed terminal action",
        })
    }
}
impl std::error::Error for ContinuationError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiIntermediate,
    CsiParam,
    CsiIgnore,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsPassthrough,
    DcsIgnore,
    OscString,
    SosPmApcString,
}

#[derive(Clone, Copy)]
enum TransitionAction {
    None,
    Ignore,
    Print,
    Execute,
    Collect,
    Param,
    EscDispatch,
    CsiDispatch,
    Put,
    OscPut,
    ApcPut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Event<'a> {
    Print(char),
    Execute(u8),
    Esc {
        intermediates: &'a [u8],
        final_byte: u8,
    },
    Csi {
        intermediates: &'a [u8],
        params: &'a [u16],
        colon_separators: u32,
        final_byte: u8,
    },
    /// Raw OSC command, including its numeric prefix. Command-specific
    /// validation and storage limits belong to the consumer.
    Osc {
        data: &'a [u8],
        terminated_by_bell: bool,
    },
    DcsHook {
        intermediates: &'a [u8],
        params: &'a [u16],
        final_byte: u8,
    },
    DcsPut(u8),
    DcsUnhook,
    ApcStart,
    ApcPut(u8),
    ApcEnd,
    /// A complete OSC exceeded the configured capture limit and was discarded.
    OscOverflow,
}

/// Parser events with printable runs borrowed directly from the input.
/// Controls and text split across input boundaries retain scalar [`Event`]s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BatchEvent<'a> {
    PrintAscii(&'a [u8]),
    /// Valid printable UTF-8, which may also contain ASCII characters.
    PrintUtf8(&'a str),
    Event(Event<'a>),
}

#[derive(Clone, Debug)]
pub struct Parser {
    state: State,
    intermediates: [u8; MAX_INTERMEDIATES],
    intermediate_count: usize,
    params: [u16; MAX_PARAMS],
    param_count: usize,
    colon_separators: u32,
    accumulator: u16,
    digits: bool,
    osc: Vec<u8>,
    osc_limit: usize,
    osc_overflow: bool,
    utf8: Utf8Decoder,
    continuation: Vec<u8>,
    continuation_limit: usize,
    continuation_broken: bool,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            intermediates: [0; MAX_INTERMEDIATES],
            intermediate_count: 0,
            params: [0; MAX_PARAMS],
            param_count: 0,
            colon_separators: 0,
            accumulator: 0,
            digits: false,
            osc: Vec::new(),
            osc_limit: MAX_OSC_BYTES,
            osc_overflow: false,
            utf8: Utf8Decoder::default(),
            continuation: Vec::new(),
            continuation_limit: MAX_OSC_BYTES,
            continuation_broken: false,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn is_ground(&self) -> bool {
        self.state == State::Ground && self.utf8.state == 0
    }

    pub fn reset(&mut self) {
        self.state = State::Ground;
        self.utf8 = Utf8Decoder::default();
        self.clear();
        self.osc.clear();
        self.osc_overflow = false;
        self.continuation.clear();
        self.continuation_broken = false;
    }

    /// Bound the replay state retained for snapshots. Zero permits ground-only
    /// snapshots. An overflow heals after reaching ground or a fresh replay start.
    pub fn set_continuation_limit(&mut self, limit: usize) {
        self.continuation_limit = limit;
        if self.continuation.len() > limit {
            self.continuation.clear();
            self.continuation_broken = true;
        }
    }

    /// Return the canonical unfinished input tail used by GHOSTSNP version 1.
    /// Controls whose effects already committed inside a sequence are omitted.
    pub fn continuation(&self) -> Result<Vec<u8>, ContinuationError> {
        if self.continuation_broken {
            return Err(ContinuationError::LimitExceeded);
        }
        let mut scanner = Self::new();
        let mut result = Vec::with_capacity(self.continuation.len());
        for &byte in &self.continuation {
            let state = (scanner.state, scanner.utf8.state);
            let committed = scanner.scan_byte(byte);
            if committed && state == (scanner.state, scanner.utf8.state) && !scanner.is_ground() {
                continue;
            }
            result.push(byte);
        }
        Ok(result)
    }

    pub fn validate_continuation(bytes: &[u8]) -> Result<(), ContinuationError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let mut scanner = Self::new();
        let mut committed = false;
        for &byte in bytes {
            committed |= scanner.scan_byte(byte);
        }
        if scanner.is_ground() {
            return Err(ContinuationError::NoPendingState);
        }
        if scanner.replay_start(bytes) != Some(0) {
            return Err(ContinuationError::NonCanonical);
        }
        if committed {
            return Err(ContinuationError::ReplayWouldCommit);
        }
        Ok(())
    }

    fn scan_byte(&mut self, byte: u8) -> bool {
        let mut committed = false;
        self.advance_byte(byte, &mut |event| {
            committed |= !matches!(
                event,
                Event::DcsHook { .. } | Event::DcsPut(_) | Event::ApcStart | Event::ApcPut(_)
            );
        });
        committed
    }

    fn replay_start(&self, bytes: &[u8]) -> Option<usize> {
        if self.state != State::Ground {
            bytes.iter().rposition(|&b| b == 0x1b)
        } else {
            bytes.iter().rposition(|&b| b & 0xc0 != 0x80)
        }
    }

    fn retain_continuation(&mut self, bytes: &[u8]) {
        if self.is_ground() {
            self.continuation.clear();
            self.continuation_broken = false;
            return;
        }
        let bytes = if let Some(start) = self.replay_start(bytes) {
            self.continuation.clear();
            self.continuation_broken = false;
            &bytes[start..]
        } else {
            bytes
        };
        if self.continuation_broken {
            return;
        }
        if bytes.len()
            > self
                .continuation_limit
                .saturating_sub(self.continuation.len())
            || self.continuation.try_reserve(bytes.len()).is_err()
        {
            self.continuation.clear();
            self.continuation_broken = true;
        } else {
            self.continuation.extend_from_slice(bytes);
        }
    }

    /// Limit retained raw OSC bytes, including the numeric command prefix.
    /// A lower limit also discards any oversized capture already in flight;
    /// subsequent bytes are consumed until exit. Snapshot replay has its own limit.
    pub fn set_osc_limit(&mut self, limit: usize) {
        self.osc_limit = limit;
        if self.osc.len() > self.osc_limit {
            self.osc.clear();
            self.osc_overflow = true;
        }
    }

    pub fn advance(&mut self, bytes: &[u8], mut handler: impl FnMut(Event<'_>)) {
        for &byte in bytes {
            self.advance_byte(byte, &mut handler);
        }
        self.retain_continuation(bytes);
    }

    /// Like [`Self::advance`], but emit printable text as borrowed runs.
    /// Pending UTF-8 and escape sequences keep their scalar path.
    pub fn advance_batched(&mut self, bytes: &[u8], mut handler: impl FnMut(BatchEvent<'_>)) {
        let mut remaining = bytes;
        while let Some(&byte) = remaining.first() {
            if (b' '..=b'~').contains(&byte) && self.is_ground() {
                let len = remaining
                    .iter()
                    .position(|byte| !(b' '..=b'~').contains(byte))
                    .unwrap_or(remaining.len());
                handler(BatchEvent::PrintAscii(&remaining[..len]));
                remaining = &remaining[len..];
            } else {
                if (0xc2..=0xf4).contains(&byte) && self.is_ground() {
                    let text = remaining.utf8_chunks().next().unwrap().valid();
                    if !text.is_empty() {
                        self.advance_valid_utf8(text, &mut handler);
                        remaining = &remaining[text.len()..];
                        continue;
                    }
                }
                self.advance_byte(byte, &mut |event| handler(BatchEvent::Event(event)));
                remaining = &remaining[1..];
            }
        }
        self.retain_continuation(bytes);
    }

    fn advance_valid_utf8(&mut self, text: &str, handler: &mut impl FnMut(BatchEvent<'_>)) {
        // Consume the validated prefix completely, including control sequences,
        // so a stream of short runs never revalidates overlapping suffixes.
        let mut offset = 0;
        while offset < text.len() {
            if self.is_ground()
                // Raw VT parsing can reach ground inside a UTF-8 codepoint.
                && let Some(remaining) = text.get(offset..)
            {
                let len = remaining.find(char::is_control).unwrap_or(remaining.len());
                if len > 0 {
                    handler(BatchEvent::PrintUtf8(&remaining[..len]));
                    offset += len;
                    continue;
                }
            }
            self.advance_byte(text.as_bytes()[offset], &mut |event| {
                handler(BatchEvent::Event(event))
            });
            offset += 1;
        }
    }

    fn advance_byte(&mut self, byte: u8, handler: &mut impl FnMut(Event<'_>)) {
        if self.state != State::Ground {
            self.control(byte, handler);
            return;
        }
        let (codepoint, consumed) = self.utf8.next(byte);
        if let Some(codepoint) = codepoint {
            self.codepoint(codepoint, handler);
        }
        if !consumed {
            let (codepoint, consumed) = self.utf8.next(byte);
            debug_assert!(consumed);
            if let Some(codepoint) = codepoint {
                self.codepoint(codepoint, handler);
            }
        }
    }

    fn codepoint(&mut self, codepoint: char, handler: &mut impl FnMut(Event<'_>)) {
        match codepoint as u32 {
            0x1b => {
                self.state = State::Escape;
                self.clear();
            }
            0..=0x1f => handler(Event::Execute(codepoint as u8)),
            0x80..=0x9f => {} // UTF-8-encoded C1 controls are ignored by Ghostty.
            _ => handler(Event::Print(codepoint)),
        }
    }

    fn clear(&mut self) {
        self.intermediate_count = 0;
        self.param_count = 0;
        self.colon_separators = 0;
        self.accumulator = 0;
        self.digits = false;
    }

    fn finalize_params(&mut self) -> bool {
        if self.param_count >= MAX_PARAMS {
            return false;
        }
        if self.digits {
            self.params[self.param_count] = self.accumulator;
            self.param_count += 1;
        }
        true
    }

    fn control(&mut self, byte: u8, handler: &mut impl FnMut(Event<'_>)) {
        let (next, action) = table::TABLE[byte as usize][self.state as usize];
        let changed = next != self.state;
        if changed {
            match self.state {
                State::OscString => {
                    if self.osc_overflow {
                        handler(Event::OscOverflow);
                    } else {
                        handler(Event::Osc {
                            data: &self.osc,
                            terminated_by_bell: byte == 7,
                        });
                    }
                }
                State::DcsPassthrough => handler(Event::DcsUnhook),
                State::SosPmApcString => handler(Event::ApcEnd),
                _ => {}
            }
        }
        match action {
            TransitionAction::None | TransitionAction::Ignore => {}
            TransitionAction::Print => handler(Event::Print(byte as char)),
            TransitionAction::Execute => handler(Event::Execute(byte)),
            TransitionAction::Collect => {
                if self.intermediate_count < MAX_INTERMEDIATES {
                    self.intermediates[self.intermediate_count] = byte;
                    self.intermediate_count += 1;
                }
            }
            TransitionAction::Param => {
                if byte == b';' || byte == b':' {
                    if self.param_count < MAX_PARAMS {
                        self.params[self.param_count] = self.accumulator;
                        if byte == b':' {
                            self.colon_separators |= 1 << self.param_count;
                        }
                        self.param_count += 1;
                        self.accumulator = 0;
                        self.digits = false;
                    }
                } else {
                    self.accumulator = self
                        .accumulator
                        .saturating_mul(10)
                        .saturating_add((byte - b'0') as u16);
                    self.digits = true;
                }
            }
            TransitionAction::EscDispatch => handler(Event::Esc {
                intermediates: &self.intermediates[..self.intermediate_count],
                final_byte: byte,
            }),
            TransitionAction::CsiDispatch => {
                if self.finalize_params() && (byte == b'm' || self.colon_separators == 0) {
                    handler(Event::Csi {
                        intermediates: &self.intermediates[..self.intermediate_count],
                        params: &self.params[..self.param_count],
                        colon_separators: self.colon_separators,
                        final_byte: byte,
                    });
                }
            }
            TransitionAction::OscPut => {
                if !self.osc_overflow {
                    if self.osc.len() < self.osc_limit && self.osc.try_reserve(1).is_ok() {
                        self.osc.push(byte);
                    } else {
                        self.osc.clear();
                        self.osc_overflow = true;
                    }
                }
            }
            TransitionAction::Put => handler(Event::DcsPut(byte)),
            TransitionAction::ApcPut => handler(Event::ApcPut(byte)),
        }
        if changed {
            match next {
                State::Escape | State::DcsEntry | State::CsiEntry => self.clear(),
                State::OscString => {
                    self.osc.clear();
                    self.osc_overflow = false;
                }
                State::DcsPassthrough => {
                    if self.finalize_params() {
                        handler(Event::DcsHook {
                            intermediates: &self.intermediates[..self.intermediate_count],
                            params: &self.params[..self.param_count],
                            final_byte: byte,
                        });
                    }
                }
                State::SosPmApcString => handler(Event::ApcStart),
                _ => {}
            }
        }
        self.state = next;
    }
}

/// Hoehrmann DFA with Ghostty's retry-on-invalid-continuation behavior.
#[derive(Clone, Debug, Default)]
pub struct Utf8Decoder {
    state: u8,
    accumulator: u32,
}

impl Utf8Decoder {
    pub fn is_pending(&self) -> bool {
        self.state != 0
    }

    pub fn next(&mut self, byte: u8) -> (Option<char>, bool) {
        let class = table::UTF8_CLASSES[byte as usize];
        let initial = self.state;
        self.accumulator = if initial != 0 {
            (self.accumulator << 6) | (byte & 0x3f) as u32
        } else {
            (0xffu32 >> class) & byte as u32
        };
        self.state = table::UTF8_TRANSITIONS[(self.state + class) as usize];
        match self.state {
            0 => {
                let value = char::from_u32(self.accumulator);
                self.accumulator = 0;
                (value, true)
            }
            12 => {
                self.accumulator = 0;
                self.state = 0;
                (Some('\u{fffd}'), initial == 0)
            }
            _ => (None, true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(chunks: &[&[u8]]) -> Vec<String> {
        let mut parser = Parser::new();
        let mut events = Vec::new();
        for chunk in chunks {
            parser.advance(chunk, |event| events.push(format!("{event:?}")));
        }
        events
    }

    #[test]
    fn chunk_boundaries_preserve_utf8_and_all_string_protocols() {
        let bytes = b"hi\xf0\x9f\x98\x84\x1b[38:2::1:2:3m\x1b]2;a\xc3\x9c\x07\x1bP1;2q\xc3\x9c\x1b\\\x1b_Ga=t;AAAA\x1b\\end";
        let whole = events(&[bytes]);
        for split in 0..=bytes.len() {
            assert_eq!(whole, events(&[&bytes[..split], &bytes[split..]]));
        }
        assert_eq!(whole, events(&bytes.chunks(1).collect::<Vec<_>>()));
        assert!(whole.iter().any(|e| e == "Print('😄')"));
        assert!(whole.iter().any(|e| e == "DcsPut(156)"));
        assert!(whole.iter().any(|e| e == "ApcStart"));
        assert!(whole.iter().any(|e| e == "ApcEnd"));
    }

    #[test]
    fn malformed_utf8_retries_control_byte_and_ignores_decoded_c1() {
        let result = events(&[b"\xf0\x9f\x1b[31m\xed\xa0\x80\xc2\x9bX"]);
        assert_eq!(
            result.iter().filter(|s| s.as_str() == "Print('�')").count(),
            4
        );
        assert!(result.iter().any(|s| s.contains("params: [31]")));
        assert_eq!(result.last().unwrap(), "Print('X')");
    }

    #[test]
    fn printable_batches_preserve_scalar_events_and_continuations() {
        let bytes = [
            b"hello\x7f world\x80\xc2\x9b\xf0\x9fASCII\xed\xa0\x80\xf0\x9f\x1b[31mred\x1b[0m\x07\x1b]2;title\x07\x1bP1;2qraw ascii\x1b\\\x1b_Ga=t;AAAA\x1b\\end".as_slice(),
            "世界 café العربية e\u{301} 👩\u{200d}💻\u{85}after C1\u{7f}".as_bytes(),
            // ST inside these valid codepoints leaves raw ESC/APC parsing in
            // ground, where the remaining continuation byte must decode alone.
            b"\x1b\xe2\x9c\x80ground\x1b_\xe2\x9c\x80after APC",
            b"\xe0\xa0\xf0\x9f\x98\x84\x1b[38:2::1:2",
        ].concat();
        for limit in [MAX_OSC_BYTES, 4] {
            for split in 0..=bytes.len() {
                let mut scalar = Parser::new();
                scalar.set_osc_limit(limit);
                scalar.set_continuation_limit(limit);
                let mut batched = scalar.clone();
                let mut expected = Vec::new();
                let mut actual = Vec::new();
                let mut runs = Vec::new();
                let mut unicode_runs = 0;
                for chunk in [&bytes[..split], &bytes[split..]] {
                    scalar.advance(chunk, |event| expected.push(format!("{event:?}")));
                    batched.advance_batched(chunk, |event| match event {
                        BatchEvent::PrintAscii(run) => {
                            assert!(!run.is_empty());
                            assert!(run.iter().all(|byte| (b' '..=b'~').contains(byte)));
                            assert!(run.as_ptr() >= chunk.as_ptr());
                            assert!(run.as_ptr_range().end <= chunk.as_ptr_range().end);
                            runs.push(run.to_vec());
                            actual.extend(
                                run.iter()
                                    .map(|&byte| format!("{:?}", Event::Print(byte as char))),
                            );
                        }
                        BatchEvent::PrintUtf8(run) => {
                            assert!(!run.is_empty());
                            assert!(!run.chars().any(char::is_control));
                            assert!(run.as_ptr() >= chunk.as_ptr());
                            assert!(run.as_bytes().as_ptr_range().end <= chunk.as_ptr_range().end);
                            unicode_runs += usize::from(!run.is_ascii());
                            actual.extend(run.chars().map(|cp| format!("{:?}", Event::Print(cp))));
                        }
                        BatchEvent::Event(event) => actual.push(format!("{event:?}")),
                    });
                    assert_eq!(actual, expected, "split={split}");
                    assert_eq!(batched.state(), scalar.state(), "split={split}");
                    assert_eq!(batched.is_ground(), scalar.is_ground(), "split={split}");
                    assert_eq!(
                        batched.continuation(),
                        scalar.continuation(),
                        "split={split}"
                    );
                }
                if split == 0 {
                    assert_eq!(runs[0], b"hello");
                    assert_eq!(runs[1], b" world");
                }
                assert!(unicode_runs > 0);
            }
        }
    }

    #[test]
    fn bounded_parameters_saturate_and_reject_invalid_separators() {
        assert!(events(&[b"\x1b[999999999999999999999m"])[0].contains("65535"));
        assert!(events(&[b"\x1b[1:2H"]).is_empty());
        assert!(events(&[format!("\x1b[{}m", "1;".repeat(24)).as_bytes()]).is_empty());
        assert!(events(&[b"\x1b[1;m"])[0].contains("params: [1]"));
    }

    #[test]
    fn overflow_recovers_at_string_terminator() {
        let mut parser = Parser::new();
        parser.set_osc_limit(4);
        let mut result = Vec::new();
        parser.advance(b"\x1b]2;too long\x07ok", |e| result.push(format!("{e:?}")));
        assert_eq!(result, ["OscOverflow", "Print('o')", "Print('k')"]);
        assert!(parser.is_ground());
    }

    #[test]
    fn raw_osc_budget_is_configurable_and_survives_reset() {
        let data = vec![b'x'; MAX_OSC_BYTES + 1];
        for limit in [MAX_OSC_BYTES, MAX_OSC_BYTES + 5] {
            let mut parser = Parser::new();
            if limit != MAX_OSC_BYTES {
                parser.set_osc_limit(limit);
            }
            parser.reset();
            parser.advance(b"\x1b]", |_| panic!("unfinished OSC emitted an event"));
            parser.advance(&data, |_| panic!("unfinished OSC emitted an event"));
            let mut emitted = false;
            parser.advance(b"\x07", |event| {
                emitted = true;
                match event {
                    Event::Osc { data, .. } => {
                        assert!(limit > MAX_OSC_BYTES);
                        assert_eq!(data.len(), MAX_OSC_BYTES + 1);
                    }
                    Event::OscOverflow => assert_eq!(limit, MAX_OSC_BYTES),
                    event => panic!("unexpected event: {event:?}"),
                }
            });
            assert!(emitted);
            assert!(parser.is_ground());
        }
    }

    #[test]
    fn unfinished_utf8_is_not_ground() {
        let mut parser = Parser::new();
        parser.advance(b"\xf0\x9f", |_| {});
        assert!(!parser.is_ground());
        let mut result = String::new();
        parser.advance(b"\x98\x84", |e| {
            if let Event::Print(c) = e {
                result.push(c)
            }
        });
        assert_eq!(result, "😄");
        assert!(parser.is_ground());
    }
}
