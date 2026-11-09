use super::*;

fn panel(id: u64, x: f32, color: [u8; 3]) -> Panel {
    let mut frame = Frame::empty([32, 32]);
    frame.quads.push(rustty_render::Quad::solid(
        [0.0, 0.0, 32.0, 32.0],
        Color::rgb(color),
    ));
    let rect = Rect::from_min_size(egui::pos2(x, 0.0), egui::vec2(32.0, 32.0));
    Panel {
        id,
        frame: Arc::new(frame),
        rect,
        clip: rect,
        opacity: 1.0,
        dim: None,
    }
}

fn callback(panels: Vec<Panel>) -> ClippedPrimitive {
    let rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(96.0, 48.0));
    ClippedPrimitive {
        clip_rect: rect,
        primitive: Primitive::Callback(egui::PaintCallback {
            rect,
            callback: Arc::new(TerminalPaint { window: 1, panels }),
        }),
    }
}

fn compositor() -> Compositor {
    let renderer = SharedGraphics::new()
        .renderer_offscreen(target([96, 48], 1.0).unwrap(), FORMAT, false)
        .expect("test needs a graphics device");
    Compositor::new(renderer)
}

fn color(image: &egui::ColorImage, x: usize, y: usize) -> [u8; 4] {
    image.pixels[y * image.size[0] + x].to_array()
}

#[test]
fn panel_changes_damage_the_old_and_new_pixel_bounds() {
    let mut changes = DamageSet::new();
    damage(
        &mut changes,
        Rect::from_min_max(egui::pos2(1.2, 2.7), egui::pos2(12.1, 19.3)),
    );
    assert_eq!(
        changes.rects(),
        &[zgui_geom::Rect::new(Point::new(1, 2), Size::new(12, 18))]
    );
    damage(&mut changes, Rect::NOTHING);
    assert_eq!(changes.len(), 1);
}

#[test]
fn a_frame_identity_and_dimming_both_invalidate_panel_pixels() {
    let pane = panel(1, 0.0, [255, 0, 0]);
    let original = Content::Panel {
        frame: pane.frame.clone(),
        dim: None,
        phase: [0.0; 2],
    };
    assert!(original.unchanged(&original.clone()));
    assert!(!original.unchanged(&Content::Panel {
        frame: Arc::new((*pane.frame).clone()),
        dim: None,
        phase: [0.0; 2],
    }));
    assert!(!original.unchanged(&Content::Panel {
        frame: pane.frame,
        dim: Some(Color::rgb([0, 0, 0]).opacity(0.5)),
        phase: [0.0; 2],
    }));
}

#[test]
fn invalid_targets_are_refused_before_gpu_allocation() {
    for scale in [f32::NAN, 0.0, -1.0, f32::INFINITY] {
        assert!(target([96, 48], scale).is_err());
    }
    assert!(target([u32::MAX, 48], 1.0).is_err());
}

#[test]
fn ui_texture_patches_preserve_order_and_reject_out_of_bounds() {
    let id = TextureId::Managed(0);
    let mut textures = ui::Textures::default();
    let mut delta = TexturesDelta::default();
    delta
        .set
        .entry(id)
        .or_default()
        .push(egui::epaint::ImageDelta::full(
            egui::ColorImage::filled([2, 2], egui::Color32::RED),
            egui::TextureOptions::LINEAR,
        ));
    delta
        .set
        .entry(id)
        .or_default()
        .push(egui::epaint::ImageDelta::partial(
            [1, 1],
            egui::ColorImage::filled([1, 1], egui::Color32::BLUE),
            egui::TextureOptions::NEAREST,
        ));
    textures.update(&delta).unwrap();
    delta.clear();
    delta
        .set
        .entry(id)
        .or_default()
        .push(egui::epaint::ImageDelta::partial(
            [2, 1],
            egui::ColorImage::filled([1, 1], egui::Color32::GREEN),
            egui::TextureOptions::LINEAR,
        ));
    assert!(textures.update(&delta).is_err());
    delta.clear();
}

#[test]
#[ignore = "requires a graphics device"]
fn changing_one_panel_retains_the_other_and_matches_a_full_redraw() {
    let mut compositor = compositor();
    let textures = ui::Textures::default();
    let left = panel(1, 0.0, [255, 0, 0]);
    let right = panel(2, 64.0, [0, 0, 255]);
    compositor
        .paint(&[callback(vec![left, right.clone()])], &textures, 1.0)
        .unwrap();
    assert_eq!(compositor.paints[&1], 2);
    assert_eq!(compositor.compositions[&1], 1);
    let right_texture = compositor.cached[&Key::Panel(1, 2)].external;
    let primitives = [callback(vec![panel(1, 0.0, [0, 255, 0]), right])];
    let outcome = compositor.paint(&primitives, &textures, 1.0).unwrap();
    assert_eq!(compositor.paints[&1], 3);
    assert_eq!(compositor.compositions[&1], 2);
    assert_eq!(compositor.cached[&Key::Panel(1, 2)].external, right_texture);
    assert_eq!(outcome.stats().unwrap().damage_px, 32 * 32);
    let incremental = compositor.capture();
    assert_eq!(color(&incremental, 16, 16), [0, 255, 0, 255]);
    assert_eq!(color(&incremental, 80, 16), [0, 0, 255, 255]);
    compositor.damage.set_full();
    compositor.paint(&primitives, &textures, 1.0).unwrap();
    assert_eq!(compositor.capture(), incremental);
    let idle = compositor.paint(&primitives, &textures, 1.0).unwrap();
    assert!(matches!(
        idle,
        FrameOutcome::Skipped(zgui_render::SkipReason::Undamaged)
    ));
    assert_eq!(compositor.paints[&1], 3);
    assert_eq!(compositor.compositions[&1], 3);
}

#[test]
#[ignore = "requires a graphics device"]
fn fractional_panel_origins_preserve_sharp_glyph_sampling() {
    let mut compositor = compositor();
    let mut reference = self::compositor();
    let textures = ui::Textures::default();
    let mut frame = Frame::empty([8, 8]);
    frame.generation = 1;
    frame.quads.push(rustty_render::Quad::solid(
        [0.0, 0.0, 8.0, 8.0],
        Color::rgb([255, 255, 255]),
    ));
    frame.atlas_uploads.push(rustty_render::AtlasUpload {
        revision: 1,
        page: 0,
        page_size: 8,
        origin: [0, 0],
        size: [8, 8],
        pixels: (0..64)
            .flat_map(|pixel| [255, 255, 255, if pixel % 2 == 0 { 255 } else { 0 }])
            .collect::<Vec<_>>()
            .into(),
    });
    frame.quads.push(rustty_render::Quad {
        rect: [1.0, 1.0, 6.0, 6.0],
        uv: [0.125, 0.125, 0.875, 0.875],
        color: Color::rgb([0, 0, 0]),
        paint: rustty_render::Paint::Mask,
        atlas: 0,
    });
    let frame = Arc::new(frame);
    for phase in [0.25, 0.5, 0.75] {
        let bounds =
            Rect::from_min_size(egui::pos2(8.0 + phase, 6.0 + phase), egui::vec2(8.0, 8.0));
        let pane = Panel {
            id: 1,
            frame: frame.clone(),
            rect: bounds,
            clip: bounds,
            opacity: 1.0,
            dim: None,
        };
        let mut composed = Frame::empty([96, 48]);
        composed
            .append_clipped(
                &frame,
                [bounds.left(), bounds.top()],
                [bounds.left(), bounds.top(), bounds.width(), bounds.height()],
            )
            .unwrap();
        let full_bounds = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(96.0, 48.0));
        reference
            .paint(
                &[callback(vec![Panel {
                    id: 0,
                    frame: Arc::new(composed),
                    rect: full_bounds,
                    clip: full_bounds,
                    opacity: 1.0,
                    dim: None,
                }])],
                &textures,
                1.0,
            )
            .unwrap();
        compositor
            .paint(&[callback(vec![pane.clone()])], &textures, 1.0)
            .unwrap();
        let actual = compositor.capture();
        let expected = reference.capture();
        let width = actual.size[0];
        let differences = actual
            .pixels
            .iter()
            .zip(&expected.pixels)
            .enumerate()
            .filter(|(_, (actual, expected))| actual != expected)
            .map(|(index, (actual, expected))| {
                (
                    index % width,
                    index / width,
                    actual.to_array(),
                    expected.to_array(),
                )
            })
            .take(12)
            .collect::<Vec<_>>();
        assert_eq!(
            actual, expected,
            "fractional phase {phase}: {differences:?}"
        );
        let paints = compositor.paints[&1];
        let moved = Panel {
            rect: pane.rect.translate(egui::vec2(16.0, 0.0)),
            clip: pane.clip.translate(egui::vec2(16.0, 0.0)),
            ..pane
        };
        compositor
            .paint(&[callback(vec![moved])], &textures, 1.0)
            .unwrap();
        assert_eq!(
            compositor.paints[&1], paints,
            "integer translation keeps the raster phase"
        );
    }
    assert_eq!(
        compositor.paints[&1], 3,
        "each distinct phase needs one rasterization"
    );
}

#[test]
#[ignore = "requires a graphics device"]
fn moving_clipping_dimming_and_removing_panels_clears_stale_pixels() {
    let mut compositor = compositor();
    let textures = ui::Textures::default();
    let mut pane = panel(1, 0.0, [255, 0, 0]);
    compositor
        .paint(&[callback(vec![pane.clone()])], &textures, 1.0)
        .unwrap();
    pane.rect = pane.rect.translate(egui::vec2(16.0, 0.0));
    pane.clip = Rect::from_min_size(egui::pos2(16.0, 0.0), egui::vec2(16.0, 32.0));
    compositor
        .paint(&[callback(vec![pane.clone()])], &textures, 1.0)
        .unwrap();
    assert_eq!(
        compositor.paints[&1], 1,
        "placement alone reuses panel pixels"
    );
    let moved = compositor.capture();
    assert_eq!(color(&moved, 8, 16), [0, 0, 0, 0]);
    assert_eq!(color(&moved, 24, 16), [255, 0, 0, 255]);
    assert_eq!(color(&moved, 40, 16), [0, 0, 0, 0]);
    pane.dim = Some(Color::rgb([0, 0, 0]).opacity(0.5));
    compositor
        .paint(&[callback(vec![pane])], &textures, 1.0)
        .unwrap();
    let dimmed = color(&compositor.capture(), 24, 16);
    assert!((127..=128).contains(&dimmed[0]), "{dimmed:?}");
    compositor
        .paint(&[callback(Vec::new())], &textures, 1.0)
        .unwrap();
    assert!(
        compositor
            .capture()
            .pixels
            .iter()
            .all(|pixel| *pixel == egui::Color32::TRANSPARENT)
    );
    assert!(compositor.cached.is_empty());
}

fn mesh(bounds: Rect, tint: egui::Color32) -> ClippedPrimitive {
    let mut mesh = egui::Mesh::default();
    mesh.add_colored_rect(bounds, tint);
    ClippedPrimitive {
        clip_rect: bounds,
        primitive: Primitive::Mesh(mesh),
    }
}

#[test]
#[ignore = "requires a graphics device"]
fn chrome_order_texture_updates_and_resize_are_retained_correctly() {
    let mut compositor = compositor();
    let mut textures = ui::Textures::default();
    let mut delta = TexturesDelta::default();
    delta
        .set
        .entry(TextureId::Managed(0))
        .or_default()
        .push(egui::epaint::ImageDelta::full(
            egui::ColorImage::filled([1, 1], egui::Color32::WHITE),
            egui::TextureOptions::NEAREST,
        ));
    textures.update(&delta).unwrap();
    delta.clear();
    let extent = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(96.0, 48.0));
    let above = Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(8.0, 8.0));
    let primitives = [
        mesh(extent, egui::Color32::BLUE),
        callback(vec![panel(1, 0.0, [255, 0, 0])]),
        mesh(above, egui::Color32::GREEN),
    ];
    compositor.paint(&primitives, &textures, 1.0).unwrap();
    let image = compositor.capture();
    assert_eq!(color(&image, 4, 4), [255, 0, 0, 255]);
    assert_eq!(color(&image, 12, 12), [0, 255, 0, 255]);
    assert_eq!(color(&image, 80, 16), [0, 0, 255, 255]);
    assert!(matches!(
        compositor.paint(&primitives, &textures, 1.0).unwrap(),
        FrameOutcome::Skipped(zgui_render::SkipReason::Undamaged)
    ));
    delta.clear();
    delta
        .set
        .entry(TextureId::Managed(0))
        .or_default()
        .push(egui::epaint::ImageDelta::full(
            egui::ColorImage::filled([1, 1], egui::Color32::BLACK),
            egui::TextureOptions::NEAREST,
        ));
    textures.update(&delta).unwrap();
    delta.clear();
    compositor.paint(&primitives, &textures, 1.0).unwrap();
    assert_eq!(
        compositor.paints[&1], 1,
        "texture updates redraw chrome only"
    );
    assert_eq!(color(&compositor.capture(), 80, 16), [0, 0, 0, 255]);
    compositor.configure(target([128, 64], 2.0).unwrap());
    compositor.paint(&primitives, &textures, 2.0).unwrap();
    assert_eq!(compositor.capture().size, [128, 64]);
}
