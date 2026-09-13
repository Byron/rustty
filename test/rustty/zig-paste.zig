const std = @import("std");
const vt = @import("ghostty-vt");
const Allocator = std.mem.Allocator;
const Emit = *const fn ([]const u8, []const u8) void;
const Content = struct { mime: []const u8, data: []const u8 };
pub const Options = struct {
    source: []const u8 = "standard",
    contents: []const Content = &.{},
    reader: bool = false,
    read_error: bool = false,
    allow_unsafe: bool = false,
    entropy: []const u8 = "00",
    entropy_error: bool = false,
};

var active: ?*Context = null;
const Context = struct {
    options: Options,
    emit: Emit,
    contents: []const vt.clipboard.Content,
    entropy: []const u8,
    entropy_offset: usize = 0,

    fn random(buffer: []u8) vt.sys.RandomSecureError!void {
        const self = active.?;
        var size: [32]u8 = undefined;
        self.emit("paste_entropy", std.fmt.bufPrint(&size, "{d}", .{buffer.len}) catch unreachable);
        if (self.options.entropy_error) return error.EntropyUnavailable;
        for (buffer) |*byte| {
            byte.* = self.entropy[self.entropy_offset % self.entropy.len];
            self.entropy_offset += 1;
        }
    }
    fn read(ctx: ?*anyopaque, mime: []const u8, sink: *std.Io.Writer) vt.clipboard.MimeReader.Error!void {
        const self: *Context = @ptrCast(@alignCast(ctx.?));
        self.emit("paste_read", mime);
        for (self.contents) |content| {
            if (std.mem.eql(u8, content.mime, mime)) {
                try sink.writeAll(content.data);
                if (self.options.read_error) return error.ReadFailed;
                return;
            }
        }
        return error.ReadFailed;
    }
};

pub fn run(alloc: Allocator, handler: *vt.TerminalStream.Handler, options: Options, emit: Emit) !void {
    const source: vt.PasteSource = if (std.mem.eql(u8, options.source, "text")) .text else .{ .clipboard = std.meta.stringToEnum(vt.clipboard.Location, options.source) orelse return error.InvalidPasteSource };
    const contents = try alloc.alloc(vt.clipboard.Content, options.contents.len);
    const mimes = try alloc.alloc([]const u8, contents.len);
    for (options.contents, contents, mimes) |original, *content, *mime| {
        content.* = .{ .mime = try decode(alloc, original.mime), .data = try decode(alloc, original.data) };
        mime.* = content.mime;
    }
    const entropy = try decode(alloc, options.entropy);
    if (entropy.len == 0) return error.InvalidTestEntropy;
    if (!options.entropy_error) {
        for (entropy) |byte| {
            if (byte < 224) break;
        } else return error.InvalidTestEntropy;
    }
    var context: Context = .{ .options = options, .emit = emit, .contents = contents, .entropy = entropy };
    active = &context;
    defer active = null;
    const previous = vt.sys.random_secure;
    vt.sys.random_secure = Context.random;
    defer vt.sys.random_secure = previous;
    const result = handler.paste(.{
        .source = source,
        .allow_unsafe = options.allow_unsafe,
        .contents = if (options.reader) .{ .reader = .{ .mimes = mimes, .read = .{ .ctx = &context, .read_fn = Context.read } } } else .{ .memory = contents },
    }) catch |err| {
        emit("paste_result", @errorName(err));
        return;
    };
    emit("paste_result", if (result) "true" else "false");
}

fn decode(alloc: Allocator, hex: []const u8) ![]const u8 {
    if (hex.len % 2 != 0) return error.InvalidHex;
    const bytes = try alloc.alloc(u8, hex.len / 2);
    return std.fmt.hexToBytes(bytes, hex) catch error.InvalidHex;
}
