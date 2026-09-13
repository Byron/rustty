//! Ghostty-compatible keybinding syntax, independent of the window toolkit.

use super::parse_positive;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub super_key: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyTrigger {
    pub modifiers: Modifiers,
    /// A Unicode character or a Ghostty key name, such as `arrow_left`.
    pub key: String,
    pub physical: bool,
}

impl KeyTrigger {
    pub fn parse(input: &str) -> Result<Self, &'static str> {
        let (physical, input) = input
            .strip_prefix("physical:")
            .map_or((false, input), |s| (true, s));
        let mut modifiers = Modifiers::default();
        let mut key = None;
        // A trailing doubled plus is the literal plus key, not an empty token.
        let input = if input == "+" {
            key = Some("+".to_owned());
            ""
        } else if let Some(input) = input.strip_suffix("++") {
            key = Some("+".to_owned());
            input
        } else {
            input
        };
        if !input.is_empty() && input.split('+').any(str::is_empty) {
            return Err("empty key or modifier");
        }
        for part in input.split('+').filter(|s| !s.is_empty()) {
            let flag = match part {
                "shift" => Some(&mut modifiers.shift),
                "ctrl" | "control" => Some(&mut modifiers.control),
                "alt" | "opt" | "option" => Some(&mut modifiers.alt),
                "super" | "cmd" | "command" => Some(&mut modifiers.super_key),
                _ => None,
            };
            if let Some(flag) = flag {
                if *flag {
                    return Err("duplicate key modifier");
                }
                *flag = true;
            } else {
                if key.is_some() {
                    return Err("a trigger must contain exactly one key");
                }
                let normalized = match part {
                    "left" => "arrow_left",
                    "right" => "arrow_right",
                    "up" => "arrow_up",
                    "down" => "arrow_down",
                    "return" => "enter",
                    "esc" => "escape",
                    "plus" => "+",
                    "equal" => "=",
                    "minus" => "-",
                    "comma" => ",",
                    "period" => ".",
                    "slash" => "/",
                    "backslash" => "\\",
                    "semicolon" => ";",
                    "quote" => "'",
                    "bracket_left" => "[",
                    "bracket_right" => "]",
                    other => other,
                };
                if !valid_key(normalized) {
                    return Err("unknown key name");
                }
                key = Some(normalized.to_owned());
            }
        }
        Ok(Self {
            modifiers,
            key: key.ok_or("key is missing")?,
            physical,
        })
    }
}

fn valid_key(key: &str) -> bool {
    key.chars().count() == 1
        || matches!(
            key,
            "arrow_left"
                | "arrow_right"
                | "arrow_up"
                | "arrow_down"
                | "enter"
                | "escape"
                | "backspace"
                | "delete"
                | "tab"
                | "space"
                | "backquote"
                | "insert"
                | "home"
                | "end"
                | "page_up"
                | "page_down"
                | "caps_lock"
                | "num_lock"
                | "scroll_lock"
                | "print_screen"
                | "pause"
                | "copy"
                | "paste"
                | "cut"
                | "catch_all"
                | "kp_add"
                | "kp_subtract"
                | "kp_multiply"
                | "kp_divide"
                | "kp_decimal"
                | "kp_enter"
                | "kp_equal"
        )
        || key
            .strip_prefix('f')
            .is_some_and(|n| n.parse::<u8>().is_ok_and(|n| (1..=35).contains(&n)))
        || key
            .strip_prefix("digit_")
            .is_some_and(|n| n.len() == 1 && n.as_bytes()[0].is_ascii_digit())
        || key
            .strip_prefix("key_")
            .is_some_and(|n| n.len() == 1 && n.as_bytes()[0].is_ascii_lowercase())
        || key
            .strip_prefix("kp_")
            .is_some_and(|n| n.len() == 1 && n.as_bytes()[0].is_ascii_digit())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
    Previous,
    Next,
    QuadrantLeft,
    QuadrantRight,
    QuadrantUp,
    QuadrantDown,
}

impl Direction {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "up" | "top" => Ok(Self::Up),
            "down" | "bottom" => Ok(Self::Down),
            "previous" => Ok(Self::Previous),
            "next" => Ok(Self::Next),
            "quadrant_left" => Ok(Self::QuadrantLeft),
            "quadrant_right" => Ok(Self::QuadrantRight),
            "quadrant_up" => Ok(Self::QuadrantUp),
            "quadrant_down" => Ok(Self::QuadrantDown),
            _ => Err("invalid split direction"),
        }
    }

    fn cardinal(value: &str) -> Result<Self, &'static str> {
        match Self::parse(value)? {
            d @ (Self::Left | Self::Right | Self::Up | Self::Down) => Ok(d),
            _ => Err("a cardinal split direction is required"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Ignore,
    Unbind,
    Text(Vec<u8>),
    NewWindow,
    NewTab,
    CloseSurface,
    CloseTab,
    CloseWindow,
    CloseAllWindows,
    Quit,
    NewSplit(Direction),
    GotoSplit(Direction),
    ResizeSplit { direction: Direction, amount: u16 },
    EqualizeSplits,
    ToggleSplitZoom,
    ToggleQuadrantZoom,
    ToggleQuickTerminal,
    ToggleFullscreen,
    ToggleCommandPalette,
    NextTab,
    PreviousTab,
    LastTab,
    GotoTab(usize),
    MoveTab(i32),
    CopyToClipboard,
    PasteFromClipboard,
    PasteFromSelection,
    SelectAll,
    ClearScreen,
    StartSearch,
    EndSearch,
    SearchSelection,
    NavigateSearch { next: bool },
    ReloadConfig,
    OpenConfig,
    IncreaseFontSize(f32),
    DecreaseFontSize(f32),
    ResetFontSize,
    ScrollToTop,
    ScrollToBottom,
    ScrollToSelection,
    ScrollPageUp,
    ScrollPageDown,
    JumpToPrompt(i32),
    Undo,
    Redo,
}

impl Action {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        let (name, argument) = value
            .split_once(':')
            .map_or((value, None), |(n, a)| (n, Some(a)));
        let required = || argument.ok_or("keybinding action requires an argument");
        let action = match name {
            "text" => Self::Text(parse_escaped_bytes(required()?)?),
            "csi" => Self::Text([b"\x1b[".as_slice(), required()?.as_bytes()].concat()),
            "esc" => Self::Text([b"\x1b".as_slice(), required()?.as_bytes()].concat()),
            "new_split" => Self::NewSplit(Direction::cardinal(required()?)?),
            "goto_split" => Self::GotoSplit(Direction::parse(required()?)?),
            "resize_split" => {
                let (direction, amount) = required()?
                    .split_once(',')
                    .ok_or("resize_split requires direction,amount")?;
                Self::ResizeSplit {
                    direction: Direction::cardinal(direction)?,
                    amount: amount
                        .parse::<u16>()
                        .ok()
                        .filter(|n| *n > 0)
                        .ok_or("resize amount must be positive")?,
                }
            }
            "goto_tab" => Self::GotoTab(
                required()?
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or("tab index must be positive")?,
            ),
            "move_tab" => Self::MoveTab(required()?.parse().map_err(|_| "invalid tab offset")?),
            "jump_to_prompt" => {
                Self::JumpToPrompt(required()?.parse().map_err(|_| "invalid prompt offset")?)
            }
            "navigate_search" => Self::NavigateSearch {
                next: match required()? {
                    "next" => true,
                    "previous" => false,
                    _ => return Err("invalid search direction"),
                },
            },
            "increase_font_size" => {
                Self::IncreaseFontSize(parse_positive(argument.unwrap_or("1"))?)
            }
            "decrease_font_size" => {
                Self::DecreaseFontSize(parse_positive(argument.unwrap_or("1"))?)
            }
            "copy_to_clipboard" if argument.is_none() || argument == Some("mixed") => {
                Self::CopyToClipboard
            }
            "open_config" if argument.is_none() || argument == Some("default") => Self::OpenConfig,
            "close_tab" if argument.is_none() || argument == Some("this") => Self::CloseTab,
            _ if argument.is_some() => return Err("unsupported keybinding action or argument"),
            "ignore" => Self::Ignore,
            "unbind" => Self::Unbind,
            "new_window" => Self::NewWindow,
            "new_tab" => Self::NewTab,
            "close_surface" => Self::CloseSurface,
            "close_window" => Self::CloseWindow,
            "close_all_windows" => Self::CloseAllWindows,
            "quit" => Self::Quit,
            "equalize_splits" => Self::EqualizeSplits,
            "toggle_split_zoom" => Self::ToggleSplitZoom,
            "toggle_quadrant_zoom" => Self::ToggleQuadrantZoom,
            "toggle_quick_terminal" => Self::ToggleQuickTerminal,
            "toggle_fullscreen" => Self::ToggleFullscreen,
            "toggle_command_palette" => Self::ToggleCommandPalette,
            "next_tab" => Self::NextTab,
            "previous_tab" => Self::PreviousTab,
            "last_tab" => Self::LastTab,
            "paste_from_clipboard" => Self::PasteFromClipboard,
            "paste_from_selection" => Self::PasteFromSelection,
            "select_all" => Self::SelectAll,
            "clear_screen" => Self::ClearScreen,
            "start_search" => Self::StartSearch,
            "end_search" => Self::EndSearch,
            "search_selection" => Self::SearchSelection,
            "reload_config" => Self::ReloadConfig,
            "reset_font_size" => Self::ResetFontSize,
            "scroll_to_top" => Self::ScrollToTop,
            "scroll_to_bottom" => Self::ScrollToBottom,
            "scroll_to_selection" => Self::ScrollToSelection,
            "scroll_page_up" => Self::ScrollPageUp,
            "scroll_page_down" => Self::ScrollPageDown,
            "undo" => Self::Undo,
            "redo" => Self::Redo,
            _ => return Err("unsupported keybinding action"),
        };
        Ok(action)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BindingFlags {
    pub global: bool,
    pub all: bool,
    pub consumed: bool,
    pub performable: bool,
}

impl Default for BindingFlags {
    fn default() -> Self {
        Self {
            global: false,
            all: false,
            consumed: true,
            performable: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyBinding {
    pub trigger: Vec<KeyTrigger>,
    pub flags: BindingFlags,
    pub actions: Vec<Action>,
    pub table: Option<String>,
}

impl KeyBinding {
    pub fn parse(input: &str) -> Result<Self, &'static str> {
        let mut split = input
            .find('=')
            .ok_or("keybinding requires trigger=action")?;
        if (split == 0 || input[..split].ends_with('+')) && input[split + 1..].starts_with('=') {
            split += 1;
        }
        let mut trigger = &input[..split];
        let mut table = None;
        if let Some((name, rest)) = trigger.split_once('/')
            && !name.is_empty()
            && !name.contains(['+', '>'])
        {
            table = Some(name.to_owned());
            trigger = rest;
        }
        let mut flags = BindingFlags::default();
        let mut seen = [false; 4];
        while let Some((prefix, rest)) = trigger.split_once(':') {
            let index = match prefix {
                "global" => 0,
                "all" => 1,
                "unconsumed" => 2,
                "performable" => 3,
                _ => break,
            };
            if seen[index] {
                return Err("duplicate keybinding flag");
            }
            seen[index] = true;
            match index {
                0 => {
                    flags.global = true;
                    flags.all = true;
                }
                1 => flags.all = true,
                2 => flags.consumed = false,
                3 => flags.performable = true,
                _ => unreachable!(),
            }
            trigger = rest;
        }
        let trigger = trigger
            .split('>')
            .map(KeyTrigger::parse)
            .collect::<Result<Vec<_>, _>>()?;
        if trigger.len() > 1 && flags.all {
            return Err("global and all bindings cannot use key sequences");
        }
        if flags.all {
            flags.consumed = true;
        }
        Ok(Self {
            trigger,
            flags,
            actions: vec![Action::parse(&input[split + 1..])?],
            table,
        })
    }
}

pub(super) fn apply(
    bindings: &mut Vec<KeyBinding>,
    chain_target: &mut Option<usize>,
    value: &str,
) -> Result<(), &'static str> {
    if value.is_empty() {
        *bindings = defaults();
        *chain_target = None;
    } else if value == "clear" {
        bindings.clear();
        *chain_target = None;
    } else if let Some(action) = value.strip_prefix("chain=") {
        let action = Action::parse(action)?;
        if action == Action::Unbind {
            return Err("unbind cannot be chained");
        }
        bindings
            .get_mut(chain_target.ok_or("chain requires a preceding binding")?)
            .ok_or("chain requires a preceding binding")?
            .actions
            .push(action);
    } else if let Some(table) = value.strip_suffix('/')
        && !table.is_empty()
        && !table.contains(['=', '+', '>'])
    {
        bindings.retain(|b| b.table.as_deref() != Some(table));
        *chain_target = None;
    } else {
        let binding = KeyBinding::parse(value)?;
        // Replacing a prefix removes its old sequence; a new sequence replaces
        // a scalar binding on its prefix. Sibling sequences remain independent.
        bindings.retain(|b| {
            !(b.table == binding.table
                && (b.trigger.starts_with(&binding.trigger)
                    || binding.trigger.starts_with(&b.trigger)))
        });
        if binding.actions != [Action::Unbind] {
            bindings.push(binding);
            *chain_target = Some(bindings.len() - 1);
        } else {
            *chain_target = None;
        }
    }
    Ok(())
}

pub(super) fn defaults() -> Vec<KeyBinding> {
    [
        "super+n=new_window",
        "super+t=new_tab",
        "super+w=close_surface",
        "super+alt+w=close_tab",
        "super+shift+w=close_window",
        "super+alt+shift+w=close_all_windows",
        "super+q=quit",
        "performable:super+c=copy_to_clipboard",
        "performable:super+v=paste_from_clipboard",
        "super+shift+v=paste_from_selection",
        "super+a=select_all",
        "performable:super+k=clear_screen",
        "super+d=new_split:right",
        "super+shift+d=new_split:down",
        "super+[=goto_split:previous",
        "super+]=goto_split:next",
        "super+alt+arrow_left=goto_split:left",
        "super+alt+arrow_right=goto_split:right",
        "super+alt+arrow_up=goto_split:up",
        "super+alt+arrow_down=goto_split:down",
        "super+ctrl+arrow_left=resize_split:left,10",
        "super+ctrl+arrow_right=resize_split:right,10",
        "super+ctrl+arrow_up=resize_split:up,10",
        "super+ctrl+arrow_down=resize_split:down,10",
        "super+ctrl+equal=equalize_splits",
        "super+enter=toggle_fullscreen",
        "super+ctrl+f=toggle_fullscreen",
        "super+shift+enter=toggle_split_zoom",
        "ctrl+tab=next_tab",
        "ctrl+shift+tab=previous_tab",
        "super+shift+[=previous_tab",
        "super+shift+]=next_tab",
        "super+1=goto_tab:1",
        "super+2=goto_tab:2",
        "super+3=goto_tab:3",
        "super+4=goto_tab:4",
        "super+5=goto_tab:5",
        "super+6=goto_tab:6",
        "super+7=goto_tab:7",
        "super+8=goto_tab:8",
        "super+9=last_tab",
        "performable:super+f=start_search",
        "performable:super+e=search_selection",
        "performable:super+shift+f=end_search",
        "performable:escape=end_search",
        "performable:super+g=navigate_search:next",
        "performable:super+shift+g=navigate_search:previous",
        "super+shift+p=toggle_command_palette",
        "super+,=open_config",
        "super+shift+,=reload_config",
        "super+equal=increase_font_size:1",
        "super+plus=increase_font_size:1",
        "super+-=decrease_font_size:1",
        "super+0=reset_font_size",
        "super+home=scroll_to_top",
        "super+end=scroll_to_bottom",
        "super+page_up=scroll_page_up",
        "super+page_down=scroll_page_down",
        "performable:super+j=scroll_to_selection",
        "super+arrow_up=jump_to_prompt:-1",
        "super+arrow_down=jump_to_prompt:1",
        "super+shift+arrow_up=jump_to_prompt:-1",
        "super+shift+arrow_down=jump_to_prompt:1",
        "performable:super+z=undo",
        "performable:super+shift+z=redo",
        "performable:super+shift+t=undo",
        "super+arrow_left=text:\\x01",
        "super+arrow_right=text:\\x05",
        "super+backspace=text:\\x15",
        "alt+arrow_left=esc:b",
        "alt+arrow_right=esc:f",
    ]
    .into_iter()
    .map(|s| KeyBinding::parse(s).expect("valid built-in binding"))
    .collect()
}

/// `text:` uses Zig string escapes. Hex escapes represent bytes, not codepoints.
pub fn parse_escaped_bytes(input: &str) -> Result<Vec<u8>, &'static str> {
    let mut output = Vec::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            output.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
            continue;
        }
        match chars.next().ok_or("unfinished string escape")? {
            'n' => output.push(b'\n'),
            'r' => output.push(b'\r'),
            't' => output.push(b'\t'),
            '\\' => output.push(b'\\'),
            '"' => output.push(b'"'),
            '\'' => output.push(b'\''),
            'x' => {
                let a = chars
                    .next()
                    .and_then(|c| c.to_digit(16))
                    .ok_or("invalid hex escape")?;
                let b = chars
                    .next()
                    .and_then(|c| c.to_digit(16))
                    .ok_or("invalid hex escape")?;
                output.push((a * 16 + b) as u8);
            }
            'u' => {
                if chars.next() != Some('{') {
                    return Err("Unicode escape requires braces");
                }
                let mut n = 0u32;
                let mut digits = 0;
                loop {
                    let c = chars.next().ok_or("unfinished Unicode escape")?;
                    if c == '}' {
                        break;
                    }
                    let digit = c.to_digit(16).ok_or("invalid Unicode escape")?;
                    n = n
                        .checked_mul(16)
                        .and_then(|n| n.checked_add(digit))
                        .ok_or("Unicode escape overflow")?;
                    digits += 1;
                }
                if digits == 0 {
                    return Err("empty Unicode escape");
                }
                let c = char::from_u32(n).ok_or("invalid Unicode codepoint")?;
                output.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
            }
            _ => return Err("invalid string escape"),
        }
    }
    Ok(output)
}
