//! Present a small opaque terminal patch independently of the window drawable.

use std::{ptr, sync::Arc};

use objc2::{MainThreadMarker, Message, rc::Retained};
use objc2_app_kit::NSView;
use objc2_foundation::{NSPoint, NSRect, NSSize};
use objc2_quartz_core::{CALayer, CAMetalLayer, CATransaction};
use rustty_render::Frame;
use winit::{
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::Window,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub struct NativeCursorLayer {
    // The surface must release its native resources before our layer handle.
    surface: wgpu::Surface<'static>,
    layer: Retained<CAMetalLayer>,
    window: Arc<Window>,
    state: egui_wgpu::RenderState,
    renderer: rustty_render_wgpu::Renderer,
    config: wgpu::SurfaceConfiguration,
    configured_size: [u32; 2],
    _mtm: MainThreadMarker,
}

impl NativeCursorLayer {
    pub fn new(window: Arc<Window>, state: &egui_wgpu::RenderState) -> Result<Self> {
        let mtm = MainThreadMarker::new().ok_or("cursor layer requires the main thread")?;
        let parent = view(&window)?
            .layer()
            .ok_or("window has no backing layer")?;
        let layer = CAMetalLayer::new();
        let _transaction = Transaction::new();
        layer.setHidden(true);
        layer.setPresentsWithTransaction(true);
        layer.setContentsScale(window.scale_factor());
        // SAFETY: the retained CAMetalLayer outlives the surface created from it.
        let surface = unsafe {
            state
                .instance
                .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(
                    Retained::as_ptr(&layer).cast_mut().cast(),
                ))?
        };
        let capabilities = surface.get_capabilities(&state.adapter);
        if !capabilities.formats.contains(&state.target_format)
            || !capabilities
                .alpha_modes
                .contains(&wgpu::CompositeAlphaMode::Opaque)
        {
            return Err("cursor layer does not support the window's opaque format".into());
        }
        let config = wgpu::SurfaceConfiguration {
            format: state.target_format,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            present_mode: state.surface_config.present_mode,
            ..surface
                .get_default_config(&state.adapter, 1, 1)
                .ok_or("cursor layer is incompatible with the window's GPU")?
        };
        let renderer = rustty_render_wgpu::Renderer::new(&state.device, state.target_format);
        parent.addSublayer(&layer);
        Ok(Self {
            surface,
            layer,
            window,
            state: state.clone(),
            renderer,
            config,
            configured_size: [0, 0],
            _mtm: mtm,
        })
    }

    /// The frame is already cropped to the rectangle; visibility is controlled separately.
    pub fn present(&mut self, rect: [u32; 4], frame: &Frame) -> Result<()> {
        let [x, y, width, height] = rect;
        let size = self.window.inner_size();
        let limit = self.state.device.limits().max_texture_dimension_2d;
        if width == 0
            || height == 0
            || width > limit
            || height > limit
            || x.checked_add(width).is_none_or(|right| right > size.width)
            || y.checked_add(height)
                .is_none_or(|bottom| bottom > size.height)
            || frame.size != [width, height]
        {
            return Err("cursor patch is outside the window or has an invalid size".into());
        }
        let scale = self.window.scale_factor();
        let view = view(&self.window)?;
        let parent = view.layer().ok_or("window has no backing layer")?;
        let main = main_layer(&self.window, &self.layer)?;
        let native_rect = view.convertRectToLayer(NSRect::new(
            NSPoint::new(f64::from(x) / scale, f64::from(y) / scale),
            NSSize::new(f64::from(width) / scale, f64::from(height) / scale),
        ));
        let _transaction = Transaction::new();
        if self.configured_size != [width, height] {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.state.device, &self.config);
            self.configured_size = [width, height];
        }
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(output) => output,
            wgpu::CurrentSurfaceTexture::Suboptimal(output) => {
                self.configured_size = [0, 0];
                output
            }
            status => {
                self.configured_size = [0, 0];
                return Err(
                    format!("cursor layer could not acquire a drawable: {status:?}").into(),
                );
            }
        };
        self.renderer
            .prepare(&self.state.device, &self.state.queue, frame)?;
        let target = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder =
            self.state
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("rustty cursor patch"),
                });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rustty cursor patch"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_viewport(0.0, 0.0, width as f32, height as f32, 0.0, 1.0);
            pass.set_scissor_rect(0, 0, width, height);
            self.renderer.paint(&mut pass);
        }
        self.state.queue.submit([encoder.finish()]);
        self.layer.setFrame(native_rect);
        self.layer.setContentsScale(scale);
        // Surface recreation can append the replacement above the retained patch.
        // SAFETY: the layer tree is read and changed only on the main thread.
        let topmost = unsafe { parent.sublayers() }
            .and_then(|layers| layers.lastObject())
            .is_some_and(|layer| ptr::eq::<CALayer>(&*layer, &**self.layer));
        if !topmost {
            if ptr::eq::<CALayer>(&*parent, &**main) {
                parent.addSublayer(&self.layer);
            } else {
                parent.insertSublayer_above(&self.layer, Some(&main));
            }
        }
        self.window.pre_present_notify();
        self.state.queue.present(output);
        Ok(())
    }

    pub fn set_visible(&self, visible: bool) {
        if self.is_visible() != visible {
            let _transaction = Transaction::new();
            self.layer.setHidden(!visible);
        }
    }

    pub fn is_visible(&self) -> bool {
        !self.layer.isHidden()
    }

    pub fn begin_full_frame(&self) -> Result<FullFrameTransaction> {
        let main_layer = main_layer(&self.window, &self.layer)?;
        let previous = main_layer.presentsWithTransaction();
        let transaction = Transaction::new();
        main_layer.setPresentsWithTransaction(true);
        Ok(FullFrameTransaction {
            transaction: Some(transaction),
            main_layer,
            previous,
            cursor_layer: self.layer.clone(),
            window: Arc::clone(&self.window),
            _mtm: self._mtm,
        })
    }
}

impl Drop for NativeCursorLayer {
    fn drop(&mut self) {
        let _transaction = Transaction::new();
        self.layer.removeFromSuperlayer();
    }
}

pub struct FullFrameTransaction {
    transaction: Option<Transaction>,
    main_layer: Retained<CAMetalLayer>,
    previous: bool,
    cursor_layer: Retained<CAMetalLayer>,
    window: Arc<Window>,
    _mtm: MainThreadMarker,
}

impl FullFrameTransaction {
    /// A recreated main surface must first present again with transaction synchronization.
    pub fn finish(mut self, frame_painted: bool) -> bool {
        let retired = frame_painted
            && main_layer(&self.window, &self.cursor_layer)
                .is_ok_and(|layer| ptr::eq(&*layer, &*self.main_layer));
        if retired {
            self.cursor_layer.setHidden(true);
        }
        self.close();
        retired
    }

    fn close(&mut self) {
        if self.transaction.is_some() {
            drop(self.transaction.take());
            self.main_layer.setPresentsWithTransaction(self.previous);
        }
    }
}

impl Drop for FullFrameTransaction {
    fn drop(&mut self) {
        self.close();
    }
}

fn view(window: &Window) -> Result<&NSView> {
    MainThreadMarker::new().ok_or("cursor layer requires the main thread")?;
    let handle = window.window_handle()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err("cursor layer requires an AppKit window".into());
    };
    // SAFETY: winit lends its live view while Window is borrowed, on the main thread.
    Ok(unsafe { handle.ns_view.cast::<NSView>().as_ref() })
}

fn main_layer(window: &Window, cursor: &CAMetalLayer) -> Result<Retained<CAMetalLayer>> {
    let root = view(window)?.layer().ok_or("window has no backing layer")?;
    if let Some(layer) = root.downcast_ref::<CAMetalLayer>() {
        return Ok(layer.retain());
    }
    // SAFETY: only read the layer tree on the main thread; retain matches before returning.
    unsafe { root.sublayers() }
        .into_iter()
        .flat_map(|layers| layers.into_iter())
        .filter_map(|layer| layer.downcast::<CAMetalLayer>().ok())
        .filter(|layer| !ptr::eq(&**layer, cursor))
        // Surface recreation appends the replacement after the old layer.
        .last()
        .ok_or_else(|| "window has no Metal presentation layer".into())
}

struct Transaction;

impl Transaction {
    fn new() -> Self {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        Self
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        CATransaction::commit();
    }
}
