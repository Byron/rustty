use crate::{
    BitmapFormat, FontConfig, FontError, FontFeature, FontId, FontMetrics, FontStyle,
    FontStyleRequest, GlyphBitmap, ShapedGlyph,
};
use objc2_core_foundation::{
    CFArray, CFAttributedString, CFData, CFDictionary, CFMutableAttributedString, CFNumber,
    CFRange, CFRetained, CFString, CFType, CGPoint, CGSize,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, CGImageByteOrderInfo,
    CGTextDrawingMode, kCGColorSpaceLinearGray, kCGColorSpaceSRGB,
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
    bold_strokes: Vec<f64>,
    mappings: Vec<Option<FontId>>,
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
            || (0..4)
                .flat_map(|style| config.style_variations(style))
                .any(|v| !v.value.is_finite())
        {
            return Err(error("invalid font feature or variation"));
        }
        if config
            .codepoint_map
            .iter()
            .any(|m| m.start > m.end || m.end > 0x10ffff)
        {
            return Err(error("invalid font codepoint mapping"));
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
                if let Some(base) = load_family(family, pixels, &known_names)? {
                    let font = match &config.style_requests[index] {
                        FontStyleRequest::Named(name) => {
                            named_style(&base, name).unwrap_or_else(|| {
                                let warning = format!("{family} ({name})");
                                if !warnings.contains(&warning) {
                                    warnings.push(warning);
                                }
                                styled(base, index)
                            })
                        }
                        _ => styled(base, index),
                    };
                    requested.push(font);
                } else if !warnings.iter().any(|w| w == family) {
                    warnings.push(family.clone());
                }
            }
            if requested.is_empty() && index > 0 {
                for family in &config.families {
                    if let Some(font) = load_family(family, pixels, &known_names)? {
                        requested.push(styled(font, index));
                    }
                }
            }
            let mut builtin = embedded(if index >= 2 { ITALIC } else { REGULAR }, pixels)?;
            if index == 1 || index == 3 {
                builtin = with_variation(&builtin, *b"wght", 700.0);
            }
            requested.push(builtin);
            requested.push(symbols.clone());
            requested.push(emoji.clone());
            for font in &mut requested {
                for variation in config.style_variations(index) {
                    *font = with_variation(font, variation.tag, variation.value);
                }
            }
            let mut base = requested.remove(0);
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
        let mut mappings = Vec::new();
        for mapping in &config.codepoint_map {
            if let Some(mut font) = load_family(&mapping.family, pixels, &known_names)? {
                if let Some(features) =
                    unsafe { fonts[styles[0].0].attribute(kCTFontFeatureSettingsAttribute) }
                {
                    let attributes = unsafe {
                        CFDictionary::<CFString, CFType>::from_slices(
                            &[kCTFontFeatureSettingsAttribute],
                            &[&features],
                        )
                    };
                    let descriptor =
                        unsafe { CTFontDescriptor::with_attributes(attributes.as_opaque()) };
                    font =
                        unsafe { font.copy_with_attributes(0.0, ptr::null(), Some(&descriptor)) };
                }
                let id = FontId(fonts.len());
                fonts.push(font);
                mappings.push(Some(id));
            } else {
                if !mapping.family.is_empty() && !warnings.contains(&mapping.family) {
                    warnings.push(mapping.family.clone());
                }
                mappings.push(None);
            }
        }
        let bold_strokes = vec![0.0; fonts.len()];
        let metrics = metrics(&fonts[styles[0].0]);
        Ok(Self {
            config,
            fonts,
            bold_strokes,
            mappings,
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

    /// An installed override with this glyph takes priority over procedural sprites.
    pub fn has_codepoint_override(&self, cp: char) -> bool {
        self.mapped_font(cp).is_some()
    }

    fn mapped_font(&self, cp: char) -> Option<FontId> {
        let index = self
            .config
            .codepoint_map
            .iter()
            .rposition(|m| (m.start..=m.end).contains(&(cp as u32)))?;
        let id = self.mappings[index]?;
        let mut units = [0; 2];
        let length = cp.encode_utf16(&mut units).len();
        let mut glyphs = [0; 2];
        unsafe {
            self.fonts[id.0].glyphs_for_characters(
                NonNull::from(&mut units[0]),
                NonNull::from(&mut glyphs[0]),
                length as isize,
            )
        }
        .then_some(id)
    }

    pub fn shape(&mut self, text: &str, style: FontStyle) -> Result<Vec<ShapedGlyph>, FontError> {
        self.shape_with_carets(text, style, &[])
            .map(|(glyphs, _)| glyphs)
    }

    /// Shape a run and resolve UTF-8 caret offsets through the native shaper.
    /// This preserves caret positions inside ligatures and combining clusters.
    pub fn shape_with_carets(
        &mut self,
        text: &str,
        style: FontStyle,
        offsets: &[usize],
    ) -> Result<(Vec<ShapedGlyph>, Vec<f32>), FontError> {
        if text.is_empty() {
            return Ok((Vec::new(), vec![0.0; offsets.len()]));
        }
        let string = CFString::from_str(text);
        let style = if self.config.style_requests[style as usize] == FontStyleRequest::Disabled {
            FontStyle::Regular
        } else {
            style
        };
        let base = &self.fonts[self.styles[style as usize].0];
        // Font features/fallback live on the font descriptor, keeping one attributed
        // run available to the shaper for ligatures and combining marks.
        let attrs = unsafe {
            CFDictionary::<CFString, CFType>::from_slices(&[kCTFontAttributeName], &[base])
        };
        let attributed =
            unsafe { CFAttributedString::new(None, Some(&string), Some(attrs.as_opaque())) }
                .ok_or_else(|| error("cannot create attributed text"))?;
        let map = utf16_to_utf8(text);
        let mut override_ranges = Vec::new();
        let attributed = if self.mappings.iter().any(Option::is_some) {
            let mutable = CFMutableAttributedString::new_copy(None, 0, Some(&attributed))
                .ok_or_else(|| error("cannot create mapped text"))?;
            let mut offset = 0;
            while offset < map.len() - 1 {
                let range =
                    unsafe { string.range_of_composed_characters_at_index(offset as isize) };
                let cp = text[map[offset]..]
                    .chars()
                    .next()
                    .expect("valid mapped UTF-16 offset");
                if let Some(id) = self.mapped_font(cp) {
                    unsafe {
                        CFMutableAttributedString::set_attribute(
                            Some(&mutable),
                            range,
                            Some(kCTFontAttributeName),
                            Some(&self.fonts[id.0]),
                        );
                    }
                    override_ranges.push(range);
                }
                offset = (range.location + range.length) as usize;
            }
            // Mutable attributed strings are a documented subclass of immutable ones.
            unsafe { CFRetained::cast_unchecked::<CFAttributedString>(mutable) }
        } else {
            attributed
        };
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
        let line = unsafe {
            typesetter.line(CFRange {
                location: 0,
                length: (map.len() - 1) as isize,
            })
        };
        let carets = offsets
            .iter()
            .map(|byte| {
                let mut byte = (*byte).min(text.len());
                while !text.is_char_boundary(byte) {
                    byte -= 1;
                }
                let index = map.partition_point(|offset| *offset < byte);
                unsafe { line.offset_for_string_index(index as isize, ptr::null_mut()) as f32 }
            })
            .collect();
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
            let mut font = attrs
                .get(unsafe { kCTFontAttributeName })
                .and_then(|v| v.downcast::<CTFont>().ok())
                .ok_or_else(|| error("glyph run has no font"))?;
            let start = unsafe { run.string_range() }.location;
            let mapped = override_ranges
                .iter()
                .any(|r| start >= r.location && start < r.location + r.length);
            let mut stroke = 0.0;
            let traits = unsafe { font.symbolic_traits() };
            if !mapped
                && !traits.contains(CTFontSymbolicTraits::TraitColorGlyphs)
                && style != FontStyle::Regular
                && self.config.synthetic_styles[style as usize - 1]
                && self.config.style_requests[style as usize] == FontStyleRequest::Default
            {
                if matches!(style, FontStyle::Bold | FontStyle::BoldItalic)
                    && !traits.contains(CTFontSymbolicTraits::TraitBold)
                {
                    stroke = unsafe { font.size() } * 0.025;
                }
                if matches!(style, FontStyle::Italic | FontStyle::BoldItalic)
                    && !traits.contains(CTFontSymbolicTraits::TraitItalic)
                {
                    let mut matrix = unsafe { font.matrix() };
                    matrix.c += 0.2;
                    font = unsafe { font.copy_with_attributes(0.0, &matrix, None) };
                }
            }
            let font_id = if let Some(index) = self
                .fonts
                .iter()
                .enumerate()
                .position(|(i, known)| known == &font && self.bold_strokes[i] == stroke)
            {
                FontId(index)
            } else {
                let index = self.fonts.len();
                self.fonts.push(font);
                self.bold_strokes.push(stroke);
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
        Ok((result, carets))
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
        let stroke = self.bold_strokes[glyph.font.0];
        let padding = if (self.config.thicken || stroke > 0.0) && !color {
            1.0
        } else {
            0.0
        } + stroke.ceil();
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
            CGColorSpace::with_name(Some(unsafe { kCGColorSpaceLinearGray }))
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
            let strength = if self.config.thicken {
                f64::from(self.config.thicken_strength) / 255.0
            } else {
                1.0
            };
            CGContext::set_gray_fill_color(Some(&context), strength, 1.0);
            CGContext::set_gray_stroke_color(Some(&context), strength, 1.0);
            if stroke > 0.0 {
                CGContext::set_text_drawing_mode(Some(&context), CGTextDrawingMode::FillStroke);
                CGContext::set_line_width(Some(&context), stroke);
            }
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

fn load_family(
    name: &str,
    pixels: f64,
    known: &HashSet<String>,
) -> Result<Option<CFRetained<CTFont>>, FontError> {
    let name_lower = name.to_lowercase();
    if matches!(
        name_lower.as_str(),
        "jetbrains mono" | "jetbrainsmono-regular"
    ) {
        return embedded(REGULAR, pixels).map(Some);
    }
    if matches!(
        name_lower.as_str(),
        "symbols nerd font" | "symbols nerd font mono" | "symbolsnerdfont-regular"
    ) {
        return embedded(SYMBOLS, pixels).map(Some);
    }
    Ok(known
        .contains(&name_lower)
        .then(|| unsafe { CTFont::with_name(&CFString::from_str(name), pixels, ptr::null()) }))
}

fn named_style(font: &CTFont, name: &str) -> Option<CFRetained<CTFont>> {
    let family = unsafe { font.family_name() };
    if family.to_string().eq_ignore_ascii_case("JetBrains Mono") {
        let name = name.to_lowercase().replace([' ', '-'], "");
        let italic = name.ends_with("italic");
        let weight = match name.trim_end_matches("italic") {
            "thin" => 100.0,
            "extralight" => 200.0,
            "light" => 300.0,
            "" | "regular" => 400.0,
            "medium" => 500.0,
            "semibold" => 600.0,
            "bold" => 700.0,
            "extrabold" => 800.0,
            _ => return None,
        };
        let base = embedded(if italic { ITALIC } else { REGULAR }, unsafe {
            font.size()
        })
        .ok()?;
        return Some(with_variation(&base, *b"wght", weight));
    }
    let style = CFString::from_str(name);
    let attrs = unsafe {
        CFDictionary::<CFString, CFType>::from_slices(
            &[kCTFontFamilyNameAttribute, kCTFontStyleNameAttribute],
            &[&family, &style],
        )
    };
    let descriptor = unsafe { CTFontDescriptor::with_attributes(attrs.as_opaque()) };
    let matched = unsafe { descriptor.matching_font_descriptor(None) }?;
    let matched_family = unsafe { matched.attribute(kCTFontFamilyNameAttribute) }?
        .downcast::<CFString>()
        .ok()?;
    if !matched_family
        .to_string()
        .eq_ignore_ascii_case(&family.to_string())
    {
        return None;
    }
    let actual = unsafe { matched.attribute(kCTFontStyleNameAttribute) }?
        .downcast::<CFString>()
        .ok()?;
    if !actual.to_string().eq_ignore_ascii_case(name) {
        return None;
    }
    Some(unsafe { CTFont::with_font_descriptor(&matched, font.size(), ptr::null()) })
}

fn with_variation(font: &CTFont, tag: [u8; 4], value: f64) -> CFRetained<CTFont> {
    // CoreText otherwise clamps invalid requests, while Ghostty ignores them.
    let supported = unsafe { font.variation_axes() }.is_some_and(|axes| {
        let axes = unsafe {
            CFRetained::cast_unchecked::<CFArray<CFDictionary<CFString, CFNumber>>>(axes)
        };
        axes.iter().any(|axis| unsafe {
            axis.get(kCTFontVariationAxisIdentifierKey)
                .and_then(|n| n.as_i64())
                == Some(i64::from(u32::from_be_bytes(tag)))
                && axis
                    .get(kCTFontVariationAxisMinimumValueKey)
                    .and_then(|n| n.as_f64())
                    .is_some_and(|min| value >= min)
                && axis
                    .get(kCTFontVariationAxisMaximumValueKey)
                    .and_then(|n| n.as_f64())
                    .is_some_and(|max| value <= max)
        })
    });
    if !supported {
        return unsafe { CFRetained::retain(NonNull::from(font)) };
    }
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
    use crate::{CodepointMap, FontVariation};

    fn coverage(fonts: &mut FontSystem, text: &str, style: FontStyle) -> u64 {
        let glyphs = fonts.shape(text, style).unwrap();
        glyphs
            .iter()
            .map(|g| {
                fonts
                    .rasterize(g)
                    .unwrap()
                    .pixels
                    .iter()
                    .map(|p| u64::from(*p))
                    .sum::<u64>()
            })
            .sum()
    }

    #[test]
    fn style_axes_named_styles_and_disabled_styles_reach_native_glyphs() {
        let mut fonts = FontSystem::new(FontConfig {
            variations: vec![FontVariation {
                tag: *b"wght",
                value: 100.0,
            }],
            bold_variations: vec![FontVariation {
                tag: *b"wght",
                value: 800.0,
            }],
            ..Default::default()
        })
        .unwrap();
        let thin = coverage(&mut fonts, "M", FontStyle::Regular);
        assert!(coverage(&mut fonts, "M", FontStyle::Bold) > thin * 2);
        let mut named = FontSystem::new(FontConfig {
            families: vec!["JetBrains Mono".into()],
            style_requests: [
                FontStyleRequest::Named("Thin".into()),
                FontStyleRequest::Disabled,
                FontStyleRequest::Default,
                FontStyleRequest::Default,
            ],
            ..Default::default()
        })
        .unwrap();
        assert_eq!(coverage(&mut named, "M", FontStyle::Regular), thin);
        assert_eq!(
            named.shape("M", FontStyle::Regular).unwrap(),
            named.shape("M", FontStyle::Bold).unwrap()
        );
        assert!(named.missing_families().is_empty());
        let mut invalid = FontSystem::new(FontConfig {
            variations: vec![FontVariation {
                tag: *b"wght",
                value: 9999.0,
            }],
            ..Default::default()
        })
        .unwrap();
        let mut baseline = FontSystem::new(FontConfig::default()).unwrap();
        assert_eq!(
            coverage(&mut invalid, "M", FontStyle::Regular),
            coverage(&mut baseline, "M", FontStyle::Regular)
        );
    }

    #[test]
    fn codepoint_overrides_preserve_graphemes_utf16_offsets_and_last_mapping() {
        let mut fonts = FontSystem::new(FontConfig {
            codepoint_map: vec![
                CodepointMap {
                    start: 'A' as u32,
                    end: 'z' as u32,
                    family: "Menlo".into(),
                },
                CodepointMap {
                    start: 'B' as u32,
                    end: 'B' as u32,
                    family: "Helvetica".into(),
                },
            ],
            ..Default::default()
        })
        .unwrap();
        let text = "A🙂e\u{301}B";
        let glyphs = fonts.shape(text, FontStyle::Bold).unwrap();
        for (byte, expected) in [(0, "Menlo"), (5, "Menlo"), (8, "Helvetica")] {
            let glyph = glyphs.iter().find(|g| g.cluster == byte).unwrap();
            assert!(fonts.font_name(glyph.font).unwrap().starts_with(expected));
            assert_eq!(fonts.bold_strokes[glyph.font.0], 0.0);
        }
        assert!(glyphs.iter().all(|g| text.is_char_boundary(g.cluster)));
        assert!(fonts.has_codepoint_override('A'));
        assert!(!fonts.has_codepoint_override('🙂'));
    }

    #[test]
    fn synthetic_styles_and_zero_thickening_strength_still_draw() {
        let mut fonts = FontSystem::new(FontConfig {
            families: vec!["Symbols Nerd Font".into()],
            ..Default::default()
        })
        .unwrap();
        let regular = coverage(&mut fonts, "\u{e0a0}", FontStyle::Regular);
        let bold = coverage(&mut fonts, "\u{e0a0}", FontStyle::Bold);
        assert!(regular > 0 && bold > regular);
        let regular_glyph = fonts
            .shape("\u{e0a0}", FontStyle::Regular)
            .unwrap()
            .remove(0);
        let italic_glyph = fonts
            .shape("\u{e0a0}", FontStyle::Italic)
            .unwrap()
            .remove(0);
        assert_ne!(
            fonts.rasterize(&regular_glyph).unwrap(),
            fonts.rasterize(&italic_glyph).unwrap()
        );
        let mut disabled = FontSystem::new(FontConfig {
            families: vec!["Symbols Nerd Font".into()],
            synthetic_styles: [false; 3],
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            coverage(&mut disabled, "\u{e0a0}", FontStyle::Regular),
            coverage(&mut disabled, "\u{e0a0}", FontStyle::Bold)
        );
        let mut thicken = FontSystem::new(FontConfig {
            thicken: true,
            thicken_strength: 0,
            ..Default::default()
        })
        .unwrap();
        assert!(coverage(&mut thicken, "M", FontStyle::Regular) > 0);
    }

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
