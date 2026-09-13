//! Read stored native outlines without sorting or replacing entry order.
const std = @import("std");
const vt = @import("ghostty-vt");
const NativeEntry = vt.apc.glyph.Glossary.Entry;
const Outline = @FieldType(NativeEntry.Glyph, "glyf");
const Constraint = struct {
    size: []const u8,
    align_horizontal: []const u8,
    align_vertical: []const u8,
    pad_top_bits: u64,
    pad_right_bits: u64,
    pad_bottom_bits: u64,
    pad_left_bits: u64,
};
const Entry = struct {
    codepoint: u32,
    outline: Outline,
    design: @FieldType(NativeEntry, "design"),
    width: u8,
    constraint: Constraint,
};
pub const State = struct {
    enabled: bool,
    apc_limit: usize,
    dirty: bool,
    entries: []const Entry,
};

pub fn observe(alloc: std.mem.Allocator, stream: *vt.TerminalStream) !State {
    const terminal = stream.handler.terminal;
    const glossary = terminal.glyph_glossary;
    const entries = try alloc.alloc(Entry, glossary.entries.count());
    for (glossary.entries.keys(), glossary.entries.values(), entries) |cp, entry, *out| {
        const outline = entry.glyph.glyf;
        out.* = .{
            .codepoint = cp,
            .outline = .{
                .contours = try alloc.dupe(u16, outline.contours),
                .points = try alloc.dupe(Outline.Point, outline.points),
            },
            .design = entry.design,
            .width = @intFromEnum(entry.width),
            .constraint = .{
                .size = @tagName(entry.constraint.size),
                .align_horizontal = @tagName(entry.constraint.align_horizontal),
                .align_vertical = @tagName(entry.constraint.align_vertical),
                .pad_top_bits = @bitCast(entry.constraint.pad_top),
                .pad_right_bits = @bitCast(entry.constraint.pad_right),
                .pad_bottom_bits = @bitCast(entry.constraint.pad_bottom),
                .pad_left_bits = @bitCast(entry.constraint.pad_left),
            },
        };
    }
    return .{
        .enabled = stream.handler.apc_handler.enabled.contains(.glyph),
        .apc_limit = stream.handler.apc_handler.max_bytes.get(.glyph) orelse vt.apc.Protocol.defaultMaxBytes(.glyph),
        .dirty = terminal.flags.dirty.glyph_glossary,
        .entries = entries,
    };
}
