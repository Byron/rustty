//! Input encoders consume terminal modes and leave platform event handling to hosts.
use crate::Terminal;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub super_key: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
}

impl Modifiers {
    fn number(self) -> u16 {
        1 + u16::from(self.shift)
            + 2 * u16::from(self.alt)
            + 4 * u16::from(self.control)
            + 8 * u16::from(self.super_key)
    }
    fn kitty_number(self) -> u16 {
        self.number() + 64 * u16::from(self.caps_lock) + 128 * u16::from(self.num_lock)
    }

    fn without(self, consumed: Self) -> Self {
        Self {
            shift: self.shift && !consumed.shift,
            control: self.control && !consumed.control,
            alt: self.alt && !consumed.alt,
            super_key: self.super_key && !consumed.super_key,
            caps_lock: self.caps_lock && !consumed.caps_lock,
            num_lock: self.num_lock && !consumed.num_lock,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A host key without a terminal-specific identity; text and layout data still apply.
    Unidentified,
    Char(char),
    Enter,
    Backspace,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Help,
    ContextMenu,
    CapsLock,
    NumLock,
    ScrollLock,
    PrintScreen,
    Pause,
    Function(u8),
    Keypad(u8),
    KeypadEnter,
    KeypadDecimal,
    KeypadAdd,
    KeypadSubtract,
    KeypadMultiply,
    KeypadDivide,
    KeypadEqual,
    KeypadSeparator,
    KeypadLeft,
    KeypadRight,
    KeypadUp,
    KeypadDown,
    KeypadPageUp,
    KeypadPageDown,
    KeypadHome,
    KeypadEnd,
    KeypadInsert,
    KeypadDelete,
    KeypadBegin,
    Shift,
    Control,
    Alt,
    Super,
    ShiftRight,
    ControlRight,
    AltRight,
    SuperRight,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyAction {
    #[default]
    Press,
    Repeat,
    Release,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub text: Option<String>,
    pub modifiers: Modifiers,
    /// Modifiers consumed in producing nonempty text. Raw modifier bits remain
    /// available for protocol reporting and physical functional-key matching.
    pub consumed_modifiers: Modifiers,
    pub action: KeyAction,
    /// Logical unshifted Unicode key. `key` retains the physical/base-layout key.
    pub unshifted: Option<char>,
    pub composing: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyEncodeOptions {
    /// Whether the held macOS Option key acts as terminal Alt. The host resolves
    /// left/right configuration using native modifier-side information.
    pub macos_option_as_alt: bool,
}

impl KeyEvent {
    pub fn new(key: Key) -> Self {
        Self {
            key,
            text: match key {
                Key::Char(cp) => Some(cp.to_string()),
                _ => None,
            },
            modifiers: Modifiers::default(),
            consumed_modifiers: Modifiers::default(),
            action: KeyAction::Press,
            unshifted: match key {
                Key::Char(cp) => Some(cp),
                _ => None,
            },
            composing: false,
        }
    }

    fn effective_modifiers(&self) -> Modifiers {
        if self.text.as_ref().is_some_and(|text| !text.is_empty()) {
            self.modifiers.without(self.consumed_modifiers)
        } else {
            self.modifiers
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Move,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
    Extra(u8),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseEvent {
    pub action: MouseAction,
    pub button: Option<MouseButton>,
    /// Surface-space pixels, including renderer padding. Negative and
    /// fractional positions are retained until protocol encoding.
    pub x: f64,
    pub y: f64,
    pub modifiers: Modifiers,
}

#[derive(Debug)]
pub struct MouseEncodeOptions<'a> {
    /// Full surface dimensions, including padding, in physical pixels.
    pub screen_size: [f64; 2],
    pub cell_size: [u32; 2],
    /// Left, top, right and bottom padding in physical pixels.
    pub padding: [f64; 4],
    /// Current host button state, including the event being encoded.
    pub any_button_pressed: bool,
    /// Optional host-owned state. Motion within the same cell is suppressed,
    /// except in SGR-pixel mode. Clear the stored value to reset tracking.
    pub last_cell: Option<&'a mut Option<[u16; 2]>>,
}

impl Terminal {
    /// Application's XTSHIFTESCAPE request; the host applies its user policy.
    pub fn mouse_shift_capture(&self) -> Option<bool> {
        self.metadata.mouse_shift_capture
    }

    /// W3C pointer name selected by OSC 22, also retained in terminal snapshots.
    pub fn mouse_shape(&self) -> &'static str {
        MOUSE_SHAPES[usize::from(self.metadata.mouse_shape)]
    }

    /// Encode an event whose Alt modifier already denotes terminal Alt.
    pub fn encode_key(&self, event: &KeyEvent) -> Vec<u8> {
        self.encode_key_with_options(
            event,
            KeyEncodeOptions {
                macos_option_as_alt: true,
            },
        )
    }

    pub fn encode_key_with_options(&self, event: &KeyEvent, options: KeyEncodeOptions) -> Vec<u8> {
        if self.modes.get(false, 2) {
            return Vec::new();
        }
        let flags = self.screen().kitty_keyboard.current();
        if flags != 0 {
            return kitty_key(event, flags, options);
        }
        if event.action == KeyAction::Release || event.composing {
            return Vec::new();
        }
        let mods = event.modifiers;
        let effective = event.effective_modifiers();
        let text = event.text.as_deref().unwrap_or("");
        if let Some(sequence) = pc_key(self, event.key, mods) {
            if !text.is_empty()
                && !is_control_text(text)
                && matches!(event.key, Key::Enter | Key::Escape | Key::Backspace)
            {
                if event.key == Key::Backspace {
                    return Vec::new();
                }
            } else {
                return sequence;
            }
        }
        if self.modify_other_keys
            && let Some(cp) = single_char(text)
            && (('@'..='\u{7f}').contains(&cp)
                || mods.control
                || mods.alt
                || mods.super_key
                || cp == ' ')
            && mods.number() > 1
        {
            return format!("\x1b[27;{};{}~", mods.number(), cp as u32).into_bytes();
        }
        if let Some(byte) = control_sequence(event) {
            return if effective.alt {
                vec![0x1b, byte]
            } else {
                vec![byte]
            };
        }
        if text.is_empty() {
            return alt_prefix(self, event, options).unwrap_or_default();
        }
        if mods.control
            && let Some(mut cp) = single_char(text)
        {
            let mut sequence_mods = mods;
            sequence_mods.super_key = false;
            if sequence_mods.shift && cp.is_ascii_uppercase() {
                cp = cp.to_ascii_lowercase();
            }
            if event.unshifted != Some(cp) {
                sequence_mods.shift = false;
            }
            return format!("\x1b[{};{}u", cp as u32, sequence_mods.number()).into_bytes();
        }
        if let Some(output) = alt_prefix(self, event, options) {
            return output;
        }
        if cfg!(target_os = "macos") && mods.super_key {
            return Vec::new();
        }
        text.as_bytes().to_vec()
    }

    pub fn encode_focus(&self, focused: bool) -> Vec<u8> {
        if self.modes.dec(1004) {
            if focused {
                b"\x1b[I".to_vec()
            } else {
                b"\x1b[O".to_vec()
            }
        } else {
            Vec::new()
        }
    }

    /// Reports whether a clipboard paste is safe in the current terminal mode.
    pub fn paste_is_safe(&self, text: impl AsRef<[u8]>) -> bool {
        paste_is_safe(text, self.modes.dec(2004))
    }

    /// Low-level text insertion; callers enforce their clipboard confirmation policy.
    pub fn encode_paste(&self, text: impl AsRef<[u8]>) -> Vec<u8> {
        let text = text.as_ref();
        let bracketed = self.modes.dec(2004);
        let mut output = Vec::with_capacity(text.len() + 12);
        if bracketed {
            output.extend_from_slice(b"\x1b[200~");
        }
        for &byte in text {
            output.push(match byte {
                0x00 | 0x08 | 0x05 | 0x04 | 0x1b | 0x7f | 0x03 | 0x1c | 0x15 | 0x1a | 0x11
                | 0x13 | 0x17 | 0x16 | 0x12 | 0x0f => b' ',
                b'\n' if !bracketed => b'\r',
                _ => byte,
            });
        }
        if bracketed {
            output.extend_from_slice(b"\x1b[201~");
        }
        output
    }

    pub fn encode_mouse(&self, event: MouseEvent, options: MouseEncodeOptions<'_>) -> Vec<u8> {
        let mode = self.mouse_mode;
        if !event.x.is_finite()
            || !event.y.is_finite()
            || options.cell_size.contains(&0)
            || options
                .screen_size
                .iter()
                .any(|p| !p.is_finite() || *p < 0.0)
            || options.padding.iter().any(|p| !p.is_finite() || *p < 0.0)
            || mode == 0
            || mode == 9
                && (event.action != MouseAction::Press
                    || !matches!(
                        event.button,
                        Some(MouseButton::Left | MouseButton::Middle | MouseButton::Right)
                    ))
            || mode == 1000 && event.action == MouseAction::Move
            || mode == 1002 && event.button.is_none()
        {
            return Vec::new();
        }
        // Native viewport bounds are compared at the surface's f32 precision.
        let bounds = options.screen_size.map(|size| f64::from(size as f32));
        let outside = event.x < 0.0 || event.y < 0.0 || event.x > bounds[0] || event.y > bounds[1];
        if event.action != MouseAction::Release
            && outside
            && (!matches!(mode, 1002 | 1003) || !options.any_button_pressed)
        {
            return Vec::new();
        }
        let pixels = [event.x - options.padding[0], event.y - options.padding[1]];
        let mut cell = [0; 2];
        for axis in 0..2 {
            let extent =
                (options.screen_size[axis] - options.padding[axis] - options.padding[axis + 2])
                    .max(0.0);
            // Native renderer grid dimensions use f32 division. Position
            // conversion retains f64 precision until the cell is clamped.
            let count = ((extent as f32 / options.cell_size[axis] as f32) as u16).max(1);
            cell[axis] = ((pixels[axis].max(0.0) / f64::from(options.cell_size[axis])) as u16)
                .min(count - 1);
        }
        if let Some(last) = options.last_cell {
            if event.action == MouseAction::Move && self.mouse_format != 1016 && *last == Some(cell)
            {
                return Vec::new();
            }
            // Native tracking advances before rejecting an unsupported
            // button or a coordinate the selected format cannot encode.
            *last = Some(cell);
        }
        let release = event.action == MouseAction::Release;
        let mut code: u16 = if release && !matches!(self.mouse_format, 1006 | 1016) {
            3
        } else {
            match event.button {
                Some(MouseButton::Left) => 0,
                Some(MouseButton::Middle) => 1,
                Some(MouseButton::Right) => 2,
                Some(MouseButton::WheelUp) => 64,
                Some(MouseButton::WheelDown) => 65,
                Some(MouseButton::WheelLeft) => 66,
                Some(MouseButton::WheelRight) => 67,
                Some(MouseButton::Extra(n @ 0..=1)) => 128 + u16::from(n),
                Some(MouseButton::Extra(_)) => return Vec::new(),
                None => 3,
            }
        };
        if event.action == MouseAction::Move {
            code += 32;
        }
        if mode != 9 {
            code += u16::from(event.modifiers.shift) * 4
                + u16::from(event.modifiers.alt) * 8
                + u16::from(event.modifiers.control) * 16;
        }
        let (x, y) = if self.mouse_format == 1016 {
            (pixels[0].round() as i32, pixels[1].round() as i32)
        } else {
            (i32::from(cell[0]) + 1, i32::from(cell[1]) + 1)
        };
        match self.mouse_format {
            1006 | 1016 => {
                format!("\x1b[<{code};{x};{y}{}", if release { 'm' } else { 'M' }).into_bytes()
            }
            1015 => format!("\x1b[{};{x};{y}M", code + 32).into_bytes(),
            1005 => {
                let (Some(x), Some(y)) =
                    (char::from_u32(x as u32 + 32), char::from_u32(y as u32 + 32))
                else {
                    return Vec::new();
                };
                let mut output = b"\x1b[M".to_vec();
                output.push((code + 32) as u8);
                for cp in [x, y] {
                    output.extend_from_slice(cp.encode_utf8(&mut [0; 4]).as_bytes());
                }
                output
            }
            0 if x <= 223 && y <= 223 && code + 32 <= 255 => vec![
                0x1b,
                b'[',
                b'M',
                (code + 32) as u8,
                (x + 32) as u8,
                (y + 32) as u8,
            ],
            _ => Vec::new(),
        }
    }
}

// Order is part of the inherited GHOSTSNP terminal header.
const MOUSE_SHAPES: [&str; 34] = [
    "default",
    "context-menu",
    "help",
    "pointer",
    "progress",
    "wait",
    "cell",
    "crosshair",
    "text",
    "vertical-text",
    "alias",
    "copy",
    "move",
    "no-drop",
    "not-allowed",
    "grab",
    "grabbing",
    "all-scroll",
    "col-resize",
    "row-resize",
    "n-resize",
    "e-resize",
    "s-resize",
    "w-resize",
    "ne-resize",
    "nw-resize",
    "se-resize",
    "sw-resize",
    "ew-resize",
    "ns-resize",
    "nesw-resize",
    "nwse-resize",
    "zoom-in",
    "zoom-out",
];

pub(crate) fn mouse_shape_index(name: &str) -> Option<u8> {
    let name = match name {
        "left_ptr" => "default",
        "question_arrow" => "help",
        "hand" => "pointer",
        "left_ptr_watch" => "progress",
        "watch" => "wait",
        "cross" => "crosshair",
        "xterm" => "text",
        "dnd-link" => "alias",
        "dnd-copy" => "copy",
        "dnd-move" => "move",
        "dnd-no-drop" => "no-drop",
        "crossed_circle" => "not-allowed",
        "hand1" => "grab",
        "right_side" => "e-resize",
        "top_side" => "n-resize",
        "top_right_corner" => "ne-resize",
        "top_left_corner" => "nw-resize",
        "bottom_side" => "s-resize",
        "bottom_right_corner" => "se-resize",
        "bottom_left_corner" => "sw-resize",
        "left_side" => "w-resize",
        "fleur" => "all-scroll",
        name => name,
    };
    MOUSE_SHAPES
        .iter()
        .position(|&shape| shape == name)
        .map(|index| index as u8)
}

/// Check clipboard bytes before encoding. Pass `false` to reject newlines
/// regardless of terminal state, or the active bracketed-paste mode otherwise.
pub fn paste_is_safe(text: impl AsRef<[u8]>, bracketed: bool) -> bool {
    let text = text.as_ref();
    (bracketed || !text.contains(&b'\n')) && !text.windows(6).any(|part| part == b"\x1b[201~")
}

fn single_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let cp = chars.next()?;
    chars.next().is_none().then_some(cp)
}

fn is_control(cp: char) -> bool {
    cp < ' ' || cp == '\u{7f}'
}
fn is_control_text(text: &str) -> bool {
    text.len() == 1 && is_control(text.chars().next().unwrap())
}

fn physical_codepoint(key: Key) -> Option<char> {
    match key {
        Key::Char(cp) => Some(cp),
        Key::KeypadEqual => Some('='),
        Key::KeypadEnter => None,
        key => keypad(key).map(|(_, plain)| plain),
    }
}

fn alt_prefix(terminal: &Terminal, event: &KeyEvent, options: KeyEncodeOptions) -> Option<Vec<u8>> {
    if !event.effective_modifiers().alt
        || !terminal.modes.dec(1036)
        || cfg!(target_os = "macos") && !options.macos_option_as_alt
    {
        return None;
    }
    let text = event.text.as_deref().unwrap_or("");
    let value = if text.len() == 1 {
        text.to_owned()
    } else if cfg!(target_os = "macos")
        && let Some(cp) = event.unshifted
    {
        cp.to_string()
    } else if !text.is_empty() {
        text.to_owned()
    } else {
        event.unshifted?.to_string()
    };
    let mut output = vec![0x1b];
    output.extend_from_slice(value.as_bytes());
    Some(output)
}

fn control_sequence(event: &KeyEvent) -> Option<u8> {
    let mut mods = event.modifiers;
    if !mods.control {
        return None;
    }
    mods.alt = false;
    let text = event.text.as_deref().unwrap_or("");
    let mut cp = if text.len() == 1 {
        text.as_bytes()[0] as char
    } else if let Key::Char(cp) = event.key {
        if !cp.is_ascii() || mods.shift || mods.super_key {
            return None;
        }
        cp
    } else {
        return None;
    };
    if mods.shift && !cp.is_ascii_uppercase() && cp != '@' {
        mods.shift = false;
    }
    if cp.is_ascii_uppercase()
        && let Some(base) = event.unshifted.filter(char::is_ascii)
    {
        cp = base;
    }
    if mods.shift || mods.super_key {
        return None;
    }
    Some(match cp {
        ' ' | '2' | '@' => 0,
        '/' | '7' | '_' => 31,
        '0' | '1' | '9' => cp as u8,
        '3' => 27,
        '4' | '\\' => 28,
        '5' | ']' => 29,
        '6' | '^' | '~' => 30,
        '8' | '?' => 127,
        'a'..='h' | 'j'..='l' | 'n'..='z' => cp as u8 - b'a' + 1,
        _ => return None,
    })
}

fn pc_key(t: &Terminal, key: Key, mods: Modifiers) -> Option<Vec<u8>> {
    let number = mods.number();
    if let Some(final_byte) = match key {
        Key::Up | Key::KeypadUp => Some('A'),
        Key::Down | Key::KeypadDown => Some('B'),
        Key::Right | Key::KeypadRight => Some('C'),
        Key::Left | Key::KeypadLeft => Some('D'),
        Key::KeypadBegin => Some('E'),
        Key::Home | Key::KeypadHome => Some('H'),
        Key::End | Key::KeypadEnd => Some('F'),
        _ => None,
    } {
        return Some(
            if number > 1 {
                format!("\x1b[1;{number}{final_byte}")
            } else {
                format!("\x1b{}{final_byte}", if t.modes.dec(1) { 'O' } else { '[' })
            }
            .into_bytes(),
        );
    }
    if let Some((code, final_byte)) = functional(key) {
        return Some(
            if let Key::Function(f @ 1..=4) = key
                && number == 1
            {
                vec![0x1b, b'O', b'P' + f - 1]
            } else if number == 1 {
                format!("\x1b[{code}{final_byte}").into_bytes()
            } else {
                format!("\x1b[{code};{number}{final_byte}").into_bytes()
            },
        );
    }
    if let Some((app, plain)) = keypad(key) {
        return Some(if t.modes.dec(66) && !t.modes.dec(1035) {
            if number > 1 {
                format!("\x1bO{number}{app}").into_bytes()
            } else {
                vec![0x1b, b'O', app as u8]
            }
        } else {
            vec![plain as u8]
        });
    }
    if key == Key::Backspace {
        let mask = u8::from(mods.shift)
            + 2 * u8::from(mods.control)
            + 4 * u8::from(mods.alt)
            + 8 * u8::from(mods.super_key);
        if t.modify_other_keys && !matches!(mask, 0 | 2) {
            return Some(format!("\x1b[27;{number};127~").into_bytes());
        }
        return Some(match mask {
            0 | 7 => vec![if t.modes.dec(67) { 8 } else { 127 }],
            2 => vec![if t.modes.dec(67) { 127 } else { 8 }],
            1 | 8 | 9 => vec![127],
            4 | 5 | 12 | 13 => vec![0x1b, 127],
            3 | 10 | 11 => vec![8],
            6 | 14 | 15 => vec![0x1b, 8],
            _ => unreachable!(),
        });
    }
    let byte = match key {
        Key::Enter => 13,
        Key::Tab => 9,
        Key::Escape => 27,
        _ => return None,
    };
    Some(if number == 1 {
        vec![byte]
    } else if !t.modify_other_keys && number == 3 {
        vec![0x1b, byte]
    } else if !t.modify_other_keys && key == Key::Tab && number == 2 {
        b"\x1b[Z".to_vec()
    } else {
        format!("\x1b[27;{number};{byte}~").into_bytes()
    })
}

fn functional(key: Key) -> Option<(u32, char)> {
    Some(match key {
        Key::Insert | Key::KeypadInsert => (2, '~'),
        Key::Delete | Key::KeypadDelete => (3, '~'),
        Key::PageUp | Key::KeypadPageUp => (5, '~'),
        Key::PageDown | Key::KeypadPageDown => (6, '~'),
        Key::Help => (28, '~'),
        Key::ContextMenu => (29, '~'),
        Key::Function(1) => (1, 'P'),
        Key::Function(2) => (1, 'Q'),
        Key::Function(3) => (13, '~'),
        Key::Function(4) => (1, 'S'),
        Key::Function(f @ 5..=25) => (
            [
                15, 17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29, 31, 32, 33, 34, 42, 43, 44, 45, 46,
            ][(f - 5) as usize],
            '~',
        ),
        _ => return None,
    })
}

fn keypad(key: Key) -> Option<(char, char)> {
    Some(match key {
        Key::Keypad(n @ 0..=9) => ((b'p' + n) as char, (b'0' + n) as char),
        Key::KeypadEnter => ('M', '\r'),
        Key::KeypadDecimal => ('n', '.'),
        Key::KeypadAdd => ('k', '+'),
        Key::KeypadSubtract => ('m', '-'),
        Key::KeypadMultiply => ('j', '*'),
        Key::KeypadDivide => ('o', '/'),
        _ => return None,
    })
}

fn kitty_key(event: &KeyEvent, flags: u8, options: KeyEncodeOptions) -> Vec<u8> {
    let all = flags & 8 != 0;
    let report_events = flags & 2 != 0;
    let modifier_key = matches!(
        event.key,
        Key::Shift
            | Key::Control
            | Key::Alt
            | Key::Super
            | Key::ShiftRight
            | Key::ControlRight
            | Key::AltRight
            | Key::SuperRight
            | Key::CapsLock
            | Key::NumLock
    );
    if event.action == KeyAction::Release
        && (!report_events || !all && matches!(event.key, Key::Enter | Key::Tab | Key::Backspace))
        || event.composing && !modifier_key
    {
        return Vec::new();
    }
    let text = event.text.as_deref().unwrap_or("");
    if !text.is_empty()
        && !is_control_text(text)
        && matches!(event.key, Key::Enter | Key::Backspace)
    {
        return if event.key == Key::Backspace {
            Vec::new()
        } else {
            text.as_bytes().to_vec()
        };
    }
    if !all && event.effective_modifiers().number() == 1 {
        match event.key {
            Key::Enter => return vec![b'\r'],
            Key::Tab => return vec![b'\t'],
            Key::Backspace => return vec![0x7f],
            _ => {}
        }
        if event.action != KeyAction::Release
            && !text.is_empty()
            && text.chars().all(|cp| !is_control(cp))
        {
            return text.as_bytes().to_vec();
        }
    }
    if modifier_key && !all {
        return Vec::new();
    }
    let code = match event.key {
        Key::Enter => Some((13, 'u')),
        Key::Tab => Some((9, 'u')),
        Key::Backspace => Some((127, 'u')),
        Key::Escape => Some((27, 'u')),
        Key::Up => Some((1, 'A')),
        Key::Down => Some((1, 'B')),
        Key::Right => Some((1, 'C')),
        Key::Left => Some((1, 'D')),
        Key::Home => Some((1, 'H')),
        Key::End => Some((1, 'F')),
        Key::CapsLock => Some((57358, 'u')),
        Key::ScrollLock => Some((57359, 'u')),
        Key::NumLock => Some((57360, 'u')),
        Key::PrintScreen => Some((57361, 'u')),
        Key::Pause => Some((57362, 'u')),
        Key::Function(f @ 13..=25) => Some((57376 + u32::from(f - 13), 'u')),
        Key::Keypad(n @ 0..=9) => Some((57399 + u32::from(n), 'u')),
        Key::KeypadDecimal => Some((57409, 'u')),
        Key::KeypadDivide => Some((57410, 'u')),
        Key::KeypadMultiply => Some((57411, 'u')),
        Key::KeypadSubtract => Some((57412, 'u')),
        Key::KeypadAdd => Some((57413, 'u')),
        Key::KeypadEnter => Some((57414, 'u')),
        Key::KeypadEqual => Some((57415, 'u')),
        Key::KeypadSeparator => Some((57416, 'u')),
        Key::KeypadLeft => Some((57417, 'u')),
        Key::KeypadRight => Some((57418, 'u')),
        Key::KeypadUp => Some((57419, 'u')),
        Key::KeypadDown => Some((57420, 'u')),
        Key::KeypadPageUp => Some((57421, 'u')),
        Key::KeypadPageDown => Some((57422, 'u')),
        Key::KeypadHome => Some((57423, 'u')),
        Key::KeypadEnd => Some((57424, 'u')),
        Key::KeypadInsert => Some((57425, 'u')),
        Key::KeypadDelete => Some((57426, 'u')),
        Key::KeypadBegin => Some((57427, 'u')),
        Key::Shift => Some((57441, 'u')),
        Key::Control => Some((57442, 'u')),
        Key::Alt => Some((57443, 'u')),
        Key::Super => Some((57444, 'u')),
        Key::ShiftRight => Some((57447, 'u')),
        Key::ControlRight => Some((57448, 'u')),
        Key::AltRight => Some((57449, 'u')),
        Key::SuperRight => Some((57450, 'u')),
        Key::Insert | Key::Delete | Key::PageUp | Key::PageDown | Key::Function(1..=12) => {
            functional(event.key)
        }
        _ => event
            .unshifted
            .filter(|&cp| cp != '\0')
            .map(|cp| (cp as u32, 'u')),
    };
    let Some((code, final_byte)) = code else {
        return if event.action == KeyAction::Release {
            Vec::new()
        } else {
            text.as_bytes().to_vec()
        };
    };
    let mods = event.modifiers.kitty_number();
    let event_number = match event.action {
        KeyAction::Press => 1,
        KeyAction::Repeat => 2,
        KeyAction::Release => 3,
    };
    if !matches!(final_byte, 'u' | '~') {
        return if report_events {
            format!("\x1b[1;{mods}:{event_number}{final_byte}")
        } else if mods > 1 {
            format!("\x1b[1;{mods}{final_byte}")
        } else {
            format!("\x1b[{final_byte}")
        }
        .into_bytes();
    }
    let mut output = format!("\x1b[{code}");
    if flags & 4 != 0 && code >= 32 && code != 127 {
        let mut chars = text.chars();
        let first = chars.next();
        let has_second = chars.next().is_some();
        let shifted = first.filter(|&cp| event.modifiers.shift && cp as u32 != code);
        let base = physical_codepoint(event.key).filter(|&cp| {
            cp as u32 != code && first.is_none_or(|first| first != cp && !has_second)
        });
        if let Some(cp) = shifted {
            output.push_str(&format!(":{}", cp as u32));
        }
        if let Some(cp) = base {
            if shifted.is_none() {
                output.push(':');
            }
            output.push_str(&format!(":{}", cp as u32));
        }
    }
    let has_modifiers = mods != 1 || report_events && event_number != 1;
    if has_modifiers {
        output.push_str(&format!(";{mods}"));
        if report_events && event_number != 1 {
            output.push_str(&format!(":{event_number}"));
        }
    }
    if flags & 16 != 0
        && event.action != KeyAction::Release
        && !event.modifiers.control
        && !(event.modifiers.alt && (!cfg!(target_os = "macos") || options.macos_option_as_alt))
        && !event.modifiers.super_key
    {
        let mut chars = text.chars().filter(|&cp| !is_control(cp)).peekable();
        if chars.peek().is_some() {
            if !has_modifiers {
                output.push(';');
            }
            output.push(';');
            for (index, cp) in chars.enumerate() {
                if index != 0 {
                    output.push(':');
                }
                output.push_str(&(cp as u32).to_string());
            }
        }
    }
    output.push(final_byte);
    output.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paste_preserves_binary_text_and_checks_safety_before_encoding() {
        let mut terminal = Terminal::new(80, 24, 0);
        let bytes = b"\xff\n\0\xfe";
        assert!(!terminal.paste_is_safe(bytes));
        assert_eq!(terminal.encode_paste(bytes), b"\xff\r \xfe");
        terminal.feed(b"\x1b[?2004h");
        assert!(terminal.paste_is_safe(bytes));
        assert!(!paste_is_safe(bytes, false));
        assert!(!terminal.paste_is_safe(b"\xff\x1b[201~\xfe"));
        assert_eq!(
            terminal.encode_paste(bytes),
            b"\x1b[200~\xff\n \xfe\x1b[201~"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn option_text_and_terminal_alt_have_distinct_legacy_and_kitty_output() {
        let mut terminal = Terminal::new(80, 24, 0);
        let mut event = KeyEvent::new(Key::Char('['));
        event.text = Some("{".into());
        event.modifiers.alt = true;
        let option = KeyEncodeOptions::default();
        assert_eq!(terminal.encode_key_with_options(&event, option), b"{");
        assert_eq!(terminal.encode_key(&event), b"\x1b{");
        terminal.feed(b"\x1b[>24u");
        assert_eq!(
            terminal.encode_key_with_options(&event, option),
            b"\x1b[91;3;123u"
        );
        assert_eq!(terminal.encode_key(&event), b"\x1b[91;3u");
    }

    #[test]
    fn key_modes_and_ime() {
        let mut t = Terminal::new(80, 24, 10);
        let mut key = KeyEvent::new(Key::Up);
        assert_eq!(t.encode_key(&key), b"\x1b[A");
        t.feed(b"\x1b[?1h");
        assert_eq!(t.encode_key(&key), b"\x1bOA");
        key.modifiers.control = true;
        assert_eq!(t.encode_key(&key), b"\x1b[1;5A");
        let mut key = KeyEvent::new(Key::Char('c'));
        key.modifiers.control = true;
        assert_eq!(t.encode_key(&key), [3]);
        t.feed(b"\x1b[>4;2m");
        assert_eq!(t.encode_key(&key), b"\x1b[27;5;99~");
        key.composing = true;
        assert!(t.encode_key(&key).is_empty());
        let mut key = KeyEvent::new(Key::Enter);
        key.text = Some("日本".into());
        assert_eq!(t.encode_key(&key), "日本".as_bytes());
    }
    #[test]
    fn kitty_reports_modifiers_repeats_release_and_associated_text() {
        let mut t = Terminal::new(80, 24, 10);
        t.feed(b"\x1b[>31u");
        let mut key = KeyEvent::new(Key::Char('a'));
        key.modifiers.control = true;
        assert_eq!(t.encode_key(&key), b"\x1b[97;5u");
        key.action = KeyAction::Repeat;
        assert_eq!(t.encode_key(&key), b"\x1b[97;5:2u");
        key.action = KeyAction::Release;
        assert_eq!(t.encode_key(&key), b"\x1b[97;5:3u");
        key = KeyEvent::new(Key::Char('a'));
        assert_eq!(t.encode_key(&key), b"\x1b[97;;97u");
        t.feed(b"\x1b[<u");
        assert_eq!(t.encode_key(&key), b"a");
    }
    #[test]
    fn keyboard_consumption_and_control_distinctions() {
        let mut t = Terminal::new(80, 24, 10);
        let mut key = KeyEvent::new(Key::Char('i'));
        key.modifiers.control = true;
        assert_eq!(t.encode_key(&key), b"\x1b[105;5u");
        key = KeyEvent::new(Key::Char('a'));
        key.text = Some("A".into());
        key.modifiers.shift = true;
        key.modifiers.control = true;
        assert_eq!(t.encode_key(&key), b"\x1b[97;6u");
        key.modifiers.control = false;
        key.consumed_modifiers.shift = true;
        t.feed(b"\x1b[>3u");
        assert_eq!(t.encode_key(&key), b"A");
        t.feed(b"\x1b[>24u");
        assert_eq!(t.encode_key(&key), b"\x1b[97;2;65u");
        key = KeyEvent::new(Key::Up);
        t.feed(b"\x1b[>3u");
        assert_eq!(t.encode_key(&key), b"\x1b[1;1:1A");
    }
    #[test]
    fn paste_frames_strip_unsafe_bytes_and_preserve_utf8() {
        let mut t = Terminal::new(80, 24, 10);
        assert!(!t.paste_is_safe("echo hi\n"));
        assert_eq!(t.encode_paste("é\x1b[201~\n"), "é [201~\r".as_bytes());
        t.feed(b"\x1b[?2004h");
        assert!(t.paste_is_safe("echo hi\n"));
        assert!(!t.paste_is_safe("\x1b[201~"));
        assert_eq!(t.encode_paste("a\nb"), b"\x1b[200~a\nb\x1b[201~");
    }
    fn mouse_options() -> MouseEncodeOptions<'static> {
        MouseEncodeOptions {
            screen_size: [640.0, 384.0],
            cell_size: [8, 16],
            padding: [0.0; 4],
            any_button_pressed: true,
            last_cell: None,
        }
    }

    #[test]
    fn mouse_protocol_and_focus() {
        let mut t = Terminal::new(80, 24, 10);
        let mut e = MouseEvent {
            action: MouseAction::Press,
            button: Some(MouseButton::Left),
            x: 24.0,
            y: 64.0,
            modifiers: Modifiers::default(),
        };
        assert!(t.encode_mouse(e, mouse_options()).is_empty());
        t.feed(b"\x1b[?1000h\x1b[?1006h\x1b[?1004h");
        assert_eq!(t.encode_mouse(e, mouse_options()), b"\x1b[<0;4;5M");
        e.action = MouseAction::Release;
        assert_eq!(t.encode_mouse(e, mouse_options()), b"\x1b[<0;4;5m");
        e.action = MouseAction::Move;
        assert!(t.encode_mouse(e, mouse_options()).is_empty());
        t.feed(b"\x1b[?1003h\x1b[?1016h");
        assert_eq!(t.encode_mouse(e, mouse_options()), b"\x1b[<32;24;64M");
        assert_eq!(t.encode_focus(false), b"\x1b[O");
    }
    #[test]
    fn mouse_drag_bounds_pixel_rounding_and_extra_buttons() {
        let mut t = Terminal::new(80, 24, 10);
        t.set_pixel_size(640, 384);
        t.feed(b"\x1b[?1000h\x1b[?1016h");
        let mut event = MouseEvent {
            action: MouseAction::Press,
            button: Some(MouseButton::Left),
            x: -0.25,
            y: 0.0,
            modifiers: Modifiers::default(),
        };
        assert!(t.encode_mouse(event, mouse_options()).is_empty());
        event.action = MouseAction::Release;
        event.x = -1.5;
        event.y = 400.25;
        assert_eq!(t.encode_mouse(event, mouse_options()), b"\x1b[<0;-2;400m");
        t.feed(b"\x1b[?1003h\x1b[?1005h");
        event.action = MouseAction::Press;
        event.button = Some(MouseButton::Extra(0));
        event.x = 0.0;
        event.y = 0.0;
        assert_eq!(
            t.encode_mouse(event, mouse_options()),
            [0x1b, b'[', b'M', 160, 33, 33]
        );
        t.feed(b"\x1b[?9h");
        event.button = Some(MouseButton::WheelUp);
        assert!(t.encode_mouse(event, mouse_options()).is_empty());
    }

    #[test]
    fn mouse_tracking_uses_padded_cells_and_keeps_pixel_motion() {
        let mut terminal = Terminal::new(80, 24, 0);
        terminal.feed(b"\x1b[?1003h\x1b[?1006h");
        let mut last = None;
        let mut event = MouseEvent {
            action: MouseAction::Move,
            button: None,
            x: 12.0,
            y: 20.0,
            modifiers: Modifiers::default(),
        };
        let encode = |terminal: &Terminal, event, last: &mut _| {
            terminal.encode_mouse(
                event,
                MouseEncodeOptions {
                    padding: [10.0, 20.0, 10.0, 20.0],
                    last_cell: Some(last),
                    any_button_pressed: false,
                    ..mouse_options()
                },
            )
        };
        assert_eq!(encode(&terminal, event, &mut last), b"\x1b[<35;1;1M");
        event.x += 1.0;
        assert!(encode(&terminal, event, &mut last).is_empty());
        // Even an unsupported button updates the retained cell before the
        // native encoder decides whether it can produce bytes.
        event.x = 20.0;
        event.action = MouseAction::Press;
        event.button = Some(MouseButton::Extra(2));
        assert!(encode(&terminal, event, &mut last).is_empty());
        assert_eq!(last, Some([1, 0]));
        event.action = MouseAction::Move;
        event.button = None;
        assert!(encode(&terminal, event, &mut last).is_empty());
        terminal.feed(b"\x1b[?1016h");
        for _ in 0..2 {
            assert_eq!(encode(&terminal, event, &mut last), b"\x1b[<35;10;0M");
        }
        event.x = -1.0;
        assert!(encode(&terminal, event, &mut last).is_empty());
        event.action = MouseAction::Release;
        event.button = Some(MouseButton::Left);
        assert_eq!(encode(&terminal, event, &mut last), b"\x1b[<0;-11;0m");

        terminal.feed(b"\x1b[?1006h");
        event.x = 640.5;
        event.y = 384.5;
        assert_eq!(
            terminal.encode_mouse(
                event,
                MouseEncodeOptions {
                    screen_size: [640.5, 384.5],
                    padding: [0.5, 0.5, 0.0, 0.0],
                    ..mouse_options()
                }
            ),
            b"\x1b[<0;80;24m"
        );
    }
}
