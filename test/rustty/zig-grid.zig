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
    active_dirty: ?bool = null,
    scroll: ?bool = null,
    delta: i32 = 0,
    lines: ?usize = null,
    bytes: ?usize = null,
    boundary_codepoints: ?[]const u32 = null,
    whitespace: ?[]const u32 = null,
    trim_line: ?bool = null,
    semantic_prompt_boundary: ?bool = null,
    adjustment: ?vt.Selection.Adjustment = null,
    format: ?struct { emit: vt.formatter.Format = .plain, unwrap: bool = true, trim: bool = true } = null,
    gesture: ?GestureOptions = null,
};
const GestureOptions = struct {
    time: ?i64 = null,
    xpos: f64 = 0,
    ypos: f64 = 0,
    max_distance: f64 = 10,
    repeat_interval: u64 = 500_000_000,
    behaviors: [3]vt.SelectionGesture.Behavior = vt.SelectionGesture.default_behaviors,
    geometry: ?vt.SelectionGesture.Drag.Geometry = null,
};
const GestureState = struct {
    click_count: u3,
    behavior: vt.SelectionGesture.Behavior,
    dragged: bool,
    autoscroll: vt.SelectionGesture.Autoscroll,
    anchor_retained: bool,
    anchor_valid: bool,
    anchor: ?Location,
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
const SearchState = struct {
    status: []const u8,
    tick: ?[]const u8,
    total: usize,
    selected_index: ?usize,
    selected_match: ?Match,
    screen: []const u8,
};
const Bounds = struct { start: ?Location, end: ?Location, rectangle: bool };
const Tracked = struct {
    id: u32,
    screen: []const u8,
    value: ?struct { location: ?Location, text: []const u21 },
};
pub const Result = struct {
    action: []const u8,
    status: []const u8,
    matches: ?[]const Match,
    search_needle: ?[]const u8,
    search_state: ?SearchState,
    active_screen: []const u8,
    viewport_top: ?[2]u32,
    selection: ?Selection,
    selection_result: ?Bounds,
    formatted: ?[]const u8,
    tracked: []const Tracked,
    gesture: ?GestureState,
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
    search: ?vt.search.Terminal = null,
    gesture: vt.SelectionGesture = .init,
    gesture_used: bool = false,

    pub fn hasHandles(self: Context) bool {
        return self.handles.items.len != 0 or self.search != null or self.gesture.left_click_pin != null;
    }

    pub fn deinit(self: *Context, alloc: Allocator, terminal: *vt.Terminal) void {
        if (self.search) |*search| search.deinit(terminal);
        for (self.handles.items) |handle| {
            if (handle.screen(terminal)) |screen| screen.pages.untrackPin(handle.pin);
        }
        self.handles.deinit(alloc);
        self.gesture.deinit(terminal);
    }

    pub fn run(self: *Context, alloc: Allocator, terminal: *vt.Terminal, op: Operation) !Result {
        var status: []const u8 = "ok";
        var matches: ?[]const Match = null;
        var search_needle: ?[]const u8 = null;
        var search_state: ?SearchState = null;
        var search_tick: ?[]const u8 = null;
        var selection_result: ?Bounds = null;
        const screen = terminal.screens.active;
        if (std.mem.eql(u8, op.action, "observe")) {
            // Reading is explicit so writes retain their original boundaries.
        } else if (std.mem.eql(u8, op.action, "format_selection")) {
            if (screen.selection == null) status = "no_value";
        } else if (std.mem.eql(u8, op.action, "select")) {
            if (screen.pages.pin(op.start.native())) |start| {
                if (screen.pages.pin(op.end.native())) |end| {
                    try screen.select(vt.Selection.init(start, end, op.rectangle));
                } else status = "invalid";
            } else status = "invalid";
        } else if (std.mem.eql(u8, op.action, "clear_selection")) {
            screen.clearSelection();
        } else if (std.mem.eql(u8, op.action, "adjust_selection")) {
            const adjustment = op.adjustment orelse return error.InvalidAdjustment;
            if (screen.selection) |*selection| {
                selection.adjust(screen, adjustment);
            } else status = "no_value";
        } else if (std.mem.eql(u8, op.action, "select_word") or
            std.mem.eql(u8, op.action, "select_word_between") or
            std.mem.eql(u8, op.action, "select_line") or
            std.mem.eql(u8, op.action, "select_output") or
            std.mem.eql(u8, op.action, "select_all"))
        {
            // The native selectWord API takes an explicit boundary set; these
            // are selection_codepoints.default_word_boundaries.
            const defaults = [_]u21{ 0, ' ', '\t', '\'', '"', '│', '`', '|', ':', ';', ',', '(', ')', '[', ']', '{', '}', '<', '>', '$' };
            const boundaries = (try codepoints(alloc, op.boundary_codepoints)) orelse &defaults;
            const whitespace = try codepoints(alloc, op.whitespace);
            const selected: ?vt.Selection = if (std.mem.eql(u8, op.action, "select_all"))
                screen.selectAll()
            else if (std.mem.eql(u8, op.action, "select_word_between")) selected: {
                const start = screen.pages.pin(op.start.native()) orelse {
                    status = "invalid";
                    break :selected null;
                };
                const end = screen.pages.pin(op.end.native()) orelse {
                    status = "invalid";
                    break :selected null;
                };
                break :selected screen.selectWordBetween(start, end, boundaries);
            } else selected: {
                const pin = screen.pages.pin(op.point.native()) orelse {
                    status = "invalid";
                    break :selected null;
                };
                if (std.mem.eql(u8, op.action, "select_word")) break :selected screen.selectWord(pin, boundaries);
                if (std.mem.eql(u8, op.action, "select_output")) break :selected screen.selectOutput(pin);
                var options: vt.Screen.SelectLine = .{ .pin = pin };
                if (!(op.trim_line orelse true)) options.whitespace = null else if (whitespace) |value| options.whitespace = value;
                options.semantic_prompt_boundary = op.semantic_prompt_boundary orelse true;
                break :selected screen.selectLine(options);
            };
            if (selected) |selection| {
                selection_result = .{
                    .start = location(screen, selection.start()),
                    .end = location(screen, selection.end()),
                    .rectangle = selection.rectangle,
                };
            } else if (std.mem.eql(u8, status, "ok")) status = "no_value";
        } else if (std.mem.startsWith(u8, op.action, "gesture_")) {
            self.gesture_used = true;
            const options = op.gesture orelse GestureOptions{};
            const defaults = [_]u21{ 0, ' ', '\t', '\'', '"', '│', '`', '|', ':', ';', ',', '(', ')', '[', ']', '{', '}', '<', '>', '$' };
            const boundaries = (try codepoints(alloc, op.boundary_codepoints)) orelse &defaults;
            const geometry = options.geometry orelse vt.SelectionGesture.Drag.Geometry{
                .columns = terminal.cols,
                .cell_width = 10,
                .padding_left = 5,
                .screen_height = 100,
            };
            const pin = screen.pages.pin(op.point.native());
            const selected: ?vt.Selection = selected: {
                if (std.mem.eql(u8, op.action, "gesture_press")) {
                    const point = pin orelse {
                        status = "invalid";
                        break :selected null;
                    };
                    break :selected try self.gesture.press(terminal, .{
                        .pin = point,
                        .time = if (options.time) |time| .{ .nanoseconds = time } else null,
                        .xpos = options.xpos,
                        .ypos = options.ypos,
                        .max_distance = options.max_distance,
                        .repeat_interval = options.repeat_interval,
                        .behaviors = &options.behaviors,
                        .word_boundary_codepoints = boundaries,
                    });
                } else if (std.mem.eql(u8, op.action, "gesture_drag")) {
                    const point = pin orelse {
                        status = "invalid";
                        break :selected null;
                    };
                    break :selected self.gesture.drag(terminal, .{ .pin = point, .xpos = options.xpos, .ypos = options.ypos, .rectangle = op.rectangle, .word_boundary_codepoints = boundaries, .geometry = geometry });
                } else if (std.mem.eql(u8, op.action, "gesture_release")) {
                    self.gesture.release(terminal, .{ .pin = pin });
                } else if (std.mem.eql(u8, op.action, "gesture_reset")) {
                    self.gesture.reset(terminal);
                } else if (std.mem.eql(u8, op.action, "gesture_deep_press")) {
                    break :selected self.gesture.deepPress(terminal, .{ .word_boundary_codepoints = boundaries });
                } else if (std.mem.eql(u8, op.action, "gesture_autoscroll")) {
                    break :selected self.gesture.autoscrollTick(terminal, .{ .viewport = .{ .x = op.point.x, .y = op.point.y }, .xpos = options.xpos, .ypos = options.ypos, .rectangle = op.rectangle, .word_boundary_codepoints = boundaries, .geometry = geometry });
                } else return error.UnsupportedGridAction;
                break :selected null;
            };
            if (selected) |selection| selection_result = .{
                .start = location(screen, selection.start()),
                .end = location(screen, selection.end()),
                .rectangle = selection.rectangle,
            };
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
        } else if (std.mem.eql(u8, op.action, "search_needle")) {
            const needle = try unhex(alloc, op.needle);
            // Mirror the native C wrapper's lifecycle around TerminalSearch.
            if (needle.len == 0) {
                if (self.search) |*search| search.deinit(terminal);
                self.search = null;
            } else {
                const same = if (self.search) |*search| std.ascii.eqlIgnoreCase(search.needle(), needle) else false;
                if (!same) {
                    const replacement = try vt.search.Terminal.init(alloc, needle);
                    if (self.search) |*search| search.deinit(terminal);
                    self.search = replacement;
                }
            }
            search_needle = try hex(alloc, if (self.search) |*search| search.needle() else "");
        } else if (std.mem.eql(u8, op.action, "search_feed")) {
            if (self.search) |*search| search.feed(terminal, op.active_dirty orelse true);
        } else if (std.mem.eql(u8, op.action, "search_viewport")) {
            if (self.search) |*search| {
                const found = try search.viewportMatches();
                const results = try alloc.alloc(Match, found.len);
                for (found, results) |value, *result| {
                    const bounds = value.untracked();
                    result.* = .{ .start = location(screen, bounds.start), .end = location(screen, bounds.end) };
                }
                matches = results;
            } else matches = &.{};
        } else if (std.mem.eql(u8, op.action, "search_status") or std.mem.eql(u8, op.action, "search_selected")) {
            // Read the cached state below, without feeding.
        } else if (std.mem.eql(u8, op.action, "search_tick")) {
            search_tick = if (self.search) |*search| @tagName(search.tick()) else "complete";
        } else if (std.mem.eql(u8, op.action, "search_run")) {
            if (self.search) |*search| {
                search.feed(terminal, true);
                while (true) switch (search.status()) {
                    .complete => break,
                    .running => _ = search.tick(),
                    .feed_required => search.feed(terminal, true),
                };
            }
        } else if (std.mem.eql(u8, op.action, "search_matches") or std.mem.eql(u8, op.action, "search_match")) {
            const single = std.mem.eql(u8, op.action, "search_match");
            const searcher = if (self.search) |*search| search.activeScreenSearch() else null;
            const total = if (searcher) |search| search.matchesLen() else 0;
            const count = if (single) @intFromBool(op.id < total) else total;
            const results = try alloc.alloc(Match, count);
            for (results, 0..) |*result, index| {
                const search = searcher.?;
                const found = search.matchAt(if (single) op.id else index).?.untracked();
                result.* = .{ .start = location(search.screen, found.start), .end = location(search.screen, found.end) };
            }
            if (single and count == 0) status = "no_value";
            matches = results;
        } else if (std.mem.eql(u8, op.action, "search_next") or std.mem.eql(u8, op.action, "search_prev")) {
            const selected = if (self.search) |*search| try search.select(
                terminal,
                if (std.mem.eql(u8, op.action, "search_next")) .next else .prev,
                if (op.scroll orelse true) .if_needed else .none,
            ) else false;
            if (!selected) status = "no_value";
        } else return error.UnsupportedGridAction;

        for ([_][]const u8{ "search_status", "search_selected", "search_tick", "search_run", "search_matches", "search_match", "search_next", "search_prev" }) |action| {
            if (!std.mem.eql(u8, action, op.action)) continue;
            const searcher = if (self.search) |*search| search.activeScreenSearch() else null;
            const selected = if (searcher) |search| if (search.selectedMatch()) |found| selected: {
                const bounds = found.untracked();
                break :selected Match{ .start = location(search.screen, bounds.start), .end = location(search.screen, bounds.end) };
            } else null else null;
            search_state = .{
                .status = if (self.search) |*search| @tagName(search.status()) else "complete",
                .tick = search_tick,
                .total = if (searcher) |search| search.matchesLen() else 0,
                .selected_index = if (searcher) |search| if (search.selected) |value| value.idx else null else null,
                .selected_match = selected,
                .screen = if (self.search) |*search| @tagName(search.active_key) else "primary",
            };
            break;
        }

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
        const formatted = if (std.mem.eql(u8, op.action, "format_selection")) formatted: {
            const selection = active.selection orelse break :formatted null;
            const opts: @TypeOf(op.format.?) = op.format orelse .{};
            var formatter: vt.formatter.TerminalFormatter = .init(terminal, .{ .emit = opts.emit, .unwrap = opts.unwrap, .trim = opts.trim });
            formatter.content = .{ .selection = selection };
            var output: std.Io.Writer.Allocating = .init(alloc);
            defer output.deinit();
            try formatter.format(&output.writer);
            break :formatted try hex(alloc, output.written());
        } else null;
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
            .search_needle = search_needle,
            .search_state = search_state,
            .active_screen = @tagName(terminal.screens.active_key),
            .viewport_top = coordinate(active, .screen, active.pages.getTopLeft(.viewport)),
            .selection = selection,
            .selection_result = selection_result,
            .formatted = formatted,
            .tracked = tracked,
            .gesture = if (self.gesture_used) .{
                .click_count = self.gesture.left_click_count,
                .behavior = self.gesture.left_click_behavior,
                .dragged = self.gesture.left_click_dragged,
                .autoscroll = self.gesture.left_drag_autoscroll,
                .anchor_retained = self.gesture.left_click_pin != null,
                .anchor_valid = self.gesture.validatedLeftClickPin(&terminal.screens) != null,
                .anchor = if (self.gesture.validatedLeftClickPin(&terminal.screens)) |pin| location(active, pin.*) else null,
            } else null,
        };
    }
};

fn codepoints(alloc: Allocator, values: ?[]const u32) !?[]const u21 {
    const source = values orelse return null;
    const result = try alloc.alloc(u21, source.len);
    for (source, result) |value, *destination| {
        if (value > 0x10ffff or (value >= 0xd800 and value <= 0xdfff)) return error.InvalidCodepoint;
        destination.* = @intCast(value);
    }
    return result;
}

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
