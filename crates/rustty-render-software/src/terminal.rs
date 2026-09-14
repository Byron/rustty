use crate::{
    RenderError, Result, TerminalPaint, image_bytes,
    raster::{Canvas, Clip, coordinate},
    reserve_bytes,
    texture::{Texture, encode},
};
use egui::TextureFilter;
use rustty_render::{Frame, Paint};

#[derive(Default)]
pub(crate) struct Atlases {
    generation: Option<u64>,
    pages: Vec<Page>,
}
struct Page {
    texture: Texture,
    revision: Option<u64>,
}
impl Atlases {
    pub fn bytes(&self) -> usize {
        self.pages.iter().map(|page| page.texture.bytes()).sum()
    }
    pub fn update(&mut self, frame: &Frame, bytes: &mut usize) -> Result<()> {
        if self.generation != Some(frame.generation) {
            *bytes -= self.bytes();
            self.pages.clear();
            self.generation = Some(frame.generation);
        }
        if frame.atlas_uploads.len() > crate::MAX_GEOMETRY {
            return Err(RenderError("too many terminal atlas updates"));
        }
        for update in &frame.atlas_uploads {
            let page_bytes = image_bytes([update.page_size as usize; 2], false)?;
            let data_bytes = image_bytes(update.size.map(|v| v as usize), true)?;
            if data_bytes != update.pixels.len()
                || (0..2).any(|axis| {
                    update.origin[axis]
                        .checked_add(update.size[axis])
                        .is_none_or(|end| end > update.page_size)
                })
                || update.page > self.pages.len()
            {
                return Err(RenderError("invalid terminal atlas upload"));
            }
            if update.page == self.pages.len() {
                if self.pages.len() >= crate::MAX_TEXTURES {
                    return Err(RenderError("too many terminal atlas pages"));
                }
                if self
                    .bytes()
                    .checked_add(page_bytes)
                    .is_none_or(|bytes| bytes > 64 * 1024 * 1024)
                {
                    return Err(RenderError("terminal atlas exceeds the 64 MiB limit"));
                }
                let next_bytes = reserve_bytes(*bytes, 0, page_bytes)?;
                self.pages.push(Page {
                    texture: Texture::empty([update.page_size as usize; 2])?,
                    revision: None,
                });
                *bytes = next_bytes;
            }
            let page = &mut self.pages[update.page];
            if page.texture.size != [update.page_size as usize; 2] {
                return Err(RenderError(
                    "terminal atlas size changed within a generation",
                ));
            }
            if page
                .revision
                .is_some_and(|revision| update.revision <= revision)
            {
                continue;
            }
            let row_width = update.size[0] as usize;
            if row_width != 0 {
                for (y, row) in update.pixels.chunks_exact(row_width * 4).enumerate() {
                    let start = (update.origin[1] as usize + y) * update.page_size as usize
                        + update.origin[0] as usize;
                    for (dst, src) in page.texture.pixels[start..start + row_width]
                        .iter_mut()
                        .zip(row.as_chunks::<4>().0)
                    {
                        dst.copy_from_slice(src);
                    }
                }
            }
            page.revision = Some(update.revision);
        }
        Ok(())
    }
}

pub(crate) fn paint(
    canvas: &mut Canvas<'_>,
    clip: Clip,
    viewport: Clip,
    paint: &TerminalPaint,
    atlases: &Atlases,
) -> Result<()> {
    let frame = &paint.frame;
    image_bytes(frame.size.map(|v| v as usize), true)?;
    canvas.geometry(frame.quads.len().saturating_mul(6))?;
    if frame.size.contains(&0) || viewport.area() == 0 {
        return Ok(());
    }
    let sx = (viewport.0[2] - viewport.0[0]) as f32 / frame.size[0] as f32;
    let sy = (viewport.0[3] - viewport.0[1]) as f32 / frame.size[1] as f32;
    for quad in &frame.quads {
        if !quad.rect.iter().chain(&quad.uv).copied().all(coordinate)
            || quad
                .color
                .0
                .iter()
                .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
        {
            return Err(RenderError("invalid terminal quad"));
        }
        let [x, y, w, h] = quad.rect;
        if w <= 0.0 || h <= 0.0 {
            continue;
        }
        let rect = [
            viewport.0[0] as f32 + x * sx,
            viewport.0[1] as f32 + y * sy,
            viewport.0[0] as f32 + (x + w) * sx,
            viewport.0[1] as f32 + (y + h) * sy,
        ];
        let bounds = clip.bounds(rect);
        canvas.charge(bounds)?;
        let gamma = [
            encode(quad.color.0[0]),
            encode(quad.color.0[1]),
            encode(quad.color.0[2]),
        ];
        let alpha = quad.color.0[3];
        if quad.paint == Paint::Solid {
            canvas.solid(
                bounds,
                [
                    gamma[0] * alpha,
                    gamma[1] * alpha,
                    gamma[2] * alpha,
                    255.0 * alpha,
                ],
            );
            continue;
        }
        let texture = &atlases
            .pages
            .get(quad.atlas)
            .ok_or(RenderError("terminal quad references an absent atlas"))?
            .texture;
        let du = (quad.uv[2] - quad.uv[0]) / (rect[2] - rect[0]);
        let dv = (quad.uv[3] - quad.uv[1]) / (rect[3] - rect[1]);
        for py in bounds.0[1]..bounds.0[3] {
            let v = quad.uv[1] + (py as f32 + 0.5 - rect[1]) * dv;
            for px in bounds.0[0]..bounds.0[2] {
                let u = quad.uv[0] + (px as f32 + 0.5 - rect[0]) * du;
                let (color, a) = if quad.paint == Paint::Color {
                    let texel = texture.sample([u, v], TextureFilter::Linear, true);
                    (
                        [encode(texel[0]), encode(texel[1]), encode(texel[2])],
                        alpha * texel[3],
                    )
                } else {
                    (gamma, alpha * texture.alpha([u, v]))
                };
                if a > 0.0 {
                    canvas.pixel(
                        px,
                        py,
                        [color[0] * a, color[1] * a, color[2] * a, 255.0 * a],
                    );
                }
            }
        }
    }
    Ok(())
}
