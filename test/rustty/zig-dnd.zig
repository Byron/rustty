//! Native OSC 72 host actions and retained state, without OS drag sessions.
const std = @import("std");
const vt = @import("ghostty-vt");
const dnd = vt.kitty.dnd;
const Allocator = std.mem.Allocator;

const Item = struct { mime: []const u8, data: []const u8 };
pub const Operation = struct {
    action: []const u8 = "observe",
    cell_x: u32 = 0,
    cell_y: u32 = 0,
    pixel_x: i32 = 0,
    pixel_y: i32 = 0,
    operations: u8 = 1,
    mimes: []const []const u8 = &.{},
    items: []const Item = &.{},
};

pub const State = struct {
    chunking: dnd.Chunking,
    client_id: u32,
    registered_mimes: []const u8,
    hovered: bool,
    dropped: bool,
    accepted: ?dnd.Operation,
    client_accepted: ?dnd.Operation,
    accept_in_progress: bool,
    accepted_mimes: []const u8,
    offered: ?[]const []const u8,
    items: ?[]const Item,
};

pub fn run(alloc: Allocator, t: *vt.Terminal, writer: *std.Io.Writer, op: Operation) !?State {
    const action = std.meta.stringToEnum(enum { observe, move, leave, drop }, op.action) orelse
        return error.UnsupportedDndOperation;
    if (op.operations > 3) return error.InvalidDndOperations;
    if (t.kitty_dnd) |state| {
        const ev: dnd.State.MoveEvent = .{
            .cell_x = op.cell_x,
            .cell_y = op.cell_y,
            .pixel_x = op.pixel_x,
            .pixel_y = op.pixel_y,
            .operations = @bitCast(@as(u2, @intCast(op.operations))),
        };
        switch (action) {
            .observe => {},
            .leave => try state.dragLeave(alloc, writer),
            .move => {
                const mimes = try alloc.alloc([]const u8, op.mimes.len);
                for (op.mimes, mimes) |source, *destination| destination.* = try decode(alloc, source);
                try state.dragMove(alloc, writer, ev, mimes);
            },
            .drop => {
                const items = try alloc.alloc(dnd.Item, op.items.len);
                for (op.items, items) |source, *destination| destination.* = .{
                    .mime = try decode(alloc, source.mime),
                    .data = try decode(alloc, source.data),
                };
                try state.dragDrop(alloc, writer, ev, items);
            },
        }
    }
    return observe(alloc, t);
}

pub fn observe(alloc: Allocator, t: *const vt.Terminal) !?State {
    const state = t.kitty_dnd orelse return null;
    const drop = state.drop;
    const offered: ?[]const []const u8 = if (drop.offered) |value| result: {
        const mimes = try alloc.alloc([]const u8, value.mimes.len);
        for (value.mimes, mimes) |source, *destination| destination.* = try encode(alloc, source);
        break :result mimes;
    } else null;
    const items: ?[]const Item = if (drop.items) |value| result: {
        const items = try alloc.alloc(Item, value.len);
        for (value, items) |source, *destination| destination.* = .{
            .mime = try encode(alloc, source.mime),
            .data = try encode(alloc, source.data),
        };
        break :result items;
    } else null;
    return .{
        .chunking = state.chunking,
        .client_id = drop.client_id,
        .registered_mimes = try encode(alloc, drop.registered_mimes.items),
        .hovered = drop.hovered,
        .dropped = drop.dropped,
        .accepted = drop.accepted,
        .client_accepted = state.clientAccepted(),
        .accept_in_progress = drop.accept_in_progress,
        .accepted_mimes = try encode(alloc, drop.accepted_mimes.items),
        .offered = offered,
        .items = items,
    };
}

fn decode(alloc: Allocator, text: []const u8) ![]const u8 {
    if (text.len % 2 != 0) return error.InvalidHex;
    const bytes = try alloc.alloc(u8, text.len / 2);
    return std.fmt.hexToBytes(bytes, text) catch error.InvalidHex;
}

fn encode(alloc: Allocator, bytes: []const u8) ![]const u8 {
    return std.fmt.allocPrint(alloc, "{x}", .{bytes});
}
