use crate::{
    AtlasUpload, Color, Frame, MAX_GLYPH_ATLAS_BYTES, MAX_IMAGE_ATLAS_BYTES, Paint, Quad,
    RenderError, SearchHighlight,
};
use rustty_font::{
    BitmapFormat, FontConfig, FontError, FontId, FontMetrics, FontStyle, FontSystem, GlyphBitmap,
    ShapedGlyph, sprite,
};
use rustty_vt::extra_cursors::{Color as ExtraColor, Colors as ExtraColors};
use rustty_vt::render::{CursorKey, RowKey, ViewKey};
use rustty_vt::screen::{Color as TerminalColor, CursorShape, RowView, Screen, Style, Underline};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

const PAGE_SIZE: u32 = 1024;
const MAX_SHAPED_BYTES: usize = 4 * 1024 * 1024;
const RENDER_TEXT: u8 = 0;
const FIND_RUNS: u8 = 1;
const SHAPE_RUNS: u8 = 2;
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

#[path = "graphics.rs"]
mod graphics;

#[path = "preedit.rs"]
mod preedit;
pub use preedit::Preedit;

#[derive(Clone, Debug, PartialEq)]
pub struct RenderOptions {
    pub size: [u32; 2],
    pub padding: [f32; 2],
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub cursor_color: [u8; 3],
    pub cursor_text: [u8; 3],
    pub selection_background: [u8; 3],
    pub selection_foreground: Option<[u8; 3]>,
    pub search_highlights: Vec<SearchHighlight>,
    pub palette: [[u8; 3]; 256],
    /// Indexed graphics colors 248..255; omitted values use the resolved text colors.
    pub indexed_colors: Option<[[u8; 3]; 8]>,
    pub focused: bool,
    pub cursor_visible: bool,
    /// Host visibility for extra cursors, independent of main-cursor hiding and IME.
    pub extra_cursor_visible: bool,
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
            search_highlights: Vec::new(),
            palette,
            indexed_colors: None,
            focused: true,
            cursor_visible: true,
            extra_cursor_visible: true,
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

#[derive(Default)]
struct RowScratch {
    paints: Vec<Color>,
    cursors: Vec<Option<ExtraCursorCell>>,
    search: Vec<Option<bool>>,
    text: String,
    sources: Vec<(usize, usize)>,
    anchors: Vec<Option<f32>>,
}

#[derive(Clone, Copy)]
struct ExtraCursorCell {
    strokes: u8,
    color: [u8; 3],
}

impl ExtraCursorCell {
    const BLOCK: u8 = 1;
    const BAR: u8 = 2;
    const UNDERLINE: u8 = 4;
    const TOP: u8 = 8;
    const BOTTOM: u8 = 16;
    const LEFT: u8 = 32;
    const RIGHT: u8 = 64;
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ExtraCursorKey {
    revision: u64,
    colors: ExtraColors,
    shape: CursorShape,
    visible: bool,
}

struct CachedRow {
    key: RowKey,
    cursor: Option<(CursorKey, bool)>,
    extra_cursors: Option<ExtraCursorKey>,
    blink_visible: bool,
    background: Vec<Quad>,
    foreground: Vec<Quad>,
    quad_counts: [usize; 2],
    valid: bool,
    blinking_text: bool,
    cursor_bounds: Option<[f32; 4]>,
    used: bool,
}

/// Caller-owned geometry for one pane. Pass its live view key when rendering
/// detached viewport snapshots, whose storage identity changes on each capture.
#[derive(Default)]
pub struct RowCache {
    view: Option<ViewKey>,
    options: Option<RenderOptions>,
    generation: u64,
    rows: HashMap<u64, CachedRow>,
    scratch: RowScratch,
    #[cfg(test)]
    prepared_rows: usize,
}

impl RowCache {
    /// Discard row geometry without changing the renderer's font atlas.
    pub fn clear(&mut self) {
        self.rows.clear();
        self.view = None;
        self.options = None;
    }

    fn begin(&mut self, view: ViewKey, options: &RenderOptions, generation: u64) {
        let mut settings = options.clone();
        // These affect individual rows or the separately prepared IME overlay.
        settings.blink_visible = true;
        settings.preedit = None;
        settings.indexed_colors = None;
        if self.generation != generation
            || self.view.is_none_or(|old| !old.same_row_layout(&view))
            || self.options.as_ref() != Some(&settings)
        {
            self.rows.clear();
        }
        self.view = Some(view);
        self.options = Some(settings);
        self.generation = generation;
        for row in self.rows.values_mut() {
            row.used = false;
        }
    }
}

/// Builds frames on the host thread. Frame values themselves contain no native
/// handles and can be sent to another thread or retained by a UI paint callback.
pub struct Renderer {
    fonts: FontSystem,
    glyphs: HashMap<(FontId, u16), CachedGlyph>,
    shaped: [HashMap<String, Arc<[ShapedGlyph]>>; 4],
    shaped_bytes: usize,
    sprites: HashMap<(char, u8), CachedGlyph>,
    images: HashMap<graphics::TileKey, graphics::CachedTile>,
    placeholders: graphics::PlaceholderCache,
    pages: BTreeMap<usize, Page>,
    uploads: Vec<AtlasUpload>,
    next_page: usize,
    next_revision: u64,
    generation: u64,
}

impl Renderer {
    pub fn new(config: FontConfig) -> Result<Self, FontError> {
        Ok(Self {
            fonts: FontSystem::new(config)?,
            glyphs: HashMap::new(),
            shaped: Default::default(),
            shaped_bytes: 0,
            sprites: HashMap::new(),
            images: HashMap::new(),
            placeholders: Default::default(),
            pages: BTreeMap::new(),
            uploads: Vec::new(),
            next_page: 0,
            next_revision: 0,
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
        })
    }

    pub fn metrics(&self) -> FontMetrics {
        self.fonts.metrics()
    }
    /// Atlas identity; retained frames from another generation must be rebuilt.
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn missing_families(&self) -> &[String] {
        self.fonts.missing_families()
    }

    pub fn clear_cache(&mut self) {
        self.glyphs.clear();
        for cache in &mut self.shaped {
            cache.clear();
        }
        self.shaped_bytes = 0;
        self.sprites.clear();
        self.images.clear();
        self.placeholders = Default::default();
        self.pages.clear();
        self.uploads.clear();
        self.next_page = 0;
        self.next_revision = 0;
        self.generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    }

    pub fn prepare(
        &mut self,
        screen: &Screen,
        options: &RenderOptions,
    ) -> Result<Frame, RenderError> {
        match self.prepare_once(screen, options, false, None) {
            Err(RenderError::AtlasCapacity) => {
                // Rebuild the entire visible frame after eviction; a mid-frame
                // reset would invalidate the atlas coordinates of earlier quads.
                self.clear_cache();
                self.prepare_once(screen, options, true, None)
            }
            result => result,
        }
    }

    /// Reuse unchanged visible rows while preparing graphics and preedit anew.
    /// `live_view` identifies the source, not the detached `screen` snapshot.
    pub fn prepare_cached(
        &mut self,
        screen: &Screen,
        live_view: ViewKey,
        options: &RenderOptions,
        cache: &mut RowCache,
    ) -> Result<Frame, RenderError> {
        cache.begin(live_view, options, self.generation);
        let result = match self.prepare_once(screen, options, false, Some(&mut *cache)) {
            Err(RenderError::AtlasCapacity) => {
                self.clear_cache();
                cache.begin(live_view, options, self.generation);
                self.prepare_once(screen, options, true, Some(&mut *cache))
            }
            result => result,
        };
        if result.is_err() {
            cache.clear();
        }
        result
    }

    fn prepare_once(
        &mut self,
        screen: &Screen,
        options: &RenderOptions,
        omit_excess_images: bool,
        mut cache: Option<&mut RowCache>,
    ) -> Result<Frame, RenderError> {
        let metrics = self.metrics();
        let mut frame = Frame::empty(options.size);
        frame.generation = self.generation;
        let [below_background, below_text, above_text] =
            self.prepare_graphics(screen, options, omit_excess_images)?;
        let (background_capacity, foreground_capacity) = cache
            .as_deref()
            .map(|cache| {
                cache
                    .rows
                    .values()
                    .fold((0, 0), |(background, foreground), row| {
                        (
                            background + row.quad_counts[0],
                            foreground + row.quad_counts[1],
                        )
                    })
            })
            .unwrap_or_default();
        frame.quads.reserve(
            1 + background_capacity
                + foreground_capacity
                + below_background.len()
                + below_text.len()
                + above_text.len(),
        );
        frame.quads.push(Quad::solid(
            [0.0, 0.0, options.size[0] as f32, options.size[1] as f32],
            Color::rgb(options.background).opacity(options.background_opacity),
        ));
        frame.quads.extend(below_background);
        let mut foreground = Frame::empty(options.size);
        foreground.quads.reserve(foreground_capacity);
        let mut scratch = cache
            .as_deref_mut()
            .map(|cache| std::mem::take(&mut cache.scratch))
            .unwrap_or_default();
        let view = screen.render_view();
        let cursor_view = view.cursor();
        for (row_index, rendered) in view.rows().enumerate() {
            let row_key = rendered.key();
            let row = rendered.row;
            let selection = rendered.selection;
            let row_top = options.padding[1] + row_index as f32 * metrics.cell_height as f32;
            if row_top >= options.size[1] as f32 {
                break;
            }
            let cursor_row = cursor_view.is_some_and(|cursor| cursor.y == row_index)
                && options.cursor_visible
                && !(options.focused
                    && options.preedit.as_ref().is_some_and(|p| !p.text.is_empty()))
                && screen.viewport_offset == 0;
            let cursor =
                cursor_row && (!options.focused || !screen.cursor.blink || options.blink_visible);
            let cursor_key = cursor_row.then(|| (cursor_view.unwrap().key(), cursor));
            let cursor_span = row
                .cells()
                .get(screen.cursor.col)
                .map(|_| row.cell_span(screen.cursor.col))
                .filter(|span| span.explicit);
            let cursor_col = cursor_span.map_or(screen.cursor.col, |span| span.start);
            let cursor_width = cursor_span.map_or(1, |span| usize::from(span.width));
            let extra_row = options
                .extra_cursor_visible
                .then(|| screen.extra_cursors().row(row_index))
                .flatten();
            let extra_colors = screen.extra_cursors().colors();
            let extra_visible = !options.focused || !screen.cursor.blink || options.blink_visible;
            let extra_key = extra_row.as_ref().map(|extra| ExtraCursorKey {
                revision: extra.revision,
                colors: extra_colors,
                shape: screen.cursor.shape,
                visible: extra_visible,
            });
            let mut cache_row = false;
            if let Some(cached) = cache
                .as_deref_mut()
                .and_then(|cache| cache.rows.get_mut(&row.id))
                .filter(|cached| {
                    // Rewritten rows avoid a second geometry copy until their key repeats.
                    cache_row = cached.key == row_key;
                    cached.valid
                        && cache_row
                        && cached.cursor == cursor_key
                        && cached.extra_cursors == extra_key
                        && (!cached.blinking_text || cached.blink_visible == options.blink_visible)
                })
            {
                cached.used = true;
                for (quads, stored) in [
                    (&mut frame.quads, &cached.background),
                    (&mut foreground.quads, &cached.foreground),
                ] {
                    quads.extend(stored.iter().cloned().map(|mut quad| {
                        quad.rect[1] += row_top;
                        quad
                    }));
                }
                frame.blinking_text |= cached.blinking_text;
                if let Some(mut bounds) = cached.cursor_bounds {
                    bounds[1] += row_top;
                    frame.cursor_row_bounds = Some(bounds);
                }
                continue;
            }
            // Both paths position row-local geometry identically, including after scrolling.
            let top = 0.0;
            let background_start = frame.quads.len();
            let foreground_start = foreground.quads.len();
            let mut blinking_text = false;
            let mut cursor_bounds = None;
            let visible_cols = ((options.size[0] as f32 - options.padding[0]).max(0.0)
                / metrics.cell_width as f32)
                .ceil() as usize;
            let visible_cols = visible_cols.min(screen.columns);
            let physical_cols = visible_cols.min(row.cells().len());
            scratch.cursors.clear();
            if let Some(extra) = extra_row.filter(|_| extra_visible) {
                scratch.cursors.resize(visible_cols, None);
                for (col, &shape) in extra.shapes.iter().take(visible_cols).enumerate() {
                    let shape = match shape {
                        1 => CursorShape::Block,
                        2 => CursorShape::Bar,
                        3 => CursorShape::Underline,
                        29 if !options.focused => CursorShape::HollowBlock,
                        29 => screen.cursor.shape,
                        _ => continue,
                    };
                    let (start, end) = if col < physical_cols {
                        let span = row.cell_span(col);
                        (span.start, span.start + usize::from(span.width))
                    } else {
                        (col, col + 1)
                    };
                    for x in start..end.min(visible_cols) {
                        // A visible main cursor takes precedence over extra cursors.
                        if cursor && (cursor_col..cursor_col + cursor_width).contains(&x) {
                            continue;
                        }
                        let strokes = match shape {
                            CursorShape::Block => ExtraCursorCell::BLOCK,
                            CursorShape::Bar if x == start => ExtraCursorCell::BAR,
                            CursorShape::Bar => 0,
                            CursorShape::Underline => ExtraCursorCell::UNDERLINE,
                            CursorShape::HollowBlock => {
                                ExtraCursorCell::TOP
                                    | ExtraCursorCell::BOTTOM
                                    | if x == start { ExtraCursorCell::LEFT } else { 0 }
                                    | if x + 1 == end {
                                        ExtraCursorCell::RIGHT
                                    } else {
                                        0
                                    }
                            }
                        };
                        if strokes != 0 {
                            // Several positions can share a span. Keep all requested shapes
                            // while drawing each overlapping stroke once.
                            let cell = scratch.cursors[x].get_or_insert(ExtraCursorCell {
                                strokes: 0,
                                color: options.cursor_color,
                            });
                            cell.strokes |= strokes;
                        }
                    }
                }
            }
            scratch.search.clear();
            for highlight in options.search_highlights.iter().filter(|h| h.row == row.id) {
                scratch.search.resize(visible_cols, None);
                let start = (*highlight.columns.start()).min(visible_cols);
                let end = highlight.columns.end().saturating_add(1).min(visible_cols);
                if let Some(cells) = scratch.search.get_mut(start..end) {
                    for cell in cells {
                        *cell = Some(cell.unwrap_or(false) || highlight.selected);
                    }
                }
            }
            // Raw-zero tails have no text, background, blink or decorations.
            let text_cols = row.cells()[..physical_cols]
                .iter()
                .rposition(|cell| cell.bits() != 0)
                .map_or(0, |col| col + 1);
            // ponytail: keep full rows for cursor/selection/search painting; bound
            // their ranges if those redraws become a bottleneck.
            let paint_cols = if cursor
                || !scratch.cursors.is_empty()
                || selection.is_some()
                || !scratch.search.is_empty()
            {
                visible_cols
            } else {
                text_cols
            };
            scratch.paints.clear();
            scratch.paints.reserve(paint_cols);
            for col in 0..paint_cols {
                let cell = row.cells().get(col);
                let span = cell.map(|_| row.cell_span(col));
                let style = cell.map_or_else(Style::default, |_| row.style(col));
                if style.blink
                    && !style.invisible
                    && let Some(cell) = cell
                    && let Some(span) = span
                    && span.start == col
                    && !cell.spacer_head()
                    && (style.underline != Underline::None
                        || style.strikethrough
                        || style.overline
                        || cell.codepoint() != Some(graphics::PLACEHOLDER)
                            && row.text(col).chars().any(|ch| !ch.is_whitespace()))
                {
                    blinking_text = true;
                }
                let selected = selection.as_ref().is_some_and(|range| range.contains(&col));
                let mut fg = resolve(style.foreground, options.foreground, options);
                let mut bg = resolve(style.background, options.background, options);
                if style.inverse {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if selected {
                    bg = options.selection_background;
                    fg = options.selection_foreground.unwrap_or(fg);
                }
                let search_col = span.map_or(col, |span| span.start);
                let search = scratch.search.get(search_col).copied().flatten();
                if let Some(selected) = search {
                    bg = if selected {
                        [242, 165, 126]
                    } else {
                        [255, 224, 130]
                    };
                    fg = [0; 3];
                }
                let mut extra_block = false;
                if let Some(Some(extra)) = scratch.cursors.get_mut(col) {
                    let (background, foreground) =
                        extra_cursor_colors(extra_colors, fg, bg, options);
                    extra.color = background;
                    extra_block = extra.strokes & ExtraCursorCell::BLOCK != 0;
                    if extra_block {
                        bg = background;
                        fg = foreground;
                    }
                }
                let block_cursor = cursor
                    && options.focused
                    && screen.cursor.shape == CursorShape::Block
                    && (cursor_col..cursor_col + cursor_width).contains(&col);
                if block_cursor {
                    bg = options.cursor_color;
                    fg = options.cursor_text;
                }
                if bg != options.background
                    || selected
                    || search.is_some()
                    || block_cursor
                    || extra_block
                {
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
                let fg = Color::rgb(fg).opacity(if style.faint { 0.5 } else { 1.0 });
                scratch.paints.push(fg);
            }
            scratch.paints.truncate(text_cols);
            self.row_text::<RENDER_TEXT>(row, &mut scratch, top, options, &mut foreground)?;
            let paints = &scratch.paints;
            for (col, cell) in row.cells().iter().take(text_cols).enumerate() {
                let span = row.cell_span(col);
                if span.start != col || cell.spacer_head() {
                    continue;
                }
                let x = options.padding[0] + col as f32 * metrics.cell_width as f32;
                let width = f32::from(span.width) * metrics.cell_width as f32;
                let line_color = if row.style(col).underline_color == TerminalColor::Default {
                    paints[col]
                } else {
                    Color::rgb(resolve(
                        row.style(col).underline_color,
                        options.foreground,
                        options,
                    ))
                };
                if !row.style(col).invisible && (!row.style(col).blink || options.blink_visible) {
                    decorations(
                        &mut foreground,
                        &row.style(col),
                        [x, top, width],
                        metrics,
                        paints[col],
                        line_color,
                    );
                }
            }
            for (col, extra) in scratch.cursors.iter().enumerate() {
                let Some(extra) = extra else { continue };
                let x = options.padding[0] + col as f32 * metrics.cell_width as f32;
                let w = metrics.cell_width as f32;
                let h = metrics.cell_height as f32;
                let color = Color::rgb(extra.color);
                let has = |stroke| extra.strokes & stroke != 0;
                for (draw, rect) in [
                    (has(ExtraCursorCell::BAR), [x, top, 2.0, h]),
                    (has(ExtraCursorCell::UNDERLINE), [x, top + h - 2.0, w, 2.0]),
                    (has(ExtraCursorCell::TOP), [x, top, w, 1.0]),
                    (
                        has(ExtraCursorCell::BOTTOM) && !has(ExtraCursorCell::UNDERLINE),
                        [x, top + h - 1.0, w, 1.0],
                    ),
                    (
                        has(ExtraCursorCell::LEFT) && !has(ExtraCursorCell::BAR),
                        [x, top, 1.0, h],
                    ),
                    (has(ExtraCursorCell::RIGHT), [x + w - 1.0, top, 1.0, h]),
                ] {
                    if draw {
                        foreground.quads.push(Quad::solid(rect, color));
                    }
                }
            }
            if cursor && !(options.focused && screen.cursor.shape == CursorShape::Block) {
                let x = options.padding[0] + cursor_col as f32 * metrics.cell_width as f32;
                let w = cursor_width as f32 * metrics.cell_width as f32;
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
            if cursor_row {
                let mut left = options.padding[0] + cursor_col as f32 * metrics.cell_width as f32;
                let mut upper = top;
                let mut right = left + cursor_width as f32 * metrics.cell_width as f32;
                let mut bottom = top + metrics.cell_height as f32;
                // Cursor recoloring can reshape ligatures beyond its cell.
                for quad in &foreground.quads[foreground_start..] {
                    let [x, y, w, h] = quad.rect;
                    left = left.min(x);
                    upper = upper.min(y);
                    right = right.max(x + w);
                    bottom = bottom.max(y + h);
                }
                cursor_bounds = Some([left, upper, right - left, bottom - upper]);
            }
            if let Some(cache) = cache.as_deref_mut() {
                #[cfg(test)]
                {
                    cache.prepared_rows += 1;
                }
                let cached = cache.rows.entry(row.id).or_insert_with(|| CachedRow {
                    key: row_key,
                    cursor: cursor_key,
                    extra_cursors: extra_key,
                    blink_visible: options.blink_visible,
                    background: Vec::new(),
                    foreground: Vec::new(),
                    quad_counts: [0; 2],
                    valid: false,
                    blinking_text,
                    cursor_bounds,
                    used: true,
                });
                cached.key = row_key;
                cached.cursor = cursor_key;
                cached.extra_cursors = extra_key;
                cached.blink_visible = options.blink_visible;
                cached.quad_counts = [
                    frame.quads.len() - background_start,
                    foreground.quads.len() - foreground_start,
                ];
                cached.valid = cache_row;
                if cache_row {
                    cached.background.clear();
                    cached
                        .background
                        .extend_from_slice(&frame.quads[background_start..]);
                    cached.foreground.clear();
                    cached
                        .foreground
                        .extend_from_slice(&foreground.quads[foreground_start..]);
                }
                cached.blinking_text = blinking_text;
                cached.cursor_bounds = cursor_bounds;
                cached.used = true;
            }
            for quad in frame.quads[background_start..]
                .iter_mut()
                .chain(&mut foreground.quads[foreground_start..])
            {
                quad.rect[1] += row_top;
            }
            frame.blinking_text |= blinking_text;
            if let Some(mut bounds) = cursor_bounds {
                bounds[1] += row_top;
                frame.cursor_row_bounds = Some(bounds);
            }
        }
        frame.quads.extend(below_text);
        frame.quads.extend(foreground.quads);
        frame.quads.extend(above_text);
        self.preedit(screen, options, &mut frame)?;
        let image_pages = self.image_pages(
            screen
                .graphics
                .images
                .values()
                .map(|image| (image, options)),
        );
        frame.atlas_uploads = self
            .uploads
            .iter()
            .filter(|upload| !self.pages[&upload.page].image || image_pages.contains(&upload.page))
            .cloned()
            .collect();
        if let Some(cache) = cache {
            cache.scratch = scratch;
            cache.rows.retain(|_, row| row.used);
        }
        Ok(frame)
    }

    fn row_text<const MODE: u8>(
        &mut self,
        row: RowView<'_>,
        scratch: &mut RowScratch,
        top: f32,
        options: &RenderOptions,
        frame: &mut Frame,
    ) -> Result<usize, RenderError> {
        let metrics = self.metrics();
        let mut work = 0;
        let RowScratch {
            paints,
            text,
            sources,
            anchors,
            ..
        } = scratch;
        let mut col = 0;
        while col < paints.len() {
            let cell = &row.cells()[col];
            let span = row.cell_span(col);
            if span.start != col
                || cell.spacer_head()
                || row.style(col).invisible
                || row.style(col).blink && !options.blink_visible
                || cell.codepoint() == Some(graphics::PLACEHOLDER)
            {
                col += 1;
                continue;
            }
            let style = row.style(col);
            let color = paints[col];
            let span_end = (span.start + usize::from(span.width)).min(paints.len());
            let split_colors = span.width > 1
                && paints[span.start..span_end]
                    .iter()
                    .any(|&paint| paint != color);
            if !span.explicit
                && !split_colors
                && let Some(cp) = self.sprite_codepoint(&row.text(col))
            {
                if MODE != RENDER_TEXT {
                    std::hint::black_box((cp, cell.width()));
                    work += 1;
                    col += 1;
                    continue;
                }
                let cached = self.sprite(cp, cell.width())?;
                frame.quads.push(Quad {
                    offset: [0.0; 2],
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
            text.clear();
            sources.clear();
            let clip = if span.explicit || split_colors {
                sources.push((0, col));
                text.push_str(&row.text(col));
                col += usize::from(span.width);
                let left = options.padding[0] + span.start as f32 * metrics.cell_width as f32;
                Some([
                    left,
                    top,
                    left + f32::from(span.width) * metrics.cell_width as f32,
                    top + metrics.cell_height as f32,
                ])
            } else {
                None
            };
            while clip.is_none()
                && col < paints.len()
                && row.style(col) == style
                && paints[col] == color
                && !row.cell_span(col).explicit
                && (row.cells()[col].width() != 2
                    || paints.get(col + 1).is_none_or(|&paint| paint == color))
                && !row.cells()[col].spacer_head()
                && self.sprite_codepoint(&row.text(col)).is_none()
                && row.cells()[col].codepoint() != Some(graphics::PLACEHOLDER)
            {
                let cell = &row.cells()[col];
                if cell.width() != 0 {
                    sources.push((text.len(), col));
                    if cell.codepoint().is_none() {
                        text.push(' ');
                    } else {
                        text.push_str(&row.text(col));
                    }
                }
                col += 1;
            }
            let font_style = match (style.bold, style.italic) {
                (false, false) => FontStyle::Regular,
                (true, false) => FontStyle::Bold,
                (false, true) => FontStyle::Italic,
                (true, true) => FontStyle::BoldItalic,
            };
            if MODE == FIND_RUNS {
                std::hint::black_box((text.as_str(), font_style, sources.as_slice(), clip));
                work += 1;
                continue;
            }
            let glyphs = if MODE == SHAPE_RUNS {
                self.shape::<false>(text, font_style)?
            } else {
                self.shape::<true>(text, font_style)?
            };
            if MODE == SHAPE_RUNS {
                work += std::hint::black_box(glyphs.as_ref()).len();
                continue;
            }
            let source = |g: &ShapedGlyph| {
                sources
                    .partition_point(|(byte, _)| *byte <= g.cluster)
                    .saturating_sub(1)
            };
            // Source indices are dense within this run, including wide cells.
            anchors.clear();
            anchors.resize(sources.len(), span.explicit.then_some(0.0));
            for glyph in glyphs.iter() {
                if glyph.advance > 0.0 {
                    anchors[source(glyph)].get_or_insert(glyph.x);
                }
            }
            for glyph in glyphs.iter() {
                let source = source(glyph);
                // Mark-only clusters anchor at their first glyph, even an empty bitmap.
                let anchor = *anchors[source].get_or_insert(glyph.x);
                let cached = self.glyph(glyph)?;
                if cached.size.contains(&0) {
                    continue;
                }
                let col = sources[source].1;
                let x = options.padding[0] + col as f32 * metrics.cell_width as f32 + glyph.x
                    - anchor
                    + cached.bearing[0] as f32;
                let y = top + metrics.baseline - glyph.y - cached.bearing[1] as f32;
                let quad = Quad {
                    offset: [0.0; 2],
                    rect: [x, y, cached.size[0] as f32, cached.size[1] as f32],
                    uv: cached.uv,
                    color,
                    paint: if cached.color {
                        Paint::Color
                    } else {
                        Paint::Mask
                    },
                    atlas: cached.atlas,
                };
                if let Some(clip) = clip {
                    let end = (span.start + usize::from(span.width)).min(paints.len());
                    if paints[span.start..end].iter().all(|&paint| paint == color) {
                        frame.quads.extend(quad.clipped([0.0; 2], clip));
                    } else {
                        // Keep atomic text shaped once, but honor cell-local cursor colors.
                        let mut start = span.start;
                        while start < end {
                            let color = paints[start];
                            let mut next = start + 1;
                            while next < end && paints[next] == color {
                                next += 1;
                            }
                            let left =
                                options.padding[0] + start as f32 * metrics.cell_width as f32;
                            let right =
                                options.padding[0] + next as f32 * metrics.cell_width as f32;
                            let mut colored = quad.clone();
                            colored.color = color;
                            frame
                                .quads
                                .extend(colored.clipped([0.0; 2], [left, clip[1], right, clip[3]]));
                            start = next;
                        }
                    }
                } else {
                    frame.quads.push(quad);
                }
            }
        }
        Ok(work)
    }

    fn shape<const CACHE: bool>(
        &mut self,
        text: &str,
        style: FontStyle,
    ) -> Result<Arc<[ShapedGlyph]>, FontError> {
        if CACHE && let Some(glyphs) = self.shaped[style as usize].get(text) {
            return Ok(glyphs.clone());
        }
        let glyphs: Arc<[ShapedGlyph]> = self.fonts.shape(text, style)?.into();
        let bytes = text.len()
            + std::mem::size_of_val(&*glyphs)
            + std::mem::size_of::<(String, Arc<[ShapedGlyph]>)>();
        if CACHE && bytes <= MAX_SHAPED_BYTES {
            // ponytail: clear the bounded run cache on overflow; use LRU eviction
            // if a working set larger than 4 MiB makes repeated eviction measurable.
            if self.shaped_bytes + bytes > MAX_SHAPED_BYTES {
                for cache in &mut self.shaped {
                    cache.clear();
                }
                self.shaped_bytes = 0;
            }
            self.shaped_bytes += bytes;
            self.shaped[style as usize].insert(text.to_owned(), glyphs.clone());
        }
        Ok(glyphs)
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
        for (&index, page) in self.pages.iter_mut().filter(|(_, p)| p.image == image) {
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
                .values()
                .filter(|p| p.image == image)
                .map(|p| u64::from(p.size).pow(2) * 4)
                .sum::<u64>();
            let budget = if image {
                MAX_IMAGE_ATLAS_BYTES
            } else {
                MAX_GLYPH_ATLAS_BYTES
            };
            if bytes + u64::from(required).pow(2) * 4 > budget as u64 {
                return Err(RenderError::AtlasCapacity);
            }
            let index = self.next_page;
            self.next_page += 1;
            let mut page = Page {
                x: 0,
                y: 0,
                row_height: 0,
                size: required,
                image,
            };
            let origin = reserve(&mut page, width + 2, height + 2).expect("new page fits bitmap");
            self.pages.insert(index, page);
            (index, origin)
        };
        let origin = [origin[0] + 1, origin[1] + 1];
        let size = self.pages[&page].size as f32;
        cached.atlas = page;
        cached.uv = [
            origin[0] as f32 / size,
            origin[1] as f32 / size,
            (origin[0] + width) as f32 / size,
            (origin[1] + height) as f32 / size,
        ];
        self.next_revision += 1;
        self.uploads.push(AtlasUpload {
            revision: self.next_revision,
            page,
            page_size: self.pages[&page].size,
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

fn extra_cursor_colors(
    colors: ExtraColors,
    foreground: [u8; 3],
    background: [u8; 3],
    options: &RenderOptions,
) -> ([u8; 3], [u8; 3]) {
    if colors.cursor == ExtraColor::Special {
        return (foreground, background);
    }
    let resolve = |color, default, special| match color {
        ExtraColor::Default => default,
        ExtraColor::Special => special,
        ExtraColor::Indexed(index) => options.palette[usize::from(index)],
        ExtraColor::Rgb(r, g, b) => [r, g, b],
    };
    (
        resolve(colors.cursor, options.cursor_color, foreground),
        resolve(colors.text, options.cursor_text, background),
    )
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

    fn cached_matches_rebuild(
        renderer: &mut Renderer,
        terminal: &Terminal,
        options: &RenderOptions,
        cache: &mut RowCache,
    ) -> Frame {
        let source = terminal.screen().render_key();
        let screen = terminal.screen().snapshot_viewport();
        assert_ne!(source.identity, screen.render_identity());
        let cached = renderer
            .prepare_cached(&screen, source, options, cache)
            .unwrap();
        // Keep the atlas intact: UVs, upload histories and every quad must agree exactly.
        let rebuilt = renderer.prepare(&screen, options).unwrap();
        assert_eq!(cached.size, rebuilt.size);
        assert_eq!(cached.generation, rebuilt.generation);
        assert_eq!(cached.quads, rebuilt.quads);
        assert_eq!(cached.atlas_uploads, rebuilt.atlas_uploads);
        assert_eq!(cached.ime_cursor, rebuilt.ime_cursor);
        assert_eq!(cached.cursor_row_bounds, rebuilt.cursor_row_bounds);
        assert_eq!(cached.blinking_text, rebuilt.blinking_text);
        cached
    }

    fn extra_rects(frame: &Frame) -> Vec<[f32; 4]> {
        frame
            .quads
            .iter()
            .filter(|q| q.paint == Paint::Solid && q.color == Color::rgb([230, 120, 45]))
            .map(|q| q.rect)
            .collect()
    }

    fn extra_color(terminal: &mut Terminal) {
        terminal.feed(b"\x1b[>40;2:230:120:45 q");
    }

    #[test]
    fn extra_cursor_shapes_blink_and_host_visibility_ignore_main_visibility_and_preedit() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let metrics = renderer.metrics();
        let (w, h) = (metrics.cell_width as f32, metrics.cell_height as f32);
        let mut terminal = Terminal::new(4, 2, 0);
        terminal.screen_mut().cursor.visible = false;
        terminal.screen_mut().cursor.shape = CursorShape::Bar;
        terminal.screen_mut().cursor.blink = true;
        extra_color(&mut terminal);
        terminal.feed(b"\x1b[>1;2:1:1 q\x1b[>2;2:1:3 q\x1b[>3;2:2:2 q\x1b[>29;2:2:4 q");
        let mut options = RenderOptions {
            padding: [0.0; 2],
            cursor_visible: false,
            ..Default::default()
        };
        let mut cache = RowCache::default();
        let frame = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(
            extra_rects(&frame),
            vec![
                [0.0, 0.0, w, h],
                [2.0 * w, 0.0, 2.0, h],
                [w, 2.0 * h - 2.0, w, 2.0],
                [3.0 * w, h, 2.0, h]
            ]
        );
        options.preedit = Some(Preedit {
            text: "compose".into(),
            selection: None,
        });
        let composed = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(extra_rects(&composed), extra_rects(&frame));
        options.blink_visible = false;
        assert!(
            extra_rects(&cached_matches_rebuild(
                &mut renderer,
                &terminal,
                &options,
                &mut cache
            ))
            .is_empty()
        );
        options.focused = false;
        let unfocused = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(
            extra_rects(&unfocused).len(),
            7,
            "explicit shapes remain; follow-main becomes hollow"
        );
        options.extra_cursor_visible = false;
        assert!(
            extra_rects(&cached_matches_rebuild(
                &mut renderer,
                &terminal,
                &options,
                &mut cache
            ))
            .is_empty()
        );
    }

    #[test]
    fn cached_extra_cursors_rebuild_only_affected_rows_and_main_cursor_wins_overlap() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let mut terminal = Terminal::new(10, 4, 20);
        terminal.feed(b"first\r\nsecond\r\nthird\r\nfourth\x1b[?25l");
        extra_color(&mut terminal);
        terminal.feed(b"\x1b[>1;2:1:2;2:3:4 q");
        let options = RenderOptions::default();
        let mut cache = RowCache::default();
        for _ in 0..3 {
            cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        }
        assert_eq!(cache.prepared_rows, 8);
        terminal.feed(b"\x1b[>2;2:1:2 q");
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 9);
        terminal.feed(b"\x1b[>30;2:1:2:3 q");
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 11);
        terminal.feed(b"\x1b[?25h\x1b[1;2H");
        for shape in [
            CursorShape::Block,
            CursorShape::Bar,
            CursorShape::Underline,
            CursorShape::HollowBlock,
        ] {
            terminal.screen_mut().cursor.shape = shape;
            let with_extra = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
            let mut without = terminal.clone();
            without.feed(b"\x1b[>0;2:1:2 q");
            let main_only = renderer.prepare(without.screen(), &options).unwrap();
            assert_eq!(with_extra.quads, main_only.quads, "main shape {shape:?}");
        }
        let mut second = Terminal::new(10, 4, 0);
        second.feed(b"different\x1b[>3;4 q");
        let mut second_cache = RowCache::default();
        cached_matches_rebuild(&mut renderer, &second, &options, &mut second_cache);
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
    }

    #[test]
    fn extra_cursors_cover_wide_and_explicit_tails_and_deduplicate_adjacent_shapes() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let options = RenderOptions {
            padding: [0.0; 2],
            cursor_visible: false,
            ..Default::default()
        };
        for (source, width) in [("界Z", 2), ("\x1b]66;w=3;abcdef\x1b\\Z", 3)] {
            let mut terminal = Terminal::new(8, 2, 0);
            terminal.feed(source.as_bytes());
            extra_color(&mut terminal);
            let mut cache = RowCache::default();
            for shape in [1, 2, 3] {
                terminal.feed(format!("\x1b[>0;4 q\x1b[>{shape};2:1:1 q").as_bytes());
                let head = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
                for col in 2..=width {
                    terminal.feed(format!("\x1b[>0;4 q\x1b[>{shape};2:1:{col} q").as_bytes());
                    let tail =
                        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
                    assert_eq!(
                        head.quads, tail.quads,
                        "source={source:?} shape={shape} col={col}"
                    );
                }
                terminal.feed(format!("\x1b[>{shape};4:1:1:1:{width} q").as_bytes());
                let adjacent =
                    cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
                assert_eq!(
                    head.quads, adjacent.quads,
                    "duplicate positions in one span do not overdraw"
                );
                terminal.feed(format!("\x1b[>{shape};2:1:{} q", width + 1).as_bytes());
                let neighbor =
                    cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
                assert_eq!(extra_rects(&neighbor).len(), extra_rects(&head).len() + 1);
            }
            terminal.feed(b"\x1b[>0;4 q\x1b[>1;2:1:1 q\x1b[>2;2:1:2 q");
            let mixed = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
            assert_eq!(
                extra_rects(&mixed).len(),
                width + 1,
                "overlapping block and beam both survive"
            );
        }
    }

    #[test]
    fn extra_cursor_recolors_a_wide_tail_when_the_main_beam_covers_its_head() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let mut terminal = Terminal::new(8, 2, 0);
        terminal.feed("A界\x1b[1;2H\x1b[>1;2:1:3 q".as_bytes());
        terminal.screen_mut().cursor.shape = CursorShape::Bar;
        let options = RenderOptions {
            padding: [0.0; 2],
            ..Default::default()
        };
        let mut cache = RowCache::default();
        let frame = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        let w = renderer.metrics().cell_width as f32;
        let tail: Vec<_> = frame
            .quads
            .iter()
            .filter(|q| q.paint == Paint::Mask && q.color == Color::rgb(options.cursor_text))
            .collect();
        assert!(!tail.is_empty());
        assert!(
            tail.iter()
                .all(|q| q.rect[0] >= 2.0 * w && q.rect[0] + q.rect[2] <= 3.0 * w)
        );
        assert!(frame.quads.contains(&Quad::solid(
            [w, 0.0, 2.0, renderer.metrics().cell_height as f32],
            Color::rgb(options.cursor_color)
        )));
    }

    #[test]
    fn extra_cursor_colors_follow_defaults_palette_and_cell_local_reverse_video() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let options = RenderOptions {
            padding: [0.0; 2],
            cursor_visible: false,
            ..Default::default()
        };
        let mut terminal = Terminal::new(8, 2, 0);
        terminal.feed(b"\x1b]66;w=3;MMMM\x1b\\");
        let natural = renderer.prepare(terminal.screen(), &options).unwrap();
        let backgrounds = [[3, 60, 100], [50, 4, 110], [80, 90, 5]];
        let foregrounds = [[210, 11, 12], [13, 220, 14], [15, 16, 230]];
        for col in 0..3 {
            let mut style = terminal.screen().row(0).style(col);
            let [r, g, b] = foregrounds[col];
            style.foreground = TerminalColor::Rgb(r, g, b);
            let [r, g, b] = backgrounds[col];
            style.background = TerminalColor::Rgb(r, g, b);
            terminal.screen_mut().set_cell_style(0, col, style);
        }
        terminal.feed(b"\x1b[>40;1 q\x1b[>30;2:255:0:255 q\x1b[>1;2:1:2 q");
        let mut cache = RowCache::default();
        let reversed = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        let metrics = renderer.metrics();
        let w = metrics.cell_width as f32;
        let h = metrics.cell_height as f32;
        for (col, color) in foregrounds.into_iter().enumerate() {
            assert!(
                reversed
                    .quads
                    .contains(&Quad::solid([col as f32 * w, 0.0, w, h], Color::rgb(color)))
            );
        }
        let expected: Vec<_> = natural
            .quads
            .iter()
            .filter(|q| q.paint != Paint::Solid)
            .flat_map(|quad| {
                backgrounds
                    .into_iter()
                    .enumerate()
                    .filter_map(move |(col, color)| {
                        let mut colored = quad.clone();
                        colored.color = Color::rgb(color);
                        colored.clipped([0.0; 2], [col as f32 * w, 0.0, (col + 1) as f32 * w, h])
                    })
            })
            .collect();
        assert_eq!(
            reversed
                .quads
                .iter()
                .filter(|q| q.paint != Paint::Solid)
                .cloned()
                .collect::<Vec<_>>(),
            expected
        );
        for (colors, expected) in [
            (
                ExtraColors {
                    cursor: ExtraColor::Default,
                    text: ExtraColor::Default,
                },
                (options.cursor_color, options.cursor_text),
            ),
            (
                ExtraColors {
                    cursor: ExtraColor::Indexed(1),
                    text: ExtraColor::Indexed(2),
                },
                (options.palette[1], options.palette[2]),
            ),
            (
                ExtraColors {
                    cursor: ExtraColor::Rgb(1, 2, 3),
                    text: ExtraColor::Special,
                },
                ([1, 2, 3], backgrounds[0]),
            ),
        ] {
            assert_eq!(
                extra_cursor_colors(colors, foregrounds[0], backgrounds[0], &options),
                expected
            );
        }
    }

    #[test]
    fn extra_cursors_stay_in_viewport_coordinates_and_cover_short_history_rows() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let mut terminal = Terminal::new(8, 2, 20);
        terminal.feed(b"old\r\nsecond\r\nthird\r\nfourth");
        extra_color(&mut terminal);
        terminal.feed(b"\x1b[>1;2:1:7 q");
        let options = RenderOptions {
            padding: [0.0; 2],
            cursor_visible: false,
            ..Default::default()
        };
        let mut cache = RowCache::default();
        let live = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        terminal.screen_mut().viewport_offset = 2;
        let scrolled = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(extra_rects(&scrolled), extra_rects(&live));

        // Imported/partially reflowed history can have fewer physical cells than
        // the logical viewport. A cursor beyond those cells overlays a blank.
        let mut short = Terminal::new(4, 2, 0);
        short.feed(b"short");
        *terminal.screen_mut() = short.screen().snapshot_viewport();
        terminal.screen_mut().columns = 8;
        extra_color(&mut terminal);
        terminal.feed(b"\x1b[>1;2:1:7 q");
        assert_eq!(terminal.screen().row(0).cells.len(), 4);
        let frame = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(extra_rects(&frame), extra_rects(&live));
    }

    #[test]
    fn cached_rows_follow_edits_selection_settings_sources_and_atlas_resets() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let mut terminal = Terminal::new(32, 4, 100);
        terminal.feed("first ffi 界\r\nsecond e\u{301}\r\nthird\r\nfourth".as_bytes());
        let mut options = RenderOptions {
            cursor_visible: false,
            ..Default::default()
        };
        let mut cache = RowCache::default();
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 4);
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 8);
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(
            cache.prepared_rows, 8,
            "new snapshot identity must still reuse rows"
        );

        terminal.screen_mut().set_cell_text(1, 3, "X");
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 9);
        let mut style = terminal.screen().row(2).style(0);
        style.foreground = TerminalColor::Indexed(1);
        terminal.screen_mut().set_cell_style(2, 0, style);
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 11);
        terminal.screen_mut().selection = Some(Selection {
            start: GridPoint {
                row: terminal.screen().row(0).id,
                col: 1,
            },
            end: GridPoint {
                row: terminal.screen().row(1).id,
                col: 8,
            },
            rectangular: false,
        });
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 14);
        options.palette[1] = [12, 34, 56];
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 18);
        options.search_highlights.push(SearchHighlight {
            row: terminal.screen().row(3).id,
            columns: 2..=4,
            selected: true,
        });
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 22);
        terminal = terminal.clone();
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 26);
        renderer.clear_cache();
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 30);
        let previous_generation = renderer.generation();
        renderer = Renderer::new(FontConfig::default()).unwrap();
        assert_ne!(renderer.generation(), previous_generation);
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 34);
        terminal.resize(24, 5);
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 39);
        assert_eq!(cache.rows.len(), 5);
    }

    #[test]
    fn cached_rows_scroll_and_keep_two_panes_with_overlapping_row_ids_independent() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let mut first = Terminal::new(32, 4, 100);
        let mut second = Terminal::new(32, 4, 100);
        for index in 0..12 {
            first.feed(format!("\x1b[4:3mfirst {index} 界\x1b[m\r\n").as_bytes());
            second.feed(format!("\x1b[44msecond {index} ffi\x1b[m\r\n").as_bytes());
        }
        assert_eq!(first.screen().row(0).id, second.screen().row(0).id);
        let options = RenderOptions {
            cursor_visible: false,
            padding: [3.25, 5.5],
            ..Default::default()
        };
        let mut caches = [RowCache::default(), RowCache::default()];
        for _ in 0..3 {
            cached_matches_rebuild(&mut renderer, &first, &options, &mut caches[0]);
            cached_matches_rebuild(&mut renderer, &second, &options, &mut caches[1]);
        }
        assert_eq!([caches[0].prepared_rows, caches[1].prepared_rows], [8, 8]);
        for (delta, expected) in [(1isize, 1), (2, 3), (-1, 2), (-2, 3)] {
            let before = caches[0].prepared_rows;
            first.screen_mut().scroll_viewport(delta);
            cached_matches_rebuild(&mut renderer, &first, &options, &mut caches[0]);
            assert_eq!(caches[0].prepared_rows - before, expected);
            assert_eq!(
                caches[0].rows.len(),
                4,
                "old viewport rows must be released"
            );
            cached_matches_rebuild(&mut renderer, &second, &options, &mut caches[1]);
            assert_eq!(caches[1].prepared_rows, 8);
        }
    }

    #[test]
    fn cached_rows_track_cursor_and_text_blink_with_fresh_preedit_and_graphics() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let mut terminal = Terminal::new(32, 4, 100);
        terminal.feed(b"first ffi\r\n\x1b[5;4mblinking\x1b[m\r\nthird\r\nfourth\x1b[H");
        terminal.screen_mut().cursor.blink = true;
        let mut options = RenderOptions::default();
        let mut cache = RowCache::default();
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 4);
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 8);
        terminal.screen_mut().cursor.col = 3;
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 9);
        terminal.screen_mut().cursor.row = 2;
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(
            cache.prepared_rows, 11,
            "rebuild both old and new cursor rows"
        );
        options.blink_visible = false;
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(
            cache.prepared_rows, 13,
            "only cursor and blinking text changed"
        );
        options.blink_visible = true;
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 15);
        options.preedit = Some(Preedit {
            text: "日本".into(),
            selection: Some((0, 6)),
        });
        let frame = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert!(frame.ime_cursor.is_some());
        assert_eq!(cache.prepared_rows, 16, "preedit hides the terminal cursor");
        options.preedit.as_mut().unwrap().text.push('語');
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(
            cache.prepared_rows, 16,
            "preedit text is prepared separately"
        );
        options.preedit = None;
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 17);
        options.focused = false;
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        options.cursor_visible = false;
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        let plain = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        let before = cache.prepared_rows;
        terminal.feed(b"\x1b_Ga=T,f=32,s=1,v=1,i=1,C=1,z=-1;/wAA/w==\x1b\\");
        let graphics = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, before);
        assert_ne!(plain.quads, graphics.quads);
        terminal.feed(b"\x1b_Ga=d,d=A\x1b\\");
        let deleted = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, before);
        assert_eq!(plain.quads, deleted.quads);
    }

    #[test]
    fn cached_rows_rebuild_after_atlas_capacity_retry() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let mut terminal = Terminal::new(16, 2, 0);
        terminal.feed(b"first\r\nsecond");
        let options = RenderOptions {
            cursor_visible: false,
            ..Default::default()
        };
        let mut cache = RowCache::default();
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        let before = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 4);
        // Exhaust atlas metadata without allocating a 64 MiB test image.
        for page in renderer.pages.values_mut() {
            page.size = 4096;
            page.x = page.size;
            page.y = page.size;
            page.row_height = page.size;
        }
        terminal.screen_mut().set_cell_text(1, 0, "Z");
        let after = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_ne!(before.generation, after.generation);
        assert_eq!(
            cache.prepared_rows, 6,
            "retry must rebuild the earlier cached row too"
        );
    }

    #[test]
    fn cached_rows_admit_geometry_after_repaint_and_resize_settle() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let mut terminal = Terminal::new(32, 4, 0);
        terminal.feed(b"\x1b[?1049h");
        let options = RenderOptions {
            cursor_visible: false,
            ..Default::default()
        };
        let mut cache = RowCache::default();
        for text in ["first", "other", "third"] {
            for row in 0..4 {
                terminal.screen_mut().set_cell_text(row, 0, text);
            }
            cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
            assert!(cache.rows.values().all(|row| !row.valid));
            assert!(cache.rows.values().all(|row| row.background.capacity() == 0
                && row.foreground.capacity() == 0
                && row.quad_counts[1] > 0));
        }
        assert_eq!(cache.prepared_rows, 12);

        // Stable rows become reusable while a repeatedly changed status row stays uncached.
        terminal.screen_mut().set_cell_text(0, 0, "-");
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 16);
        assert_eq!(cache.rows.values().filter(|row| row.valid).count(), 3);
        terminal.screen_mut().set_cell_text(0, 0, "+");
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 17);
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 18);
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 18);
        assert!(cache.rows.values().all(|row| row.valid));

        for row in 0..4 {
            terminal.screen_mut().set_cell_text(row, 0, "repaint");
        }
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 22);
        assert!(
            cache
                .rows
                .values()
                .all(|row| !row.valid && row.foreground.capacity() > 0)
        );
        terminal.resize(24, 5);
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 27);
        assert_eq!(cache.rows.len(), 5);
        assert!(cache.rows.values().all(|row| !row.valid));
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 32);
        assert!(cache.rows.values().all(|row| row.valid));
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 32);
    }

    #[test]
    fn explicit_width_runs_keep_native_shaping_and_clip_before_the_next_cell() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let options = RenderOptions {
            padding: [0.0; 2],
            cursor_visible: false,
            ..Default::default()
        };
        for (text, width) in [
            ("ffi", 3),
            ("e\u{301}", 1),
            ("אבג", 3),
            ("العربية", 4),
            ("👩🏽‍💻", 2),
            ("  X", 3),
            ("abcdef", 1),
            ("─", 7),
        ] {
            let mut terminal = Terminal::new(20, 2, 0);
            terminal.feed(format!("\x1b]66;w={width};{text}\x07\x1b[31mZ").as_bytes());
            assert_eq!(terminal.screen().row(0).text(0).as_str(), text);
            let frame = cached_matches_rebuild(
                &mut renderer,
                &terminal,
                &options,
                &mut RowCache::default(),
            );
            let metrics = renderer.metrics();
            let mut natural = Frame::empty(options.size);
            natural.generation = renderer.generation();
            // Native run positions are the oracle, including RTL order and combining offsets.
            for glyph in renderer.fonts.shape(text, FontStyle::Regular).unwrap() {
                let cached = renderer.glyph(&glyph).unwrap();
                if cached.size.contains(&0) {
                    continue;
                }
                natural.quads.push(Quad {
                    offset: [0.0; 2],
                    rect: [
                        glyph.x + cached.bearing[0] as f32,
                        metrics.baseline - glyph.y - cached.bearing[1] as f32,
                        cached.size[0] as f32,
                        cached.size[1] as f32,
                    ],
                    uv: cached.uv,
                    color: Color::rgb(options.foreground),
                    paint: if cached.color {
                        Paint::Color
                    } else {
                        Paint::Mask
                    },
                    atlas: cached.atlas,
                });
            }
            let mut expected = Frame::empty(options.size);
            expected
                .append_clipped(
                    &natural,
                    [0.0; 2],
                    [
                        0.0,
                        0.0,
                        width as f32 * metrics.cell_width as f32,
                        metrics.cell_height as f32,
                    ],
                )
                .unwrap();
            let actual: Vec<_> = frame
                .quads
                .iter()
                .filter(|q| q.paint != Paint::Solid && q.color == Color::rgb(options.foreground))
                .cloned()
                .collect();
            assert_eq!(
                actual, expected.quads,
                "explicit text {text:?}, width {width}"
            );

            let mut neighbor = Terminal::new(20, 2, 0);
            neighbor.feed(format!("\x1b[1;{}H\x1b[31mZ", width + 1).as_bytes());
            let reference = renderer.prepare(neighbor.screen(), &options).unwrap();
            let red = |frame: &Frame| {
                frame
                    .quads
                    .iter()
                    .filter(|q| q.paint == Paint::Mask && q.color == Color::rgb(options.palette[1]))
                    .cloned()
                    .collect::<Vec<_>>()
            };
            assert_eq!(red(&frame), red(&reference), "neighbor after {text:?}");
        }
    }

    #[test]
    fn explicit_width_cursors_selection_search_and_decorations_cover_the_span() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let mut terminal = Terminal::new(16, 2, 0);
        terminal.feed(b"L\x1b[4m\x1b]66;w=3;ffi\x07\x1b[mR");
        let mut options = RenderOptions {
            cursor_visible: false,
            ..Default::default()
        };
        let mut cache = RowCache::default();
        let plain = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        let cell = renderer.metrics().cell_width as f32;
        assert_eq!(
            plain
                .quads
                .iter()
                .filter(|q| q.paint == Paint::Solid
                    && q.color == Color::rgb(options.foreground)
                    && q.rect[2] == 3.0 * cell)
                .count(),
            1
        );
        options.cursor_visible = true;
        for focused in [false, true] {
            options.focused = focused;
            for shape in [
                CursorShape::Block,
                CursorShape::Bar,
                CursorShape::Underline,
                CursorShape::HollowBlock,
            ] {
                terminal.screen_mut().cursor.shape = shape;
                let mut owner = None;
                for col in 1..4 {
                    terminal.screen_mut().cursor.col = col;
                    let frame =
                        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
                    assert_eq!(terminal.screen().cursor.col, col);
                    if let Some(previous) = &owner {
                        assert_eq!(
                            &frame.quads, previous,
                            "cursor at span column {col}, {shape:?}"
                        );
                    } else {
                        owner = Some(frame.quads);
                    }
                }
            }
        }
        options.cursor_visible = false;
        let row = terminal.screen().row(0).id;
        terminal.screen_mut().selection = Some(Selection {
            start: GridPoint { row, col: 2 },
            end: GridPoint { row, col: 2 },
            rectangular: false,
        });
        let selected = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        let backgrounds = |frame: &Frame, color| {
            frame
                .quads
                .iter()
                .filter(|q| q.paint == Paint::Solid && q.color == Color::rgb(color))
                .map(|q| q.rect)
                .collect::<Vec<_>>()
        };
        let expected = (1..4)
            .map(|col| {
                [
                    options.padding[0] + col as f32 * cell,
                    options.padding[1],
                    cell,
                    renderer.metrics().cell_height as f32,
                ]
            })
            .collect::<Vec<_>>();
        assert_eq!(
            backgrounds(&selected, options.selection_background),
            expected
        );
        terminal.screen_mut().selection = None;
        options.search_highlights.push(SearchHighlight {
            row,
            columns: 1..=1,
            selected: true,
        });
        let searched = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(backgrounds(&searched, [242, 165, 126]), expected);
    }

    #[test]
    fn explicit_width_metadata_invalidates_cached_rows_with_identical_fallback_cells() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let mut terminal = Terminal::new(16, 2, 0);
        terminal.feed(b"\x1b]66;w=1;MMMM\x07");
        let options = RenderOptions {
            cursor_visible: false,
            ..Default::default()
        };
        let mut cache = RowCache::default();
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        let before = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        let fallback = terminal.screen().row(0).cells[0].bits();
        let revision = terminal.screen().row(0).revision();
        terminal.feed(b"\r\x1b]66;w=1;iiii\x07");
        assert_eq!(terminal.screen().row(0).cells[0].bits(), fallback);
        assert_ne!(terminal.screen().row(0).revision(), revision);
        let changed = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_ne!(before.quads, changed.quads);
        assert_eq!(cache.prepared_rows, 5);
        terminal.feed(b"\r\x1b]66;w=2;iiii\x07");
        let widened = cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_ne!(changed.quads, widened.quads);
        assert_eq!(cache.prepared_rows, 6);
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        cached_matches_rebuild(&mut renderer, &terminal, &options, &mut cache);
        assert_eq!(cache.prepared_rows, 7);
    }

    #[test]
    fn explicit_width_clipping_adjusts_texture_coordinates() {
        let quad = Quad {
            offset: [0.0; 2],
            rect: [-1.0, -2.0, 8.0, 12.0],
            uv: [0.125, 0.25, 0.625, 0.75],
            color: Color::rgb([255; 3]),
            paint: Paint::Mask,
            atlas: 3,
        };
        let clipped = quad.clipped([0.0; 2], [1.0, 1.0, 5.0, 7.0]).unwrap();
        assert_eq!(clipped.rect, [1.0, 1.0, 4.0, 6.0]);
        assert_eq!(clipped.uv, [0.25, 0.375, 0.5, 0.625]);
        assert!(quad.clipped([0.0; 2], [8.0, 1.0, 12.0, 7.0]).is_none());
    }

    #[test]
    fn blank_tail_paint_matches_explicit_spaces() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        for (cursor_visible, focused, blink_visible) in [
            (false, true, true),
            (true, true, true),
            (true, false, true),
            (true, true, false),
        ] {
            for selected in [false, true] {
                for colored in [false, true] {
                    let options = RenderOptions {
                        cursor_visible,
                        focused,
                        blink_visible,
                        ..RenderOptions::default()
                    };
                    let frames = [false, true].map(|padded| {
                        let mut terminal = Terminal::new(8, 2, 0);
                        terminal.feed(if padded {
                            b"A       \r\n        \x1b[H"
                        } else {
                            b"A"
                        });
                        if colored {
                            // Erased colored cells have no codepoint, but
                            // their background must remain visible.
                            terminal.feed(b"\x1b[2;5H\x1b[44m\x1b[X\x1b[0m");
                        }
                        terminal.feed(b"\x1b[2;8H");
                        terminal.screen_mut().cursor.blink = true;
                        if selected {
                            terminal.screen_mut().selection = Some(Selection {
                                start: GridPoint {
                                    row: terminal.screen().row(0).id,
                                    col: 6,
                                },
                                end: GridPoint {
                                    row: terminal.screen().row(1).id,
                                    col: 7,
                                },
                                rectangular: false,
                            });
                        }
                        renderer.prepare(terminal.screen(), &options).unwrap()
                    });
                    assert_eq!(
                        frames[0].quads, frames[1].quads,
                        "cursor={cursor_visible} focused={focused} blink={blink_visible} selection={selected} background={colored}"
                    );
                    assert_eq!(frames[0].blinking_text, frames[1].blinking_text);
                }
            }
        }
    }

    #[test]
    fn empty_tails_skip_shaping_without_moving_unicode_cursor_or_selection() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        for text in [
            "ffi",
            "e\u{301}界",
            "العربية",
            "עברית 123",
            "abc العربية 123",
            "👩🏽‍💻",
        ] {
            renderer.clear_cache();
            let mut terminal = Terminal::new(40, 2, 0);
            terminal.feed(format!("\x1b[?2027h{text}").as_bytes());
            let col = terminal.screen().cursor.col;
            terminal.screen_mut().selection = Some(Selection {
                start: GridPoint {
                    row: terminal.screen().row(0).id,
                    col: 24,
                },
                end: GridPoint {
                    row: terminal.screen().row(1).id,
                    col: 16,
                },
                rectangular: false,
            });
            let options = RenderOptions::default();
            let sparse = renderer.prepare(terminal.screen(), &options).unwrap();
            assert!(
                renderer
                    .shaped
                    .iter()
                    .flat_map(HashMap::keys)
                    .all(|text| !text.trim().is_empty()),
                "empty row tails were shaped for {text:?}"
            );
            // Explicit spaces used to share the same shaping path as empty
            // cells. They must still produce identical glyph positions.
            terminal.feed(b"\x1b7");
            terminal.feed(" ".repeat(40 - col).as_bytes());
            terminal.feed(b"\x1b[2;1H");
            terminal.feed(" ".repeat(40).as_bytes());
            terminal.feed(b"\x1b8");
            let padded = renderer.prepare(terminal.screen(), &options).unwrap();
            assert_eq!(sparse.quads, padded.quads, "{text:?}");
        }
    }

    #[test]
    fn glyph_anchors_preserve_marks_wide_cells_and_unordered_clusters() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let glyph = renderer.fonts.shape("a", FontStyle::Regular).unwrap()[0].clone();
        let bearing = renderer.glyph(&glyph).unwrap().bearing[0] as f32;
        // The first cluster-3 glyph is a mark before its advancing base.
        // Cluster 6 has only marks and anchors at its first glyph, even if empty.
        let mut glyphs: Vec<_> = [
            (3, 20.0, 0.0),
            (0, 80.0, 8.0),
            (3, 24.0, 8.0),
            (3, 27.0, 0.0),
            (6, 11.0, 0.0),
            (6, 12.0, 0.0),
        ]
        .into_iter()
        .map(|(cluster, x, advance)| ShapedGlyph {
            cluster,
            x,
            advance,
            ..glyph.clone()
        })
        .collect();
        let empty = renderer.fonts.shape(" ", FontStyle::Regular).unwrap()[0].clone();
        assert!(renderer.glyph(&empty).unwrap().size.contains(&0));
        glyphs.insert(
            4,
            ShapedGlyph {
                cluster: 6,
                x: 9.0,
                advance: 0.0,
                ..empty
            },
        );
        renderer.shaped[FontStyle::Regular as usize].insert("a\u{301}界b".into(), glyphs.into());
        let mut terminal = Terminal::new(4, 1, 0);
        terminal.feed("\x1b[?2027ha\u{301}界b".as_bytes());
        let options = RenderOptions::default();
        let mut scratch = RowScratch {
            paints: vec![Color::rgb([255; 3]); 4],
            ..RowScratch::default()
        };
        let mut frame = Frame::empty(options.size);
        renderer
            .row_text::<RENDER_TEXT>(
                terminal.screen().row(0),
                &mut scratch,
                0.0,
                &options,
                &mut frame,
            )
            .unwrap();
        let width = renderer.metrics().cell_width as f32;
        let positions: Vec<_> = frame
            .quads
            .iter()
            .map(|q| q.rect[0] - options.padding[0] - bearing)
            .collect();
        assert_eq!(
            positions,
            [
                width - 4.0,
                0.0,
                width,
                width + 3.0,
                3.0 * width + 2.0,
                3.0 * width + 3.0
            ]
        );
        // Reused scratch must not retain text, source offsets or glyph anchors.
        // Shifting native run positions leaves their cell-relative geometry fixed.
        for glyph in Arc::make_mut(
            renderer.shaped[FontStyle::Regular as usize]
                .get_mut("a\u{301}界b")
                .unwrap(),
        ) {
            glyph.x += 100.0;
        }
        let mut repeated = Frame::empty(options.size);
        renderer
            .row_text::<RENDER_TEXT>(
                terminal.screen().row(0),
                &mut scratch,
                0.0,
                &options,
                &mut repeated,
            )
            .unwrap();
        assert_eq!(frame.quads, repeated.quads);
    }

    #[test]
    fn blink_metadata_matches_visible_text_and_decorations() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let metrics = renderer.metrics();
        for (bytes, blinking) in [
            (b"\x1b[5mtext".as_slice(), true),
            (b"\x1b[6mtext", true),
            (b"\x1b[5;8mhidden", false),
            (b"\x1b[5m   ", false),
            (b"\x1b[5;4m ", true),
            (b"\x1b[5m\x1b[3;1HX", false),
            (b"\x1b[5m\x1b[1;4HX", false),
            (b"\x1b[5m\xf4\x8e\xbb\xae", false), // Kitty placeholder.
        ] {
            let mut terminal = Terminal::new(10, 3, 0);
            terminal.feed(bytes);
            let mut options = RenderOptions {
                size: [metrics.cell_width * 3, metrics.cell_height * 2],
                padding: [0.0; 2],
                cursor_visible: false,
                ..Default::default()
            };
            let shown = renderer.prepare(terminal.screen(), &options).unwrap();
            options.blink_visible = false;
            let hidden = renderer.prepare(terminal.screen(), &options).unwrap();
            assert_eq!(shown.blinking_text, blinking, "{bytes:?}");
            assert_eq!(hidden.blinking_text, blinking, "{bytes:?}");
            assert_eq!(shown.quads != hidden.quads, blinking, "{bytes:?}");
            let mut composed = Frame::empty(options.size);
            composed
                .append_clipped(&shown, [0.0; 2], [0.0, 0.0, 1000.0, 1000.0])
                .unwrap();
            assert_eq!(composed.blinking_text, blinking);
        }
    }

    #[test]
    fn focused_cursor_blinks_without_blinking_the_unfocused_outline() {
        let mut terminal = Terminal::new(10, 2, 0);
        terminal.feed(b"cursor\r");
        terminal.screen_mut().cursor.blink = true;
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        for shape in [
            CursorShape::Block,
            CursorShape::Bar,
            CursorShape::Underline,
            CursorShape::HollowBlock,
        ] {
            terminal.screen_mut().cursor.shape = shape;
            let mut options = RenderOptions::default();
            let shown = renderer.prepare(terminal.screen(), &options).unwrap();
            options.blink_visible = false;
            let hidden = renderer.prepare(terminal.screen(), &options).unwrap();
            options.cursor_visible = false;
            let without = renderer.prepare(terminal.screen(), &options).unwrap();
            assert_eq!(hidden.quads, without.quads, "hidden phase for {shape:?}");
            assert_ne!(shown.quads, hidden.quads);
            options.cursor_visible = true;
            options.focused = false;
            let outline = renderer.prepare(terminal.screen(), &options).unwrap();
            options.blink_visible = true;
            assert_eq!(
                outline.quads,
                renderer.prepare(terminal.screen(), &options).unwrap().quads
            );
        }
    }

    fn assert_cursor_bounds_cover_changes(shown: &Frame, hidden: &Frame) {
        let [ax, ay, aw, ah] = shown.cursor_row_bounds.unwrap();
        let [bx, by, bw, bh] = hidden.cursor_row_bounds.unwrap();
        let bounds = [
            ax.min(bx),
            ay.min(by),
            (ax + aw).max(bx + bw),
            (ay + ah).max(by + bh),
        ];
        let contains = |quad: &Quad| {
            let [x, y, w, h] = quad.rect;
            assert!(
                x >= bounds[0] - 0.001
                    && y >= bounds[1] - 0.001
                    && x + w <= bounds[2] + 0.001
                    && y + h <= bounds[3] + 0.001,
                "changed quad {quad:?} exceeds cursor bounds {bounds:?}"
            );
        };
        let mut unmatched = hidden.quads.clone();
        for quad in &shown.quads {
            if let Some(index) = unmatched.iter().position(|other| other == quad) {
                unmatched.swap_remove(index);
            } else {
                contains(quad);
            }
        }
        unmatched.iter().for_each(contains);
    }

    #[test]
    fn cursor_row_bounds_cover_both_phases_unicode_decorations_and_clipping() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let metrics = renderer.metrics();
        for shape in [
            CursorShape::Block,
            CursorShape::Bar,
            CursorShape::Underline,
            CursorShape::HollowBlock,
        ] {
            for text in ["ffi", "界e\u{301}", "a\u{301}界b"] {
                for col in [0, 1, 3] {
                    let mut terminal = Terminal::new(12, 4, 0);
                    terminal.feed(
                        format!("above\x1b[2;1H\x1b[4:3;9;53m{text}\x1b[0m\x1b[3;1Hbelow")
                            .as_bytes(),
                    );
                    terminal.screen_mut().cursor.row = 1;
                    terminal.screen_mut().cursor.col = col;
                    terminal.screen_mut().cursor.shape = shape;
                    terminal.screen_mut().cursor.blink = true;
                    let mut options = RenderOptions {
                        padding: [3.5, 5.25],
                        size: [metrics.cell_width * 3 + 4, metrics.cell_height * 2 + 2],
                        ..Default::default()
                    };
                    let shown = renderer.prepare(terminal.screen(), &options).unwrap();
                    options.blink_visible = false;
                    let hidden = renderer.prepare(terminal.screen(), &options).unwrap();
                    assert_cursor_bounds_cover_changes(&shown, &hidden);
                    let [x, y, w, h] = hidden.cursor_row_bounds.unwrap();
                    let cell = [
                        options.padding[0] + col as f32 * metrics.cell_width as f32,
                        options.padding[1] + metrics.cell_height as f32,
                    ];
                    assert!(x <= cell[0] && y <= cell[1]);
                    assert!(x + w >= cell[0] + metrics.cell_width as f32 - 0.001);
                    assert!(y + h >= cell[1] + metrics.cell_height as f32 - 0.001);
                }
            }
        }
    }

    #[test]
    fn cursor_row_bounds_include_ligature_overhang_outside_the_cell_row() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let metrics = renderer.metrics();
        let glyph = renderer.fonts.shape("a", FontStyle::Regular).unwrap()[0].clone();
        // Make ligature overhang deterministic across installed fonts.
        renderer.shaped[FontStyle::Regular as usize].insert(
            "ffi".into(),
            vec![ShapedGlyph {
                cluster: 0,
                x: 0.0,
                y: metrics.cell_height as f32 * 2.0,
                advance: metrics.cell_width as f32 * 3.0,
                ..glyph
            }]
            .into(),
        );
        let mut terminal = Terminal::new(12, 4, 0);
        terminal.feed(b"\x1b[3;1Hffi\x1b[3;2H");
        terminal.screen_mut().cursor.blink = true;
        let mut options = RenderOptions::default();
        let shown = renderer.prepare(terminal.screen(), &options).unwrap();
        options.blink_visible = false;
        let hidden = renderer.prepare(terminal.screen(), &options).unwrap();
        assert_cursor_bounds_cover_changes(&shown, &hidden);
        assert!(
            hidden.cursor_row_bounds.unwrap()[1] < options.padding[1] + metrics.cell_height as f32
        );
    }

    #[test]
    fn cursor_row_bounds_follow_cursor_visibility_except_blink_phase() {
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let mut terminal = Terminal::new(12, 3, 10);
        terminal.feed(b"cursor\r");
        terminal.screen_mut().cursor.blink = true;
        let mut options = RenderOptions {
            blink_visible: false,
            ..Default::default()
        };
        assert!(
            renderer
                .prepare(terminal.screen(), &options)
                .unwrap()
                .cursor_row_bounds
                .is_some()
        );
        options.cursor_visible = false;
        assert!(
            renderer
                .prepare(terminal.screen(), &options)
                .unwrap()
                .cursor_row_bounds
                .is_none()
        );
        options.cursor_visible = true;
        terminal.screen_mut().cursor.visible = false;
        assert!(
            renderer
                .prepare(terminal.screen(), &options)
                .unwrap()
                .cursor_row_bounds
                .is_none()
        );
        terminal.screen_mut().cursor.visible = true;
        terminal.screen_mut().viewport_offset = 1;
        assert!(
            renderer
                .prepare(terminal.screen(), &options)
                .unwrap()
                .cursor_row_bounds
                .is_none()
        );
        terminal.screen_mut().viewport_offset = 0;
        options.preedit = Some(Preedit {
            text: "compose".into(),
            selection: None,
        });
        assert!(
            renderer
                .prepare(terminal.screen(), &options)
                .unwrap()
                .cursor_row_bounds
                .is_none()
        );
        options.preedit = None;
        options.size[1] = 1;
        assert!(
            renderer
                .prepare(terminal.screen(), &options)
                .unwrap()
                .cursor_row_bounds
                .is_none()
        );
    }

    #[test]
    fn unchanged_runs_reuse_shaping_without_caching_color_or_cell_positions() {
        let mut terminal = Terminal::new(20, 2, 0);
        terminal.feed("水e\u{301} => 👩🏽‍💻".as_bytes());
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let mut options = RenderOptions {
            cursor_visible: false,
            ..Default::default()
        };
        let original = renderer.prepare(terminal.screen(), &options).unwrap();
        let cached = renderer.shaped.clone();
        assert!(cached.iter().any(|cache| !cache.is_empty()));
        options.padding = [20.0, 25.0];
        options.foreground = [13, 24, 35];
        let updated = renderer.prepare(terminal.screen(), &options).unwrap();
        for (old, new) in cached.iter().zip(&renderer.shaped) {
            assert_eq!(old.len(), new.len());
            for (text, glyphs) in old {
                assert!(Arc::ptr_eq(glyphs, &new[text]));
            }
        }
        assert_ne!(original.quads, updated.quads);
        renderer.clear_cache();
        assert_eq!(renderer.shaped_bytes, 0);
        assert_eq!(
            renderer.prepare(terminal.screen(), &options).unwrap().quads,
            updated.quads
        );
        let regular = renderer.shape::<true>("style", FontStyle::Regular).unwrap();
        let bold = renderer.shape::<true>("style", FontStyle::Bold).unwrap();
        assert!(!Arc::ptr_eq(&regular, &bold));
        // Exercise eviction without allocating a huge CoreText run in a unit test.
        renderer.shaped_bytes = MAX_SHAPED_BYTES;
        renderer.shape::<true>("new", FontStyle::Regular).unwrap();
        assert!(renderer.shaped_bytes < MAX_SHAPED_BYTES);
        assert_eq!(renderer.shaped.iter().map(HashMap::len).sum::<usize>(), 1);
    }

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
    fn selection_on_a_wide_tail_paints_the_whole_glyph() {
        let mut terminal = Terminal::new(10, 2, 0);
        terminal.feed("界".as_bytes());
        let point = terminal.screen().point(0, 1).unwrap();
        terminal.screen_mut().selection = Some(Selection {
            start: point,
            end: point,
            rectangular: false,
        });
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let options = RenderOptions {
            cursor_visible: false,
            ..Default::default()
        };
        let frame = renderer.prepare(terminal.screen(), &options).unwrap();
        let metrics = renderer.metrics();
        for col in [0, 1] {
            assert!(frame.quads.iter().any(|quad| quad.paint == Paint::Solid
                && quad.color == Color::rgb(options.selection_background)
                && quad.rect[0] == options.padding[0] + col as f32 * metrics.cell_width as f32));
        }
    }

    #[test]
    fn selection_and_unfocused_cursor_have_explicit_geometry() {
        let mut terminal = Terminal::new(10, 2, 10);
        terminal.feed(b"hello");
        let row = terminal.screen().row(0).id;
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
    fn viewport_snapshots_preserve_selection_paint_while_scrolling() {
        let mut terminal = Terminal::new(10, 3, 100);
        terminal.feed(b"0123456789\r\n".repeat(11).as_slice());
        terminal.feed(b"0123456789");
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let options = RenderOptions {
            cursor_visible: false,
            ..Default::default()
        };
        let selected_rects = |frame: Frame| {
            frame
                .quads
                .into_iter()
                .filter(|q| {
                    q.paint == Paint::Solid && q.color == Color::rgb(options.selection_background)
                })
                .map(|q| q.rect)
                .collect::<Vec<_>>()
        };
        for rectangular in [false, true] {
            for reversed in [false, true] {
                let mut selection = Selection {
                    start: terminal.screen().point(3, 7).unwrap(),
                    end: terminal.screen().point(8, 2).unwrap(),
                    rectangular,
                };
                if reversed {
                    std::mem::swap(&mut selection.start, &mut selection.end);
                }
                terminal.screen_mut().selection = Some(selection);
                let text = terminal.screen().selection_text().unwrap();
                assert_eq!(text.lines().count(), 6);
                for offset in (0..=9).chain((0..9).rev()) {
                    let screen = terminal.screen_mut();
                    screen.scroll_viewport(offset - screen.viewport_offset as isize);
                    let expected = selected_rects(renderer.prepare(screen, &options).unwrap());
                    let snapshot = screen.snapshot_viewport();
                    assert_eq!(screen.selection, Some(selection));
                    assert_eq!(screen.selection_text().as_deref(), Some(text.as_str()));
                    assert_eq!(
                        selected_rects(renderer.prepare(&snapshot, &options).unwrap()),
                        expected,
                        "offset={offset}, rectangular={rectangular}, reversed={reversed}",
                    );
                }
            }
        }
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
                family: "JetBrains Mono".into(),
            }],
            ..Default::default()
        })
        .unwrap();
        mapped.prepare(terminal.screen(), &options).unwrap();
        assert!(!mapped.sprites.contains_key(&('─', 1)));
        assert!(mapped.sprites.contains_key(&('█', 1)));
    }
}

#[cfg(test)]
#[path = "../../../test/rustty/shaper_rows.rs"]
mod shaper_rows;
