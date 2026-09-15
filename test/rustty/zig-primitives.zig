//! Portable unit-level benchmarks. Only libghostty-vt and std are imported;
//! there is no renderer, application runtime, PTY, or platform instrumentation.
//! Input generation is outside the timer; feed/stream include UTF-8/VT parsing.
const std = @import("std");
const vt = @import("ghostty-vt");

pub const std_options: std.Options = .{ .log_level = .err };

const Operation = enum { width, print, scalar, read, clone, reflow, feed, stream, stream_styled };
const cols = 128;
const rows = 32;
const history_lines = 1024;
const prime_batches = 32;

fn isStream(op: Operation) bool {
    return op == .stream or op == .stream_styled;
}

fn parsed(op: Operation) bool {
    return op == .feed or isStream(op);
}

fn streamChecksum(screen: *const vt.Screen) u64 {
    var sum: u64 = 0;
    var it = screen.pages.rowIterator(.right_down, .{ .active = .{} }, null);
    while (it.next()) |pin| {
        for (pin.cells(.all)) |*cell| {
            sum *%= 16_777_619;
            sum +%= cell.codepoint();
            if (cell.hasGrapheme()) {
                for (pin.grapheme(cell).?) |cp| sum +%= cp;
            }
            if (cell.codepoint() != 0) {
                const style = pin.style(cell);
                const color: u64 = switch (style.fg_color) {
                    .none => 0,
                    .palette => |index| @as(u64, index) + 1,
                    .rgb => unreachable,
                };
                sum +%= (color + 257 * @as(u64, @intFromBool(style.flags.bold))) * 0x11_0000;
            }
        }
    }
    return sum;
}

fn checkStream(terminal: *const vt.Terminal) !u64 {
    const screen = terminal.screens.active;
    const history = screen.pages.total_rows - screen.pages.rows;
    if (history == 0 or history > history_lines) return error.InvalidHistory;
    if (screen.cursor.x != 0 or screen.cursor.y != rows - 1 or
        screen.cursor.pending_wrap) return error.InvalidCursor;
    return streamChecksum(screen);
}

fn decode(alloc: std.mem.Allocator, bytes: []const u8) ![]u21 {
    const view = try std.unicode.Utf8View.init(bytes);
    var result: std.ArrayList(u21) = .empty;
    errdefer result.deinit(alloc);
    var it = view.iterator();
    while (it.nextCodepoint()) |cp| {
        // These workloads exercise text primitives, not terminal controls.
        if (cp < 0x20 or (cp >= 0x7f and cp < 0xa0)) return error.InvalidCorpus;
        try result.append(alloc, cp);
    }
    if (result.items.len == 0) return error.InvalidCorpus;
    return result.toOwnedSlice(alloc);
}

fn fill(terminal: *vt.Terminal, cps: []const u21) !void {
    terminal.setCursorPos(1, 1);
    for (cps) |cp| try terminal.print(cp);
}

fn read(screen: *const vt.Screen, comptime full_text: bool) u64 {
    var sum: u64 = 0;
    var it = screen.pages.rowIterator(.right_down, .{ .active = .{} }, null);
    while (it.next()) |pin| {
        for (pin.cells(.all)) |*cell| {
            sum +%= cell.codepoint();
            if (full_text and cell.hasGrapheme()) {
                for (pin.grapheme(cell).?) |cp| sum +%= cp;
            }
        }
    }
    return sum;
}

fn step(
    comptime op: Operation,
    terminal: *vt.Terminal,
    stream: *vt.TerminalStream,
    cps: []const u21,
    bytes: []const u8,
) !u64 {
    std.mem.doNotOptimizeAway(terminal);
    std.mem.doNotOptimizeAway(cps);
    std.mem.doNotOptimizeAway(bytes);
    switch (op) {
        .width => {
            var sum: u64 = 0;
            for (cps) |cp| sum +%= vt.unicode.codepointWidth(cp);
            return sum;
        },
        .print => try fill(terminal, cps),
        .feed, .stream, .stream_styled => stream.nextSlice(bytes),
        .scalar => return read(terminal.screens.active, false),
        .read => return read(terminal.screens.active, true),
        .clone => {
            var copy = try terminal.screens.active.clone(
                terminal.io(),
                terminal.gpa(),
                .{ .viewport = .{} },
                null,
            );
            std.mem.doNotOptimizeAway(&copy);
            copy.deinit();
        },
        .reflow => {
            try terminal.resize(terminal.gpa(), .{ .cols = cols / 2, .rows = rows });
            try terminal.resize(terminal.gpa(), .{ .cols = cols, .rows = rows });
        },
    }
    std.mem.doNotOptimizeAway(terminal);
    return 0;
}

pub fn main(init: std.process.Init) !void {
    const alloc = init.gpa;
    const args = try init.minimal.args.toSlice(init.arena.allocator());
    if (args.len != 4) return error.ExpectedOperationDatafileIterations;
    const op = std.meta.stringToEnum(Operation, args[1]) orelse return error.InvalidOperation;
    const iterations = try std.fmt.parseInt(u64, args[3], 10);
    if (iterations == 0) return error.InvalidIterations;
    const bytes = try std.Io.Dir.cwd().readFileAlloc(init.io, args[2], alloc, .limited(16 * 1024 * 1024));
    defer alloc.free(bytes);
    const cps = if (parsed(op)) try alloc.alloc(u21, 0) else try decode(alloc, bytes);
    defer alloc.free(cps);
    var terminal = try vt.Terminal.init(init.io, alloc, .{
        .cols = cols,
        .rows = rows,
        .max_scrollback_bytes = if (isStream(op)) null else 0,
        .max_scrollback_lines = if (isStream(op)) history_lines else null,
        .default_modes = .{ .grapheme_cluster = true },
    });
    defer terminal.deinit(alloc);
    var stream = vt.TerminalStream.init(.{ .allocator = alloc, .handler = .init(&terminal) });
    defer stream.deinit();
    if (isStream(op)) {
        for (0..prime_batches) |_| stream.nextSlice(bytes);
    } else if (op == .feed) {
        stream.nextSlice(bytes);
    } else {
        try fill(&terminal, cps);
    }
    const expected = switch (op) {
        .width => try step(.width, &terminal, &stream, cps, bytes),
        .stream, .stream_styled => try checkStream(&terminal),
        .scalar => read(terminal.screens.active, false),
        else => read(terminal.screens.active, true),
    };

    var checksum: u64 = 0;
    const elapsed = switch (op) {
        inline else => |operation| measured: {
            const start = std.Io.Timestamp.now(init.io, .awake);
            for (0..iterations) |_| {
                const value = try step(operation, &terminal, &stream, cps, bytes);
                std.mem.doNotOptimizeAway(value);
                checksum = value;
            }
            break :measured start.durationTo(.now(init.io, .awake)).nanoseconds;
        },
    };
    switch (op) {
        .print, .clone, .reflow, .feed => checksum = read(terminal.screens.active, true),
        .stream, .stream_styled => checksum = try checkStream(&terminal),
        else => {},
    }
    if (checksum != expected) return error.ChecksumMismatch;

    var buffer: [4096]u8 = undefined;
    var stdout = std.Io.File.stdout().writerStreaming(init.io, &buffer);
    try std.json.Stringify.value(.{
        .engine = "ghostty",
        .operation = @tagName(op),
        .iterations = iterations,
        .elapsed_ns = elapsed,
        .units_per_iteration = switch (op) {
            .width, .print => cps.len,
            .scalar, .read, .clone => cols * rows,
            .reflow => 1,
            .feed, .stream, .stream_styled => bytes.len,
        },
        .cell_bytes = @sizeOf(vt.Cell),
        .checksum = checksum,
    }, .{}, &stdout.interface);
    try stdout.interface.writeByte('\n');
    try stdout.interface.flush();
}

test "primitive workloads retain scalar and grapheme contents" {
    const alloc = std.testing.allocator;
    const cps = try decode(alloc, "a\u{0301}天地👩\u{200d}💻");
    defer alloc.free(cps);
    var terminal = try vt.Terminal.init(std.testing.io, alloc, .{
        .cols = cols,
        .rows = rows,
        .max_scrollback_bytes = 0,
        .default_modes = .{ .grapheme_cluster = true },
    });
    defer terminal.deinit(alloc);
    var stream = vt.TerminalStream.init(.{ .allocator = alloc, .handler = .init(&terminal) });
    defer stream.deinit();
    try fill(&terminal, cps);
    var expected: u64 = 0;
    for (cps) |cp| expected += cp;
    try std.testing.expectEqual(expected, read(terminal.screens.active, true));
    const pin = terminal.screens.active.pages.getTopLeft(.active);
    const cells = pin.cells(.all);
    try std.testing.expectEqualSlices(u21, &.{0x0301}, pin.grapheme(&cells[0]).?);
    try std.testing.expectEqualSlices(u21, &.{ 0x200d, 0x1f4bb }, pin.grapheme(&cells[5]).?);
    try std.testing.expectEqual('a' + 0x5929 + 0x5730 + 0x1f469, try step(.scalar, &terminal, &stream, cps, ""));
    var copy = try terminal.screens.active.clone(std.testing.io, alloc, .{ .viewport = .{} }, null);
    defer copy.deinit();
    try std.testing.expectEqual(expected, read(&copy, true));
    inline for ([_]Operation{ .print, .clone, .reflow }) |op| {
        _ = try step(op, &terminal, &stream, cps, "");
        try std.testing.expectEqual(expected, read(terminal.screens.active, true));
    }
    try std.testing.expectEqual(expected, try step(.read, &terminal, &stream, cps, ""));
    try std.testing.expectEqual(9, try step(.width, &terminal, &stream, cps, ""));
    try std.testing.expectError(error.InvalidCorpus, decode(alloc, "\n"));
    try std.testing.expectError(error.InvalidCorpus, decode(alloc, ""));
    try std.testing.expectError(error.InvalidUtf8, decode(alloc, "\xff"));
}
