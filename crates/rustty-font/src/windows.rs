//! DirectWrite fonts. Native layouts are used only for shaping and caret positions;
//! the terminal renderer still owns cell placement, glyph caching and presentation.

use crate::{
    BitmapFormat, FontConfig, FontError, FontId, FontMetrics, FontStyle, FontStyleRequest,
    FontVariation, GlyphBitmap, ShapedGlyph,
};
use ::windows::{
    Win32::Graphics::DirectWrite::*,
    core::{BOOL, IUnknown, Interface, PCWSTR, Ref, implement, w},
};
use std::{
    cell::RefCell,
    ffi::c_void,
    mem::ManuallyDrop,
    sync::{Arc, Mutex},
};
use unicode_segmentation::UnicodeSegmentation;

mod color;
#[cfg(test)]
mod tests;

const REGULAR: &[u8] = include_bytes!("../resources/JetBrainsMono[wght].ttf");
const ITALIC: &[u8] = include_bytes!("../resources/JetBrainsMono-Italic[wght].ttf");
const SYMBOLS: &[u8] = include_bytes!("../resources/SymbolsNerdFont-Regular.ttf");

type Result<T> = std::result::Result<T, FontError>;

fn error(value: impl std::fmt::Display) -> FontError {
    FontError(value.to_string())
}

impl From<::windows::core::Error> for FontError {
    fn from(value: ::windows::core::Error) -> Self {
        error(value)
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

// Unregister even when construction fails. The static font bytes outlive every
// registered file; registration is private to this isolated factory.
struct EmbeddedFonts {
    factory: IDWriteFactory7,
    loader: IDWriteInMemoryFontFileLoader,
}

impl Drop for EmbeddedFonts {
    fn drop(&mut self) {
        // SAFETY: this guard owns the matching successful registration.
        let _ = unsafe { self.factory.UnregisterFontFileLoader(&self.loader) };
    }
}

struct Style {
    family: String,
    font: IDWriteFont,
    weight: DWRITE_FONT_WEIGHT,
    slant: DWRITE_FONT_STYLE,
    stretch: DWRITE_FONT_STRETCH,
    axes: Vec<DWRITE_FONT_AXIS_VALUE>,
    fallback: IDWriteFontFallback,
}

struct Face {
    native: IDWriteFontFace,
    pixels: f32,
}

/// One font configuration and scale. Glyph identifiers are local to this instance.
pub struct FontSystem {
    config: FontConfig,
    factory: IDWriteFactory7,
    collection: IDWriteFontCollection,
    styles: Vec<Style>,
    mappings: Vec<Option<Style>>,
    faces: Vec<Face>,
    metrics: FontMetrics,
    warnings: Vec<String>,
    color: RefCell<Option<color::Renderer>>,
    // Keep registration alive until all layouts, collections and faces are gone.
    _embedded: EmbeddedFonts,
}

impl FontSystem {
    pub fn new(config: FontConfig) -> Result<Self> {
        let pixels = config.size_points * config.scale_factor;
        if !pixels.is_finite() || !(1.0..=1024.0).contains(&pixels) {
            return Err(error(
                "font size must be between 1 and 1024 physical pixels",
            ));
        }
        if config.features.iter().any(|f| !f.tag.is_ascii())
            || (0..4)
                .flat_map(|i| config.style_variations(i))
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
        // SAFETY: DirectWrite factories need no COM apartment initialization.
        let factory: IDWriteFactory7 =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_ISOLATED) }.map_err(error)?;
        let loader = unsafe { factory.CreateInMemoryFontFileLoader() }.map_err(error)?;
        unsafe { factory.RegisterFontFileLoader(&loader) }.map_err(error)?;
        let embedded = EmbeddedFonts {
            factory: factory.clone(),
            loader,
        };
        let builder = unsafe { factory.CreateFontSetBuilder() }.map_err(error)?;
        for bytes in [REGULAR, ITALIC, SYMBOLS] {
            // SAFETY: the input is static and the factory registration outlives its files.
            let file = unsafe {
                embedded.loader.CreateInMemoryFontFileReference(
                    &factory,
                    bytes.as_ptr().cast(),
                    bytes.len() as u32,
                    None,
                )
            }
            .map_err(error)?;
            unsafe { IDWriteFontSetBuilder1::AddFontFile(&builder, &file) }.map_err(error)?;
        }
        let system = unsafe { factory.GetSystemFontSet(false) }.map_err(error)?;
        unsafe { builder.AddFontSet(&system) }.map_err(error)?;
        let set = unsafe { builder.CreateFontSet() }.map_err(error)?;
        let collection: IDWriteFontCollection = unsafe {
            factory.CreateFontCollectionFromFontSet(
                &set,
                DWRITE_FONT_FAMILY_MODEL_WEIGHT_STRETCH_STYLE,
            )
        }
        .map_err(error)?
        .cast()
        .map_err(error)?;
        let mut warnings = Vec::new();
        let mut styles = Vec::new();
        for i in 0..4 {
            let requested = match i {
                1 if !config.bold_families.is_empty() => &config.bold_families,
                2 if !config.italic_families.is_empty() => &config.italic_families,
                3 if !config.bold_italic_families.is_empty() => &config.bold_italic_families,
                _ => &config.families,
            };
            let mut families = available_families(&collection, requested, &mut warnings)?;
            if families.is_empty() && i > 0 {
                families = available_families(&collection, &config.families, &mut warnings)?;
            }
            families.extend([
                "JetBrains Mono".into(),
                "Symbols Nerd Font".into(),
                "Segoe UI Emoji".into(),
            ]);
            styles.push(make_style(
                &factory,
                &collection,
                families,
                i,
                &config.style_requests[i],
                config.style_variations(i),
                &mut warnings,
            )?);
        }
        let mut mappings = Vec::new();
        for mapping in &config.codepoint_map {
            let families = available_families(
                &collection,
                std::slice::from_ref(&mapping.family),
                &mut warnings,
            )?;
            mappings.push(if families.is_empty() {
                None
            } else {
                Some(make_style(
                    &factory,
                    &collection,
                    families,
                    0,
                    &FontStyleRequest::Default,
                    &[],
                    &mut warnings,
                )?)
            });
        }
        let face = unsafe { styles[0].font.CreateFontFace() }.map_err(error)?;
        let face = face_with_axes(&face, &styles[0].axes, false)?;
        let metrics = font_metrics(&face, pixels)?;
        Ok(Self {
            config,
            factory,
            collection,
            styles,
            mappings,
            faces: Vec::new(),
            metrics,
            warnings,
            color: RefCell::new(None),
            _embedded: embedded,
        })
    }

    pub fn metrics(&self) -> FontMetrics {
        self.metrics
    }

    pub fn missing_families(&self) -> &[String] {
        &self.warnings
    }

    pub fn font_name(&self, id: FontId) -> Option<String> {
        let face: IDWriteFontFace3 = self.faces.get(id.0)?.native.cast().ok()?;
        let mut strings = None;
        let mut exists = BOOL(0);
        unsafe {
            face.GetInformationalStrings(
                DWRITE_INFORMATIONAL_STRING_POSTSCRIPT_NAME,
                &mut strings,
                &mut exists,
            )
        }
        .ok()?;
        if exists.as_bool() {
            localized(&strings?).ok()
        } else {
            localized(&unsafe { face.GetFamilyNames() }.ok()?).ok()
        }
    }

    pub fn has_codepoint_override(&self, cp: char) -> bool {
        self.mapped_style(cp).is_some()
    }

    fn mapped_style(&self, cp: char) -> Option<&Style> {
        let i = self
            .config
            .codepoint_map
            .iter()
            .rposition(|m| (m.start..=m.end).contains(&(cp as u32)))?;
        let style = self.mappings[i].as_ref()?;
        let supported = unsafe { style.font.HasCharacter(cp as u32) }.ok()?;
        supported.as_bool().then_some(style)
    }

    pub fn shape(&mut self, text: &str, style: FontStyle) -> Result<Vec<ShapedGlyph>> {
        self.shape_with_carets(text, style, &[])
            .map(|(glyphs, _)| glyphs)
    }

    pub fn shape_with_carets(
        &mut self,
        text: &str,
        style: FontStyle,
        offsets: &[usize],
    ) -> Result<(Vec<ShapedGlyph>, Vec<f32>)> {
        if text.is_empty() {
            return Ok((Vec::new(), vec![0.0; offsets.len()]));
        }
        let index = if self.config.style_requests[style as usize] == FontStyleRequest::Disabled {
            0
        } else {
            style as usize
        };
        let plan = &self.styles[index];
        let pixels = self.config.size_points * self.config.scale_factor;
        let family = wide(&plan.family);
        let factory: IDWriteFactory = self.factory.cast().map_err(error)?;
        let format = unsafe {
            factory.CreateTextFormat(
                PCWSTR(family.as_ptr()),
                &self.collection,
                plan.weight,
                plan.slant,
                plan.stretch,
                pixels,
                w!("en-us"),
            )
        }
        .map_err(error)?;
        unsafe { format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP) }.map_err(error)?;
        // Terminal cells are already in display order. An LTR override prevents
        // native paragraph bidi reordering while preserving contextual shaping.
        // The two formatting controls are excluded from returned glyph clusters.
        let mut units = vec![0x202d];
        units.extend(text.encode_utf16());
        units.push(0x202c);
        let layout: IDWriteTextLayout4 =
            unsafe { factory.CreateTextLayout(&units, &format, 16_777_216.0, 16384.0) }
                .map_err(error)?
                .cast()
                .map_err(error)?;
        let whole = DWRITE_TEXT_RANGE {
            startPosition: 0,
            length: units.len() as u32,
        };
        unsafe {
            layout.SetFontFallback(&plan.fallback)?;
            layout.SetFontAxisValues(&plan.axes, whole)?;
            layout.SetAutomaticFontAxes(DWRITE_AUTOMATIC_FONT_AXES_NONE)?;
        }
        let typography = unsafe { factory.CreateTypography() }.map_err(error)?;
        if !self.config.features.iter().any(|f| f.tag == *b"liga") {
            unsafe {
                typography.AddFontFeature(DWRITE_FONT_FEATURE {
                    nameTag: DWRITE_FONT_FEATURE_TAG_STANDARD_LIGATURES,
                    parameter: 1,
                })
            }
            .map_err(error)?;
        }
        if !self.config.features.iter().any(|f| f.tag == *b"calt") {
            unsafe {
                typography.AddFontFeature(DWRITE_FONT_FEATURE {
                    nameTag: DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_ALTERNATES,
                    parameter: 1,
                })
            }
            .map_err(error)?;
        }
        for feature in &self.config.features {
            unsafe {
                typography.AddFontFeature(DWRITE_FONT_FEATURE {
                    nameTag: DWRITE_FONT_FEATURE_TAG(u32::from_le_bytes(feature.tag)),
                    parameter: feature.value,
                })
            }
            .map_err(error)?;
        }
        unsafe { layout.SetTypography(&typography, whole) }.map_err(error)?;
        let mut mapped_ranges = Vec::new();
        let mut utf16 = 1u32;
        for grapheme in text.graphemes(true) {
            let length = grapheme.encode_utf16().count() as u32;
            if let Some(mapped) = self.mapped_style(grapheme.chars().next().unwrap()) {
                let range = DWRITE_TEXT_RANGE {
                    startPosition: utf16,
                    length,
                };
                let name = wide(&mapped.family);
                unsafe {
                    layout.SetFontFamilyName(PCWSTR(name.as_ptr()), range)?;
                    layout.SetFontWeight(mapped.weight, range)?;
                    layout.SetFontStyle(mapped.slant, range)?;
                    layout.SetFontStretch(mapped.stretch, range)?;
                    layout.SetFontAxisValues(&mapped.axes, range)?;
                }
                mapped_ranges.push(utf16..utf16 + length);
            }
            utf16 += length;
        }
        let map = utf16_to_utf8(text);
        let mut carets = Vec::with_capacity(offsets.len());
        for byte in offsets {
            let mut byte = (*byte).min(text.len());
            while !text.is_char_boundary(byte) {
                byte -= 1;
            }
            let position = map.partition_point(|v| *v < byte) as u32 + 1;
            let (mut x, mut y, mut metrics) = (0.0, 0.0, DWRITE_HIT_TEST_METRICS::default());
            unsafe { layout.HitTestTextPosition(position, false, &mut x, &mut y, &mut metrics) }
                .map_err(error)?;
            if position > metrics.textPosition && position < metrics.textPosition + metrics.length {
                // DirectWrite reports only the outside edges of a ligature.
                // Divide its native advance among graphemes, never UTF-16 units:
                // the IME can place a caret inside "fi", but not a surrogate
                // pair, combining sequence or joined emoji.
                let start =
                    map[(metrics.textPosition.saturating_sub(1) as usize).min(map.len() - 1)];
                let end = map[((metrics.textPosition + metrics.length).saturating_sub(1) as usize)
                    .min(map.len() - 1)];
                let mut boundaries: Vec<_> = text[start..end]
                    .grapheme_indices(true)
                    .map(|(i, _)| start + i)
                    .collect();
                let count = boundaries.len();
                boundaries.push(end);
                let before = boundaries
                    .partition_point(|offset| *offset <= byte)
                    .saturating_sub(1);
                if count > 0 {
                    x = metrics.left + metrics.width * before as f32 / count as f32;
                }
            }
            carets.push(x);
        }
        let runs = Arc::new(Mutex::new(Vec::new()));
        let renderer: IDWriteTextRenderer = Capture { runs: runs.clone() }.into();
        unsafe { layout.Draw(None, &renderer, 0.0, 0.0) }.map_err(error)?;
        let mut result = Vec::new();
        let runs = runs.lock().map_err(error)?;
        let baseline = runs.first().map_or(0.0, |r: &Run| r.y);
        for run in runs.iter() {
            let mapped = mapped_ranges
                .iter()
                .any(|range| range.contains(&run.position));
            let remove_simulations = mapped
                || index == 0
                || !self.config.synthetic_styles[index - 1]
                || self.config.style_requests[index] != FontStyleRequest::Default;
            let native = face_with_axes(&run.face, &[], remove_simulations)?;
            let font = if let Some(i) = self
                .faces
                .iter()
                .position(|f| f.native == native && f.pixels == run.pixels)
            {
                FontId(i)
            } else {
                let id = FontId(self.faces.len());
                self.faces.push(Face {
                    native,
                    pixels: run.pixels,
                });
                id
            };
            // Invert DirectWrite's UTF-16-to-glyph map once. Looking up the
            // originating cluster for each glyph separately makes long rows
            // quadratic in the number of characters.
            let mut clusters = vec![None; run.glyphs.len()];
            for (unit, &glyph) in run.clusters.iter().enumerate() {
                let entry = clusters
                    .get_mut(usize::from(glyph))
                    .ok_or_else(|| error("DirectWrite returned an invalid glyph cluster"))?;
                entry.get_or_insert(unit);
            }
            let mut cluster = 0;
            let mut x = run.x;
            for (i, &glyph) in run.glyphs.iter().enumerate() {
                // A cluster can contain several glyphs or UTF-16 code units.
                if let Some(unit) = clusters[i] {
                    cluster = unit;
                }
                let unit = run.position as usize + cluster;
                if unit > 0 && unit < map.len() {
                    result.push(ShapedGlyph {
                        font,
                        glyph,
                        cluster: map[unit - 1],
                        x: x + run.offsets[i].advanceOffset,
                        y: baseline - run.y + run.offsets[i].ascenderOffset,
                        advance: run.advances[i],
                    });
                }
                x += run.advances[i];
            }
        }
        Ok((result, carets))
    }

    pub fn rasterize(&self, glyph: &ShapedGlyph) -> Result<GlyphBitmap> {
        let face = self
            .faces
            .get(glyph.font.0)
            .ok_or_else(|| error("glyph belongs to another font system"))?;
        let run = SingleGlyph::new(&face.native, face.pixels, &glyph.glyph);
        let modern: IDWriteFontFace4 = face.native.cast().map_err(error)?;
        let ppem = face.pixels.ceil() as u32;
        let mut formats =
            unsafe { modern.GetGlyphImageFormats(glyph.glyph, ppem, ppem) }.map_err(error)?;
        // IDWriteFontFace4's per-glyph query predates COLRv1. Current Windows
        // emoji fonts expose paint trees through the font-wide format flags.
        if formats.0 == 0 {
            formats = unsafe { modern.GetGlyphImageFormats2() }
                & DWRITE_GLYPH_IMAGE_FORMATS_COLR_PAINT_TREE;
        }
        let color_formats = DWRITE_GLYPH_IMAGE_FORMATS_COLR
            | DWRITE_GLYPH_IMAGE_FORMATS_COLR_PAINT_TREE
            | DWRITE_GLYPH_IMAGE_FORMATS_PNG
            | DWRITE_GLYPH_IMAGE_FORMATS_JPEG
            | DWRITE_GLYPH_IMAGE_FORMATS_TIFF
            | DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8
            | DWRITE_GLYPH_IMAGE_FORMATS_SVG;
        if (formats & color_formats).0 != 0 {
            let mut color = self.color.borrow_mut();
            if color.is_none() {
                *color = Some(color::Renderer::new()?);
            }
            return color
                .as_ref()
                .unwrap()
                .rasterize(&self.factory, &run.raw, formats);
        }
        let analysis = unsafe {
            self.factory.CreateGlyphRunAnalysis(
                &run.raw,
                None,
                DWRITE_RENDERING_MODE1_NATURAL_SYMMETRIC,
                DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_GRID_FIT_MODE_ENABLED,
                DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE,
                0.0,
                0.0,
            )
        }
        .map_err(error)?;
        let rect =
            unsafe { analysis.GetAlphaTextureBounds(DWRITE_TEXTURE_ALIASED_1x1) }.map_err(error)?;
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return Ok(GlyphBitmap {
                width: 0,
                height: 0,
                bearing_x: 0,
                bearing_y: 0,
                format: BitmapFormat::Alpha,
                pixels: Vec::new(),
            });
        }
        if width > 8192 || height > 8192 {
            return Err(error("font glyph exceeds rasterization limits"));
        }
        let mut pixels = vec![0; width as usize * height as usize];
        unsafe { analysis.CreateAlphaTexture(DWRITE_TEXTURE_ALIASED_1x1, &rect, &mut pixels) }
            .map_err(error)?;
        let mut bitmap = GlyphBitmap {
            width: width as u32,
            height: height as u32,
            bearing_x: rect.left,
            bearing_y: -rect.top,
            format: BitmapFormat::Alpha,
            pixels,
        };
        if self.config.thicken && self.config.thicken_strength > 0 {
            thicken(&mut bitmap, self.config.thicken_strength);
        }
        Ok(bitmap)
    }
}

fn localized(strings: &IDWriteLocalizedStrings) -> Result<String> {
    let (mut index, mut exists) = (0, BOOL(0));
    unsafe { strings.FindLocaleName(w!("en-us"), &mut index, &mut exists) }.map_err(error)?;
    if !exists.as_bool() {
        index = 0;
    }
    let count = unsafe { strings.GetStringLength(index) }.map_err(error)?;
    let mut value = vec![0; count as usize + 1];
    unsafe { strings.GetString(index, &mut value) }.map_err(error)?;
    Ok(String::from_utf16_lossy(&value[..count as usize]))
}

fn find_family(
    collection: &IDWriteFontCollection,
    name: &str,
) -> Result<Option<IDWriteFontFamily>> {
    let utf16_name = wide(name);
    let (mut index, mut exists) = (0, BOOL(0));
    unsafe { collection.FindFamilyName(PCWSTR(utf16_name.as_ptr()), &mut index, &mut exists) }
        .map_err(error)?;
    if exists.as_bool() {
        Ok(Some(
            unsafe { collection.GetFontFamily(index) }.map_err(error)?,
        ))
    } else {
        // Ghostty configuration accepts both family and PostScript names.
        for i in 0..unsafe { collection.GetFontFamilyCount() } {
            let family = unsafe { collection.GetFontFamily(i) }.map_err(error)?;
            if find_postscript_font(&family, name)?.is_some() {
                return Ok(Some(family));
            }
        }
        Ok(None)
    }
}

fn find_postscript_font(family: &IDWriteFontFamily, name: &str) -> Result<Option<IDWriteFont>> {
    for i in 0..unsafe { family.GetFontCount() } {
        let font = unsafe { family.GetFont(i) }.map_err(error)?;
        let (mut strings, mut exists) = (None, BOOL(0));
        unsafe {
            font.GetInformationalStrings(
                DWRITE_INFORMATIONAL_STRING_POSTSCRIPT_NAME,
                &mut strings,
                &mut exists,
            )
        }
        .map_err(error)?;
        if exists.as_bool()
            && let Some(strings) = strings
            && localized(&strings)?.eq_ignore_ascii_case(name)
        {
            return Ok(Some(font));
        }
    }
    Ok(None)
}

fn available_families(
    collection: &IDWriteFontCollection,
    requested: &[String],
    warnings: &mut Vec<String>,
) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for name in requested {
        if find_family(collection, name)?.is_some() {
            result.push(name.clone());
        } else if !warnings.contains(name) {
            warnings.push(name.clone());
        }
    }
    Ok(result)
}

fn make_style(
    factory: &IDWriteFactory7,
    collection: &IDWriteFontCollection,
    families: Vec<String>,
    index: usize,
    request: &FontStyleRequest,
    variations: &[FontVariation],
    warnings: &mut Vec<String>,
) -> Result<Style> {
    let requested = families.first().ok_or_else(|| error("no font family"))?;
    let family = find_family(collection, requested)?
        .ok_or_else(|| error(format!("font family unavailable: {requested}")))?;
    let family_name = localized(&unsafe { family.GetFamilyNames() }.map_err(error)?)?;
    let alias = find_postscript_font(&family, requested)?;
    let mut weight = if index == 1 || index == 3 {
        DWRITE_FONT_WEIGHT_BOLD
    } else {
        alias
            .as_ref()
            .map_or(DWRITE_FONT_WEIGHT_NORMAL, |font| unsafe {
                font.GetWeight()
            })
    };
    let mut slant = if index >= 2 {
        DWRITE_FONT_STYLE_ITALIC
    } else {
        alias
            .as_ref()
            .map_or(DWRITE_FONT_STYLE_NORMAL, |font| unsafe { font.GetStyle() })
    };
    let mut stretch = alias
        .as_ref()
        .map_or(DWRITE_FONT_STRETCH_NORMAL, |font| unsafe {
            font.GetStretch()
        });
    if let FontStyleRequest::Named(name) = request {
        let mut found = false;
        for i in 0..unsafe { family.GetFontCount() } {
            let font = unsafe { family.GetFont(i) }.map_err(error)?;
            if localized(&unsafe { font.GetFaceNames() }.map_err(error)?)?
                .eq_ignore_ascii_case(name)
            {
                weight = unsafe { font.GetWeight() };
                slant = unsafe { font.GetStyle() };
                stretch = unsafe { font.GetStretch() };
                found = true;
                break;
            }
        }
        if !found {
            let named_weight = match name.to_ascii_lowercase().replace([' ', '-'], "").as_str() {
                "thin" => Some(100),
                "extralight" | "ultralight" => Some(200),
                "light" => Some(300),
                "regular" | "normal" => Some(400),
                "medium" => Some(500),
                "semibold" | "demibold" => Some(600),
                "bold" => Some(700),
                "extrabold" | "ultrabold" => Some(800),
                "black" | "heavy" => Some(900),
                _ => None,
            };
            if let Some(value) = named_weight {
                weight = DWRITE_FONT_WEIGHT(value);
            } else {
                let warning = format!("{family_name} ({name})");
                if !warnings.contains(&warning) {
                    warnings.push(warning);
                }
            }
        }
    }
    let font = unsafe { family.GetFirstMatchingFont(weight, stretch, slant) }.map_err(error)?;
    let face: IDWriteFontFace5 = unsafe { font.CreateFontFace() }
        .map_err(error)?
        .cast()
        .map_err(error)?;
    let resource = unsafe { face.GetFontResource() }.map_err(error)?;
    let mut ranges =
        vec![DWRITE_FONT_AXIS_RANGE::default(); unsafe { resource.GetFontAxisCount() } as usize];
    unsafe { resource.GetFontAxisRanges(&mut ranges) }.map_err(error)?;
    let mut axes = Vec::new();
    // Standard style axes are explicit, including for codepoint overrides.
    for (tag, value) in [
        (*b"wght", weight.0 as f64),
        (
            *b"ital",
            if slant == DWRITE_FONT_STYLE_ITALIC {
                1.0
            } else {
                0.0
            },
        ),
    ] {
        add_axis(&mut axes, &ranges, tag, value);
    }
    for variation in variations {
        add_axis(&mut axes, &ranges, variation.tag, variation.value);
    }
    let fallback_builder = unsafe { factory.CreateFontFallbackBuilder() }.map_err(error)?;
    let mut names = Vec::new();
    for requested in &families {
        if let Some(family) = find_family(collection, requested)? {
            names.push(wide(&localized(
                &unsafe { family.GetFamilyNames() }.map_err(error)?,
            )?));
        }
    }
    let pointers: Vec<_> = names.iter().map(|f| f.as_ptr()).collect();
    unsafe {
        fallback_builder.AddMapping(
            &[DWRITE_UNICODE_RANGE {
                first: 0,
                last: 0x10ffff,
            }],
            &pointers,
            collection,
            PCWSTR::null(),
            PCWSTR::null(),
            1.0,
        )?;
        fallback_builder.AddMappings(&factory.GetSystemFontFallback()?)?;
    }
    let fallback = unsafe { fallback_builder.CreateFontFallback() }.map_err(error)?;
    Ok(Style {
        family: family_name,
        font,
        weight,
        slant,
        stretch,
        axes,
        fallback,
    })
}

fn add_axis(
    axes: &mut Vec<DWRITE_FONT_AXIS_VALUE>,
    ranges: &[DWRITE_FONT_AXIS_RANGE],
    tag: [u8; 4],
    value: f64,
) {
    let tag = DWRITE_FONT_AXIS_TAG(u32::from_le_bytes(tag));
    if ranges.iter().any(|r| {
        r.axisTag == tag && value >= f64::from(r.minValue) && value <= f64::from(r.maxValue)
    }) {
        axes.retain(|a| a.axisTag != tag);
        axes.push(DWRITE_FONT_AXIS_VALUE {
            axisTag: tag,
            value: value as f32,
        });
    }
}

fn face_with_axes(
    face: &IDWriteFontFace,
    axes: &[DWRITE_FONT_AXIS_VALUE],
    remove_simulations: bool,
) -> Result<IDWriteFontFace> {
    let simulations = unsafe { face.GetSimulations() };
    if axes.is_empty() && (!remove_simulations || simulations == DWRITE_FONT_SIMULATIONS_NONE) {
        return Ok(face.clone());
    }
    let modern: IDWriteFontFace5 = face.cast().map_err(error)?;
    let mut values =
        vec![DWRITE_FONT_AXIS_VALUE::default(); unsafe { modern.GetFontAxisValueCount() } as usize];
    unsafe { modern.GetFontAxisValues(&mut values) }.map_err(error)?;
    for axis in axes {
        if let Some(value) = values.iter_mut().find(|a| a.axisTag == axis.axisTag) {
            value.value = axis.value;
        } else {
            values.push(*axis);
        }
    }
    unsafe {
        modern.GetFontResource()?.CreateFontFace(
            if remove_simulations {
                DWRITE_FONT_SIMULATIONS_NONE
            } else {
                simulations
            },
            &values,
        )
    }
    .map_err(error)?
    .cast()
    .map_err(error)
}

fn font_metrics(face: &IDWriteFontFace, pixels: f32) -> Result<FontMetrics> {
    let mut metrics = DWRITE_FONT_METRICS::default();
    unsafe {
        face.GetMetrics(&mut metrics);
    }
    let scale = pixels / f32::from(metrics.designUnitsPerEm);
    let characters: Vec<u32> = (0x20..=0x7e).collect();
    let mut glyphs = vec![0; characters.len()];
    let mut advances = vec![DWRITE_GLYPH_METRICS::default(); characters.len()];
    unsafe {
        face.GetGlyphIndices(
            characters.as_ptr(),
            characters.len() as u32,
            glyphs.as_mut_ptr(),
        )?;
        face.GetDesignGlyphMetrics(
            glyphs.as_ptr(),
            glyphs.len() as u32,
            advances.as_mut_ptr(),
            false,
        )?;
    }
    let leading = f32::from(metrics.lineGap).max(0.0) * scale;
    Ok(FontMetrics {
        cell_width: advances
            .iter()
            .map(|m| m.advanceWidth as f32 * scale)
            .fold(1.0, f32::max)
            .ceil() as u32,
        cell_height: ((f32::from(metrics.ascent) + f32::from(metrics.descent)) * scale + leading)
            .ceil()
            .max(1.0) as u32,
        baseline: (f32::from(metrics.ascent) * scale + leading / 2.0).ceil(),
        underline_position: -f32::from(metrics.underlinePosition) * scale,
        underline_thickness: (f32::from(metrics.underlineThickness) * scale).max(1.0),
    })
}

fn utf16_to_utf8(text: &str) -> Vec<usize> {
    let mut result = Vec::new();
    for (byte, cp) in text.char_indices() {
        result.extend(std::iter::repeat_n(byte, cp.len_utf16()));
    }
    result.push(text.len());
    result
}

fn thicken(bitmap: &mut GlyphBitmap, strength: u8) {
    let width = bitmap.width as usize;
    let height = bitmap.height as usize;
    let mut pixels = vec![0; (width + 2) * (height + 2)];
    for y in 0..height {
        for x in 0..width {
            let value = bitmap.pixels[y * width + x];
            let i = (y + 1) * (width + 2) + x + 1;
            pixels[i] = pixels[i].max(value);
            let added = ((u32::from(value) * u32::from(strength)) / (255 * 3)) as u8;
            for j in [i - 1, i + 1] {
                pixels[j] = pixels[j].max(added);
            }
        }
    }
    bitmap.width += 2;
    bitmap.height += 2;
    bitmap.bearing_x -= 1;
    bitmap.bearing_y += 1;
    bitmap.pixels = pixels;
}

struct SingleGlyph<'a> {
    raw: DWRITE_GLYPH_RUN,
    _glyph: &'a u16,
}

impl<'a> SingleGlyph<'a> {
    fn new(face: &IDWriteFontFace, pixels: f32, glyph: &'a u16) -> Self {
        Self {
            raw: DWRITE_GLYPH_RUN {
                fontFace: ManuallyDrop::new(Some(face.clone())),
                fontEmSize: pixels,
                glyphCount: 1,
                glyphIndices: glyph,
                ..Default::default()
            },
            _glyph: glyph,
        }
    }
}

impl Drop for SingleGlyph<'_> {
    fn drop(&mut self) {
        unsafe {
            ManuallyDrop::drop(&mut self.raw.fontFace);
        }
    }
}

struct Run {
    face: IDWriteFontFace,
    pixels: f32,
    x: f32,
    y: f32,
    position: u32,
    glyphs: Vec<u16>,
    advances: Vec<f32>,
    offsets: Vec<DWRITE_GLYPH_OFFSET>,
    clusters: Vec<u16>,
}

#[implement(IDWriteTextRenderer)]
struct Capture {
    runs: Arc<Mutex<Vec<Run>>>,
}

#[allow(non_snake_case)]
impl IDWritePixelSnapping_Impl for Capture_Impl {
    fn IsPixelSnappingDisabled(&self, _: *const c_void) -> ::windows::core::Result<BOOL> {
        Ok(true.into())
    }
    fn GetPixelsPerDip(&self, _: *const c_void) -> ::windows::core::Result<f32> {
        Ok(1.0)
    }
    fn GetCurrentTransform(
        &self,
        _: *const c_void,
        transform: *mut DWRITE_MATRIX,
    ) -> ::windows::core::Result<()> {
        // SAFETY: DirectWrite supplies the writable output pointer for this callback.
        unsafe {
            *transform = DWRITE_MATRIX {
                m11: 1.0,
                m22: 1.0,
                ..Default::default()
            };
        }
        Ok(())
    }
}

#[allow(non_snake_case)]
impl IDWriteTextRenderer_Impl for Capture_Impl {
    fn DrawGlyphRun(
        &self,
        _: *const c_void,
        x: f32,
        y: f32,
        _: DWRITE_MEASURING_MODE,
        raw: *const DWRITE_GLYPH_RUN,
        description: *const DWRITE_GLYPH_RUN_DESCRIPTION,
        _: Ref<IUnknown>,
    ) -> ::windows::core::Result<()> {
        // SAFETY: all pointers are owned by DirectWrite and valid throughout Draw.
        // Copy the arrays and retain the face before the callback returns.
        unsafe {
            let raw = &*raw;
            let desc = &*description;
            let count = raw.glyphCount as usize;
            if count == 0 {
                return Ok(());
            }
            let run = Run {
                face: raw.fontFace.as_ref().unwrap().clone(),
                pixels: raw.fontEmSize,
                x,
                y,
                position: desc.textPosition,
                glyphs: std::slice::from_raw_parts(raw.glyphIndices, count).to_vec(),
                advances: std::slice::from_raw_parts(raw.glyphAdvances, count).to_vec(),
                offsets: if raw.glyphOffsets.is_null() {
                    vec![DWRITE_GLYPH_OFFSET::default(); count]
                } else {
                    std::slice::from_raw_parts(raw.glyphOffsets, count).to_vec()
                },
                clusters: std::slice::from_raw_parts(desc.clusterMap, desc.stringLength as usize)
                    .to_vec(),
            };
            self.runs.lock().unwrap().push(run);
        }
        Ok(())
    }
    fn DrawUnderline(
        &self,
        _: *const c_void,
        _: f32,
        _: f32,
        _: *const DWRITE_UNDERLINE,
        _: Ref<IUnknown>,
    ) -> ::windows::core::Result<()> {
        Ok(())
    }
    fn DrawStrikethrough(
        &self,
        _: *const c_void,
        _: f32,
        _: f32,
        _: *const DWRITE_STRIKETHROUGH,
        _: Ref<IUnknown>,
    ) -> ::windows::core::Result<()> {
        Ok(())
    }
    fn DrawInlineObject(
        &self,
        _: *const c_void,
        _: f32,
        _: f32,
        _: Ref<IDWriteInlineObject>,
        _: BOOL,
        _: BOOL,
        _: Ref<IUnknown>,
    ) -> ::windows::core::Result<()> {
        Ok(())
    }
}
