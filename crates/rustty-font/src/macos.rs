use crate::{
    BitmapFormat, FontConfig, FontError, FontFeature, FontId, FontMetrics, FontStyle, GlyphBitmap,
    ShapedGlyph,
};
use objc2_core_foundation::{
    CFArray, CFAttributedString, CFData, CFDictionary, CFNumber, CFRange, CFRetained, CFString,
    CFType, CGPoint, CGSize,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, CGImageByteOrderInfo,
    kCGColorSpaceSRGB,
};
use objc2_core_text::*;
use std::collections::HashSet;
use std::ptr::{self, NonNull};

const REGULAR: &[u8] = include_bytes!("../resources/JetBrainsMono[wght].ttf");
const ITALIC: &[u8] = include_bytes!("../resources/JetBrainsMono-Italic[wght].ttf");
const SYMBOLS: &[u8] = include_bytes!("../resources/SymbolsNerdFont-Regular.ttf");

/// Native font discovery, shaping, and rasterization. Keep one instance per
/// font configuration/scale; glyph IDs are scoped to this instance.
pub struct FontSystem {
    config: FontConfig,
    fonts: Vec<CFRetained<CTFont>>,
    styles: [FontId; 4],
    metrics: FontMetrics,
    warnings: Vec<String>,
}

impl FontSystem {
    pub fn new(config: FontConfig) -> Result<Self, FontError> {
        let pixels = f64::from(config.size_points) * f64::from(config.scale_factor);
        if !pixels.is_finite() || !(1.0..=1024.0).contains(&pixels) {
            return Err(error(
                "font size must be between 1 and 1024 physical pixels",
            ));
        }
        if config.features.iter().any(|f| !f.tag.is_ascii())
            || config
                .variations
                .iter()
                .any(|v| !v.tag.is_ascii() || !v.value.is_finite())
        {
            return Err(error("invalid font feature or variation"));
        }

        let mut fonts = Vec::new();
        let mut styles = [FontId(0); 4];
        let mut warnings = Vec::new();
        let known_names = available_names();
        let symbols = embedded(SYMBOLS, pixels)?;
        // Native discovery supplies the OS-owned emoji font; it is never bundled.
        let emoji = unsafe {
            CTFont::with_name(
                &CFString::from_str("Apple Color Emoji"),
                pixels,
                ptr::null(),
            )
        };
        for (index, families) in [
            &config.families,
            if config.bold_families.is_empty() {
                &config.families
            } else {
                &config.bold_families
            },
            if config.italic_families.is_empty() {
                &config.families
            } else {
                &config.italic_families
            },
            if config.bold_italic_families.is_empty() {
                &config.families
            } else {
                &config.bold_italic_families
            },
        ]
        .into_iter()
        .enumerate()
        {
            let mut requested = Vec::new();
            for family in families {
                if known_names.contains(&family.to_lowercase()) {
                    let base = unsafe {
                        CTFont::with_name(&CFString::from_str(family), pixels, ptr::null())
                    };
                    requested.push(styled(base, index));
                } else if !warnings.iter().any(|w| w == family) {
                    warnings.push(family.clone());
                }
            }
            let mut builtin = embedded(if index >= 2 { ITALIC } else { REGULAR }, pixels)?;
            if index == 1 || index == 3 {
                builtin = with_variation(&builtin, *b"wght", 700.0);
            }
            requested.push(builtin);
            requested.push(symbols.clone());
            requested.push(emoji.clone());
            let mut base = requested.remove(0);
            for variation in &config.variations {
                base = with_variation(&base, variation.tag, variation.value);
            }
            let cascade: Vec<_> = requested
                .iter()
                .map(|font| unsafe { font.font_descriptor() })
                .collect();
            let cascade = CFArray::from_retained_objects(&cascade);
            let default_liga = FontFeature {
                tag: *b"liga",
                value: 1,
            };
            let default_features =
                (!config.features.iter().any(|f| f.tag == *b"liga")).then_some(&default_liga);
            let feature_dicts: Vec<_> = default_features
                .into_iter()
                .chain(config.features.iter())
                .map(|feature| {
                    let tag = CFString::from_str(
                        std::str::from_utf8(&feature.tag).expect("validated ASCII"),
                    );
                    let value = CFNumber::new_i64(i64::from(feature.value));
                    // CoreText's documented OpenType feature dictionaries retain their values.
                    unsafe {
                        CFDictionary::<CFString, CFType>::from_slices(
                            &[kCTFontOpenTypeFeatureTag, kCTFontOpenTypeFeatureValue],
                            &[&tag, &value],
                        )
                    }
                })
                .collect();
            let features = CFArray::from_retained_objects(&feature_dicts);
            let attributes = unsafe {
                CFDictionary::<CFString, CFType>::from_slices(
                    &[kCTFontCascadeListAttribute, kCTFontFeatureSettingsAttribute],
                    &[&cascade, &features],
                )
            };
            let descriptor = unsafe { CTFontDescriptor::with_attributes(attributes.as_opaque()) };
            base = unsafe { base.copy_with_attributes(pixels, ptr::null(), Some(&descriptor)) };
            styles[index] = FontId(fonts.len());
            fonts.push(base);
        }
        let metrics = metrics(&fonts[0]);
        Ok(Self {
            config,
            fonts,
            styles,
            metrics,
            warnings,
        })
    }

    pub fn metrics(&self) -> FontMetrics {
        self.metrics
    }

    /// Requested families that were not installed; the bundled font was used.
    pub fn missing_families(&self) -> &[String] {
        &self.warnings
    }

    pub fn font_name(&self, id: FontId) -> Option<String> {
        self.fonts
            .get(id.0)
            .map(|font| unsafe { font.post_script_name().to_string() })
    }

    pub fn shape(&mut self, text: &str, style: FontStyle) -> Result<Vec<ShapedGlyph>, FontError> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let string = CFString::from_str(text);
        let base = &self.fonts[self.styles[style as usize].0];
        // Font features/fallback live on the font descriptor, keeping one attributed
        // run available to the shaper for ligatures and combining marks.
        let attrs = unsafe {
            CFDictionary::<CFString, CFType>::from_slices(&[kCTFontAttributeName], &[base])
        };
        let attributed =
            unsafe { CFAttributedString::new(None, Some(&string), Some(attrs.as_opaque())) }
                .ok_or_else(|| error("cannot create attributed text"))?;
        // Terminal cells are already in display order. Disabling bidi here matches
        // Ghostty and prevents a trailing space in RTL text from moving to its start.
        let level = CFNumber::new_i32(0);
        let options = unsafe {
            CFDictionary::from_slices(&[kCTTypesetterOptionForcedEmbeddingLevel], &[&*level])
        };
        let typesetter = unsafe {
            CTTypesetter::with_attributed_string_and_options(&attributed, Some(options.as_opaque()))
        }
        .ok_or_else(|| error("cannot create terminal typesetter"))?;
        let map = utf16_to_utf8(text);
        let line = unsafe {
            typesetter.line(CFRange {
                location: 0,
                length: (map.len() - 1) as isize,
            })
        };
        // CTLine guarantees that the returned array contains CoreText run objects.
        let runs = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(line.glyph_runs()) };
        let mut result = Vec::new();
        for item in runs.iter() {
            let run = item
                .downcast::<CTRun>()
                .map_err(|_| error("invalid CoreText glyph run"))?;
            let count = unsafe { run.glyph_count() } as usize;
            if count == 0 {
                continue;
            }
            let attrs = unsafe {
                CFRetained::cast_unchecked::<CFDictionary<CFString, CFType>>(run.attributes())
            };
            let font = attrs
                .get(unsafe { kCTFontAttributeName })
                .and_then(|v| v.downcast::<CTFont>().ok())
                .ok_or_else(|| error("glyph run has no font"))?;
            let font_id = if let Some(index) = self.fonts.iter().position(|known| known == &font) {
                FontId(index)
            } else {
                let index = self.fonts.len();
                self.fonts.push(font);
                FontId(index)
            };
            let mut glyphs = vec![0u16; count];
            let mut positions = vec![CGPoint::default(); count];
            let mut advances = vec![CGSize::default(); count];
            let mut indices = vec![0isize; count];
            let all = CFRange {
                location: 0,
                length: count as isize,
            };
            // All buffers contain exactly count elements and outlive the native calls.
            unsafe {
                run.glyphs(all, NonNull::new(glyphs.as_mut_ptr()).unwrap());
                run.positions(all, NonNull::new(positions.as_mut_ptr()).unwrap());
                run.advances(all, NonNull::new(advances.as_mut_ptr()).unwrap());
                run.string_indices(all, NonNull::new(indices.as_mut_ptr()).unwrap());
            }
            for i in 0..count {
                let cluster = usize::try_from(indices[i])
                    .ok()
                    .and_then(|i| map.get(i))
                    .copied()
                    .ok_or_else(|| error("CoreText returned an invalid string index"))?;
                result.push(ShapedGlyph {
                    font: font_id,
                    glyph: glyphs[i],
                    cluster,
                    x: positions[i].x as f32,
                    y: positions[i].y as f32,
                    advance: advances[i].width as f32,
                });
            }
        }
        Ok(result)
    }

    pub fn rasterize(&self, glyph: &ShapedGlyph) -> Result<GlyphBitmap, FontError> {
        let font = self
            .fonts
            .get(glyph.font.0)
            .ok_or_else(|| error("glyph belongs to another font system"))?;
        let mut id = glyph.glyph;
        let rect = unsafe {
            font.bounding_rects_for_glyphs(
                CTFontOrientation::Horizontal,
                NonNull::from(&mut id),
                ptr::null_mut(),
                1,
            )
        };
        let color =
            unsafe { font.symbolic_traits() }.contains(CTFontSymbolicTraits::TraitColorGlyphs);
        let format = if color {
            BitmapFormat::Rgba
        } else {
            BitmapFormat::Alpha
        };
        if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
            return Ok(GlyphBitmap {
                width: 0,
                height: 0,
                bearing_x: 0,
                bearing_y: 0,
                format,
                pixels: Vec::new(),
            });
        }
        let padding = if self.config.thicken && !color {
            1.0
        } else {
            0.0
        };
        let left = rect.origin.x.floor() - padding;
        let bottom = rect.origin.y.floor() - padding;
        let width = (rect.origin.x + rect.size.width).ceil() + padding - left;
        let height = (rect.origin.y + rect.size.height).ceil() + padding - bottom;
        if !width.is_finite()
            || !height.is_finite()
            || !(1.0..=8192.0).contains(&width)
            || !(1.0..=8192.0).contains(&height)
        {
            return Err(error("font glyph exceeds rasterization limits"));
        }
        let width = width as usize;
        let height = height as usize;
        let channels = if color { 4 } else { 1 };
        let mut pixels = vec![0u8; width * height * channels];
        // Convert native color glyphs into the explicitly documented sRGB atlas
        // space here; never label Display-P3 bytes as sRGB in the GPU renderer.
        let space = if color {
            CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        } else {
            None
        };
        let flags = if color {
            CGImageAlphaInfo::PremultipliedLast.0 | CGImageByteOrderInfo::Order32Big.0
        } else {
            CGImageAlphaInfo::Only.0
        };
        let context = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                width,
                height,
                8,
                width * channels,
                space.as_deref(),
                flags,
            )
        }
        .ok_or_else(|| error("cannot create glyph bitmap"))?;
        CGContext::set_allows_antialiasing(Some(&context), true);
        CGContext::set_should_antialias(Some(&context), true);
        CGContext::set_allows_font_smoothing(Some(&context), true);
        CGContext::set_should_smooth_fonts(Some(&context), self.config.thicken && !color);
        CGContext::set_allows_font_subpixel_positioning(Some(&context), true);
        CGContext::set_should_subpixel_position_fonts(Some(&context), true);
        CGContext::set_allows_font_subpixel_quantization(Some(&context), false);
        CGContext::set_should_subpixel_quantize_fonts(Some(&context), false);
        if color {
            CGContext::set_rgb_fill_color(Some(&context), 1.0, 1.0, 1.0, 1.0);
        } else {
            CGContext::set_gray_fill_color(Some(&context), 1.0, 1.0);
        }
        let mut position = CGPoint {
            x: -left,
            y: -bottom,
        };
        unsafe {
            font.draw_glyphs(
                NonNull::from(&mut id),
                NonNull::from(&mut position),
                1,
                &context,
            );
        }
        drop(context); // CoreGraphics no longer borrows the pixel storage.
        Ok(GlyphBitmap {
            width: width as u32,
            height: height as u32,
            bearing_x: left as i32,
            bearing_y: (bottom + height as f64) as i32,
            format,
            pixels,
        })
    }
}

fn error(message: &str) -> FontError {
    FontError(message.to_owned())
}

fn embedded(bytes: &'static [u8], pixels: f64) -> Result<CFRetained<CTFont>, FontError> {
    let data = CFData::from_static_bytes(bytes);
    let descriptor = unsafe { CTFontManagerCreateFontDescriptorFromData(&data) }
        .ok_or_else(|| error("invalid bundled font"))?;
    Ok(unsafe { CTFont::with_font_descriptor(&descriptor, pixels, ptr::null()) })
}

fn with_variation(font: &CTFont, tag: [u8; 4], value: f64) -> CFRetained<CTFont> {
    let axis = CFNumber::new_i64(i64::from(u32::from_be_bytes(tag)));
    unsafe {
        let descriptor = font.font_descriptor().copy_with_variation(&axis, value);
        font.copy_with_attributes(0.0, ptr::null(), Some(&descriptor))
    }
}

fn styled(font: CFRetained<CTFont>, style: usize) -> CFRetained<CTFont> {
    let mut traits = CTFontSymbolicTraits::empty();
    if style == 1 || style == 3 {
        traits |= CTFontSymbolicTraits::TraitBold;
    }
    if style >= 2 {
        traits |= CTFontSymbolicTraits::TraitItalic;
    }
    if traits.is_empty() {
        return font;
    }
    unsafe { font.copy_with_symbolic_traits(0.0, ptr::null(), traits, traits) }.unwrap_or(font)
}

fn available_names() -> HashSet<String> {
    let mut result = HashSet::new();
    for names in unsafe {
        [
            CTFontManagerCopyAvailableFontFamilyNames(),
            CTFontManagerCopyAvailablePostScriptNames(),
        ]
    } {
        // Both CoreText enumeration functions document arrays of CFString objects.
        let names = unsafe { CFRetained::cast_unchecked::<CFArray<CFString>>(names) };
        for name in names.iter() {
            result.insert(name.to_string().to_lowercase());
        }
    }
    result
}

fn metrics(font: &CTFont) -> FontMetrics {
    let mut characters: Vec<u16> = (0x20..=0x7e).collect();
    let mut glyphs = vec![0u16; characters.len()];
    let mut advances = vec![CGSize::default(); characters.len()];
    unsafe {
        font.glyphs_for_characters(
            NonNull::new(characters.as_mut_ptr()).unwrap(),
            NonNull::new(glyphs.as_mut_ptr()).unwrap(),
            characters.len() as isize,
        );
        font.advances_for_glyphs(
            CTFontOrientation::Horizontal,
            NonNull::new(glyphs.as_mut_ptr()).unwrap(),
            advances.as_mut_ptr(),
            glyphs.len() as isize,
        );
        let ascent = font.ascent();
        let descent = font.descent();
        let leading = font.leading().max(0.0);
        FontMetrics {
            cell_width: advances.iter().map(|a| a.width).fold(1.0, f64::max).ceil() as u32,
            cell_height: (ascent + descent + leading).ceil().max(1.0) as u32,
            baseline: (ascent + leading / 2.0).ceil() as f32,
            underline_position: -font.underline_position() as f32,
            underline_thickness: font.underline_thickness().max(1.0) as f32,
        }
    }
}

fn utf16_to_utf8(text: &str) -> Vec<usize> {
    let mut map = Vec::with_capacity(text.len() + 1);
    for (byte, ch) in text.char_indices() {
        map.extend(std::iter::repeat_n(byte, ch.len_utf16()));
    }
    map.push(text.len());
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_offsets_are_translated_to_utf8_boundaries() {
        assert_eq!(utf16_to_utf8("a🙂é"), [0, 1, 1, 5, 7]);
    }

    #[test]
    fn bundled_fonts_shape_and_rasterize_with_native_fallback() {
        let mut fonts = FontSystem::new(FontConfig::default()).unwrap();
        assert!(fonts.metrics().cell_width > 0);
        for style in [
            FontStyle::Regular,
            FontStyle::Bold,
            FontStyle::Italic,
            FontStyle::BoldItalic,
        ] {
            let glyphs = fonts.shape("Hello", style).unwrap();
            assert_eq!(glyphs.len(), 5);
            assert!(glyphs.iter().all(|g| g.glyph != 0));
            let image = fonts.rasterize(&glyphs[0]).unwrap();
            assert_eq!(image.format, BitmapFormat::Alpha);
            assert!(image.pixels.iter().any(|b| *b != 0));
        }
        let text = "a👩🏽‍💻水e\u{301}";
        let glyphs = fonts.shape(text, FontStyle::Regular).unwrap();
        assert!(glyphs.iter().all(|g| text.is_char_boundary(g.cluster)));
        assert!(glyphs.iter().all(|g| g.glyph != 0));
        assert!(
            glyphs
                .iter()
                .any(|g| fonts.rasterize(g).unwrap().format == BitmapFormat::Rgba)
        );
    }

    #[test]
    fn invalid_sizes_fail_before_native_allocation() {
        for size in [0.0, -1.0, f32::NAN, f32::INFINITY, 2048.0] {
            assert!(
                FontSystem::new(FontConfig {
                    size_points: size,
                    ..FontConfig::default()
                })
                .is_err()
            );
        }
    }

    #[test]
    fn unavailable_family_uses_bundled_font_and_retina_scale_changes_metrics() {
        let mut fonts = FontSystem::new(FontConfig {
            families: vec!["Rustty deliberately absent family".into()],
            ..FontConfig::default()
        })
        .unwrap();
        assert_eq!(
            fonts.missing_families(),
            ["Rustty deliberately absent family"]
        );
        let glyph = fonts.shape("M", FontStyle::Regular).unwrap().remove(0);
        assert!(fonts.font_name(glyph.font).unwrap().contains("JetBrains"));
        let retina = FontSystem::new(FontConfig {
            scale_factor: 2.0,
            ..FontConfig::default()
        })
        .unwrap();
        assert!(retina.metrics().cell_width >= fonts.metrics().cell_width * 2 - 1);
        assert!(retina.metrics().cell_height >= fonts.metrics().cell_height * 2 - 1);
    }

    #[test]
    fn emoji_bitmap_contains_color_and_bold_changes_coverage() {
        let mut fonts = FontSystem::new(FontConfig::default()).unwrap();
        let regular = fonts.shape("M", FontStyle::Regular).unwrap().remove(0);
        let bold = fonts.shape("M", FontStyle::Bold).unwrap().remove(0);
        let regular = fonts.rasterize(&regular).unwrap();
        let bold = fonts.rasterize(&bold).unwrap();
        assert!(
            bold.pixels.iter().map(|b| u64::from(*b)).sum::<u64>()
                > regular.pixels.iter().map(|b| u64::from(*b)).sum::<u64>()
        );
        let emoji = fonts.shape("🙂", FontStyle::Regular).unwrap();
        let bitmap = fonts.rasterize(&emoji[0]).unwrap();
        assert_eq!(bitmap.format, BitmapFormat::Rgba);
        assert!(
            bitmap
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[3] > 0 && (p[0] != p[1] || p[1] != p[2]))
        );
    }
}
