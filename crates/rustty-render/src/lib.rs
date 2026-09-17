//! Portable terminal draw data. The host owns windows, scheduling and presentation.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, LazyLock},
};

#[cfg(target_os = "macos")]
mod prepare;
#[cfg(target_os = "macos")]
pub use prepare::{Preedit, RenderOptions, Renderer};

#[derive(Debug)]
pub enum RenderError {
    Font(rustty_font::FontError),
    AtlasCapacity,
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Font(error) => error.fmt(f),
            Self::AtlasCapacity => {
                f.write_str("visible terminal glyphs exceed the 64 MiB atlas budget")
            }
        }
    }
}
impl std::error::Error for RenderError {}
impl From<rustty_font::FontError> for RenderError {
    fn from(value: rustty_font::FontError) -> Self {
        Self::Font(value)
    }
}

/// A color with linear RGB channels and straight alpha.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color(pub [f32; 4]);

impl Color {
    pub fn rgb(rgb: [u8; 3]) -> Self {
        static LINEAR: LazyLock<[f32; 256]> =
            LazyLock::new(|| std::array::from_fn(|value| linear(value as u8)));
        let linear = &*LINEAR;
        Self([
            linear[usize::from(rgb[0])],
            linear[usize::from(rgb[1])],
            linear[usize::from(rgb[2])],
            1.0,
        ])
    }

    pub fn opacity(mut self, alpha: f32) -> Self {
        self.0[3] *= alpha.clamp(0.0, 1.0);
        self
    }
}

fn linear(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Paint {
    Solid,
    /// Alpha coverage in the atlas; color comes from the quad.
    Mask,
    /// An RGBA atlas glyph/image whose original colors are preserved.
    Color,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Quad {
    /// Left, top, width and height in physical pixels, relative to this frame.
    pub rect: [f32; 4],
    /// Normalized atlas coordinates: left, top, right, bottom.
    pub uv: [f32; 4],
    pub color: Color,
    pub paint: Paint,
    pub atlas: usize,
}

impl Quad {
    pub fn solid(rect: [f32; 4], color: Color) -> Self {
        Self {
            rect,
            uv: [0.0; 4],
            color,
            paint: Paint::Solid,
            atlas: 0,
        }
    }
}

/// An immutable atlas update. Frames retain all updates required to reproduce
/// their atlas state, so a new GPU backend can recover without native font handles.
#[derive(Clone, Debug, PartialEq)]
pub struct AtlasUpload {
    pub revision: u64,
    pub page: usize,
    pub page_size: u32,
    pub origin: [u32; 2],
    pub size: [u32; 2],
    /// Straight sRGB RGBA, tightly packed in top-to-bottom row order.
    pub pixels: Arc<[u8]>,
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub size: [u32; 2],
    /// Changes when font/atlas state is discarded. Revisions are scoped to it.
    pub generation: u64,
    pub quads: Vec<Quad>,
    pub atlas_uploads: Vec<AtlasUpload>,
    /// IME candidate-window anchor in physical pixels, when a preedit caret is present.
    pub ime_cursor: Option<[f32; 4]>,
    /// Prepared text or decorations depend on `RenderOptions::blink_visible`.
    pub blinking_text: bool,
}

impl Frame {
    pub fn empty(size: [u32; 2]) -> Self {
        Self {
            size,
            generation: 0,
            quads: Vec::new(),
            atlas_uploads: Vec::new(),
            ime_cursor: None,
            blinking_text: false,
        }
    }

    /// Compose a pane into a window-sized frame. `origin` and `clip` are in
    /// destination physical pixels; clipping adjusts texture coordinates too.
    ///
    /// All textured panes must come from the same font/atlas generation. If
    /// eviction changes it while preparing later panes, rebuild the pane frames
    /// before composing them. Failure leaves the destination unchanged.
    pub fn append_clipped(
        &mut self,
        other: &Frame,
        origin: [f32; 2],
        clip: [f32; 4],
    ) -> Result<(), ComposeError> {
        if origin.iter().chain(clip.iter()).any(|v| !v.is_finite())
            || clip[2] < 0.0
            || clip[3] < 0.0
        {
            return Err(ComposeError::InvalidClip);
        }
        let has_resources =
            !self.atlas_uploads.is_empty() || self.quads.iter().any(|q| q.paint != Paint::Solid);
        if has_resources && self.generation != other.generation {
            return Err(ComposeError::GenerationChanged);
        }
        let mut updates: BTreeMap<_, _> =
            self.atlas_uploads.iter().map(|u| (u.revision, u)).collect();
        for update in &other.atlas_uploads {
            if let Some(existing) = updates.insert(update.revision, update)
                && existing != update
            {
                return Err(ComposeError::ConflictingAtlasRevision);
            }
        }
        let uploads = updates.into_values().cloned().collect();
        let clip_left = clip[0].max(0.0);
        let clip_top = clip[1].max(0.0);
        let clip_right = (clip[0] + clip[2]).min(self.size[0] as f32);
        let clip_bottom = (clip[1] + clip[3]).min(self.size[1] as f32);
        for quad in &other.quads {
            let [x, y, w, h] = quad.rect;
            if w <= 0.0 || h <= 0.0 {
                continue;
            }
            let x = x + origin[0];
            let y = y + origin[1];
            let left = x.max(clip_left);
            let top = y.max(clip_top);
            let right = (x + w).min(clip_right);
            let bottom = (y + h).min(clip_bottom);
            if left >= right || top >= bottom {
                continue;
            }
            let [u0, v0, u1, v1] = quad.uv;
            let mut clipped = quad.clone();
            clipped.rect = [left, top, right - left, bottom - top];
            clipped.uv = [
                u0 + (u1 - u0) * (left - x) / w,
                v0 + (v1 - v0) * (top - y) / h,
                u0 + (u1 - u0) * (right - x) / w,
                v0 + (v1 - v0) * (bottom - y) / h,
            ];
            self.quads.push(clipped);
        }
        self.generation = other.generation;
        self.atlas_uploads = uploads;
        self.blinking_text |= other.blinking_text;
        if let Some([x, y, w, h]) = other.ime_cursor {
            self.ime_cursor = Some([x + origin[0], y + origin[1], w, h]);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComposeError {
    GenerationChanged,
    ConflictingAtlasRevision,
    InvalidClip,
}

impl fmt::Display for ComposeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::GenerationChanged => "pane frames belong to different atlas generations",
            Self::ConflictingAtlasRevision => "pane frames contain conflicting atlas revisions",
            Self::InvalidClip => "invalid pane origin or clipping rectangle",
        })
    }
}
impl std::error::Error for ComposeError {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn srgb_colors_are_linearized_before_blending() {
        for value in 0..=255 {
            let expected = linear(value);
            assert_eq!(
                Color::rgb([value; 3]).0,
                [expected, expected, expected, 1.0]
            );
        }
        let color = Color::rgb([0, 128, 255]).opacity(0.5);
        assert_eq!(color.0[0], 0.0);
        assert!((color.0[1] - 0.21586).abs() < 0.0001);
        assert_eq!(color.0[2..], [1.0, 0.5]);
    }

    #[test]
    fn pane_composition_clips_texture_coordinates_and_shares_atlas_updates() {
        let mut pane = Frame::empty([10, 10]);
        pane.generation = 7;
        pane.quads.push(Quad {
            rect: [0.0, 0.0, 10.0, 10.0],
            uv: [0.0, 0.0, 1.0, 1.0],
            color: Color::rgb([255; 3]),
            paint: Paint::Mask,
            atlas: 0,
        });
        pane.atlas_uploads.push(AtlasUpload {
            revision: 1,
            page: 0,
            page_size: 1,
            origin: [0, 0],
            size: [1, 1],
            pixels: Arc::from([255; 4]),
        });
        let mut window = Frame::empty([50, 50]);
        window
            .append_clipped(&pane, [5.0, 7.0], [8.0, 9.0, 4.0, 6.0])
            .unwrap();
        assert_eq!(window.quads[0].rect, [8.0, 9.0, 4.0, 6.0]);
        for (actual, expected) in window.quads[0].uv.into_iter().zip([0.3, 0.2, 0.7, 0.8]) {
            assert!((actual - expected).abs() < 0.00001);
        }
        window
            .append_clipped(&pane, [20.0, 0.0], [20.0, 0.0, 10.0, 10.0])
            .unwrap();
        assert_eq!(window.quads.len(), 2);
        assert_eq!(window.atlas_uploads.len(), 1);
        let quads = window.quads.clone();
        pane.generation = 8;
        assert_eq!(
            window.append_clipped(&pane, [0.0, 0.0], [0.0, 0.0, 10.0, 10.0]),
            Err(ComposeError::GenerationChanged)
        );
        assert_eq!(window.quads, quads);
    }
}
