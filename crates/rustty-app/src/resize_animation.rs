//! Blend endpoint frames without dimming the text they have in common.
use rustty_render::{ComposeError, Frame, Paint, Quad};
use std::collections::HashMap;

type QuadKey = (u8, usize, [u32; 4], [u32; 4], [u32; 4]);

fn key(quad: &Quad) -> QuadKey {
    (
        quad.paint as u8,
        quad.atlas,
        quad.rect.map(f32::to_bits),
        quad.uv.map(f32::to_bits),
        quad.color.0.map(f32::to_bits),
    )
}

fn counts<'a>(quads: impl Iterator<Item = &'a Quad>) -> HashMap<QuadKey, usize> {
    let mut counts = HashMap::new();
    for quad in quads {
        *counts.entry(key(quad)).or_default() += 1;
    }
    counts
}

fn take(counts: &mut HashMap<QuadKey, usize>, quad: &Quad) -> bool {
    let Some(count) = counts.get_mut(&key(quad)).filter(|count| **count > 0) else {
        return false;
    };
    *count -= 1;
    true
}

pub fn mostly_unchanged(source: &Frame, target: &Frame) -> bool {
    let text = |quad: &&Quad| quad.paint != Paint::Solid;
    let mut remaining = counts(source.quads.iter().filter(text));
    let source_count: usize = remaining.values().sum();
    let target_count = target.quads.iter().filter(text).count();
    let matched = target
        .quads
        .iter()
        .filter(text)
        .filter(|quad| take(&mut remaining, quad))
        .count();
    let total = source_count.max(target_count);
    total == 0 || matched > total / 2
}

pub fn blend(source: &Frame, target: &Frame, mix: f32) -> Result<Frame, ComposeError> {
    if mix <= 0.0 {
        return Ok(source.clone());
    }
    if mix >= 1.0 {
        return Ok(target.clone());
    }
    let size = std::array::from_fn(|axis| source.size[axis].max(target.size[axis]));
    let mut frame = Frame::empty(size);
    let clip = |frame: &Frame| [0.0, 0.0, frame.size[0] as f32, frame.size[1] as f32];
    frame.append_clipped(source, [0.0; 2], clip(source))?;
    let split = frame.quads.len();
    frame.append_clipped(target, [0.0; 2], clip(target))?;
    let quads = std::mem::take(&mut frame.quads);
    let (source, target) = quads.split_at(split);
    if let Some(background) = target.first().or_else(|| source.first()) {
        let mut background = background.clone();
        background.rect = [0.0, 0.0, size[0] as f32, size[1] as f32];
        frame.quads.push(background);
    }
    let mut remaining = counts(source.iter().skip(1));
    let matched: Vec<_> = target
        .iter()
        .skip(1)
        .map(|quad| take(&mut remaining, quad))
        .collect();
    // Shared backgrounds must remain below the outgoing text.
    for quad in source.iter().skip(1) {
        let unmatched = take(&mut remaining, quad);
        if unmatched || quad.paint == Paint::Solid {
            let mut quad = quad.clone();
            if unmatched {
                quad.color = quad.color.opacity(1.0 - mix);
            }
            frame.quads.push(quad);
        }
    }
    for (quad, matched) in target.iter().skip(1).zip(matched) {
        if matched && quad.paint == Paint::Solid {
            continue;
        }
        let mut quad = quad.clone();
        if !matched {
            quad.color = quad.color.opacity(mix);
        }
        frame.quads.push(quad);
    }
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustty_render::Color;

    fn endpoint(size: [u32; 2], glyphs: &[f32]) -> Frame {
        let mut frame = Frame::empty(size);
        frame.quads.push(Quad::solid(
            [0.0, 0.0, size[0] as f32, size[1] as f32],
            Color::rgb([0; 3]),
        ));
        for &x in glyphs {
            let mut glyph = Quad::solid([x, 0.0, 1.0, 1.0], Color::rgb([255; 3]));
            glyph.paint = Paint::Mask;
            glyph.uv = [0.0, 0.0, 1.0, 1.0];
            frame.quads.push(glyph);
        }
        frame
    }

    #[test]
    fn unchanged_glyphs_stay_opaque_while_changed_glyphs_cross_fade() {
        let source = endpoint([8, 1], &[0.0, 2.0]);
        let target = endpoint([8, 2], &[0.0, 4.0]);
        for mix in [0.25, 0.5, 0.75] {
            let frame = blend(&source, &target, mix).unwrap();
            assert_eq!(frame.size, [8, 2]);
            assert_eq!(frame.quads.len(), 4);
            assert_eq!(frame.quads[0], target.quads[0]);
            assert_eq!(frame.quads[1].rect, source.quads[2].rect);
            assert_eq!(frame.quads[1].color.0[3], 1.0 - mix);
            assert_eq!(frame.quads[2], target.quads[1]);
            assert_eq!(frame.quads[3].rect, target.quads[2].rect);
            assert_eq!(frame.quads[3].color.0[3], mix);
        }
    }

    #[test]
    fn a_strict_majority_of_both_endpoints_must_match() {
        let source = endpoint([8, 1], &[0.0, 1.0, 2.0]);
        assert!(mostly_unchanged(&source, &source));
        assert!(mostly_unchanged(
            &source,
            &endpoint([8, 2], &[0.0, 1.0, 4.0])
        ));
        let half = endpoint([8, 2], &[0.0, 1.0, 4.0, 5.0]);
        assert!(!mostly_unchanged(&source, &half));
        assert!(!mostly_unchanged(&half, &source));
        let blank = endpoint([8, 1], &[]);
        assert!(mostly_unchanged(&blank, &endpoint([8, 2], &[])));
        assert!(!mostly_unchanged(&blank, &source));
        assert!(!mostly_unchanged(&source, &blank));
        let duplicate = endpoint([8, 1], &[0.0, 0.0, 0.0]);
        assert!(!mostly_unchanged(&source, &duplicate));
    }

    #[test]
    fn shared_solid_backgrounds_stay_below_both_endpoints_text() {
        let mut source = endpoint([8, 1], &[0.0, 2.0]);
        let solid = Quad::solid([2.0, 0.0, 3.0, 1.0], Color::rgb([12, 34, 56]));
        source.quads.insert(1, solid.clone());
        let mut target = endpoint([8, 2], &[0.0, 4.0]);
        target.quads.insert(1, solid.clone());
        let frame = blend(&source, &target, 0.5).unwrap();
        assert_eq!(frame.quads[1], solid);
        assert_eq!(frame.quads[2].rect, source.quads[3].rect);
        assert_eq!(frame.quads[2].color.0[3], 0.5);
        assert_eq!(frame.quads[3], target.quads[2]);
        assert_eq!(frame.quads[4].rect, target.quads[3].rect);
        assert_eq!(frame.quads[4].color.0[3], 0.5);
        assert_eq!(frame.quads.iter().filter(|quad| **quad == solid).count(), 1);
    }

    #[test]
    fn endpoints_are_exact_and_intermediate_frames_clip_to_each_endpoint() {
        let source = endpoint([8, 1], &[0.0, 7.5]);
        let target = endpoint([4, 2], &[0.0, 3.5]);
        for (mix, expected) in [(0.0, &source), (1.0, &target)] {
            let frame = blend(&source, &target, mix).unwrap();
            assert_eq!(frame.size, expected.size);
            assert_eq!(frame.quads, expected.quads);
        }
        let frame = blend(&source, &target, 0.5).unwrap();
        assert_eq!(frame.size, [8, 2]);
        assert_eq!(frame.quads[1].rect, [7.5, 0.0, 0.5, 1.0]);
        assert_eq!(frame.quads[1].uv, [0.0, 0.0, 0.5, 1.0]);
        assert_eq!(frame.quads[3].rect, [3.5, 0.0, 0.5, 1.0]);
        assert_eq!(frame.quads[3].uv, [0.0, 0.0, 0.5, 1.0]);
    }

    #[test]
    fn incompatible_atlases_are_rejected() {
        let source = endpoint([8, 1], &[0.0]);
        let mut target = endpoint([8, 2], &[0.0]);
        target.generation = 1;
        assert!(matches!(
            blend(&source, &target, 0.5),
            Err(ComposeError::GenerationChanged)
        ));
    }
}
