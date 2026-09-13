//! Terminal fonts, with native handles confined to the platform implementation.
//!
//! Positions and metrics are physical pixels. Shaping preserves UTF-8 byte
//! offsets so the renderer can anchor clusters to the terminal's cell grid.

use std::fmt;

pub mod sprite;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::FontSystem;

/// A stable identifier within the `FontSystem` that produced it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FontId(pub(crate) usize);

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[repr(usize)]
pub enum FontStyle {
    #[default]
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FontFeature {
    pub tag: [u8; 4],
    pub value: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FontVariation {
    pub tag: [u8; 4],
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FontConfig {
    pub families: Vec<String>,
    pub bold_families: Vec<String>,
    pub italic_families: Vec<String>,
    pub bold_italic_families: Vec<String>,
    pub size_points: f32,
    pub scale_factor: f32,
    pub features: Vec<FontFeature>,
    pub variations: Vec<FontVariation>,
    pub thicken: bool,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            families: Vec::new(),
            bold_families: Vec::new(),
            italic_families: Vec::new(),
            bold_italic_families: Vec::new(),
            size_points: 13.0,
            scale_factor: 1.0,
            features: Vec::new(),
            variations: Vec::new(),
            thicken: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontMetrics {
    pub cell_width: u32,
    pub cell_height: u32,
    /// Distance from the top of the cell to the baseline.
    pub baseline: f32,
    /// Distance below the baseline to the underline.
    pub underline_position: f32,
    pub underline_thickness: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShapedGlyph {
    pub font: FontId,
    pub glyph: u16,
    /// UTF-8 byte offset of the source cluster, never a UTF-16 code-unit index.
    pub cluster: usize,
    /// Position relative to the run origin; positive y points upward.
    pub x: f32,
    pub y: f32,
    pub advance: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BitmapFormat {
    /// Coverage only; the renderer supplies the foreground color.
    Alpha,
    /// Premultiplied sRGB RGBA, in top-to-bottom row order.
    Rgba,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlyphBitmap {
    pub width: u32,
    pub height: u32,
    /// Horizontal offset from the glyph origin to the bitmap's left edge.
    pub bearing_x: i32,
    /// Distance upward from the baseline to the bitmap's top edge.
    pub bearing_y: i32,
    pub format: BitmapFormat,
    pub pixels: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FontError(pub(crate) String);

impl fmt::Display for FontError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FontError {}

pub const JETBRAINS_LICENSE: &str = include_str!("../resources/JetBrainsMono-OFL.txt");
pub const NERD_SYMBOLS_LICENSE: &str = include_str!("../resources/NerdFontsSymbols-LICENSE.txt");
