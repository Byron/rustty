//! Select presentation once at startup; terminal layout and UI stay shared.
use egui::{Context, ViewportId};
use rustty::config::Renderer as Preference;
use rustty_render::Frame;
use std::{collections::HashMap, num::NonZeroU32, sync::Arc};
use winit::window::Window;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub struct Painter {
    backend: Backend,
    #[cfg(target_os = "windows")]
    auto_fallback: bool,
    description: String,
}

enum Backend {
    Gpu(Box<egui_wgpu::winit::Painter>),
    #[cfg(target_os = "windows")]
    Software(Box<Software>),
}

#[cfg(target_os = "windows")]
#[derive(Default)]
struct Software {
    renderer: rustty_render_software::Renderer,
    surfaces: HashMap<ViewportId, rustty_app::software_surface::NativeSurface>,
    screenshots: Vec<egui::Event>,
}

impl Painter {
    pub async fn new(
        context: Context,
        config: egui_wgpu::WgpuConfiguration,
        preference: Preference,
    ) -> Result<Self> {
        #[cfg(target_os = "windows")]
        let mut config = config;
        #[cfg(target_os = "windows")]
        {
            if preference == Preference::Software {
                return Ok(Self::software("selected explicitly"));
            }
            if preference == Preference::Auto {
                // DXGI descriptions do not create devices. WGPU enumeration can
                // initialize WARP even when we would immediately discard it.
                match hardware_available() {
                    Ok(false) => return Ok(Self::software("no hardware graphics adapter")),
                    Err(error) => return Ok(Self::software(&format!("DXGI discovery: {error}"))),
                    Ok(true) => {}
                }
                if let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
                    setup.native_adapter_selector = Some(Arc::new(|adapters, surface| {
                        adapters
                            .iter()
                            .filter(|adapter| {
                                let info = adapter.get_info();
                                info.device_type != wgpu::DeviceType::Cpu
                                    && !software_adapter(info.vendor, &info.name)
                                    && surface
                                        .is_none_or(|surface| adapter.is_surface_supported(surface))
                            })
                            .min_by_key(|adapter| match adapter.get_info().device_type {
                                wgpu::DeviceType::DiscreteGpu => 0,
                                wgpu::DeviceType::IntegratedGpu => 1,
                                _ => 2,
                            })
                            .cloned()
                            .ok_or_else(|| "no compatible hardware graphics adapter".into())
                    }));
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        if preference == Preference::Software {
            return Err("software presentation is currently available on Windows".into());
        }
        let backend =
            egui_wgpu::winit::Painter::new(context, config, true, Default::default()).await;
        Ok(Self {
            backend: Backend::Gpu(Box::new(backend)),
            #[cfg(target_os = "windows")]
            auto_fallback: preference == Preference::Auto,
            description: "gpu (initializing)".into(),
        })
    }

    #[cfg(target_os = "windows")]
    fn software(reason: &str) -> Self {
        let description = format!("software — {reason}");
        eprintln!("Rustty renderer: {description}");
        Self {
            backend: Backend::Software(Box::default()),
            auto_fallback: false,
            description,
        }
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn is_software(&self) -> bool {
        match self.backend {
            Backend::Gpu(_) => false,
            #[cfg(target_os = "windows")]
            Backend::Software(_) => true,
        }
    }

    pub async fn set_window(&mut self, viewport: ViewportId, window: Arc<Window>) -> Result<()> {
        match &mut self.backend {
            Backend::Gpu(gpu) => {
                let first = gpu.render_state().is_none();
                match gpu.set_window(viewport, Some(window.clone())).await {
                    Ok(()) => {
                        if first && let Some(state) = gpu.render_state() {
                            let info = state.adapter.get_info();
                            self.description = format!(
                                "gpu — {} ({:?}, {:?})",
                                info.name, info.backend, info.device_type
                            );
                            eprintln!("Rustty renderer: {}", self.description());
                        }
                        Ok(())
                    }
                    Err(error) => {
                        #[cfg(target_os = "windows")]
                        if first && self.auto_fallback {
                            // No UI textures or pane frames have been uploaded yet.
                            // Drop the failed surface before installing CPU presentation.
                            *self = Self::software(&format!("GPU initialization failed: {error}"));
                            return self.set_software_window(viewport, window);
                        }
                        Err(error.into())
                    }
                }
            }
            #[cfg(target_os = "windows")]
            Backend::Software(_) => self.set_software_window(viewport, window),
        }
    }

    #[cfg(target_os = "windows")]
    fn set_software_window(&mut self, viewport: ViewportId, window: Arc<Window>) -> Result<()> {
        let Backend::Software(software) = &mut self.backend else {
            unreachable!("software backend selected before adding a window");
        };
        software.surfaces.insert(
            viewport,
            rustty_app::software_surface::NativeSurface::new(window)?,
        );
        Ok(())
    }

    pub fn render_state(&self) -> Option<egui_wgpu::RenderState> {
        match &self.backend {
            Backend::Gpu(gpu) => gpu.render_state(),
            #[cfg(target_os = "windows")]
            Backend::Software(_) => None,
        }
    }

    pub fn max_texture_side(&self) -> usize {
        self.render_state().map_or(8192, |state| {
            state.device.limits().max_texture_dimension_2d as usize
        })
    }

    pub fn remove_window(
        &mut self,
        _viewport: ViewportId,
        window: u64,
        remaining: &egui::ViewportIdSet,
    ) {
        match &mut self.backend {
            Backend::Gpu(gpu) => {
                gpu.gc_viewports(remaining);
                if let Some(state) = gpu.render_state()
                    && let Some(renderers) = state
                        .renderer
                        .write()
                        .callback_resources
                        .get_mut::<GpuRenderers>()
                {
                    renderers.0.remove(&window);
                }
            }
            #[cfg(target_os = "windows")]
            Backend::Software(software) => {
                software.surfaces.remove(&_viewport);
                software.renderer.free_window(window);
            }
        }
    }

    pub fn on_window_resized(
        &mut self,
        viewport: ViewportId,
        width: NonZeroU32,
        height: NonZeroU32,
    ) {
        match &mut self.backend {
            Backend::Gpu(gpu) => gpu.on_window_resized(viewport, width, height),
            #[cfg(target_os = "windows")]
            Backend::Software(_) => {}
        }
    }

    pub fn terminal_callback(
        &self,
        rect: egui::Rect,
        window: u64,
        frame: Arc<Frame>,
    ) -> egui::Shape {
        match &self.backend {
            Backend::Gpu(gpu) => egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(
                rect,
                TerminalPaint {
                    window,
                    frame,
                    format: gpu
                        .render_state()
                        .expect("window initialized before drawing")
                        .target_format,
                },
            )),
            #[cfg(target_os = "windows")]
            Backend::Software(_) => egui::Shape::Callback(egui::PaintCallback {
                rect,
                callback: Arc::new(rustty_render_software::TerminalPaint { window, frame }),
            }),
        }
    }

    pub fn paint(
        &mut self,
        viewport: ViewportId,
        pixels_per_point: f32,
        primitives: &[egui::ClippedPrimitive],
        textures: &mut egui::TexturesDelta,
        capture: bool,
        window: &Arc<Window>,
    ) -> Result<()> {
        match &mut self.backend {
            Backend::Gpu(gpu) => {
                gpu.paint_and_update_textures(
                    viewport,
                    pixels_per_point,
                    [0.0; 4],
                    primitives,
                    textures,
                    if capture {
                        vec![egui::UserData::default()]
                    } else {
                        Vec::new()
                    },
                    window,
                );
            }
            #[cfg(target_os = "windows")]
            Backend::Software(software) => {
                let size = window.inner_size();
                let result = software.renderer.render(
                    [size.width, size.height],
                    pixels_per_point,
                    primitives,
                    textures,
                );
                textures.clear();
                let pixels = result?;
                software
                    .surfaces
                    .get_mut(&viewport)
                    .ok_or("software window is unavailable")?
                    .present([size.width, size.height], pixels)?;
                if capture {
                    software.screenshots.push(egui::Event::Screenshot {
                        viewport_id: viewport,
                        user_data: egui::UserData::default(),
                        image: Arc::new(egui::ColorImage::from_rgba_premultiplied(
                            [size.width as usize, size.height as usize],
                            pixels,
                        )),
                    });
                }
            }
        }
        Ok(())
    }

    pub fn handle_screenshots(&mut self, events: &mut Vec<egui::Event>) {
        match &mut self.backend {
            Backend::Gpu(gpu) => gpu.handle_screenshots(events),
            #[cfg(target_os = "windows")]
            Backend::Software(software) => events.append(&mut software.screenshots),
        }
    }
}

#[cfg(target_os = "windows")]
fn software_adapter(vendor: u32, name: &str) -> bool {
    vendor == 0x1414 && name.contains("Microsoft Basic Render Driver")
}

#[cfg(target_os = "windows")]
fn hardware_available() -> windows::core::Result<bool> {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND, IDXGIFactory1,
    };
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    for index in 0.. {
        let adapter = match unsafe { factory.EnumAdapters1(index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => return Ok(false),
            Err(error) => return Err(error),
        };
        let desc = unsafe { adapter.GetDesc1()? };
        let name = String::from_utf16_lossy(
            &desc.Description[..desc
                .Description
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(desc.Description.len())],
        );
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0
            && !software_adapter(desc.VendorId, &name)
        {
            return Ok(true);
        }
    }
    unreachable!()
}

struct GpuRenderer {
    renderer: rustty_render_wgpu::Renderer,
    frame: Option<Arc<Frame>>,
    prepares: u64,
}
struct GpuRenderers(HashMap<u64, GpuRenderer>);
struct TerminalPaint {
    window: u64,
    frame: Arc<Frame>,
    format: wgpu::TextureFormat,
}
impl egui_wgpu::CallbackTrait for TerminalPaint {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _: &egui_wgpu::ScreenDescriptor,
        _: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if resources.get::<GpuRenderers>().is_none() {
            resources.insert(GpuRenderers(HashMap::new()));
        }
        let renderer = resources
            .get_mut::<GpuRenderers>()
            .unwrap()
            .0
            .entry(self.window)
            .or_insert_with(|| GpuRenderer {
                renderer: rustty_render_wgpu::Renderer::new(device, self.format),
                frame: None,
                prepares: 0,
            });
        if !renderer
            .frame
            .as_ref()
            .is_some_and(|frame| Arc::ptr_eq(frame, &self.frame))
        {
            // A failed upload can replace part of the GPU state. Retry it even
            // when the next UI frame still retains the same terminal content.
            renderer.frame = None;
            match renderer.renderer.prepare(device, queue, &self.frame) {
                Ok(()) => {
                    renderer.frame = Some(Arc::clone(&self.frame));
                    renderer.prepares += 1;
                }
                Err(error) => eprintln!("Rustty renderer: {error}"),
            }
        }
        Vec::new()
    }
    fn paint(
        &self,
        _: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(renderer) = resources
            .get::<GpuRenderers>()
            .and_then(|all| all.0.get(&self.window))
        {
            renderer.renderer.paint(pass);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_gpu_frames_skip_preparation_and_retry_after_errors() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .expect("GPU adapter");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
        let mut resources = egui_wgpu::CallbackResources::default();
        let mut encoder = device.create_command_encoder(&Default::default());
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [20, 20],
            pixels_per_point: 1.0,
        };
        let mut frame = Frame::empty([20, 20]);
        frame.quads.push(rustty_render::Quad::solid(
            [0.0, 0.0, 20.0, 20.0],
            rustty_render::Color::rgb([255; 3]),
        ));
        let original = Arc::new(frame);
        let mut prepare = |frame: Arc<Frame>| {
            let paint = TerminalPaint {
                window: 1,
                frame,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
            };
            egui_wgpu::CallbackTrait::prepare(
                &paint,
                &device,
                &queue,
                &screen,
                &mut encoder,
                &mut resources,
            );
            let renderer = &resources.get::<GpuRenderers>().unwrap().0[&1];
            (renderer.prepares, renderer.frame.is_some())
        };
        assert_eq!(prepare(Arc::clone(&original)), (1, true));
        assert_eq!(prepare(Arc::clone(&original)), (1, true));
        let replacement = Arc::new((*original).clone());
        assert_eq!(prepare(Arc::clone(&replacement)), (2, true));

        let mut invalid = (*original).clone();
        invalid.quads[0].rect[0] = f32::NAN;
        assert_eq!(prepare(Arc::new(invalid)), (2, false));
        assert_eq!(prepare(replacement), (3, true));
    }
}

#[cfg(all(test, target_os = "windows"))]
mod windows_tests {
    use super::*;

    fn unavailable_gpu_config() -> egui_wgpu::WgpuConfiguration {
        let mut config = egui_wgpu::WgpuConfiguration::default();
        let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup else {
            unreachable!();
        };
        // Surface creation fails deterministically without enumerating adapters
        // or creating a graphics device on the machine running the test.
        setup.instance_descriptor.backends = wgpu::Backends::empty();
        config
    }

    #[test]
    fn microsofts_unflagged_headless_adapter_is_software() {
        assert!(software_adapter(0x1414, "Microsoft Basic Render Driver"));
        assert!(!software_adapter(
            0x1414,
            "Microsoft Remote Display Adapter"
        ));
        assert!(!software_adapter(0x10de, "NVIDIA graphics"));
    }

    #[test]
    fn explicit_software_does_not_need_an_available_gpu_backend() {
        let painter = pollster::block_on(Painter::new(
            Context::default(),
            unavailable_gpu_config(),
            Preference::Software,
        ))
        .unwrap();
        assert!(painter.is_software());
        assert!(painter.render_state().is_none());
    }

    #[test]
    #[expect(
        deprecated,
        reason = "Windows supports hidden windows before the event loop starts"
    )]
    fn first_window_gpu_failure_falls_back_only_when_automatic() {
        use winit::{event_loop::EventLoop, platform::windows::EventLoopBuilderExtWindows};

        let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes().with_visible(false))
                .unwrap(),
        );
        let mut forced = pollster::block_on(Painter::new(
            Context::default(),
            unavailable_gpu_config(),
            Preference::Gpu,
        ))
        .unwrap();
        assert!(pollster::block_on(forced.set_window(ViewportId::ROOT, window.clone())).is_err());
        assert!(!forced.is_software());
        assert!(forced.render_state().is_none());

        // Inject the state reached after automatic hardware preflight succeeds,
        // while keeping the actual surface failure independent of the host GPU.
        forced.auto_fallback = true;
        pollster::block_on(forced.set_window(ViewportId::ROOT, window)).unwrap();
        assert!(forced.is_software());
        assert!(forced.description().contains("GPU initialization failed"));
        assert!(forced.render_state().is_none());

        let second = ViewportId::from_hash_of("second");
        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes().with_visible(false))
                .unwrap(),
        );
        pollster::block_on(forced.set_window(second, window)).unwrap();
        let Backend::Software(software) = &forced.backend else {
            unreachable!();
        };
        assert_eq!(software.surfaces.len(), 2);
        assert!(software.surfaces.contains_key(&ViewportId::ROOT));
        assert!(software.surfaces.contains_key(&second));
    }
}
