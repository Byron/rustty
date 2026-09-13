use rustty_vt::glyph::Glyphs;
use serde_json::{Value, json};

pub fn observe(glyphs: &Glyphs) -> Value {
    // Exact IEEE bits preserve precision and signed zero independently of
    // the JSON writer's choice of integer or decimal number spelling.
    let entries: Vec<_> = glyphs
        .entries()
        .iter()
        .map(|entry| {
            json!({
                "codepoint": entry.codepoint,
                "outline": entry.outline,
                "design": entry.design,
                "width": entry.width,
                "constraint": {
                    "size": entry.constraint.size,
                    "align_horizontal": entry.constraint.align_horizontal,
                    "align_vertical": entry.constraint.align_vertical,
                    "pad_top_bits": entry.constraint.pad_top.to_bits(),
                    "pad_right_bits": entry.constraint.pad_right.to_bits(),
                    "pad_bottom_bits": entry.constraint.pad_bottom.to_bits(),
                    "pad_left_bits": entry.constraint.pad_left.to_bits(),
                },
            })
        })
        .collect();
    json!({
        "enabled": glyphs.enabled(),
        "apc_limit": glyphs.apc_limit(),
        "dirty": glyphs.is_dirty(),
        "entries": entries,
    })
}
