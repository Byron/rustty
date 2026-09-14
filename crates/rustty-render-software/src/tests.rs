use super::*;
use egui::{
    Color32, ColorImage, Mesh, PaintCallback, Pos2, Rect, TextureOptions,
    epaint::{ImageDelta, Primitive, Vertex},
};
use rustty_render::{AtlasUpload, Color, Frame, Paint, Quad};
use std::sync::Arc;

fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + width, y + height))
}
fn clipped(mesh: Mesh, clip_rect: Rect) -> ClippedPrimitive {
    ClippedPrimitive {
        clip_rect,
        primitive: Primitive::Mesh(mesh),
    }
}
fn quad(bounds: Rect, uv: Rect, color: Color32, texture_id: TextureId) -> Mesh {
    let mut mesh = Mesh::with_texture(texture_id);
    mesh.add_rect_with_uv(bounds, uv, color);
    mesh
}
fn render(
    renderer: &mut Renderer,
    size: [u32; 2],
    scale: f32,
    primitives: &[ClippedPrimitive],
    delta: &mut TexturesDelta,
) -> Vec<u8> {
    let result = renderer
        .render(size, scale, primitives, delta)
        .unwrap()
        .to_vec();
    delta.clear();
    result
}
fn texture(
    delta: &mut TexturesDelta,
    id: TextureId,
    size: [usize; 2],
    pixels: Vec<Color32>,
    options: TextureOptions,
) {
    delta.push(id, ImageDelta::full(ColorImage::new(size, pixels), options));
}
fn pixel(pixels: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
    pixels[(y * width + x) * 4..(y * width + x + 1) * 4]
        .try_into()
        .unwrap()
}
fn callback(frame: Frame, window: u64, viewport: Rect, clip_rect: Rect) -> ClippedPrimitive {
    ClippedPrimitive {
        clip_rect,
        primitive: Primitive::Callback(PaintCallback {
            rect: viewport,
            callback: Arc::new(TerminalPaint {
                window,
                frame: Arc::new(frame),
            }),
        }),
    }
}

#[test]
fn triangles_share_one_coverage_edge_with_both_windings_and_fractional_dpi() {
    let mut renderer = Renderer::default();
    let mut delta = TexturesDelta::default();
    texture(
        &mut delta,
        TextureId::Managed(0),
        [1, 1],
        vec![Color32::WHITE],
        TextureOptions::NEAREST,
    );
    let color = Color32::from_rgba_premultiplied(128, 0, 0, 128);
    // Separate primitives prevent the rectangle optimization from hiding an
    // edge coverage defect in the general triangle rasterizer.
    let vertices = vec![
        Vertex {
            pos: Pos2::new(0.0, 0.0),
            uv: Pos2::ZERO,
            color,
        },
        Vertex {
            pos: Pos2::new(4.0, 0.0),
            uv: Pos2::ZERO,
            color,
        },
        Vertex {
            pos: Pos2::new(4.0, 4.0),
            uv: Pos2::ZERO,
            color,
        },
        Vertex {
            pos: Pos2::new(0.0, 4.0),
            uv: Pos2::ZERO,
            color,
        },
    ];
    let primitives = [
        clipped(
            Mesh {
                indices: vec![0, 1, 2],
                vertices: vertices.clone(),
                texture_id: TextureId::Managed(0),
            },
            Rect::EVERYTHING,
        ),
        clipped(
            Mesh {
                indices: vec![3, 2, 0],
                vertices,
                texture_id: TextureId::Managed(0),
            },
            Rect::EVERYTHING,
        ),
    ];
    let pixels = render(&mut renderer, [6, 6], 1.5, &primitives, &mut delta);
    assert!(
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [128, 0, 0, 128])
    );
}

#[test]
fn clipping_uses_physical_pixel_rounding_and_clears_between_frames() {
    let mut renderer = Renderer::default();
    let mut delta = TexturesDelta::default();
    texture(
        &mut delta,
        TextureId::Managed(0),
        [1, 1],
        vec![Color32::WHITE],
        TextureOptions::NEAREST,
    );
    let primitives = [clipped(
        quad(
            rect(0.0, 0.0, 4.0, 4.0),
            Rect::ZERO,
            Color32::GREEN,
            TextureId::Managed(0),
        ),
        rect(0.6, 0.6, 1.1, 1.1),
    )];
    let pixels = render(&mut renderer, [8, 8], 2.0, &primitives, &mut delta);
    for y in 0..8 {
        for x in 0..8 {
            assert_eq!(
                pixel(&pixels, 8, x, y),
                if (1..3).contains(&x) && (1..3).contains(&y) {
                    [0, 255, 0, 255]
                } else {
                    [0; 4]
                }
            );
        }
    }
    let empty = render(&mut renderer, [3, 2], 1.0, &[], &mut delta);
    assert_eq!(empty, vec![0; 24]);
}

#[test]
fn partial_texture_updates_filtering_and_free_happen_in_order() {
    let mut renderer = Renderer::default();
    let mut delta = TexturesDelta::default();
    let id = TextureId::User(42);
    texture(
        &mut delta,
        id,
        [2, 1],
        vec![Color32::RED, Color32::BLUE],
        TextureOptions::NEAREST,
    );
    delta.push(
        id,
        ImageDelta::partial(
            [1, 0],
            ColorImage::filled([1, 1], Color32::GREEN),
            TextureOptions::LINEAR,
        ),
    );
    let primitive = clipped(
        quad(
            rect(0.0, 0.0, 4.0, 1.0),
            rect(0.0, 0.0, 1.0, 1.0),
            Color32::WHITE,
            id,
        ),
        Rect::EVERYTHING,
    );
    delta.free(id);
    let pixels = render(
        &mut renderer,
        [4, 1],
        1.0,
        std::slice::from_ref(&primitive),
        &mut delta,
    );
    assert_eq!(
        pixels,
        vec![
            255, 0, 0, 255, 191, 64, 0, 255, 64, 191, 0, 255, 0, 255, 0, 255
        ]
    );
    assert!(renderer.render([4, 1], 1.0, &[primitive], &delta).is_err());
    assert_eq!(renderer.resource_bytes, 0);
}

#[test]
fn gamma_premultiplication_tint_and_paint_order_are_preserved() {
    let mut renderer = Renderer::default();
    let mut delta = TexturesDelta::default();
    texture(
        &mut delta,
        TextureId::Managed(0),
        [1, 1],
        vec![Color32::WHITE],
        TextureOptions::NEAREST,
    );
    texture(
        &mut delta,
        TextureId::User(1),
        [1, 1],
        vec![Color32::from_rgba_premultiplied(128, 0, 0, 128)],
        TextureOptions::NEAREST,
    );
    let bg = clipped(
        quad(
            rect(0.0, 0.0, 1.0, 1.0),
            Rect::ZERO,
            Color32::BLUE,
            TextureId::Managed(0),
        ),
        Rect::EVERYTHING,
    );
    let fg = clipped(
        quad(
            rect(0.0, 0.0, 1.0, 1.0),
            Rect::ZERO,
            Color32::from_rgba_premultiplied(128, 128, 128, 128),
            TextureId::User(1),
        ),
        Rect::EVERYTHING,
    );
    let pixels = render(&mut renderer, [1, 1], 1.0, &[bg, fg], &mut delta);
    assert_eq!(pixels, [64, 0, 191, 255]);
}

#[test]
fn wrap_modes_and_distinct_minification_filters_are_honored() {
    let mut renderer = Renderer::default();
    let mut delta = TexturesDelta::default();
    let id = TextureId::User(2);
    let mut options = TextureOptions::NEAREST_REPEAT;
    options.minification = egui::TextureFilter::Linear;
    texture(
        &mut delta,
        id,
        [2, 1],
        vec![Color32::RED, Color32::BLUE],
        options,
    );
    let primitive = clipped(
        quad(
            rect(0.0, 0.0, 4.0, 1.0),
            rect(-1.0, 0.0, 2.0, 1.0),
            Color32::WHITE,
            id,
        ),
        Rect::EVERYTHING,
    );
    assert_eq!(
        render(&mut renderer, [4, 1], 1.0, &[primitive], &mut delta),
        [
            255, 0, 0, 255, 0, 0, 255, 255, 255, 0, 0, 255, 0, 0, 255, 255
        ]
    );
    options.wrap_mode = egui::TextureWrapMode::MirroredRepeat;
    texture(
        &mut delta,
        id,
        [2, 1],
        vec![Color32::RED, Color32::BLUE],
        options,
    );
    let primitive = clipped(
        quad(
            rect(0.0, 0.0, 4.0, 1.0),
            rect(-1.0, 0.0, 2.0, 1.0),
            Color32::WHITE,
            id,
        ),
        Rect::EVERYTHING,
    );
    assert_eq!(
        render(&mut renderer, [4, 1], 1.0, &[primitive], &mut delta),
        [
            0, 0, 255, 255, 255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 255, 255
        ]
    );
    let primitive = clipped(
        quad(
            rect(0.0, 0.0, 1.0, 1.0),
            rect(0.0, 0.0, 1.0, 1.0),
            Color32::WHITE,
            id,
        ),
        Rect::EVERYTHING,
    );
    assert_eq!(
        render(&mut renderer, [1, 1], 1.0, &[primitive], &mut delta),
        [128, 0, 128, 255]
    );
}

fn upload(revision: u64, page_size: u32, pixels: Vec<u8>) -> AtlasUpload {
    AtlasUpload {
        revision,
        page: 0,
        page_size,
        origin: [0, 0],
        size: [page_size, page_size],
        pixels: pixels.into(),
    }
}
fn terminal_quad(paint: Paint, rect: [f32; 4], uv: [f32; 4], color: Color) -> Quad {
    Quad {
        paint,
        rect,
        uv,
        color,
        atlas: 0,
    }
}

#[test]
fn terminal_masks_colors_srgb_filtering_and_callback_viewport_match_wgsl() {
    let mut renderer = Renderer::default();
    let mut delta = TexturesDelta::default();
    let mut frame = Frame::empty([3, 1]);
    frame.generation = 1;
    frame.atlas_uploads.push(upload(
        1,
        2,
        vec![
            255, 0, 0, 128, 0, 0, 255, 128, 255, 0, 0, 128, 0, 0, 255, 128,
        ],
    ));
    frame.quads.push(terminal_quad(
        Paint::Mask,
        [0.0, 0.0, 1.0, 1.0],
        [0.25, 0.25, 0.25, 0.25],
        Color::rgb([0, 255, 0]),
    ));
    frame.quads.push(terminal_quad(
        Paint::Color,
        [1.0, 0.0, 1.0, 1.0],
        [0.25, 0.25, 0.25, 0.25],
        Color::rgb([0, 255, 0]).opacity(0.5),
    ));
    frame.quads.push(terminal_quad(
        Paint::Color,
        [2.0, 0.0, 1.0, 1.0],
        [0.5, 0.25, 0.5, 0.25],
        Color::rgb([0; 3]),
    ));
    let primitive = callback(frame, 7, rect(1.0, 0.0, 3.0, 1.0), Rect::EVERYTHING);
    let pixels = render(&mut renderer, [5, 1], 1.0, &[primitive], &mut delta);
    assert_eq!(pixel(&pixels, 5, 0, 0), [0; 4]);
    assert_eq!(pixel(&pixels, 5, 1, 0), [0, 128, 0, 128]);
    assert_eq!(pixel(&pixels, 5, 2, 0), [64, 0, 0, 64]);
    // A 50/50 red-blue mix is encoded from linear sRGB (~188), not gamma (128).
    assert_eq!(pixel(&pixels, 5, 3, 0), [94, 0, 94, 128]);
    assert_eq!(pixel(&pixels, 5, 4, 0), [0; 4]);
}

#[test]
fn atlas_revision_generation_and_window_lifetime_are_independent() {
    let mut renderer = Renderer::default();
    let mut delta = TexturesDelta::default();
    let mut frame = Frame::empty([1, 1]);
    frame.generation = 10;
    frame.atlas_uploads.push(upload(5, 1, vec![255, 0, 0, 255]));
    frame.quads.push(terminal_quad(
        Paint::Color,
        [0.0, 0.0, 1.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
        Color::rgb([255; 3]),
    ));
    let draw = |renderer: &mut Renderer, frame: Frame, window: u64, delta: &mut TexturesDelta| {
        render(
            renderer,
            [1, 1],
            1.0,
            &[callback(
                frame,
                window,
                rect(0.0, 0.0, 1.0, 1.0),
                Rect::EVERYTHING,
            )],
            delta,
        )
    };
    assert_eq!(
        draw(&mut renderer, frame.clone(), 1, &mut delta),
        [255, 0, 0, 255]
    );
    frame.atlas_uploads[0] = upload(4, 1, vec![0, 255, 0, 255]);
    assert_eq!(
        draw(&mut renderer, frame.clone(), 1, &mut delta),
        [255, 0, 0, 255]
    );
    assert_eq!(
        draw(&mut renderer, frame.clone(), 2, &mut delta),
        [0, 255, 0, 255]
    );
    frame.generation = 11;
    frame.atlas_uploads[0] = upload(0, 1, vec![0, 0, 255, 255]);
    assert_eq!(
        draw(&mut renderer, frame.clone(), 1, &mut delta),
        [0, 0, 255, 255]
    );
    frame.atlas_uploads.clear();
    assert_eq!(
        draw(&mut renderer, frame.clone(), 1, &mut delta),
        [0, 0, 255, 255]
    );
    renderer.free_window(1);
    assert_eq!(renderer.resource_bytes, 4);
    assert!(
        renderer
            .render(
                [1, 1],
                1.0,
                &[callback(
                    frame,
                    1,
                    rect(0.0, 0.0, 1.0, 1.0),
                    Rect::EVERYTHING
                )],
                &delta
            )
            .is_err()
    );
    renderer.free_window(2);
    assert_eq!(renderer.resource_bytes, 0);
}

#[test]
fn ui_meshes_before_and_after_terminal_callbacks_keep_paint_order() {
    let mut renderer = Renderer::default();
    let mut delta = TexturesDelta::default();
    texture(
        &mut delta,
        TextureId::Managed(0),
        [1, 1],
        vec![Color32::WHITE],
        TextureOptions::LINEAR,
    );
    let mut frame = Frame::empty([1, 1]);
    frame.quads.push(Quad::solid(
        [0.0, 0.0, 1.0, 1.0],
        Color::rgb([0, 255, 0]).opacity(0.5),
    ));
    let primitives = [
        clipped(
            quad(
                rect(0.0, 0.0, 1.0, 1.0),
                Rect::ZERO,
                Color32::BLUE,
                TextureId::Managed(0),
            ),
            Rect::EVERYTHING,
        ),
        callback(frame, 1, rect(0.0, 0.0, 1.0, 1.0), Rect::EVERYTHING),
        clipped(
            quad(
                rect(0.0, 0.0, 1.0, 1.0),
                Rect::ZERO,
                Color32::from_rgba_premultiplied(128, 0, 0, 128),
                TextureId::Managed(0),
            ),
            Rect::EVERYTHING,
        ),
    ];
    assert_eq!(
        render(&mut renderer, [1, 1], 1.0, &primitives, &mut delta),
        [128, 64, 64, 255]
    );
}

#[test]
fn malformed_geometry_textures_and_excess_resources_return_errors() {
    let mut renderer = Renderer::default();
    let mut delta = TexturesDelta::default();
    assert!(renderer.render([u32::MAX, 1], 1.0, &[], &delta).is_err());
    assert!(renderer.render([1, 1], f32::NAN, &[], &delta).is_err());
    texture(
        &mut delta,
        TextureId::Managed(0),
        [1, 1],
        vec![Color32::WHITE],
        TextureOptions::LINEAR,
    );
    render(&mut renderer, [1, 1], 1.0, &[], &mut delta);
    let mut mesh = quad(
        rect(0.0, 0.0, 1.0, 1.0),
        Rect::ZERO,
        Color32::WHITE,
        TextureId::Managed(0),
    );
    mesh.indices.push(u32::MAX);
    assert!(
        renderer
            .render([1, 1], 1.0, &[clipped(mesh, Rect::EVERYTHING)], &delta)
            .is_err()
    );
    delta.push(
        TextureId::Managed(0),
        ImageDelta::partial(
            [1, 0],
            ColorImage::filled([1, 1], Color32::WHITE),
            TextureOptions::LINEAR,
        ),
    );
    assert!(renderer.render([1, 1], 1.0, &[], &delta).is_err());
    delta.clear();
    let mut frame = Frame::empty([1, 1]);
    frame.atlas_uploads.push(upload(1, 1, vec![255; 3]));
    assert!(
        renderer
            .render(
                [1, 1],
                1.0,
                &[callback(
                    frame,
                    1,
                    rect(0.0, 0.0, 1.0, 1.0),
                    Rect::EVERYTHING
                )],
                &delta
            )
            .is_err()
    );
    let mut frame = Frame::empty([1, 1]);
    frame.quads.push(Quad::solid(
        [f32::INFINITY, 0.0, 1.0, 1.0],
        Color::rgb([255; 3]),
    ));
    assert!(
        renderer
            .render(
                [1, 1],
                1.0,
                &[callback(
                    frame,
                    1,
                    rect(0.0, 0.0, 1.0, 1.0),
                    Rect::EVERYTHING
                )],
                &delta
            )
            .is_err()
    );
    let mut canvas = raster::Canvas {
        pixels: &mut [0; 4],
        size: [1, 1],
        work: 0,
        geometry: 10,
    };
    assert!(canvas.charge(raster::Clip([0, 0, 1, 1])).is_err());
}

#[test]
fn triangle_interpolates_vertex_tint_and_uv_without_perspective() {
    let mut renderer = Renderer::default();
    let mut delta = TexturesDelta::default();
    let id = TextureId::User(9);
    texture(
        &mut delta,
        id,
        [2, 1],
        vec![Color32::WHITE, Color32::BLACK],
        TextureOptions::LINEAR,
    );
    let mesh = Mesh {
        texture_id: id,
        indices: vec![0, 1, 2],
        vertices: vec![
            Vertex {
                pos: Pos2::new(0.0, 0.0),
                uv: Pos2::new(0.25, 0.5),
                color: Color32::RED,
            },
            Vertex {
                pos: Pos2::new(2.0, 0.0),
                uv: Pos2::new(0.75, 0.5),
                color: Color32::GREEN,
            },
            Vertex {
                pos: Pos2::new(0.0, 2.0),
                uv: Pos2::new(0.75, 0.5),
                color: Color32::BLUE,
            },
        ],
    };
    // At the first pixel's center the weights are 1/2, 1/4, 1/4 and the
    // interpolated UV is halfway from the white texel to the black texel.
    let pixels = render(
        &mut renderer,
        [1, 1],
        1.0,
        &[clipped(mesh, Rect::EVERYTHING)],
        &mut delta,
    );
    assert_eq!(pixels, [64, 32, 32, 255]);
}

#[test]
fn terminal_partial_atlas_uploads_replay_and_clip_a_scaled_viewport() {
    let mut renderer = Renderer::default();
    let mut delta = TexturesDelta::default();
    let mut frame = Frame::empty([2, 2]);
    frame.generation = 3;
    frame.atlas_uploads.push(upload(1, 2, vec![0; 16]));
    frame.atlas_uploads.push(AtlasUpload {
        revision: 2,
        page: 0,
        page_size: 2,
        origin: [1, 0],
        size: [1, 1],
        pixels: Arc::from([255, 255, 255, 255]),
    });
    frame.quads.push(terminal_quad(
        Paint::Mask,
        [0.0, 0.0, 2.0, 2.0],
        [0.75, 0.25, 0.75, 0.25],
        Color::rgb([17, 89, 203]),
    ));
    let primitive = callback(frame, 1, rect(1.0, 1.0, 4.0, 4.0), rect(2.0, 2.0, 2.0, 2.0));
    for _ in 0..2 {
        let pixels = render(
            &mut renderer,
            [6, 6],
            1.0,
            std::slice::from_ref(&primitive),
            &mut delta,
        );
        for y in 0..6 {
            for x in 0..6 {
                assert_eq!(
                    pixel(&pixels, 6, x, y),
                    if (2..4).contains(&x) && (2..4).contains(&y) {
                        [17, 89, 203, 255]
                    } else {
                        [0; 4]
                    }
                );
            }
        }
    }
}
