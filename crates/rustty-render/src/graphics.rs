//! Kitty placements become the same portable textured quads as colored glyphs.
use super::{CachedGlyph, Color, Quad, RenderError, RenderOptions, Renderer};
use crate::Paint;
use rustty_font::FontMetrics;
use rustty_vt::{
    Screen,
    graphics::{Image, Placement},
    screen::{Cell, Color as TerminalColor},
};
use std::{collections::HashMap, sync::Arc};

pub(super) const PLACEHOLDER: char = '\u{10eeee}';
// One neighboring pixel on each side prevents seams under bilinear filtering;
// the atlas then adds its own one-pixel gutter, fitting a 1024px page.
const TILE: u32 = 1020;
pub(super) type TileKey = (usize, u32, u32, u32, u32);
pub(super) struct CachedTile {
    // Retain the source so allocator reuse cannot collide with a cached pointer.
    _source: Arc<[u8]>,
    glyph: CachedGlyph,
}

struct Geometry {
    image: u32,
    placement: u32,
    z: i32,
    source: [f32; 4],
    rect: [f32; 4],
}

impl Renderer {
    pub(super) fn prepare_graphics(
        &mut self,
        screen: &Screen,
        options: &RenderOptions,
        omit_excess: bool,
    ) -> Result<[Vec<Quad>; 3], RenderError> {
        let mut layers: [Vec<Quad>; 3] = Default::default();
        if screen.graphics.placements.is_empty() {
            return Ok(layers);
        }
        let metrics = self.metrics();
        let mut placements = geometry(screen, metrics, options);
        placements.sort_by_key(|p| (p.z, p.image, p.placement));
        let clip = [
            options.padding[0],
            options.padding[1],
            (screen.rows.first().map_or(0, |r| r.cells.len()) as f32 * metrics.cell_width as f32)
                .min((options.size[0] as f32 - 2.0 * options.padding[0]).max(0.0)),
            (screen.rows.len() as f32 * metrics.cell_height as f32)
                .min((options.size[1] as f32 - 2.0 * options.padding[1]).max(0.0)),
        ];
        for mut placement in placements {
            let Some(image) = screen.graphics.images.get(&placement.image) else {
                continue;
            };
            if !clip_image(&mut placement.source, &mut placement.rect, clip) {
                continue;
            }
            let layer = if placement.z < i32::MIN / 2 {
                0
            } else if placement.z < 0 {
                1
            } else {
                2
            };
            let source = placement.source;
            let [sx, sy, sw, sh] = source;
            let [x, y, w, h] = placement.rect;
            for ty in (sy as u32 / TILE)..=((sy + sh).ceil() as u32).saturating_sub(1) / TILE {
                for tx in (sx as u32 / TILE)..=((sx + sw).ceil() as u32).saturating_sub(1) / TILE {
                    let cached = match self.image_tile(image, tx, ty) {
                        Ok(cached) => cached,
                        Err(RenderError::AtlasCapacity) if omit_excess => {
                            // ponytail: a 320 MiB image atlas is separate from text;
                            // omit excess visible tiles after eviction rather than lose terminal text.
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    let left = sx.max((tx * TILE) as f32);
                    let top = sy.max((ty * TILE) as f32);
                    let right = (sx + sw).min((tx * TILE + cached.size[0]) as f32);
                    let bottom = (sy + sh).min((ty * TILE + cached.size[1]) as f32);
                    if left >= right || top >= bottom {
                        continue;
                    }
                    let [u0, v0, u1, v1] = cached.uv;
                    let uvx = |pixel| {
                        u0 + (u1 - u0) * (pixel - (tx * TILE) as f32) / cached.size[0] as f32
                    };
                    let uvy = |pixel| {
                        v0 + (v1 - v0) * (pixel - (ty * TILE) as f32) / cached.size[1] as f32
                    };
                    layers[layer].push(Quad {
                        rect: [
                            x + (left - sx) / sw * w,
                            y + (top - sy) / sh * h,
                            (right - left) / sw * w,
                            (bottom - top) / sh * h,
                        ],
                        uv: [uvx(left), uvy(top), uvx(right), uvy(bottom)],
                        color: Color::rgb([255; 3]),
                        paint: Paint::Color,
                        atlas: cached.atlas,
                    });
                }
            }
        }
        Ok(layers)
    }

    fn image_tile(&mut self, image: &Image, tx: u32, ty: u32) -> Result<CachedGlyph, RenderError> {
        let pixels = image.displayed_pixels();
        let key = (pixels.as_ptr() as usize, image.width, image.height, tx, ty);
        if let Some(tile) = self.images.get(&key) {
            return Ok(tile.glyph.clone());
        }
        let width = TILE.min(image.width.saturating_sub(tx * TILE));
        let height = TILE.min(image.height.saturating_sub(ty * TILE));
        let mut data = Vec::with_capacity((width as usize + 2) * (height as usize + 2) * 4);
        for y in -1..=height as i32 {
            let y = (i64::from(ty * TILE) + i64::from(y)).clamp(0, i64::from(image.height) - 1)
                as usize;
            for x in -1..=width as i32 {
                let x = (i64::from(tx * TILE) + i64::from(x)).clamp(0, i64::from(image.width) - 1)
                    as usize;
                let index = (y * image.width as usize + x) * 4;
                data.extend_from_slice(&pixels[index..index + 4]);
            }
        }
        let mut cached = self.cache_pixels([width + 2, height + 2], data.into(), true)?;
        let step = 1.0 / self.pages[cached.atlas].size as f32;
        cached.uv[0] += step;
        cached.uv[1] += step;
        cached.uv[2] -= step;
        cached.uv[3] -= step;
        cached.size = [width, height];
        self.images.insert(
            key,
            CachedTile {
                _source: pixels,
                glyph: cached.clone(),
            },
        );
        Ok(cached)
    }
}

fn geometry(screen: &Screen, metrics: FontMetrics, options: &RenderOptions) -> Vec<Geometry> {
    let cell = [metrics.cell_width as f32, metrics.cell_height as f32];
    let mut result = Vec::new();
    let mut virtual_origins: HashMap<(u32, u32), [i64; 2]> = HashMap::new();
    for (row_index, row) in screen.viewport().enumerate() {
        let mut run: Option<Run> = None;
        for (col, cell) in row.cells.iter().enumerate() {
            let current = Placeholder::from_cell(cell);
            if let (Some(previous), Some(next)) = (run.as_mut(), current.as_ref())
                && previous.can_append(next)
            {
                previous.width += 1;
                continue;
            }
            if let Some(previous) = run.take() {
                virtual_geometry(
                    previous,
                    row_index,
                    screen,
                    metrics,
                    options,
                    &mut result,
                    &mut virtual_origins,
                );
            }
            if let Some(current) = current {
                run = Some(Run::new(current, col));
            }
        }
        if let Some(previous) = run {
            virtual_geometry(
                previous,
                row_index,
                screen,
                metrics,
                options,
                &mut result,
                &mut virtual_origins,
            );
        }
    }
    let index: HashMap<_, _> = screen
        .graphics
        .placements
        .iter()
        .map(|p| ((p.image_id, p.placement_id), p))
        .collect();
    let first = screen.history.len().saturating_sub(screen.viewport_offset) as i64;
    let rows: HashMap<_, _> = if screen
        .graphics
        .placements
        .iter()
        .any(|p| p.viewport_row.is_none())
    {
        screen
            .all_rows()
            .enumerate()
            .map(|(i, r)| (r.id, i as i64 - first))
            .collect()
    } else {
        HashMap::new()
    };
    for placement in &screen.graphics.placements {
        if placement.virtual_placement {
            continue;
        }
        let Some(image) = screen.graphics.images.get(&placement.image_id) else {
            continue;
        };
        if !valid_image(image) {
            continue;
        }
        let mut p = placement;
        let mut offset = [0i64; 2];
        let mut depth = 0;
        while let Some(parent) = p.parent {
            offset[0] = offset[0].saturating_add(i64::from(p.parent_offset[0]));
            offset[1] = offset[1].saturating_add(i64::from(p.parent_offset[1]));
            let Some(next) = index.get(&parent) else {
                break;
            };
            p = next;
            depth += 1;
            if depth > 64 {
                break;
            }
        }
        if p.parent.is_some() {
            continue;
        }
        let origin = if p.virtual_placement {
            let Some(origin) = virtual_origins.get(&(p.image_id, p.placement_id)) else {
                continue;
            };
            *origin
        } else {
            let Some(row) = p.viewport_row.or_else(|| rows.get(&p.row).copied()) else {
                continue;
            };
            if row == i64::MIN {
                continue;
            }
            [p.col as i64, row]
        };
        let source = clipped_source(placement, image);
        if source[2] <= 0.0 || source[3] <= 0.0 {
            continue;
        }
        let shift = [
            placement.offset[0].min(metrics.cell_width - 1) as f32,
            placement.offset[1].min(metrics.cell_height - 1) as f32,
        ];
        let size = pixel_size(
            placement.requested_size,
            [source[2], source[3]],
            cell,
            shift,
        );
        result.push(Geometry {
            image: image.id,
            placement: placement.placement_id,
            z: placement.z,
            source,
            rect: [
                options.padding[0]
                    + origin[0].saturating_add(offset[0]) as f32 * cell[0]
                    + shift[0],
                options.padding[1]
                    + origin[1].saturating_add(offset[1]) as f32 * cell[1]
                    + shift[1],
                size[0],
                size[1],
            ],
        });
    }
    result
}

fn valid_image(image: &Image) -> bool {
    image.width > 0
        && image.height > 0
        && image.width <= 10000
        && image.height <= 10000
        && image.current_frame <= image.frames.len()
        && image.display_pixels().len() == image.width as usize * image.height as usize * 4
}

fn clipped_source(p: &Placement, image: &Image) -> [f32; 4] {
    let x = p.source[0].min(image.width);
    let y = p.source[1].min(image.height);
    [
        x as f32,
        y as f32,
        p.source[2].min(image.width - x) as f32,
        p.source[3].min(image.height - y) as f32,
    ]
}

fn pixel_size(requested: [u32; 2], source: [f32; 2], cell: [f32; 2], offset: [f32; 2]) -> [f32; 2] {
    let [cols, rows] = requested;
    let width =
        ((cols as f64 * cell[0] as f64).min(u32::MAX as f64) - offset[0] as f64).max(0.0) as f32;
    let height =
        ((rows as f64 * cell[1] as f64).min(u32::MAX as f64) - offset[1] as f64).max(0.0) as f32;
    let scale = |value: f32, numerator: f32, denominator: f32| {
        (value as f64 * numerator as f64 / denominator as f64)
            .round()
            .min(u32::MAX as f64) as f32
    };
    match (cols, rows) {
        (0, 0) => source,
        (_, 0) => [width, scale(width, source[1], source[0])],
        (0, _) => [scale(height, source[0], source[1]), height],
        _ => [width, height],
    }
}

// Intersect in destination coordinates while preserving the source mapping.
fn clip_image(source: &mut [f32; 4], rect: &mut [f32; 4], clip: [f32; 4]) -> bool {
    let [x, y, w, h] = *rect;
    if w <= 0.0 || h <= 0.0 {
        return false;
    }
    let left = x.max(clip[0]);
    let top = y.max(clip[1]);
    let right = (x + w).min(clip[0] + clip[2]);
    let bottom = (y + h).min(clip[1] + clip[3]);
    if left >= right || top >= bottom {
        return false;
    }
    let [sx, sy, sw, sh] = *source;
    *source = [
        sx + (left - x) / w * sw,
        sy + (top - y) / h * sh,
        (right - left) / w * sw,
        (bottom - top) / h * sh,
    ];
    *rect = [left, top, right - left, bottom - top];
    true
}

#[derive(Clone)]
struct Placeholder {
    low: u32,
    high: Option<u8>,
    placement: u32,
    row: Option<u32>,
    col: Option<u32>,
}
impl Placeholder {
    fn from_cell(cell: &Cell) -> Option<Self> {
        let mut chars = cell.text.chars();
        if chars.next()? != PLACEHOLDER {
            return None;
        }
        let mut next = || {
            chars
                .next()
                .and_then(|cp| DIACRITICS.binary_search(&(cp as u32)).ok())
                .map(|i| i as u32)
        };
        Some(Self {
            low: color_id(cell.style.foreground),
            placement: color_id(cell.style.underline_color),
            row: next(),
            col: next(),
            high: next().and_then(|i| u8::try_from(i).ok()),
        })
    }
}
struct Run {
    p: Placeholder,
    col: usize,
    width: u32,
}
impl Run {
    fn new(mut p: Placeholder, col: usize) -> Self {
        p.row.get_or_insert(0);
        p.col.get_or_insert(0);
        Self { p, col, width: 1 }
    }
    fn can_append(&self, p: &Placeholder) -> bool {
        self.p.low == p.low
            && self.p.placement == p.placement
            && p.row.is_none_or(|r| Some(r) == self.p.row)
            && p.col
                .is_none_or(|c| c == self.p.col.unwrap_or(0) + self.width)
            && p.high.is_none_or(|h| Some(h) == self.p.high)
    }
}
fn color_id(c: TerminalColor) -> u32 {
    match c {
        TerminalColor::Default => 0,
        TerminalColor::Indexed(i) => u32::from(i),
        TerminalColor::Rgb(r, g, b) => (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b),
    }
}

#[allow(clippy::too_many_arguments)]
fn virtual_geometry(
    run: Run,
    row: usize,
    screen: &Screen,
    metrics: FontMetrics,
    options: &RenderOptions,
    result: &mut Vec<Geometry>,
    origins: &mut HashMap<(u32, u32), [i64; 2]>,
) {
    let id = run.p.low | (u32::from(run.p.high.unwrap_or(0)) << 24);
    let Some(p) = screen.graphics.placements.iter().find(|p| {
        p.virtual_placement
            && p.image_id == id
            && (run.p.placement == 0 || run.p.placement == p.placement_id)
    }) else {
        return;
    };
    let Some(image) = screen.graphics.images.get(&id).filter(|i| valid_image(i)) else {
        return;
    };
    origins
        .entry((id, p.placement_id))
        .and_modify(|o| {
            o[0] = o[0].min(run.col as i64);
            o[1] = o[1].min(row as i64);
        })
        .or_insert([run.col as i64, row as i64]);
    let cols = if p.requested_size[0] == 0 {
        image.width.div_ceil(metrics.cell_width)
    } else {
        p.requested_size[0]
    };
    let rows = if p.requested_size[1] == 0 {
        image.height.div_ceil(metrics.cell_height)
    } else {
        p.requested_size[1]
    };
    let [col, img_row] = [run.p.col.unwrap_or(0), run.p.row.unwrap_or(0)];
    if col >= cols || img_row >= rows {
        return;
    }
    let cell = [metrics.cell_width as f32, metrics.cell_height as f32];
    let grid = [cols as f32 * cell[0], rows as f32 * cell[1]];
    let scale = (grid[0] / image.width as f32).min(grid[1] / image.height as f32);
    let size = [image.width as f32 * scale, image.height as f32 * scale];
    let start = [
        options.padding[0] + run.col as f32 * cell[0],
        options.padding[1] + row as f32 * cell[1],
    ];
    let mut rect = [
        start[0] - col as f32 * cell[0] + (grid[0] - size[0]) / 2.0,
        start[1] - img_row as f32 * cell[1] + (grid[1] - size[1]) / 2.0,
        size[0],
        size[1],
    ];
    let mut source = [0.0, 0.0, image.width as f32, image.height as f32];
    if clip_image(
        &mut source,
        &mut rect,
        [
            start[0],
            start[1],
            run.width.min(cols - col) as f32 * cell[0],
            cell[1],
        ],
    ) {
        result.push(Geometry {
            image: id,
            placement: p.placement_id,
            z: -1,
            source,
            rect,
        });
    }
}

include!("kitty_diacritics.rs");

#[cfg(test)]
mod tests {
    use super::*;
    use rustty_font::FontConfig;
    use rustty_vt::{Terminal, graphics::AnimationFrame};

    fn renderer() -> (Renderer, RenderOptions) {
        (
            Renderer::new(FontConfig::default()).unwrap(),
            RenderOptions {
                cursor_visible: false,
                ..Default::default()
            },
        )
    }
    fn transmit(t: &mut Terminal, id: u32, placement: &str) {
        t.feed(
            format!("\x1b_Ga=T,f=32,s=1,v=1,i={id},p={id},C=1,{placement};/wAAgA==\x1b\\")
                .as_bytes(),
        );
        assert!(t.screen().graphics.images.contains_key(&id));
    }

    #[test]
    fn kitty_layers_preserve_straight_alpha_and_native_size() {
        assert_eq!(
            pixel_size([2, 0], [4.0, 2.0], [8.0, 16.0], [1.0, 3.0]),
            [15.0, 8.0]
        );
        assert_eq!(
            pixel_size([0, 2], [4.0, 2.0], [8.0, 16.0], [1.0, 3.0]),
            [58.0, 29.0]
        );
        let mut terminal = Terminal::new(10, 3, 100);
        transmit(&mut terminal, 1, "z=-1073741825");
        transmit(&mut terminal, 2, "z=-1,c=1,r=1");
        transmit(&mut terminal, 3, "z=0,c=1,r=1");
        terminal.feed(b"\x1b[44mA");
        let (mut renderer, options) = renderer();
        let frame = renderer.prepare(terminal.screen(), &options).unwrap();
        let images: Vec<_> = frame
            .quads
            .iter()
            .enumerate()
            .filter(|(_, q)| q.paint == Paint::Color)
            .collect();
        assert_eq!(images.len(), 3);
        assert_eq!(
            images[0].1.rect,
            [options.padding[0], options.padding[1], 1.0, 1.0]
        );
        let background = frame
            .quads
            .iter()
            .position(|q| q.paint == Paint::Solid && q.color == Color::rgb(options.palette[4]))
            .unwrap();
        let text = frame
            .quads
            .iter()
            .position(|q| q.paint == Paint::Mask)
            .unwrap();
        assert!(
            images[0].0 < background
                && background < images[1].0
                && images[1].0 < text
                && text < images[2].0
        );
        for tile in renderer.images.values() {
            let upload = frame
                .atlas_uploads
                .iter()
                .find(|u| u.page == tile.glyph.atlas && u.size == [3, 3])
                .unwrap();
            assert!(
                upload
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| *p == [255, 0, 0, 128])
            );
        }
    }

    #[test]
    fn scrolled_image_roots_survive_viewport_snapshots() {
        let mut terminal = Terminal::new(10, 2, 100);
        transmit(&mut terminal, 1, "c=2,r=3");
        terminal.feed(b"\r\n\r\n");
        let snapshot = terminal.screen().snapshot_viewport();
        assert_eq!(snapshot.graphics.placements[0].viewport_row, Some(-1));
        assert!(snapshot.history.is_empty());
        let (mut renderer, options) = renderer();
        let direct = renderer.prepare(terminal.screen(), &options).unwrap();
        let projected = renderer.prepare(&snapshot, &options).unwrap();
        assert_eq!(direct.quads, projected.quads);
        let image = projected
            .quads
            .iter()
            .find(|q| q.paint == Paint::Color)
            .unwrap();
        assert_eq!(image.rect[1], options.padding[1]);
        assert!((image.rect[3] - renderer.metrics().cell_height as f32 * 2.0).abs() < 0.001);
    }

    #[test]
    fn tiled_images_keep_neighbor_pixels_and_animation_buffers_are_distinct() {
        let mut terminal = Terminal::new(400, 2, 100);
        transmit(&mut terminal, 1, "");
        let image = terminal.screen_mut().graphics.images.get_mut(&1).unwrap();
        image.width = 2041;
        image.pixels = (0..2041)
            .flat_map(|x| [(x % 256) as u8, 0, 0, 255])
            .collect::<Vec<_>>()
            .into();
        image.frames.push(AnimationFrame {
            pixels: vec![255; 2041 * 4].into(),
            gap_ms: 10,
        });
        terminal.screen_mut().graphics.placements[0].source = [0, 0, 2041, 1];
        let (mut renderer, mut options) = renderer();
        options.size = [5000, 100];
        let frame = renderer.prepare(terminal.screen(), &options).unwrap();
        let images: Vec<_> = frame
            .quads
            .iter()
            .filter(|q| q.paint == Paint::Color)
            .collect();
        assert_eq!(images.len(), 3);
        assert_eq!(images.iter().map(|q| q.rect[2]).sum::<f32>(), 2041.0);
        let first = &frame.atlas_uploads[0];
        assert_eq!(&first.pixels[(1021 * 4)..(1022 * 4)], &[252, 0, 0, 255]);
        let old = frame.atlas_uploads.len();
        terminal
            .screen_mut()
            .graphics
            .images
            .get_mut(&1)
            .unwrap()
            .current_frame = 1;
        let animated = renderer.prepare(terminal.screen(), &options).unwrap();
        assert_eq!(animated.atlas_uploads.len(), old + 3);
        assert_eq!(animated.generation, frame.generation);
        assert!(
            animated.atlas_uploads[old..]
                .iter()
                .all(|u| u.pixels.iter().all(|p| *p == 255))
        );
    }

    #[test]
    fn unicode_placeholder_runs_and_relative_children_share_visible_origins() {
        let (mut renderer, options) = renderer();
        let metrics = renderer.metrics();
        let mut terminal = Terminal::new(10, 3, 100);
        transmit(&mut terminal, 1, "U=1,c=2,r=2");
        transmit(&mut terminal, 2, "c=1,r=1,P=1,Q=1,H=3,V=0");
        let image = terminal.screen_mut().graphics.images.get_mut(&1).unwrap();
        image.width = metrics.cell_width * 2;
        image.height = metrics.cell_height * 2;
        image.pixels = vec![255; image.width as usize * image.height as usize * 4].into();
        for row in 0..2 {
            for col in 0..2 {
                let cell = &mut terminal.screen_mut().rows[row].cells[col];
                cell.text = if col == 0 {
                    format!(
                        "{PLACEHOLDER}{}\u{305}",
                        if row == 0 { '\u{305}' } else { '\u{30d}' }
                    )
                } else {
                    PLACEHOLDER.to_string()
                };
                cell.style.foreground = TerminalColor::Indexed(1);
            }
        }
        let frame = renderer.prepare(terminal.screen(), &options).unwrap();
        let images: Vec<_> = frame
            .quads
            .iter()
            .filter(|q| q.paint == Paint::Color)
            .collect();
        assert_eq!(images.len(), 3);
        assert_eq!(
            images[0].rect,
            [
                options.padding[0],
                options.padding[1],
                metrics.cell_width as f32 * 2.0,
                metrics.cell_height as f32
            ]
        );
        assert_eq!(
            images[1].rect[1],
            options.padding[1] + metrics.cell_height as f32
        );
        assert_eq!(
            images[2].rect[0],
            options.padding[0] + metrics.cell_width as f32 * 3.0
        );
        assert!(!frame.quads.iter().any(|q| q.paint == Paint::Mask));
    }
}
