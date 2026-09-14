//! Replay the direct OSC fuzzer's selector and payload without stream framing.
const std = @import("std");
const vt = @import("ghostty-vt");

pub fn run(alloc: std.mem.Allocator, stream: *vt.TerminalStream, input: []const u8, scalar: bool) void {
    if (input.len == 0) return;
    var parser: vt.osc.Parser = .init(alloc);
    defer parser.deinit();
    if (scalar) {
        for (input[1..]) |byte| parser.next(byte);
    } else parser.nextSlice(input[1..]);
    const terminator: ?u8 = switch (input[0] % 3) {
        0 => 0x07,
        1 => 0x9c,
        else => null,
    };
    if (parser.end(terminator)) |command| stream.oscDispatch(command.*);
}
