//! Portable unit-level benchmarks. Only libghostty-vt and std are imported;
//! there is no renderer, application runtime, PTY, or platform instrumentation.
//! Input generation is outside the timer; feed/stream include UTF-8/VT parsing.
const std = @import("std");
const vt = @import("ghostty-vt");

pub const std_options: std.Options = .{ .log_level = .err };

const Operation = enum { width, print, scalar, read, clone, reflow, feed, stream, stream_styled, reflow_history_styled, reflow_scrollback, reflow_history_content, kitty_place };
const cols = 128;
const rows = 32;
const history_lines = 1024;
const prime_batches = 32;

fn isStream(op: Operation) bool {
    return op == .stream or op == .stream_styled;
}

fn parsed(op: Operation) bool {
    return op == .feed or isStream(op) or op == .reflow_history_styled or op == .reflow_scrollback;
}

fn historyChecksum(screen: *const vt.Screen) u64 {
    var sum: u64 = 0;
    var it = screen.pages.rowIterator(.right_down, .{ .screen = .{} }, null);
    while (it.next()) |pin| {
        for (pin.cells(.all)) |*cell| {
            if (cell.codepoint() == 0) continue;
            sum *%= 16_777_619;
            sum +%= cell.codepoint();
            if (cell.hasGrapheme()) {
                for (pin.grapheme(cell).?) |cp| sum +%= cp;
            }
            const style = pin.style(cell);
            const color: u64 = switch (style.fg_color) {
                .none => 0,
                .palette => |value| @as(u64, value) + 1,
                .rgb => |value| 257 + (@as(u64, value.r) << 16) + (@as(u64, value.g) << 8) + value.b,
            };
            sum +%= (color + 257 * @as(u64, @intFromBool(style.flags.bold))) * 0x11_0000;
        }
    }
    return sum;
}

// Check all retained rows, not just the active screen. The supplied records
// contain 192 ASCII columns with alternating bold and four palette colors.
fn checkStyledHistory(terminal: *const vt.Terminal, records: usize, columns: usize) !u64 {
    const screen = terminal.screens.active;
    const record_rows = std.math.divCeil(usize, 192, columns) catch unreachable;
    if (screen.pages.total_rows != records * record_rows + 1) return error.InvalidHistory;
    var sum: u64 = 0;
    var index: usize = 0;
    var it = screen.pages.rowIterator(.right_down, .{ .screen = .{} }, null);
    while (it.next()) |pin| : (index += 1) {
        const record = index / record_rows;
        const used = if (record < records) @min(192 - index % record_rows * columns, columns) else 0;
        for (pin.cells(.all), 0..) |*cell, col| {
            if (col >= used) {
                if (cell.codepoint() != 0) return error.InvalidText;
                continue;
            }
            if (cell.codepoint() != "abcdefgh"[col % 8]) return error.InvalidText;
            const style = pin.style(cell);
            const color: u64 = switch (style.fg_color) {
                .palette => |value| value,
                else => return error.InvalidStyle,
            };
            if (color != 1 + record % 4 or style.flags.bold != (record % 2 == 0)) return error.InvalidStyle;
            sum +%= cell.codepoint() + (color + 1 + 257 * @as(u64, @intFromBool(style.flags.bold))) * 0x11_0000;
        }
    }
    if (index != screen.pages.total_rows) return error.InvalidHistory;
    return sum;
}

const ContentUnit = struct { text: []const u8, width: u2 = 1 };
const ContentPattern = enum {
    ascii,
    chinese,
    combining,
    emoji,
    mixed,

    fn units(self: ContentPattern) []const ContentUnit {
        return switch (self) {
            .ascii => &.{ .{ .text = "a" }, .{ .text = "b" }, .{ .text = "c" }, .{ .text = "d" }, .{ .text = "e" }, .{ .text = "f" }, .{ .text = "g" }, .{ .text = "h" } },
            .chinese => &.{ .{ .text = "天", .width = 2 }, .{ .text = "地", .width = 2 }, .{ .text = "玄", .width = 2 }, .{ .text = "黄", .width = 2 }, .{ .text = "宇", .width = 2 }, .{ .text = "宙", .width = 2 }, .{ .text = "洪", .width = 2 }, .{ .text = "荒", .width = 2 } },
            .combining => &.{ .{ .text = "a\u{0301}" }, .{ .text = "b\u{0302}" }, .{ .text = "c\u{0303}" }, .{ .text = "d\u{0308}" } },
            .emoji => &.{ .{ .text = "👩\u{200d}💻", .width = 2 }, .{ .text = "👨\u{200d}🚀", .width = 2 } },
            .mixed => &.{ .{ .text = "a" }, .{ .text = "界", .width = 2 }, .{ .text = "e\u{0301}" }, .{ .text = "👩\u{200d}💻", .width = 2 } },
        };
    }

    fn repeats(self: ContentPattern) usize {
        return switch (self) {
            .ascii => 24,
            .chinese => 12,
            .combining, .emoji => 48,
            .mixed => 32,
        };
    }
};

const ContentConfig = struct {
    pattern: ContentPattern,
    records: usize,
    styled: bool,
    link_repeats: usize = 0,
    tracked: bool = false,
    narrow: u16 = cols / 2,
};
const ContentCell = struct {
    kind: enum { head, tail, spacer, padding } = .padding,
    unit: usize = 0,
    repeat: usize = 0,
};
const ContentPins = [3]?*vt.Pin;

// Model one hard line independently of either terminal's reflow algorithm.
fn contentLayout(alloc: std.mem.Allocator, pattern: ContentPattern, columns: usize) ![]ContentCell {
    var cells: std.ArrayList(ContentCell) = .empty;
    errdefer cells.deinit(alloc);
    for (0..pattern.repeats()) |repeat| {
        for (pattern.units(), 0..) |unit, i| {
            if (cells.items.len % columns + unit.width > columns) {
                try cells.append(alloc, .{ .kind = .spacer });
            }
            try cells.append(alloc, .{ .kind = .head, .unit = i, .repeat = repeat });
            if (unit.width == 2) try cells.append(alloc, .{ .kind = .tail, .unit = i, .repeat = repeat });
        }
    }
    while (cells.items.len % columns != 0) try cells.append(alloc, .{});
    return cells.toOwnedSlice(alloc);
}

fn feedContent(alloc: std.mem.Allocator, stream: *vt.TerminalStream, config: ContentConfig) !void {
    var record: std.ArrayList(u8) = .empty;
    defer record.deinit(alloc);
    for (0..config.pattern.repeats()) |_| {
        for (config.pattern.units()) |unit| try record.appendSlice(alloc, unit.text);
    }
    const linked_end = record.items.len / config.pattern.repeats() * config.link_repeats;
    var buffer: [128]u8 = undefined;
    for (0..config.records) |i| {
        if (config.styled) stream.nextSlice(try std.fmt.bufPrint(&buffer, "\x1b[{};{}m", .{ @as(u8, if (i % 2 == 0) 1 else 22), 31 + i % 4 }));
        if (config.link_repeats != 0) {
            stream.nextSlice(try std.fmt.bufPrint(&buffer, "\x1b]8;id={};https://example.test/reflow/{}\x1b\\", .{ i, i }));
            stream.nextSlice(record.items[0..linked_end]);
            stream.nextSlice("\x1b]8;;\x1b\\");
        }
        stream.nextSlice(record.items[linked_end..]);
        if (config.styled) stream.nextSlice("\x1b[0m");
        stream.nextSlice("\r\n");
    }
}

fn trackContent(screen: *vt.Screen, config: ContentConfig, record_rows: usize) !ContentPins {
    var pins: ContentPins = .{ null, null, null };
    errdefer for (pins) |maybe_pin| {
        if (maybe_pin) |pin| screen.pages.untrackPin(pin);
    };
    if (config.tracked) {
        for ([3]usize{ 0, config.records / 2, config.records - 1 }, 0..) |record, i| {
            const pin = screen.pages.pin(.{ .screen = .{ .y = @intCast(record * record_rows) } }) orelse return error.InvalidPin;
            pins[i] = try screen.pages.trackPin(pin);
        }
    }
    return pins;
}

fn checkContent(terminal: *const vt.Terminal, config: ContentConfig, layout: []const ContentCell, columns: usize, pins: ContentPins) !u64 {
    const screen = terminal.screens.active;
    const record_rows = layout.len / columns;
    const populated_rows = config.records * record_rows;
    const total_rows = @max(populated_rows + 1, rows);
    if (screen.pages.total_rows != total_rows or screen.pages.rows != rows or screen.pages.cols != columns) return error.InvalidHistory;
    if (screen.cursor.x != 0 or screen.cursor.y != @min(populated_rows, rows - 1) or screen.cursor.pending_wrap) return error.InvalidCursor;
    var it = screen.pages.rowIterator(.right_down, .{ .screen = .{} }, null);
    var index: usize = 0;
    while (it.next()) |pin| : (index += 1) {
        const record = index / record_rows;
        const part = index % record_rows;
        const populated = index < populated_rows;
        const header = pin.rowAndCell().row;
        if (header.wrap != (populated and part + 1 < record_rows) or header.wrap_continuation != (populated and part != 0)) return error.InvalidWrap;
        var style: vt.Style = .{};
        if (config.styled and populated) {
            style.fg_color = .{ .palette = @intCast(1 + record % 4) };
            style.flags.bold = record % 2 == 0;
        }
        var id_buffer: [24]u8 = undefined;
        var uri_buffer: [80]u8 = undefined;
        const id = try std.fmt.bufPrint(&id_buffer, "{}", .{record});
        const uri = try std.fmt.bufPrint(&uri_buffer, "https://example.test/reflow/{}", .{record});
        const actual = pin.cells(.all);
        if (actual.len != columns) return error.InvalidHistory;
        for (actual, 0..) |*cell, col| {
            if (cell.content_tag != .codepoint and cell.content_tag != .codepoint_grapheme) return error.InvalidStyle;
            const expected: ContentCell = if (populated) layout[part * columns + col] else .{};
            const logical = expected.kind == .head or expected.kind == .tail;
            const wide: vt.Cell.Wide = switch (expected.kind) {
                .head => if (config.pattern.units()[expected.unit].width == 2) .wide else .narrow,
                .tail => .spacer_tail,
                .spacer => .spacer_head,
                .padding => .narrow,
            };
            if (cell.wide != wide) return error.InvalidWidth;
            const extra = pin.grapheme(cell) orelse &.{};
            if (expected.kind == .head) {
                var scalars = (try std.unicode.Utf8View.init(config.pattern.units()[expected.unit].text)).iterator();
                if (cell.codepoint() != scalars.nextCodepoint().?) return error.InvalidText;
                var n: usize = 0;
                while (scalars.nextCodepoint()) |cp| : (n += 1) {
                    if (n >= extra.len or extra[n] != cp) return error.InvalidGrapheme;
                }
                if (n != extra.len or cell.hasGrapheme() != (n > 0)) return error.InvalidGrapheme;
            } else if (cell.codepoint() != 0 or cell.hasGrapheme() or extra.len != 0) return error.InvalidText;
            // Spacer-head padding can retain the printing pen, whereas newly
            // generated reflow spacers are unstyled and unlinked.
            if (expected.kind == .spacer) continue;
            if (!pin.style(cell).eql(if (logical) style else .{})) return error.InvalidStyle;
            const linked = logical and expected.repeat < config.link_repeats;
            if (cell.hyperlink != linked) {
                std.debug.print("Hyperlink flag mismatch: cols={} record={} row={} col={} expected={} actual={}\n", .{ columns, record, part, col, linked, cell.hyperlink });
                return error.InvalidHyperlink;
            }
            if (cell.hyperlink) {
                const page = pin.node.page();
                const link_id = page.lookupHyperlink(cell) orelse return error.InvalidHyperlink;
                const link = page.hyperlink_set.get(page.memory, link_id);
                if (!std.mem.eql(u8, uri, link.uri.slice(page.memory))) return error.InvalidHyperlink;
                switch (link.id) {
                    .explicit => |value| if (!std.mem.eql(u8, id, value.slice(page.memory))) return error.InvalidHyperlink,
                    .implicit => return error.InvalidHyperlink,
                }
            }
        }
    }
    if (index != total_rows) return error.InvalidHistory;
    for (pins, [3]usize{ 0, config.records / 2, config.records - 1 }) |maybe_pin, record| {
        if (maybe_pin) |pin| {
            const point = screen.pages.pointFromPin(.screen, pin.*) orelse return error.InvalidPin;
            if (point.screen.x != 0 or point.screen.y != record * record_rows) return error.InvalidPin;
        }
    }
    return historyChecksum(screen);
}

fn contentBenchmark(init: std.process.Init, bytes: []const u8, iterations: u64) !void {
    const alloc = init.gpa;
    const parsed_config = try std.json.parseFromSlice(ContentConfig, alloc, bytes, .{});
    defer parsed_config.deinit();
    const config = parsed_config.value;
    if (config.records == 0 or config.narrow < 2 or config.narrow > cols or config.link_repeats > config.pattern.repeats()) return error.InvalidConfig;
    const wide_layout = try contentLayout(alloc, config.pattern, cols);
    defer alloc.free(wide_layout);
    const narrow_layout = try contentLayout(alloc, config.pattern, config.narrow);
    defer alloc.free(narrow_layout);
    var terminal = try vt.Terminal.init(init.io, alloc, .{
        .cols = cols,
        .rows = rows,
        .max_scrollback_bytes = null,
        .max_scrollback_lines = null,
        .default_modes = .{ .grapheme_cluster = true },
    });
    defer terminal.deinit(alloc);
    var stream = vt.TerminalStream.init(.{ .allocator = alloc, .handler = .init(&terminal) });
    defer stream.deinit();
    try feedContent(alloc, &stream, config);
    const checksum = checkContent(&terminal, config, wide_layout, cols, .{ null, null, null }) catch |err| {
        std.debug.print("Content validation failed after input, before resize\n", .{});
        return err;
    };
    const pins = try trackContent(terminal.screens.active, config, wide_layout.len / cols);
    defer for (pins) |maybe_pin| {
        if (maybe_pin) |pin| terminal.screens.active.pages.untrackPin(pin);
    };
    try terminal.resize(alloc, .{ .cols = config.narrow, .rows = rows });
    if (try checkContent(&terminal, config, narrow_layout, config.narrow, pins) != checksum) return error.ChecksumMismatch;
    try terminal.resize(alloc, .{ .cols = cols, .rows = rows });
    if (try checkContent(&terminal, config, wide_layout, cols, pins) != checksum) return error.ChecksumMismatch;
    const start = std.Io.Timestamp.now(init.io, .awake);
    for (0..iterations) |_| {
        try terminal.resize(alloc, .{ .cols = config.narrow, .rows = rows });
        try terminal.resize(alloc, .{ .cols = cols, .rows = rows });
        std.mem.doNotOptimizeAway(&terminal);
    }
    const elapsed = start.durationTo(.now(init.io, .awake)).nanoseconds;
    if (try checkContent(&terminal, config, wide_layout, cols, pins) != checksum) return error.ChecksumMismatch;
    var buffer: [4096]u8 = undefined;
    var stdout = std.Io.File.stdout().writerStreaming(init.io, &buffer);
    try std.json.Stringify.value(.{
        .engine = "ghostty",
        .operation = "reflow_history_content",
        .iterations = iterations,
        .elapsed_ns = elapsed,
        .units_per_iteration = config.records,
        .cell_bytes = @sizeOf(vt.Cell),
        .checksum = checksum,
    }, .{}, &stdout.interface);
    try stdout.interface.writeByte('\n');
    try stdout.interface.flush();
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
        .reflow, .reflow_history_styled, .reflow_scrollback => {
            try terminal.resize(terminal.gpa(), .{ .cols = cols / 2, .rows = rows });
            try terminal.resize(terminal.gpa(), .{ .cols = cols, .rows = rows });
        },
        .reflow_history_content, .kitty_place => unreachable,
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
    const bytes = try std.Io.Dir.cwd().readFileAlloc(init.io, args[2], alloc, .limited(128 * 1024 * 1024));
    defer alloc.free(bytes);
    if (op == .reflow_history_content) return contentBenchmark(init, bytes, iterations);
    if (op == .kitty_place) return @import("zig-kitty-bench.zig").run(init, bytes, iterations);
    const cps = if (parsed(op)) try alloc.alloc(u21, 0) else try decode(alloc, bytes);
    defer alloc.free(cps);
    const styled_records = std.mem.count(u8, bytes, "\r\n");
    var terminal = try vt.Terminal.init(init.io, alloc, .{
        .cols = cols,
        .rows = rows,
        .max_scrollback_bytes = if (isStream(op) or op == .reflow_history_styled or op == .reflow_scrollback) null else 0,
        .max_scrollback_lines = if (isStream(op)) history_lines else null,
        .default_modes = .{ .grapheme_cluster = true },
    });
    defer terminal.deinit(alloc);
    var stream = vt.TerminalStream.init(.{ .allocator = alloc, .handler = .init(&terminal) });
    defer stream.deinit();
    if (isStream(op)) {
        for (0..prime_batches) |_| stream.nextSlice(bytes);
    } else if (op == .feed or op == .reflow_history_styled or op == .reflow_scrollback) {
        stream.nextSlice(bytes);
    } else {
        try fill(&terminal, cps);
    }
    if (op == .reflow_history_styled) {
        _ = try checkStyledHistory(&terminal, styled_records, cols);
        try terminal.resize(terminal.gpa(), .{ .cols = cols / 2, .rows = rows });
        _ = try checkStyledHistory(&terminal, styled_records, cols / 2);
        try terminal.resize(terminal.gpa(), .{ .cols = cols, .rows = rows });
    }
    const expected = switch (op) {
        .width => try step(.width, &terminal, &stream, cps, bytes),
        .stream, .stream_styled => try checkStream(&terminal),
        .reflow_history_styled => try checkStyledHistory(&terminal, styled_records, cols),
        .reflow_scrollback => historyChecksum(terminal.screens.active),
        .scalar => read(terminal.screens.active, false),
        else => read(terminal.screens.active, true),
    };
    if (op == .reflow_scrollback) {
        // Match Rust's priming and validate the narrowed history as well.
        try terminal.resize(terminal.gpa(), .{ .cols = cols / 2, .rows = rows });
        if (historyChecksum(terminal.screens.active) != expected) return error.ChecksumMismatch;
        try terminal.resize(terminal.gpa(), .{ .cols = cols, .rows = rows });
        if (historyChecksum(terminal.screens.active) != expected) return error.ChecksumMismatch;
    }

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
        .reflow_history_styled => checksum = try checkStyledHistory(&terminal, styled_records, cols),
        .reflow_scrollback => checksum = historyChecksum(terminal.screens.active),
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
            .reflow, .reflow_history_content, .kitty_place => 1,
            .reflow_history_styled => styled_records,
            .reflow_scrollback => 1,
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

test "long-history content checks mixed clusters links and anchors at odd widths" {
    const alloc = std.testing.allocator;
    const config: ContentConfig = .{ .pattern = .mixed, .records = 40, .styled = true, .link_repeats = 32, .tracked = true, .narrow = 63 };
    const wide_layout = try contentLayout(alloc, config.pattern, cols);
    defer alloc.free(wide_layout);
    const narrow_layout = try contentLayout(alloc, config.pattern, config.narrow);
    defer alloc.free(narrow_layout);
    var terminal = try vt.Terminal.init(std.testing.io, alloc, .{
        .cols = cols,
        .rows = rows,
        .max_scrollback_bytes = null,
        .max_scrollback_lines = null,
        .default_modes = .{ .grapheme_cluster = true },
    });
    defer terminal.deinit(alloc);
    var stream = vt.TerminalStream.init(.{ .allocator = alloc, .handler = .init(&terminal) });
    defer stream.deinit();
    try feedContent(alloc, &stream, config);
    const checksum = try checkContent(&terminal, config, wide_layout, cols, .{ null, null, null });
    const pins = try trackContent(terminal.screens.active, config, wide_layout.len / cols);
    defer for (pins) |maybe_pin| {
        if (maybe_pin) |pin| terminal.screens.active.pages.untrackPin(pin);
    };
    for (0..2) |_| {
        try terminal.resize(alloc, .{ .cols = config.narrow, .rows = rows });
        try std.testing.expectEqual(checksum, try checkContent(&terminal, config, narrow_layout, config.narrow, pins));
        try terminal.resize(alloc, .{ .cols = cols, .rows = rows });
        try std.testing.expectEqual(checksum, try checkContent(&terminal, config, wide_layout, cols, pins));
    }
}
