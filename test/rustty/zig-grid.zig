//! Direct selection/search/tracked-reference APIs, independent of snapshots.
const std = @import("std");
const vt = @import("ghostty-vt");
const Allocator = std.mem.Allocator;

pub const Point = struct {
    tag: vt.point.Tag = .active,
    x: u16 = 0,
    y: u32 = 0,

    fn native(self: Point) vt.Point {
        return switch (self.tag) {
            inline else => |tag| @unionInit(vt.Point, @tagName(tag), .{ .x = self.x, .y = self.y }),
        };
    }
};
pub const Operation = struct {
    action: []const u8 = "",
    id: u32 = 0,
    point: Point = .{},
    start: Point = .{},
    end: Point = .{},
    rectangle: bool = false,
    needle: []const u8 = "",
    delta: i32 = 0,
    lines: ?usize = null,
    bytes: ?usize = null,
};
const Location = struct {
    screen: ?[2]u32,
    history: ?[2]u32,
    active: ?[2]u32,
    viewport: ?[2]u32,
};
const Selection = struct {
    start: ?Location,
    end: ?Location,
    rectangle: bool,
    text: ?[]const u8,
};
const Match = struct { start: ?Location, end: ?Location };
const Tracked = struct {
    id: u32,
    screen: []const u8,
    value: ?struct { location: ?Location, text: []const u21 },
};
pub const Result = struct {
    action: []const u8,
    status: []const u8,
    matches: ?[]const Match,
    active_screen: []const u8,
    viewport_top: ?[2]u32,
    selection: ?Selection,
    tracked: []const Tracked,
};
const Handle = struct {
    id: u32,
    key: vt.ScreenSet.Key,
    generation: usize,
    pin: *vt.Pin,

    fn screen(self: Handle, terminal: *vt.Terminal) ?*vt.Screen {
        // This is the same lifetime check used by the native C tracked-handle
        // wrapper. Never dereference a pin after its screen is reinitialized.
        if (terminal.screens.generation(self.key) != self.generation) return null;
        return terminal.screens.get(self.key);
    }
};
pub const Context = struct {
    handles: std.ArrayList(Handle) = .empty,

    pub fn hasHandles(self: Context) bool {
        return self.handles.items.len != 0;
    }

    pub fn deinit(self: *Context, alloc: Allocator, terminal: *vt.Terminal) void {
        for (self.handles.items) |handle| {
            if (handle.screen(terminal)) |screen| screen.pages.untrackPin(handle.pin);
        }
        self.handles.deinit(alloc);
    }

    pub fn run(self: *Context, alloc: Allocator, terminal: *vt.Terminal, op: Operation) !Result {
        var status: []const u8 = "ok";
        var matches: ?[]const Match = null;
        const screen = terminal.screens.active;
        if (std.mem.eql(u8, op.action, "observe")) {
            // Reading is explicit so writes retain their original boundaries.
        } else if (std.mem.eql(u8, op.action, "select")) {
            if (screen.pages.pin(op.start.native())) |start| {
                if (screen.pages.pin(op.end.native())) |end| {
                    try screen.select(vt.Selection.init(start, end, op.rectangle));
                } else status = "invalid";
            } else status = "invalid";
        } else if (std.mem.eql(u8, op.action, "clear_selection")) {
            screen.clearSelection();
        } else if (std.mem.eql(u8, op.action, "track")) {
            const duplicate = for (self.handles.items) |handle| {
                if (handle.id == op.id) break true;
            } else false;
            if (duplicate) {
                status = "invalid";
            } else if (screen.pages.pin(op.point.native())) |pin| {
                const tracked = try screen.pages.trackPin(pin);
                errdefer screen.pages.untrackPin(tracked);
                try self.handles.append(alloc, .{
                    .id = op.id,
                    .key = terminal.screens.active_key,
                    .generation = terminal.screens.generation(terminal.screens.active_key),
                    .pin = tracked,
                });
            } else status = "invalid";
        } else if (std.mem.eql(u8, op.action, "untrack")) {
            const index = for (self.handles.items, 0..) |handle, index| {
                if (handle.id == op.id) break index;
            } else null;
            if (index) |i| {
                const handle = self.handles.orderedRemove(i);
                if (handle.screen(terminal)) |owner| owner.pages.untrackPin(handle.pin);
            } else status = "invalid";
        } else if (std.mem.eql(u8, op.action, "viewport")) {
            terminal.scrollViewport(.{ .delta = op.delta });
        } else if (std.mem.eql(u8, op.action, "limits")) {
            terminal.setScrollbackMaxLines(op.lines);
            terminal.setScrollbackMaxBytes(op.bytes);
        } else if (std.mem.eql(u8, op.action, "search")) {
            const needle = try unhex(alloc, op.needle);
            if (needle.len == 0) {
                // The native public search API clears an empty needle.
                matches = &.{};
            } else {
                var search = try vt.search.Screen.init(alloc, screen, needle);
                defer search.deinit();
                try search.searchAll();
                const found = try search.matches(alloc);
                defer alloc.free(found);
                const results = try alloc.alloc(Match, found.len);
                for (found, results) |value, *result| {
                    const bounds = value.untracked();
                    result.* = .{ .start = location(screen, bounds.start), .end = location(screen, bounds.end) };
                }
                matches = results;
            }
        } else return error.UnsupportedGridAction;

        const tracked = try alloc.alloc(Tracked, self.handles.items.len);
        for (self.handles.items, tracked) |handle, *result| {
            result.* = .{ .id = handle.id, .screen = @tagName(handle.key), .value = null };
            if (handle.screen(terminal)) |owner| {
                if (!handle.pin.garbage) result.value = .{
                    .location = location(owner, handle.pin.*),
                    .text = try cellText(alloc, handle.pin.*),
                };
            }
        }
        const active = terminal.screens.active;
        const selection = if (active.selection) |selection| sel: {
            const start = location(active, selection.start());
            const end = location(active, selection.end());
            const text = if (start != null and end != null)
                try hex(alloc, try active.selectionString(alloc, .{ .sel = selection, .trim = true }))
            else
                null;
            break :sel Selection{ .start = start, .end = end, .rectangle = selection.rectangle, .text = text };
        } else null;
        return .{
            .action = op.action,
            .status = status,
            .matches = matches,
            .active_screen = @tagName(terminal.screens.active_key),
            .viewport_top = coordinate(active, .screen, active.pages.getTopLeft(.viewport)),
            .selection = selection,
            .tracked = tracked,
        };
    }
};

fn coordinate(screen: *vt.Screen, tag: vt.point.Tag, pin: vt.Pin) ?[2]u32 {
    if (pin.garbage) return null;
    const point = screen.pages.pointFromPin(tag, pin) orelse return null;
    return .{ point.coord().x, point.coord().y };
}

fn location(screen: *vt.Screen, pin: vt.Pin) ?Location {
    if (pin.garbage) return null;
    return .{
        .screen = coordinate(screen, .screen, pin),
        .history = coordinate(screen, .history, pin),
        .active = coordinate(screen, .active, pin),
        .viewport = coordinate(screen, .viewport, pin),
    };
}

fn cellText(alloc: Allocator, pin: vt.Pin) ![]const u21 {
    var values: std.ArrayList(u21) = .empty;
    const cell = pin.rowAndCell().cell;
    if (cell.codepoint() != 0) try values.append(alloc, cell.codepoint());
    if (pin.grapheme(cell)) |extra| try values.appendSlice(alloc, extra);
    return values.items;
}

fn unhex(alloc: Allocator, text: []const u8) ![]const u8 {
    if (text.len % 2 != 0) return error.InvalidHex;
    const bytes = try alloc.alloc(u8, text.len / 2);
    return std.fmt.hexToBytes(bytes, text) catch return error.InvalidHex;
}

fn hex(alloc: Allocator, bytes: []const u8) ![]const u8 {
    const result = try alloc.alloc(u8, bytes.len * 2);
    const digits = "0123456789abcdef";
    for (bytes, 0..) |byte, i| {
        result[i * 2] = digits[byte >> 4];
        result[i * 2 + 1] = digits[byte & 15];
    }
    return result;
}
