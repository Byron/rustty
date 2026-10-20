//! Headless terminal integration backed by the shared Rustty VT.
//!
//! This first backend follows Rustty terminal semantics, not a claim of complete
//! C tmux equivalence. Snapshots are read models, not process checkpoints.

pub use rustty_vt::TmuxPaneStyle;
pub use rustty_vt::graphics::ViewSnapshot as GraphicsSnapshot;
pub use rustty_vt::graphics::quota::Quota as ImageQuota;
pub use rustty_vt::tmux_state::TmuxState;
pub use rustty_vt::{
    Color, Cursor, Effect, Key, KeyAction, KeyEncodeOptions, KeyEvent, Modifiers, Screen, Style,
};
pub use rustty_vt::{CursorShape, clipboard, parse_color};
use serde::{Deserialize, Serialize};
use std::io;
mod agent;
pub use agent::AgentStatus;
mod capture;
mod capture_raw;
pub use capture::CaptureOptions;

/// Native input retains its kind so each synchronized pane uses its own modes.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum Input {
    Key {
        event: KeyEvent,
        options: KeyEncodeOptions,
    },
    Text(Vec<u8>),
    Paste(Vec<u8>),
    Mouse(Vec<u8>),
    Reply(Vec<u8>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HostAction {
    Bell,
    Notification { title: Vec<u8>, body: Vec<u8> },
    ClipboardRead(clipboard::Read),
    ClipboardWrite(clipboard::Write),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HostResponse {
    Read(clipboard::ReadResult),
    Write(clipboard::WriteResult),
}

impl HostAction {
    pub fn denied(&self) -> Option<HostResponse> {
        match self {
            Self::ClipboardRead(_) => Some(HostResponse::Read(clipboard::ReadResult::Denied)),
            Self::ClipboardWrite(_) => Some(HostResponse::Write(clipboard::WriteResult::Denied)),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Terminal {
    inner: rustty_vt::Terminal,
    agent_status: AgentStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mode {
    pub private: bool,
    pub number: u16,
    pub enabled: bool,
    pub saved: bool,
    pub default: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalSettings {
    pub alternate_screen: bool,
    pub scroll_on_clear: bool,
    pub allow_set_title: bool,
    pub cursor_style: u16,
    pub state: TmuxState,
    pub pane_style: Option<TmuxPaneStyle>,
    pub control_colors: [Option<[u8; 3]>; 2],
    pub host_dark: Option<bool>,
    pub widths: Vec<(u32, u32, u8)>,
    pub vs16_wide: bool,
}
impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            alternate_screen: true,
            scroll_on_clear: true,
            allow_set_title: true,
            cursor_style: 0,
            state: TmuxState::default(),
            pane_style: None,
            control_colors: [None; 2],
            host_dark: None,
            widths: Vec::new(),
            vs16_wide: true,
        }
    }
}

/// Both screens retain complete rows/history, cells, styles, and saved cursors.
/// Host effects are deliberately absent: reconnect must not repeat notifications
/// or clipboard actions. Graphics retain display state and pending upload bytes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot<S = Screen> {
    #[serde(default)]
    pub agent_status: AgentStatus,
    pub version: u32,
    pub cols: u16,
    pub rows: u16,
    pub generation: u64,
    pub alternate_active: bool,
    #[serde(default)]
    pub extended_keys: Option<(u8, u8)>,
    #[serde(default)]
    pub settings: TerminalSettings,
    pub primary: S,
    pub alternate: Option<S>,
    pub modes: Vec<Mode>,
    pub margins: [usize; 4],
    pub tabstops: Vec<bool>,
    pub palette: Vec<[u8; 3]>,
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub cursor_color: Option<[u8; 3]>,
    pub title: Vec<u8>,
    pub working_directory: Vec<u8>,
    pub parser_continuation: Option<Vec<u8>>,
    pub continuation_error: Option<String>,
    #[serde(default)]
    pub capture_pending: Option<Vec<u8>>,
    #[serde(default)]
    pub capture_pending_error: Option<String>,
    /// Graphics display state and upload continuation are included.
    pub graphics_included: bool,
    pub primary_graphics: GraphicsSnapshot,
    pub alternate_graphics: Option<GraphicsSnapshot>,
    pub checkpoint: Option<Vec<u8>>,
    pub checkpoint_error: Option<String>,
}

impl Snapshot {
    pub fn restore(&self) -> io::Result<Terminal> {
        self.restore_with_image_limit(1024 * 1024 * 1024)
    }
    /// Reject oversized image state before replacing the receiving terminal.
    pub fn restore_with_image_limit(&self, max_image_bytes: usize) -> io::Result<Terminal> {
        if self.version != 1 {
            return Err(io::Error::other("unsupported terminal snapshot"));
        }
        let bytes = self.checkpoint.as_ref().ok_or_else(|| {
            io::Error::other(
                self.checkpoint_error
                    .as_deref()
                    .unwrap_or("missing terminal checkpoint"),
            )
        })?;
        let mut inner = rustty_vt::snapshot::decode_exact(
            bytes.as_slice(),
            rustty_vt::snapshot::DecodeOptions {
                tmux_strings: true,
                ..Default::default()
            },
        )?;
        if (inner.cols, inner.rows, inner.is_alternate_screen())
            != (self.cols, self.rows, self.alternate_active)
        {
            return Err(io::Error::other("snapshot metadata mismatch"));
        }
        if self
            .extended_keys
            .is_some_and(|(policy, mode)| policy > 2 || mode > 2)
        {
            return Err(io::Error::other("invalid extended-key state"));
        }
        inner.tmux_extended_keys = self.extended_keys;
        if self.settings.cursor_style > 6
            || self.settings.widths.iter().any(|(start, end, width)| {
                *start == 0 || start > end || *end > i32::MAX as u32 || *width > 2
            })
        {
            return Err(io::Error::other("invalid terminal settings"));
        }
        inner.tmux_alternate_screen = self.settings.alternate_screen;
        inner.tmux_scroll_on_clear = self.settings.scroll_on_clear;
        inner.allow_set_title = self.settings.allow_set_title;
        inner.tmux_cursor_style = self.settings.cursor_style;
        if self.settings.state.progress_state > 4 || self.settings.state.progress > 100 {
            return Err(io::Error::other("invalid terminal progress state"));
        }
        inner.tmux_state = self.settings.state;
        inner.query_defaults.color_scheme = self.settings.host_dark.map(|dark| {
            if dark {
                rustty_vt::query::ColorScheme::Dark
            } else {
                rustty_vt::query::ColorScheme::Light
            }
        });
        if let Some(style) = self.settings.pane_style {
            inner.set_tmux_pane_style(style);
        }
        inner.tmux_control_colors = self.settings.control_colors;
        inner.tmux_widths = self.settings.widths.clone();
        inner.tmux_vs16_wide = self.settings.vs16_wide;
        inner
            .restore_view_row_identity(&self.primary, self.alternate.as_ref())
            .map_err(io::Error::other)?;
        inner
            .restore_view_graphics_with_limit(
                self.primary_graphics.clone(),
                self.alternate_graphics.clone(),
                max_image_bytes,
            )
            .map_err(io::Error::other)?;
        inner
            .restore_tmux_capture_metadata(&self.primary, self.alternate.as_ref())
            .map_err(io::Error::other)?;
        if self.capture_pending.is_some() || self.capture_pending_error.is_some() {
            inner
                .restore_tmux_pending_input(self.capture_pending.as_deref())
                .map_err(io::Error::other)?;
        }
        self.agent_status.validate().map_err(io::Error::other)?;
        inner.agent_status_events = true;
        inner.shell_command_events = true;
        inner.linefeed_mode_events = true;
        inner.generation = self.generation;
        inner.tmux_clipboard = true;
        inner.tmux_graphics = true;
        inner.set_tmux_image_quota(ImageQuota::default());
        Ok(Terminal {
            inner,
            agent_status: self.agent_status.clone(),
        })
    }
}

impl Terminal {
    /// Pull retained history into unused rows below the cursor for resize-pane -T.
    pub fn trim_history_below_cursor(&mut self) {
        self.inner.trim_tmux_history_below_cursor();
    }

    /// Reset pane input parsing and the visible screen for send-keys -R.
    pub fn reset_from_host(&mut self) {
        self.inner.reset_from_host();
    }

    pub fn application_color(&self, index: usize) -> Option<[u8; 3]> {
        self.inner.color_override(
            [
                rustty_vt::color::Target::Foreground,
                rustty_vt::color::Target::Background,
                rustty_vt::color::Target::Cursor,
            ][index],
        )
    }
    /// These fields read the live state without copying history or image payloads.
    pub fn format_value(&self, key: &str) -> Option<String> {
        let t = &self.inner;
        let state = &t.tmux_state;
        Some(match key {
            "history_bytes" => t.primary_screen().owned_bytes().to_string(),
            "history_all_bytes" => t
                .primary_screen()
                .owned_storage_parts()
                .map(|value| value.to_string())
                .join(","),
            "history_added" => t.primary_screen().tmux_history?.added.to_string(),
            "history_collected" => t.primary_screen().tmux_history?.collected.to_string(),
            "history_generation" => t.primary_screen().tmux_history?.generation.to_string(),
            "history_limit" => t.primary_screen().tmux_history?.limit.to_string(),
            "alternate_saved_x" => state.alternate_saved[0].to_string(),
            "alternate_saved_y" => state.alternate_saved[1].to_string(),
            "cursor_shape" => t.tmux_cursor_shape().into(),
            "cursor_blinking" => u8::from(state.cursor_blinking).to_string(),
            "cursor_very_visible" => u8::from(state.cursor_very_visible).to_string(),
            "cursor_character" => {
                let screen = t.screen();
                let cursor = &screen.cursor;
                if cursor.col >= usize::from(t.cols) {
                    return Some(" ".into());
                }
                let row = screen.row(cursor.row);
                if row
                    .cells()
                    .get(cursor.col)
                    .is_some_and(|cell| cell.width() == 0)
                {
                    String::new()
                } else {
                    let value = screen.cell_text(row, cursor.col).to_string();
                    if value.is_empty() { " ".into() } else { value }
                }
            }
            "pane_path" => std::str::from_utf8(t.working_directory_bytes())
                .ok()?
                .to_owned(),
            "pane_pb_state" => ["hidden", "normal", "error", "indeterminate", "paused"]
                [usize::from(state.progress_state)]
            .into(),
            "pane_pb_progress" => state.progress.to_string(),
            "pane_private_modes" => [
                1, 6, 7, 12, 25, 1000, 1002, 1003, 1004, 1005, 1006, 2004, 2026, 2031,
            ]
            .into_iter()
            .filter(|mode| {
                if *mode == 12 {
                    state.cursor_blinking && state.cursor_blinking_set
                } else {
                    t.modes.dec(*mode)
                }
            })
            .map(|mode| mode.to_string())
            .collect::<Vec<_>>()
            .join(","),
            "pane_tabs" => t
                .tabstops()
                .iter()
                .enumerate()
                .take(usize::from(t.cols))
                .filter(|(_, on)| **on)
                .map(|(index, _)| index.to_string())
                .collect::<Vec<_>>()
                .join(","),
            "scroll_region_upper" => t.margins.top.to_string(),
            "scroll_region_lower" => t.margins.bottom.to_string(),
            "mouse_any_flag" => {
                u8::from([1000, 1002, 1003].iter().any(|mode| t.modes.dec(*mode))).to_string()
            }
            "bracket_paste_flag"
            | "mouse_all_flag"
            | "mouse_button_flag"
            | "mouse_sgr_flag"
            | "mouse_standard_flag"
            | "mouse_utf8_flag"
            | "origin_flag"
            | "synchronized_output_flag" => {
                let mode = match key {
                    "bracket_paste_flag" => 2004,
                    "mouse_all_flag" => 1003,
                    "mouse_button_flag" => 1002,
                    "mouse_sgr_flag" => 1006,
                    "mouse_standard_flag" => 1000,
                    "mouse_utf8_flag" => 1005,
                    "origin_flag" => 6,
                    _ => 2026,
                };
                u8::from(t.modes.dec(mode)).to_string()
            }
            _ => return None,
        })
    }
    pub fn control_colors(&self) -> [Option<[u8; 3]>; 2] {
        self.inner.tmux_control_colors
    }
    pub fn set_history_limit(&mut self, limit: usize) {
        self.inner.set_tmux_history_limit(limit);
    }
    pub fn clear_history(&mut self, hyperlinks: bool) {
        self.inner.clear_tmux_history(hyperlinks);
    }
    pub fn set_control_colors(&mut self, colors: [Option<[u8; 3]>; 2]) -> Vec<Effect> {
        if self.inner.tmux_control_colors == colors {
            return Vec::new();
        }
        let old = self.inner.tmux_color_scheme();
        self.inner.tmux_control_colors = colors;
        self.inner.generation = self.inner.generation.wrapping_add(1);
        self.theme_update(old)
    }
    pub fn generation(&self) -> u64 {
        self.inner.generation
    }
    pub fn host_dark(&self) -> Option<bool> {
        self.inner
            .query_defaults
            .color_scheme
            .map(|scheme| scheme == rustty_vt::query::ColorScheme::Dark)
    }
    pub fn set_pane_style(&mut self, style: TmuxPaneStyle) -> Vec<Effect> {
        let old = self.inner.tmux_color_scheme();
        self.inner.set_tmux_pane_style(style);
        self.theme_update(old)
    }
    fn theme_update(&self, old: Option<rustty_vt::query::ColorScheme>) -> Vec<Effect> {
        let new = self.inner.tmux_color_scheme();
        if self.inner.modes.dec(2031) && new != old {
            new.map(|scheme| vec![Effect::Write(scheme.encode())])
                .unwrap_or_default()
        } else {
            Vec::new()
        }
    }
    pub fn agent_status(&self) -> &AgentStatus {
        &self.agent_status
    }
    pub fn clear_agent_status(&mut self) {
        self.agent_status = AgentStatus::default();
    }
    /// Transfer a native view to Rustty without changing its terminal type or profile.
    pub fn into_backend(self) -> rustty_vt::Terminal {
        self.inner
    }
    pub fn hide_cursor(&mut self) {
        self.inner.modes.set(true, 25, false);
        self.inner.screen_mut().cursor.visible = false;
    }
    pub fn set_width_options(&mut self, widths: &[(u32, u32, u8)], vs16_wide: bool) {
        self.inner.tmux_widths.clear();
        self.inner.tmux_widths.extend_from_slice(widths);
        self.inner.tmux_vs16_wide = vs16_wide;
    }
    pub fn set_screen_options(&mut self, alternate: bool, scroll_on_clear: bool) {
        self.inner.tmux_alternate_screen = alternate;
        self.inner.tmux_scroll_on_clear = scroll_on_clear;
    }
    pub fn set_cursor_options(&mut self, style: u16, color: Option<[u8; 3]>, palette: &[[u8; 3]]) {
        self.inner.tmux_cursor_style = style;
        let shape = match style {
            3 | 4 => rustty_vt::CursorShape::Underline,
            5 | 6 => rustty_vt::CursorShape::Bar,
            _ => rustty_vt::CursorShape::Block,
        };
        self.inner
            .set_default_cursor(shape, Some(matches!(style, 0 | 1 | 3 | 5)));
        self.inner.set_default_colors(None, None, color, palette);
    }
    pub fn default_palette() -> Vec<[u8; 3]> {
        rustty_vt::default_palette()
    }
    pub fn set_image_size_limit(&mut self, bytes: usize) {
        self.inner.tmux_image_limit = bytes.max(1);
    }
    pub fn set_image_quota(&mut self, quota: ImageQuota) {
        self.inner.set_tmux_image_quota(quota);
    }
    /// Only use with a fork of the currently attached owner quota.
    pub fn rebind_image_quota(&mut self, quota: ImageQuota) {
        self.inner.rebind_tmux_image_quota(quota);
    }
    pub fn image_owners(&self) -> impl Iterator<Item = u64> + '_ {
        self.inner.tmux_image_owners()
    }
    pub fn uses_image_quota(&self, quota: &ImageQuota) -> bool {
        self.inner.uses_tmux_image_quota(quota)
    }
    pub fn prune_images(&mut self) {
        self.inner.prune_tmux_images();
    }
    pub fn graphics_generation(&self) -> (u64, Option<u64>) {
        (
            self.inner.primary_screen().graphics.generation,
            self.inner
                .alternate_screen()
                .map(|screen| screen.graphics.generation),
        )
    }
    pub fn set_title(&mut self, title: &[u8]) {
        self.inner.set_title(title);
    }
    pub fn set_allow_set_title(&mut self, enabled: bool) {
        self.inner.allow_set_title = enabled;
    }
    pub fn new(cols: u16, rows: u16, history_limit: usize) -> Self {
        let mut inner = rustty_vt::Terminal::new(cols, rows, history_limit);
        inner.set_tmux_history_limit(history_limit);
        inner.set_tmux_strings(true);
        inner.terminfo_name = Some(b"tmux-256color".to_vec());
        inner.shell_command_events = true;
        inner.agent_status_events = true;
        inner.linefeed_mode_events = true;
        inner.tmux_clipboard = true;
        inner.tmux_graphics = true;
        inner.set_tmux_image_quota(ImageQuota::default());
        Self {
            inner,
            agent_status: AgentStatus::default(),
        }
    }

    /// The owner sends each `Effect::Write` to the application's PTY exactly once.
    /// Other effects belong to a separate host-action delivery policy.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Effect> {
        self.inner.prune_tmux_images();
        let old = self.inner.tmux_color_scheme();
        let mut effects = self.inner.feed(bytes);
        for effect in &effects {
            let _ = self.agent_status.observe(effect);
        }
        self.inner.prune_tmux_images();
        effects.extend(self.theme_update(old));
        effects
    }

    pub fn feed_at(&mut self, bytes: &[u8], seconds: u64) -> Vec<Effect> {
        self.inner.set_tmux_time(seconds);
        self.feed(bytes)
    }

    pub fn host_response(
        &mut self,
        action: HostAction,
        response: HostResponse,
    ) -> io::Result<Option<Vec<u8>>> {
        match (action, response) {
            (HostAction::ClipboardRead(request), HostResponse::Read(result)) => {
                Ok(Some(self.inner.reply_clipboard_read(request, result)))
            }
            (HostAction::ClipboardWrite(request), HostResponse::Write(result)) => {
                Ok(self.inner.reply_clipboard_write(request, result))
            }
            _ => Err(io::Error::other("host response does not match request")),
        }
    }

    pub fn cell_size(&self) -> (u16, u16) {
        let size = self.inner.query_size();
        (
            size.cell_width.min(u16::MAX.into()) as u16,
            size.cell_height.min(u16::MAX.into()) as u16,
        )
    }
    pub fn set_cell_size(&mut self, width: u16, height: u16) {
        self.inner.set_pixel_size(
            u32::from(self.inner.cols) * u32::from(width),
            u32::from(self.inner.rows) * u32::from(height),
        );
    }
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cell = self.cell_size();
        self.inner.resize(cols.max(1), rows.max(1));
        self.set_cell_size(cell.0, cell.1);
    }

    pub fn size(&self) -> (u16, u16) {
        (self.inner.cols, self.inner.rows)
    }

    /// Zero-based column and row in the active screen.
    pub fn cursor(&self) -> (usize, usize) {
        let cursor = &self.inner.screen().cursor;
        (cursor.col, cursor.row)
    }

    pub fn title(&self) -> &[u8] {
        self.inner.title_bytes()
    }

    pub fn mode(&self, private: bool, number: u16) -> bool {
        self.inner.modes.get(private, number)
    }

    /// Apply aggregated native GUI host state. The owner writes returned reports once.
    pub fn set_host_state(
        &mut self,
        focused: bool,
        visible: bool,
        dark: Option<bool>,
        focus_events: bool,
    ) -> Vec<Effect> {
        let old_theme = self.inner.tmux_color_scheme();
        let terminal = &mut self.inner;
        let mut reply = Vec::new();
        let reported_focus = focus_events.then_some(focused);
        if terminal.query_defaults.focused != reported_focus {
            terminal.query_defaults.focused = reported_focus;
            if focus_events {
                reply.extend(terminal.encode_focus(focused));
            }
        }
        if terminal.visible != visible {
            terminal.visible = visible;
            if terminal.modes.dec(2033) {
                reply.extend(rustty_vt::query::visibility(visible));
            }
        }
        if let Some(dark) = dark {
            let scheme = if dark {
                rustty_vt::query::ColorScheme::Dark
            } else {
                rustty_vt::query::ColorScheme::Light
            };
            if terminal.query_defaults.color_scheme != Some(scheme) {
                terminal.query_defaults.color_scheme = Some(scheme);
                terminal.generation = terminal.generation.wrapping_add(1);
            }
        }
        for effect in self.theme_update(old_theme) {
            if let Effect::Write(data) = effect {
                reply.extend(data);
            }
        }
        if reply.is_empty() {
            Vec::new()
        } else {
            vec![Effect::Write(reply)]
        }
    }

    pub fn encode_input(&self, input: &Input) -> Vec<u8> {
        match input {
            Input::Key { event, options } => self.inner.encode_key_with_options(event, *options),
            Input::Paste(data) => self.inner.encode_paste(data),
            Input::Text(data) | Input::Mouse(data) | Input::Reply(data) => data.clone(),
        }
    }

    pub fn set_extended_keys(&mut self, policy: u8) {
        if let Some((old, _)) = &mut self.inner.tmux_extended_keys {
            *old = policy;
        } else {
            self.inner.tmux_extended_keys = Some((policy, u8::from(policy == 2)));
        }
    }
    pub fn extended_keys_mode(&self) -> u8 {
        self.inner.tmux_extended_keys.map_or(0, |(_, mode)| mode)
    }
    pub fn kitty_keyboard_active(&self) -> bool {
        self.inner.screen().kitty_keyboard.current() != 0
    }
    pub fn encode_key(&self, event: &KeyEvent) -> Vec<u8> {
        self.inner.encode_key(event)
    }

    pub fn history_len(&self) -> usize {
        self.inner.primary_screen().history_len()
    }

    pub fn cwd(&self) -> &[u8] {
        self.inner.working_directory_bytes()
    }

    pub fn capture(&self) -> String {
        self.capture_lines(None, None, false)
    }

    /// Inclusive tmux-style rows: zero is the first visible row, negatives are
    /// history. Missing bounds select the visible screen. `i32::MIN` selects
    /// the beginning of history; `join` joins soft-wrapped physical rows.
    pub fn capture_lines(&self, start: Option<i32>, end: Option<i32>, join: bool) -> String {
        String::from_utf8(
            self.capture_with(CaptureOptions {
                start,
                end,
                join,
                ..CaptureOptions::default()
            })
            .expect("plain capture always has a screen"),
        )
        .expect("terminal text is UTF-8")
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot_with(Clone::clone)
    }

    pub fn compact_snapshot(&self) -> Snapshot<impl Serialize + '_> {
        self.snapshot_with(|screen| screen.compact())
    }

    fn snapshot_with<'a, S>(&'a self, screen: impl Fn(&'a Screen) -> S) -> Snapshot<S> {
        let terminal = &self.inner;
        let continuation = terminal.parser().continuation();
        let capture_pending = terminal.parser().pending_input();
        let checkpoint = rustty_vt::snapshot::encode_to_vec(terminal);
        Snapshot {
            agent_status: self.agent_status.clone(),
            version: 1,
            cols: terminal.cols,
            rows: terminal.rows,
            generation: terminal.generation,
            alternate_active: terminal.is_alternate_screen(),
            extended_keys: terminal.tmux_extended_keys,
            settings: TerminalSettings {
                alternate_screen: terminal.tmux_alternate_screen,
                scroll_on_clear: terminal.tmux_scroll_on_clear,
                allow_set_title: terminal.allow_set_title,
                cursor_style: terminal.tmux_cursor_style,
                state: terminal.tmux_state,
                pane_style: terminal.tmux_pane_style,
                control_colors: terminal.tmux_control_colors,
                host_dark: self.host_dark(),
                widths: terminal.tmux_widths.clone(),
                vs16_wide: terminal.tmux_vs16_wide,
            },
            primary: screen(terminal.primary_screen()),
            alternate: terminal.alternate_screen().map(screen),
            modes: terminal
                .modes
                .iter()
                .map(|(private, number)| Mode {
                    private,
                    number,
                    enabled: terminal.modes.get(private, number),
                    saved: terminal.modes.get_saved(private, number).unwrap_or(false),
                    default: terminal.modes.get_default(private, number).unwrap_or(false),
                })
                .collect(),
            margins: [
                terminal.margins.top,
                terminal.margins.bottom,
                terminal.margins.left,
                terminal.margins.right,
            ],
            tabstops: terminal.tabstops().to_vec(),
            palette: terminal.palette.clone(),
            foreground: terminal.foreground,
            background: terminal.background,
            cursor_color: terminal.cursor_color,
            title: terminal.title_bytes().to_vec(),
            working_directory: terminal.working_directory_bytes().to_vec(),
            parser_continuation: continuation.as_ref().ok().cloned(),
            continuation_error: continuation.err().map(|error| error.to_string()),
            capture_pending: capture_pending.as_ref().ok().map(|bytes| bytes.to_vec()),
            capture_pending_error: capture_pending.err().map(|error| error.to_string()),
            graphics_included: true,
            primary_graphics: terminal.primary_screen().graphics.view_snapshot(),
            alternate_graphics: terminal
                .alternate_screen()
                .map(|screen| screen.graphics.view_snapshot()),
            checkpoint: checkpoint.as_ref().ok().cloned(),
            checkpoint_error: checkpoint.err().map(|error| error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn compact_snapshot_preserves_terminal_state_and_continuation() {
        let mut terminal = Terminal::new(80, 4, 100);
        for input in [
            b"\x1b[31mone\ttwo\r\n\r\n\r\n\r\n\x1b[0m".as_slice(),
            b"\x1b[?1049halternate\x1b[?1049l",
            b"\x1b_Ga=T,i=7,f=32,s=1,v=1;/////w==\x1b\\",
            b"\x1b]133;A\x07prompt\x1b]133;B\x07\x1b[3",
        ] {
            terminal.feed(input);
            let legacy = terminal.snapshot().restore().unwrap();
            let compact: Snapshot =
                serde_json::from_slice(&serde_json::to_vec(&terminal.compact_snapshot()).unwrap())
                    .unwrap();
            let mut compact = compact.restore().unwrap();
            assert_eq!(
                serde_json::to_value(legacy.snapshot()).unwrap(),
                serde_json::to_value(compact.snapshot()).unwrap()
            );
            assert!(compact.feed(b"").is_empty());
        }
        let snapshot: Snapshot =
            serde_json::from_value(serde_json::to_value(terminal.compact_snapshot()).unwrap())
                .unwrap();
        let mut restored = snapshot.restore().unwrap();
        restored.feed(b"2mcontinued");
        terminal.feed(b"2mcontinued");
        assert_eq!(restored.capture(), terminal.capture());
    }

    #[test]
    fn agent_status_survives_fragmented_snapshots_without_replaying_host_actions() {
        use rustty_vt::agent::Event;
        let begin =
            Event::from_json(br#"{"op":"begin","state":"working","thread_id":"thread"}"#).unwrap();
        let update =
            Event::from_json(br#"{"op":"update","state":"done","turn_id":"turn"}"#).unwrap();
        let mut stream = b"\x1b]133;C\x07".to_vec();
        stream.extend(begin.frame().unwrap());
        stream.extend(b"\x07\x1b]52;c;Y2xpcGJvYXJk\x07");
        stream.extend(update.frame().unwrap());
        for split in 0..=stream.len() {
            let mut owner = Terminal::new(20, 4, 20);
            owner.feed(&stream[..split]);
            let snapshot = owner.snapshot();
            let mut restored = snapshot.restore().unwrap();
            assert!(restored.feed(b"").is_empty());
            owner.feed(&stream[split..]);
            restored.feed(&stream[split..]);
            assert_eq!(restored.agent_status(), owner.agent_status());
            assert!(restored.agent_status().enclosing_command);
            assert!(matches!(
                restored.agent_status().snapshot.as_ref().unwrap().state,
                rustty_vt::agent::State::Done
            ));
            restored.feed(b"\x1b]133;D;0\x07");
            assert!(restored.agent_status().snapshot.is_none());
        }
    }

    #[test]
    fn agent_registration_replacement_end_and_invalid_snapshot_are_explicit() {
        use rustty_vt::agent::Event;
        let begin = Event::from_json(br#"{"op":"begin","state":"idle"}"#).unwrap();
        let update = Event::from_json(br#"{"op":"update","state":"working"}"#).unwrap();
        let mut terminal = Terminal::new(20, 4, 20);
        terminal.feed(&update.frame().unwrap());
        assert!(terminal.agent_status().snapshot.is_none());
        terminal.feed(&begin.frame().unwrap());
        let first = terminal.agent_status().clone();
        assert!(first.event_since(&first).is_none());
        terminal.feed(&begin.frame().unwrap());
        assert!(matches!(
            terminal.agent_status().event_since(&first),
            Some(Event::Begin(_))
        ));
        let mut invalid = terminal.snapshot();
        invalid.agent_status.snapshot.as_mut().unwrap().label = Some("x".repeat(129));
        assert!(invalid.restore().is_err());
        terminal.feed(&Event::End.frame().unwrap());
        assert_eq!(
            terminal.agent_status().event_since(&first),
            Some(Event::End)
        );
        assert!(
            terminal
                .snapshot()
                .restore()
                .unwrap()
                .agent_status()
                .snapshot
                .is_none()
        );
    }

    #[test]
    fn legacy_rename_and_apc_title_resume_across_snapshot_boundaries() {
        use super::*;
        for bytes in [b"\x1bkrename\x1b\\".as_slice(), b"\x1b_apc title\x1b\\"] {
            for split in 1..bytes.len() {
                let mut terminal = Terminal::new(20, 3, 0);
                let first = terminal.feed(&bytes[..split]);
                let mut restored = terminal.snapshot().restore().unwrap();
                assert_eq!(
                    terminal.feed(&bytes[split..]),
                    restored.feed(&bytes[split..]),
                    "split {split}"
                );
                assert_eq!(terminal.title(), restored.title());
                assert_eq!(terminal.capture(), restored.capture());
                if split + 1 < bytes.len() {
                    assert!(first.is_empty());
                }
            }
        }
    }
    use super::*;
    #[test]
    fn host_state_focus_gate_and_subscriptions_are_idempotent() {
        let mut terminal = Terminal::new(80, 24, 10);
        assert!(
            terminal
                .set_host_state(false, false, Some(true), false)
                .is_empty()
        );
        assert!(terminal.feed(b"\x1b[?1004h").is_empty());
        terminal.feed(b"\x1b[?2031h\x1b[?2033h");
        assert_eq!(
            terminal.set_host_state(true, true, Some(false), true),
            [Effect::Write(b"\x1b[I\x1b[?999;1n\x1b[?997;2n".to_vec())]
        );
        assert!(
            terminal
                .set_host_state(true, true, Some(false), true)
                .is_empty()
        );
        assert_eq!(
            terminal.set_host_state(false, false, Some(true), false),
            [Effect::Write(b"\x1b[?999;2n\x1b[?997;1n".to_vec())]
        );
        assert!(terminal.feed(b"\x1b[?1004l\x1b[?1004h").is_empty());
    }

    #[test]
    fn snapshots_keep_both_screens_history_rendition_and_pending_input() {
        let mut terminal = Terminal::new(8, 2, 20);
        terminal.feed(b"first\r\nsecond\r\n\x1b[31mthird\x1b[?1049h\x1b[HALT\x1b[");
        let snapshot = terminal.snapshot();
        assert!(snapshot.alternate_active);
        assert_eq!(snapshot.primary.history_len(), 1);
        assert!(
            snapshot
                .parser_continuation
                .as_ref()
                .unwrap()
                .ends_with(b"\x1b[")
        );
        let json = serde_json::to_vec(&snapshot).unwrap();
        let decoded: Snapshot = serde_json::from_slice(&json).unwrap();
        assert_eq!(decoded.primary.row_text(decoded.primary.row(1)), "third");
        let alternate = decoded.alternate.unwrap();
        assert_eq!(alternate.row_text(alternate.row(0)), "ALT");
        assert_eq!(
            decoded.primary.row(1).style(0).foreground,
            Color::Indexed(1)
        );
    }

    #[test]
    fn query_reply_is_emitted_once_and_snapshot_does_not_reissue_effects() {
        let mut terminal = Terminal::new(8, 3, 20);
        terminal.feed(b"\x1b[2;4H");
        let effects = terminal.feed(b"\x1b[6n");
        assert_eq!(effects, [Effect::Write(b"\x1b[2;4R".to_vec())]);
        terminal.snapshot();
        assert!(terminal.feed(b"").is_empty());
        assert_eq!(terminal.cursor(), (3, 1));
    }

    #[test]
    fn capture_ranges_wrap_history_and_resize() {
        let mut terminal = Terminal::new(4, 2, 20);
        terminal.feed(b"abcdef\r\nlast");
        assert_eq!(
            terminal.capture_lines(Some(-1), None, true),
            "abcdef\nlast\n"
        );
        assert_eq!(terminal.capture_lines(Some(-1), Some(-1), false), "abcd\n");
        terminal.resize(8, 3);
        assert_eq!(terminal.size(), (8, 3));
        assert!(terminal.capture().contains("abcdef"));
    }

    #[test]
    fn capture_swaps_reversed_bounds_and_join_keeps_written_spaces() {
        let mut terminal = Terminal::new(8, 2, 20);
        terminal.feed(b"one  \r\ntwo");
        assert_eq!(
            terminal.capture_lines(Some(1), Some(0), false),
            "one\ntwo\n"
        );
        assert_eq!(terminal.capture_lines(None, None, true), "one  \ntwo\n");
    }

    #[test]
    fn arbitrary_delivery_keeps_unicode_and_escape_state() {
        let bytes = "a\u{301}界\x1b[31mred\x1b[0m\r\nend".as_bytes();
        let mut whole = Terminal::new(12, 3, 20);
        whole.feed(bytes);
        for split in 0..=bytes.len() {
            let mut terminal = Terminal::new(12, 3, 20);
            terminal.feed(&bytes[..split]);
            terminal.feed(&bytes[split..]);
            assert_eq!(terminal.capture(), whole.capture());
            assert_eq!(terminal.cursor(), whole.cursor());
        }
    }

    #[test]
    fn restore_resumes_chunked_graphics_uploads() {
        let mut terminal = Terminal::new(12, 3, 20);
        terminal.feed(b"\x1b_Ga=T,f=32,s=1,v=1,i=7,m=1;////\x1b\\");
        let mut restored = terminal.snapshot().restore().unwrap();
        let end = b"\x1b_Gm=0;/w==\x1b\\";
        assert_eq!(restored.feed(end), terminal.feed(end));
        let expected = serde_json::to_value(terminal.snapshot().primary_graphics).unwrap();
        assert_eq!(
            serde_json::to_value(restored.snapshot().primary_graphics).unwrap(),
            expected
        );
        assert_eq!(
            restored.snapshot().primary_graphics.images[0]
                .pixels
                .as_slice(),
            &[255; 4]
        );
    }

    #[test]
    fn restore_retains_pending_state_and_graphics_without_replaying_effects() {
        let mut terminal = Terminal::new(12, 3, 20);
        terminal.feed(b"\x1b_Ga=T,f=32,s=1,v=1,i=7;/////w==\x1b\\\x1b]2;title\x07\x1b[31mred\x1b[");
        let snapshot = terminal.snapshot();
        assert_eq!(snapshot.primary_graphics.images.len(), 1);
        let mut restored = snapshot.restore().unwrap();
        assert_eq!(restored.title(), b"title");
        assert_eq!(restored.capture(), terminal.capture());
        assert!(restored.feed(b"").is_empty());
        assert_eq!(restored.feed(b"0mnext"), terminal.feed(b"0mnext"));
        assert_eq!(restored.capture(), terminal.capture());
        assert_eq!(
            restored.snapshot().primary_graphics.images[0].pixels.len(),
            4
        );
        let mut malformed = snapshot;
        malformed.primary_graphics.images[0].width = u32::MAX;
        assert!(malformed.restore().is_err());
    }
}
