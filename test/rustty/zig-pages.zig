//! Read native page allocation metadata without restoring compressed pages.
const std = @import("std");
const vt = @import("ghostty-vt");

pub const Page = struct {
    columns: u16,
    rows: u16,
    capacity: vt.page.Capacity,
    pooled: bool,
    allocation_bytes: usize,
};

pub const Screen = struct {
    allocation_bytes: usize,
    total_rows: usize,
    pages: []const Page,
};

pub const State = struct {
    primary: Screen,
    alternate: ?Screen,
};

pub fn observe(alloc: std.mem.Allocator, terminal: *const vt.Terminal) !State {
    return .{
        .primary = try screen(alloc, terminal.screens.get(.primary).?),
        .alternate = if (terminal.screens.get(.alternate)) |value| try screen(alloc, value) else null,
    };
}

fn screen(alloc: std.mem.Allocator, value: *const vt.Screen) !Screen {
    var pages: std.ArrayList(Page) = .empty;
    var next = value.pages.pages.first;
    while (next) |node| : (next = node.next) {
        const bytes = if (node.owned == .pool) vt.Page.layout(vt.page.std_capacity).total_size else switch (node.data) {
            .resident => |page| page.memory.len,
            .compressed => |page| page.page.memory.len,
        };
        try pages.append(alloc, .{
            .columns = node.cols(),
            .rows = node.rows(),
            .capacity = node.capacity(),
            .pooled = node.owned == .pool,
            .allocation_bytes = bytes,
        });
    }
    return .{
        .allocation_bytes = value.pages.page_size,
        .total_rows = value.pages.total_rows,
        .pages = pages.items,
    };
}
