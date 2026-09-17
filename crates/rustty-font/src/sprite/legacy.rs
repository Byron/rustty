//! Remaining Symbols for Legacy Computing, following Ghostty's cell geometry.
use super::{Canvas, tables};
use tiny_skia::PathBuilder;

impl Canvas {
    pub(super) fn legacy(&mut self, cp: u32) {
        let (w, h, t) = (self.w, self.h, self.t);
        match cp {
            0x1fb3c..=0x1fb67 => {
                let mask = tables::SMOOTH_MOSAICS[(cp - 0x1fb3c) as usize];
                let points = [
                    [0.0, 0.0],
                    [0.0, 1.0 / 3.0],
                    [0.0, 2.0 / 3.0],
                    [0.0, 1.0],
                    [0.5, 1.0],
                    [1.0, 1.0],
                    [1.0, 2.0 / 3.0],
                    [1.0, 1.0 / 3.0],
                    [1.0, 0.0],
                    [0.5, 0.0],
                ];
                let points: Vec<_> = points
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| mask & (1 << i) != 0)
                    .map(|(_, p)| [p[0] * w as f32, p[1] * h as f32])
                    .collect();
                self.polygon(&points, 255, false);
            }
            0x1fb68..=0x1fb6f => {
                self.edge_triangle((cp - 0x1fb68) % 4);
                if cp < 0x1fb6c {
                    self.invert();
                }
            }
            0x1fb70..=0x1fb75 => {
                let n = (cp - 0x1fb6f) as f32;
                self.fraction(n / 8.0, 0.0, (n + 1.0) / 8.0, 1.0, 255);
            }
            0x1fb76..=0x1fb7b => {
                let n = (cp - 0x1fb75) as f32;
                self.fraction(0.0, n / 8.0, 1.0, (n + 1.0) / 8.0, 255);
            }
            0x1fb7c..=0x1fb7f => {
                let i = cp - 0x1fb7c;
                self.aligned_block([if i >= 2 { 2 } else { 0 }, 1], [0.125, 1.0], 255);
                self.aligned_block([1, if i == 0 || i == 3 { 2 } else { 0 }], [1.0, 0.125], 255);
            }
            0x1fb80 => {
                self.aligned_block([1, 0], [1.0, 0.125], 255);
                self.aligned_block([1, 2], [1.0, 0.125], 255);
            }
            0x1fb81 => {
                for n in [0, 2, 4, 7] {
                    self.fraction(0.0, n as f32 / 8.0, 1.0, (n + 1) as f32 / 8.0, 255);
                }
            }
            0x1fb82..=0x1fb86 => self.aligned_block(
                [1, 0],
                [
                    1.0,
                    [0.25, 0.375, 0.625, 0.75, 0.875][(cp - 0x1fb82) as usize],
                ],
                255,
            ),
            0x1fb87..=0x1fb8b => self.aligned_block(
                [2, 1],
                [
                    [0.25, 0.375, 0.625, 0.75, 0.875][(cp - 0x1fb87) as usize],
                    1.0,
                ],
                255,
            ),
            0x1fb8c => self.aligned_block([0, 1], [0.5, 1.0], 128),
            0x1fb8d => self.aligned_block([2, 1], [0.5, 1.0], 128),
            0x1fb8e => self.aligned_block([1, 0], [1.0, 0.5], 128),
            0x1fb8f => self.aligned_block([1, 2], [1.0, 0.5], 128),
            0x1fb90 => self.rect(0, 0, w, h, 128),
            0x1fb91 | 0x1fb92 => {
                self.rect(0, 0, w, h, 128);
                self.aligned_block([1, if cp == 0x1fb91 { 0 } else { 2 }], [1.0, 0.5], 255);
            }
            0x1fb93 => (), // Unallocated hole is an empty sprite in Ghostty.
            0x1fb94 => {
                self.rect(0, 0, w, h, 128);
                self.aligned_block([2, 1], [0.5, 1.0], 255);
            }
            0x1fb95 | 0x1fb96 => self.checkerboard((cp - 0x1fb95) as i32),
            0x1fb97 => {
                self.rect(0, h / 4, w, h * 2 / 4, 255);
                self.rect(0, h * 3 / 4, w, h, 255);
            }
            0x1fb98 | 0x1fb99 => {
                let count = (w / (2 * t)).max(1);
                let stride = (w as f32 / count as f32).round();
                for i in -count..=count {
                    let x = i as f32 * stride;
                    self.line(
                        if cp == 0x1fb98 {
                            [x, 0.0]
                        } else {
                            [x + w as f32, 0.0]
                        },
                        if cp == 0x1fb98 {
                            [x + w as f32, h as f32]
                        } else {
                            [x, h as f32]
                        },
                    );
                }
            }
            0x1fb9a => {
                self.edge_triangle(1);
                self.edge_triangle(3);
            }
            0x1fb9b => {
                self.edge_triangle(0);
                self.edge_triangle(2);
            }
            0x1fb9c..=0x1fb9f => {
                self.corner_triangle([0, 1, 3, 2][(cp - 0x1fb9c) as usize], 128, false)
            }
            0x1fba0..=0x1fbae => self.corner_lines(
                [1, 2, 4, 8, 5, 10, 12, 3, 9, 6, 14, 13, 11, 7, 15][(cp - 0x1fba0) as usize],
            ),
            0x1fbaf => self.lines(0x66),
            0x1fbbd => {
                self.diagonal(true);
                self.diagonal(false);
                self.invert();
            }
            0x1fbbe => {
                self.corner_lines(8);
                self.invert();
            }
            0x1fbbf => {
                self.corner_lines(15);
                self.invert();
            }
            0x1fbce..=0x1fbcf => self.aligned_block(
                [0, 1],
                [if cp == 0x1fbce { 2.0 / 3.0 } else { 1.0 / 3.0 }, 1.0],
                255,
            ),
            0x1fbd0..=0x1fbdf => self.cell_diagonals(cp - 0x1fbd0),
            0x1fbe0..=0x1fbe3 => self.circle([1, 2, 3, 0][(cp - 0x1fbe0) as usize], false),
            0x1fbe4..=0x1fbe7 => self.aligned_block(
                [[1, 0], [1, 2], [0, 1], [2, 1]][(cp - 0x1fbe4) as usize],
                [0.5, 0.5],
                255,
            ),
            0x1fbe8..=0x1fbeb => self.circle([1, 2, 3, 0][(cp - 0x1fbe8) as usize], true),
            0x1fbec..=0x1fbef => self.circle([5, 6, 7, 4][(cp - 0x1fbec) as usize], true),
            0x1cc1b | 0x1cc1c => {
                self.lines(0x44);
                self.rect(
                    w - t,
                    if cp == 0x1cc1b { 0 } else { h / 2 },
                    w,
                    if cp == 0x1cc1b { h / 2 } else { h },
                    255,
                );
            }
            0x1cc1d => {
                self.rect(0, 0, w, t, 255);
                self.rect(0, 0, t, h / 2, 255);
            }
            0x1cc1e => {
                self.rect(0, h - t, w, h, 255);
                self.rect(0, h / 2, t, h, 255);
            }
            0x1cc21..=0x1cc2f => self.separated(cp - 0x1cc20, 2),
            0x1cc30..=0x1cc3f => {
                let i = cp - 0x1cc30;
                let x = (i % 4) as f32;
                let y = (i / 4) as f32;
                let corner = usize::from(x >= 2.0) + 2 * usize::from(y >= 2.0);
                if (1.0..=2.0).contains(&x) && (1.0..=2.0).contains(&y) {
                    self.circle_piece([x - 1.0, y - 1.0], [1.0, 1.0], corner);
                } else {
                    self.circle_piece([x, y], [2.0, 2.0], corner);
                }
            }
            0x1ce00 => {
                self.circle(0, false);
                self.circle(2, false);
            }
            0x1ce01 => {
                self.circle(1, false);
                self.circle(3, false);
            }
            0x1ce0b | 0x1ce0c => {
                let right = cp == 0x1ce0c;
                let x = f32::from(right);
                let side = usize::from(right);
                self.circle_piece([x, 0.0], [1.0, 0.5], side);
                self.circle_piece([x, 0.0], [1.0, 0.5], side + 2);
            }
            0x1ce16..=0x1ce19 => {
                self.lines(0x11);
                let i = cp - 0x1ce16;
                self.rect(
                    if i < 2 { w / 2 } else { 0 },
                    if i.is_multiple_of(2) { 0 } else { h - t },
                    if i < 2 { w } else { w / 2 },
                    if i.is_multiple_of(2) { t } else { h },
                    255,
                );
            }
            0x1ce51..=0x1ce8f => self.separated(cp - 0x1ce50, 3),
            0x1ce90..=0x1ce9f => self.mosaic(1 << (cp - 0x1ce90), 4, 4),
            0x1cea0..=0x1ceaf => {
                let [x0, x1, y0, y1] = [
                    [2, 4, 3, 4],
                    [1, 4, 3, 4],
                    [0, 3, 3, 4],
                    [0, 2, 3, 4],
                    [0, 1, 2, 4],
                    [0, 1, 1, 4],
                    [0, 1, 0, 3],
                    [0, 1, 0, 2],
                    [0, 2, 0, 1],
                    [0, 3, 0, 1],
                    [1, 4, 0, 1],
                    [2, 4, 0, 1],
                    [3, 4, 0, 2],
                    [3, 4, 0, 3],
                    [3, 4, 1, 4],
                    [3, 4, 2, 4],
                ][(cp - 0x1cea0) as usize];
                self.fraction(
                    x0 as f32 / 4.0,
                    y0 as f32 / 4.0,
                    x1 as f32 / 4.0,
                    y1 as f32 / 4.0,
                    255,
                );
            }
            _ => unreachable!("legacy contains matches draw ranges"),
        }
    }

    fn aligned_block(&mut self, align: [u8; 2], size: [f32; 2], alpha: u8) {
        let w = (self.w as f32 * size[0]).round() as i32;
        let h = (self.h as f32 * size[1]).round() as i32;
        let x = (self.w - w) * i32::from(align[0]) / 2;
        let y = (self.h - h) * i32::from(align[1]) / 2;
        self.rect(x, y, x + w, y + h, alpha);
    }
    fn invert(&mut self) {
        for pixel in self.pixmap.data_mut().as_chunks_mut::<4>().0 {
            *pixel = [255 - pixel[3]; 4];
        }
    }
    fn edge_triangle(&mut self, edge: u32) {
        let (w, h) = (self.w as f32, self.h as f32);
        let pair = match edge {
            0 => [[0.0, 0.0], [0.0, h]],
            1 => [[w, 0.0], [0.0, 0.0]],
            2 => [[w, h], [w, 0.0]],
            _ => [[0.0, h], [w, h]],
        };
        self.polygon(
            &[[(w / 2.0).round(), (h / 2.0).round()], pair[0], pair[1]],
            255,
            false,
        );
    }
    fn corner_lines(&mut self, mask: u8) {
        let (w, h) = (self.w as f32, self.h as f32);
        let (cx, cy) = ((w / 2.0).ceil(), (h / 2.0).ceil());
        for i in 0..4 {
            if mask & (1 << i) != 0 {
                self.line(
                    [cx, if i & 2 == 0 { 0.0 } else { h }],
                    [if i & 1 == 0 { 0.0 } else { w }, cy],
                );
            }
        }
    }
    fn cell_diagonals(&mut self, index: u32) {
        let single = [
            [[2, 1], [0, 2]],
            [[2, 0], [0, 1]],
            [[0, 0], [2, 1]],
            [[0, 1], [2, 2]],
            [[0, 0], [1, 2]],
            [[1, 0], [2, 2]],
            [[2, 0], [1, 2]],
            [[1, 0], [0, 2]],
        ];
        let double = [
            [[0, 0], [1, 1], [2, 0]],
            [[2, 0], [1, 1], [2, 2]],
            [[0, 2], [1, 1], [2, 2]],
            [[0, 0], [1, 1], [0, 2]],
            [[0, 0], [1, 2], [2, 0]],
            [[2, 0], [0, 1], [2, 2]],
            [[0, 2], [1, 0], [2, 2]],
            [[0, 0], [2, 1], [0, 2]],
        ];
        let scale = |p: [i32; 2]| {
            [
                p[0] as f32 * self.w as f32 / 2.0,
                p[1] as f32 * self.h as f32 / 2.0,
            ]
        };
        if index < 8 {
            let [a, b] = single[index as usize];
            self.line(scale(a), scale(b));
        } else {
            let [a, b, c] = double[(index - 8) as usize];
            let [a, b, c] = [scale(a), scale(b), scale(c)];
            self.line(a, b);
            self.line(b, c);
        }
    }
    fn checkerboard(&mut self, parity: i32) {
        let rows = (4.0 * self.h as f32 / self.w as f32).round().max(1.0) as i32;
        for x in 0..4 {
            for y in 0..rows {
                if (x + y) % 2 == parity {
                    self.rect(
                        self.w * x / 4,
                        self.h * y / rows,
                        self.w * (x + 1) / 4,
                        self.h * (y + 1) / rows,
                        255,
                    );
                }
            }
        }
    }
    fn circle(&mut self, position: usize, filled: bool) {
        let [x, y] = [
            [0.0, 0.5],
            [0.5, 0.0],
            [1.0, 0.5],
            [0.5, 1.0],
            [0.0, 0.0],
            [1.0, 0.0],
            [0.0, 1.0],
            [1.0, 1.0],
        ][position];
        let radius = 0.5 * self.w.min(self.h) as f32;
        let path = PathBuilder::from_circle(
            x * self.w as f32,
            y * self.h as f32,
            if filled {
                radius
            } else {
                (radius - self.t as f32 / 2.0).max(0.0)
            },
        );
        if filled {
            self.fill(path, 255);
        } else {
            self.stroke(path, self.t as f32, false);
        }
    }
    fn separated(&mut self, mask: u32, rows: i32) {
        let gap = (self.w / 12).max(1);
        let gx = gap * 2 + self.w % 2;
        let gy = gap * 2 + (self.h % rows) / (rows - 1);
        let w = (self.w - 2 * gap - gx) / 2;
        let h = (self.h - 2 * gap - (rows - 1) * gy).div_euclid(rows);
        let middle = self.h - 2 * gap - 2 * gy - 2 * h;
        for row in 0..rows {
            for col in 0..2 {
                if mask & (1 << (row * 2 + col)) != 0 {
                    let y = gap + row * (h + gy) + if row == 2 { middle - h } else { 0 };
                    self.rect(
                        gap + col * (w + gx),
                        y,
                        gap + col * (w + gx) + w,
                        y + if rows == 3 && row == 1 { middle } else { h },
                        255,
                    );
                }
            }
        }
    }
    fn circle_piece(&mut self, origin: [f32; 2], radius: [f32; 2], corner: usize) {
        let (w, h) = (self.w as f32 * radius[0], self.h as f32 * radius[1]);
        let (x, y) = (self.w as f32 * origin[0], self.h as f32 * origin[1]);
        let c = (std::f32::consts::SQRT_2 - 1.0) * 4.0 / 3.0;
        let (cw, ch, ht) = (c * w, c * h, self.t as f32 / 2.0);
        let mut p = PathBuilder::new();
        match corner {
            0 => {
                p.move_to(w - x, ht - y);
                p.cubic_to(w - cw - x, ht - y, ht - x, h - ch - y, ht - x, h - y);
            }
            1 => {
                p.move_to(w - x, ht - y);
                p.cubic_to(
                    w + cw - x,
                    ht - y,
                    w * 2.0 - ht - x,
                    h - ch - y,
                    w * 2.0 - ht - x,
                    h - y,
                );
            }
            2 => {
                p.move_to(ht - x, h - y);
                p.cubic_to(
                    ht - x,
                    h + ch - y,
                    w - cw - x,
                    h * 2.0 - ht - y,
                    w - x,
                    h * 2.0 - ht - y,
                );
            }
            _ => {
                p.move_to(w * 2.0 - ht - x, h - y);
                p.cubic_to(
                    w * 2.0 - ht - x,
                    h + ch - y,
                    w + cw - x,
                    h * 2.0 - ht - y,
                    w - x,
                    h * 2.0 - ht - y,
                );
            }
        }
        self.stroke(p.finish(), self.t as f32, false);
    }
}

#[cfg(test)]
mod tests {
    use crate::{FontMetrics, sprite::rasterize};

    #[test]
    fn legacy_rectangles_match_ghostty_reference_atlases() {
        type AtlasCase = (u32, &'static [u8], &'static [[u32; 2]]);
        let cases: &[AtlasCase] = &[
            (
                0x1fb00,
                include_bytes!("../../../../src/font/sprite/testdata/U+1FB00...U+1FBFF-9x17+1.png"),
                &[
                    [0x1fb00, 0x1fb3b],
                    [0x1fb70, 0x1fb97],
                    [0x1fbaf, 0x1fbaf],
                    [0x1fbce, 0x1fbcf],
                    [0x1fbe4, 0x1fbe7],
                ],
            ),
            (
                0x1cc00,
                include_bytes!("../../../../src/font/sprite/testdata/U+1CC00...U+1CCFF-9x17+1.png"),
                &[[0x1cc1b, 0x1cc1e], [0x1cc21, 0x1cc2f]],
            ),
            (
                0x1cd00,
                include_bytes!("../../../../src/font/sprite/testdata/U+1CD00...U+1CDFF-9x17+1.png"),
                &[[0x1cd00, 0x1cde5]],
            ),
            (
                0x1ce00,
                include_bytes!("../../../../src/font/sprite/testdata/U+1CE00...U+1CEFF-9x17+1.png"),
                &[[0x1ce16, 0x1ce19], [0x1ce51, 0x1ceaf]],
            ),
        ];
        let metrics = FontMetrics {
            cell_width: 9,
            cell_height: 17,
            baseline: 15.0,
            underline_position: 1.0,
            underline_thickness: 1.0,
        };
        for (base, data, ranges) in cases {
            let mut decoder = png::Decoder::new(std::io::Cursor::new(data))
                .read_info()
                .unwrap();
            let mut pixels = vec![0; decoder.output_buffer_size().unwrap()];
            let info = decoder.next_frame(&mut pixels).unwrap();
            assert_eq!(info.color_type, png::ColorType::Grayscale);
            for [start, end] in *ranges {
                for cp in *start..=*end {
                    let bitmap = rasterize(char::from_u32(cp).unwrap(), metrics, 1)
                        .unwrap()
                        .unwrap();
                    let index = cp - base;
                    for y in 0..17 {
                        for x in 0..9 {
                            let rx = index % 16 * 13 + 2 + x;
                            let ry = index / 16 * 25 + 4 + y;
                            assert_eq!(
                                bitmap.pixels[(y * 9 + x) as usize],
                                pixels[(ry * info.width + rx) as usize],
                                "U+{cp:05x} at {x},{y}"
                            );
                        }
                    }
                }
            }
        }
    }
}
