//! Cell-sized procedural glyphs, ported from Ghostty's MIT-licensed sprite face.
//! Rectangles use Ghostty's pixel alignment; tiny-skia rasterizes curved paths.

use crate::{BitmapFormat, FontError, FontMetrics, GlyphBitmap};
use tiny_skia::{FillRule, Mask, Paint, Path, PathBuilder, Pixmap, Stroke, Transform};

mod legacy;
mod tables;

/// These codepoints must fill the cell, independently of the selected font.
pub fn contains(cp: char) -> bool {
    legacy::contains(cp as u32)
        || matches!(cp as u32,
        0x2500..=0x259f | 0x25e2..=0x25e5 | 0x25f8..=0x25fa | 0x25ff |
        0x2800..=0x28ff | 0xe0b0..=0xe0bf | 0xe0d2 | 0xe0d4 |
        0xf5d0..=0xf60d | 0x1fb00..=0x1fb3b | 0x1cd00..=0x1cde5)
}

/// Rasterize a supported sprite at the current cell metrics, or return `None`
/// so ordinary text can use native font shaping. `columns` includes wide cells.
pub fn rasterize(
    cp: char,
    metrics: FontMetrics,
    columns: u8,
) -> Result<Option<GlyphBitmap>, FontError> {
    if !contains(cp) {
        return Ok(None);
    }
    let width = metrics
        .cell_width
        .checked_mul(u32::from(columns.max(1)))
        .ok_or_else(|| FontError("sprite width overflow".into()))?;
    let height = metrics.cell_height;
    if width == 0
        || height == 0
        || width > 4096
        || height > 4096
        || !metrics.baseline.is_finite()
        || !metrics.underline_thickness.is_finite()
    {
        return Err(FontError("invalid sprite cell metrics".into()));
    }
    let mut c = Canvas {
        pixmap: Pixmap::new(width, height)
            .ok_or_else(|| FontError("invalid sprite size".into()))?,
        w: width as i32,
        h: height as i32,
        t: metrics
            .underline_thickness
            .round()
            .clamp(1.0, width.max(height) as f32) as i32,
    };
    match cp as u32 {
        cp @ 0x2500..=0x257f => c.box_drawing(cp),
        cp @ 0x2580..=0x259f => c.block(cp),
        cp @ 0x2800..=0x28ff => c.braille(cp as u8),
        cp @ 0xe0b0..=0xe0bf | cp @ 0xe0d2 | cp @ 0xe0d4 => c.powerline(cp),
        cp @ 0xf5d0..=0xf60d => c.branch(cp),
        cp @ 0x1fb00..=0x1fb3b => {
            let index = cp - 0x1fb00;
            c.mosaic(index + index / 20 + 1, 2, 3);
        }
        cp @ 0x1cd00..=0x1cde5 => c.mosaic(tables::OCTANTS[(cp - 0x1cd00) as usize] as u32, 2, 4),
        cp @ 0x25e2..=0x25e5 => c.corner_triangle([3, 2, 0, 1][(cp - 0x25e2) as usize], 255, false),
        cp @ 0x25f8..=0x25fa => c.corner_triangle((cp - 0x25f8) as usize, 255, true),
        0x25ff => c.corner_triangle(3, 255, true),
        _ => c.legacy(cp as u32),
    }
    Ok(Some(GlyphBitmap {
        width,
        height,
        bearing_x: 0,
        bearing_y: metrics.baseline.round() as i32,
        format: BitmapFormat::Alpha,
        pixels: c.pixmap.pixels().iter().map(|p| p.alpha()).collect(),
    }))
}

struct Canvas {
    pixmap: Pixmap,
    w: i32,
    h: i32,
    t: i32,
}

impl Canvas {
    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, alpha: u8) {
        let stride = self.w as usize * 4;
        for y in y0.max(0)..y1.min(self.h) {
            let start = y as usize * stride + x0.clamp(0, self.w) as usize * 4;
            let end =
                y as usize * stride + x1.clamp(0, self.w).max(x0.clamp(0, self.w)) as usize * 4;
            for pixel in self.pixmap.data_mut()[start..end].as_chunks_mut::<4>().0 {
                *pixel = [alpha; 4];
            }
        }
    }

    fn fraction(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, alpha: u8) {
        let min = |f: f32, size: i32| size - ((1.0 - f) * size as f32).round() as i32;
        let max = |f: f32, size: i32| (f * size as f32).round() as i32;
        self.rect(
            min(x0, self.w),
            min(y0, self.h),
            max(x1, self.w),
            max(y1, self.h),
            alpha,
        );
    }

    fn paint(alpha: u8) -> Paint<'static> {
        let mut paint = Paint::default();
        paint.set_color_rgba8(255, 255, 255, alpha);
        paint.anti_alias = true;
        paint
    }

    fn fill(&mut self, path: Option<Path>, alpha: u8) {
        if let Some(path) = path {
            self.pixmap.fill_path(
                &path,
                &Self::paint(alpha),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    fn stroke(&mut self, path: Option<Path>, width: f32, inner: bool) {
        let Some(path) = path else {
            return;
        };
        let mask = if inner {
            let mut mask = Mask::new(self.w as u32, self.h as u32).expect("validated sprite size");
            mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
            Some(mask)
        } else {
            None
        };
        self.pixmap.stroke_path(
            &path,
            &Self::paint(255),
            &Stroke {
                width: if inner { 2.0 * width } else { width },
                ..Default::default()
            },
            Transform::identity(),
            mask.as_ref(),
        );
    }

    fn polygon(&mut self, points: &[[f32; 2]], alpha: u8, outline: bool) {
        let mut p = PathBuilder::new();
        if let Some(&[x, y]) = points.first() {
            p.move_to(x, y);
        }
        for &[x, y] in points.iter().skip(1) {
            p.line_to(x, y);
        }
        p.close();
        if outline {
            self.stroke(p.finish(), self.t as f32, true);
        } else {
            self.fill(p.finish(), alpha);
        }
    }

    fn line(&mut self, a: [f32; 2], b: [f32; 2]) {
        let mut p = PathBuilder::new();
        p.move_to(a[0], a[1]);
        p.line_to(b[0], b[1]);
        self.stroke(p.finish(), self.t as f32, false);
    }

    fn flip_x(&mut self) {
        for row in self.pixmap.data_mut().chunks_exact_mut(self.w as usize * 4) {
            let row = row.as_chunks_mut::<4>().0;
            row.reverse();
        }
    }

    fn mosaic(&mut self, pattern: u32, columns: u32, rows: u32) {
        for i in 0..columns * rows {
            if pattern & (1 << i) != 0 {
                let (x, y) = (i % columns, i / columns);
                self.fraction(
                    x as f32 / columns as f32,
                    y as f32 / rows as f32,
                    (x + 1) as f32 / columns as f32,
                    (y + 1) as f32 / rows as f32,
                    255,
                );
            }
        }
    }

    fn block(&mut self, cp: u32) {
        let (w, h) = (self.w, self.h);
        match cp {
            0x2580 => self.rect(0, 0, w, (h as f32 / 2.0).round() as i32, 255),
            0x2581..=0x2588 => self.rect(
                0,
                h - (h as f32 * (cp - 0x2580) as f32 / 8.0).round() as i32,
                w,
                h,
                255,
            ),
            0x2589..=0x258f => self.rect(
                0,
                0,
                (w as f32 * (0x2590 - cp) as f32 / 8.0).round() as i32,
                h,
                255,
            ),
            0x2590 => self.rect(w - (w as f32 / 2.0).round() as i32, 0, w, h, 255),
            0x2591..=0x2593 => self.rect(0, 0, w, h, ((cp - 0x2590) * 64) as u8),
            0x2594 => self.rect(0, 0, w, (h as f32 / 8.0).round() as i32, 255),
            0x2595 => self.rect(w - (w as f32 / 8.0).round() as i32, 0, w, h, 255),
            0x2596..=0x259f => self.mosaic(
                [4, 8, 1, 13, 9, 7, 11, 2, 6, 14][(cp - 0x2596) as usize],
                2,
                2,
            ),
            _ => unreachable!(),
        }
    }

    fn braille(&mut self, pattern: u8) {
        let (mut dot, mut xs, mut ys) = ((self.w / 4).min(self.h / 8), self.w / 4, self.h / 8);
        let (mut xm, mut ym) = (xs / 2, ys / 2);
        let (mut xr, mut yr) = (
            self.w - 2 * xm - xs - 2 * dot,
            self.h - 2 * ym - 3 * ys - 4 * dot,
        );
        if xr >= 2 && yr >= 4 && dot == 0 {
            dot += 1;
            xr -= 2;
            yr -= 4;
        }
        if xr >= 2 && xm == 0 {
            xm = 1;
            xr -= 2;
        }
        if yr >= 2 && ym == 0 {
            ym = 1;
            yr -= 2;
        }
        if xr >= 1 {
            xs += 1;
            xr -= 1;
        }
        if yr >= 3 {
            ys += 1;
            yr -= 3;
        }
        if xr >= 2 {
            xm += 1;
            xr -= 2;
        }
        if yr >= 2 {
            ym += 1;
            yr -= 2;
        }
        if xr >= 2 && yr >= 4 {
            dot += 1;
        }
        for (bit, (x, y)) in [
            (0, 0),
            (0, 1),
            (0, 2),
            (1, 0),
            (1, 1),
            (1, 2),
            (0, 3),
            (1, 3),
        ]
        .into_iter()
        .enumerate()
        {
            if pattern & (1 << bit) != 0 {
                let (x, y) = (xm + x * (dot + xs), ym + y * (dot + ys));
                self.rect(x, y, x + dot, y + dot, 255);
            }
        }
    }

    fn box_drawing(&mut self, cp: u32) {
        let lines = tables::BOX_LINES[(cp - 0x2500) as usize];
        if lines != 0 {
            self.lines(lines);
            return;
        }
        match cp {
            0x2504..=0x250b | 0x254c..=0x254f => {
                let count = if cp >= 0x254c {
                    2
                } else if cp >= 0x2508 {
                    4
                } else {
                    3
                };
                self.dashes(cp & 2 != 0, count, self.t * if cp & 1 != 0 { 2 } else { 1 });
            }
            0x256d..=0x2570 => self.arc([3, 2, 0, 1][(cp - 0x256d) as usize]),
            0x2571..=0x2573 => {
                if cp != 0x2572 {
                    self.diagonal(true);
                }
                if cp != 0x2571 {
                    self.diagonal(false);
                }
            }
            _ => unreachable!("box table covers every straight line"),
        }
    }

    // Bit pairs specify up, right, down, left: 0 none, 1 light, 2 heavy, 3 double.
    fn lines(&mut self, pattern: u8) {
        let [up, right, down, left] = [
            pattern & 3,
            (pattern >> 2) & 3,
            (pattern >> 4) & 3,
            (pattern >> 6) & 3,
        ];
        let (w, h, t) = (self.w, self.h, self.t);
        let (ht, hb) = ((h - t).max(0) / 2, (h - t).max(0) / 2 + t);
        let (hht, hhb) = ((h - 2 * t).max(0) / 2, (h - 2 * t).max(0) / 2 + 2 * t);
        let (hdt, hdb) = ((ht - t).max(0), hb + t);
        let (vl, vr) = ((w - t).max(0) / 2, (w - t).max(0) / 2 + t);
        let (vhl, vhr) = ((w - 2 * t).max(0) / 2, (w - 2 * t).max(0) / 2 + 2 * t);
        let (vdl, vdr) = ((vl - t).max(0), vr + t);
        let ub = if left == 2 || right == 2 {
            hhb
        } else if left != right || down == up {
            if left == 3 || right == 3 { hdb } else { hb }
        } else if left == 0 && right == 0 {
            hb
        } else {
            ht
        };
        let dt = if left == 2 || right == 2 {
            hht
        } else if left != right || up == down {
            if left == 3 || right == 3 { hdt } else { ht }
        } else if left == 0 && right == 0 {
            ht
        } else {
            hb
        };
        let lr = if up == 2 || down == 2 {
            vhr
        } else if up != down || left == right {
            if up == 3 || down == 3 { vdr } else { vr }
        } else if up == 0 && down == 0 {
            vr
        } else {
            vl
        };
        let rl = if up == 2 || down == 2 {
            vhl
        } else if up != down || right == left {
            if up == 3 || down == 3 { vdl } else { vl }
        } else if up == 0 && down == 0 {
            vl
        } else {
            vr
        };
        match up {
            1 => self.rect(vl, 0, vr, ub, 255),
            2 => self.rect(vhl, 0, vhr, ub, 255),
            3 => {
                self.rect(vdl, 0, vl, if left == 3 { ht } else { ub }, 255);
                self.rect(vr, 0, vdr, if right == 3 { ht } else { ub }, 255);
            }
            _ => (),
        }
        match right {
            1 => self.rect(rl, ht, w, hb, 255),
            2 => self.rect(rl, hht, w, hhb, 255),
            3 => {
                self.rect(if up == 3 { vr } else { rl }, hdt, w, ht, 255);
                self.rect(if down == 3 { vr } else { rl }, hb, w, hdb, 255);
            }
            _ => (),
        }
        match down {
            1 => self.rect(vl, dt, vr, h, 255),
            2 => self.rect(vhl, dt, vhr, h, 255),
            3 => {
                self.rect(vdl, if left == 3 { hb } else { dt }, vl, h, 255);
                self.rect(vr, if right == 3 { hb } else { dt }, vdr, h, 255);
            }
            _ => (),
        }
        match left {
            1 => self.rect(0, ht, lr, hb, 255),
            2 => self.rect(0, hht, lr, hhb, 255),
            3 => {
                self.rect(0, hdt, if up == 3 { vl } else { lr }, ht, 255);
                self.rect(0, hb, if down == 3 { vl } else { lr }, hdb, 255);
            }
            _ => (),
        }
    }

    fn dashes(&mut self, vertical: bool, count: i32, thickness: i32) {
        let size = if vertical { self.h } else { self.w };
        if size < count * 2 {
            self.lines(if vertical { 17 } else { 68 });
            return;
        }
        let desired_gap = if count == 2 {
            if vertical { self.t * 2 } else { thickness }
        } else {
            self.t.max(4)
        };
        let gap = desired_gap.min(size / (2 * count));
        let dash = (size - count * gap) / count;
        let extra = (size - count * gap) % count;
        let offset = if vertical {
            (self.w - thickness).max(0) / 2
        } else {
            (self.h - thickness).max(0) / 2
        };
        let mut start = if vertical { 0 } else { gap / 2 };
        for i in 0..count {
            let end = start + dash + i32::from(i < extra);
            if vertical {
                self.rect(offset, start, offset + thickness, end, 255);
            } else {
                self.rect(start, offset, end, offset + thickness, 255);
            }
            start = end + gap;
        }
    }

    fn diagonal(&mut self, rising: bool) {
        let (w, h) = (self.w as f32, self.h as f32);
        let (sx, sy) = ((w / h).min(1.0) / 2.0, (h / w).min(1.0) / 2.0);
        if rising {
            self.line([w + sx, -sy], [-sx, h + sy]);
        } else {
            self.line([-sx, -sy], [w + sx, h + sy]);
        }
    }

    // Corner bits: 0 top-left, 1 top-right, 2 bottom-left, 3 bottom-right.
    fn arc(&mut self, corner: usize) {
        let (w, h, t) = (self.w as f32, self.h as f32, self.t as f32);
        let (cx, cy) = (
            ((self.w - self.t).max(0) / 2) as f32 + t / 2.0,
            ((self.h - self.t).max(0) / 2) as f32 + t / 2.0,
        );
        let r = w.min(h) / 2.0;
        let dx = if corner & 1 == 0 { -1.0 } else { 1.0 };
        let dy = if corner & 2 == 0 { -1.0 } else { 1.0 };
        let mut p = PathBuilder::new();
        p.move_to(cx, if dy < 0.0 { 0.0 } else { h });
        p.line_to(cx, cy + dy * r);
        p.cubic_to(
            cx,
            cy + dy * r * 0.25,
            cx + dx * r * 0.25,
            cy,
            cx + dx * r,
            cy,
        );
        p.line_to(if dx < 0.0 { 0.0 } else { w }, cy);
        self.stroke(p.finish(), t, false);
    }

    fn corner_triangle(&mut self, corner: usize, alpha: u8, outline: bool) {
        let (w, h) = (self.w as f32, self.h as f32);
        let points = match corner {
            0 => [[0.0, 0.0], [0.0, h], [w, 0.0]],
            1 => [[0.0, 0.0], [w, h], [w, 0.0]],
            2 => [[0.0, 0.0], [0.0, h], [w, h]],
            _ => [[0.0, h], [w, h], [w, 0.0]],
        };
        self.polygon(&points, alpha, outline);
    }

    fn powerline(&mut self, cp: u32) {
        let (w, h, t) = (self.w as f32, self.h as f32, self.t as f32);
        match cp {
            0xe0b0 | 0xe0b2 => {
                self.polygon(&[[0.0, 0.0], [w, h / 2.0], [0.0, h]], 255, false);
                if cp == 0xe0b2 {
                    self.flip_x();
                }
            }
            0xe0b1 | 0xe0b3 => {
                let mut p = PathBuilder::new();
                p.move_to(0.0, 0.0);
                p.line_to(w, h / 2.0);
                p.line_to(0.0, h);
                self.stroke(p.finish(), t, true);
                if cp == 0xe0b3 {
                    self.flip_x();
                }
            }
            0xe0b4..=0xe0b7 => {
                let r = w.min(h / 2.0);
                let k = (std::f32::consts::SQRT_2 - 1.0) * 4.0 / 3.0;
                let outline = cp & 1 != 0;
                let mut p = PathBuilder::new();
                p.move_to(0.0, 0.0);
                if outline {
                    p.line_to(1.0, 0.0);
                }
                p.cubic_to(r * k, 0.0, r, r - r * k, r, r);
                p.line_to(r, h - r);
                p.cubic_to(
                    r,
                    h - r + r * k,
                    r * k,
                    h,
                    if outline { 1.0 } else { 0.0 },
                    h,
                );
                if outline {
                    p.line_to(0.0, h);
                    self.stroke(p.finish(), t, true);
                } else {
                    p.close();
                    self.fill(p.finish(), 255);
                }
                if cp >= 0xe0b6 {
                    self.flip_x();
                }
            }
            0xe0b8 => self.corner_triangle(2, 255, false),
            0xe0ba => self.corner_triangle(3, 255, false),
            0xe0bc => self.corner_triangle(0, 255, false),
            0xe0be => self.corner_triangle(1, 255, false),
            0xe0b9 | 0xe0bf => self.diagonal(false),
            0xe0bb | 0xe0bd => self.diagonal(true),
            0xe0d2 | 0xe0d4 => {
                self.polygon(
                    &[
                        [0.0, 0.0],
                        [w, 0.0],
                        [w / 2.0, h / 2.0 - t / 2.0],
                        [0.0, h / 2.0 - t / 2.0],
                    ],
                    255,
                    false,
                );
                self.polygon(
                    &[
                        [0.0, h],
                        [w, h],
                        [w / 2.0, h / 2.0 + t / 2.0],
                        [0.0, h / 2.0 + t / 2.0],
                    ],
                    255,
                    false,
                );
                if cp == 0xe0d4 {
                    self.flip_x();
                }
            }
            _ => unreachable!(),
        }
    }

    fn branch(&mut self, cp: u32) {
        if cp >= 0xf5ee {
            self.branch_node(tables::BRANCH_NODES[(cp - 0xf5ee) as usize]);
            return;
        }
        match cp {
            0xf5d0 => self.lines(68),
            0xf5d1 => self.lines(17),
            0xf5d2..=0xf5d5 => {
                let vertical = cp >= 0xf5d4;
                let reverse = cp & 1 == 0;
                let count = if vertical { self.h } else { self.w };
                for i in 0..count {
                    let value = (255.0
                        * if reverse {
                            (count - i) as f32
                        } else {
                            i as f32
                        }
                        / count as f32)
                        .round() as u8;
                    if vertical {
                        let x = (self.w - self.t).max(0) / 2;
                        self.rect(x, i, x + self.t, i + 1, value);
                    } else {
                        let y = (self.h - self.t).max(0) / 2;
                        self.rect(i, y, i + 1, y + self.t, value);
                    }
                }
            }
            _ => {
                let pattern = tables::BRANCH_LINES[(cp - 0xf5d6) as usize];
                if pattern & 16 != 0 {
                    self.lines(68);
                }
                if pattern & 32 != 0 {
                    self.lines(17);
                }
                for corner in 0..4 {
                    if pattern & (1 << corner) != 0 {
                        self.arc(corner);
                    }
                }
            }
        }
    }

    fn branch_node(&mut self, node: u8) {
        let (w, h, t) = (self.w, self.h, self.t);
        let (vl, ht) = ((w - t).max(0) / 2, (h - t).max(0) / 2);
        let (cx, cy) = (vl as f32 + t as f32 / 2.0, ht as f32 + t as f32 / 2.0);
        let r = cx.min(cy).min(w as f32 - cx).min(h as f32 - cy);
        let half = t as f32 / 2.0;
        if node & 1 != 0 {
            self.rect(vl, 0, vl + t, (cy - r + half).ceil() as i32, 255);
        }
        if node & 2 != 0 {
            self.rect((cx + r - half).floor() as i32, ht, w, ht + t, 255);
        }
        if node & 4 != 0 {
            self.rect(vl, (cy + r - half).floor() as i32, vl + t, h, 255);
        }
        if node & 8 != 0 {
            self.rect(0, ht, (cx - r + half).ceil() as i32, ht + t, 255);
        }
        let radius = if node & 16 != 0 {
            r
        } else {
            (r - half).max(0.0)
        };
        let path = PathBuilder::from_circle(cx, cy, radius);
        if node & 16 != 0 {
            self.fill(path, 255);
        } else {
            self.stroke(path, t as f32, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(w: u32, h: u32, t: f32) -> FontMetrics {
        FontMetrics {
            cell_width: w,
            cell_height: h,
            baseline: h as f32 - 2.0,
            underline_position: 1.0,
            underline_thickness: t,
        }
    }

    #[test]
    fn boxes_blocks_and_braille_match_ghostty_pixels() {
        let data = include_bytes!("../../../src/font/sprite/testdata/U+2500...U+25FF-9x17+1.png");
        let mut reader = png::Decoder::new(std::io::Cursor::new(data))
            .read_info()
            .unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let image = reader.next_frame(&mut pixels).unwrap();
        assert_eq!(image.color_type, png::ColorType::Grayscale);
        let (w, h, padx, pady) = (9, 17, 2, 4);
        for cp in 0x2500..=0x259f {
            if (0x256d..=0x2573).contains(&cp) {
                continue;
            }
            let ours = rasterize(char::from_u32(cp).unwrap(), metrics(w, h, 1.0), 1)
                .unwrap()
                .unwrap();
            let index = cp - 0x2500;
            for y in 0..h {
                for x in 0..w {
                    let rx = (index % 16) * (w + 2 * padx) + padx + x;
                    let ry = (index / 16) * (h + 2 * pady) + pady + y;
                    assert_eq!(
                        ours.pixels[(y * w + x) as usize],
                        pixels[(ry * image.width + rx) as usize],
                        "U+{cp:04x} at {x},{y}"
                    );
                }
            }
        }
        let blank = rasterize('\u{2800}', metrics(9, 17, 1.0), 1)
            .unwrap()
            .unwrap();
        assert!(blank.pixels.iter().all(|p| *p == 0));
        let all = rasterize('\u{28ff}', metrics(9, 17, 1.0), 1)
            .unwrap()
            .unwrap();
        assert_eq!(all.pixels.iter().filter(|p| **p != 0).count(), 32);
    }

    #[test]
    fn sprites_cover_cells_and_all_declared_ranges_are_drawable() {
        let full = rasterize('█', metrics(11, 21, 2.0), 2).unwrap().unwrap();
        assert_eq!((full.width, full.height), (22, 21));
        assert!(full.pixels.iter().all(|a| *a == 255));
        for cp in (0x2500..=0x28ff)
            .chain(0xe0b0..=0xe0d4)
            .chain(0xf5d0..=0xf60d)
            .chain(0x1fb00..=0x1fbef)
            .chain(0x1cc00..=0x1ceaf)
        {
            let cp = char::from_u32(cp).unwrap();
            if contains(cp) {
                assert!(rasterize(cp, metrics(11, 21, 2.0), 1).unwrap().is_some());
                assert!(rasterize(cp, metrics(1, 1, 1.0), 1).unwrap().is_some());
            }
        }
        assert!(rasterize('A', metrics(11, 21, 2.0), 1).unwrap().is_none());
        assert!(rasterize('█', metrics(u32::MAX, 21, 2.0), 2).is_err());
    }
}
