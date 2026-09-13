//! Test-only NDJSON process adapter around the original Zig terminal.
//! No code from this executable is linked into the Rust application.
const std = @import("std");
const vt = @import("ghostty-vt");
const input_adapter = @import("zig-input.zig");
const parser_adapter = @import("zig-parser.zig");
const Allocator = std.mem.Allocator;

pub const std_options: std.Options = .{ .log_level = .err };

const capabilities = [_][]const u8{
    "terminal.write",    "terminal.resize", "terminal.reset",   "terminal.observe",
    "terminal.cells",    "terminal.styles", "terminal.screens", "terminal.cursor",
    "effects.pty",       "effects.title",   "effects.pwd",      "effects.bell",
    "unicode.width",     "input.key",       "input.mouse",      "input.focus-paste",
    "parser.raw-events",
};

const Operation = struct {
    op: []const u8,
    data: []const u8 = "",
    cols: u16 = 0,
    rows: u16 = 0,
    input: ?input_adapter.Event = null,
};
const Request = struct {
    id: []const u8 = "case",
    kind: []const u8 = "terminal",
    cols: u16 = 12,
    rows: u16 = 4,
    scalar: bool = false,
    operations: []const Operation = &.{},
    codepoints: []const u32 = &.{},
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
const Cell = struct {
    text: []const u21,
    width: u8,
    spacer_head: bool,
    style: Style,
    hyperlink: ?[]const u8,
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
};
const Event = struct { kind: []const u8, data: []const u8 = "" };
const Response = struct {
    id: []const u8,
    ok: bool = true,
    err: ?[]const u8 = null,
    capabilities: []const []const u8 = &capabilities,
    observations: []Observation = &.{},
    events: []Event = &.{},
    widths: []const i8 = &.{},
    parser: ?parser_adapter.Result = null,
};

// Effects arrive synchronously; one terminal is exercised at a time. This
// test-only pointer avoids changing the production callback API for the oracle.
var current: ?*Context = null;
const Context = struct {
    alloc: Allocator,
    events: std.ArrayList(Event) = .empty,

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
    var stream = t.vtStream();
    defer stream.deinit();
    var ctx: Context = .{ .alloc = alloc };
    current = &ctx;
    defer current = null;
    stream.handler.effects.write_pty = Context.write;
    stream.handler.effects.bell = Context.bell;
    stream.handler.effects.title_changed = Context.title;
    stream.handler.effects.pwd_changed = Context.pwd;
    var observations: std.ArrayList(Observation) = .empty;
    for (request.operations) |op| {
        if (std.mem.eql(u8, op.op, "write")) {
            const bytes = try hexDecode(alloc, op.data);
            if (request.scalar) {
                for (bytes) |byte| stream.next(byte);
            } else stream.nextSlice(bytes);
        } else if (std.mem.eql(u8, op.op, "resize")) {
            if (op.cols == 0 or op.rows == 0 or op.cols > 1024 or op.rows > 1024) return error.InvalidDimensions;
            try stream.handler.resize(.{ .cols = op.cols, .rows = op.rows });
        } else if (std.mem.eql(u8, op.op, "reset")) {
            stream.nextSlice("\x1bc");
        } else if (std.mem.eql(u8, op.op, "observe")) {
            try observations.append(alloc, try observe(alloc, &t));
        } else if (std.mem.eql(u8, op.op, "input")) {
            Context.append("input", try input_adapter.encode(alloc, &t, op.input orelse return error.MissingInput));
        } else return error.UnsupportedOperation;
    }
    if (observe_terminal) try observations.append(alloc, try observe(alloc, &t));
    response.observations = observations.items;
    response.events = ctx.events.items;
    return response;
}

fn observe(alloc: Allocator, t: *vt.Terminal) !Observation {
    return .{
        .cols = t.cols,
        .rows = t.rows,
        .alternate_active = t.screens.active_key == .alternate,
        .primary = try observeScreen(alloc, t.screens.all.get(.primary).?),
        .alternate = if (t.screens.all.get(.alternate)) |screen| try observeScreen(alloc, screen) else null,
        .margins = .{ t.scrolling_region.top, t.scrolling_region.bottom, t.scrolling_region.left, t.scrolling_region.right },
        .title = try alloc.dupe(u8, t.getTitle() orelse ""),
        .pwd = try alloc.dupe(u8, t.getPwd() orelse ""),
    };
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
            const link: ?[]const u8 = if (page.lookupHyperlink(cell)) |id|
                try alloc.dupe(u8, page.hyperlink_set.get(page.memory, id).uri.slice(page.memory))
            else
                null;
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
