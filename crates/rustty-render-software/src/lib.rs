//! Portable CPU rendering into a host-presented, premultiplied sRGB framebuffer.
//! No GPU, window, native font handle, or presentation API is used here.
use egui::{ClippedPrimitive, TextureId, TexturesDelta, epaint::Primitive};
use std::{collections::HashMap, fmt, sync::Arc};

mod raster;
mod terminal;
mod texture;

const MAX_DIMENSION: usize = 16_384;
const MAX_BYTES: usize = 256 * 1024 * 1024;
const MAX_TEXTURES: usize = 4096;
const MAX_WINDOWS: usize = 128;
const MAX_GEOMETRY: usize = 6_000_000;
const MAX_PIXEL_WORK: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderError(&'static str);
impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for RenderError {}
type Result<T> = std::result::Result<T, RenderError>;

/// Put this value in an `egui::PaintCallback` to draw a terminal frame in order
/// among egui meshes. Its frame is scaled to the callback's pixel viewport.
#[derive(Clone, Debug)]
pub struct TerminalPaint {
    pub window: u64,
    pub frame: Arc<rustty_render::Frame>,
}

#[derive(Default)]
pub struct Renderer {
    pixels: Vec<u8>,
    textures: HashMap<TextureId, texture::Texture>,
    windows: HashMap<u64, terminal::Atlases>,
    resource_bytes: usize,
}

impl Renderer {
    /// Apply texture sets, clear to transparent, paint in order, then free the
    /// textures named in `textures.free`. The caller still owns the delta and
    /// must clear/consume it after all backends have handled it.
    ///
    /// RGB blending matches the UNORM target used by egui-wgpu: premultiplied
    /// gamma-space blending. Terminal atlas filtering happens in linear sRGB,
    /// as in the shared terminal WGSL. Optional egui dithering is not applied.
    pub fn render(
        &mut self,
        size: [u32; 2],
        pixels_per_point: f32,
        primitives: &[ClippedPrimitive],
        textures: &TexturesDelta,
    ) -> Result<&[u8]> {
        if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
            return Err(RenderError("invalid display scale"));
        }
        let length = image_bytes([size[0] as usize, size[1] as usize], true)?;
        if primitives.len() > MAX_GEOMETRY {
            return Err(RenderError("too many paint primitives"));
        }
        texture::update(&mut self.textures, &mut self.resource_bytes, textures)?;
        if length > self.pixels.len() {
            self.pixels
                .try_reserve(length - self.pixels.len())
                .map_err(|_| RenderError("framebuffer allocation failed"))?;
        }
        self.pixels.resize(length, 0);
        self.pixels.fill(0);
        let result = self.paint(size, pixels_per_point, primitives);
        for id in &textures.free {
            if let Some(texture) = self.textures.remove(id) {
                self.resource_bytes -= texture.bytes();
            }
        }
        result?;
        Ok(&self.pixels)
    }

    fn paint(&mut self, size: [u32; 2], scale: f32, primitives: &[ClippedPrimitive]) -> Result<()> {
        if size.contains(&0) {
            return Ok(());
        }
        let mut canvas = raster::Canvas {
            pixels: &mut self.pixels,
            size,
            work: MAX_PIXEL_WORK,
            geometry: MAX_GEOMETRY,
        };
        for primitive in primitives {
            let clip = raster::Clip::from_points(primitive.clip_rect, scale, size)?;
            match &primitive.primitive {
                Primitive::Mesh(mesh) => {
                    if mesh.indices.is_empty() {
                        continue;
                    }
                    let texture = self
                        .textures
                        .get(&mesh.texture_id)
                        .ok_or(RenderError("mesh references an absent texture"))?;
                    raster::mesh(&mut canvas, clip, scale, mesh, texture)?;
                }
                Primitive::Callback(callback) => {
                    let paint = callback
                        .callback
                        .downcast_ref::<TerminalPaint>()
                        .ok_or(RenderError("unsupported software paint callback"))?;
                    if !self.windows.contains_key(&paint.window)
                        && self.windows.len() >= MAX_WINDOWS
                    {
                        return Err(RenderError("too many terminal atlas owners"));
                    }
                    let atlases = self.windows.entry(paint.window).or_default();
                    atlases.update(&paint.frame, &mut self.resource_bytes)?;
                    let viewport = raster::Clip::from_points(callback.rect, scale, size)?;
                    terminal::paint(
                        &mut canvas,
                        clip.intersect(viewport),
                        viewport,
                        paint,
                        atlases,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Discard a closed window's cached terminal atlas pages.
    pub fn free_window(&mut self, window: u64) {
        if let Some(atlases) = self.windows.remove(&window) {
            self.resource_bytes -= atlases.bytes();
        }
    }
}

fn image_bytes(size: [usize; 2], allow_empty: bool) -> Result<usize> {
    if size.iter().any(|v| *v > MAX_DIMENSION) || (!allow_empty && size.contains(&0)) {
        return Err(RenderError("invalid image dimensions"));
    }
    size[0]
        .checked_mul(size[1])
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| *bytes <= MAX_BYTES)
        .ok_or(RenderError("image exceeds the software memory limit"))
}

fn reserve_bytes(current: usize, old: usize, new: usize) -> Result<usize> {
    current
        .checked_sub(old)
        .and_then(|bytes| bytes.checked_add(new))
        .filter(|bytes| *bytes <= MAX_BYTES)
        .ok_or(RenderError("textures exceed the software memory limit"))
}

#[cfg(test)]
mod tests;
