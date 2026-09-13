//! Live semantic prompt state, independent of snapshot encoding.
const std = @import("std");
const vt = @import("ghostty-vt");
const Allocator = std.mem.Allocator;

pub const State = struct {
    shell_redraw: []const u8,
    primary: Screen,
    alternate: ?Screen,
};
const Click = struct {
    kind: []const u8,
    relative: ?bool = null,
    motion: ?[]const u8 = null,
};
const Screen = struct {
    input_clears_at_eol: bool,
    click: Click,
    rows: []const []const u8,
    history: []const []const u8,
};

pub fn observe(alloc: Allocator, terminal: *vt.Terminal) !State {
    return .{
        .shell_redraw = switch (terminal.flags.shell_redraws_prompt) {
            .true => "all",
            .false => "none",
            .last => "last",
        },
        .primary = try screenState(alloc, terminal.screens.all.get(.primary).?),
        .alternate = if (terminal.screens.all.get(.alternate)) |screen| try screenState(alloc, screen) else null,
    };
}

fn screenState(alloc: Allocator, screen: *vt.Screen) !Screen {
    var rows: std.ArrayList([]const u8) = .empty;
    var iterator = screen.pages.rowIterator(.right_down, .{ .screen = .{} }, null);
    while (iterator.next()) |pin| {
        try rows.append(alloc, switch (pin.rowAndCell().row.semantic_prompt) {
            .none => "none",
            .prompt => "prompt",
            .prompt_continuation => "continuation",
        });
    }
    const history_count = rows.items.len - screen.pages.rows;
    return .{
        .input_clears_at_eol = screen.cursor.semantic_content_clear_eol,
        .click = switch (screen.semantic_prompt.click) {
            .none => .{ .kind = "none" },
            .click_events => |value| .{ .kind = "events", .relative = value == .relative },
            .cl => |value| .{ .kind = "cursor_keys", .motion = @tagName(value) },
        },
        .rows = rows.items[history_count..],
        .history = rows.items[0..history_count],
    };
}
