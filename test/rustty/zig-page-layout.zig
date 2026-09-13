//! Native allocation arithmetic; never allocate the requested backing page.
const std = @import("std");
const vt = @import("ghostty-vt");

pub const Request = struct {
    action: []const u8 = "initial",
    columns: u16 = 80,
    capacity: ?vt.page.Capacity = null,
    exact: bool = false,
};

const StyleSet = @FieldType(vt.Page, "styles");
const HyperlinkSet = @FieldType(vt.Page, "hyperlink_set");
const page_alignment = std.heap.page_size_min;
const cell_alignment = @max(@alignOf(vt.page.Cell), @min(std.atomic.cache_line, page_alignment));

pub const Constants = struct {
    page_alignment: usize = page_alignment,
    cell_alignment: usize = cell_alignment,
    row_size: usize = @sizeOf(vt.page.Row),
    cell_size: usize = @sizeOf(vt.page.Cell),
    style_item_size: usize = @sizeOf(StyleSet.Item),
    style_item_alignment: usize = @alignOf(StyleSet.Item),
    style_set_alignment: usize = StyleSet.base_align.toByteUnits(),
    hyperlink_item_size: usize = @sizeOf(HyperlinkSet.Item),
    hyperlink_item_alignment: usize = @alignOf(HyperlinkSet.Item),
    hyperlink_set_alignment: usize = HyperlinkSet.base_align.toByteUnits(),
    metadata_alignment: usize = vt.Page.MetaLayout.alignment,
    standard_capacity: vt.page.Capacity = vt.page.std_capacity,
    standard_bytes: usize = vt.Page.layout(vt.page.std_capacity).total_size,
};

pub const Result = struct {
    constants: Constants = .{},
    layout: vt.Page.Layout,
    metadata: vt.Page.MetaLayout,
    allocation_bytes: usize,
    pooled: bool,
};

pub fn run(request: Request) !Result {
    if (!std.mem.eql(u8, request.action, "initial") and
        !std.mem.eql(u8, request.action, "adjust") and
        !std.mem.eql(u8, request.action, "layout")) return error.InvalidLayoutAction;
    var capacity: vt.page.Capacity = request.capacity orelse vt.page.std_capacity;
    if (capacity.cols == 0 or capacity.rows == 0) return error.InvalidDimensions;
    var layout = vt.Page.layout(capacity);
    if (layout.total_size > std.math.maxInt(u32)) return error.PageTooLarge;
    if (std.mem.eql(u8, request.action, "initial")) {
        if (request.columns == 0) return error.InvalidDimensions;
        // PageList.initialCapacity is private; use the native adjustment and
        // its documented fallback with the unchanged standard row capacity.
        capacity = vt.page.std_capacity.adjust(.{ .cols = request.columns }) catch blk: {
            var fallback = vt.page.std_capacity;
            fallback.cols = request.columns;
            break :blk fallback;
        };
    } else if (std.mem.eql(u8, request.action, "adjust")) {
        if (request.columns == 0) return error.InvalidDimensions;
        const available = layout.total_size - vt.Page.MetaLayout.init(capacity).total_size;
        const row_bytes = @sizeOf(vt.page.Row) + @as(usize, request.columns) * @sizeOf(vt.page.Cell);
        // Native adjust uses a checked u16 cast. Report its invalid input
        // domain without crashing the long-running comparison process.
        if (available / row_bytes > std.math.maxInt(u16)) return error.RowCountOverflow;
        capacity = try capacity.adjust(.{ .cols = request.columns });
    } else if (!std.mem.eql(u8, request.action, "layout")) return error.InvalidLayoutAction;

    layout = vt.Page.layout(capacity);
    if (layout.total_size > std.math.maxInt(u32)) return error.PageTooLarge;
    const pool_bytes = vt.Page.layout(vt.page.std_capacity).total_size;
    const pooled = !request.exact and layout.total_size <= pool_bytes;
    return .{
        .layout = layout,
        .metadata = vt.Page.MetaLayout.init(capacity),
        .allocation_bytes = if (pooled) pool_bytes else layout.total_size,
        .pooled = pooled,
    };
}
