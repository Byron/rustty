//! Physical-size key artwork, adapted from Dazer's MIT-licensed dark key design.
//! See docs/rustty/stream-deck-reference-LICENSE.txt for attribution.

use image::{Rgb, RgbImage};
use rustty::vt::agent::State;
use rustty_font::{BitmapFormat, FontConfig, FontStyle, FontSystem, GlyphBitmap};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug, PartialEq)]
pub enum Visual {
    Agent {
        label: String,
        state: Option<State>,
        focused: bool,
        reserved: bool,
        moving: bool,
    },
    Function {
        value: String,
        mark: Mark,
        dim: bool,
    },
    Decorative(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mark {
    State(State),
    Brightness,
    Page,
}

pub struct Renderer {
    label: FontSystem,
    number: FontSystem,
    large: FontSystem,
    size: u32,
}

const TEXT: [u8; 3] = [242, 242, 239];
const NEUTRAL: [u8; 3] = [145, 152, 158];
const MOVE: [u8; 3] = [192, 147, 255];

impl Renderer {
    pub fn new(size: (usize, usize)) -> Result<Self, String> {
        if size.0 != size.1 || !(32..=256).contains(&size.0) {
            return Err("Stream Deck key images must be square, between 32 and 256 pixels".into());
        }
        let scale = size.0 as f32 / 72.0;
        let fonts = |points| {
            FontSystem::new(FontConfig {
                families: vec![if cfg!(target_os = "macos") {
                    "Helvetica Neue".into()
                } else {
                    "Segoe UI".into()
                }],
                size_points: points,
                scale_factor: scale,
                ..Default::default()
            })
            .map_err(|error| error.to_string())
        };
        Ok(Self {
            label: fonts(10.5)?,
            number: fonts(13.0)?,
            large: fonts(32.0)?,
            size: size.0 as u32,
        })
    }

    pub fn render(&mut self, visual: &Visual) -> Result<RgbImage, String> {
        let (state, focused, moving, dim, function) = match visual {
            Visual::Agent {
                state,
                focused,
                moving,
                reserved,
                ..
            } => (
                *state,
                *focused,
                *moving,
                state.is_none() || *reserved,
                false,
            ),
            Visual::Function { mark, dim, .. } => (
                match mark {
                    Mark::State(state) => Some(*state),
                    _ => None,
                },
                false,
                false,
                *dim,
                true,
            ),
            Visual::Decorative(_) => (None, false, false, false, false),
        };
        let signal = if moving { MOVE } else { color(state) };
        let mut image = RgbImage::from_pixel(self.size, self.size, Rgb([13, 15, 17]));
        let scale = self.size as f32 / 72.0;
        for (px, py, pixel) in image.enumerate_pixels_mut() {
            let x = (px as f32 + 0.5) / scale;
            let y = (py as f32 + 0.5) / scale;
            let d = rounded_distance(x, y, 4.0, 4.0, 64.0, 64.0, 8.0);
            let glow = (-d.max(0.0) / 2.0).exp() * if dim { 0.08 } else { 0.24 };
            mix(&mut pixel.0, signal, glow);
            if d <= 0.5 {
                let gradient = ((y - 4.0) / 64.0).clamp(0.0, 1.0);
                let top: [u8; 3] = if function { [42, 47, 51] } else { [52, 54, 56] };
                let mut face = top.map(|channel| (channel as f32 - gradient * 18.0) as u8);
                let bottom_glow = (1.0 - ((x - 36.0) / 40.0).powi(2)).max(0.0)
                    * gradient.powi(3)
                    * if dim { 0.035 } else { 0.23 };
                mix(&mut face, signal, bottom_glow);
                if d > -1.0 {
                    mix(&mut face, signal, if dim { 0.15 } else { 0.65 });
                } else if d > -1.8 {
                    mix(&mut face, [95, 98, 100], 0.4);
                }
                mix(&mut pixel.0, face, (0.5 - d).clamp(0.0, 1.0));
                if moving && (-3.8..-2.4).contains(&d) {
                    mix(&mut pixel.0, MOVE, 0.8);
                }
            }
        }
        if focused {
            line(&mut image, (26.0, 8.0), (46.0, 8.0), 1.5, [76, 224, 194]);
        }
        let text = if dim { [114, 119, 123] } else { TEXT };
        match visual {
            Visual::Agent {
                label,
                state,
                reserved,
                moving,
                ..
            } => {
                let lines = wrap_label(&mut self.label, label, 52.0 * scale)?;
                let top = if lines.len() == 1 { 51.0 } else { 44.0 };
                for (i, label) in lines.iter().enumerate() {
                    draw_text(
                        &mut self.label,
                        &mut image,
                        label,
                        (10.0, top + i as f32 * 10.0, 52.0, 10.0),
                        text,
                    )?;
                }
                let center = if label.is_empty() {
                    (36.0, 36.0)
                } else {
                    (36.0, 28.0)
                };
                if *moving {
                    draw_swap(&mut image, center, MOVE);
                } else {
                    draw_mark(&mut image, *state, center, signal);
                    if *reserved {
                        line(
                            &mut image,
                            (26.0, center.1 + 6.0),
                            (46.0, center.1 + 6.0),
                            1.0,
                            NEUTRAL,
                        );
                    }
                }
            }
            Visual::Function { value, mark, .. } => {
                let value_font = if text_width(&mut self.number, value)? > 52.0 * scale {
                    &mut self.label
                } else {
                    &mut self.number
                };
                draw_text(
                    value_font,
                    &mut image,
                    value,
                    (10.0, 49.0, 52.0, 14.0),
                    text,
                )?;
                let ink = if dim { [82, 86, 90] } else { signal };
                match mark {
                    Mark::State(state) => draw_mark(&mut image, Some(*state), (36.0, 28.0), ink),
                    Mark::Brightness => {
                        circle(&mut image, (36.0, 28.0), 7.0, 2.4, ink);
                        for i in 0..8 {
                            let angle = i as f32 * std::f32::consts::FRAC_PI_4;
                            line(
                                &mut image,
                                (36.0 + angle.cos() * 12.0, 28.0 + angle.sin() * 12.0),
                                (36.0 + angle.cos() * 16.0, 28.0 + angle.sin() * 16.0),
                                2.2,
                                ink,
                            );
                        }
                    }
                    Mark::Page => {
                        line(&mut image, (29.0, 15.0), (43.0, 28.0), 3.6, ink);
                        line(&mut image, (43.0, 28.0), (29.0, 41.0), 3.6, ink);
                    }
                }
            }
            Visual::Decorative(label) => {
                draw_text(
                    &mut self.large,
                    &mut image,
                    label,
                    (9.0, 11.0, 54.0, 50.0),
                    TEXT,
                )?;
            }
        }
        Ok(image)
    }
}

pub fn color(state: Option<State>) -> [u8; 3] {
    match state {
        Some(State::Working) => [22, 131, 255],
        Some(State::NeedsInput) => [255, 154, 61],
        Some(State::Done) => [53, 216, 107],
        Some(State::Error) => [255, 75, 97],
        _ => NEUTRAL,
    }
}

fn rounded_distance(
    x: f32,
    y: f32,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    radius: f32,
) -> f32 {
    let dx = (x - left - width / 2.0).abs() - width / 2.0 + radius;
    let dy = (y - top - height / 2.0).abs() - height / 2.0 + radius;
    dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - radius
}

fn mix(destination: &mut [u8; 3], source: [u8; 3], alpha: f32) {
    for (dst, src) in destination.iter_mut().zip(source) {
        *dst = (f32::from(*dst) * (1.0 - alpha) + f32::from(src) * alpha).round() as u8;
    }
}

fn line(image: &mut RgbImage, from: (f32, f32), to: (f32, f32), width: f32, color: [u8; 3]) {
    let scale = image.width() as f32 / 72.0;
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    for (px, py, pixel) in image.enumerate_pixels_mut() {
        let x = (px as f32 + 0.5) / scale - from.0;
        let y = (py as f32 + 0.5) / scale - from.1;
        let t = ((x * dx + y * dy) / (dx * dx + dy * dy).max(0.001)).clamp(0.0, 1.0);
        let distance = (x - t * dx).hypot(y - t * dy);
        mix(
            &mut pixel.0,
            color,
            ((width / 2.0 - distance) * scale + 0.5).clamp(0.0, 1.0),
        );
    }
}

fn circle(image: &mut RgbImage, center: (f32, f32), radius: f32, width: f32, color: [u8; 3]) {
    let scale = image.width() as f32 / 72.0;
    for (px, py, pixel) in image.enumerate_pixels_mut() {
        let distance =
            ((px as f32 + 0.5) / scale - center.0).hypot((py as f32 + 0.5) / scale - center.1);
        mix(
            &mut pixel.0,
            color,
            ((width / 2.0 - (distance - radius).abs()) * scale + 0.5).clamp(0.0, 1.0),
        );
    }
}

fn draw_mark(image: &mut RgbImage, state: Option<State>, center: (f32, f32), color: [u8; 3]) {
    let (x, y) = center;
    match state {
        Some(State::Working) => {
            for (dx, height) in [(-10.0, 13.0), (0.0, 27.0), (10.0, 20.0)] {
                line(
                    image,
                    (x + dx, y + 13.5 - height),
                    (x + dx, y + 13.5),
                    4.2,
                    color,
                );
            }
        }
        Some(State::NeedsInput) => {
            line(image, (x, y - 13.0), (x, y + 4.0), 5.0, color);
            line(image, (x, y + 12.0), (x, y + 12.0), 5.5, color);
        }
        Some(State::Done) => {
            line(image, (x - 13.0, y), (x - 3.0, y + 10.0), 4.4, color);
            line(image, (x - 3.0, y + 10.0), (x + 14.0, y - 12.0), 4.4, color);
        }
        Some(State::Error) => {
            line(
                image,
                (x - 11.0, y - 11.0),
                (x + 11.0, y + 11.0),
                4.4,
                color,
            );
            line(
                image,
                (x + 11.0, y - 11.0),
                (x - 11.0, y + 11.0),
                4.4,
                color,
            );
        }
        Some(State::Paused) => {
            line(image, (x - 7.0, y - 12.0), (x - 7.0, y + 12.0), 5.0, color);
            line(image, (x + 7.0, y - 12.0), (x + 7.0, y + 12.0), 5.0, color);
        }
        Some(State::Unknown) => circle(image, center, 12.0, 3.0, color),
        Some(State::Idle) => line(image, center, center, 17.0, color),
        None => line(image, (x - 12.0, y), (x + 12.0, y), 2.5, [86, 92, 98]),
    }
}

fn draw_swap(image: &mut RgbImage, center: (f32, f32), color: [u8; 3]) {
    let (x, y) = center;
    for direction in [-1.0, 1.0] {
        let tip = (x + direction * 14.0, y - direction * 6.0);
        line(image, (x - direction * 14.0, tip.1), tip, 3.0, color);
        line(
            image,
            (tip.0 - direction * 6.0, tip.1 - 6.0),
            tip,
            3.0,
            color,
        );
        line(
            image,
            (tip.0 - direction * 6.0, tip.1 + 6.0),
            tip,
            3.0,
            color,
        );
    }
}

fn text_width(fonts: &mut FontSystem, text: &str) -> Result<f32, String> {
    Ok(fonts
        .shape(text, FontStyle::Regular)
        .map_err(|error| error.to_string())?
        .iter()
        .map(|glyph| glyph.x + glyph.advance)
        .fold(0.0, f32::max))
}

fn wrap_label(fonts: &mut FontSystem, text: &str, width: f32) -> Result<Vec<String>, String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let suffix = text
        .rsplit_once(" ·")
        .filter(|(_, suffix)| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
        .map(|(_, suffix)| format!(" ·{suffix}"));
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    let mut lines = Vec::new();
    let mut offset = 0;
    while offset < graphemes.len() && lines.len() < 2 {
        let mut end = offset;
        while end < graphemes.len()
            && text_width(fonts, &graphemes[offset..=end].concat())? <= width
        {
            end += 1;
        }
        if end == offset {
            end += 1;
        }
        let mut line = graphemes[offset..end].concat();
        if lines.len() == 1 && end < graphemes.len() {
            let ending = format!("…{}", suffix.as_deref().unwrap_or_default());
            while !line.is_empty() && text_width(fonts, &format!("{line}{ending}"))? > width {
                line.truncate(line.grapheme_indices(true).next_back().unwrap().0);
            }
            line.push_str(&ending);
        } else if end < graphemes.len()
            && let Some(boundary) = graphemes[offset..end]
                .iter()
                .rposition(|part| matches!(*part, " " | "-" | "_"))
            && boundary > 0
        {
            end = offset + boundary + 1;
            line = graphemes[offset..end].concat();
        }
        lines.push(line.trim().to_string());
        offset = end;
    }
    Ok(lines)
}

fn draw_text(
    fonts: &mut FontSystem,
    image: &mut RgbImage,
    text: &str,
    rect: (f32, f32, f32, f32),
    color: [u8; 3],
) -> Result<(), String> {
    let mut glyphs = fonts
        .shape(text, FontStyle::Regular)
        .map_err(|error| error.to_string())?;
    if glyphs.iter().any(|glyph| glyph.glyph == 0) {
        let mut supported = String::new();
        for cluster in text.graphemes(true) {
            let shapes = fonts
                .shape(cluster, FontStyle::Regular)
                .map_err(|error| error.to_string())?;
            supported.push_str(if shapes.iter().any(|glyph| glyph.glyph == 0) {
                "?"
            } else {
                cluster
            });
        }
        glyphs = fonts
            .shape(&supported, FontStyle::Regular)
            .map_err(|error| error.to_string())?;
    }
    let mut pixels = Vec::new();
    let (mut left, mut top, mut right, mut bottom) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for glyph in glyphs {
        let bitmap = fonts.rasterize(&glyph).map_err(|error| error.to_string())?;
        if bitmap.width == 0 || bitmap.height == 0 {
            continue;
        }
        let x = glyph.x.round() as i32 + bitmap.bearing_x;
        let y = -glyph.y.round() as i32 - bitmap.bearing_y;
        left = left.min(x);
        top = top.min(y);
        right = right.max(x + bitmap.width as i32);
        bottom = bottom.max(y + bitmap.height as i32);
        pixels.push((x, y, bitmap));
    }
    if pixels.is_empty() {
        return Ok(());
    }
    let scale = image.width() as f32 / 72.0;
    let (rx, ry, rw, rh) = (
        rect.0 * scale,
        rect.1 * scale,
        rect.2 * scale,
        rect.3 * scale,
    );
    let ox = (rx + (rw - (right - left) as f32) / 2.0).round() as i32 - left;
    let oy = (ry + (rh - (bottom - top) as f32) / 2.0).round() as i32 - top;
    for (x, y, bitmap) in pixels {
        for by in 0..bitmap.height {
            for bx in 0..bitmap.width {
                let px = x + ox + bx as i32;
                let py = y + oy + by as i32;
                if px < rx as i32
                    || py < ry as i32
                    || px >= (rx + rw) as i32
                    || py >= (ry + rh) as i32
                    || px < 0
                    || py < 0
                    || px >= image.width() as i32
                    || py >= image.height() as i32
                {
                    continue;
                }
                composite(
                    &mut image.get_pixel_mut(px as u32, py as u32).0,
                    &bitmap,
                    (by * bitmap.width + bx) as usize,
                    color,
                );
            }
        }
    }
    Ok(())
}

fn composite(dst: &mut [u8; 3], bitmap: &GlyphBitmap, index: usize, color: [u8; 3]) {
    let (source, alpha) = match bitmap.format {
        BitmapFormat::Alpha => {
            let alpha = u32::from(bitmap.pixels[index]);
            (color.map(|c| (u32::from(c) * alpha + 127) / 255), alpha)
        }
        BitmapFormat::Rgba => {
            let p = &bitmap.pixels[index * 4..index * 4 + 4];
            (
                [u32::from(p[0]), u32::from(p[1]), u32::from(p[2])],
                u32::from(p[3]),
            )
        }
    };
    for (dst, src) in dst.iter_mut().zip(source) {
        *dst = (src + (u32::from(*dst) * (255 - alpha) + 127) / 255).min(255) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplied_color_is_not_multiplied_twice() {
        let bitmap = GlyphBitmap {
            width: 1,
            height: 1,
            bearing_x: 0,
            bearing_y: 0,
            format: BitmapFormat::Rgba,
            pixels: vec![100, 50, 0, 128],
        };
        let mut dst = [20, 40, 60];
        composite(&mut dst, &bitmap, 0, [255; 3]);
        assert_eq!(dst, [110, 70, 30]);
    }

    #[test]
    fn labels_wrap_at_graphemes_without_shrinking() {
        let mut renderer = Renderer::new((72, 72)).unwrap();
        assert_eq!(
            wrap_label(&mut renderer.label, "foo-bar", 52.0).unwrap(),
            ["foo-bar"]
        );
        let slug = wrap_label(&mut renderer.label, "foo-bar-baz-quux", 52.0).unwrap();
        assert_eq!(slug.len(), 2);
        assert!(slug[0].ends_with('-'));
        assert!("foo-bar-baz-quux".starts_with(slug.concat().trim_end_matches('…')));
        let lines = wrap_label(
            &mut renderer.label,
            "Review e\u{301} 👩‍💻 Δ a very long thread label",
            52.0,
        )
        .unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with('…'));
        for line in lines {
            assert!(text_width(&mut renderer.label, &line).unwrap() <= 52.0);
        }
        let lines = wrap_label(&mut renderer.label, "long-worktree-task-slug ·4096", 52.0).unwrap();
        assert!(lines[1].ends_with("… ·4096"));
        assert!(text_width(&mut renderer.label, &lines[1]).unwrap() <= 52.0);
        let visual = Visual::Agent {
            label: "\u{10ffff}".into(),
            state: Some(State::Working),
            focused: true,
            reserved: false,
            moving: false,
        };
        assert_eq!(renderer.render(&visual).unwrap().dimensions(), (72, 72));
        assert!(Renderer::new((72, 80)).is_err());
    }
}
