use crate::{AtlasUpload, Color, Frame, Paint, Quad, RenderError};
use rustty_font::{
    BitmapFormat, FontConfig, FontError, FontId, FontMetrics, FontStyle, FontSystem, GlyphBitmap,
    ShapedGlyph, sprite,
};
use rustty_vt::screen::{Color as TerminalColor, CursorShape, Row, Screen, Style, Underline};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

const PAGE_SIZE: u32 = 1024;
const MAX_ATLAS_BYTES: u64 = 64 * 1024 * 1024;
const MAX_IMAGE_ATLAS_BYTES: u64 = 320 * 1024 * 1024;
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

#[path = "graphics.rs"]
mod graphics;

#[path = "preedit.rs"]
mod preedit;
pub use preedit::Preedit;

#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub size: [u32; 2],
    pub padding: [f32; 2],
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub cursor_color: [u8; 3],
    pub cursor_text: [u8; 3],
    pub selection_background: [u8; 3],
    pub selection_foreground: Option<[u8; 3]>,
    pub palette: [[u8; 3]; 256],
    pub focused: bool,
    pub cursor_visible: bool,
    pub blink_visible: bool,
    pub background_opacity: f32,
    pub preedit: Option<Preedit>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        let mut palette = [[0; 3]; 256];
        palette[..16].copy_from_slice(&[
            [0, 0, 0],
            [205, 0, 0],
            [0, 205, 0],
            [205, 205, 0],
            [0, 0, 238],
            [205, 0, 205],
            [0, 205, 205],
            [229, 229, 229],
            [127, 127, 127],
            [255, 0, 0],
            [0, 255, 0],
            [255, 255, 0],
            [92, 92, 255],
            [255, 0, 255],
            [0, 255, 255],
            [255, 255, 255],
        ]);
        let levels = [0, 95, 135, 175, 215, 255];
        for i in 0..216 {
            palette[16 + i] = [levels[i / 36], levels[(i / 6) % 6], levels[i % 6]];
        }
        for i in 0..24 {
            palette[232 + i] = [8 + i as u8 * 10; 3];
        }
        Self {
            size: [800, 600],
            padding: [8.0, 8.0],
            foreground: [220, 220, 220],
            background: [24, 24, 24],
            cursor_color: [220, 220, 220],
            cursor_text: [24, 24, 24],
            selection_background: [65, 85, 120],
            selection_foreground: None,
            palette,
            focused: true,
            cursor_visible: true,
            blink_visible: true,
            background_opacity: 1.0,
            preedit: None,
        }
    }
}

#[derive(Clone)]
struct CachedGlyph {
    atlas: usize,
    uv: [f32; 4],
    size: [u32; 2],
    bearing: [i32; 2],
    color: bool,
}

struct Page {
    x: u32,
    y: u32,
    row_height: u32,
    size: u32,
    image: bool,
}

/// Builds frames on the host thread. Frame values themselves contain no native
/// handles and can be sent to another thread or retained by a UI paint callback.
pub struct Renderer {
    fonts: FontSystem,
    glyphs: HashMap<(FontId, u16), CachedGlyph>,
    sprites: HashMap<(char, u8), CachedGlyph>,
    images: HashMap<graphics::TileKey, graphics::CachedTile>,
    pages: Vec<Page>,
    uploads: Vec<AtlasUpload>,
    generation: u64,
}

impl Renderer {
    pub fn new(config: FontConfig) -> Result<Self, FontError> {
        Ok(Self {
            fonts: FontSystem::new(config)?,
            glyphs: HashMap::new(),
            sprites: HashMap::new(),
            images: HashMap::new(),
            pages: Vec::new(),
            uploads: Vec::new(),
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
        })
    }

    pub fn metrics(&self) -> FontMetrics {
        self.fonts.metrics()
    }
    pub fn missing_families(&self) -> &[String] {
        self.fonts.missing_families()
    }

    pub fn clear_cache(&mut self) {
        self.glyphs.clear();
        self.sprites.clear();
        self.images.clear();
        self.pages.clear();
        self.uploads.clear();
        self.generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    }

    pub fn prepare(
        &mut self,
        screen: &Screen,
        options: &RenderOptions,
    ) -> Result<Frame, RenderError> {
        match self.prepare_once(screen, options, false) {
            Err(RenderError::AtlasCapacity) => {
                // Rebuild the entire visible frame after eviction; a mid-frame
                // reset would invalidate the atlas coordinates of earlier quads.
                self.clear_cache();
                self.prepare_once(screen, options, true)
            }
            result => result,
        }
    }

    fn prepare_once(
        &mut self,
        screen: &Screen,
        options: &RenderOptions,
        omit_excess_images: bool,
    ) -> Result<Frame, RenderError> {
        let metrics = self.metrics();
        let mut frame = Frame::empty(options.size);
        frame.generation = self.generation;
        frame.quads.push(Quad::solid(
            [0.0, 0.0, options.size[0] as f32, options.size[1] as f32],
            Color::rgb(options.background).opacity(options.background_opacity),
        ));
        let [below_background, below_text, above_text] =
            self.prepare_graphics(screen, options, omit_excess_images)?;
        frame.quads.extend(below_background);
        let mut foreground = Frame::empty(options.size);
        let viewport_start = screen.history.len().saturating_sub(screen.viewport_offset);
        let selection = screen.selection.and_then(|selection| {
            let start = screen
                .all_rows()
                .position(|r| r.id == selection.start.row)?;
            let end = screen.all_rows().position(|r| r.id == selection.end.row)?;
            let a = (start, selection.start.col);
            let b = (end, selection.end.col);
            Some((a.min(b), a.max(b), selection.rectangular))
        });
        for (row_index, row) in screen.viewport().enumerate() {
            let top = options.padding[1] + row_index as f32 * metrics.cell_height as f32;
            if top >= options.size[1] as f32 {
                break;
            }
            let cursor = screen.cursor.visible
                && options.cursor_visible
                && !(options.focused
                    && options.preedit.as_ref().is_some_and(|p| !p.text.is_empty()))
                && screen.viewport_offset == 0
                && screen.cursor.row == row_index;
            let mut paints = Vec::with_capacity(row.cells.len());
            let visible_cols = ((options.size[0] as f32 - options.padding[0]).max(0.0)
                / metrics.cell_width as f32)
                .ceil() as usize;
            for (col, cell) in row.cells.iter().take(visible_cols).enumerate() {
                let selected = selection.is_some_and(|(start, end, rectangular)| {
                    let position = (viewport_start + row_index, col);
                    if rectangular {
                        position.0 >= start.0
                            && position.0 <= end.0
                            && col >= start.1.min(end.1)
                            && col <= start.1.max(end.1)
                    } else {
                        position >= start && position <= end
                    }
                });
                let mut fg = resolve(cell.style.foreground, options.foreground, options);
                let mut bg = resolve(cell.style.background, options.background, options);
                if cell.style.inverse {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if selected {
                    bg = options.selection_background;
                    fg = options.selection_foreground.unwrap_or(fg);
                }
                let block_cursor = cursor
                    && options.focused
                    && screen.cursor.shape == CursorShape::Block
                    && col == screen.cursor.col;
                if block_cursor {
                    bg = options.cursor_color;
                    fg = options.cursor_text;
                }
                if bg != options.background || selected || block_cursor {
                    frame.quads.push(Quad::solid(
                        [
                            options.padding[0] + col as f32 * metrics.cell_width as f32,
                            top,
                            metrics.cell_width as f32,
                            metrics.cell_height as f32,
                        ],
                        Color::rgb(bg),
                    ));
                }
                let fg = Color::rgb(fg).opacity(if cell.style.faint { 0.5 } else { 1.0 });
                paints.push(fg);
            }
            self.row_text(row, &paints, top, options, &mut foreground)?;
            for (col, cell) in row.cells.iter().take(visible_cols).enumerate() {
                if cell.width == 0 {
                    continue;
                }
                let x = options.padding[0] + col as f32 * metrics.cell_width as f32;
                let width = f32::from(cell.width) * metrics.cell_width as f32;
                let line_color = if cell.style.underline_color == TerminalColor::Default {
                    paints[col]
                } else {
                    Color::rgb(resolve(
                        cell.style.underline_color,
                        options.foreground,
                        options,
                    ))
                };
                if !cell.style.invisible && (!cell.style.blink || options.blink_visible) {
                    decorations(
                        &mut foreground,
                        &cell.style,
                        [x, top, width],
                        metrics,
                        paints[col],
                        line_color,
                    );
                }
            }
            if cursor && !(options.focused && screen.cursor.shape == CursorShape::Block) {
                let x = options.padding[0] + screen.cursor.col as f32 * metrics.cell_width as f32;
                let w = metrics.cell_width as f32;
                let h = metrics.cell_height as f32;
                let color = Color::rgb(options.cursor_color);
                if !options.focused || screen.cursor.shape == CursorShape::HollowBlock {
                    for rect in [
                        [x, top, w, 1.0],
                        [x, top + h - 1.0, w, 1.0],
                        [x, top, 1.0, h],
                        [x + w - 1.0, top, 1.0, h],
                    ] {
                        foreground.quads.push(Quad::solid(rect, color));
                    }
                } else {
                    let rect = match screen.cursor.shape {
                        CursorShape::Bar => [x, top, 2.0, h],
                        _ => [x, top + h - 2.0, w, 2.0],
                    };
                    foreground.quads.push(Quad::solid(rect, color));
                }
            }
        }
        frame.quads.extend(below_text);
        frame.quads.extend(foreground.quads);
        frame.quads.extend(above_text);
        self.preedit(screen, options, &mut frame)?;
        frame.atlas_uploads = self.uploads.clone();
        Ok(frame)
    }

    fn row_text(
        &mut self,
        row: &Row,
        paints: &[Color],
        top: f32,
        options: &RenderOptions,
        frame: &mut Frame,
    ) -> Result<(), RenderError> {
        let metrics = self.metrics();
        let mut col = 0;
        while col < paints.len() {
            let cell = &row.cells[col];
            if cell.width == 0
                || cell.style.invisible
                || cell.style.blink && !options.blink_visible
                || cell.text.starts_with(graphics::PLACEHOLDER)
            {
                col += 1;
                continue;
            }
            let style = cell.style;
            let color = paints[col];
            if let Some(cp) = self.sprite_codepoint(&cell.text) {
                let cached = self.sprite(cp, cell.width)?;
                frame.quads.push(Quad {
                    rect: [
                        options.padding[0] + col as f32 * metrics.cell_width as f32,
                        top,
                        cached.size[0] as f32,
                        cached.size[1] as f32,
                    ],
                    uv: cached.uv,
                    color,
                    paint: Paint::Mask,
                    atlas: cached.atlas,
                });
                col += 1;
                continue;
            }
            let mut text = String::new();
            let mut sources = Vec::new();
            while col < paints.len()
                && row.cells[col].style == style
                && paints[col] == color
                && self.sprite_codepoint(&row.cells[col].text).is_none()
                && !row.cells[col].text.starts_with(graphics::PLACEHOLDER)
            {
                let cell = &row.cells[col];
                if cell.width != 0 {
                    sources.push((text.len(), col));
                    text.push_str(if cell.text.is_empty() {
                        " "
                    } else {
                        &cell.text
                    });
                }
                col += 1;
            }
            let font_style = match (style.bold, style.italic) {
                (false, false) => FontStyle::Regular,
                (true, false) => FontStyle::Bold,
                (false, true) => FontStyle::Italic,
                (true, true) => FontStyle::BoldItalic,
            };
            let glyphs = self.fonts.shape(&text, font_style)?;
            let column = |g: &ShapedGlyph| {
                sources[sources
                    .partition_point(|(byte, _)| *byte <= g.cluster)
                    .saturating_sub(1)]
                .1
            };
            let mut anchors = HashMap::new();
            for glyph in &glyphs {
                if glyph.advance > 0.0 {
                    anchors.entry(column(glyph)).or_insert(glyph.x);
                }
            }
            for glyph in &glyphs {
                anchors.entry(column(glyph)).or_insert(glyph.x);
            }
            for glyph in &glyphs {
                let cached = self.glyph(glyph)?;
                if cached.size.contains(&0) {
                    continue;
                }
                let col = column(glyph);
                let x = options.padding[0] + col as f32 * metrics.cell_width as f32 + glyph.x
                    - anchors[&col]
                    + cached.bearing[0] as f32;
                let y = top + metrics.baseline - glyph.y - cached.bearing[1] as f32;
                frame.quads.push(Quad {
                    rect: [x, y, cached.size[0] as f32, cached.size[1] as f32],
                    uv: cached.uv,
                    color,
                    paint: if cached.color {
                        Paint::Color
                    } else {
                        Paint::Mask
                    },
                    atlas: cached.atlas,
                });
            }
        }
        Ok(())
    }

    fn glyph(&mut self, glyph: &ShapedGlyph) -> Result<CachedGlyph, RenderError> {
        let key = (glyph.font, glyph.glyph);
        if let Some(value) = self.glyphs.get(&key) {
            return Ok(value.clone());
        }
        let bitmap = self.fonts.rasterize(glyph)?;
        let cached = self.cache_bitmap(bitmap)?;
        self.glyphs.insert(key, cached.clone());
        Ok(cached)
    }

    fn sprite_codepoint(&self, text: &str) -> Option<char> {
        sprite_codepoint(text).filter(|cp| !self.fonts.has_codepoint_override(*cp))
    }

    fn sprite(&mut self, cp: char, width: u8) -> Result<CachedGlyph, RenderError> {
        let key = (cp, width);
        if let Some(value) = self.sprites.get(&key) {
            return Ok(value.clone());
        }
        let bitmap = sprite::rasterize(cp, self.metrics(), width)?
            .expect("sprite_codepoint validates the codepoint");
        let cached = self.cache_bitmap(bitmap)?;
        self.sprites.insert(key, cached.clone());
        Ok(cached)
    }

    fn cache_bitmap(&mut self, bitmap: GlyphBitmap) -> Result<CachedGlyph, RenderError> {
        let pixels: Vec<u8> = match bitmap.format {
            BitmapFormat::Alpha => bitmap
                .pixels
                .iter()
                .flat_map(|a| [255, 255, 255, *a])
                .collect(),
            BitmapFormat::Rgba => bitmap
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| {
                    let unpremultiply = |v: u8| {
                        if p[3] == 0 {
                            0
                        } else {
                            ((u32::from(v) * 255 + u32::from(p[3]) / 2) / u32::from(p[3])).min(255)
                                as u8
                        }
                    };
                    [
                        unpremultiply(p[0]),
                        unpremultiply(p[1]),
                        unpremultiply(p[2]),
                        p[3],
                    ]
                })
                .collect(),
        };
        let mut cached = self.cache_pixels([bitmap.width, bitmap.height], pixels.into(), false)?;
        cached.bearing = [bitmap.bearing_x, bitmap.bearing_y];
        cached.color = bitmap.format == BitmapFormat::Rgba;
        Ok(cached)
    }

    fn cache_pixels(
        &mut self,
        dimensions: [u32; 2],
        pixels: Arc<[u8]>,
        image: bool,
    ) -> Result<CachedGlyph, RenderError> {
        let [width, height] = dimensions;
        let mut cached = CachedGlyph {
            atlas: 0,
            uv: [0.0; 4],
            size: dimensions,
            bearing: [0, 0],
            color: image,
        };
        if width == 0 || height == 0 {
            return Ok(cached);
        }
        let required = (width + 2)
            .max(height + 2)
            .next_power_of_two()
            .max(PAGE_SIZE);
        let mut position = None;
        for (index, page) in self
            .pages
            .iter_mut()
            .enumerate()
            .filter(|(_, p)| p.image == image)
        {
            if let Some(origin) = reserve(page, width + 2, height + 2) {
                position = Some((index, origin));
                break;
            }
        }
        let (page, origin) = if let Some(position) = position {
            position
        } else {
            let bytes = self
                .pages
                .iter()
                .filter(|p| p.image == image)
                .map(|p| u64::from(p.size).pow(2) * 4)
                .sum::<u64>();
            let budget = if image {
                MAX_IMAGE_ATLAS_BYTES
            } else {
                MAX_ATLAS_BYTES
            };
            if bytes + u64::from(required).pow(2) * 4 > budget {
                return Err(RenderError::AtlasCapacity);
            }
            let index = self.pages.len();
            let mut page = Page {
                x: 0,
                y: 0,
                row_height: 0,
                size: required,
                image,
            };
            let origin = reserve(&mut page, width + 2, height + 2).expect("new page fits bitmap");
            self.pages.push(page);
            (index, origin)
        };
        let origin = [origin[0] + 1, origin[1] + 1];
        let size = self.pages[page].size as f32;
        cached.atlas = page;
        cached.uv = [
            origin[0] as f32 / size,
            origin[1] as f32 / size,
            (origin[0] + width) as f32 / size,
            (origin[1] + height) as f32 / size,
        ];
        self.uploads.push(AtlasUpload {
            revision: self.uploads.len() as u64 + 1,
            page,
            page_size: self.pages[page].size,
            origin,
            size: dimensions,
            pixels,
        });
        Ok(cached)
    }
}

fn sprite_codepoint(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let cp = chars.next()?;
    (sprite::contains(cp)
        && matches!(chars.next(), None | Some('\u{fe0e}' | '\u{fe0f}'))
        && chars.next().is_none())
    .then_some(cp)
}

fn reserve(page: &mut Page, width: u32, height: u32) -> Option<[u32; 2]> {
    if width > page.size || height > page.size {
        return None;
    }
    if page.x + width > page.size {
        page.x = 0;
        page.y += page.row_height;
        page.row_height = 0;
    }
    if page.y + height > page.size {
        return None;
    }
    let origin = [page.x, page.y];
    page.x += width;
    page.row_height = page.row_height.max(height);
    Some(origin)
}

fn resolve(color: TerminalColor, default: [u8; 3], options: &RenderOptions) -> [u8; 3] {
    match color {
        TerminalColor::Default => default,
        TerminalColor::Indexed(i) => options.palette[i as usize],
        TerminalColor::Rgb(r, g, b) => [r, g, b],
    }
}

fn decorations(
    frame: &mut Frame,
    style: &Style,
    rect: [f32; 3],
    metrics: FontMetrics,
    fg: Color,
    underline: Color,
) {
    let [x, top, width] = rect;
    let thickness = metrics.underline_thickness.ceil();
    let y = (top + metrics.baseline + metrics.underline_position)
        .min(top + metrics.cell_height as f32 - thickness);
    match style.underline {
        Underline::None => {}
        Underline::Single => frame
            .quads
            .push(Quad::solid([x, y, width, thickness], underline)),
        Underline::Double => {
            for y in [y, y - 2.0 * thickness] {
                frame
                    .quads
                    .push(Quad::solid([x, y, width, thickness], underline));
            }
        }
        Underline::Dotted | Underline::Dashed => {
            let length = if style.underline == Underline::Dotted {
                thickness
            } else {
                3.0 * thickness
            };
            let mut dx = 0.0;
            while dx < width {
                frame.quads.push(Quad::solid(
                    [x + dx, y, length.min(width - dx), thickness],
                    underline,
                ));
                dx += length * 2.0;
            }
        }
        Underline::Curly => {
            for dx in 0..width.ceil() as u32 {
                let offset = ((x + dx as f32) * std::f32::consts::PI / 3.0).sin() * thickness;
                frame.quads.push(Quad::solid(
                    [x + dx as f32, y - thickness + offset, 1.0, thickness],
                    underline,
                ));
            }
        }
    }
    if style.strikethrough {
        frame.quads.push(Quad::solid(
            [x, top + metrics.baseline * 0.65, width, thickness],
            fg,
        ));
    }
    if style.overline {
        frame
            .quads
            .push(Quad::solid([x, top, width, thickness], fg));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustty_vt::{GridPoint, Selection, Terminal};

    #[test]
    fn styled_unicode_frame_is_self_contained_and_cache_can_be_recreated() {
        let mut terminal = Terminal::new(20, 2, 100);
        terminal.feed("A\u{1b}[31mR\u{1b}[0m👩🏽‍💻水e\u{301}".as_bytes());
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let options = RenderOptions {
            cursor_visible: false,
            ..Default::default()
        };
        let first = renderer.prepare(terminal.screen(), &options).unwrap();
        assert!(first.quads.iter().any(|q| q.paint == Paint::Color));
        assert!(
            first
                .quads
                .iter()
                .any(|q| q.paint == Paint::Mask && q.color == Color::rgb(options.palette[1]))
        );
        assert!(!first.atlas_uploads.is_empty());
        let second = renderer.prepare(terminal.screen(), &options).unwrap();
        assert_eq!(first.generation, second.generation);
        assert_eq!(first.atlas_uploads.len(), second.atlas_uploads.len());
        renderer.clear_cache();
        let restored = renderer.prepare(terminal.screen(), &options).unwrap();
        assert_ne!(first.generation, restored.generation);
        assert_eq!(first.quads, restored.quads);
    }

    #[test]
    fn selection_and_unfocused_cursor_have_explicit_geometry() {
        let mut terminal = Terminal::new(10, 2, 10);
        terminal.feed(b"hello");
        let row = terminal.screen().rows[0].id;
        terminal.screen_mut().selection = Some(Selection {
            start: GridPoint { row, col: 1 },
            end: GridPoint { row, col: 3 },
            rectangular: false,
        });
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let options = RenderOptions {
            focused: false,
            ..Default::default()
        };
        let frame = renderer.prepare(terminal.screen(), &options).unwrap();
        assert_eq!(
            frame
                .quads
                .iter()
                .filter(|q| q.paint == Paint::Solid
                    && q.color == Color::rgb(options.selection_background))
                .count(),
            3
        );
        assert_eq!(
            frame
                .quads
                .iter()
                .filter(|q| q.paint == Paint::Solid && q.color == Color::rgb(options.cursor_color))
                .count(),
            4
        );
        terminal.screen_mut().cursor.shape = CursorShape::HollowBlock;
        let focused = renderer
            .prepare(
                terminal.screen(),
                &RenderOptions {
                    focused: true,
                    ..options
                },
            )
            .unwrap();
        assert_eq!(focused.quads, frame.quads);
    }

    #[test]
    fn sprites_fill_cells_without_entering_native_shaping_runs() {
        let mut terminal = Terminal::new(10, 2, 10);
        terminal.feed("A█B─█".as_bytes());
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let options = RenderOptions {
            cursor_visible: false,
            ..Default::default()
        };
        let frame = renderer.prepare(terminal.screen(), &options).unwrap();
        let metrics = renderer.metrics();
        let full = renderer.sprites[&('█', 1)].clone();
        let quads: Vec<_> = frame
            .quads
            .iter()
            .filter(|q| q.paint == Paint::Mask && q.uv == full.uv && q.atlas == full.atlas)
            .collect();
        assert_eq!(quads.len(), 2);
        for (q, col) in quads.into_iter().zip([1, 4]) {
            assert_eq!(
                q.rect,
                [
                    options.padding[0] + col as f32 * metrics.cell_width as f32,
                    options.padding[1],
                    metrics.cell_width as f32,
                    metrics.cell_height as f32,
                ]
            );
        }
        let upload = frame
            .atlas_uploads
            .iter()
            .find(|u| {
                u.page == full.atlas && u.size == full.size && u.pixels.iter().all(|b| *b == 255)
            })
            .unwrap();
        assert_eq!(upload.size, [metrics.cell_width, metrics.cell_height]);
        assert_eq!(renderer.sprites.len(), 2);
        assert_eq!(sprite_codepoint("─\u{fe0f}"), Some('─'));
        assert_eq!(sprite_codepoint("─\u{301}"), None);
        renderer.clear_cache();
        assert!(renderer.sprites.is_empty());
        let mut mapped = Renderer::new(FontConfig {
            codepoint_map: vec![rustty_font::CodepointMap {
                start: '─' as u32,
                end: '─' as u32,
                family: "Menlo".into(),
            }],
            ..Default::default()
        })
        .unwrap();
        mapped.prepare(terminal.screen(), &options).unwrap();
        assert!(!mapped.sprites.contains_key(&('─', 1)));
        assert!(mapped.sprites.contains_key(&('█', 1)));
    }
}
