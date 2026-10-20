//! Retained SIXEL input, following tmux's image-sixel.c decoder and grid rules.
// Copyright (c) 2019 Nicholas Marriott <nicholas.marriott@gmail.com>
// Copyright (c) 2026 Michael Grant <mgrant@grant.org>
//
// Permission to use, copy, modify, and distribute this software for any
// purpose with or without fee is hereby granted, provided that the above
// copyright notice and this permission notice appear in all copies.
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
// WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
// MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
// ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION
// OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN
// CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.

use super::{Graphics, Image, Placement, PlacementId, pixels};
use crate::Terminal;

#[derive(Default)]
struct Decoder {
    width: usize,
    lines: Vec<Vec<u16>>,
    allocated: usize,
    palette: Vec<[u8; 3]>,
    x: usize,
    y: usize,
    color: u16,
    limit: usize,
}
impl Decoder {
    fn height(&mut self, height: usize) -> Option<()> {
        let height = height.max(self.lines.len());
        if height > 10000 || self.width.checked_mul(height)? > self.limit / 4 {
            return None;
        }
        self.lines.resize_with(height, Vec::new);
        Some(())
    }
    fn column(&mut self, pattern: u8) -> Option<()> {
        for bit in 0..6 {
            if pattern & (1 << bit) == 0 {
                continue;
            }
            let y = self.y.checked_add(bit)?;
            self.height(y.checked_add(1)?)?;
            let width = self.width.max(self.x.checked_add(1)?);
            if width > 10000 || width.checked_mul(self.lines.len())? > self.limit / 4 {
                return None;
            }
            let line = &mut self.lines[y];
            if self.x >= line.len() {
                self.allocated = self.allocated.checked_add((width - line.len()) * 2)?;
                if self.allocated > self.limit / 2 {
                    return None;
                }
                self.width = width;
                line.resize(width, 0);
            }
            line[self.x] = self.color;
        }
        self.x = self.x.checked_add(1)?;
        Some(())
    }
    fn attributes(&mut self, fields: &[u8]) -> Option<()> {
        let fields = fields.split(|&b| b == b';').take(6).collect::<Vec<_>>();
        if fields.len() <= 2 {
            return Some(());
        }
        if fields.len() != 4 {
            return None;
        }
        let width = number(fields[2])? as usize;
        let height = number(fields[3])? as usize;
        if width > 10000 {
            return None;
        }
        self.width = width;
        self.height(height)
    }
    fn color(&mut self, fields: &[u8]) -> Option<()> {
        let fields = fields.split(|&b| b == b';').take(6).collect::<Vec<_>>();
        let color = number(fields[0])? as usize;
        if color > 1024 {
            return None;
        }
        self.color = color as u16 + 1;
        if fields.len() == 1 {
            return Some(());
        }
        if fields.len() != 5 {
            return None;
        }
        let kind = number(fields[1])?;
        let a = number(fields[2])?;
        let b = number(fields[3])?;
        let c = number(fields[4])?;
        if b > 100 || c > 100 {
            return None;
        }
        let rgb = match kind {
            2 if a <= 100 => [a, b, c].map(|v| ((v * 255 + 50) / 100) as u8),
            1 if a <= 360 => hls(a, b, c),
            _ => return None,
        };
        self.palette
            .resize(self.palette.len().max(color + 1), [0; 3]);
        self.palette[color] = rgb;
        Some(())
    }
}
fn number(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0u32, |n, &b| {
        b.is_ascii_digit().then_some(())?;
        n.checked_mul(10)?.checked_add(u32::from(b - b'0'))
    })
}
fn hls(h: u32, l: u32, s: u32) -> [u8; 3] {
    let (h, l, s) = (
        f64::from(h) / 360.,
        f64::from(l) / 100.,
        f64::from(s) / 100.,
    );
    if s == 0. {
        return [(l * 255. + 0.5) as u8; 3];
    }
    let q = if l < 0.5 { l * (1. + s) } else { l + s - l * s };
    let p = 2. * l - q;
    let component = |mut t: f64| {
        if t < 0. {
            t += 1.;
        }
        if t > 1. {
            t -= 1.;
        }
        let value = if t < 1. / 6. {
            p + (q - p) * 6. * t
        } else if t < 0.5 {
            q
        } else if t < 2. / 3. {
            p + (q - p) * (2. / 3. - t) * 6.
        } else {
            p
        };
        (value * 255. + 0.5) as u8
    };
    [component(h), component(h - 1. / 3.), component(h + 1. / 3.)]
}
fn decode(mut bytes: &[u8], transparent: bool, limit: usize) -> Option<(u32, u32, Vec<u8>)> {
    let mut d = Decoder {
        limit,
        ..Default::default()
    };
    while let Some((&ch, rest)) = bytes.split_first() {
        bytes = rest;
        match ch {
            b'"' | b'#' => {
                let end = bytes
                    .iter()
                    .position(|b| !b.is_ascii_digit() && *b != b';')
                    .unwrap_or(bytes.len());
                let fields = &bytes[..end];
                bytes = &bytes[end..];
                if ch == b'"' {
                    d.attributes(fields)?;
                } else {
                    d.color(fields)?;
                }
            }
            b'!' => {
                let end = bytes.iter().position(|b| !b.is_ascii_digit())?;
                if end >= 31 {
                    return None;
                }
                let count = number(&bytes[..end])?;
                if !(1..=10000).contains(&count) {
                    return None;
                }
                let pattern = bytes[end].wrapping_sub(b'?');
                for _ in 0..count {
                    d.column(pattern)?;
                }
                bytes = &bytes[end + 1..];
            }
            b'-' => {
                d.x = 0;
                d.y = d.y.checked_add(6)?;
            }
            b'$' => d.x = 0,
            b'?'..=b'~' => d.column(ch - b'?')?,
            _ => (),
        }
    }
    if d.width == 0 || d.lines.is_empty() || d.width.checked_mul(d.lines.len())? > limit / 4 {
        return None;
    }
    let mut rgba = vec![0; d.width * d.lines.len() * 4];
    for (y, line) in d.lines.iter().enumerate() {
        for x in 0..d.width {
            let color = line.get(x).copied().unwrap_or(0);
            let at = (y * d.width + x) * 4;
            if color == 0 {
                rgba[at + 3] = if transparent { 0 } else { 255 };
            } else {
                rgba[at..at + 3]
                    .copy_from_slice(d.palette.get(usize::from(color - 1)).unwrap_or(&[0; 3]));
                rgba[at + 3] = 255;
            }
        }
    }
    Some((d.width as u32, d.lines.len() as u32, rgba))
}

impl Terminal {
    pub(crate) fn sixel_command(&mut self, data: &[u8], transparent: bool) {
        if self.graphics().limit == 0 {
            return;
        }
        let Some((width, height, rgba)) = decode(data, transparent, self.tmux_image_limit) else {
            return;
        };
        self.prune_tmux_images();
        let cell = [
            (self.width_px / u32::from(self.cols)).max(1),
            (self.height_px / u32::from(self.rows)).max(1),
        ];
        let cell = [
            if self.width_px == 0 { 8 } else { cell[0] },
            if self.height_px == 0 { 16 } else { cell[1] },
        ];
        let columns = width.div_ceil(cell[0]);
        let rows = height.div_ceil(cell[1]);
        if u64::from(columns) * u64::from(rows) > self.tmux_image_limit as u64 {
            return;
        }
        let storage = &mut self.screen_mut().graphics;
        let token = if let Some(quota) = &storage.quota {
            let result = quota.reserve(storage.quota_owner, None, rgba.len());
            storage.prune_quota();
            let Ok(token) = result else {
                return;
            };
            Some(token)
        } else {
            if storage.reserve(rgba.len(), 0).is_err() {
                return;
            }
            None
        };
        let id = storage.allocate_tmux_id();
        storage.generation = storage.generation.wrapping_add(1);
        let image = Image {
            id,
            tmux_unmapped: true,
            tmux_sixel_cell: Some(cell),
            tmux_order: Some(token.unwrap_or(storage.generation)),
            number: 0,
            width,
            height,
            pixels: pixels::intern(width, height, rgba),
            generation: storage.generation,
            identity: storage.generation,
            frames: Vec::new(),
            current_frame: 0,
            root_gap_ms: 0,
            animation_state: 1,
            max_loops: 0,
            completed_loops: 0,
            frame_shown_at_ms: None,
        };
        storage.images.insert(id, image.clone());
        if let Some(token) = token {
            storage.quota_images.insert(id, (token, storage.generation));
        }
        let col = self.screen().cursor.col;
        for y in 0..rows {
            let row = self.screen().row(self.screen().cursor.row).id;
            self.screen_mut()
                .graphics
                .sixel_row(&image, row, col, y, cell);
            self.index();
        }
        self.screen_mut().cursor.col = 0;
        self.screen_mut().cursor.pending_wrap = false;
        self.synchronize_tmux_sources();
        self.prune_tmux_images();
        self.generation = self.generation.wrapping_add(1);
    }
}

impl Graphics {
    fn sixel_row(&mut self, image: &Image, row: u64, col: usize, y: u32, cell: [u32; 2]) {
        let height = cell[1].min(image.height - y * cell[1]);
        let columns = image.width.div_ceil(cell[0]);
        let mut run = None;
        for x in 0..=columns {
            let visible = x < columns
                && (0..height).any(|yy| {
                    (x * cell[0]..image.width.min((x + 1) * cell[0])).any(|xx| {
                        image.pixels[(((y * cell[1] + yy) * image.width + xx) * 4 + 3) as usize]
                            != 0
                    })
                });
            if visible {
                run.get_or_insert(x);
                let at = col + x as usize;
                let covered = self
                    .placements
                    .iter()
                    .filter(|old| {
                        if old.row != row || old.col > at {
                            return false;
                        }
                        let Some(old_image) = self
                            .images
                            .get(&old.image_id)
                            .filter(|i| i.tmux_sixel_cell == Some(cell))
                        else {
                            return false;
                        };
                        let [ox, oy, ow, oh] = old.source_rect(old_image);
                        let offset = (at - old.col) as u32 * cell[0];
                        if offset >= ow {
                            return false;
                        }
                        let old_width = cell[0].min(ow - offset);
                        let new_width = cell[0].min(image.width - x * cell[0]);
                        (0..oh).all(|yy| {
                            (0..old_width).all(|xx| {
                                old_image.pixels[(((oy + yy) * old_image.width + ox + offset + xx)
                                    * 4
                                    + 3) as usize]
                                    == 0
                                    || (xx < new_width
                                        && yy < height
                                        && image.pixels[(((y * cell[1] + yy) * image.width
                                            + x * cell[0]
                                            + xx)
                                            * 4
                                            + 3)
                                            as usize]
                                            == 255)
                            })
                        })
                    })
                    .map(|p| (p.image_id, p.placement_id))
                    .collect::<Vec<_>>();
                for key in covered {
                    self.cut_sixel(key, at, at + 1);
                }
            } else if let Some(start) = run.take() {
                let placement_id = PlacementId::Internal(self.next_placement);
                self.next_placement = self.next_placement.wrapping_add(1);
                self.placements.insert(Placement {
                    image_id: image.id,
                    placement_id,
                    row,
                    col: col + start as usize,
                    columns: 0,
                    rows: 0,
                    viewport_row: None,
                    z: 0,
                    source: [
                        start * cell[0],
                        y * cell[1],
                        image.width.min(x * cell[0]) - start * cell[0],
                        height,
                    ],
                    offset: [0; 2],
                    virtual_placement: false,
                    parent: None,
                    parent_offset: [0; 2],
                });
            }
        }
        self.generation = self.generation.wrapping_add(1);
    }
    fn cut_sixel(&mut self, key: (u32, PlacementId), start: usize, end: usize) {
        let Some(mut p) = self.placements.remove(key) else {
            return;
        };
        let image = &self.images[&p.image_id];
        let cell = image.tmux_sixel_cell.unwrap()[0];
        let [x, y, width, height] = p.source_rect(image);
        let last = p.col + width.div_ceil(cell) as usize;
        if start >= last || end <= p.col {
            self.placements.insert(p);
            return;
        }
        if p.col < start {
            let mut prefix = p.clone();
            prefix.source = [x, y, width.min((start - p.col) as u32 * cell), height];
            self.placements.insert(prefix);
            p.placement_id = PlacementId::Internal(self.next_placement);
            self.next_placement = self.next_placement.wrapping_add(1);
        }
        if last > end {
            let skip = (end - p.col) as u32 * cell;
            p.col = end;
            p.source = [x + skip, y, width - skip, height];
            self.placements.insert(p);
        }
    }
    pub(crate) fn overwrite_sixel(&mut self, row: u64, start: usize, end: usize) {
        let keys = self
            .placements
            .iter()
            .filter(|p| {
                p.row == row
                    && self
                        .images
                        .get(&p.image_id)
                        .is_some_and(|i| i.tmux_sixel_cell.is_some())
            })
            .map(|p| (p.image_id, p.placement_id))
            .collect::<Vec<_>>();
        if keys.is_empty() {
            return;
        }
        for key in keys {
            self.cut_sixel(key, start, end);
        }
        self.generation = self.generation.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decoder_bounds_palette_and_background() {
        let (_, _, pixels) = decode(b"\"1;1;2;1#0;1;120;50;100@", true, 8).unwrap();
        assert_eq!(pixels, [255, 0, 0, 255, 0, 0, 0, 0]);
        assert_eq!(decode(b"\"1;1;1;1", false, 4).unwrap().2, [0, 0, 0, 255]);
        for invalid in [
            b"!0~".as_slice(),
            b"!10001~",
            b"#1025~",
            b"#0;2;101;0;0~",
            b"\"1;1;10001;1",
            b"\"1;1;1;10001",
            b"\"1;1;32;33",
            b"\"1;1;18446744073709551616;1",
        ] {
            assert!(decode(invalid, true, 4096).is_none(), "{invalid:?}");
        }
    }
}
