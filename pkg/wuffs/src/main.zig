const builtin = @import("builtin");
const std = @import("std");

pub const png = @import("png.zig");
pub const jpeg = @import("jpeg.zig");
pub const swizzle = @import("swizzle.zig");
pub const zlib = @import("zlib.zig");
pub const Error = @import("error.zig").Error;

/// The maximum image size, based on the 4G limit of Ghostty's
/// `image-storage-limit` config.
pub const maximum_image_size = 4 * 1024 * 1024 * 1024;

pub const ImageData = struct {
    width: u32,
    height: u32,
    data: []u8,
};

// Wuffs' generated `wuffs_foo__bar__alloc()` convenience functions are the
// only code that references libc's calloc/free. We never call them
// and linker garbage collection strips them, but that isn't guaranteed.
// When libc isn't linked there would be nothing to provide calloc/free if
// they survive, so export stubs to satisfy the link. Weak so that any real
// definition wins. Hidden keeps them out of the export table where the
// format honors it (e.g. wasm).
comptime {
    if (!builtin.link_libc) {
        @export(&callocStub, .{
            .name = "calloc",
            .linkage = .weak,
            .visibility = .hidden,
        });
        @export(&freeStub, .{
            .name = "free",
            .linkage = .weak,
            .visibility = .hidden,
        });
    }
}

fn callocStub(count: usize, size: usize) callconv(.c) ?*anyopaque {
    _ = count;
    _ = size;
    return null;
}

fn freeStub(ptr: ?*anyopaque) callconv(.c) void {
    _ = ptr;
}

test {
    refAllDeclsRecursive(@This());
}

test "decoders after an unaligned byte allocation" {
    const alloc = std.testing.allocator;
    const backing = try alloc.alignedAlloc(u8, .of(u64), 1024 * 1024);
    defer alloc.free(backing);

    inline for (.{ png, jpeg }, .{ "1x1#000000.png", "1x1#000000.jpg" }) |decoder, fixture| {
        var fixed = std.heap.FixedBufferAllocator.init(backing);
        const byte_alloc = fixed.allocator();
        _ = try byte_alloc.alloc(u8, 1);
        const image = try decoder.decode(byte_alloc, @embedFile(fixture));
        defer byte_alloc.free(image.data);
        try std.testing.expectEqual(1, image.width);
        try std.testing.expectEqual(1, image.height);
        try std.testing.expectEqualSlices(u8, &.{ 0, 0, 0, 255 }, image.data);
    }
}

/// Copied from 0.15.2 stdlib (MIT license).
fn refAllDeclsRecursive(comptime T: type) void {
    if (!builtin.is_test) return;
    inline for (comptime std.meta.declarations(T)) |decl| {
        if (@TypeOf(@field(T, decl.name)) == type) {
            switch (@typeInfo(@field(T, decl.name))) {
                .@"struct", .@"enum", .@"union", .@"opaque" => refAllDeclsRecursive(@field(T, decl.name)),
                else => {},
            }
        }
        _ = &@field(T, decl.name);
    }
}
