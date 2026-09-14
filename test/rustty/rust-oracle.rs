//! Test-only NDJSON adapter for comparisons with the original Zig terminal.
use rustty_vt::{
    Color, CursorShape, Effect, EffectHandler, HyperlinkId, Screen, SemanticContent, Style,
    Terminal, clipboard, color, modes::Modes, query,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{self, BufRead, Read, Write};

#[path = "rust-dnd.rs"]
mod dnd_adapter;
#[path = "rust-glyph.rs"]
mod glyph_adapter;
#[path = "rust-graphics.rs"]
mod graphics_adapter;
#[path = "rust-grid.rs"]
mod grid_adapter;
#[path = "rust-input.rs"]
mod input;
#[path = "rust-page-layout.rs"]
mod page_layout_adapter;
#[path = "rust-pages.rs"]
mod pages_adapter;
#[path = "rust-parser.rs"]
mod parser;
#[path = "rust-paste.rs"]
mod paste;
#[path = "rust-semantic.rs"]
mod semantic_adapter;

const CAPABILITIES: &[&str] = &[
    "drag-and-drop",
    "terminal.pages",
    "graphics.glyphs",
    "terminal.page-layout",
    "terminal.selection",
    "terminal.search",
    "terminal.tracked",
    "graphics.kitty",
    "graphics.png",
    "terminal.write",
    "terminal.resize",
    "terminal.reset",
    "terminal.observe",
    "terminal.cells",
    "terminal.styles",
    "terminal.screens",
    "terminal.cursor",
    "terminal.modes",
    "terminal.colors",
    "effects.pty",
    "effects.title",
    "effects.pwd",
    "effects.bell",
    "effects.host",
    "clipboard",
    "unicode.width",
    "input.key",
    "input.mouse",
    "input.focus-paste",
    "parser.raw-events",
    "snapshot.cross-decode",
    "snapshot.streaming",
    "snapshot.fixtures",
    "protocol.dcs",
];
const MAX_REQUEST_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Request {
    id: String,
    kind: String,
    cols: u16,
    rows: u16,
    scalar: bool,
    operations: Vec<Operation>,
    codepoints: Vec<u32>,
    clipboard_replies: Vec<ClipboardReply>,
    clipboard_read_enabled: bool,
    clipboard_write_enabled: bool,
    clipboard_write_limit: usize,
    host: HostOptions,
    observe_modes: Vec<ModeTag>,
    observe_mode_effects: bool,
    observe_colors: bool,
    observe_semantic: bool,
    observe_graphics: bool,
    observe_graphics_placements: bool,
    dnd_events: bool,
    color_inputs: Vec<String>,
    page_layout: Option<page_layout_adapter::Request>,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            id: "case".into(),
            kind: "terminal".into(),
            cols: 12,
            rows: 4,
            scalar: false,
            operations: Vec::new(),
            codepoints: Vec::new(),
            clipboard_replies: Vec::new(),
            clipboard_read_enabled: true,
            clipboard_write_enabled: true,
            clipboard_write_limit: 64 * 1024 * 1024,
            host: HostOptions::default(),
            observe_modes: Vec::new(),
            observe_mode_effects: false,
            observe_colors: false,
            observe_semantic: false,
            observe_graphics: false,
            observe_graphics_placements: false,
            dnd_events: true,
            color_inputs: Vec::new(),
            page_layout: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    op: String,
    #[serde(default)]
    now_ms: Option<u64>,
    #[serde(default)]
    snapshot_max_continuation_bytes: Option<usize>,
    #[serde(default)]
    glyph_max_bytes: Option<usize>,
    #[serde(default)]
    data: String,
    #[serde(default)]
    cols: u16,
    #[serde(default)]
    rows: u16,
    #[serde(default)]
    input: Option<input::Event>,
    #[serde(default)]
    paste: Option<paste::Options>,
    #[serde(default)]
    clipboard_read_enabled: Option<bool>,
    #[serde(default)]
    clipboard_write_enabled: Option<bool>,
    #[serde(default)]
    clipboard_write_limit: Option<usize>,
    #[serde(default)]
    host: Option<HostOptions>,
    #[serde(default)]
    cell_size: Option<[u32; 2]>,
    #[serde(default)]
    number: u16,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    value: bool,
    #[serde(default = "default_cursor_shape")]
    cursor_shape: String,
    #[serde(default = "default_cursor_blink")]
    cursor_blink: Option<bool>,
    #[serde(default)]
    colors: Option<ColorDefaults>,
    #[serde(default)]
    grid: Option<grid_adapter::Operation>,
    #[serde(default)]
    dnd: Option<dnd_adapter::Operation>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ColorDefaults {
    foreground: Option<[u8; 3]>,
    background: Option<[u8; 3]>,
    cursor: Option<[u8; 3]>,
    palette: Option<Vec<[u8; 3]>>,
}

fn default_cursor_shape() -> String {
    "block".into()
}

fn default_cursor_blink() -> Option<bool> {
    Some(false)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModeTag {
    number: u16,
    private: bool,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum HostScheme {
    None,
    Light,
    Dark,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HostSize {
    rows: u16,
    columns: u16,
    cell_width: u32,
    cell_height: u32,
    available: bool,
}
impl Default for HostSize {
    fn default() -> Self {
        Self {
            rows: 24,
            columns: 80,
            cell_width: 9,
            cell_height: 18,
            available: true,
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HostAttributes {
    conformance_level: u16,
    features: Vec<u16>,
    device_type: u16,
    firmware_version: u16,
    rom_cartridge: u16,
    unit_id: u32,
}
impl Default for HostAttributes {
    fn default() -> Self {
        Self {
            conformance_level: 62,
            features: vec![22],
            device_type: 1,
            firmware_version: 0,
            rom_cartridge: 0,
            unit_id: 0,
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HostOptions {
    color_scheme: Option<HostScheme>,
    device_attributes: Option<HostAttributes>,
    size: Option<HostSize>,
    enquiry: Option<String>,
    xtversion: Option<String>,
    terminfo_name: Option<String>,
    title_report: bool,
    visible: bool,
}
impl Default for HostOptions {
    fn default() -> Self {
        Self {
            color_scheme: None,
            device_attributes: None,
            size: None,
            enquiry: None,
            xtversion: None,
            terminfo_name: None,
            title_report: false,
            visible: true,
        }
    }
}

struct DecodedHost {
    color_scheme: Option<HostScheme>,
    device_attributes: Option<query::DeviceAttributes>,
    size: Option<HostSize>,
    enquiry: Option<Vec<u8>>,
    xtversion: Option<Vec<u8>>,
    terminfo_name: Option<Vec<u8>>,
    title_report: bool,
    visible: bool,
}
impl DecodedHost {
    fn new(options: &HostOptions) -> Result<Self, &'static str> {
        Ok(Self {
            color_scheme: options.color_scheme,
            device_attributes: options.device_attributes.as_ref().map(|value| {
                query::DeviceAttributes {
                    conformance_level: value.conformance_level,
                    features: value.features.clone(),
                    device_type: value.device_type,
                    firmware_version: value.firmware_version,
                    rom_cartridge: value.rom_cartridge,
                    unit_id: value.unit_id,
                }
            }),
            size: options.size,
            enquiry: options.enquiry.as_deref().map(unhex).transpose()?,
            xtversion: options.xtversion.as_deref().map(unhex).transpose()?,
            terminfo_name: options.terminfo_name.as_deref().map(unhex).transpose()?,
            title_report: options.title_report,
            visible: options.visible,
        })
    }

    fn configure(&self, terminal: &mut Terminal) {
        terminal.terminfo_name = self.terminfo_name.clone();
        terminal.title_report = self.title_report;
        terminal.visible = self.visible;
    }
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ClipboardStatus {
    #[default]
    Success,
    Denied,
    Unsupported,
    Busy,
    InvalidData,
    IoError,
    None,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ClipboardReply {
    status: ClipboardStatus,
    contents: Vec<ClipboardContent>,
    available: Vec<String>,
    remember: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipboardContent {
    mime: String,
    data: String,
}

#[derive(Default)]
struct HostReply {
    status: ClipboardStatus,
    contents: Vec<clipboard::Content>,
    available: Vec<Vec<u8>>,
    remember: bool,
}

struct Host {
    events: Vec<Value>,
    replies: std::collections::VecDeque<HostReply>,
    error: Option<&'static str>,
    clipboard_read_enabled: bool,
    clipboard_write_enabled: bool,
    clipboard_write_limit: usize,
    host: DecodedHost,
    dnd_events: bool,
}

impl Host {
    fn new(request: &Request) -> Result<Self, &'static str> {
        let mut host = Self {
            events: Vec::new(),
            replies: std::collections::VecDeque::new(),
            error: None,
            clipboard_read_enabled: request.clipboard_read_enabled,
            clipboard_write_enabled: request.clipboard_write_enabled,
            clipboard_write_limit: request.clipboard_write_limit,
            host: DecodedHost::new(&request.host)?,
            dnd_events: request.dnd_events,
        };
        for reply in &request.clipboard_replies {
            let contents = reply
                .contents
                .iter()
                .map(|content| {
                    Ok(clipboard::Content {
                        mime: unhex(&content.mime)?,
                        data: unhex(&content.data)?.into(),
                    })
                })
                .collect::<Result<Vec<_>, &'static str>>()?;
            let available = reply
                .available
                .iter()
                .map(|mime| unhex(mime))
                .collect::<Result<Vec<_>, _>>()?;
            host.replies.push_back(HostReply {
                status: reply.status,
                contents,
                available,
                remember: reply.remember,
            });
        }
        Ok(host)
    }

    fn record(&mut self, effect: Effect) -> Result<(), &'static str> {
        let (kind, data) = match effect {
            Effect::Write(bytes) => ("write", hex(&bytes)),
            Effect::Title(bytes) => ("title", hex(&bytes)),
            Effect::WorkingDirectory(bytes) => ("pwd", hex(&bytes)),
            Effect::Bell => ("bell", String::new()),
            Effect::Notification { title, body } => {
                let mut value = event("notification", String::new());
                value["notification"] = json!({"title":hex(&title),"body":hex(&body)});
                self.events.push(value);
                return Ok(());
            }
            Effect::Progress { state, value } => {
                let mut observed = event("progress", String::new());
                observed["progress"] = json!({"state":state,"value":value});
                self.events.push(observed);
                return Ok(());
            }
            // Unknown-sequence diagnostics are not an external terminal effect.
            Effect::UnknownSequence(_) => return Ok(()),
            _ => return Err("UnsupportedEffect"),
        };
        if kind == "write"
            && let Some(last) = self.events.last_mut()
            && last["kind"] == "write"
        {
            let combined = format!("{}{}", last["data"].as_str().unwrap(), data);
            last["data"] = json!(combined);
        } else {
            self.events.push(event(kind, data));
        }
        Ok(())
    }
}

impl EffectHandler for Host {
    fn effect(&mut self, effect: Effect) {
        if let Err(error) = self.record(effect) {
            self.error = Some(error);
        }
    }

    fn drag_and_drop(
        &mut self,
        value: rustty_vt::dnd::Event,
        state: Option<&rustty_vt::dnd::State>,
    ) {
        if self.dnd_events {
            let mut observed = event("dnd", hex(dnd_adapter::event_name(value).as_bytes()));
            observed["dnd_state"] = state.map(dnd_adapter::observe).unwrap_or(Value::Null);
            self.events.push(observed);
        }
    }

    fn color_scheme(&mut self) -> Option<query::ColorScheme> {
        let scheme = self.host.color_scheme?;
        self.events.push(event("query_color_scheme", String::new()));
        match scheme {
            HostScheme::None => None,
            HostScheme::Light => Some(query::ColorScheme::Light),
            HostScheme::Dark => Some(query::ColorScheme::Dark),
        }
    }

    fn device_attributes(&mut self) -> Option<query::DeviceAttributes> {
        let attributes = self.host.device_attributes.clone()?;
        self.events
            .push(event("query_device_attributes", String::new()));
        Some(attributes)
    }

    fn size(&mut self) -> Option<query::Size> {
        let size = self.host.size?;
        self.events.push(event("query_size", String::new()));
        size.available.then_some(query::Size {
            rows: size.rows,
            columns: size.columns,
            cell_width: size.cell_width,
            cell_height: size.cell_height,
        })
    }

    fn enquiry(&mut self) -> Vec<u8> {
        let Some(bytes) = &self.host.enquiry else {
            return Vec::new();
        };
        self.events.push(event("query_enquiry", String::new()));
        bytes.clone()
    }

    fn xtversion(&mut self) -> Vec<u8> {
        let Some(bytes) = &self.host.xtversion else {
            return Vec::new();
        };
        self.events.push(event("query_xtversion", String::new()));
        bytes.clone()
    }

    fn clipboard_read_enabled(&self) -> bool {
        self.clipboard_read_enabled
    }

    fn clipboard_write_enabled(&self) -> bool {
        self.clipboard_write_enabled
    }

    fn clipboard_read(&mut self, request: &clipboard::Read) -> clipboard::ReadResult {
        let mut observed = clipboard_event(
            "clipboard_read",
            request.location,
            &request.name,
            request.granted,
            request.can_remember,
        );
        observed["clipboard"]["mimes"] = json!(
            request
                .mimes
                .iter()
                .map(|mime| hex(mime))
                .collect::<Vec<_>>()
        );
        observed["clipboard"]["list"] = json!(request.list);
        self.events.push(observed);
        let reply = self.replies.pop_front().unwrap_or_default();
        match reply.status {
            ClipboardStatus::Success => clipboard::ReadResult::Success(clipboard::ReadSuccess {
                contents: reply.contents,
                available: reply.available,
                remember: reply.remember,
            }),
            ClipboardStatus::InvalidData => {
                self.error = Some("UnsupportedClipboardReadStatus");
                clipboard::ReadResult::Denied
            }
            ClipboardStatus::Denied | ClipboardStatus::None => clipboard::ReadResult::Denied,
            ClipboardStatus::Unsupported => clipboard::ReadResult::Unsupported,
            ClipboardStatus::Busy => clipboard::ReadResult::Busy,
            ClipboardStatus::IoError => clipboard::ReadResult::IoError,
        }
    }

    fn clipboard_write(&mut self, request: &clipboard::Write) -> clipboard::WriteResult {
        let mut observed = clipboard_event(
            "clipboard_write",
            request.location,
            &request.name,
            request.granted,
            request.can_remember,
        );
        observed["clipboard"]["contents"] = json!(
            request
                .contents
                .iter()
                .map(|content| { json!({"mime":hex(&content.mime),"data":hex(&content.data)}) })
                .collect::<Vec<_>>()
        );
        self.events.push(observed);
        let reply = self.replies.pop_front().unwrap_or_default();
        match reply.status {
            ClipboardStatus::Success => clipboard::WriteResult::Success {
                remember: reply.remember,
            },
            ClipboardStatus::Denied | ClipboardStatus::None => clipboard::WriteResult::Denied,
            ClipboardStatus::Unsupported => clipboard::WriteResult::Unsupported,
            ClipboardStatus::Busy => clipboard::WriteResult::Busy,
            ClipboardStatus::InvalidData => clipboard::WriteResult::InvalidData,
            ClipboardStatus::IoError => clipboard::WriteResult::IoError,
        }
    }
}

fn clipboard_event(
    kind: &str,
    location: clipboard::Location,
    name: &[u8],
    granted: bool,
    can_remember: bool,
) -> Value {
    let mut observed = event(kind, String::new());
    let location = match location {
        clipboard::Location::Standard => "standard",
        clipboard::Location::Selection => "selection",
        clipboard::Location::Primary => "primary",
    };
    observed["clipboard"] = json!({"location":location,"contents":[],"mimes":[],
        "list":false,"name":hex(name),"granted":granted,"can_remember":can_remember});
    observed
}

fn main() -> io::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::BufWriter::new(io::stdout().lock());
    loop {
        let mut line = Vec::new();
        if (&mut input)
            .take(MAX_REQUEST_BYTES + 1)
            .read_until(b'\n', &mut line)?
            == 0
        {
            break;
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.len() as u64 > MAX_REQUEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("request exceeds {MAX_REQUEST_BYTES} bytes"),
            ));
        }
        let response = match serde_json::from_slice::<Request>(&line) {
            Ok(request) => match execute(&request) {
                Ok(value) => value,
                Err(error) => response(&request.id, Some(error)),
            },
            Err(_) => response("invalid", Some("InvalidRequest")),
        };
        serde_json::to_writer(&mut output, &response)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}

fn response(id: &str, error: Option<&str>) -> Value {
    json!({"id":id,"ok":error.is_none(),"err":error,"capabilities":CAPABILITIES,
        "observations":[],"events":[],"widths":[],"parser":null,"snapshots":[],"snapshot_progress":[],"mode_results":[],"parsed_colors":[],"grid_results":[],"page_layout":null,"glyph_results":[],"page_results":[],"dnd_results":[],"graphics_ticks":[]})
}

fn execute(request: &Request) -> Result<Value, &'static str> {
    let mut result = response(&request.id, None);
    match request.kind.as_str() {
        "capabilities" => return Ok(result),
        "page_layout" => {
            result["page_layout"] = page_layout_adapter::run(
                request
                    .page_layout
                    .as_ref()
                    .unwrap_or(&page_layout_adapter::Request::default()),
            )?;
            return Ok(result);
        }
        "parser" => {
            result["parser"] = parser::run(&request.operations)?;
            return Ok(result);
        }
        "colors" => {
            let colors = request
                .color_inputs
                .iter()
                .map(|input| {
                    let bytes = unhex(input)?;
                    Ok(std::str::from_utf8(&bytes)
                        .ok()
                        .and_then(rustty_vt::parse_color))
                })
                .collect::<Result<Vec<_>, &'static str>>()?;
            result["parsed_colors"] = json!(colors);
            return Ok(result);
        }
        "unicode" => {
            let mut widths = Vec::new();
            for &cp in &request.codepoints {
                let cp = char::from_u32(cp).ok_or("InvalidCodepoint")?;
                widths.push(rustty_vt::unicode::codepoint_width(cp));
            }
            result["widths"] = json!(widths);
            return Ok(result);
        }
        "terminal" | "input" => {}
        _ => return Err("UnsupportedKind"),
    }
    dimensions(request.cols, request.rows)?;
    let mut terminal = Terminal::new(request.cols, request.rows, usize::MAX);
    terminal.clipboard_write_limit = request.clipboard_write_limit;
    if request.kind == "input" {
        terminal.set_pixel_size(u32::from(request.cols) * 8, u32::from(request.rows) * 16);
    }
    let mut observations = Vec::new();
    let mut mode_results = Vec::new();
    let mut mouse_cell = None;
    let mut grid = grid_adapter::Context::default();
    let mut grid_results = Vec::new();
    let mut glyph_results = Vec::new();
    let mut page_results = Vec::new();
    let mut dnd_results = Vec::new();
    let mut graphics_ticks = Vec::new();
    let mut host = Host::new(request)?;
    host.host.configure(&mut terminal);
    let mut snapshots = Vec::new();
    let mut snapshot_progress = Vec::new();
    let mut snapshot_decoder = None;
    let snapshot_offset = std::rc::Rc::new(std::cell::Cell::new(0));
    for operation in &request.operations {
        match operation.op.as_str() {
            "graphics_tick" => graphics_ticks.push(graphics_adapter::tick(
                &mut terminal,
                operation.now_ms.ok_or("MissingTime")?,
            )),
            "pages" => page_results.push(pages_adapter::observe(&terminal)),
            "dnd" => {
                let (state, bytes) = dnd_adapter::run(
                    &mut terminal,
                    operation.dnd.as_ref().ok_or("MissingDndOperation")?,
                )?;
                if !bytes.is_empty() {
                    host.effect(Effect::Write(bytes));
                }
                dnd_results.push(state);
            }
            "write" => {
                let bytes = unhex(&operation.data)?;
                if request.scalar {
                    for byte in &bytes {
                        terminal.feed_with_handler(std::slice::from_ref(byte), &mut host);
                    }
                } else {
                    terminal.feed_with_handler(&bytes, &mut host);
                }
            }
            "osc" => {
                let bytes = unhex(&operation.data)?;
                if let Some((&selector, payload)) = bytes.split_first() {
                    let terminator = match selector % 3 {
                        0 => Some(7),
                        1 => Some(0x9c),
                        _ => None,
                    };
                    terminal.feed_osc_with_handler(payload, terminator, &mut host);
                }
            }
            "resize" => {
                dimensions(operation.cols, operation.rows)?;
                for effect in terminal.resize_with_cell_size(
                    operation.cols,
                    operation.rows,
                    operation.cell_size.map(|value| (value[0], value[1])),
                ) {
                    host.effect(effect);
                }
            }
            "reset" => terminal.feed_with_handler(b"\x1bc", &mut host),
            "terminal_reset" => terminal.reset(),
            "title_set" => terminal.set_title(&unhex(&operation.data)?),
            "pwd_set" => terminal.set_working_directory(&unhex(&operation.data)?),
            "mode_set" => mode_results.push(terminal.modes.set(
                operation.private,
                operation.number,
                operation.value,
            )),
            "mode_default" => mode_results.push(terminal.set_default_mode(
                operation.private,
                operation.number,
                operation.value,
            )),
            "mode_raw_default" => mode_results.push(terminal.modes.set_default(
                operation.private,
                operation.number,
                operation.value,
            )),
            "mode_save" | "mode_restore" => {
                let known = terminal
                    .modes
                    .get_default(operation.private, operation.number)
                    .is_some();
                if operation.op == "mode_save" {
                    terminal.modes.save(operation.private, operation.number);
                } else {
                    terminal.modes.restore(operation.private, operation.number);
                }
                mode_results.push(known);
            }
            "modes_reset" => terminal.modes.reset(),
            "cursor_defaults" => {
                let shape = match operation.cursor_shape.as_str() {
                    "block" => CursorShape::Block,
                    "bar" => CursorShape::Bar,
                    "underline" => CursorShape::Underline,
                    "block_hollow" => CursorShape::HollowBlock,
                    _ => return Err("InvalidCursorShape"),
                };
                terminal.set_default_cursor(shape, operation.cursor_blink);
            }
            "color_defaults" => {
                let colors = operation.colors.as_ref().ok_or("MissingColorDefaults")?;
                let palette = colors
                    .palette
                    .clone()
                    .unwrap_or_else(rustty_vt::default_palette);
                if palette.len() != 256 {
                    return Err("InvalidPalette");
                }
                terminal.set_default_colors(
                    colors.foreground,
                    colors.background,
                    colors.cursor,
                    &palette,
                );
            }
            "host_options" => {
                host.host = DecodedHost::new(operation.host.as_ref().ok_or("MissingHostOptions")?)?;
                host.host.configure(&mut terminal);
            }
            "clipboard_options" => {
                if let Some(value) = operation.clipboard_read_enabled {
                    host.clipboard_read_enabled = value;
                }
                if let Some(value) = operation.clipboard_write_enabled {
                    host.clipboard_write_enabled = value;
                }
                if let Some(value) = operation.clipboard_write_limit {
                    host.clipboard_write_limit = value;
                    terminal.clipboard_write_limit = value;
                }
            }
            "observe" => {
                observations.push(observe(&terminal, request));
            }
            "glyph_observe" => glyph_results.push(glyph_adapter::observe(&terminal.glyphs)),
            "glyph_enable" => terminal.glyphs.set_enabled(operation.value),
            "glyph_limit" => terminal.glyphs.set_apc_limit(operation.glyph_max_bytes),
            "glyph_clean" => terminal.glyphs.clear_dirty(),
            "grid" => grid_results.push(grid.run(
                &mut terminal,
                operation.grid.as_ref().ok_or("MissingGridOperation")?,
            )?),
            "input" => {
                let bytes = input::encode(
                    &terminal,
                    operation.input.as_ref().ok_or("MissingInput")?,
                    &mut mouse_cell,
                )?;
                host.events.push(event("input", hex(&bytes)));
            }
            "paste" => paste::run(
                &mut terminal,
                &mut host,
                operation.paste.as_ref().ok_or("MissingPaste")?,
            )?,
            "checkpoint" => {
                observations.clear();
                host.events.clear();
                mode_results.clear();
                grid_results.clear();
                glyph_results.clear();
                page_results.clear();
                dnd_results.clear();
                graphics_ticks.clear();
            }
            "snapshot" => {
                snapshots.push(hex(&rustty_vt::snapshot::encode_to_vec(&terminal)
                    .map_err(|_| "SnapshotEncodeFailed")?));
            }
            "restore" | "restore_exact" => {
                if grid.has_handles() {
                    return Err("UnsupportedGridRestore");
                }
                let data = unhex(&operation.data)?;
                let options = rustty_vt::snapshot::DecodeOptions {
                    max_continuation_bytes: operation
                        .snapshot_max_continuation_bytes
                        .unwrap_or(8 * 1024 * 1024),
                    ..Default::default()
                };
                terminal = if operation.op == "restore_exact" {
                    rustty_vt::snapshot::decode_exact(data.as_slice(), options)
                } else {
                    rustty_vt::snapshot::decode(data.as_slice(), options)
                }
                .map_err(|_| "InvalidSnapshot")?;
                terminal.clipboard_write_limit = host.clipboard_write_limit;
                host.host.configure(&mut terminal);
                snapshot_decoder = None;
            }
            "restore_ready" => {
                if grid.has_handles() {
                    return Err("UnsupportedGridRestore");
                }
                snapshot_offset.set(0);
                let mut decoder = rustty_vt::snapshot::Decoder::new(
                    SnapshotReader {
                        data: io::Cursor::new(unhex(&operation.data)?),
                        offset: snapshot_offset.clone(),
                    },
                    rustty_vt::snapshot::DecodeOptions {
                        max_continuation_bytes: operation
                            .snapshot_max_continuation_bytes
                            .unwrap_or(8 * 1024 * 1024),
                        ..Default::default()
                    },
                );
                terminal = decoder.ready().map_err(|_| "InvalidSnapshot")?;
                terminal.clipboard_write_limit = host.clipboard_write_limit;
                host.host.configure(&mut terminal);
                snapshot_progress.push(json!({"stage":"ready", "offset":snapshot_offset.get(),
                    "history_rows":decoder.history_rows(), "screen":null, "rows":0, "remaining":0}));
                snapshot_decoder = Some(decoder);
            }
            "restore_next" => {
                let progress = snapshot_decoder
                    .as_mut()
                    .ok_or("MissingSnapshotDecoder")?
                    .next_history(&mut terminal)
                    .map_err(|_| "InvalidSnapshot")?;
                snapshot_progress.push(match progress {
                    Some(value) => json!({"stage":"history", "offset":snapshot_offset.get(),
                        "history_rows":[0,0], "screen":value.screen, "rows":value.rows, "remaining":value.remaining_pages}),
                    None => json!({"stage":"finish", "offset":snapshot_offset.get(),
                        "history_rows":[0,0], "screen":null, "rows":0, "remaining":0}),
                });
            }
            _ => return Err("UnsupportedOperation"),
        };
        if let Some(error) = host.error {
            return Err(error);
        }
    }
    if request.kind == "terminal" {
        observations.push(observe(&terminal, request));
    }
    result["observations"] = json!(observations);
    result["events"] = json!(host.events);
    result["snapshots"] = json!(snapshots);
    result["snapshot_progress"] = json!(snapshot_progress);
    result["mode_results"] = json!(mode_results);
    result["grid_results"] = json!(grid_results);
    result["glyph_results"] = json!(glyph_results);
    result["page_results"] = json!(page_results);
    result["dnd_results"] = json!(dnd_results);
    result["graphics_ticks"] = json!(graphics_ticks);
    Ok(result)
}

fn event(kind: &str, data: String) -> Value {
    json!({"kind":kind,"data":data,"notification":null,"progress":null,"clipboard":null,"dnd_state":null})
}

struct SnapshotReader {
    data: io::Cursor<Vec<u8>>,
    offset: std::rc::Rc<std::cell::Cell<usize>>,
}
impl Read for SnapshotReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let n = self.data.read(bytes)?;
        self.offset.set(self.offset.get() + n);
        Ok(n)
    }
}

fn dimensions(cols: u16, rows: u16) -> Result<(), &'static str> {
    if cols == 0 || rows == 0 || cols > 1024 || rows > 1024 {
        Err("InvalidDimensions")
    } else {
        Ok(())
    }
}

fn observe(terminal: &Terminal, request: &Request) -> Value {
    let m = &terminal.margins;
    let modes: Vec<_> = request
        .observe_modes
        .iter()
        .map(|tag| {
            let default = terminal.modes.get_default(tag.private, tag.number);
            json!({"number":tag.number,"private":tag.private,
            "current":default.map(|_| terminal.modes.get(tag.private,tag.number)),
            "saved":terminal.modes.get_saved(tag.private,tag.number),"default":default,
            "report":terminal.modes.report(tag.private,tag.number),
            "default_configurable":Modes::default_configurable(tag.private,tag.number)})
        })
        .collect();
    let mode_effects = request.observe_mode_effects.then(|| {
        let cursor = &terminal.screen().cursor;
        json!({"cursor_visible":cursor.visible,"cursor_blink":cursor.blink,
            "mouse_mode":terminal.mouse_mode,"mouse_format":terminal.mouse_format})
    });
    let colors = request.observe_colors.then(|| {
        let state = |target| json!({"current":terminal.color_current(target),
            "default":terminal.color_default(target),"override":terminal.color_override(target)});
        json!({"foreground":state(color::Target::Foreground),
            "background":state(color::Target::Background),"cursor":state(color::Target::Cursor),
            "palette":(0..=255).map(|index| state(color::Target::Palette(index))).collect::<Vec<_>>()})
    });
    json!({"cols":terminal.cols,"rows":terminal.rows,
        "alternate_active":terminal.is_alternate_screen(),
        "primary":screen(terminal.primary_screen()),
        "alternate":terminal.alternate_screen().map(screen),
        "margins":[m.top,m.bottom,m.left,m.right],
        "title":hex(terminal.title_bytes()),"pwd":hex(terminal.working_directory_bytes()),
        "modes":modes,"mode_effects":mode_effects,"colors":colors,
        "semantic":request.observe_semantic.then(|| semantic_adapter::observe(terminal)),
        "graphics":request.observe_graphics.then(|| graphics_adapter::observe(terminal)),
        "graphics_placements":request.observe_graphics_placements.then(|| graphics_adapter::observe_placements(terminal))})
}

fn screen(screen: &Screen) -> Value {
    let c = &screen.cursor;
    json!({"cursor":{"x":c.col,"y":c.row,"pending_wrap":c.pending_wrap,
        "shape":match c.shape {
            CursorShape::Block => "block", CursorShape::Bar => "bar",
            CursorShape::Underline => "underline", CursorShape::HollowBlock => "block_hollow",
        },"style":style(c.style),"hyperlink":hyperlink(&c.hyperlink, &c.hyperlink_raw, &c.hyperlink_id),
        "protected":c.protected,"semantic":semantic(c.semantic)},
        "rows":screen.rows.iter().map(row).collect::<Vec<_>>(),
        "history":screen.history.iter().map(row).collect::<Vec<_>>()})
}

fn row(row: &rustty_vt::Row) -> Value {
    json!({"wrapped":row.wrapped,"cells":row.cells.iter().map(|cell| json!({
        "text":cell.text.chars().map(u32::from).collect::<Vec<_>>(),
        "width":cell.width,"spacer_head":cell.spacer_head,"style":style(cell.style),
        "hyperlink":hyperlink(&cell.hyperlink, &cell.hyperlink_raw, &cell.hyperlink_id),
        "protected":cell.protected,
        "semantic":semantic(cell.semantic)})).collect::<Vec<_>>()})
}

fn hyperlink(
    text: &Option<String>,
    raw: &Option<Vec<u8>>,
    id: &Option<HyperlinkId>,
) -> Option<Value> {
    raw.as_deref()
        .or_else(|| text.as_deref().map(str::as_bytes))
        .map(|uri| {
            let (explicit, implicit) = match id {
                Some(HyperlinkId::Explicit(bytes)) => (Some(hex(bytes)), None),
                Some(HyperlinkId::Implicit(number)) => (None, Some(*number)),
                None => (None, None),
            };
            json!({"uri":hex(uri),"explicit":explicit,"implicit":implicit})
        })
}

fn semantic(value: SemanticContent) -> &'static str {
    match value {
        SemanticContent::Output => "output",
        SemanticContent::Prompt => "prompt",
        SemanticContent::Input => "input",
    }
}

fn style(s: Style) -> Value {
    json!({"foreground":color(s.foreground),"background":color(s.background),
        "underline_color":color(s.underline_color),"bold":s.bold,"faint":s.faint,
        "italic":s.italic,"blink":s.blink,"inverse":s.inverse,"invisible":s.invisible,
        "strikethrough":s.strikethrough,"overline":s.overline,
        "underline":format!("{:?}",s.underline).to_lowercase()})
}

fn color(color: Color) -> Value {
    match color {
        Color::Default => json!({"kind":"default","value":[]}),
        Color::Indexed(value) => json!({"kind":"indexed","value":[value]}),
        Color::Rgb(r, g, b) => json!({"kind":"rgb","value":[r,g,b]}),
    }
}

fn unhex(text: &str) -> Result<Vec<u8>, &'static str> {
    if !text.len().is_multiple_of(2) {
        return Err("InvalidHex");
    }
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16).ok_or("InvalidHex")?;
            let lo = (pair[1] as char).to_digit(16).ok_or("InvalidHex")?;
            Ok((hi * 16 + lo) as u8)
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").unwrap();
    }
    output
}
