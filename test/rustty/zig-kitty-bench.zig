//! Kitty sprite and per-cell updates using the same bytes as the Rust benchmark.
const std = @import("std");
const vt = @import("ghostty-vt");

const Input = struct { sprites: usize, setup: []const u8, frames: [2][]const u8, lanes: ?usize = null, images: ?usize = null };

fn check(terminal: *vt.Terminal, sprites: usize, phase: usize) !u64 {
    const screen = terminal.screens.active;
    const graphics = &screen.kitty_images;
    if (graphics.images.count() != 1 or graphics.placements.count() != sprites) return error.InvalidCount;
    const image = graphics.imageById(100) orelse return error.MissingImage;
    const pixels = image.data.bytes() orelse return error.PendingImage;
    if (image.width != 128 or image.height != 128 or pixels.len != 128 * 128 * 4) return error.InvalidImage;
    for (pixels, 0..) |byte, i| {
        if (byte != ([_]u8{ 24, 48, 96, 255 })[i % 4]) return error.InvalidPixels;
    }
    var checksum: u64 = 0;
    var placements = graphics.placements.iterator();
    while (placements.next()) |entry| {
        const key = entry.key_ptr.*;
        const p = entry.value_ptr.*;
        if (key.image_id != 100 or key.placement_id.tag != .external or key.placement_id.id == 0 or key.placement_id.id > sprites) return error.InvalidId;
        const i: usize = key.placement_id.id - 1;
        const pin = switch (p.location) {
            .pin => |pin| pin,
            else => return error.InvalidLocation,
        };
        const point = screen.pages.pointFromPin(.active, pin.*) orelse return error.InvalidPin;
        if (point.coord().x != (i * 7 + phase) % 128 or point.coord().y != (i * 3 + phase) % 32) return error.InvalidPosition;
        if (p.source_x != i % 4 * 32 or p.source_y != i / 4 % 4 * 32 or p.source_width != 32 or p.source_height != 32) return error.InvalidSource;
        if (p.x_offset != 3 or p.y_offset != 5 or p.z != -400 - @as(i32, @intCast(i % 5)) or p.columns != 0 or p.rows != 0) return error.InvalidGeometry;
        checksum += key.placement_id.id;
    }
    if (screen.cursor.x != ((sprites - 1) * 7 + phase) % 128 or screen.cursor.y != ((sprites - 1) * 3 + phase) % 32) return error.InvalidCursor;
    return checksum;
}

fn checkCells(terminal: *vt.Terminal, lanes: usize, image_count: usize, phase: usize) !u64 {
    const screen = terminal.screens.active;
    const graphics = &screen.kitty_images;
    if (graphics.images.count() != image_count or graphics.placements.count() != 80 * lanes) return error.InvalidCount;
    var numbers = [_]bool{false} ** 1024;
    var images = graphics.images.valueIterator();
    while (images.next()) |image| {
        const number = image.number;
        if (number == 0 or number > image_count or numbers[number - 1]) return error.InvalidImageNumber;
        numbers[number - 1] = true;
        const pixels = image.data.bytes() orelse return error.PendingImage;
        if (image.width != 10 or image.height != 20 or pixels.len != 10 * 20 * 4) return error.InvalidImage;
        const color = [_]u8{ @truncate(number), @truncate(number >> 8), @truncate(number * 17), 255 };
        for (pixels, 0..) |byte, i| {
            if (byte != color[i % 4]) return error.InvalidPixels;
        }
    }
    var ids = [_]bool{false} ** (80 * 128);
    var checksum: u64 = 0;
    var placements = graphics.placements.iterator();
    while (placements.next()) |entry| {
        const key = entry.key_ptr.*;
        const p = entry.value_ptr.*;
        if (key.placement_id.tag != .external or key.placement_id.id == 0) return error.InvalidId;
        const row = (key.placement_id.id - 1) >> 16;
        const col = (key.placement_id.id - 1) & 0xffff;
        if (row >= 80 or col >= lanes) return error.InvalidId;
        const i: usize = row * lanes + col;
        if (ids[i]) return error.DuplicatePlacement;
        ids[i] = true;
        const image = graphics.imageById(key.image_id) orelse return error.MissingImage;
        if (image.number != (i + phase * image_count / 2) % image_count + 1) return error.InvalidImageNumber;
        const pin = switch (p.location) {
            .pin => |pin| pin,
            else => return error.InvalidLocation,
        };
        const point = screen.pages.pointFromPin(.active, pin.*) orelse return error.InvalidPin;
        if (point.coord().x != i % lanes or point.coord().y != i / lanes) return error.InvalidPosition;
        if (p.source_x != 0 or p.source_y != 0 or p.source_width != 0 or p.source_height != 0) return error.InvalidSource;
        if (p.x_offset != 0 or p.y_offset != 0 or p.z != 0 or p.columns != 1 or p.rows != 1) return error.InvalidGeometry;
        checksum += @as(u64, key.placement_id.id) * image.number;
    }
    if (screen.cursor.x != lanes - 1 or screen.cursor.y != 79) return error.InvalidCursor;
    return checksum;
}

pub fn run(init: std.process.Init, bytes: []const u8, iterations: u64) !void {
    const parsed = try std.json.parseFromSlice(Input, init.gpa, bytes, .{});
    defer parsed.deinit();
    const input = parsed.value;
    if (input.lanes) |lanes| {
        if (lanes == 0 or lanes > 128 or input.sprites != lanes * 80) return error.InvalidCount;
        const image_count = input.images orelse return error.InvalidCount;
        if (image_count < 2 or image_count > 1024 or image_count > input.sprites) return error.InvalidCount;
    } else if (input.sprites == 0 or input.sprites > 1024) return error.InvalidCount;
    const rows: u16 = if (input.lanes != null) 80 else 32;
    var terminal = try vt.Terminal.init(init.io, init.gpa, .{ .cols = 128, .rows = rows, .max_scrollback_bytes = 0 });
    defer terminal.deinit(init.gpa);
    terminal.width_px = 1280;
    terminal.height_px = @as(u32, rows) * 20;
    var stream = vt.TerminalStream.init(.{ .allocator = init.gpa, .handler = .init(&terminal) });
    defer stream.deinit();
    stream.nextSlice(input.setup);
    for (input.frames, 0..) |frame, phase| {
        stream.nextSlice(frame);
        _ = if (input.lanes) |lanes| try checkCells(&terminal, lanes, input.images.?, phase) else try check(&terminal, input.sprites, phase);
    }
    const start = std.Io.Timestamp.now(init.io, .awake);
    for (0..iterations) |_| {
        for (input.frames) |frame| stream.nextSlice(frame);
        std.mem.doNotOptimizeAway(&terminal);
    }
    const elapsed = start.durationTo(.now(init.io, .awake)).nanoseconds;
    const checksum = if (input.lanes) |lanes| try checkCells(&terminal, lanes, input.images.?, 1) else try check(&terminal, input.sprites, 1);
    if (input.lanes != null) {
        stream.nextSlice("\x1b_Ga=d,d=A,q=2\x1b\\");
        if (terminal.screens.active.kitty_images.images.count() != 0 or terminal.screens.active.kitty_images.placements.count() != 0) return error.CleanupFailed;
    }
    var buffer: [4096]u8 = undefined;
    var stdout = std.Io.File.stdout().writerStreaming(init.io, &buffer);
    try std.json.Stringify.value(.{
        .engine = "ghostty",
        .operation = if (input.lanes != null) "kitty_cells" else "kitty_place",
        .iterations = iterations,
        .elapsed_ns = elapsed,
        .units_per_iteration = 2 * input.sprites,
        .cell_bytes = @sizeOf(vt.Cell),
        .checksum = checksum,
    }, .{}, &stdout.interface);
    try stdout.interface.writeByte('\n');
    try stdout.interface.flush();
}
