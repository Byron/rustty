//! Test-only NDJSON process adapter around the original Zig terminal.
//! No code from this executable is linked into the Rust application.
const std = @import("std");
const vt = @import("ghostty-vt");
const input_adapter = @import("zig-input.zig");
const parser_adapter = @import("zig-parser.zig");
const paste_adapter = @import("zig-paste.zig");
const semantic_adapter = @import("zig-semantic.zig");
const Allocator = std.mem.Allocator;
// libghostty-vt exposes this type through the callback without re-exporting
// the implementation module. Use that public signature as the source of truth.
const DeviceAttributesFn = @typeInfo(@FieldType(vt.TerminalStream.Handler.Effects, "device_attributes")).optional.child;
const DeviceAttributes = @typeInfo(@typeInfo(DeviceAttributesFn).pointer.child).@"fn".return_type.?;

pub const std_options: std.Options = .{ .log_level = .err };

const capabilities = [_][]const u8{
    "protocol.dcs",
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
};

const Operation = struct {
    op: []const u8,
    data: []const u8 = "",
    cols: u16 = 0,
    rows: u16 = 0,
    input: ?input_adapter.Event = null,
    paste: ?paste_adapter.Options = null,
    clipboard_read_enabled: ?bool = null,
    clipboard_write_enabled: ?bool = null,
    clipboard_write_limit: ?usize = null,
    host: ?HostOptions = null,
    cell_size: ?[2]u32 = null,
    number: u16 = 0,
    private: bool = false,
    value: bool = false,
    cursor_shape: []const u8 = "block",
    cursor_blink: ?bool = false,
    colors: ?ColorDefaults = null,
};
const Request = struct {
    id: []const u8 = "case",
    kind: []const u8 = "terminal",
    cols: u16 = 12,
    rows: u16 = 4,
    scalar: bool = false,
    operations: []const Operation = &.{},
    codepoints: []const u32 = &.{},
    clipboard_replies: []const ClipboardReply = &.{},
    clipboard_read_enabled: bool = true,
    clipboard_write_enabled: bool = true,
    clipboard_write_limit: usize = 64 * 1024 * 1024,
    host: HostOptions = .{},
    observe_modes: []const ModeTag = &.{},
    observe_mode_effects: bool = false,
    observe_colors: bool = false,
    observe_semantic: bool = false,
    color_inputs: []const []const u8 = &.{},
};
const ColorDefaults = struct {
    foreground: ?[3]u8 = null,
    background: ?[3]u8 = null,
    cursor: ?[3]u8 = null,
    palette: ?[]const [3]u8 = null,
};
const ColorState = struct { current: ?[3]u16, default: ?[3]u16, override: ?[3]u16 };
const Colors = struct {
    foreground: ColorState,
    background: ColorState,
    cursor: ColorState,
    palette: [256]ColorState,
};
const ModeTag = struct { number: u16, private: bool };
const Mode = struct {
    number: u16,
    private: bool,
    current: ?bool,
    saved: ?bool,
    default: ?bool,
    report: u8,
    default_configurable: bool,
};
const ModeEffects = struct {
    cursor_visible: bool,
    cursor_blink: bool,
    mouse_mode: u16,
    mouse_format: u16,
};
const HostScheme = enum { none, light, dark };
const HostSize = struct {
    rows: u16 = 24,
    columns: u16 = 80,
    cell_width: u32 = 9,
    cell_height: u32 = 18,
    available: bool = true,
};
const HostAttributes = struct {
    conformance_level: u16 = 62,
    features: []const u16 = &.{22},
    device_type: u16 = 1,
    firmware_version: u16 = 0,
    rom_cartridge: u16 = 0,
    unit_id: u32 = 0,
};
const HostOptions = struct {
    color_scheme: ?HostScheme = null,
    device_attributes: ?HostAttributes = null,
    size: ?HostSize = null,
    enquiry: ?[]const u8 = null,
    xtversion: ?[]const u8 = null,
    terminfo_name: ?[]const u8 = null,
    title_report: bool = false,
    visible: bool = true,
};
const DecodedHost = struct {
    options: HostOptions,
    attributes: ?DeviceAttributes,
    enquiry: ?[]const u8,
    xtversion: ?[]const u8,
    terminfo_name: ?[]const u8,

    fn init(alloc: Allocator, options: HostOptions) !DecodedHost {
        const attributes: ?DeviceAttributes = if (options.device_attributes) |value| attrs: {
            const features = try alloc.alloc(@FieldType(DeviceAttributes, "primary").Feature, value.features.len);
            for (value.features, features) |source, *destination| destination.* = @enumFromInt(source);
            break :attrs .{
                .primary = .{ .conformance_level = @enumFromInt(value.conformance_level), .features = features },
                .secondary = .{ .device_type = @enumFromInt(value.device_type), .firmware_version = value.firmware_version, .rom_cartridge = value.rom_cartridge },
                .tertiary = .{ .unit_id = value.unit_id },
            };
        } else null;
        return .{
            .options = options,
            .attributes = attributes,
            .enquiry = if (options.enquiry) |value| try hexDecode(alloc, value) else null,
            .xtversion = if (options.xtversion) |value| try hexDecode(alloc, value) else null,
            .terminfo_name = if (options.terminfo_name) |value| try hexDecode(alloc, value) else null,
        };
    }
};
const ClipboardStatus = enum { success, denied, unsupported, busy, invalid_data, io_error, none };
const ClipboardReply = struct {
    status: ClipboardStatus = .success,
    contents: []const ClipboardContent = &.{},
    available: []const []const u8 = &.{},
    remember: bool = false,
};
const DecodedClipboardReply = struct {
    status: ClipboardStatus = .success,
    contents: []const vt.clipboard.Content = &.{},
    available: []const []const u8 = &.{},
    remember: bool = false,
};
const Color = struct { kind: []const u8 = "default", value: []const u16 = &.{} };
const Style = struct {
    foreground: Color,
    background: Color,
    underline_color: Color,
    bold: bool,
    faint: bool,
    italic: bool,
    blink: bool,
    inverse: bool,
    invisible: bool,
    strikethrough: bool,
    overline: bool,
    underline: []const u8,
};
const Hyperlink = struct {
    uri: []const u8,
    explicit: ?[]const u8 = null,
    implicit: ?u32 = null,
};
const Cell = struct {
    text: []const u21,
    width: u8,
    spacer_head: bool,
    style: Style,
    hyperlink: ?Hyperlink,
    protected: bool,
    semantic: []const u8,
};
const Row = struct { cells: []Cell, wrapped: bool };
const Cursor = struct {
    x: u16,
    y: u16,
    pending_wrap: bool,
    shape: []const u8,
    style: Style,
    hyperlink: ?Hyperlink,
    protected: bool,
    semantic: []const u8,
};
const Screen = struct { cursor: Cursor, rows: []Row, history: []Row };
const Observation = struct {
    cols: u16,
    rows: u16,
    alternate_active: bool,
    primary: Screen,
    alternate: ?Screen,
    margins: [4]u16,
    title: []const u8,
    pwd: []const u8,
    modes: []const Mode,
    mode_effects: ?ModeEffects,
    colors: ?Colors,
    semantic: ?semantic_adapter.State,
};
const Notification = struct { title: []const u8, body: []const u8 };
const Progress = struct { state: u8, value: ?u8 };
const ClipboardContent = struct { mime: []const u8, data: []const u8 };
const Clipboard = struct {
    location: []const u8,
    contents: []const ClipboardContent = &.{},
    mimes: []const []const u8 = &.{},
    list: bool = false,
    name: []const u8 = "",
    granted: bool = false,
    can_remember: bool = false,
};
const Event = struct {
    kind: []const u8,
    data: []const u8 = "",
    notification: ?Notification = null,
    progress: ?Progress = null,
    clipboard: ?Clipboard = null,
};
const SnapshotProgress = struct {
    stage: []const u8,
    offset: usize,
    history_rows: [2]u64 = .{ 0, 0 },
    screen: ?u8 = null,
    rows: usize = 0,
    remaining: u32 = 0,
};
const Response = struct {
    id: []const u8,
    ok: bool = true,
    err: ?[]const u8 = null,
    capabilities: []const []const u8 = &capabilities,
    observations: []Observation = &.{},
    events: []Event = &.{},
    widths: []const i8 = &.{},
    parser: ?parser_adapter.Result = null,
    snapshots: []const []const u8 = &.{},
    snapshot_progress: []const SnapshotProgress = &.{},
    mode_results: []const bool = &.{},
    parsed_colors: []const ?[3]u16 = &.{},
};

// Effects arrive synchronously; one terminal is exercised at a time. This
// test-only pointer avoids changing the production callback API for the oracle.
var current: ?*Context = null;
const Context = struct {
    alloc: Allocator,
    events: std.ArrayList(Event) = .empty,
    clipboard_replies: []const DecodedClipboardReply,
    clipboard_reply_index: usize = 0,
    invalid_read_status: bool = false,
    clipboard_read_enabled: bool,
    clipboard_write_enabled: bool,
    clipboard_write_limit: usize,
    host: DecodedHost,

    fn append(kind: []const u8, bytes: []const u8) void {
        const self = current.?;
        const data = hexEncode(self.alloc, bytes) catch @panic("oracle allocation failed");
        // PTY write callback boundaries are an optimization, not protocol data.
        if (std.mem.eql(u8, kind, "write") and self.events.items.len > 0) {
            const last = &self.events.items[self.events.items.len - 1];
            if (std.mem.eql(u8, last.kind, "write")) {
                last.data = std.mem.concat(self.alloc, u8, &.{ last.data, data }) catch @panic("oracle allocation failed");
                return;
            }
        }
        self.events.append(self.alloc, .{ .kind = kind, .data = data }) catch @panic("oracle allocation failed");
    }

    fn write(_: *vt.TerminalStream.Handler, bytes: []const u8) void {
        append("write", bytes);
    }
    fn bell(_: *vt.TerminalStream.Handler) void {
        append("bell", "");
    }
    fn title(h: *vt.TerminalStream.Handler) void {
        append("title", h.terminal.getTitle() orelse "");
    }
    fn pwd(h: *vt.TerminalStream.Handler) void {
        append("pwd", h.terminal.getPwd() orelse "");
    }
    fn record(event: Event) void {
        const self = current.?;
        self.events.append(self.alloc, event) catch @panic("oracle allocation failed");
    }
    fn encoded(bytes: []const u8) []const u8 {
        return hexEncode(current.?.alloc, bytes) catch @panic("oracle allocation failed");
    }
    fn notification(_: *vt.TerminalStream.Handler, value: vt.StreamAction.ShowDesktopNotification) void {
        record(.{ .kind = "notification", .notification = .{ .title = encoded(value.title), .body = encoded(value.body) } });
    }
    fn progress(_: *vt.TerminalStream.Handler, value: vt.osc.Command.ProgressReport) void {
        record(.{ .kind = "progress", .progress = .{ .state = @intCast(@intFromEnum(value.state)), .value = value.progress } });
    }
    fn colorScheme(_: *vt.TerminalStream.Handler) ?vt.device_status.ColorScheme {
        append("query_color_scheme", "");
        return switch (current.?.host.options.color_scheme.?) {
            .none => null,
            .light => .light,
            .dark => .dark,
        };
    }
    fn deviceAttributes(_: *vt.TerminalStream.Handler) DeviceAttributes {
        append("query_device_attributes", "");
        return current.?.host.attributes.?;
    }
    fn size(_: *vt.TerminalStream.Handler) ?vt.size_report.Size {
        append("query_size", "");
        const value = current.?.host.options.size.?;
        if (!value.available) return null;
        return .{ .rows = value.rows, .columns = value.columns, .cell_width = value.cell_width, .cell_height = value.cell_height };
    }
    fn enquiry(_: *vt.TerminalStream.Handler) []const u8 {
        append("query_enquiry", "");
        return current.?.host.enquiry.?;
    }
    fn xtversion(_: *vt.TerminalStream.Handler) []const u8 {
        append("query_xtversion", "");
        return current.?.host.xtversion.?;
    }
    fn nextClipboardReply() DecodedClipboardReply {
        const self = current.?;
        if (self.clipboard_reply_index >= self.clipboard_replies.len) return .{};
        defer self.clipboard_reply_index += 1;
        return self.clipboard_replies[self.clipboard_reply_index];
    }
    fn clipboardWrite(_: *vt.TerminalStream.Handler, value: vt.clipboard.Write) void {
        const contents = current.?.alloc.alloc(ClipboardContent, value.contents.len) catch @panic("oracle allocation failed");
        for (value.contents, contents) |source, *destination| {
            destination.* = .{ .mime = encoded(source.mime), .data = encoded(source.data) };
        }
        record(.{ .kind = "clipboard_write", .clipboard = .{
            .location = @tagName(value.location),
            .contents = contents,
            .name = encoded(value.name),
            .granted = value.granted,
            .can_remember = value.can_remember,
        } });
        const reply = nextClipboardReply();
        value.reply(switch (reply.status) {
            .success => .{ .success = .{ .remember = reply.remember } },
            .denied => .denied,
            .unsupported => .unsupported,
            .busy => .busy,
            .invalid_data => .invalid_data,
            .io_error => .io_error,
            .none => return,
        });
    }
    fn clipboardRead(_: *vt.TerminalStream.Handler, value: vt.clipboard.Read) void {
        const mimes = current.?.alloc.alloc([]const u8, value.mimes.len) catch @panic("oracle allocation failed");
        for (value.mimes, mimes) |source, *destination| destination.* = encoded(source);
        record(.{ .kind = "clipboard_read", .clipboard = .{
            .location = @tagName(value.location),
            .mimes = mimes,
            .list = value.list,
            .name = encoded(value.name),
            .granted = value.granted,
            .can_remember = value.can_remember,
        } });
        const reply = nextClipboardReply();
        value.reply(switch (reply.status) {
            .success => .{ .success = .{ .contents = reply.contents, .available = reply.available, .remember = reply.remember } },
            .denied => .denied,
            .unsupported => .unsupported,
            .busy => .busy,
            .io_error => .io_error,
            .none => return,
            .invalid_data => {
                current.?.invalid_read_status = true;
                return;
            },
        });
    }
};

pub fn main(init: std.process.Init) !void {
    var read_buffer: [64 * 1024]u8 = undefined;
    var write_buffer: [64 * 1024]u8 = undefined;
    var stdin = std.Io.File.stdin().readerStreaming(init.io, &read_buffer);
    var stdout = std.Io.File.stdout().writerStreaming(init.io, &write_buffer);
    while (true) {
        _ = stdin.interface.peekByte() catch |err| switch (err) {
            error.EndOfStream => break,
            else => return err,
        };
        var arena = std.heap.ArenaAllocator.init(init.gpa);
        defer arena.deinit();
        const alloc = arena.allocator();
        var line: std.Io.Writer.Allocating = .init(alloc);
        _ = try stdin.interface.streamDelimiterLimit(&line.writer, '\n', .limited(16 * 1024 * 1024));
        _ = stdin.interface.discardDelimiterInclusive('\n') catch {};
        const parsed = std.json.parseFromSlice(Request, alloc, line.written(), .{ .allocate = .alloc_always }) catch {
            try std.json.Stringify.value(Response{ .id = "invalid", .ok = false, .err = "InvalidRequest" }, .{}, &stdout.interface);
            try stdout.interface.writeByte('\n');
            try stdout.interface.flush();
            continue;
        };
        const response = execute(alloc, init.io, parsed.value) catch |err| Response{
            .id = parsed.value.id,
            .ok = false,
            .err = @errorName(err),
        };
        try std.json.Stringify.value(response, .{}, &stdout.interface);
        try stdout.interface.writeByte('\n');
        try stdout.interface.flush();
    }
}

fn execute(alloc: Allocator, io: std.Io, request: Request) !Response {
    var response: Response = .{ .id = request.id };
    if (std.mem.eql(u8, request.kind, "capabilities")) return response;
    if (std.mem.eql(u8, request.kind, "parser")) {
        response.parser = try parser_adapter.run(alloc, request.operations);
        return response;
    }
    if (std.mem.eql(u8, request.kind, "colors")) {
        const colors = try alloc.alloc(?[3]u16, request.color_inputs.len);
        for (request.color_inputs, colors) |input, *result| {
            result.* = rgbBytes(vt.color.RGB.parse(try hexDecode(alloc, input)) catch null);
        }
        response.parsed_colors = colors;
        return response;
    }
    if (std.mem.eql(u8, request.kind, "unicode")) {
        const widths = try alloc.alloc(i8, request.codepoints.len);
        for (request.codepoints, widths) |cp, *width| {
            if (cp > 0x10ffff or (cp >= 0xd800 and cp <= 0xdfff)) return error.InvalidCodepoint;
            width.* = @intCast(vt.unicode.codepointWidth(@intCast(cp)));
        }
        response.widths = widths;
        return response;
    }
    const observe_terminal = std.mem.eql(u8, request.kind, "terminal");
    if (!observe_terminal and !std.mem.eql(u8, request.kind, "input")) return error.UnsupportedKind;
    if (request.cols == 0 or request.rows == 0 or request.cols > 1024 or request.rows > 1024) return error.InvalidDimensions;
    var t = try vt.Terminal.init(io, alloc, .{
        .cols = request.cols,
        .rows = request.rows,
        .max_scrollback_bytes = null,
        .max_scrollback_lines = null,
        .kitty_image_loading_limits = .direct,
    });
    defer t.deinit(alloc);
    const clipboard_replies = try alloc.alloc(DecodedClipboardReply, request.clipboard_replies.len);
    for (request.clipboard_replies, clipboard_replies) |source, *destination| {
        const contents = try alloc.alloc(vt.clipboard.Content, source.contents.len);
        for (source.contents, contents) |content, *decoded| {
            decoded.* = .{ .mime = try hexDecode(alloc, content.mime), .data = try hexDecode(alloc, content.data) };
        }
        const available = try alloc.alloc([]const u8, source.available.len);
        for (source.available, available) |mime, *decoded| decoded.* = try hexDecode(alloc, mime);
        destination.* = .{ .status = source.status, .contents = contents, .available = available, .remember = source.remember };
    }
    var ctx: Context = .{
        .alloc = alloc,
        .clipboard_replies = clipboard_replies,
        .clipboard_read_enabled = request.clipboard_read_enabled,
        .clipboard_write_enabled = request.clipboard_write_enabled,
        .clipboard_write_limit = request.clipboard_write_limit,
        .host = try DecodedHost.init(alloc, request.host),
    };
    current = &ctx;
    defer current = null;
    var stream = terminalStream(alloc, &t);
    defer stream.deinit();
    var observations: std.ArrayList(Observation) = .empty;
    var mode_results: std.ArrayList(bool) = .empty;
    var snapshots: std.ArrayList([]const u8) = .empty;
    var snapshot_source: std.Io.Reader = .fixed(&.{});
    var snapshot_decoder: ?vt.snapshot.Decoder = null;
    var snapshot_progress: std.ArrayList(SnapshotProgress) = .empty;
    for (request.operations) |op| {
        if (std.mem.eql(u8, op.op, "write")) {
            const bytes = try hexDecode(alloc, op.data);
            if (request.scalar) {
                for (bytes) |byte| stream.next(byte);
            } else stream.nextSlice(bytes);
        } else if (std.mem.eql(u8, op.op, "resize")) {
            if (op.cols == 0 or op.rows == 0 or op.cols > 1024 or op.rows > 1024) return error.InvalidDimensions;
            try stream.handler.resize(.{
                .cols = op.cols,
                .rows = op.rows,
                .cell_size_px = if (op.cell_size) |value| .{ .width = value[0], .height = value[1] } else null,
            });
        } else if (std.mem.eql(u8, op.op, "reset")) {
            stream.nextSlice("\x1bc");
        } else if (std.mem.eql(u8, op.op, "terminal_reset")) {
            t.fullReset();
        } else if (std.mem.eql(u8, op.op, "title_set")) {
            try t.setTitle(try hexDecode(alloc, op.data));
        } else if (std.mem.eql(u8, op.op, "pwd_set")) {
            try t.setPwd(try hexDecode(alloc, op.data));
        } else if (std.mem.eql(u8, op.op, "mode_set") or
            std.mem.eql(u8, op.op, "mode_default") or
            std.mem.eql(u8, op.op, "mode_raw_default") or
            std.mem.eql(u8, op.op, "mode_save") or
            std.mem.eql(u8, op.op, "mode_restore"))
        {
            const success = if (vt.modes.modeFromInt(op.number, !op.private)) |mode| apply: {
                if (std.mem.eql(u8, op.op, "mode_set")) {
                    t.modes.set(mode, op.value);
                } else if (std.mem.eql(u8, op.op, "mode_default")) {
                    // Match the original C embedder policy; raw ModeState's
                    // setDefault intentionally accepts every known mode.
                    if (!vt.modes.defaultConfigurable(mode)) break :apply false;
                    t.modes.setDefault(mode, op.value);
                } else if (std.mem.eql(u8, op.op, "mode_raw_default")) {
                    t.modes.setDefault(mode, op.value);
                } else if (std.mem.eql(u8, op.op, "mode_save")) {
                    t.modes.save(mode);
                } else {
                    _ = t.modes.restore(mode);
                }
                break :apply true;
            } else false;
            try mode_results.append(alloc, success);
        } else if (std.mem.eql(u8, op.op, "modes_reset")) {
            t.modes.reset();
        } else if (std.mem.eql(u8, op.op, "cursor_defaults")) {
            t.setDefaultCursorStyle(std.meta.stringToEnum(vt.Screen.CursorStyle, op.cursor_shape) orelse return error.InvalidCursorShape);
            t.setDefaultCursorBlink(op.cursor_blink);
        } else if (std.mem.eql(u8, op.op, "color_defaults")) {
            const colors = op.colors orelse return error.MissingColorDefaults;
            t.colors.foreground.default = if (colors.foreground) |value| rgb(value) else null;
            t.colors.background.default = if (colors.background) |value| rgb(value) else null;
            t.colors.cursor.default = if (colors.cursor) |value| rgb(value) else null;
            var palette = vt.color.default;
            if (colors.palette) |values| {
                if (values.len != palette.len) return error.InvalidPalette;
                for (&palette, values) |*entry, value| entry.* = rgb(value);
            }
            try t.colors.palette.changeDefault(alloc, palette);
        } else if (std.mem.eql(u8, op.op, "host_options")) {
            ctx.host = try DecodedHost.init(alloc, op.host orelse return error.MissingHostOptions);
            configureHost(&stream);
        } else if (std.mem.eql(u8, op.op, "clipboard_options")) {
            if (op.clipboard_read_enabled) |value| ctx.clipboard_read_enabled = value;
            if (op.clipboard_write_enabled) |value| ctx.clipboard_write_enabled = value;
            if (op.clipboard_write_limit) |value| ctx.clipboard_write_limit = value;
            configureClipboard(&stream);
        } else if (std.mem.eql(u8, op.op, "observe")) {
            try observations.append(alloc, try observe(alloc, &t, request));
        } else if (std.mem.eql(u8, op.op, "input")) {
            Context.append("input", try input_adapter.encode(alloc, &t, op.input orelse return error.MissingInput));
        } else if (std.mem.eql(u8, op.op, "paste")) {
            try paste_adapter.run(alloc, &stream.handler, op.paste orelse return error.MissingPaste, Context.append);
        } else if (std.mem.eql(u8, op.op, "checkpoint")) {
            observations.clearRetainingCapacity();
            ctx.events.clearRetainingCapacity();
            mode_results.clearRetainingCapacity();
        } else if (std.mem.eql(u8, op.op, "snapshot")) {
            var continuation: std.Io.Writer.Allocating = .init(alloc);
            try stream.writeContinuation(&continuation.writer);
            var encoded: std.Io.Writer.Allocating = .init(alloc);
            try vt.snapshot.encode(alloc, &encoded.writer, &t, .{
                .continuation = if (continuation.written().len == 0) .ground else .{ .bytes = continuation.written() },
            });
            try snapshots.append(alloc, try hexEncode(alloc, encoded.written()));
        } else if (std.mem.eql(u8, op.op, "restore")) {
            var source: std.Io.Reader = .fixed(try hexDecode(alloc, op.data));
            var decoded = vt.snapshot.decode(alloc, io, &source, .{ .max_continuation_bytes = 8 * 1024 * 1024 }) catch return error.InvalidSnapshot;
            defer decoded.deinit(alloc);
            restoreTerminal(alloc, &t, &stream, &decoded);
            snapshot_decoder = null;
        } else if (std.mem.eql(u8, op.op, "restore_ready")) {
            snapshot_source = .fixed(try hexDecode(alloc, op.data));
            snapshot_decoder = .init(&snapshot_source);
            var decoded = snapshot_decoder.?.ready(alloc, io, .{ .max_continuation_bytes = 8 * 1024 * 1024 }) catch return error.InvalidSnapshot;
            defer decoded.deinit(alloc);
            restoreTerminal(alloc, &t, &stream, &decoded);
            try snapshot_progress.append(alloc, .{
                .stage = "ready",
                .offset = snapshot_source.seek,
                .history_rows = .{ decoded.history_rows.get(.primary) orelse 0, decoded.history_rows.get(.alternate) orelse 0 },
            });
        } else if (std.mem.eql(u8, op.op, "restore_next")) {
            const decoder = if (snapshot_decoder) |*value| value else return error.MissingSnapshotDecoder;
            const progress = decoder.next(alloc, &t) catch return error.InvalidSnapshot;
            try snapshot_progress.append(alloc, if (progress) |value| .{
                .stage = "history",
                .offset = snapshot_source.seek,
                .screen = @intCast(@intFromEnum(value.key)),
                .rows = value.rows,
                .remaining = value.remaining,
            } else .{ .stage = "finish", .offset = snapshot_source.seek });
        } else return error.UnsupportedOperation;
    }
    if (ctx.invalid_read_status) return error.UnsupportedClipboardReadStatus;
    if (observe_terminal) try observations.append(alloc, try observe(alloc, &t, request));
    response.observations = observations.items;
    response.events = ctx.events.items;
    response.snapshots = snapshots.items;
    response.snapshot_progress = snapshot_progress.items;
    response.mode_results = mode_results.items;
    return response;
}

fn restoreTerminal(alloc: Allocator, terminal: *vt.Terminal, stream: *vt.TerminalStream, decoded: *vt.snapshot.Decoded) void {
    stream.deinit();
    terminal.deinit(alloc);
    terminal.* = decoded.toOwned();
    stream.* = terminalStream(alloc, terminal);
    switch (decoded.continuation) {
        .ground => {},
        .bytes => |bytes| stream.nextSlice(bytes),
    }
}

fn terminalStream(alloc: Allocator, terminal: *vt.Terminal) vt.TerminalStream {
    var result = vt.TerminalStream.init(.{
        .allocator = alloc,
        .handler = .init(terminal),
        .continuation_max_bytes = 8 * 1024 * 1024,
    });
    result.handler.effects.write_pty = Context.write;
    result.handler.effects.bell = Context.bell;
    result.handler.effects.title_changed = Context.title;
    result.handler.effects.pwd_changed = Context.pwd;
    result.handler.effects.desktop_notification = Context.notification;
    result.handler.effects.progress_report = Context.progress;
    configureClipboard(&result);
    configureHost(&result);
    return result;
}

fn configureClipboard(stream: *vt.TerminalStream) void {
    const ctx = current.?;
    stream.handler.effects.clipboard_write = if (ctx.clipboard_write_enabled) Context.clipboardWrite else null;
    stream.handler.effects.clipboard_read = if (ctx.clipboard_read_enabled) Context.clipboardRead else null;
    stream.handler.kitty_clipboard_write_max_bytes = ctx.clipboard_write_limit;
}

fn configureHost(stream: *vt.TerminalStream) void {
    const host = &current.?.host;
    stream.handler.effects.color_scheme = if (host.options.color_scheme != null) Context.colorScheme else null;
    stream.handler.effects.device_attributes = if (host.attributes != null) Context.deviceAttributes else null;
    stream.handler.effects.size = if (host.options.size != null) Context.size else null;
    stream.handler.effects.enquiry = if (host.enquiry != null) Context.enquiry else null;
    stream.handler.effects.xtversion = if (host.xtversion != null) Context.xtversion else null;
    stream.handler.title_report = host.options.title_report;
    stream.handler.terminfo_name = host.terminfo_name;
    stream.handler.terminal.flags.visible = host.options.visible;
}

fn observe(alloc: Allocator, t: *vt.Terminal, request: Request) !Observation {
    const modes = try alloc.alloc(Mode, request.observe_modes.len);
    for (request.observe_modes, modes) |tag, *result| {
        const mode = vt.modes.modeFromInt(tag.number, !tag.private);
        const saved: vt.modes.ModeState = .{ .values = t.modes.saved };
        const defaults: vt.modes.ModeState = .{ .values = t.modes.default };
        result.* = .{
            .number = tag.number,
            .private = tag.private,
            .current = if (mode) |value| t.modes.get(value) else null,
            .saved = if (mode) |value| saved.get(value) else null,
            .default = if (mode) |value| defaults.get(value) else null,
            .report = @intFromEnum(t.modes.getReport(.{ .value = @truncate(tag.number), .ansi = !tag.private }).state),
            .default_configurable = if (mode) |value| vt.modes.defaultConfigurable(value) else false,
        };
    }
    return .{
        .cols = t.cols,
        .rows = t.rows,
        .alternate_active = t.screens.active_key == .alternate,
        .primary = try observeScreen(alloc, t.screens.all.get(.primary).?),
        .alternate = if (t.screens.all.get(.alternate)) |screen| try observeScreen(alloc, screen) else null,
        .margins = .{ t.scrolling_region.top, t.scrolling_region.bottom, t.scrolling_region.left, t.scrolling_region.right },
        .title = try hexEncode(alloc, t.getTitle() orelse ""),
        .pwd = try hexEncode(alloc, t.getPwd() orelse ""),
        .modes = modes,
        .mode_effects = if (request.observe_mode_effects) .{
            .cursor_visible = t.modes.get(.cursor_visible),
            .cursor_blink = t.modes.get(.cursor_blinking),
            .mouse_mode = switch (t.flags.mouse_event) {
                .none => 0,
                .x10 => 9,
                .normal => 1000,
                .button => 1002,
                .any => 1003,
            },
            .mouse_format = switch (t.flags.mouse_format) {
                .x10 => 0,
                .utf8 => 1005,
                .sgr => 1006,
                .urxvt => 1015,
                .sgr_pixels => 1016,
            },
        } else null,
        .colors = if (request.observe_colors) observeColors(&t.colors) else null,
        .semantic = if (request.observe_semantic) try semantic_adapter.observe(alloc, t) else null,
    };
}

fn rgb(value: [3]u8) vt.color.RGB {
    return .{ .r = value[0], .g = value[1], .b = value[2] };
}

fn rgbBytes(value: ?vt.color.RGB) ?[3]u16 {
    const color = value orelse return null;
    return .{ color.r, color.g, color.b };
}

fn observeColors(colors: *const vt.Terminal.Colors) Colors {
    var result: Colors = undefined;
    inline for (.{ "foreground", "background", "cursor" }) |name| {
        const value = &@field(colors, name);
        @field(result, name) = .{
            .current = rgbBytes(value.get()),
            .default = rgbBytes(value.default),
            .override = rgbBytes(value.override),
        };
    }
    for (&result.palette, 0..) |*entry, i| {
        entry.* = .{
            .current = rgbBytes(colors.palette.current[i]),
            .default = rgbBytes(colors.palette.original[i]),
            .override = if (colors.palette.mask.isSet(i)) rgbBytes(colors.palette.current[i]) else null,
        };
    }
    return result;
}

fn observeScreen(alloc: Allocator, screen: *vt.Screen) !Screen {
    var rows: std.ArrayList(Row) = .empty;
    var iterator = screen.pages.rowIterator(.right_down, .{ .screen = .{} }, null);
    while (iterator.next()) |pin| {
        const cells = try alloc.alloc(Cell, pin.cells(.all).len);
        for (pin.cells(.all), cells) |*cell, *out| {
            var cps: std.ArrayList(u21) = .empty;
            if (cell.codepoint() != 0) try cps.append(alloc, cell.codepoint());
            if (pin.grapheme(cell)) |extra| try cps.appendSlice(alloc, extra);
            var style = pin.style(cell);
            switch (cell.content_tag) {
                .bg_color_palette => style.bg_color = .{ .palette = cell.content.color_palette.data },
                .bg_color_rgb => style.bg_color = .{ .rgb = .{ .r = cell.content.color_rgb.r, .g = cell.content.color_rgb.g, .b = cell.content.color_rgb.b } },
                else => {},
            }
            const page = pin.node.page();
            const link: ?Hyperlink = if (page.lookupHyperlink(cell)) |id| link: {
                const entry = page.hyperlink_set.get(page.memory, id);
                break :link .{
                    .uri = try hexEncode(alloc, entry.uri.slice(page.memory)),
                    .explicit = switch (entry.id) {
                        .explicit => |value| try hexEncode(alloc, value.slice(page.memory)),
                        .implicit => null,
                    },
                    .implicit = switch (entry.id) {
                        .explicit => null,
                        .implicit => |value| value,
                    },
                };
            } else null;
            out.* = .{
                .text = cps.items,
                .width = if (cell.wide == .spacer_tail) 0 else if (cell.wide == .wide) 2 else 1,
                .spacer_head = cell.wide == .spacer_head,
                .style = try observeStyle(alloc, style),
                .hyperlink = link,
                .protected = cell.protected,
                .semantic = @tagName(cell.semantic_content),
            };
        }
        try rows.append(alloc, .{ .cells = cells, .wrapped = pin.rowAndCell().row.wrap });
    }
    const history_count = rows.items.len - screen.pages.rows;
    const cursor = screen.cursor;
    return .{
        .cursor = .{
            .x = cursor.x,
            .y = cursor.y,
            .pending_wrap = cursor.pending_wrap,
            .shape = @tagName(cursor.cursor_style),
            .style = try observeStyle(alloc, cursor.style),
            .hyperlink = if (cursor.hyperlink) |link| .{
                .uri = try hexEncode(alloc, link.uri),
                .explicit = switch (link.id) {
                    .explicit => |value| try hexEncode(alloc, value),
                    .implicit => null,
                },
                .implicit = switch (link.id) {
                    .explicit => null,
                    .implicit => |value| value,
                },
            } else null,
            .protected = cursor.protected,
            .semantic = @tagName(cursor.semantic_content),
        },
        .history = rows.items[0..history_count],
        .rows = rows.items[history_count..],
    };
}

fn observeStyle(alloc: Allocator, s: vt.Style) !Style {
    return .{
        .foreground = try observeColor(alloc, s.fg_color),
        .background = try observeColor(alloc, s.bg_color),
        .underline_color = try observeColor(alloc, s.underline_color),
        .bold = s.flags.bold,
        .faint = s.flags.faint,
        .italic = s.flags.italic,
        .blink = s.flags.blink,
        .inverse = s.flags.inverse,
        .invisible = s.flags.invisible,
        .strikethrough = s.flags.strikethrough,
        .overline = s.flags.overline,
        .underline = @tagName(s.flags.underline),
    };
}
fn observeColor(alloc: Allocator, color: vt.Style.Color) !Color {
    return switch (color) {
        .none => .{},
        .palette => |v| .{ .kind = "indexed", .value = try alloc.dupe(u16, &.{v}) },
        .rgb => |v| .{ .kind = "rgb", .value = try alloc.dupe(u16, &.{ v.r, v.g, v.b }) },
    };
}
fn hexDecode(alloc: Allocator, input: []const u8) ![]const u8 {
    if (input.len % 2 != 0) return error.InvalidHex;
    const result = try alloc.alloc(u8, input.len / 2);
    return std.fmt.hexToBytes(result, input) catch return error.InvalidHex;
}
fn hexEncode(alloc: Allocator, input: []const u8) ![]const u8 {
    const result = try alloc.alloc(u8, input.len * 2);
    const digits = "0123456789abcdef";
    for (input, 0..) |byte, i| {
        result[i * 2] = digits[byte >> 4];
        result[i * 2 + 1] = digits[byte & 15];
    }
    return result;
}
