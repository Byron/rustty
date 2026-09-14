use crate::{MAX_TEXTURES, RenderError, Result, image_bytes, reserve_bytes};
use egui::{ImageData, TextureFilter, TextureId, TextureOptions, TextureWrapMode, TexturesDelta};
use std::{collections::HashMap, sync::OnceLock};

pub(crate) struct Texture {
    pub size: [usize; 2],
    pub pixels: Vec<[u8; 4]>,
    pub options: TextureOptions,
}

impl Texture {
    pub fn empty(size: [usize; 2]) -> Result<Self> {
        let count = image_bytes(size, false)? / 4;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(count)
            .map_err(|_| RenderError("texture allocation failed"))?;
        pixels.resize(count, [0; 4]);
        Ok(Self {
            size,
            pixels,
            options: TextureOptions::LINEAR,
        })
    }

    pub fn bytes(&self) -> usize {
        self.pixels.len() * 4
    }

    /// The overwhelmingly common terminal operation only needs alpha coverage,
    /// with the atlas's fixed clamp-to-edge, linear sampler.
    pub fn alpha(&self, uv: [f32; 2]) -> f32 {
        let x = (uv[0] * self.size[0] as f32 - 0.5).clamp(0.0, (self.size[0] - 1) as f32);
        let y = (uv[1] * self.size[1] as f32 - 0.5).clamp(0.0, (self.size[1] - 1) as f32);
        let ix = x as usize;
        let iy = y as usize;
        let nx = (ix + 1).min(self.size[0] - 1);
        let ny = (iy + 1).min(self.size[1] - 1);
        let tx = x - ix as f32;
        let ty = y - iy as f32;
        let at = |x: usize, y: usize| self.pixels[y * self.size[0] + x][3] as f32;
        let top = at(ix, iy) + (at(nx, iy) - at(ix, iy)) * tx;
        let bottom = at(ix, ny) + (at(nx, ny) - at(ix, ny)) * tx;
        (top + (bottom - top) * ty) / 255.0
    }

    pub fn filter(&self, dx: [f32; 2], dy: [f32; 2]) -> TextureFilter {
        let length = |d: [f32; 2]| {
            (d[0] * self.size[0] as f32).powi(2) + (d[1] * self.size[1] as f32).powi(2)
        };
        if length(dx).max(length(dy)) > 1.0 {
            self.options.minification
        } else {
            self.options.magnification
        }
    }

    /// Egui images are premultiplied gamma RGBA; terminal atlas images are
    /// straight sRGB. Only terminal RGB is decoded before bilinear filtering.
    pub fn sample(&self, uv: [f32; 2], filter: TextureFilter, linear_rgb: bool) -> [f32; 4] {
        let uv = uv.map(|v| match self.options.wrap_mode {
            TextureWrapMode::ClampToEdge => v.clamp(0.0, 1.0),
            TextureWrapMode::Repeat => v.rem_euclid(1.0),
            TextureWrapMode::MirroredRepeat => v.rem_euclid(2.0),
        });
        let x = uv[0] * self.size[0] as f32;
        let y = uv[1] * self.size[1] as f32;
        if filter == TextureFilter::Nearest {
            return self.texel(x.floor() as i64, y.floor() as i64, linear_rgb);
        }
        let x = x - 0.5;
        let y = y - 0.5;
        let ix = x.floor() as i64;
        let iy = y.floor() as i64;
        let tx = x - x.floor();
        let ty = y - y.floor();
        let a = self.texel(ix, iy, linear_rgb);
        let b = self.texel(ix + 1, iy, linear_rgb);
        let c = self.texel(ix, iy + 1, linear_rgb);
        let d = self.texel(ix + 1, iy + 1, linear_rgb);
        std::array::from_fn(|channel| {
            let top = a[channel] + (b[channel] - a[channel]) * tx;
            let bottom = c[channel] + (d[channel] - c[channel]) * tx;
            top + (bottom - top) * ty
        })
    }

    fn texel(&self, x: i64, y: i64, linear_rgb: bool) -> [f32; 4] {
        let wrap = |value: i64, size: usize| -> usize {
            let size = size as i64;
            (match self.options.wrap_mode {
                TextureWrapMode::ClampToEdge => value.clamp(0, size - 1),
                TextureWrapMode::Repeat => value.rem_euclid(size),
                TextureWrapMode::MirroredRepeat => {
                    let value = value.rem_euclid(size * 2);
                    if value < size {
                        value
                    } else {
                        size * 2 - 1 - value
                    }
                }
            }) as usize
        };
        let pixel = self.pixels[wrap(y, self.size[1]) * self.size[0] + wrap(x, self.size[0])];
        if linear_rgb {
            let table = decode_table();
            [
                table[pixel[0] as usize],
                table[pixel[1] as usize],
                table[pixel[2] as usize],
                pixel[3] as f32 / 255.0,
            ]
        } else {
            pixel.map(f32::from)
        }
    }
}

pub(crate) fn update(
    textures: &mut HashMap<TextureId, Texture>,
    bytes: &mut usize,
    delta: &TexturesDelta,
) -> Result<()> {
    for (id, changes) in &delta.set {
        for change in changes {
            let ImageData::Color(image) = &change.image;
            let required = image_bytes(image.size, false)?;
            if image.pixels.len() != required / 4 {
                return Err(RenderError("invalid texture pixel count"));
            }
            if let Some(origin) = change.pos {
                let texture = textures
                    .get_mut(id)
                    .ok_or(RenderError("partial update references an absent texture"))?;
                if (0..2).any(|axis| {
                    origin[axis]
                        .checked_add(image.size[axis])
                        .is_none_or(|end| end > texture.size[axis])
                }) {
                    return Err(RenderError("partial texture update is out of bounds"));
                }
                for (y, row) in image.pixels.chunks_exact(image.size[0]).enumerate() {
                    let start = (origin[1] + y) * texture.size[0] + origin[0];
                    for (dst, src) in texture.pixels[start..start + row.len()].iter_mut().zip(row) {
                        *dst = src.to_array();
                    }
                }
                texture.options = change.options;
            } else {
                if !textures.contains_key(id) && textures.len() >= MAX_TEXTURES {
                    return Err(RenderError("too many egui textures"));
                }
                let next_bytes =
                    reserve_bytes(*bytes, textures.get(id).map_or(0, Texture::bytes), required)?;
                let mut texture = Texture::empty(image.size)?;
                for (dst, src) in texture.pixels.iter_mut().zip(&image.pixels) {
                    *dst = src.to_array();
                }
                texture.options = change.options;
                textures.insert(*id, texture);
                *bytes = next_bytes;
            }
        }
    }
    Ok(())
}

fn decode_table() -> &'static [f32; 256] {
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|v| {
            let x = v as f32 / 255.0;
            if x <= 0.04045 {
                x / 12.92
            } else {
                ((x + 0.055) / 1.055).powf(2.4)
            }
        })
    })
}

pub(crate) fn encode(linear: f32) -> f32 {
    static TABLE: OnceLock<Box<[f32]>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        (0..=65535)
            .map(|v| {
                let x = v as f64 / 65535.0;
                (255.0
                    * if x <= 0.0031308 {
                        x * 12.92
                    } else {
                        1.055 * x.powf(1.0 / 2.4) - 0.055
                    }) as f32
            })
            .collect()
    });
    table[(linear.clamp(0.0, 1.0) * 65535.0).round() as usize]
}
