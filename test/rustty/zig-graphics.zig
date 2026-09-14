//! Native image decode and stored pixels, kept separate from renderer validation.
const std = @import("std");
const vt = @import("ghostty-vt");
const wuffs = @import("wuffs");
const Allocator = std.mem.Allocator;

pub const Image = struct {
    id: u32,
    number: u32,
    width: u32,
    height: u32,
    pixels: []const u8,
    displayed_pixels: []const u8,
    frames: []const struct { pixels: []const u8, gap_ms: u32 },
    current_frame: u32,
    root_gap_ms: u32,
    animation_state: u8,
    max_loops: u32,
    completed_loops: u32,
    frame_shown_at_ms: ?u64,
};
pub const State = struct { primary: []Image, alternate: ?[]Image };
pub const Tick = struct { next_delay_ms: ?u64, changed: bool };

pub fn tick(t: *vt.Terminal, io: std.Io, now_ms: u64) Tick {
    const storage = &t.screens.active.kitty_images;
    const before = storage.generation;
    const delay = storage.animationTick(io, now_ms);
    return .{ .next_delay_ms = delay, .changed = storage.generation != before };
}

const PlacementId = struct { internal: bool, id: u32 };
const Location = struct { screen: ?[2]u32, active: ?[2]u32, viewport: ?[2]u32 };
const Placement = struct {
    image_id: u32,
    placement_id: PlacementId,
    location: []const u8,
    anchor: ?Location,
    parent: ?struct { image_id: u32, placement_id: PlacementId, offset: [2]i32 },
    chain: ?struct { image_id: u32, placement_id: PlacementId, anchor: ?Location, offset: [2]i32 },
    requested_size: [2]u32,
    requested_source: [4]u32,
    stored_offset: [2]u32,
    z: i32,
    source: [4]u32,
    offset: [2]u32,
    pixels: [2]u32,
    grid: [2]u32,
    rect: ?struct { start: ?Location, end: ?Location },
};
const PlaceholderGeometry = struct { offset: [2]u32, source: [4]u32, pixels: [2]u32 };
const Placeholder = struct {
    image_id: u32,
    placement_id: u32,
    anchor: ?Location,
    fragment: [2]u32,
    size: [2]u32,
    target: ?PlacementId,
    geometry: ?PlaceholderGeometry,
    geometry_error: ?[]const u8,
};
pub const Placements = struct {
    primary: []Placement,
    alternate: ?[]Placement,
    primary_placeholders: []Placeholder,
    alternate_placeholders: ?[]Placeholder,
};

pub fn observePlacements(alloc: Allocator, t: *vt.Terminal) !Placements {
    return .{
        .primary = try placements(alloc, t, t.screens.all.get(.primary).?),
        .alternate = if (t.screens.all.get(.alternate)) |alt| try placements(alloc, t, alt) else null,
        .primary_placeholders = try placeholders(alloc, t, t.screens.all.get(.primary).?),
        .alternate_placeholders = if (t.screens.all.get(.alternate)) |alt| try placeholders(alloc, t, alt) else null,
    };
}

fn placeholders(alloc: Allocator, t: *vt.Terminal, owner: *vt.Screen) ![]Placeholder {
    var result: std.ArrayList(Placeholder) = .empty;
    var it = vt.kitty.graphics.unicode.placementIterator(owner.pages.getTopLeft(.viewport), owner.pages.getBottomRight(.viewport));
    while (it.next()) |p| {
        const target = owner.kitty_images.placeholderTarget(p.image_id, p.placement_id);
        var geometry_error: ?[]const u8 = null;
        const geometry: ?PlaceholderGeometry = geometry: {
            const image = owner.kitty_images.imageById(p.image_id) orelse {
                geometry_error = "MissingImage";
                break :geometry null;
            };
            const rendered = p.renderPlacement(&owner.kitty_images, &image, t.width_px / t.cols, t.height_px / t.rows) catch |err| {
                geometry_error = @errorName(err);
                break :geometry null;
            };
            break :geometry .{
                .offset = .{ rendered.offset_x, rendered.offset_y },
                .source = .{ rendered.source_x, rendered.source_y, rendered.source_width, rendered.source_height },
                .pixels = .{ rendered.dest_width, rendered.dest_height },
            };
        };
        try result.append(alloc, .{
            .image_id = p.image_id,
            .placement_id = p.placement_id,
            .anchor = location(owner, p.pin),
            .fragment = .{ p.col, p.row },
            .size = .{ p.width, p.height },
            .target = if (target) |v| .{ .internal = v.key.placement_id.tag == .internal, .id = v.key.placement_id.id } else null,
            .geometry = geometry,
            .geometry_error = geometry_error,
        });
    }
    return result.toOwnedSlice(alloc);
}

fn location(owner: *vt.Screen, pin: vt.Pin) ?Location {
    if (pin.garbage) return null;
    var result: Location = undefined;
    inline for (.{ .screen, .active, .viewport }) |tag| {
        const point = owner.pages.pointFromPin(tag, pin);
        @field(result, @tagName(tag)) = if (point) |p| .{ p.coord().x, p.coord().y } else null;
    }
    return result;
}

fn placements(alloc: Allocator, t: *vt.Terminal, owner: *vt.Screen) ![]Placement {
    const result = try alloc.alloc(Placement, owner.kitty_images.placements.count());
    var it = owner.kitty_images.placements.iterator();
    var i: usize = 0;
    while (it.next()) |entry| : (i += 1) {
        const key = entry.key_ptr.*;
        const p = entry.value_ptr.*;
        const img = owner.kitty_images.imageById(key.image_id) orelse return error.MissingPlacementImage;
        const source = p.sourceRect(img);
        const offset = p.cellOffset(t);
        const pixels = p.pixelSize(img, t);
        const grid = p.gridSize(img, t);
        const rect = p.rect(img, t);
        result[i] = .{
            .image_id = key.image_id,
            .placement_id = .{ .internal = key.placement_id.tag == .internal, .id = key.placement_id.id },
            .location = @tagName(p.location),
            .anchor = switch (p.location) {
                .pin => |pin| location(owner, pin.*),
                else => null,
            },
            .parent = switch (p.location) {
                .relative => |r| .{ .image_id = r.parent.image_id, .placement_id = .{ .internal = r.parent.placement_id.tag == .internal, .id = r.parent.placement_id.id }, .offset = .{ r.horizontal_offset, r.vertical_offset } },
                else => null,
            },
            .chain = chain: {
                const rel = switch (p.location) {
                    .relative => |rel| rel,
                    else => break :chain null,
                };
                const resolved = owner.kitty_images.resolveChain(rel) orelse break :chain null;
                break :chain .{
                    .image_id = resolved.root_key.image_id,
                    .placement_id = .{ .internal = resolved.root_key.placement_id.tag == .internal, .id = resolved.root_key.placement_id.id },
                    .anchor = switch (resolved.root.location) {
                        .pin => |pin| location(owner, pin.*),
                        else => null,
                    },
                    .offset = .{ resolved.horizontal_offset, resolved.vertical_offset },
                };
            },
            .requested_size = .{ p.columns, p.rows },
            .requested_source = .{ p.source_x, p.source_y, p.source_width, p.source_height },
            .stored_offset = .{ p.x_offset, p.y_offset },
            .z = p.z,
            .source = .{ source.x, source.y, source.width, source.height },
            .offset = .{ offset.x, offset.y },
            .pixels = .{ pixels.width, pixels.height },
            .grid = .{ grid.cols, grid.rows },
            .rect = if (rect) |r| .{ .start = location(owner, r.top_left), .end = location(owner, r.bottom_right) } else null,
        };
    }
    std.mem.sort(Placement, result, {}, struct {
        fn less(_: void, a: Placement, b: Placement) bool {
            if (a.image_id != b.image_id) return a.image_id < b.image_id;
            if (a.placement_id.internal != b.placement_id.internal) return a.placement_id.internal;
            return a.placement_id.id < b.placement_id.id;
        }
    }.less);
    return result;
}

pub fn install() void {
    vt.sys.decode_png = decode;
}

fn decode(alloc: Allocator, data: []const u8) vt.sys.DecodeError!vt.sys.Image {
    const image = wuffs.png.decode(alloc, data) catch |err| switch (err) {
        error.OutOfMemory => return error.OutOfMemory,
        error.WuffsError, error.Overflow => return error.InvalidData,
    };
    return .{ .width = image.width, .height = image.height, .data = image.data };
}

pub fn observe(alloc: Allocator, terminal: *vt.Terminal) !State {
    return .{
        .primary = try screen(alloc, terminal.screens.all.get(.primary).?),
        .alternate = if (terminal.screens.all.get(.alternate)) |alternate| try screen(alloc, alternate) else null,
    };
}

fn screen(alloc: Allocator, value: *vt.Screen) ![]Image {
    const images = try alloc.alloc(Image, value.kitty_images.images.count());
    var it = value.kitty_images.images.valueIterator();
    var i: usize = 0;
    while (it.next()) |image| : (i += 1) {
        const data = switch (image.data) {
            .complete => |bytes| bytes,
            .pending => return error.UnsupportedPendingImage,
        };
        // Rust's render storage is RGBA. Convert native RGB only at this
        // observation boundary; protocol replies and dimensions remain exact.
        const rgba = if (image.format == .rgb) pixels: {
            if (data.len % 3 != 0) return error.InvalidImagePixels;
            const bytes = try alloc.alloc(u8, data.len / 3 * 4);
            for (0..data.len / 3) |pixel| {
                @memcpy(bytes[pixel * 4 ..][0..3], data[pixel * 3 ..][0..3]);
                bytes[pixel * 4 + 3] = 255;
            }
            break :pixels bytes;
        } else if (image.format == .rgba) data else return error.UnsupportedImageFormat;
        // An image without animation state has an implicit stopped root frame.
        const anim: vt.kitty.graphics.Animation = if (image.animation) |a| a.* else .{};
        const frames = try alloc.alloc(std.meta.Elem(@FieldType(Image, "frames")), anim.frames.items.len);
        for (frames, anim.frames.items) |*dst, src| dst.* = .{ .pixels = try hex(alloc, src.data), .gap_ms = src.gap_ms };
        images[i] = .{
            .id = image.id,
            .number = image.number,
            .width = image.width,
            .height = image.height,
            .pixels = try hex(alloc, rgba),
            .displayed_pixels = try hex(alloc, if (anim.current_index == 0) rgba else image.renderData().bytes().?),
            .frames = frames,
            .current_frame = anim.current_index,
            .root_gap_ms = anim.root_gap_ms,
            .animation_state = switch (anim.state) {
                .stopped => 1,
                .loading => 2,
                .running => 3,
            },
            .max_loops = anim.max_loops,
            .completed_loops = anim.current_loop,
            .frame_shown_at_ms = anim.frame_shown_at_ms,
        };
    }
    std.mem.sort(Image, images, {}, struct {
        fn less(_: void, a: Image, b: Image) bool {
            return a.id < b.id;
        }
    }.less);
    return images;
}

fn hex(alloc: Allocator, bytes: []const u8) ![]const u8 {
    const result = try alloc.alloc(u8, bytes.len * 2);
    for (bytes, 0..) |byte, index| result[index * 2 ..][0..2].* = std.fmt.bytesToHex([_]u8{byte}, .lower);
    return result;
}
