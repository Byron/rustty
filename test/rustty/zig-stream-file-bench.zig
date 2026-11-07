//! Whole-file terminal feeds shared with the Rust primitive benchmark.
//! File I/O, resets, parser setup, and state checks are outside the timer.
const std = @import("std");
const vt = @import("ghostty-vt");

const Input = struct {
    name: []const u8 = "file",
    data: []const u8,
    cols: u16 = 120,
    rows: u16 = 80,
    scrollback_bytes: usize = 10_000,
    grapheme_width: enum { legacy, unicode } = .legacy,
};

const Summary = struct {
    checksum: u64,
    history_rows: usize,
    page_bytes: usize,
    page_count: usize,
};

fn mix(sum: *u64, value: u64) void {
    sum.* *%= 16_777_619;
    sum.* +%= value;
}

fn mixBytes(sum: *u64, bytes: []const u8) void {
    mix(sum, bytes.len);
    for (bytes) |byte| mix(sum, byte);
}

fn colorBits(color: vt.Style.Color) u32 {
    return switch (color.value()) {
        .none => 0,
        .palette => |value| 1 | (@as(u32, value) << 8),
        .rgb => |value| 2 | (@as(u32, value.r) << 8) | (@as(u32, value.g) << 16) | (@as(u32, value.b) << 24),
    };
}

fn mixStyle(sum: *u64, style: vt.Style) void {
    mix(sum, colorBits(style.fg_color));
    mix(sum, colorBits(style.bg_color));
    mix(sum, colorBits(style.underline_color));
    const flags: u16 = @as(u16, @intFromBool(style.flags.bold)) |
        (@as(u16, @intFromBool(style.flags.italic)) << 1) |
        (@as(u16, @intFromBool(style.flags.faint)) << 2) |
        (@as(u16, @intFromBool(style.flags.blink)) << 3) |
        (@as(u16, @intFromBool(style.flags.inverse)) << 4) |
        (@as(u16, @intFromBool(style.flags.invisible)) << 5) |
        (@as(u16, @intFromBool(style.flags.strikethrough)) << 6) |
        (@as(u16, @intFromBool(style.flags.overline)) << 7) |
        (@as(u16, @backingInt(style.flags.underline)) << 8);
    mix(sum, flags);
}

// Hash logical visible state, excluding resource IDs and allocation details.
// In particular, implicit link counters may differ while their URI is equal.
fn summarize(terminal: *const vt.Terminal) !Summary {
    const screen = terminal.screens.active;
    const cursor = screen.cursor;
    var checksum: u64 = 0;
    mix(&checksum, cursor.x);
    mix(&checksum, cursor.y);
    mix(&checksum, @intFromBool(cursor.pending_wrap));
    mix(&checksum, @intFromBool(terminal.modes.get(.grapheme_cluster)));
    mixStyle(&checksum, cursor.style);
    var it = screen.pages.rowIterator(.right_down, .{ .active = .{} }, null);
    while (it.next()) |pin| {
        const row = pin.rowAndCell().row;
        mix(&checksum, @intFromBool(row.wrap));
        mix(&checksum, @intFromBool(row.wrap_continuation));
        for (pin.cells(.all)) |*cell| {
            const cp = cell.codepoint();
            if (cp == 0) {
                mix(&checksum, 0);
            } else {
                const extra = pin.grapheme(cell) orelse &.{};
                mix(&checksum, 1 + extra.len);
                mix(&checksum, cp);
                for (extra) |scalar| mix(&checksum, scalar);
            }
            mix(&checksum, switch (cell.wide) {
                .narrow, .spacer_head => 1,
                .wide => 2,
                .spacer_tail => 0,
            });
            mix(&checksum, @intFromBool(cell.wide == .spacer_head));
            mix(&checksum, @intFromBool(cell.protected));
            mix(&checksum, @backingInt(cell.semantic_content));
            var style = pin.style(cell);
            switch (cell.content_tag) {
                .codepoint, .codepoint_grapheme => {},
                .bg_color_palette => style.bg_color = .palette(cell.content.color_palette.data),
                .bg_color_rgb => {
                    const rgb = cell.content.color_rgb;
                    style.bg_color = .rgb(.{ .r = rgb.r, .g = rgb.g, .b = rgb.b });
                },
            }
            mixStyle(&checksum, style);
            mix(&checksum, @intFromBool(cell.hyperlink));
            if (cell.hyperlink) {
                const page = pin.node.page();
                const id = page.lookupHyperlink(cell) orelse return error.InvalidHyperlink;
                const link = page.hyperlink_set.get(page.memory, id);
                mixBytes(&checksum, link.uri.slice(page.memory));
                switch (link.id) {
                    .implicit => mix(&checksum, 1),
                    .explicit => |value| {
                        mix(&checksum, 2);
                        mixBytes(&checksum, value.slice(page.memory));
                    },
                }
            }
        }
    }
    var page_count: usize = 0;
    var node = screen.pages.pages.first;
    while (node) |current| : (node = current.next) page_count += 1;
    return .{
        .checksum = checksum,
        .history_rows = screen.pages.total_rows - screen.pages.rows,
        .page_bytes = screen.pages.page_size,
        .page_count = page_count,
    };
}

fn reset(alloc: std.mem.Allocator, terminal: *vt.Terminal, stream: *vt.TerminalStream) void {
    stream.deinit();
    terminal.fullReset();
    stream.* = vt.TerminalStream.init(.{ .allocator = alloc, .handler = .init(terminal) });
}

fn feed(stream: *vt.TerminalStream, bytes: []const u8) void {
    var offset: usize = 0;
    while (offset < bytes.len) {
        const end = offset + @min(64 * 1024, bytes.len - offset);
        stream.nextSlice(bytes[offset..end]);
        offset = end;
    }
}

pub fn run(init: std.process.Init, config_bytes: []const u8, iterations: u64) !void {
    const parsed = try std.json.parseFromSlice(Input, init.gpa, config_bytes, .{});
    defer parsed.deinit();
    const input = parsed.value;
    if (input.cols == 0 or input.rows == 0 or iterations == 0) return error.InvalidConfig;
    const bytes = try std.Io.Dir.cwd().readFileAlloc(init.io, input.data, init.gpa, .limited(128 * 1024 * 1024));
    defer init.gpa.free(bytes);
    if (bytes.len == 0) return error.EmptyInput;
    var terminal = try vt.Terminal.init(init.io, init.gpa, .{
        .cols = input.cols,
        .rows = input.rows,
        .max_scrollback_bytes = input.scrollback_bytes,
        .default_modes = .{ .grapheme_cluster = input.grapheme_width == .unicode },
    });
    defer terminal.deinit(init.gpa);
    var stream = vt.TerminalStream.init(.{ .allocator = init.gpa, .handler = .init(&terminal) });
    defer stream.deinit();
    reset(init.gpa, &terminal, &stream);
    feed(&stream, bytes);
    const expected = try summarize(&terminal);
    var summary = expected;
    var elapsed: i128 = 0;
    for (0..iterations) |_| {
        reset(init.gpa, &terminal, &stream);
        const start = std.Io.Timestamp.now(init.io, .awake);
        feed(&stream, bytes);
        elapsed += start.durationTo(.now(init.io, .awake)).nanoseconds;
        std.mem.doNotOptimizeAway(&terminal);
        summary = try summarize(&terminal);
        if (summary.checksum != expected.checksum) return error.ChecksumMismatch;
    }

    var buffer: [4096]u8 = undefined;
    var stdout = std.Io.File.stdout().writerStreaming(init.io, &buffer);
    try std.json.Stringify.value(.{
        .engine = "ghostty",
        .operation = "stream_file",
        .name = input.name,
        .iterations = iterations,
        .elapsed_ns = elapsed,
        .units_per_iteration = bytes.len,
        .cell_bytes = @sizeOf(vt.Cell),
        .checksum = summary.checksum,
        .history_rows = summary.history_rows,
        .page_bytes = summary.page_bytes,
        .page_count = summary.page_count,
        .owned_bytes = @as(?usize, null),
    }, .{}, &stdout.interface);
    try stdout.interface.writeByte('\n');
    try stdout.interface.flush();
}

test "stream file reset retains width defaults and visible styled linked content" {
    const alloc = std.testing.allocator;
    var terminal = try vt.Terminal.init(std.testing.io, alloc, .{
        .cols = 8,
        .rows = 3,
        .max_scrollback_bytes = 0,
        .default_modes = .{ .grapheme_cluster = true },
    });
    defer terminal.deinit(alloc);
    var stream = vt.TerminalStream.init(.{ .allocator = alloc, .handler = .init(&terminal) });
    defer stream.deinit();
    const input = "\x1b[31;1mA\x1b[0m界e\u{0301}" ++
        "\x1b]8;id=chosen;https://x\x1b\\B\x1b]8;;\x1b\\" ++
        "\x1b[44m\x1b[2X\x1b[0m\r\n" ++
        "\x1b]8;;https://i\x1b\\C\x1b]8;;\x1b\\" ++
        "\x1b[38;2;3;5;7;58:2::11:13:17;1;2;3;5;7;8;9;53;4:3m";
    for (0..2) |_| {
        // A partial parser sequence and non-default mode must not survive reset.
        stream.nextSlice("stale\x1b[?2027l\x1b[");
        reset(alloc, &terminal, &stream);
        feed(&stream, input);
        const actual = try summarize(&terminal);
        try std.testing.expectEqual(@as(u64, 8376364026207819422), actual.checksum);
        try std.testing.expectEqual(0, actual.history_rows);
        try std.testing.expectEqual(1, actual.page_count);
    }
}
