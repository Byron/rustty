//! Input protocol adapter. Coordinates use fixed 8x16-pixel cells.
const std = @import("std");
const vt = @import("ghostty-vt");

pub const Event = struct {
    kind: []const u8,
    key: []const u8 = "unidentified",
    data: []const u8 = "",
    action: []const u8 = "press",
    modifiers: u16 = 0,
    consumed_modifiers: u16 = 0,
    unshifted: u21 = 0,
    composing: bool = false,
    macos_option_as_alt: []const u8 = "true",
    conservative: bool = false,
    focused: bool = true,
    button: ?[]const u8 = null,
    x: f32 = 0,
    y: f32 = 0,
};

pub fn encode(alloc: std.mem.Allocator, terminal: *vt.Terminal, event: Event) ![]const u8 {
    var writer: std.Io.Writer.Allocating = .init(alloc);
    const mods: vt.input.KeyMods = @bitCast(event.modifiers);
    if (std.mem.eql(u8, event.kind, "key")) {
        const bytes = try unhex(alloc, event.data);
        if (!std.unicode.utf8ValidateSlice(bytes)) return error.InvalidText;
        var options: vt.input.KeyEncodeOptions = .fromTerminal(terminal);
        options.macos_option_as_alt = std.meta.stringToEnum(@TypeOf(options.macos_option_as_alt), event.macos_option_as_alt) orelse return error.InvalidOptionAsAlt;
        try vt.input.encodeKey(&writer.writer, .{
            .key = std.meta.stringToEnum(vt.input.Key, event.key) orelse return error.InvalidKey,
            .action = std.meta.stringToEnum(vt.input.KeyAction, event.action) orelse return error.InvalidAction,
            .mods = mods,
            .consumed_mods = @bitCast(event.consumed_modifiers),
            .utf8 = bytes,
            .unshifted_codepoint = event.unshifted,
            .composing = event.composing,
        }, options);
    } else if (std.mem.eql(u8, event.kind, "mouse")) {
        var options: vt.input.MouseEncodeOptions = .fromTerminal(terminal, .{
            .screen = .{ .width = @as(u32, terminal.cols) * 8, .height = @as(u32, terminal.rows) * 16 },
            .cell = .{ .width = 8, .height = 16 },
            .padding = .{},
        });
        options.any_button_pressed = event.button != null and !std.mem.eql(u8, event.action, "release");
        try vt.input.encodeMouse(&writer.writer, .{
            .action = std.meta.stringToEnum(vt.input.MouseAction, event.action) orelse return error.InvalidAction,
            .button = if (event.button) |name| std.meta.stringToEnum(vt.input.MouseButton, name) orelse return error.InvalidButton else null,
            .mods = mods,
            .pos = .{ .x = event.x, .y = event.y },
        }, options);
    } else if (std.mem.eql(u8, event.kind, "focus")) {
        if (terminal.modes.get(.focus_event)) try vt.input.encodeFocus(&writer.writer, if (event.focused) .gained else .lost);
    } else if (std.mem.eql(u8, event.kind, "paste")) {
        try vt.input.encodePasteWriter(&writer.writer, try unhex(alloc, event.data), .fromTerminal(terminal));
    } else if (std.mem.eql(u8, event.kind, "paste_safe")) {
        const bytes = try unhex(alloc, event.data);
        const safe = if (event.conservative) vt.input.isSafePaste(bytes) else vt.input.isSafePasteWith(bytes, .fromTerminal(terminal));
        try writer.writer.writeByte(@intFromBool(safe));
    } else return error.UnsupportedInput;
    return writer.written();
}

fn unhex(alloc: std.mem.Allocator, input: []const u8) ![]const u8 {
    if (input.len % 2 != 0) return error.InvalidHex;
    const result = try alloc.alloc(u8, input.len / 2);
    return std.fmt.hexToBytes(result, input) catch return error.InvalidHex;
}
