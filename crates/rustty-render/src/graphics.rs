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
            (screen.rows().next().map_or(0, |r| r.cells.len()) as f32 * metrics.cell_width as f32)
                .min((options.size[0] as f32 - 2.0 * options.padding[0]).max(0.0)),
            (screen.height() as f32 * metrics.cell_height as f32)
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
            let [sx, sy, sw, sh] = placement.source;
            let [x, y, w, h] = placement.rect;
            for [sy, sh, y, h] in image_axis([sy, sh], [y, h], image.height) {
                for [sx, sw, x, w] in image_axis([sx, sw], [x, w], image.width) {
                    for ty in (sy as u32 / TILE)
                        ..=(((sy + sh).ceil() as u32).saturating_sub(1) / TILE)
                            .max(sy as u32 / TILE)
                    {
                        for tx in (sx as u32 / TILE)
                            ..=(((sx + sw).ceil() as u32).saturating_sub(1) / TILE)
                                .max(sx as u32 / TILE)
                        {
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
                            if (sw > 0.0 && left >= right) || (sh > 0.0 && top >= bottom) {
                                continue;
                            }
                            let [u0, v0, u1, v1] = cached.uv;
                            let uvx = |pixel| {
                                u0 + (u1 - u0) * (pixel - (tx * TILE) as f32)
                                    / cached.size[0] as f32
                            };
                            let uvy = |pixel| {
                                v0 + (v1 - v0) * (pixel - (ty * TILE) as f32)
                                    / cached.size[1] as f32
                            };
                            layers[layer].push(Quad {
                                rect: [
                                    if sw == 0.0 {
                                        x
                                    } else {
                                        x + (left - sx) / sw * w
                                    },
                                    if sh == 0.0 {
                                        y
                                    } else {
                                        y + (top - sy) / sh * h
                                    },
                                    if sw == 0.0 {
                                        w
                                    } else {
                                        (right - left) / sw * w
                                    },
                                    if sh == 0.0 {
                                        h
                                    } else {
                                        (bottom - top) / sh * h
                                    },
                                ],
                                uv: [uvx(left), uvy(top), uvx(right), uvy(bottom)],
                                color: Color::rgb([255; 3]),
                                paint: Paint::Color,
                                atlas: cached.atlas,
                            });
                        }
                    }
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
            for run in unicode::placements(screen, row) {
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
    let first = screen.history_len().saturating_sub(screen.viewport_offset) as i64;
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

// Native textures clamp to their edge texels. Rounded placeholder fragments
// can have zero source extent or extend past an image edge; keep their full
// destination area by splitting off a constant-texel strip before atlas tiling.
// Each slice is [source start, source length, destination start, destination length].
fn image_axis(
    source: [f32; 2],
    destination: [f32; 2],
    extent: u32,
) -> impl Iterator<Item = [f32; 4]> {
    let length = source[1].min((extent as f32 - source[0]).max(0.0));
    let pixels = if source[1] == 0.0 {
        0.0
    } else {
        length / source[1] * destination[1]
    };
    [
        [source[0], length, destination[0], pixels],
        [
            (source[0] + length).clamp(0.5, extent as f32 - 0.5),
            0.0,
            destination[0] + pixels,
            destination[1] - pixels,
        ],
    ]
    .into_iter()
    .filter(|slice| slice[3] > 0.0)
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
    let Some(geometry) = run.geometry(p, image, [metrics.cell_width, metrics.cell_height]) else {
        return;
    };
    if geometry.pixels.contains(&0) {
        return;
    }
    result.push(Geometry {
        image: id,
        placement: p.placement_id,
        z: -1,
        source: geometry.source.map(|value| value as f32),
        rect: [
            options.padding[0]
                + run.col as f32 * metrics.cell_width as f32
                + geometry.offset[0] as f32,
            options.padding[1]
                + row as f32 * metrics.cell_height as f32
                + geometry.offset[1] as f32,
            geometry.pixels[0] as f32,
            geometry.pixels[1] as f32,
        ],
    });
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
        assert_eq!(snapshot.history_len(), 0);
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
    fn kitty_protocol_lifecycle_refreshes_pixels_and_reuses_unchanged_frames() {
        let mut terminal = Terminal::new(10, 3, 100);
        let (mut renderer, options) = renderer();
        let mut check = |terminal: &Terminal, expected: Option<[u8; 4]>, uploads: usize| {
            let frame = renderer.prepare(terminal.screen(), &options).unwrap();
            assert_eq!(frame.atlas_uploads.len(), uploads);
            let images: Vec<_> = frame
                .quads
                .iter()
                .filter(|q| q.paint == Paint::Color)
                .collect();
            let Some(expected) = expected else {
                assert!(images.is_empty());
                return;
            };
            assert_eq!(images.len(), 1);
            let quad = images[0];
            let pixel = frame.atlas_uploads.iter().find_map(|upload| {
                let x = (quad.uv[0] * upload.page_size as f32) as u32;
                let y = (quad.uv[1] * upload.page_size as f32) as u32;
                if quad.atlas != upload.page
                    || !(upload.origin[0]..upload.origin[0] + upload.size[0]).contains(&x)
                    || !(upload.origin[1]..upload.origin[1] + upload.size[1]).contains(&y)
                {
                    return None;
                }
                let index =
                    ((y - upload.origin[1]) * upload.size[0] + x - upload.origin[0]) as usize * 4;
                Some(&upload.pixels[index..index + 4])
            });
            assert_eq!(pixel, Some(expected.as_slice()));
        };
        let red = Some([255, 0, 0, 128]);
        let green = Some([0, 255, 0, 255]);
        transmit(&mut terminal, 1, "");
        check(&terminal, red, 1);
        terminal.feed(b"\x1b_Ga=f,i=1,f=32,s=1,v=1,z=10;AP8A/w==\x1b\\");
        check(&terminal, red, 1); // Uploading a hidden frame needs no atlas update.
        terminal.feed(b"\x1b_Ga=a,i=1,r=1,z=10,s=3\x1b\\");
        assert_eq!(terminal.tick_graphics(0), Some(10));
        assert_eq!(terminal.tick_graphics(10), Some(20));
        check(&terminal, green, 2);
        check(&terminal, green, 2); // Repainting the same frame reuses its texture.
        terminal.feed(b"\x1b_Ga=a,i=1,c=1,s=1\x1b\\");
        check(&terminal, red, 2);
        terminal.feed(b"\x1b_Ga=a,i=1,c=2\x1b\\");
        check(&terminal, green, 2);

        // Both editing and composing into the displayed frame replace its pixels.
        terminal.feed(b"\x1b_Ga=f,i=1,r=2,f=32,s=1,v=1,X=1;AAD//w==\x1b\\");
        check(&terminal, Some([0, 0, 255, 255]), 3);
        terminal.feed(b"\x1b_Ga=c,i=1,r=1,c=2,C=1\x1b\\");
        check(&terminal, red, 4);
        terminal.feed(b"\x1b_Ga=d,d=f,i=1,r=2\x1b\\");
        check(&terminal, red, 4);
        terminal.feed(b"\x1b_Ga=d,d=I,i=1\x1b\\");
        check(&terminal, None, 4);
        terminal.feed(b"\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,C=1;//8A/w==\x1b\\");
        check(&terminal, Some([255, 255, 0, 255]), 5);
    }

    #[test]
    fn unicode_placeholders_use_native_whole_pixel_source_and_destination_rectangles() {
        let (renderer, options) = renderer();
        let metrics = FontMetrics {
            cell_width: 36,
            cell_height: 80,
            ..renderer.metrics()
        };
        let mut terminal = Terminal::new(8, 3, 100);
        transmit(&mut terminal, 1, "U=1,c=4,r=2");
        let image = terminal.screen_mut().graphics.images.get_mut(&1).unwrap();
        image.width = 500;
        image.height = 306;
        image.pixels = vec![255; 500 * 306 * 4].into();
        for row in 0..2 {
            for col in 0..4 {
                let text = if col == 0 {
                    format!(
                        "{PLACEHOLDER}{}\u{305}",
                        if row == 0 { '\u{305}' } else { '\u{30d}' }
                    )
                } else {
                    PLACEHOLDER.to_string()
                };
                terminal.screen_mut().set_cell_text(row, col, &text);
                terminal.screen_mut().set_cell_style(
                    row,
                    col,
                    rustty_vt::Style {
                        foreground: TerminalColor::Indexed(1),
                        ..rustty_vt::Style::default()
                    },
                );
            }
        }
        let rendered = geometry(terminal.screen(), metrics, &options);
        assert_eq!(rendered.len(), 2);
        assert_eq!(rendered[0].source, [0.0, 0.0, 500.0, 153.0]);
        assert_eq!(rendered[1].source, [0.0, 153.0, 500.0, 153.0]);
        assert_eq!(
            rendered[0].rect,
            [options.padding[0], options.padding[1] + 36.0, 144.0, 44.0]
        );
        assert_eq!(
            rendered[1].rect,
            [options.padding[0], options.padding[1] + 80.0, 144.0, 44.0]
        );
    }

    #[test]
    fn tiny_placeholder_fragments_sample_clamped_texels_after_source_rounding() {
        let (mut renderer, options) = renderer();
        let mut terminal = Terminal::new(6, 4, 100);
        transmit(&mut terminal, 1, "U=1,c=3,r=2");
        let diacritics = ['\u{305}', '\u{30d}', '\u{30e}'];
        for (row, (image_row, image_col, width)) in [(0, 0, 1), (0, 1, 2), (1, 0, 4), (1, 2, 1)]
            .into_iter()
            .enumerate()
        {
            for col in 0..width {
                let text = if col == 0 {
                    format!(
                        "{PLACEHOLDER}{}{}",
                        diacritics[image_row], diacritics[image_col]
                    )
                } else {
                    PLACEHOLDER.to_string()
                };
                terminal.screen_mut().set_cell_text(row, col, &text);
                terminal.screen_mut().set_cell_style(
                    row,
                    col,
                    rustty_vt::Style {
                        foreground: TerminalColor::Indexed(1),
                        ..rustty_vt::Style::default()
                    },
                );
            }
        }
        let expected = geometry(terminal.screen(), renderer.metrics(), &options);
        assert_eq!(expected.len(), 4);
        let frame = renderer.prepare(terminal.screen(), &options).unwrap();
        let quads: Vec<_> = frame
            .quads
            .iter()
            .filter(|quad| quad.paint == Paint::Color)
            .collect();
        assert_eq!(quads.len(), 4);
        for (quad, expected) in quads.iter().zip(expected) {
            assert_eq!(quad.rect, expected.rect);
        }
        assert!(quads.iter().any(|quad| quad.uv[0] == quad.uv[2]));
        assert!(quads.iter().any(|quad| quad.uv[1] == quad.uv[3]));
    }

    #[test]
    fn rounded_placeholder_overhang_keeps_its_area_and_samples_the_last_texel() {
        let (mut renderer, options) = renderer();
        let mut terminal = Terminal::new(4, 2, 100);
        transmit(&mut terminal, 1, "U=1,c=2,r=2");
        let image = terminal.screen_mut().graphics.images.get_mut(&1).unwrap();
        image.width = 5;
        image.height = 5;
        image.pixels = (0..5)
            .flat_map(|y| (0..5).flat_map(move |x| [x * 33, y * 27, 128, 255]))
            .collect::<Vec<_>>()
            .into();
        terminal
            .screen_mut()
            .set_cell_text(0, 0, &format!("{PLACEHOLDER}\u{30d}\u{30d}"));
        terminal.screen_mut().set_cell_style(
            0,
            0,
            rustty_vt::Style {
                foreground: TerminalColor::Indexed(1),
                ..rustty_vt::Style::default()
            },
        );
        let expected = geometry(terminal.screen(), renderer.metrics(), &options);
        assert_eq!(expected.len(), 1);
        assert_eq!(expected[0].source, [3.0, 3.0, 3.0, 3.0]);
        let frame = renderer.prepare(terminal.screen(), &options).unwrap();
        let quads: Vec<_> = frame
            .quads
            .iter()
            .filter(|quad| quad.paint == Paint::Color)
            .collect();
        let area: f32 = quads.iter().map(|quad| quad.rect[2] * quad.rect[3]).sum();
        assert!((area - expected[0].rect[2] * expected[0].rect[3]).abs() < 0.01);
        let corner = quads
            .iter()
            .find(|quad| quad.uv[0] == quad.uv[2] && quad.uv[1] == quad.uv[3])
            .unwrap();
        let upload = &frame.atlas_uploads[0];
        let x = (corner.uv[0] * upload.page_size as f32) as u32 - upload.origin[0];
        let y = (corner.uv[1] * upload.page_size as f32) as u32 - upload.origin[1];
        let pixel = ((y * upload.size[0] + x) * 4) as usize;
        assert_eq!(&upload.pixels[pixel..pixel + 4], &[132, 108, 128, 255]);
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
                let text = if col == 0 {
                    format!(
                        "{PLACEHOLDER}{}\u{305}",
                        if row == 0 { '\u{305}' } else { '\u{30d}' }
                    )
                } else {
                    PLACEHOLDER.to_string()
                };
                terminal.screen_mut().set_cell_text(row, col, &text);
                terminal.screen_mut().set_cell_style(
                    row,
                    col,
                    rustty_vt::Style {
                        foreground: TerminalColor::Indexed(1),
                        ..rustty_vt::Style::default()
                    },
                );
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
