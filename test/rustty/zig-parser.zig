//! Raw UTF-8/ANSI parser observations. OSC capture is intentionally before
//! command validation; terminal-effect tests must cover OSC semantics separately.
const std = @import("std");
const vt = @import("ghostty-vt");
const Allocator = std.mem.Allocator;
const Decoder = @FieldType(vt.TerminalStream, "utf8decoder");
const max_osc = 8 * 1024 * 1024;

pub const Event = struct {
    kind: []const u8,
    codepoint: ?u32 = null,
    byte: ?u8 = null,
    intermediates: []const u8 = "",
    params: []const u16 = &.{},
    colon_separators: u32 = 0,
    data: []const u8 = "",
    bell: bool = false,
};
pub const State = struct { state: []const u8, ground: bool, events: usize };
pub const Result = struct { events: []Event, states: []State };

pub fn run(alloc: Allocator, operations: anytype) !Result {
    var parser: vt.Parser = .init();
    parser.osc_parser.alloc = alloc;
    defer parser.deinit();
    var decoder: Decoder = .{};
    var events: std.ArrayList(Event) = .empty;
    var states: std.ArrayList(State) = .empty;
    var osc: std.ArrayList(u8) = .empty;
    var overflow = false;
    for (operations) |op| {
        if (std.mem.eql(u8, op.op, "reset")) {
            parser.deinit();
            parser = .init();
            parser.osc_parser.alloc = alloc;
            decoder = .{};
            osc.clearRetainingCapacity();
            overflow = false;
        } else if (std.mem.eql(u8, op.op, "observe")) {
            try states.append(alloc, observe(&parser, decoder, events.items.len));
        } else if (std.mem.eql(u8, op.op, "write")) {
            for (try unhex(alloc, op.data)) |byte| {
                if (parser.state == .ground) {
                    const decoded = decoder.next(byte);
                    if (decoded[0]) |cp| try codepoint(alloc, &events, &parser, cp);
                    if (!decoded[1]) {
                        const retry = decoder.next(byte);
                        std.debug.assert(retry[1]);
                        if (retry[0]) |cp| try codepoint(alloc, &events, &parser, cp);
                    }
                    continue;
                }
                const transition = vt.parse_table.table[byte][@intFromEnum(parser.state)];
                if (parser.state == .osc_string and transition.state != .osc_string) {
                    try events.append(alloc, if (overflow) .{ .kind = "osc_overflow" } else .{
                        .kind = "osc",
                        .data = try hex(alloc, osc.items),
                        .bell = byte == 7,
                    });
                }
                if (transition.action == .osc_put and !overflow) {
                    if (osc.items.len < max_osc) try osc.append(alloc, byte) else {
                        osc.clearRetainingCapacity();
                        overflow = true;
                    }
                }
                const old_state = parser.state;
                for (parser.next(byte)) |maybe_action| {
                    const action = maybe_action orelse continue;
                    const event: Event = switch (action) {
                        .print => |cp| .{ .kind = "print", .codepoint = cp },
                        .execute => |b| .{ .kind = "execute", .byte = b },
                        .esc_dispatch => |v| .{ .kind = "esc", .intermediates = try hex(alloc, v.intermediates), .byte = v.final },
                        .csi_dispatch => |v| .{ .kind = "csi", .intermediates = try hex(alloc, v.intermediates), .params = try alloc.dupe(u16, v.params), .colon_separators = v.params_sep.mask, .byte = v.final },
                        .dcs_hook => |v| .{ .kind = "dcs_hook", .intermediates = try hex(alloc, v.intermediates), .params = try alloc.dupe(u16, v.params), .byte = v.final },
                        .dcs_put => |b| .{ .kind = "dcs_put", .byte = b },
                        .dcs_unhook => .{ .kind = "dcs_unhook" },
                        .apc_start => .{ .kind = "apc_start" },
                        .apc_put => |b| .{ .kind = "apc_put", .byte = b },
                        .apc_end => .{ .kind = "apc_end" },
                        // Validated OSC commands are a different interface.
                        .osc_dispatch => continue,
                    };
                    try events.append(alloc, event);
                }
                if (old_state != .osc_string and parser.state == .osc_string) {
                    osc.clearRetainingCapacity();
                    overflow = false;
                }
            }
        } else return error.UnsupportedOperation;
    }
    try states.append(alloc, observe(&parser, decoder, events.items.len));
    return .{ .events = events.items, .states = states.items };
}

fn observe(parser: *const vt.Parser, decoder: Decoder, count: usize) State {
    return .{ .state = @tagName(parser.state), .ground = parser.state == .ground and decoder.state == 0, .events = count };
}

fn codepoint(alloc: Allocator, events: *std.ArrayList(Event), parser: *vt.Parser, cp: u21) !void {
    switch (cp) {
        0x1b => {
            _ = parser.next(0x1b);
        },
        0...0x1a, 0x1c...0x1f => try events.append(alloc, .{ .kind = "execute", .byte = @intCast(cp) }),
        0x80...0x9f => {},
        else => try events.append(alloc, .{ .kind = "print", .codepoint = cp }),
    }
}
fn unhex(alloc: Allocator, input: []const u8) ![]const u8 {
    if (input.len % 2 != 0) return error.InvalidHex;
    const result = try alloc.alloc(u8, input.len / 2);
    return std.fmt.hexToBytes(result, input) catch return error.InvalidHex;
}
fn hex(alloc: Allocator, input: []const u8) ![]const u8 {
    const result = try alloc.alloc(u8, input.len * 2);
    const digits = "0123456789abcdef";
    for (input, 0..) |byte, i| {
        result[i * 2] = digits[byte >> 4];
        result[i * 2 + 1] = digits[byte & 15];
    }
    return result;
}
