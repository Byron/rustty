//! Kitty placements become the same portable textured quads as colored glyphs.
use super::{CachedGlyph, Color, Quad, RenderError, RenderOptions, Renderer};
use crate::Paint;
use rustty_font::FontMetrics;
use rustty_vt::{
    Screen,
    graphics::{Image, Placement, PlacementId, unicode},
};
use std::{collections::HashMap, sync::Arc};

pub(super) use unicode::PLACEHOLDER;
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
    placement: PlacementId,
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
    let mut virtual_origins: HashMap<(u32, PlacementId), [i64; 2]> = HashMap::new();
    // Like the native renderer, scan placeholders only while a virtual
    // placement exists. Explicit placeholder IDs may target ordinary placements.
    if screen
        .graphics
        .placements
        .iter()
        .any(|p| p.virtual_placement)
    {
        for (row_index, row) in screen.viewport().enumerate() {
            for run in unicode::placements(row) {
                virtual_geometry(
                    run,
                    row_index,
                    screen,
                    metrics,
                    options,
                    &mut result,
                    &mut virtual_origins,
                );
            }
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
        let Some((p, offset)) = placement.resolve_chain(|key| index.get(&key).copied()) else {
            continue;
        };
        let offset = offset.map(i64::from);
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
        let size = placement
            .pixel_size(image, [metrics.cell_width, metrics.cell_height])
            .map(|v| v as f32);
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
    p.source_rect(image).map(|v| v as f32)
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

#[allow(clippy::too_many_arguments)]
fn virtual_geometry(
    run: unicode::Placement,
    row: usize,
    screen: &Screen,
    metrics: FontMetrics,
    options: &RenderOptions,
    result: &mut Vec<Geometry>,
    origins: &mut HashMap<(u32, PlacementId), [i64; 2]>,
) {
    let id = run.image_id;
    let Some(p) = screen.graphics.placeholder_target(id, run.placement_id) else {
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
    let cols = if p.columns == 0 {
        image.width.div_ceil(metrics.cell_width)
    } else {
        p.columns
    };
    let rows = if p.rows == 0 {
        image.height.div_ceil(metrics.cell_height)
    } else {
        p.rows
    };
    let [col, img_row] = [run.image_col, run.image_row];
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

#[cfg(test)]
mod tests {
    use super::*;
    use rustty_font::FontConfig;
    use rustty_vt::{Terminal, graphics::AnimationFrame, screen::Color as TerminalColor};

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
    fn relative_descendants_past_eight_links_are_omitted_after_replacement() {
        let mut terminal = Terminal::new(10, 3, 100);
        transmit(&mut terminal, 1, "c=1,r=1");
        for id in 2..=9 {
            terminal
                .feed(format!("\x1b_Ga=p,i=1,p={id},P=1,Q={},c=1,r=1\x1b\\", id - 1).as_bytes());
        }
        let (renderer, options) = renderer();
        assert_eq!(
            geometry(terminal.screen(), renderer.metrics(), &options).len(),
            9
        );
        terminal.feed(b"\x1b_Ga=p,i=1,p=20,C=1,c=1,r=1\x1b\\");
        terminal.feed(b"\x1b_Ga=p,i=1,p=1,P=1,Q=20,c=1,r=1\x1b\\");
        assert_eq!(terminal.graphics().placements.len(), 10);
        let rendered = geometry(terminal.screen(), renderer.metrics(), &options);
        assert_eq!(rendered.len(), 9);
        assert!(
            !rendered
                .iter()
                .any(|p| p.placement == PlacementId::External(9))
        );
        assert!(
            rendered
                .iter()
                .any(|p| p.placement == PlacementId::External(8))
        );
    }

    #[test]
    fn kitty_layers_preserve_straight_alpha_and_native_size() {
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
    fn unicode_placeholder_targets_choose_stably_and_position_relative_children() {
        let (renderer, options) = renderer();
        let metrics = renderer.metrics();
        let mut terminal = Terminal::new(10, 4, 100);
        terminal.feed(b"\x1b_Ga=t,f=32,s=1,v=1,i=1;/wAAgA==\x1b\\");
        terminal.feed(b"\x1b_Ga=p,i=1,p=9,U=1,c=1,r=1\x1b\\");
        terminal.feed(b"\x1b_Ga=p,i=1,p=3,U=1,c=1,r=1\x1b\\");
        transmit(&mut terminal, 2, "c=1,r=1,P=1,Q=3,H=1,V=1");
        terminal
            .feed(format!("\x1b[38;5;1m\x1b[2;5H{PLACEHOLDER}\x1b[4;2H{PLACEHOLDER}").as_bytes());
        let rendered = geometry(terminal.screen(), metrics, &options);
        assert_eq!(rendered.len(), 3);
        assert_eq!(
            rendered
                .iter()
                .filter(|p| p.placement == PlacementId::External(3))
                .count(),
            2
        );
        let child = rendered.iter().find(|p| p.image == 2).unwrap();
        assert_eq!(
            &child.rect[..2],
            &[
                options.padding[0] + 2.0 * metrics.cell_width as f32,
                options.padding[1] + 2.0 * metrics.cell_height as f32,
            ]
        );
        terminal.feed(b"\x1b_Ga=p,i=1,p=5,C=1,c=1,r=1\x1b\\");
        terminal.feed(format!("\x1b[H\x1b[58;5;5m{PLACEHOLDER}").as_bytes());
        let count = |terminal: &Terminal| {
            geometry(terminal.screen(), metrics, &options)
                .iter()
                .filter(|p| p.image == 1 && p.placement == PlacementId::External(5))
                .count()
        };
        assert_eq!(count(&terminal), 2); // Ordinary placement plus its explicit placeholder.
        terminal.feed(b"\x1b_Ga=d,d=i,i=1,p=3\x1b\\\x1b_Ga=d,d=i,i=1,p=9\x1b\\");
        assert_eq!(count(&terminal), 1); // No virtuals remain to enable placeholder rendering.
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
