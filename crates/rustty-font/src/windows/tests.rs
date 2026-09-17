use super::*;
use crate::{CodepointMap, FontFeature};

fn bundled_config() -> FontConfig {
    FontConfig {
        families: vec!["JetBrains Mono".into()],
        ..Default::default()
    }
}

fn coverage(fonts: &mut FontSystem, text: &str, style: FontStyle) -> u64 {
    fonts
        .shape(text, style)
        .unwrap()
        .iter()
        .map(|g| {
            fonts
                .rasterize(g)
                .unwrap()
                .pixels
                .iter()
                .map(|b| u64::from(*b))
                .sum::<u64>()
        })
        .sum()
}

#[test]
fn point_sizes_reach_native_metrics_shaping_carets_and_rasterization() {
    for (size_points, scale_factor, pixels) in [
        (12.0, 1.0, 16.0),
        (12.0, 1.25, 20.0),
        (12.0, 1.5, 24.0),
        (12.0, 2.0, 32.0),
        // Explicit overrides, including the old 13-pixel size in points.
        (9.75, 1.0, 13.0),
        (18.0, 1.25, 30.0),
    ] {
        let mut fonts = FontSystem::new(FontConfig {
            families: vec!["Consolas".into()],
            size_points,
            scale_factor,
            ..Default::default()
        })
        .unwrap();
        let (glyphs, carets) = fonts
            .shape_with_carets("MM", FontStyle::Regular, &[0, 1, 2])
            .unwrap();
        assert_eq!(glyphs.len(), 2);
        let face = &fonts.faces[glyphs[0].font.0];
        // Rasterization uses the em size captured from DirectWrite's glyph run.
        assert_eq!(face.pixels, pixels, "{size_points} pt at {scale_factor}x");
        assert_eq!(fonts.metrics(), font_metrics(&face.native, pixels).unwrap());
        assert_eq!(fonts.metrics().cell_width, glyphs[0].advance.ceil() as u32);
        assert_eq!(carets[0], 0.0);
        assert!((carets[1] - glyphs[0].advance).abs() < 0.001);
        assert!((carets[2] - glyphs.iter().map(|g| g.advance).sum::<f32>()).abs() < 0.001);
        let bitmap = fonts.rasterize(&glyphs[0]).unwrap();
        assert_eq!(bitmap.format, BitmapFormat::Alpha);
        assert!(bitmap.pixels.iter().any(|alpha| *alpha != 0));
    }
}

#[test]
fn native_defaults_fall_back_to_available_families_without_warnings() {
    let fonts = FontSystem::new(FontConfig::default()).unwrap();
    assert_eq!(fonts.config.size_points, 12.0);
    assert!(fonts.missing_families().is_empty());
    let selected = default_family(&fonts.collection).unwrap();
    assert!(fonts.styles.iter().all(|style| style.family == selected));

    // Build real DirectWrite collections with successively more defaults. This
    // exercises missing optional fonts without changing the machine's fonts.
    let builder = unsafe { fonts.factory.CreateFontSetBuilder() }.unwrap();
    for name in ["JetBrains Mono", "Consolas", "Cascadia Mono"] {
        let Some(family) = find_family(&fonts.collection, name).unwrap() else {
            continue;
        };
        let family: IDWriteFontFamily1 = family.cast().unwrap();
        let collection: IDWriteFontCollection = unsafe {
            builder
                .AddFontFaceReference2(&family.GetFontFaceReference(0).unwrap())
                .unwrap();
            fonts
                .factory
                .CreateFontCollectionFromFontSet(
                    &builder.CreateFontSet().unwrap(),
                    DWRITE_FONT_FAMILY_MODEL_WEIGHT_STRETCH_STYLE,
                )
                .unwrap()
        }
        .cast()
        .unwrap();
        let selected = default_family(&collection).unwrap();
        assert_eq!(selected, name);
        let mut warnings = Vec::new();
        let style = make_style(
            &fonts.factory,
            &collection,
            vec![selected.into()],
            0,
            &FontStyleRequest::Default,
            &[],
            &mut warnings,
        )
        .unwrap();
        assert_eq!(style.family, name);
        assert!(warnings.is_empty());
    }
}

#[test]
fn explicit_family_order_and_style_overrides_precede_native_defaults() {
    let absent = "Rustty deliberately absent family";
    let mut fonts = FontSystem::new(FontConfig {
        families: vec![absent.into(), "JetBrains Mono".into(), "Consolas".into()],
        bold_families: vec!["Consolas".into()],
        italic_families: vec![absent.into()],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(fonts.missing_families(), [absent]);
    for (style, family) in [
        (FontStyle::Regular, "JetBrains Mono"),
        (FontStyle::Bold, "Consolas"),
        (FontStyle::Italic, "JetBrains Mono"),
        (FontStyle::BoldItalic, "JetBrains Mono"),
    ] {
        assert_eq!(fonts.styles[style as usize].family, family);
        let glyph = fonts.shape("M", style).unwrap().remove(0);
        assert!(
            fonts
                .font_name(glyph.font)
                .unwrap()
                .starts_with(&family.replace(' ', ""))
        );
        assert!(
            fonts
                .rasterize(&glyph)
                .unwrap()
                .pixels
                .iter()
                .any(|p| *p != 0)
        );
    }
}

#[test]
fn bundled_fonts_and_native_fallback_shape_and_rasterize() {
    let mut fonts = FontSystem::new(bundled_config()).unwrap();
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
        let bitmap = fonts.rasterize(&glyphs[0]).unwrap();
        assert_eq!(bitmap.format, BitmapFormat::Alpha);
        assert!(bitmap.pixels.iter().any(|b| *b != 0));
    }
    let text = "a👩🏽‍💻水e\u{301}";
    let glyphs = fonts.shape(text, FontStyle::Regular).unwrap();
    assert!(glyphs.iter().all(|g| text.is_char_boundary(g.cluster)));
    assert!(glyphs.iter().all(|g| g.glyph != 0), "{glyphs:?}");
    assert!(
        glyphs
            .iter()
            .any(|g| fonts.rasterize(g).unwrap().format == BitmapFormat::Rgba)
    );
}

#[test]
fn color_emoji_is_premultiplied_rgba_and_styles_change_coverage() {
    let mut fonts = FontSystem::new(FontConfig::default()).unwrap();
    let regular = coverage(&mut fonts, "M", FontStyle::Regular);
    assert!(coverage(&mut fonts, "M", FontStyle::Bold) > regular);
    let glyph = fonts.shape("🙂", FontStyle::Regular).unwrap().remove(0);
    let bitmap = fonts.rasterize(&glyph).unwrap();
    let ppem = fonts.faces[glyph.font.0].pixels.ceil() as u32;
    assert_eq!(
        bitmap.format,
        BitmapFormat::Rgba,
        "font={:?}, glyph={glyph:?}, formats={:?}, all={:?}",
        fonts.font_name(glyph.font),
        unsafe {
            fonts.faces[glyph.font.0]
                .native
                .cast::<IDWriteFontFace4>()
                .unwrap()
                .GetGlyphImageFormats(glyph.glyph, ppem, ppem)
        },
        unsafe {
            fonts.faces[glyph.font.0]
                .native
                .cast::<IDWriteFontFace4>()
                .unwrap()
                .GetGlyphImageFormats2()
        }
    );
    assert!(
        bitmap
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[3] > 0 && (p[0] != p[1] || p[1] != p[2]))
    );
    assert!(
        bitmap
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[..3].iter().all(|c| *c <= p[3]))
    );
}

#[test]
fn named_styles_variations_and_disabled_styles_are_applied() {
    let mut fonts = FontSystem::new(FontConfig {
        variations: vec![FontVariation {
            tag: *b"wght",
            value: 100.0,
        }],
        bold_variations: vec![FontVariation {
            tag: *b"wght",
            value: 800.0,
        }],
        ..bundled_config()
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
fn codepoint_mapping_preserves_graphemes_and_uses_last_mapping() {
    let mut fonts = FontSystem::new(FontConfig {
        codepoint_map: vec![
            CodepointMap {
                start: 'A' as u32,
                end: 'z' as u32,
                family: "Consolas".into(),
            },
            CodepointMap {
                start: 'B' as u32,
                end: 'B' as u32,
                family: "Arial".into(),
            },
        ],
        ..Default::default()
    })
    .unwrap();
    let text = "A🙂e\u{301}B";
    let glyphs = fonts.shape(text, FontStyle::Bold).unwrap();
    for (byte, expected) in [(0, "Consolas"), (5, "Consolas"), (8, "Arial")] {
        let glyph = glyphs.iter().find(|g| g.cluster == byte).unwrap();
        assert!(fonts.font_name(glyph.font).unwrap().starts_with(expected));
        assert_eq!(
            unsafe { fonts.faces[glyph.font.0].native.GetSimulations() },
            DWRITE_FONT_SIMULATIONS_NONE
        );
    }
    assert!(glyphs.iter().all(|g| text.is_char_boundary(g.cluster)));
    assert!(fonts.has_codepoint_override('A'));
    assert!(!fonts.has_codepoint_override('🙂'));
}

#[test]
fn synthetic_style_switch_and_zero_thickening_preserve_visible_glyphs() {
    let mut fonts = FontSystem::new(FontConfig {
        families: vec!["Symbols Nerd Font".into()],
        ..Default::default()
    })
    .unwrap();
    let regular = coverage(&mut fonts, "\u{e0a0}", FontStyle::Regular);
    assert!(regular > 0);
    assert!(coverage(&mut fonts, "\u{e0a0}", FontStyle::Bold) > regular);
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
    let mut thick = FontSystem::new(FontConfig {
        thicken: true,
        ..Default::default()
    })
    .unwrap();
    assert!(
        coverage(&mut thick, "M", FontStyle::Regular)
            > coverage(&mut thicken, "M", FontStyle::Regular)
    );
}

#[test]
fn missing_fonts_and_invalid_sizes_have_stable_fallbacks() {
    for size in [0.0, -1.0, f32::NAN, f32::INFINITY, 2048.0] {
        assert!(
            FontSystem::new(FontConfig {
                size_points: size,
                ..Default::default()
            })
            .is_err()
        );
    }
    let mut fonts = FontSystem::new(FontConfig {
        families: vec!["Rustty deliberately absent family".into()],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        fonts.missing_families(),
        ["Rustty deliberately absent family"]
    );
    let glyph = fonts.shape("M", FontStyle::Regular).unwrap().remove(0);
    let expected = default_family(&fonts.collection).unwrap().replace(' ', "");
    assert!(fonts.font_name(glyph.font).unwrap().starts_with(&expected));
    let scaled = FontSystem::new(FontConfig {
        scale_factor: 2.0,
        ..Default::default()
    })
    .unwrap();
    assert!(scaled.metrics().cell_width >= fonts.metrics().cell_width * 2 - 1);
    assert!(scaled.metrics().cell_height >= fonts.metrics().cell_height * 2 - 1);
}

#[test]
fn terminal_display_order_and_utf8_carets_survive_native_shaping() {
    assert_eq!(utf16_to_utf8("a🙂é"), [0, 1, 1, 5, 7]);
    let mut fonts = FontSystem::new(FontConfig::default()).unwrap();
    let text = "a🙂e\u{301}אב ";
    let (glyphs, carets) = fonts
        .shape_with_carets(text, FontStyle::Regular, &[0, 1, 2, 5, text.len()])
        .unwrap();
    assert_eq!(carets[1], carets[2]);
    assert!(carets.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(
        glyphs
            .windows(2)
            .all(|pair| pair[0].cluster <= pair[1].cluster),
        "{glyphs:?}"
    );
    assert!(carets.last().unwrap() > &0.0);
}

#[test]
fn opentype_features_reach_native_shaping() {
    let mut fonts = FontSystem::new(bundled_config()).unwrap();
    let default = fonts.shape("->", FontStyle::Regular).unwrap();
    let mut disabled = FontSystem::new(FontConfig {
        features: vec![
            FontFeature {
                tag: *b"calt",
                value: 0,
            },
            FontFeature {
                tag: *b"liga",
                value: 0,
            },
        ],
        ..bundled_config()
    })
    .unwrap();
    let without = disabled.shape("->", FontStyle::Regular).unwrap();
    assert_ne!(
        default.iter().map(|g| g.glyph).collect::<Vec<_>>(),
        without.iter().map(|g| g.glyph).collect::<Vec<_>>(),
        "fonts={:?}, glyphs={default:?}",
        default
            .iter()
            .map(|g| fonts.font_name(g.font))
            .collect::<Vec<_>>()
    );
}

#[test]
fn postscript_names_select_native_faces_without_warnings() {
    let mut fonts = FontSystem::new(FontConfig {
        families: vec!["Consolas-Bold".into()],
        ..Default::default()
    })
    .unwrap();
    assert!(
        fonts.missing_families().is_empty(),
        "{:?}",
        fonts.missing_families()
    );
    let glyph = fonts.shape("M", FontStyle::Regular).unwrap().remove(0);
    assert_eq!(
        fonts.font_name(glyph.font).as_deref(),
        Some("Consolas-Bold")
    );
}

#[test]
fn malformed_font_configuration_is_rejected_before_native_calls() {
    assert!(
        FontSystem::new(FontConfig {
            features: vec![FontFeature {
                tag: [255; 4],
                value: 1
            }],
            ..Default::default()
        })
        .is_err()
    );
    assert!(
        FontSystem::new(FontConfig {
            variations: vec![FontVariation {
                tag: *b"wght",
                value: f64::NAN
            }],
            ..Default::default()
        })
        .is_err()
    );
    assert!(
        FontSystem::new(FontConfig {
            codepoint_map: vec![CodepointMap {
                start: 5,
                end: 3,
                family: "Consolas".into()
            }],
            ..Default::default()
        })
        .is_err()
    );
}

#[test]
fn native_caret_positions_include_ligature_interiors() {
    for family in ["Calibri", "Cambria", "Segoe UI", "Gabriola"] {
        let mut fonts = FontSystem::new(FontConfig {
            families: vec![family.into()],
            ..Default::default()
        })
        .unwrap();
        let (glyphs, carets) = fonts
            .shape_with_carets("fi", FontStyle::Regular, &[0, 1, 2, usize::MAX])
            .unwrap();
        if glyphs.len() != 1 {
            continue;
        }
        assert!(
            carets[0] < carets[1] && carets[1] < carets[2],
            "{family}: {carets:?}"
        );
        assert_eq!(carets[2], carets[3]);
        return;
    }
    panic!("no Windows test font formed the fi ligature");
}
