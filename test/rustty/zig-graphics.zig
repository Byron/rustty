//! Native image decode and stored pixels, kept separate from renderer validation.
const std = @import("std");
const vt = @import("ghostty-vt");
const wuffs = @import("wuffs");
const Allocator = std.mem.Allocator;

pub const Image = struct { id: u32, number: u32, width: u32, height: u32, pixels: []const u8 };
pub const State = struct { primary: []Image, alternate: ?[]Image };

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
        const hex = try alloc.alloc(u8, rgba.len * 2);
        const alphabet = "0123456789abcdef";
        for (rgba, 0..) |byte, index| {
            hex[index * 2] = alphabet[byte >> 4];
            hex[index * 2 + 1] = alphabet[byte & 15];
        }
        images[i] = .{ .id = image.id, .number = image.number, .width = image.width, .height = image.height, .pixels = hex };
    }
    std.mem.sort(Image, images, {}, struct {
        fn less(_: void, a: Image, b: Image) bool {
            return a.id < b.id;
        }
    }.less);
    return images;
}
