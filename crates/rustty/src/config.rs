//! Read Rustty settings, falling back to Ghostty without changing either file.
//!
//! Configuration uses Ghostty's `key = value` syntax. Sources are selected as
//! one family; an existing Rustty file, even an empty or invalid one, disables
//! Ghostty fallback. Includes are applied after root files and CLI options.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::env;
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

#[path = "config/font.rs"]
mod font;
#[path = "config/keybinding.rs"]
mod keybinding;
pub use font::{CodepointMap, FontStyleRequest, FontVariation};
pub use keybinding::{
    Action, BindingFlags, Direction, KeyBinding, KeyTrigger, Modifiers, parse_escaped_bytes,
};

/// The maximum size of an individual config/theme file. Includes also have a
/// finite count so a configuration cannot exhaust memory through a huge graph.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FILES: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn parse(value: &str) -> Result<Self, &'static str> {
        rustty_vt::parse_color(value)
            .map(|[r, g, b]| Self::new(r, g, b))
            .ok_or("invalid color; expected hex, rgb:, rgbi: or an X11 color")
    }

    pub fn to_array(self) -> [u8; 3] {
        [self.r, self.g, self.b]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalColor {
    Rgb(Rgb),
    CellForeground,
    CellBackground,
}

impl TerminalColor {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "cell-foreground" => Ok(Self::CellForeground),
            "cell-background" => Ok(Self::CellBackground),
            _ => Rgb::parse(value).map(Self::Rgb),
        }
    }

    pub fn resolve(self, foreground: Rgb, background: Rgb) -> Rgb {
        match self {
            Self::Rgb(c) => c,
            Self::CellForeground => foreground,
            Self::CellBackground => background,
        }
    }
}

macro_rules! config_enum {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name { $($variant),+ }
        impl $name {
            pub fn parse(value: &str) -> Result<Self, &'static str> {
                match value { $($text => Ok(Self::$variant),)+ _ => Err(concat!("invalid ", stringify!($name), " value")) }
            }
        }
    };
}

config_enum!(CursorStyle { Block => "block", Bar => "bar", Underline => "underline", BlockHollow => "block_hollow" });
config_enum!(CopyOnSelect { None => "none", Primary => "primary", Clipboard => "clipboard", Both => "both" });
config_enum!(WindowSaveState { Default => "default", Never => "never", Always => "always" });
config_enum!(OptionAsAlt { False => "false", True => "true", Left => "left", Right => "right" });
config_enum!(NotifyOnCommandFinish { Never => "never", Unfocused => "unfocused", Always => "always" });
config_enum!(ClipboardAccess { Allow => "allow", Deny => "deny", Ask => "ask" });
config_enum!(ConfirmCloseSurface { False => "false", True => "true", Always => "always" });
config_enum!(WindowTheme { Auto => "auto", System => "system", Light => "light", Dark => "dark" });
config_enum!(ShellIntegration { None => "none", Detect => "detect", Bash => "bash", Zsh => "zsh", Fish => "fish", Elvish => "elvish", Nushell => "nushell" });
config_enum!(QuickTerminalPosition { Top => "top", Bottom => "bottom", Left => "left", Right => "right", Center => "center" });
config_enum!(QuickTerminalScreen { Main => "main", Mouse => "mouse", MacosMenuBar => "macos-menu-bar" });
config_enum!(QuickTerminalSpaceBehavior { Move => "move", Remain => "remain" });

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Padding {
    pub start: f32,
    pub end: f32,
}

impl Padding {
    fn parse(value: &str) -> Result<Self, &'static str> {
        let (start, end) = value.split_once(',').unwrap_or((value, value));
        Ok(Self {
            start: parse_nonnegative(start.trim())?,
            end: parse_nonnegative(end.trim())?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BellFeatures {
    pub system: bool,
    pub audio: bool,
    pub attention: bool,
    pub title: bool,
    pub border: bool,
}

impl Default for BellFeatures {
    fn default() -> Self {
        Self {
            system: false,
            audio: false,
            attention: true,
            title: true,
            border: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotifyActions {
    pub bell: bool,
    pub notify: bool,
}

impl Default for NotifyActions {
    fn default() -> Self {
        Self {
            bell: true,
            notify: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Shell(String),
    Direct(Vec<String>),
}

impl Command {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        let value = value.trim();
        if value.is_empty() || value.contains('\0') {
            return Err("command must be nonempty and contain no NUL");
        }
        if let Some(value) = value.strip_prefix("direct:") {
            let value = value.trim();
            if value.is_empty() {
                return Err("direct command is empty");
            }
            // Ghostty's direct form deliberately splits on spaces without shell expansion.
            Ok(Self::Direct(value.split(' ').map(str::to_owned).collect()))
        } else {
            let value = value.strip_prefix("shell:").unwrap_or(value).trim();
            if value.is_empty() {
                return Err("shell command is empty");
            }
            Ok(Self::Shell(value.to_owned()))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub light: String,
    pub dark: String,
}

impl Theme {
    fn parse(value: &str) -> Result<Self, &'static str> {
        if !value.starts_with("light:") && !value.starts_with("dark:") {
            return Ok(Self {
                light: value.to_owned(),
                dark: value.to_owned(),
            });
        }
        let (mut light, mut dark) = (None, None);
        for part in value.split(',') {
            let (key, name) = part
                .trim()
                .split_once(':')
                .ok_or("invalid light/dark theme")?;
            if name.trim().is_empty() {
                return Err("theme name is empty");
            }
            let target = match key {
                "light" => &mut light,
                "dark" => &mut dark,
                _ => return Err("invalid light/dark theme"),
            };
            if target.replace(name.trim().to_owned()).is_some() {
                return Err("duplicate theme variant");
            }
        }
        Ok(Self {
            light: light.ok_or("light theme is required")?,
            dark: dark.ok_or("dark theme is required")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub font_family: Vec<String>,
    pub font_family_bold: Vec<String>,
    pub font_family_italic: Vec<String>,
    pub font_family_bold_italic: Vec<String>,
    pub font_feature: Vec<String>,
    pub font_size: f32,
    pub font_style: FontStyleRequest,
    pub font_style_bold: FontStyleRequest,
    pub font_style_italic: FontStyleRequest,
    pub font_style_bold_italic: FontStyleRequest,
    pub font_variation: Vec<FontVariation>,
    pub font_variation_bold: Vec<FontVariation>,
    pub font_variation_italic: Vec<FontVariation>,
    pub font_variation_bold_italic: Vec<FontVariation>,
    pub font_codepoint_map: Vec<CodepointMap>,
    /// Synthetic bold, italic and bold-italic are controlled independently.
    pub font_synthetic_style: [bool; 3],
    pub font_thicken: bool,
    /// Zero is the lightest thickening; `font_thicken` controls whether it is used.
    pub font_thicken_strength: u8,
    pub background: Rgb,
    pub foreground: Rgb,
    pub palette: [Rgb; 256],
    pub cursor_color: Option<TerminalColor>,
    pub cursor_text: Option<TerminalColor>,
    pub cursor_style: CursorStyle,
    pub cursor_style_blink: Option<bool>,
    pub selection_foreground: Option<TerminalColor>,
    pub selection_background: Option<TerminalColor>,
    pub background_opacity: f32,
    pub unfocused_split_opacity: f32,
    pub unfocused_split_fill: Option<Rgb>,
    pub split_divider_color: Option<Rgb>,
    pub quadrant_peek_opacity: f32,
    pub window_padding_x: Padding,
    pub window_padding_y: Padding,
    /// Initial dimensions in terminal cells; zero means use the app default.
    pub window_width: u32,
    pub window_height: u32,
    pub window_theme: WindowTheme,
    pub window_save_state: WindowSaveState,
    pub macos_option_as_alt: OptionAsAlt,
    pub copy_on_select: CopyOnSelect,
    pub clipboard_read: ClipboardAccess,
    pub clipboard_write: ClipboardAccess,
    pub clipboard_write_limit_bytes: Option<usize>,
    pub confirm_close_surface: ConfirmCloseSurface,
    pub wait_after_command: bool,
    pub abnormal_command_exit_runtime: u32,
    pub undo_timeout: Duration,
    pub quit_after_last_window_closed: bool,
    pub bell_features: BellFeatures,
    pub notify_on_command_finish: NotifyOnCommandFinish,
    pub notify_on_command_finish_action: NotifyActions,
    pub notify_on_command_finish_after: Duration,
    pub quick_terminal_animation_duration: Duration,
    pub quick_terminal_position: QuickTerminalPosition,
    pub quick_terminal_screen: QuickTerminalScreen,
    pub quick_terminal_space_behavior: QuickTerminalSpaceBehavior,
    pub quick_terminal_autohide: bool,
    pub shell_integration: ShellIntegration,
    pub scrollback_limit_bytes: Option<usize>,
    pub scrollback_limit_lines: Option<usize>,
    pub scrollback_compression: bool,
    pub link_url: bool,
    pub command: Option<Command>,
    pub initial_command: Option<Command>,
    /// `None` means inherit; `home` is resolved to the user's home directory.
    pub working_directory: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub theme: Option<Theme>,
    pub keybinds: Vec<KeyBinding>,
    keybind_chain: Option<usize>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_family: vec![],
            font_family_bold: vec![],
            font_family_italic: vec![],
            font_family_bold_italic: vec![],
            font_feature: vec![],
            font_size: 13.0,
            font_style: FontStyleRequest::Default,
            font_style_bold: FontStyleRequest::Default,
            font_style_italic: FontStyleRequest::Default,
            font_style_bold_italic: FontStyleRequest::Default,
            font_variation: vec![],
            font_variation_bold: vec![],
            font_variation_italic: vec![],
            font_variation_bold_italic: vec![],
            font_codepoint_map: vec![],
            font_synthetic_style: [true; 3],
            font_thicken: false,
            font_thicken_strength: 255,
            background: Rgb::new(0x28, 0x2c, 0x34),
            foreground: Rgb::new(255, 255, 255),
            palette: default_palette(),
            cursor_color: None,
            cursor_text: None,
            cursor_style: CursorStyle::Block,
            cursor_style_blink: None,
            selection_foreground: None,
            selection_background: None,
            background_opacity: 1.0,
            unfocused_split_opacity: 0.7,
            unfocused_split_fill: None,
            split_divider_color: None,
            quadrant_peek_opacity: 0.5,
            window_padding_x: Padding {
                start: 2.0,
                end: 2.0,
            },
            window_padding_y: Padding {
                start: 2.0,
                end: 2.0,
            },
            window_width: 0,
            window_height: 0,
            window_theme: WindowTheme::Auto,
            window_save_state: WindowSaveState::Default,
            macos_option_as_alt: OptionAsAlt::False,
            copy_on_select: CopyOnSelect::None,
            clipboard_read: ClipboardAccess::Ask,
            clipboard_write: ClipboardAccess::Allow,
            clipboard_write_limit_bytes: Some(64 * 1024 * 1024),
            confirm_close_surface: ConfirmCloseSurface::True,
            wait_after_command: false,
            abnormal_command_exit_runtime: 250,
            undo_timeout: Duration::from_secs(5),
            quit_after_last_window_closed: false,
            bell_features: BellFeatures::default(),
            notify_on_command_finish: NotifyOnCommandFinish::Never,
            notify_on_command_finish_action: NotifyActions::default(),
            notify_on_command_finish_after: Duration::from_secs(5),
            quick_terminal_animation_duration: Duration::from_millis(200),
            quick_terminal_position: QuickTerminalPosition::Top,
            quick_terminal_screen: QuickTerminalScreen::Mouse,
            quick_terminal_space_behavior: QuickTerminalSpaceBehavior::Move,
            quick_terminal_autohide: true,
            shell_integration: ShellIntegration::Detect,
            scrollback_limit_bytes: Some(50_000_000),
            scrollback_limit_lines: None,
            scrollback_compression: true,
            link_url: true,
            command: None,
            initial_command: None,
            working_directory: None,
            env: BTreeMap::new(),
            theme: None,
            keybinds: keybinding::defaults(),
            keybind_chain: None,
        }
    }
}

impl Config {
    pub fn load() -> io::Result<LoadedConfig> {
        Self::load_with_args(&[])
    }

    /// Pass arguments after argv[0]. `-e` consumes the remaining command argv.
    pub fn load_with_args(args: &[String]) -> io::Result<LoadedConfig> {
        Ok(ConfigLoader::from_env()?.load_with_args(args))
    }

    /// Find a single-key root binding. Sequence-aware hosts can inspect
    /// `keybinds`; returning references keeps event dispatch allocation-free.
    pub fn binding(&self, trigger: &KeyTrigger) -> Option<&KeyBinding> {
        self.keybinds.iter().rev().find(|binding| {
            binding.table.is_none() && binding.trigger.as_slice() == std::slice::from_ref(trigger)
        })
    }

    fn apply(&mut self, entry: &Entry, home: &Path) -> Result<(), &'static str> {
        let value = entry.value.as_str();
        macro_rules! set {
            ($field:ident, $expression:expr) => {{
                self.$field = if value.is_empty() {
                    Self::default().$field
                } else {
                    $expression
                };
            }};
        }
        match entry.key.as_str() {
            "font-family" => append_or_clear(&mut self.font_family, value),
            "font-family-bold" => append_or_clear(&mut self.font_family_bold, value),
            "font-family-italic" => append_or_clear(&mut self.font_family_italic, value),
            "font-family-bold-italic" => append_or_clear(&mut self.font_family_bold_italic, value),
            "font-feature" => append_or_clear(&mut self.font_feature, value),
            "font-size" => set!(font_size, parse_positive(value)?),
            "font-style" => set!(font_style, FontStyleRequest::parse(value)?),
            "font-style-bold" => set!(font_style_bold, FontStyleRequest::parse(value)?),
            "font-style-italic" => set!(font_style_italic, FontStyleRequest::parse(value)?),
            "font-style-bold-italic" => {
                set!(font_style_bold_italic, FontStyleRequest::parse(value)?)
            }
            "font-variation" => font::append_variation(&mut self.font_variation, value)?,
            "font-variation-bold" => font::append_variation(&mut self.font_variation_bold, value)?,
            "font-variation-italic" => {
                font::append_variation(&mut self.font_variation_italic, value)?
            }
            "font-variation-bold-italic" => {
                font::append_variation(&mut self.font_variation_bold_italic, value)?
            }
            "font-codepoint-map" => {
                if value.is_empty() {
                    self.font_codepoint_map.clear();
                } else {
                    self.font_codepoint_map.extend(CodepointMap::parse(value)?);
                }
            }
            "font-synthetic-style" => set!(font_synthetic_style, font::synthetic_styles(value)?),
            "font-thicken" => set!(font_thicken, parse_bool(value)?),
            "font-thicken-strength" => set!(
                font_thicken_strength,
                parse_usize(value)?
                    .try_into()
                    .map_err(|_| "font thickening strength must be 0 through 255")?
            ),
            "foreground" => set!(foreground, Rgb::parse(value)?),
            "background" => set!(background, Rgb::parse(value)?),
            "palette" => {
                if value.is_empty() {
                    self.palette = default_palette();
                } else {
                    let (index, color) = value
                        .split_once('=')
                        .ok_or("palette requires index=color")?;
                    let index = parse_usize(index.trim())?;
                    if index > 255 {
                        return Err("palette index must be 0 through 255");
                    }
                    self.palette[index] = Rgb::parse(color)?;
                }
            }
            "cursor-color" => set!(cursor_color, Some(TerminalColor::parse(value)?)),
            "cursor-text" => set!(cursor_text, Some(TerminalColor::parse(value)?)),
            "cursor-style" => set!(cursor_style, CursorStyle::parse(value)?),
            "cursor-style-blink" => set!(cursor_style_blink, Some(parse_bool(value)?)),
            "selection-foreground" => {
                set!(selection_foreground, Some(TerminalColor::parse(value)?))
            }
            "selection-background" => {
                set!(selection_background, Some(TerminalColor::parse(value)?))
            }
            "background-opacity" => set!(background_opacity, parse_opacity(value)?),
            "unfocused-split-opacity" => set!(unfocused_split_opacity, parse_opacity(value)?),
            "unfocused-split-fill" => set!(unfocused_split_fill, Some(Rgb::parse(value)?)),
            "split-divider-color" => set!(split_divider_color, Some(Rgb::parse(value)?)),
            "quadrant-peek-opacity" => set!(quadrant_peek_opacity, parse_opacity(value)?),
            "window-padding-x" => set!(window_padding_x, Padding::parse(value)?),
            "window-padding-y" => set!(window_padding_y, Padding::parse(value)?),
            "window-width" => set!(
                window_width,
                value.parse().map_err(|_| "invalid window width")?
            ),
            "window-height" => set!(
                window_height,
                value.parse().map_err(|_| "invalid window height")?
            ),
            "window-theme" => set!(window_theme, WindowTheme::parse(value)?),
            "window-save-state" => set!(window_save_state, WindowSaveState::parse(value)?),
            "macos-option-as-alt" => set!(macos_option_as_alt, OptionAsAlt::parse(value)?),
            "copy-on-select" => set!(copy_on_select, CopyOnSelect::parse(value)?),
            "clipboard-read" => set!(clipboard_read, ClipboardAccess::parse(value)?),
            "clipboard-write" => set!(clipboard_write, ClipboardAccess::parse(value)?),
            "clipboard-write-limit-bytes" => set!(
                clipboard_write_limit_bytes,
                parse_limit(value, Some(64 * 1024 * 1024))?
            ),
            "confirm-close-surface" => {
                set!(confirm_close_surface, ConfirmCloseSurface::parse(value)?)
            }
            "wait-after-command" => set!(wait_after_command, parse_bool(value)?),
            "abnormal-command-exit-runtime" => set!(
                abnormal_command_exit_runtime,
                value
                    .parse()
                    .map_err(|_| "invalid abnormal command exit runtime")?
            ),
            "undo-timeout" => set!(undo_timeout, {
                let timeout = parse_duration(value)?;
                if std::time::Instant::now().checked_add(timeout).is_none() {
                    return Err("undo timeout is too large");
                }
                timeout
            }),
            "quit-after-last-window-closed" => {
                set!(quit_after_last_window_closed, parse_bool(value)?)
            }
            "bell-features" => set!(bell_features, parse_bell_features(value)?),
            "notify-on-command-finish" => set!(
                notify_on_command_finish,
                NotifyOnCommandFinish::parse(value)?
            ),
            "notify-on-command-finish-action" => set!(
                notify_on_command_finish_action,
                parse_notify_actions(value)?
            ),
            "notify-on-command-finish-after" => {
                set!(notify_on_command_finish_after, parse_duration(value)?)
            }
            "quick-terminal-animation-duration" => set!(
                quick_terminal_animation_duration,
                Duration::try_from_secs_f64(
                    value.parse().map_err(|_| "invalid duration in seconds")?
                )
                .map_err(|_| "invalid duration in seconds")?
            ),
            "quick-terminal-position" => set!(
                quick_terminal_position,
                QuickTerminalPosition::parse(value)?
            ),
            "quick-terminal-screen" => {
                set!(quick_terminal_screen, QuickTerminalScreen::parse(value)?)
            }
            "quick-terminal-space-behavior" => set!(
                quick_terminal_space_behavior,
                QuickTerminalSpaceBehavior::parse(value)?
            ),
            "quick-terminal-autohide" => set!(quick_terminal_autohide, parse_bool(value)?),
            "shell-integration" => set!(shell_integration, ShellIntegration::parse(value)?),
            "scrollback-limit" | "scrollback-limit-bytes" => set!(
                scrollback_limit_bytes,
                parse_limit(value, Some(50_000_000))?
            ),
            "scrollback-limit-lines" => set!(scrollback_limit_lines, parse_limit(value, None)?),
            "scrollback-compression" => set!(scrollback_compression, parse_bool(value)?),
            "link-url" => set!(link_url, parse_bool(value)?),
            "command" => set!(command, Some(Command::parse(value)?)),
            "initial-command" => set!(initial_command, Some(Command::parse(value)?)),
            "working-directory" => set!(
                working_directory,
                match value {
                    "home" => Some(home.to_owned()),
                    "inherit" => None,
                    _ => Some(expand_path(value, entry.base(), home)),
                }
            ),
            "env" => {
                if value.is_empty() {
                    self.env.clear();
                } else {
                    let (key, value) = value.split_once('=').ok_or("env requires name=value")?;
                    if key.is_empty() || key.contains('\0') || value.contains('\0') {
                        return Err("invalid environment entry");
                    }
                    self.env.insert(key.to_owned(), value.to_owned());
                }
            }
            "theme" => set!(theme, Some(Theme::parse(value)?)),
            "keybind" => keybinding::apply(&mut self.keybinds, &mut self.keybind_chain, value)?,
            "config-default-files" => {} // CLI-only; the loader handles it before discovery.
            _ => return Err("setting is not supported by Rustty"),
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigFamily {
    Rustty,
    GhosttyLocal,
    Ghostty,
    Defaults,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub path: PathBuf,
    /// Zero denotes a file-level error, rather than an individual line.
    pub line: usize,
    pub key: Option<String>,
    /// Messages deliberately exclude setting values, which can contain secrets.
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path.display())?;
        if self.line > 0 {
            write!(f, ":{}", self.line)?;
        }
        if let Some(key) = &self.key {
            write!(f, " ({key})")?;
        }
        write!(f, ": {}", self.message)
    }
}

#[derive(Clone, Debug)]
pub struct LoadedConfig {
    pub config: Config,
    pub family: ConfigFamily,
    pub sources: Vec<PathBuf>,
    pub diagnostics: Vec<Diagnostic>,
    /// Use only for an explicit create/edit action. Loading never creates files.
    pub own_config_path: PathBuf,
}

/// Explicit paths make configuration loading independent of process-global
/// environment changes and allow isolated tests and embedded applications.
#[derive(Clone, Debug)]
pub struct ConfigLoader {
    pub home: PathBuf,
    pub xdg_config_home: PathBuf,
    /// The directory containing bundled `themes/` and other app resources.
    pub resources_dir: Option<PathBuf>,
    pub dark_mode: bool,
    pub working_directory: PathBuf,
}

impl ConfigLoader {
    pub fn from_env() -> io::Result<Self> {
        let home = env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is unavailable"))?;
        let xdg = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute());
        let resources_dir = env::current_exe().ok().and_then(|exe| {
            let contents = exe.parent()?.parent()?;
            (contents.file_name()? == "Contents").then(|| contents.join("Resources/rustty"))
        });
        Ok(Self {
            xdg_config_home: xdg.unwrap_or_else(|| home.join(".config")),
            home,
            resources_dir,
            dark_mode: true,
            working_directory: env::current_dir()?,
        })
    }

    pub fn load(&self) -> LoadedConfig {
        self.load_with_args(&[])
    }

    pub fn load_with_args(&self, args: &[String]) -> LoadedConfig {
        let own_app = self.home.join("Library/Application Support/com.rustty.app");
        let own = candidates(&self.xdg_config_home.join("rustty"), &own_app, "rustty");
        let mut result = LoadedConfig {
            config: Config::default(),
            family: ConfigFamily::Defaults,
            sources: vec![],
            diagnostics: vec![],
            own_config_path: own
                .iter()
                .rev()
                .find(|p| present(p))
                .cloned()
                .unwrap_or_else(|| own_app.join("config.rustty")),
        };
        let mut entries = Vec::new();
        let mut includes = VecDeque::new();
        let mut seen = HashSet::new();
        let mut cli_entries = Vec::new();
        let mut default_files = true;
        let mut initial_argv = None;
        let cli_path = self.working_directory.join("<command-line>");
        for (index, arg) in args.iter().enumerate() {
            if arg == "-e" || arg == "--" {
                if args[index + 1..].is_empty() {
                    push_diagnostic(
                        &mut result,
                        &cli_path,
                        index + 1,
                        None,
                        "-e requires a command",
                    );
                } else if args[index + 1..].iter().any(|v| v.contains('\0')) {
                    push_diagnostic(
                        &mut result,
                        &cli_path,
                        index + 1,
                        None,
                        "command contains NUL",
                    );
                } else {
                    initial_argv = Some(args[index + 1..].to_vec());
                }
                break;
            }
            let Some(arg) = arg.strip_prefix("--") else {
                push_diagnostic(
                    &mut result,
                    &cli_path,
                    index + 1,
                    None,
                    "expected --key=value or -e command",
                );
                continue;
            };
            if let Some(entry) = parse_entry(arg, &cli_path, index + 1, &mut result) {
                if entry.key == "config-default-files" {
                    match parse_bool(&entry.value) {
                        Ok(value) => default_files = value,
                        Err(message) => entry.diagnose(&mut result, message),
                    }
                } else {
                    cli_entries.push(entry);
                }
            }
        }
        let roots = if !default_files {
            vec![]
        } else if own.iter().any(|p| present(p)) {
            result.family = ConfigFamily::Rustty;
            own
        } else {
            let local = self
                .home
                .join("Library/Application Support/com.mitchellh.ghostty.local");
            let stable = self
                .home
                .join("Library/Application Support/com.mitchellh.ghostty");
            let local_exists = [local.join("config"), local.join("config.ghostty")]
                .iter()
                .any(|p| present(p));
            result.family = if local_exists {
                ConfigFamily::GhosttyLocal
            } else {
                ConfigFamily::Ghostty
            };
            let paths = candidates(
                &self.xdg_config_home.join("ghostty"),
                if local_exists { &local } else { &stable },
                "ghostty",
            );
            if !paths.iter().any(|p| present(p)) {
                result.family = ConfigFamily::Defaults;
            }
            paths
        };
        for path in roots {
            self.read_entries(
                &path,
                true,
                &mut result,
                &mut seen,
                &mut entries,
                &mut includes,
            );
        }
        // CLI font lists replace the corresponding file-defined lists once.
        let mut cli_fonts = HashSet::new();
        for entry in cli_entries {
            if matches!(
                entry.key.as_str(),
                "font-family"
                    | "font-family-bold"
                    | "font-family-italic"
                    | "font-family-bold-italic"
            ) && cli_fonts.insert(entry.key.clone())
            {
                entries.push(Entry {
                    value: String::new(),
                    ..entry.clone()
                });
            }
            self.queue_entry(entry, &mut entries, &mut includes);
        }
        while let Some(include) = includes.pop_front() {
            if seen.len() >= MAX_FILES {
                include
                    .entry
                    .diagnose(&mut result, "configuration include limit exceeded");
                break;
            }
            self.read_entries(
                &include.path,
                include.optional,
                &mut result,
                &mut seen,
                &mut entries,
                &mut includes,
            );
        }
        // Determine the final selected theme before applying it beneath all
        // explicit settings, regardless of where `theme` appears in a file.
        let mut selected_theme = None;
        let mut theme_entry = None;
        for entry in entries.iter().filter(|e| e.key == "theme") {
            if entry.value.is_empty() {
                selected_theme = None;
                theme_entry = None;
            } else if let Ok(theme) = Theme::parse(&entry.value) {
                selected_theme = Some(theme);
                theme_entry = Some(entry);
            }
        }
        if let (Some(theme), Some(entry)) = (selected_theme.as_ref(), theme_entry) {
            let name = if self.dark_mode {
                &theme.dark
            } else {
                &theme.light
            };
            if let Some(path) = self.theme_path(name, result.family) {
                let mut theme_entries = Vec::new();
                let mut unused_includes = VecDeque::new();
                self.read_entries(
                    &path,
                    false,
                    &mut result,
                    &mut HashSet::new(),
                    &mut theme_entries,
                    &mut unused_includes,
                );
                for entry in theme_entries.iter().filter(|e| e.key != "theme") {
                    if let Err(message) = result.config.apply(entry, &self.home) {
                        entry.diagnose(&mut result, message);
                    }
                }
            } else {
                entry.diagnose(&mut result, "theme was not found or has an invalid path");
            }
        }
        for entry in &entries {
            if let Err(message) = result.config.apply(entry, &self.home) {
                entry.diagnose(&mut result, message);
            }
        }
        if let Some(argv) = initial_argv {
            result.config.initial_command = Some(Command::Direct(argv));
        }
        if let Some(theme) = &result.config.theme
            && theme.light != theme.dark
            && result.config.window_theme == WindowTheme::Auto
        {
            result.config.window_theme = WindowTheme::System;
        }
        result
    }

    fn read_entries(
        &self,
        path: &Path,
        optional: bool,
        result: &mut LoadedConfig,
        seen: &mut HashSet<PathBuf>,
        entries: &mut Vec<Entry>,
        includes: &mut VecDeque<Include>,
    ) {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if optional && error.kind() == io::ErrorKind::NotFound => return,
            Err(error) => {
                push_diagnostic(
                    result,
                    path,
                    0,
                    None,
                    &format!("cannot read configuration: {}", error.kind()),
                );
                return;
            }
        };
        if !metadata.is_file() {
            push_diagnostic(
                result,
                path,
                0,
                None,
                "configuration path is not a regular file",
            );
            return;
        }
        let identity = fs::canonicalize(path).unwrap_or_else(|_| normalize_path(path));
        if !seen.insert(identity) {
            push_diagnostic(
                result,
                path,
                0,
                Some("config-file"),
                "configuration include cycle or duplicate",
            );
            return;
        }
        let bytes = (|| {
            let mut bytes = Vec::new();
            fs::File::open(path)?
                .take(MAX_FILE_BYTES + 1)
                .read_to_end(&mut bytes)?;
            Ok::<_, io::Error>(bytes)
        })();
        let bytes = match bytes {
            Ok(bytes) if bytes.len() as u64 <= MAX_FILE_BYTES => bytes,
            Ok(_) => {
                push_diagnostic(
                    result,
                    path,
                    0,
                    None,
                    "configuration file exceeds size limit",
                );
                return;
            }
            Err(error) => {
                push_diagnostic(
                    result,
                    path,
                    0,
                    None,
                    &format!("cannot read configuration: {}", error.kind()),
                );
                return;
            }
        };
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                push_diagnostic(result, path, 0, None, "configuration is not valid UTF-8");
                return;
            }
        };
        result.sources.push(path.to_owned());
        for (line, text) in text.trim_start_matches('\u{feff}').lines().enumerate() {
            if let Some(entry) = parse_entry(text, path, line + 1, result) {
                self.queue_entry(entry, entries, includes);
            }
        }
    }

    fn queue_entry(
        &self,
        entry: Entry,
        entries: &mut Vec<Entry>,
        includes: &mut VecDeque<Include>,
    ) {
        if entry.key == "config-file" {
            if entry.value.is_empty() {
                includes.clear();
                return;
            }
            let (optional, value) = if entry.quoted {
                (false, entry.value.as_str())
            } else {
                entry
                    .value
                    .strip_prefix('?')
                    .map_or((false, entry.value.as_str()), |v| (true, v))
            };
            let path = expand_path(unquote(value), entry.base(), &self.home);
            includes.push_back(Include {
                path,
                optional,
                entry,
            });
        } else {
            entries.push(entry);
        }
    }

    fn theme_path(&self, name: &str, family: ConfigFamily) -> Option<PathBuf> {
        if name.starts_with("~/") {
            return Some(expand_path(name, &self.home, &self.home));
        }
        let path = Path::new(name);
        if path.is_absolute() {
            return Some(path.to_owned());
        }
        if name.is_empty() || name.contains(['/', '\\']) {
            return None;
        }
        let user_dir = if family == ConfigFamily::Rustty {
            "rustty"
        } else {
            "ghostty"
        };
        let user_path = self
            .xdg_config_home
            .join(user_dir)
            .join("themes")
            .join(name);
        if present(&user_path) {
            return Some(user_path);
        }
        self.resources_dir
            .as_ref()
            .map(|p| p.join("themes").join(name))
            .filter(|p| present(p))
    }
}

#[derive(Clone)]
struct Entry {
    path: PathBuf,
    line: usize,
    key: String,
    value: String,
    quoted: bool,
}
impl Entry {
    fn base(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }
    fn diagnose(&self, result: &mut LoadedConfig, message: &str) {
        push_diagnostic(result, &self.path, self.line, Some(&self.key), message);
    }
}
struct Include {
    path: PathBuf,
    optional: bool,
    entry: Entry,
}

fn parse_entry(text: &str, path: &Path, line: usize, result: &mut LoadedConfig) -> Option<Entry> {
    let text = text.trim();
    if text.is_empty() || text.starts_with('#') {
        return None;
    }
    let Some((key, value)) = text.split_once('=') else {
        push_diagnostic(result, path, line, None, "expected key = value");
        return None;
    };
    let key = key.trim();
    if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        push_diagnostic(result, path, line, None, "invalid setting name");
        return None;
    }
    let value = value.trim();
    if (value.starts_with('"') && !value.ends_with('"')) || value == "\"" {
        push_diagnostic(result, path, line, Some(key), "unmatched quote");
        return None;
    }
    Some(Entry {
        path: path.to_owned(),
        line,
        key: key.to_owned(),
        value: unquote(value).to_owned(),
        quoted: value.starts_with('"'),
    })
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
}
fn push_diagnostic(
    result: &mut LoadedConfig,
    path: &Path,
    line: usize,
    key: Option<&str>,
    message: &str,
) {
    result.diagnostics.push(Diagnostic {
        path: path.to_owned(),
        line,
        key: key.map(str::to_owned),
        message: message.to_owned(),
    });
}
fn candidates(xdg: &Path, app: &Path, extension: &str) -> Vec<PathBuf> {
    vec![
        xdg.join("config"),
        xdg.join(format!("config.{extension}")),
        app.join("config"),
        app.join(format!("config.{extension}")),
    ]
}
fn present(path: &Path) -> bool {
    !matches!(fs::symlink_metadata(path), Err(error) if error.kind() == io::ErrorKind::NotFound)
}
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}
fn expand_path(value: &str, base: &Path, home: &Path) -> PathBuf {
    if let Some(value) = value.strip_prefix("~/") {
        return normalize_path(&home.join(value));
    }
    let path = Path::new(value);
    normalize_path(&if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    })
}
fn append_or_clear(values: &mut Vec<String>, value: &str) {
    if value.is_empty() {
        values.clear();
    } else {
        values.push(value.to_owned());
    }
}
fn parse_bool(value: &str) -> Result<bool, &'static str> {
    match value {
        "true" | "1" | "t" | "T" => Ok(true),
        "false" | "0" | "f" | "F" => Ok(false),
        _ => Err("expected true or false"),
    }
}
fn parse_nonnegative(value: &str) -> Result<f32, &'static str> {
    value
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or("expected a finite nonnegative number")
}
fn parse_positive(value: &str) -> Result<f32, &'static str> {
    let value = parse_nonnegative(value)?;
    if value > 0.0 {
        Ok(value)
    } else {
        Err("expected a positive number")
    }
}
fn parse_opacity(value: &str) -> Result<f32, &'static str> {
    let value = parse_nonnegative(value)?;
    if value <= 1.0 {
        Ok(value)
    } else {
        Err("opacity must be between zero and one")
    }
}
fn parse_usize(value: &str) -> Result<usize, &'static str> {
    let (radix, number) = if let Some(n) = value.strip_prefix("0x") {
        (16, n)
    } else if let Some(n) = value.strip_prefix("0o") {
        (8, n)
    } else if let Some(n) = value.strip_prefix("0b") {
        (2, n)
    } else {
        (10, value)
    };
    usize::from_str_radix(number, radix).map_err(|_| "invalid unsigned integer")
}
fn parse_limit(value: &str, default: Option<usize>) -> Result<Option<usize>, &'static str> {
    match value {
        "unlimited" => Ok(None),
        "default" => Ok(default),
        _ => parse_usize(value).map(Some),
    }
}
fn feature_flags(value: &str) -> impl Iterator<Item = (&str, bool)> {
    value.split(',').map(|s| {
        s.trim()
            .strip_prefix("no-")
            .map_or((s.trim(), true), |v| (v, false))
    })
}
fn parse_bell_features(value: &str) -> Result<BellFeatures, &'static str> {
    if let Ok(value) = parse_bool(value) {
        return Ok(BellFeatures {
            system: value,
            audio: value,
            attention: value,
            title: value,
            border: value,
        });
    }
    let mut result = BellFeatures::default();
    for (name, enabled) in feature_flags(value) {
        *match name {
            "system" => &mut result.system,
            "audio" => &mut result.audio,
            "attention" => &mut result.attention,
            "title" => &mut result.title,
            "border" => &mut result.border,
            _ => return Err("unknown bell feature"),
        } = enabled;
    }
    Ok(result)
}
fn parse_notify_actions(value: &str) -> Result<NotifyActions, &'static str> {
    if let Ok(value) = parse_bool(value) {
        return Ok(NotifyActions {
            bell: value,
            notify: value,
        });
    }
    let mut result = NotifyActions::default();
    for (name, enabled) in feature_flags(value) {
        *match name {
            "bell" => &mut result.bell,
            "notify" => &mut result.notify,
            _ => return Err("unknown notification action"),
        } = enabled;
    }
    Ok(result)
}
fn parse_duration(mut value: &str) -> Result<Duration, &'static str> {
    if value == "0" {
        return Ok(Duration::ZERO);
    }
    let mut total = 0u64;
    while !value.trim_start().is_empty() {
        value = value.trim_start();
        let length = value.bytes().take_while(u8::is_ascii_digit).count();
        if length == 0 {
            return Err("duration requires a number and unit");
        }
        let number: u64 = value[..length].parse().map_err(|_| "duration overflow")?;
        value = &value[length..];
        let units = [
            ("ms", 1_000_000),
            ("us", 1_000),
            ("ns", 1),
            ("y", 31_536_000_000_000_000),
            ("d", 86_400_000_000_000),
            ("h", 3_600_000_000_000),
            ("m", 60_000_000_000),
            ("s", 1_000_000_000),
        ];
        let Some((unit, factor)) = units.into_iter().find(|(unit, _)| value.starts_with(unit))
        else {
            return Err("invalid duration unit");
        };
        total = total.saturating_add(number.saturating_mul(factor));
        value = &value[unit.len()..];
    }
    Ok(Duration::from_nanos(total))
}

fn default_palette() -> [Rgb; 256] {
    let mut palette = [Rgb::default(); 256];
    let base = [
        0x1d1f21, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6, 0x666666,
        0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea,
    ];
    for (cell, color) in palette.iter_mut().zip(base) {
        *cell = Rgb::new((color >> 16) as u8, (color >> 8) as u8, color as u8);
    }
    for (i, cell) in palette.iter_mut().enumerate().take(232).skip(16) {
        let index = i - 16;
        let channel = |v: usize| if v == 0 { 0 } else { (v * 40 + 55) as u8 };
        *cell = Rgb::new(
            channel(index / 36),
            channel(index / 6 % 6),
            channel(index % 6),
        );
    }
    for (i, cell) in palette.iter_mut().enumerate().skip(232) {
        let gray = ((i - 232) * 10 + 8) as u8;
        *cell = Rgb::new(gray, gray, gray);
    }
    palette
}

#[cfg(test)]
#[path = "config/tests.rs"]
mod tests;
