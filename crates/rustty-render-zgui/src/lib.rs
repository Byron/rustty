//! Retained terminal panels and desktop chrome composed by zgui.

use std::{
    collections::{HashMap, HashSet},
    num::NonZeroU32,
    sync::Arc,
};

use egui::{ClippedPrimitive, Rect, TextureId, TexturesDelta, ViewportId, epaint::Primitive};
use rustty_render::{Color, Frame};
use winit::window::Window;
use zgui_bits::DamageSet;
use zgui_geom::{Device, DevicePx, Point, Scale, Size};
use zgui_render::{ExternalTexture, FrameOutcome, RenderTarget, Renderer as _, TextureHandle};
use zgui_render_wgpu::{PresentPacing, SharedGraphics, WgpuRenderer, wgpu};
use zgui_scene::{ClipLink, ExternalQuad, ExternalTextureId, Scene};

mod ui;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const MAX_TARGET_BYTES: u64 = 512 * 1024 * 1024;
const MAX_LAYERS: usize = 256;

/// A panel's retained contents and its current placement, in physical pixels.
#[derive(Clone, Debug)]
pub struct Panel {
    pub id: u64,
    pub frame: Arc<Frame>,
    pub rect: Rect,
    pub clip: Rect,
    pub opacity: f32,
    pub dim: Option<Color>,
}

/// The terminal callback inserted between the ordinary egui chrome primitives.
#[derive(Clone, Debug)]
pub struct TerminalPaint {
    pub window: u64,
    pub panels: Vec<Panel>,
}

/// Desktop presentation with zgui's persistent target and damage tracking.
pub struct Painter {
    graphics: SharedGraphics,
    windows: HashMap<ViewportId, NativeWindow>,
    textures: ui::Textures,
    screenshots: Vec<egui::Event>,
    pending_captures: HashSet<ViewportId>,
    offscreen: bool,
    description: String,
}

struct NativeWindow {
    window: Arc<Window>,
    compositor: Compositor,
}

impl Painter {
    pub fn new(offscreen: bool) -> Self {
        Self {
            graphics: SharedGraphics::new(),
            windows: HashMap::new(),
            textures: ui::Textures::default(),
            screenshots: Vec::new(),
            pending_captures: HashSet::new(),
            offscreen,
            description: "zgui (initializing)".into(),
        }
    }

    pub fn set_window(&mut self, viewport: ViewportId, window: Arc<Window>) -> Result<()> {
        let size = window.inner_size();
        let target = target([size.width, size.height], window.scale_factor() as f32)?;
        let renderer = if self.offscreen {
            self.graphics.renderer_offscreen(target, FORMAT, false)?
        } else {
            let notify = window.clone();
            self.graphics.renderer_for_surface(
                target,
                window.clone(),
                PresentPacing::Display,
                Some(Box::new(move |_| notify.pre_present_notify())),
            )?
        };
        self.description = format!("zgui — {}", renderer.gpu().describe());
        let mut compositor = Compositor::new(renderer);
        if let Some(previous) = self.windows.remove(&viewport) {
            compositor.paints = previous.compositor.paints;
            compositor.compositions = previous.compositor.compositions;
        }
        self.windows
            .insert(viewport, NativeWindow { window, compositor });
        Ok(())
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn max_texture_side(&self) -> usize {
        self.graphics.gpu().map_or(8192, |gpu| {
            gpu.device().limits().max_texture_dimension_2d as usize
        })
    }

    pub fn remove_window(&mut self, viewport: ViewportId) {
        self.windows.remove(&viewport);
        self.pending_captures.remove(&viewport);
    }

    pub fn on_window_resized(
        &mut self,
        viewport: ViewportId,
        width: NonZeroU32,
        height: NonZeroU32,
    ) {
        if let Some(native) = self.windows.get_mut(&viewport)
            && let Ok(target) = target(
                [width.get(), height.get()],
                native.window.scale_factor() as f32,
            )
        {
            native.compositor.configure(target);
        }
    }

    pub fn terminal_paints(&self, window: u64) -> u64 {
        self.windows
            .values()
            .map(|native| {
                native
                    .compositor
                    .compositions
                    .get(&window)
                    .copied()
                    .unwrap_or(0)
            })
            .sum()
    }

    pub fn panel_paints(&self, window: u64) -> u64 {
        self.windows
            .values()
            .map(|native| native.compositor.paints.get(&window).copied().unwrap_or(0))
            .sum()
    }

    pub fn handle_screenshots(&mut self, events: &mut Vec<egui::Event>) {
        events.append(&mut self.screenshots);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        viewport: ViewportId,
        pixels_per_point: f32,
        primitives: &[ClippedPrimitive],
        textures: &mut TexturesDelta,
        capture: bool,
        window: &Arc<Window>,
    ) -> Result<()> {
        if capture {
            self.pending_captures.insert(viewport);
        }
        if self
            .windows
            .get(&viewport)
            .is_some_and(|native| native.compositor.renderer.gpu().loss().is_lost())
        {
            self.set_window(viewport, window.clone())?;
        }
        self.textures.update(textures)?;
        let result = (|| {
            let native = self
                .windows
                .get_mut(&viewport)
                .ok_or("zgui window is unavailable")?;
            let size = window.inner_size();
            if size.width == 0 || size.height == 0 {
                return Ok(());
            }
            native
                .compositor
                .configure(target([size.width, size.height], pixels_per_point)?);
            let outcome = native
                .compositor
                .paint(primitives, &self.textures, pixels_per_point)?;
            let lost_during_paint = matches!(
                outcome,
                FrameOutcome::Skipped(zgui_render::SkipReason::DeviceUnavailable)
            ) && native.compositor.renderer.gpu().loss().is_lost();
            if outcome.wants_another_frame() || lost_during_paint {
                window.request_redraw();
            }
            if self.pending_captures.contains(&viewport)
                && outcome.retires_damage()
                && !matches!(outcome, FrameOutcome::Recovered)
            {
                self.screenshots.push(egui::Event::Screenshot {
                    viewport_id: viewport,
                    user_data: egui::UserData::default(),
                    image: Arc::new(native.compositor.capture()),
                });
                self.pending_captures.remove(&viewport);
            }
            Ok(())
        })();
        self.textures.free(&textures.free);
        textures.clear();
        result
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Key {
    Panel(u64, u64),
    Chrome(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Placement {
    rect: Rect,
    clip: Rect,
    opacity: f32,
}

impl Placement {
    fn ink(self) -> Rect {
        self.rect.intersect(self.clip)
    }
}

#[derive(Clone)]
enum Content {
    Panel {
        frame: Arc<Frame>,
        dim: Option<Color>,
        phase: [f32; 2],
    },
    Chrome {
        meshes: Vec<ui::Mesh>,
        versions: Vec<(TextureId, u64)>,
        scale: f32,
    },
}

impl Content {
    fn unchanged(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Panel {
                    frame: a,
                    dim: ad,
                    phase: ap,
                },
                Self::Panel {
                    frame: b,
                    dim: bd,
                    phase: bp,
                },
            ) => Arc::ptr_eq(a, b) && ad == bd && ap == bp,
            (
                Self::Chrome {
                    meshes: a,
                    versions: av,
                    scale: ap,
                },
                Self::Chrome {
                    meshes: b,
                    versions: bv,
                    scale: bp,
                },
            ) => a == b && av == bv && ap == bp,
            _ => false,
        }
    }
}

struct Layer {
    key: Key,
    placement: Placement,
    content: Content,
    size: [u32; 2],
}

struct Cached {
    placement: Placement,
    content: Content,
    texture: wgpu::Texture,
    external: ExternalTextureId,
    handle: TextureHandle,
    size: [u32; 2],
}

struct Compositor {
    renderer: WgpuRenderer,
    terminal: rustty_render_wgpu::zgui::Renderer,
    ui: ui::Renderer,
    cached: HashMap<Key, Cached>,
    order: Vec<Key>,
    damage: DamageSet,
    next_external: u64,
    paints: HashMap<u64, u64>,
    compositions: HashMap<u64, u64>,
}

impl Compositor {
    fn new(renderer: WgpuRenderer) -> Self {
        let device = renderer.gpu().device();
        Self {
            terminal: rustty_render_wgpu::zgui::Renderer::new(device, FORMAT),
            ui: ui::Renderer::new(device),
            renderer,
            cached: HashMap::new(),
            order: Vec::new(),
            damage: DamageSet::full(),
            next_external: 1,
            paints: HashMap::new(),
            compositions: HashMap::new(),
        }
    }

    fn configure(&mut self, target: RenderTarget) {
        if self.renderer.target() != Some(target) {
            self.renderer.configure(target);
            self.damage.set_full();
        }
    }

    fn layers(
        &self,
        primitives: &[ClippedPrimitive],
        textures: &ui::Textures,
        scale: f32,
    ) -> Result<Vec<Layer>> {
        let size = self
            .renderer
            .target()
            .ok_or("zgui target is unavailable")?
            .size;
        let extent = Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(size.width as f32, size.height as f32),
        );
        let mut layers = Vec::new();
        let mut meshes = Vec::new();
        let flush = |layers: &mut Vec<Layer>, meshes: &mut Vec<ui::Mesh>| -> Result<()> {
            if !meshes.is_empty() {
                let versions = textures.versions(meshes)?;
                layers.push(Layer {
                    key: Key::Chrome(layers.len()),
                    placement: Placement {
                        rect: extent,
                        clip: extent,
                        opacity: 1.0,
                    },
                    content: Content::Chrome {
                        meshes: std::mem::take(meshes),
                        versions,
                        scale,
                    },
                    size: [size.width as u32, size.height as u32],
                });
            }
            Ok(())
        };
        for primitive in primitives {
            match &primitive.primitive {
                Primitive::Mesh(mesh) => {
                    if !mesh.indices.is_empty() {
                        meshes.push(ui::Mesh {
                            clip: primitive.clip_rect,
                            mesh: mesh.clone(),
                        });
                    }
                }
                Primitive::Callback(callback) => {
                    flush(&mut layers, &mut meshes)?;
                    let paint = callback
                        .callback
                        .downcast_ref::<TerminalPaint>()
                        .ok_or("unsupported zgui paint callback")?;
                    let callback_clip =
                        (primitive.clip_rect * scale).intersect(callback.rect * scale);
                    for panel in &paint.panels {
                        let origin = panel.rect.min.floor();
                        let phase = [panel.rect.left() - origin.x, panel.rect.top() - origin.y];
                        let size = [
                            panel.frame.size[0]
                                .checked_add(u32::from(phase[0] != 0.0))
                                .ok_or("zgui panel size overflow")?,
                            panel.frame.size[1]
                                .checked_add(u32::from(phase[1] != 0.0))
                                .ok_or("zgui panel size overflow")?,
                        ];
                        let placement = Placement {
                            rect: Rect::from_min_size(
                                origin,
                                egui::vec2(size[0] as f32, size[1] as f32),
                            ),
                            clip: pixel_clip(
                                panel
                                    .rect
                                    .intersect(panel.clip)
                                    .intersect(callback_clip)
                                    .intersect(extent),
                            ),
                            opacity: panel.opacity,
                        };
                        if placement.ink().is_positive() && !panel.frame.size.contains(&0) {
                            if !panel.rect.is_finite()
                                || !panel.clip.is_finite()
                                || !panel.opacity.is_finite()
                            {
                                return Err("invalid zgui panel placement".into());
                            }
                            layers.push(Layer {
                                key: Key::Panel(paint.window, panel.id),
                                placement,
                                content: Content::Panel {
                                    frame: panel.frame.clone(),
                                    dim: panel.dim,
                                    phase,
                                },
                                size,
                            });
                        }
                    }
                }
            }
        }
        flush(&mut layers, &mut meshes)?;
        if layers.len() > MAX_LAYERS {
            return Err("too many zgui retained layers".into());
        }
        let mut bytes = 0u64;
        let limit = self
            .renderer
            .gpu()
            .device()
            .limits()
            .max_texture_dimension_2d;
        for layer in &layers {
            if layer.size.contains(&0) || layer.size.iter().any(|side| *side > limit) {
                return Err("zgui panel exceeds the GPU texture size limit".into());
            }
            bytes = bytes
                .checked_add(u64::from(layer.size[0]) * u64::from(layer.size[1]) * 4)
                .ok_or("zgui target storage overflow")?;
        }
        if bytes > MAX_TARGET_BYTES {
            return Err("zgui retained targets exceed the 512 MiB budget".into());
        }
        Ok(layers)
    }

    fn paint(
        &mut self,
        primitives: &[ClippedPrimitive],
        textures: &ui::Textures,
        scale: f32,
    ) -> Result<FrameOutcome> {
        let layers = self.layers(primitives, textures, scale)?;
        let order = layers.iter().map(|layer| layer.key).collect::<Vec<_>>();
        if self.order != order {
            // Reordering translucent layers can change every overlap without changing their content.
            for cached in self.cached.values() {
                damage(&mut self.damage, cached.placement.ink());
            }
            for layer in &layers {
                damage(&mut self.damage, layer.placement.ink());
            }
        }
        let removed = self
            .cached
            .keys()
            .filter(|key| !order.contains(key))
            .copied()
            .collect::<Vec<_>>();
        for key in removed {
            if let Some(cached) = self.cached.remove(&key) {
                damage(&mut self.damage, cached.placement.ink());
                self.renderer.release_external(cached.handle);
            }
        }
        let gpu = self.renderer.gpu().clone();
        for layer in layers {
            let mut cached = self.cached.remove(&layer.key);
            if let Some(previous) = &cached
                && (previous.placement != layer.placement
                    || !previous.content.unchanged(&layer.content))
            {
                damage(&mut self.damage, previous.placement.ink());
                damage(&mut self.damage, layer.placement.ink());
            }
            if cached
                .as_ref()
                .is_some_and(|cached| cached.size != layer.size)
            {
                let previous = cached.take().unwrap();
                self.renderer.release_external(previous.handle);
            }
            let repaint = cached
                .as_ref()
                .is_none_or(|cached| !cached.content.unchanged(&layer.content));
            if cached.is_none() {
                damage(&mut self.damage, layer.placement.ink());
                let texture = texture(gpu.device(), layer.size);
                let external = ExternalTextureId(self.next_external);
                self.next_external += 1;
                let handle = self.renderer.register_external(ExternalTexture {
                    id: external,
                    handle: TextureHandle(0),
                    size: Size::new(layer.size[0] as i32, layer.size[1] as i32),
                    premultiplied: true,
                });
                if !self.renderer.attach_external(external, &texture) {
                    self.renderer.release_external(handle);
                    return Err("zgui rejected a retained panel texture".into());
                }
                cached = Some(Cached {
                    placement: layer.placement,
                    content: layer.content.clone(),
                    texture,
                    external,
                    handle,
                    size: layer.size,
                });
            }
            let mut cached = cached.unwrap();
            if repaint {
                let result = match &layer.content {
                    Content::Panel { frame, dim, phase } => {
                        self.paint_terminal(&cached.texture, frame, *dim, *phase)
                    }
                    Content::Chrome { meshes, .. } => {
                        self.ui
                            .paint(&gpu, &cached.texture, meshes, textures, scale)
                    }
                };
                if let Err(error) = result {
                    // A failed paint may have partially overwritten the target; never reuse its old key.
                    self.renderer.release_external(cached.handle);
                    self.damage.set_full();
                    return Err(error);
                }
                if let Key::Panel(window, _) = layer.key {
                    *self.paints.entry(window).or_default() += 1;
                }
            }
            cached.placement = layer.placement;
            cached.content = layer.content;
            self.cached.insert(layer.key, cached);
        }
        self.order = order;
        self.ui.collect(textures);
        self.damage.absorb_set(&DamageSet::for_frame());
        let mut scene = Scene::new();
        scene.begin_frame(self.renderer.target().unwrap().size);
        for key in &self.order {
            let cached = &self.cached[key];
            let clip = scene
                .clips
                .only(ClipLink::rect(rect(cached.placement.clip)));
            let mut quad =
                ExternalQuad::new(rect(cached.placement.rect), cached.external).clipped(clip);
            quad.opacity = cached.placement.opacity.clamp(0.0, 1.0);
            scene.push_external(quad);
        }
        scene.finish(&self.damage);
        let outcome = self.renderer.draw(&scene, &self.damage);
        if outcome.retires_damage()
            && !matches!(
                outcome,
                FrameOutcome::Recovered | FrameOutcome::Skipped(zgui_render::SkipReason::Undamaged)
            )
        {
            let windows = self
                .order
                .iter()
                .filter_map(|key| match key {
                    Key::Panel(window, _) => Some(*window),
                    Key::Chrome(_) => None,
                })
                .collect::<HashSet<_>>();
            for window in windows {
                *self.compositions.entry(window).or_default() += 1;
            }
        }
        if outcome.retires_damage() {
            self.damage.clear();
        }
        if matches!(outcome, FrameOutcome::Recovered) {
            self.cached.clear();
            self.order.clear();
            self.damage.set_full();
            self.terminal =
                rustty_render_wgpu::zgui::Renderer::new(self.renderer.gpu().device(), FORMAT);
            self.ui = ui::Renderer::new(self.renderer.gpu().device());
        }
        Ok(outcome)
    }

    fn paint_terminal(
        &mut self,
        texture: &wgpu::Texture,
        frame: &Arc<Frame>,
        dim: Option<Color>,
        phase: [f32; 2],
    ) -> Result<()> {
        let mut shaded;
        let frame = if dim.is_some() || phase != [0.0; 2] {
            shaded = (**frame).clone();
            if let Some(color) = dim {
                shaded.quads.push(rustty_render::Quad::solid(
                    [0.0, 0.0, frame.size[0] as f32, frame.size[1] as f32],
                    color,
                ));
            }
            // Rasterize at the final pixel phase, then composite at an integer origin to keep glyph masks sharp.
            for quad in &mut shaded.quads {
                quad.rect[0] += phase[0];
                quad.rect[1] += phase[1];
            }
            shaded.size = [texture.width(), texture.height()];
            &shaded
        } else {
            frame.as_ref()
        };
        let gpu = self.renderer.gpu();
        self.terminal.prepare(gpu.device(), gpu.queue(), frame)?;
        let view = texture.create_view(&Default::default());
        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rustty zgui panel"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rustty zgui panel"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.terminal.paint(&mut pass);
        }
        gpu.queue().submit([encoder.finish()]);
        Ok(())
    }

    fn capture(&self) -> egui::ColorImage {
        let image = self.renderer.read_composed();
        let mut rgba = image.bytes().to_vec();
        if image.is_bgra() {
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
        }
        egui::ColorImage::from_rgba_premultiplied(
            [image.size().width as usize, image.size().height as usize],
            &rgba,
        )
    }
}

fn target(size: [u32; 2], scale: f32) -> Result<RenderTarget> {
    if !scale.is_finite() || scale <= 0.0 || size.iter().any(|side| *side > 16_384) {
        return Err("invalid zgui render target".into());
    }
    Ok(RenderTarget::new(
        Size::new(size[0].max(1) as i32, size[1].max(1) as i32),
        Scale::new(scale),
    )
    .translucent())
}

fn texture(device: &wgpu::Device, size: [u32; 2]) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rustty retained zgui layer"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn rect(bounds: Rect) -> zgui_geom::Rect<DevicePx, Device> {
    zgui_geom::Rect::new(
        Point::new(DevicePx(bounds.left()), DevicePx(bounds.top())),
        Size::new(DevicePx(bounds.width()), DevicePx(bounds.height())),
    )
}

fn pixel_clip(bounds: Rect) -> Rect {
    // Hard clips test pixel centers; integer edges avoid interpolation error at half-pixel ties.
    Rect::from_min_max(
        egui::pos2((bounds.left() - 0.5).ceil(), (bounds.top() - 0.5).ceil()),
        egui::pos2(
            (bounds.right() - 0.5).ceil(),
            (bounds.bottom() - 0.5).ceil(),
        ),
    )
}

fn damage(damage: &mut DamageSet, bounds: Rect) {
    if bounds.is_positive() && bounds.is_finite() {
        let left = bounds.left().floor() as i32;
        let top = bounds.top().floor() as i32;
        damage.absorb(zgui_geom::Rect::new(
            Point::new(left, top),
            Size::new(
                bounds.right().ceil() as i32 - left,
                bounds.bottom().ceil() as i32 - top,
            ),
        ));
    }
}

#[cfg(test)]
mod tests;
