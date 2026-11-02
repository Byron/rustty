//! Blend endpoint frames without dimming the text they have in common.
use rustty_render::{ComposeError, Frame, Paint, Quad};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bottom {
    pub source_y: f32,
    pub target_y: f32,
}

type QuadKey = (u8, usize, [u32; 4], [u32; 4], [u32; 4]);

fn key(quad: &Quad) -> QuadKey {
    offset_key(quad, 0.0)
}

fn offset_key(quad: &Quad, offset: f32) -> QuadKey {
    let mut rect = quad.rect.map(f32::to_bits);
    // Interpolated positions can differ by a few float bits after undoing their offset.
    rect[1] = ((quad.rect[1] + offset) * 64.0).round().to_bits();
    (
        quad.paint as u8,
        quad.atlas,
        rect,
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
    take_key(counts, key(quad))
}

fn take_key(counts: &mut HashMap<QuadKey, usize>, key: QuadKey) -> bool {
    let Some(count) = counts.get_mut(&key).filter(|count| **count > 0) else {
        return false;
    };
    *count -= 1;
    true
}

pub fn bottom_anchor(
    source: &Frame,
    target: &Frame,
    height_change: f32,
    cell_height: f32,
    padding_top: f32,
) -> Option<Bottom> {
    if height_change == 0.0
        || !height_change.is_finite()
        || !cell_height.is_finite()
        || cell_height <= 0.0
        || !padding_top.is_finite()
        || source.generation != target.generation
    {
        return None;
    }
    let text = |quad: &&Quad| quad.paint != Paint::Solid;
    let row = |quad: &Quad, offset: f32| {
        ((quad.rect[1] + quad.rect[3] * 0.5 + offset - padding_top) / cell_height).floor() as i64
    };
    let mut rows = BTreeMap::<i64, [usize; 4]>::new();
    let mut shifted = HashMap::new();
    let mut fixed = counts(target.quads.iter().filter(text));
    for quad in target.quads.iter().filter(text) {
        *shifted.entry(offset_key(quad, -height_change)).or_default() += 1;
        rows.entry(row(quad, -height_change)).or_default()[1] += 1;
    }
    for quad in source.quads.iter().filter(text) {
        let counts = rows.entry(row(quad, 0.0)).or_default();
        counts[0] += 1;
        counts[2] += usize::from(take(&mut shifted, quad));
        counts[3] += usize::from(take(&mut fixed, quad));
    }
    let mut suffix = [0; 4];
    let mut best = None;
    let mut best_gain = 0;
    for (row, counts) in rows.into_iter().rev() {
        for (total, count) in suffix.iter_mut().zip(counts) {
            *total += count;
        }
        let [source_count, target_count, shifted, fixed] = suffix;
        let gain = shifted.saturating_sub(fixed);
        let source_y = padding_top + row as f32 * cell_height;
        let target_y = source_y + height_change;
        if gain > best_gain
            && shifted > source_count.max(target_count) / 2
            && source_y >= padding_top
            && target_y >= padding_top
            && source_y < source.size[1] as f32
            && target_y < target.size[1] as f32
        {
            best = Some(Bottom { source_y, target_y });
            best_gain = gain;
        }
    }
    best.filter(|bottom| {
        [(source, bottom.source_y), (target, bottom.target_y)]
            .into_iter()
            .all(|(frame, cutoff)| {
                // Keep images spanning multiple rows out of independently moving bands.
                !frame.quads.iter().any(|quad| {
                    quad.paint != Paint::Solid
                        && quad.rect[3] > cell_height
                        && quad.rect[1] < cutoff
                        && quad.rect[1] + quad.rect[3] > cutoff
                })
            })
    })
}

pub fn mostly_unchanged(source: &Frame, target: &Frame, bottom: Option<Bottom>) -> bool {
    let text = |quad: &&Quad| quad.paint != Paint::Solid;
    let mut remaining = counts(source.quads.iter().filter(text));
    let source_count: usize = remaining.values().sum();
    let target_count = target.quads.iter().filter(text).count();
    let matched = target
        .quads
        .iter()
        .filter(text)
        .filter(|quad| {
            let offset = bottom
                .filter(|bottom| quad.rect[1] + quad.rect[3] * 0.5 >= bottom.target_y)
                .map_or(0.0, |bottom| bottom.source_y - bottom.target_y);
            take_key(&mut remaining, offset_key(quad, offset))
        })
        .count();
    let total = source_count.max(target_count);
    total == 0 || matched > total / 2
}

fn append_endpoint(
    frame: &mut Frame,
    endpoint: &Frame,
    band: Option<(f32, f32, f32)>,
) -> Result<(), ComposeError> {
    let width = endpoint.size[0] as f32;
    let height = endpoint.size[1] as f32;
    let Some((cutoff, offset, moving_cutoff)) = band else {
        return frame.append_clipped(endpoint, [0.0; 2], [0.0, 0.0, width, height]);
    };
    if let Some(background) = endpoint.quads.first() {
        frame.quads.push(background.clone());
    }
    for (footer, offset, top, bottom) in [
        (false, 0.0, 0.0, cutoff.min(moving_cutoff).min(height)),
        (true, offset, moving_cutoff, height + offset),
    ] {
        if bottom <= top {
            continue;
        }
        let mut band = endpoint.clone();
        band.quads.retain(|quad| {
            quad.paint == Paint::Solid || (quad.rect[1] + quad.rect[3] * 0.5 >= cutoff) == footer
        });
        let start = frame.quads.len();
        frame.append_clipped(&band, [0.0, offset], [0.0, top, width, bottom - top])?;
        // The full-frame background stays fixed; only its content is split into bands.
        if frame.quads.len() > start {
            frame.quads.remove(start);
        }
    }
    Ok(())
}

pub fn project(endpoint: &Frame, cutoff: f32, offset: f32) -> Result<Frame, ComposeError> {
    if !cutoff.is_finite() || !offset.is_finite() {
        return Err(ComposeError::InvalidClip);
    }
    if offset == 0.0 {
        return Ok(endpoint.clone());
    }
    let mut size = endpoint.size;
    size[1] = (size[1] as f32 + offset.max(0.0)).ceil() as u32;
    let mut frame = Frame::empty(size);
    append_endpoint(
        &mut frame,
        endpoint,
        Some((cutoff, offset, cutoff + offset)),
    )?;
    if let Some(background) = frame.quads.first_mut() {
        background.rect = [0.0, 0.0, size[0] as f32, size[1] as f32];
    }
    Ok(frame)
}

pub fn blend(
    source: &Frame,
    target: &Frame,
    mix: f32,
    bottom: Option<Bottom>,
) -> Result<Frame, ComposeError> {
    if mix <= 0.0 {
        return Ok(source.clone());
    }
    if mix >= 1.0 {
        return Ok(target.clone());
    }
    let size = std::array::from_fn(|axis| source.size[axis].max(target.size[axis]));
    let mut frame = Frame::empty(size);
    let bands = bottom
        .filter(|bottom| bottom.source_y != bottom.target_y)
        .map(|bottom| {
            let shift = (bottom.target_y - bottom.source_y) * mix;
            let cutoff = bottom.source_y + shift;
            (
                (bottom.source_y, shift, cutoff),
                (bottom.target_y, cutoff - bottom.target_y, cutoff),
            )
        });
    append_endpoint(&mut frame, source, bands.map(|bands| bands.0))?;
    let split = frame.quads.len();
    append_endpoint(&mut frame, target, bands.map(|bands| bands.1))?;
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
            let frame = blend(&source, &target, mix, None).unwrap();
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
        assert!(mostly_unchanged(&source, &source, None));
        assert!(mostly_unchanged(
            &source,
            &endpoint([8, 2], &[0.0, 1.0, 4.0]),
            None
        ));
        let half = endpoint([8, 2], &[0.0, 1.0, 4.0, 5.0]);
        assert!(!mostly_unchanged(&source, &half, None));
        assert!(!mostly_unchanged(&half, &source, None));
        let blank = endpoint([8, 1], &[]);
        assert!(mostly_unchanged(&blank, &endpoint([8, 2], &[]), None));
        assert!(!mostly_unchanged(&blank, &source, None));
        assert!(!mostly_unchanged(&source, &blank, None));
        let duplicate = endpoint([8, 1], &[0.0, 0.0, 0.0]);
        assert!(!mostly_unchanged(&source, &duplicate, None));
    }

    #[test]
    fn shared_solid_backgrounds_stay_below_both_endpoints_text() {
        let mut source = endpoint([8, 1], &[0.0, 2.0]);
        let solid = Quad::solid([2.0, 0.0, 3.0, 1.0], Color::rgb([12, 34, 56]));
        source.quads.insert(1, solid.clone());
        let mut target = endpoint([8, 2], &[0.0, 4.0]);
        target.quads.insert(1, solid.clone());
        let frame = blend(&source, &target, 0.5, None).unwrap();
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
            let frame = blend(&source, &target, mix, None).unwrap();
            assert_eq!(frame.size, expected.size);
            assert_eq!(frame.quads, expected.quads);
        }
        let frame = blend(&source, &target, 0.5, None).unwrap();
        assert_eq!(frame.size, [8, 2]);
        assert_eq!(frame.quads[1].rect, [7.5, 0.0, 0.5, 1.0]);
        assert_eq!(frame.quads[1].uv, [0.0, 0.0, 0.5, 1.0]);
        assert_eq!(frame.quads[3].rect, [3.5, 0.0, 0.5, 1.0]);
        assert_eq!(frame.quads[3].uv, [0.0, 0.0, 0.5, 1.0]);
    }

    fn rows(height: u32, rows: &[(f32, &[f32])]) -> Frame {
        let mut frame = endpoint([8, height], &[]);
        for &(row, positions) in rows {
            for &x in positions {
                let mut glyph = endpoint([8, height], &[x]).quads.pop().unwrap();
                glyph.rect[1] = 4.0 + row * 10.0;
                glyph.rect[3] = 4.0;
                frame.quads.push(glyph);
            }
        }
        frame
    }

    #[test]
    fn mixed_header_and_footer_follow_the_grid_in_both_directions() {
        let small = rows(
            83,
            &[
                (0.0, &[0.0, 1.0, 2.0]),
                (6.0, &[0.0, 2.0]),
                (7.0, &[1.0, 3.0]),
            ],
        );
        let large = rows(
            107,
            &[
                (0.0, &[0.0, 1.0, 2.0]),
                (8.0, &[0.0, 2.0]),
                (9.0, &[1.0, 3.0]),
            ],
        );
        for (source, target, delta, cutoff) in
            [(&small, &large, 20.0, 62.0), (&large, &small, -20.0, 82.0)]
        {
            let bottom = bottom_anchor(source, target, delta, 10.0, 2.0).unwrap();
            assert_eq!(
                bottom,
                Bottom {
                    source_y: cutoff,
                    target_y: cutoff + delta
                }
            );
            assert!(!mostly_unchanged(source, target, None));
            assert!(mostly_unchanged(source, target, Some(bottom)));
            for mix in [0.25, 0.5, 0.75] {
                let frame = blend(source, target, mix, Some(bottom)).unwrap();
                assert_eq!(frame.quads.len(), source.quads.len());
                for quad in source.quads.iter().skip(1) {
                    let mut expected = quad.clone();
                    if expected.rect[1] >= cutoff {
                        expected.rect[1] += delta * mix;
                    }
                    assert!(
                        frame.quads.contains(&expected),
                        "{delta}, {mix}: {expected:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn bottom_detection_requires_evidence_and_prefers_the_smaller_band() {
        let repeated = rows(
            83,
            &[(0.0, &[0.0]), (1.0, &[0.0]), (2.0, &[0.0]), (3.0, &[0.0])],
        );
        let mut taller = repeated.clone();
        taller.size[1] = 107;
        assert_eq!(bottom_anchor(&repeated, &taller, 20.0, 10.0, 2.0), None);
        let source = rows(83, &[(6.0, &[0.0, 1.0, 2.0, 3.0])]);
        let half = rows(107, &[(8.0, &[0.0, 1.0, 4.0, 5.0])]);
        assert_eq!(bottom_anchor(&source, &half, 20.0, 10.0, 2.0), None);
        let duplicate = rows(107, &[(8.0, &[0.0, 0.0, 0.0, 0.0])]);
        assert_eq!(bottom_anchor(&source, &duplicate, 20.0, 10.0, 2.0), None);
        let source = rows(83, &[(5.0, &[0.0, 1.0]), (6.0, &[0.0, 1.0])]);
        let target = rows(107, &[(5.0, &[0.0]), (7.0, &[1.0]), (8.0, &[0.0, 1.0])]);
        assert_eq!(
            bottom_anchor(&source, &target, 20.0, 10.0, 2.0),
            Some(Bottom {
                source_y: 62.0,
                target_y: 82.0
            })
        );
        for delta in [0.0, f32::NAN, f32::INFINITY] {
            assert_eq!(bottom_anchor(&source, &target, delta, 10.0, 2.0), None);
        }
        let mut incompatible = target.clone();
        incompatible.generation += 1;
        assert_eq!(bottom_anchor(&source, &incompatible, 20.0, 10.0, 2.0), None);
    }

    #[test]
    fn footer_backgrounds_stay_behind_text_and_clip_the_other_band() {
        let color = Color::rgb([12, 34, 56]);
        let mut source = rows(83, &[(0.0, &[0.0]), (6.0, &[1.0]), (7.0, &[2.0])]);
        source
            .quads
            .insert(1, Quad::solid([0.0, 62.0, 8.0, 20.0], color));
        let mut target = rows(
            107,
            &[(0.0, &[0.0]), (7.0, &[6.0]), (8.0, &[1.0]), (9.0, &[2.0])],
        );
        target
            .quads
            .insert(1, Quad::solid([0.0, 82.0, 8.0, 20.0], color));
        let bottom = Bottom {
            source_y: 62.0,
            target_y: 82.0,
        };
        let frame = blend(&source, &target, 0.5, Some(bottom)).unwrap();
        let background = frame
            .quads
            .iter()
            .position(|quad| quad.color == color)
            .unwrap();
        assert_eq!(frame.quads[background].rect, [0.0, 72.0, 8.0, 20.0]);
        for (index, quad) in frame.quads.iter().enumerate().skip(1) {
            assert!(quad.rect[1] + quad.rect[3] <= 72.0 || quad.rect[1] >= 72.0);
            if quad.paint != Paint::Solid && quad.rect[1] >= 72.0 {
                assert!(index > background);
                assert_eq!(quad.color.0[3], 1.0);
            }
        }
        assert!(
            !frame
                .quads
                .iter()
                .any(|quad| quad.paint != Paint::Solid && quad.rect[0] == 6.0)
        );
        let mut overhang = rows(83, &[(6.0, &[1.0])]);
        overhang.quads[1].rect[1] = 60.0;
        assert_eq!(
            blend(
                &overhang,
                &overhang,
                0.5,
                Some(Bottom {
                    source_y: 62.0,
                    target_y: 62.0
                })
            )
            .unwrap()
            .quads,
            overhang.quads,
        );
        let projected = project(&overhang, 62.0, 10.0).unwrap();
        assert_eq!(projected.size, [8, 93]);
        assert_eq!(projected.quads.len(), 2);
        assert_eq!(projected.quads[1].rect, [1.0, 72.0, 1.0, 2.0]);
        assert_eq!(projected.quads[1].uv, [0.0, 0.5, 1.0, 1.0]);
        let repeated = project(&projected, 72.0, 10.0).unwrap();
        assert_eq!(repeated.quads.len(), 2);
        assert_eq!(repeated.quads[0].rect, [0.0, 0.0, 8.0, 103.0]);
        assert_eq!(repeated.quads[1].rect, [1.0, 82.0, 1.0, 2.0]);
        assert_eq!(repeated.quads[1].uv, projected.quads[1].uv);
    }

    #[test]
    fn fractional_footer_positions_match_after_offset_cancellation() {
        let source = rows(83, &[(6.0, &[1.0]), (7.0, &[2.0])]);
        let mut target = source.clone();
        target.size[1] = 107;
        for quad in target.quads.iter_mut().skip(1) {
            quad.rect[1] += 20.3 + 0.00001;
        }
        let bottom = bottom_anchor(&source, &target, 20.3, 10.0, 2.0).unwrap();
        assert!(mostly_unchanged(&source, &target, Some(bottom)));
        let frame = blend(&source, &target, 0.37, Some(bottom)).unwrap();
        assert_eq!(frame.quads.len(), source.quads.len());
        assert!(frame.quads.iter().all(|quad| quad.color.0[3] == 1.0));
    }

    #[test]
    fn bottom_detection_does_not_slice_images_across_bands() {
        let mut source = rows(83, &[(0.0, &[0.0]), (6.0, &[1.0, 2.0]), (7.0, &[3.0])]);
        let mut target = rows(107, &[(0.0, &[0.0]), (8.0, &[1.0, 2.0]), (9.0, &[3.0])]);
        let mut image = source.quads[1].clone();
        image.paint = Paint::Color;
        image.rect = [4.0, 50.0, 2.0, 30.0];
        source.quads.push(image.clone());
        target.quads.push(image);
        assert_eq!(bottom_anchor(&source, &target, 20.0, 10.0, 2.0), None);
    }

    #[test]
    fn incompatible_atlases_are_rejected() {
        let source = endpoint([8, 1], &[0.0]);
        let mut target = endpoint([8, 2], &[0.0]);
        target.generation = 1;
        assert!(matches!(
            blend(&source, &target, 0.5, None),
            Err(ComposeError::GenerationChanged)
        ));
    }
}
