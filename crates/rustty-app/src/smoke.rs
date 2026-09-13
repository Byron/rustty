//! Opt-in native integration check; all shells and saved state are test-owned.
use super::*;
use std::fs;

pub(super) struct Smoke {
    pub directory: PathBuf,
    pub offscreen: bool,
    stage: u8,
    deadline: Instant,
    next: Instant,
    original: Option<Id>,
    idle_frames: u64,
}
impl Smoke {
    pub fn from_env(loaded: &mut LoadedConfig) -> Result<Option<Self>> {
        let Some(directory) = std::env::var_os("RUSTTY_SMOKE_DIR") else {
            return Ok(None);
        };
        let directory = PathBuf::from(directory);
        fs::create_dir_all(&directory)?;
        for name in ["window.png", "result.json"] {
            match fs::remove_file(directory.join(name)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        let command = config::Command::Direct(vec!["/bin/sh".into(),"-c".into(),r"printf '\033[2J\033[H\033[1;36mRustty native smoke\033[0m\n\033]7;file://localhost/tmp\007'; exec /bin/sh -i".into()]);
        loaded.config.command = Some(command.clone());
        loaded.config.initial_command = Some(command);
        loaded.config.working_directory = Some(PathBuf::from("/tmp"));
        loaded.config.window_save_state = config::WindowSaveState::Always;
        loaded.config.cursor_style_blink = Some(false);
        loaded.config.keybinds.retain(|b| !b.flags.global);
        Ok(Some(Self {
            directory,
            offscreen: std::env::var_os("RUSTTY_SMOKE_OFFSCREEN").is_some(),
            stage: 0,
            deadline: Instant::now() + Duration::from_secs(45),
            next: Instant::now(),
            original: None,
            idle_frames: 0,
        }))
    }
    pub fn step(&mut self, app: &mut App, event_loop: &ActiveEventLoop) -> Result<bool> {
        if Instant::now() > self.deadline {
            return Err(format!(
                "native smoke timed out at stage {}: {:?}",
                self.stage, app.errors
            )
            .into());
        }
        if Instant::now() < self.next {
            return Ok(false);
        }
        let Some(key) = app
            .windows
            .iter()
            .find(|(_, h)| {
                app.index(h.id)
                    .is_some_and(|i| !app.workspace.windows[i].quick)
            })
            .map(|(key, _)| *key)
        else {
            return Ok(false);
        };
        let mut host = app.windows.remove(&key).unwrap();
        let result = self.step_window(app, event_loop, &mut host);
        app.windows.insert(key, host);
        app.reconcile(event_loop);
        result
    }
    fn step_window(
        &mut self,
        app: &mut App,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
    ) -> Result<bool> {
        let text = |app: &App, id: Id| {
            app.panes
                .get(&id)
                .and_then(|p| p.session.terminal().ok().map(|t| t.plain_text()))
                .unwrap_or_default()
        };
        let pane = app.focused(host.id).ok_or("no active pane")?;
        match self.stage {
            0 => {
                if host.frames == 0 || !text(app, pane).contains("Rustty native smoke") {
                    return Ok(false);
                }
                self.original = Some(pane);
                app.write(pane, b"printf '\\122USTTY_INPUT_OK\\n'\r".to_vec());
                eprintln!("Native smoke: shell window rendered");
                self.stage = 1;
            }
            1 => {
                if !text(app, pane).contains("RUSTTY_INPUT_OK") {
                    return Ok(false);
                }
                app.action(event_loop, host, Action::NewSplit(Direction::Right), true);
                app.action(event_loop, host, Action::NewSplit(Direction::Down), true);
                app.focus_pane(host.id, self.original.unwrap());
                app.action(event_loop, host, Action::NewSplit(Direction::Down), true);
                if app.tab(host.id).unwrap().panes.len() != 4 {
                    return Err("split creation lost a pane".into());
                }
                app.action(event_loop, host, Action::ToggleQuadrantZoom, true);
                let before = app.focused(host.id).unwrap();
                host.peek = Some(Peek {
                    chord: config::Modifiers {
                        control: true,
                        super_key: true,
                        ..Default::default()
                    },
                    target: before,
                });
                if !app.action(
                    event_loop,
                    host,
                    Action::GotoSplit(Direction::QuadrantRight),
                    true,
                ) {
                    return Err("quadrant navigation was blocked".into());
                }
                let target = host.peek.take().unwrap().target;
                if target == before {
                    return Err("quadrant navigation kept old focus".into());
                }
                app.focus_pane(host.id, target);
                app.action(event_loop, host, Action::ToggleQuadrantZoom, true);
                app.action(event_loop, host, Action::NewTab, true);
                if app.panes.len() != 5 {
                    return Err("tab creation did not start a fifth PTY".into());
                }
                app.action(event_loop, host, Action::PreviousTab, true);
                eprintln!("Native smoke: splits, tabs, and quadrant actions passed");
                self.stage = 2;
            }
            2 => {
                if app
                    .panes
                    .keys()
                    .any(|&id| !text(app, id).contains("Rustty native smoke"))
                {
                    return Ok(false);
                }
                if app.panes.values().any(|p| p.cwd != Path::new("/tmp")) {
                    return Err("OSC directory was not decoded before restoration".into());
                }
                app.save();
                let restored =
                    Workspace::load(&app.state_path)?.ok_or("workspace was not saved")?;
                if restored.windows[0].tabs.len() != 2
                    || restored.windows[0].tabs[0].panes.len() != 4
                {
                    return Err("restoration lost tabs or splits".into());
                }
                eprintln!(
                    "Native smoke: requesting frame, visible={}, occluded={}, frames={}, format={:?}",
                    host.visible,
                    host.occluded,
                    host.frames,
                    app.painter.render_state().map(|s| s.target_format)
                );
                host.capture = true;
                host.repaint();
                self.stage = 3;
            }
            3 => {
                if self.offscreen && self.directory.join("window.png").is_file() {
                    self.idle_frames = host.frames;
                    self.next = Instant::now() + Duration::from_millis(1500);
                    self.stage = 4;
                    return Ok(false);
                }
                if let Some(state) = app.painter.render_state() {
                    state.device.poll(wgpu::PollType::Poll)?;
                }
                let mut events = Vec::new();
                app.painter.handle_screenshots(&mut events);
                if events.is_empty() {
                    host.capture = true;
                    host.repaint();
                    self.next = Instant::now() + Duration::from_millis(100);
                }
                for event in events {
                    if let egui::Event::Screenshot { image, .. } = event {
                        let file = fs::File::create(self.directory.join("window.png"))?;
                        let mut encoder =
                            png::Encoder::new(file, image.width() as u32, image.height() as u32);
                        encoder.set_color(png::ColorType::Rgba);
                        encoder.set_depth(png::BitDepth::Eight);
                        encoder.write_header()?.write_image_data(
                            &image
                                .pixels
                                .iter()
                                .flat_map(|c| c.to_array())
                                .collect::<Vec<_>>(),
                        )?;
                        self.idle_frames = host.frames;
                        self.next = Instant::now() + Duration::from_millis(1500);
                        self.stage = 4;
                    }
                }
            }
            4 => {
                if !app.errors.is_empty() {
                    return Err(format!("native app errors: {:?}", app.errors).into());
                }
                if host.frames.saturating_sub(self.idle_frames) > 4 {
                    return Err(format!(
                        "idle window kept repainting: {} frames",
                        host.frames - self.idle_frames
                    )
                    .into());
                }
                let report = serde_json::json!({"passed":true,"capture_mode":if self.offscreen { "offscreen" } else { "surface" },"checks":["native-window","metal-wgpu-frame","pty-input-output","four-splits","tab-creation","quadrant-focus-and-zoom","cwd-uri-decoding","workspace-roundtrip","idle-rendering"],"frames":host.frames,"idle_frames":host.frames-self.idle_frames,"panes":app.panes.len()});
                fs::write(
                    self.directory.join("result.json"),
                    serde_json::to_vec_pretty(&report)?,
                )?;
                println!("Native smoke passed: {}", self.directory.display());
                return Ok(true);
            }
            _ => unreachable!(),
        }
        Ok(false)
    }
}

/// Render the same host primitives to a texture when no drawable is available
/// (e.g. a locked CI Mac). This does not claim visible surface presentation.
pub(super) fn capture(
    state: &egui_wgpu::RenderState,
    primitives: &[egui::ClippedPrimitive],
    delta: &egui::TexturesDelta,
    size: [u32; 2],
    scale: f32,
    path: &Path,
) -> Result<()> {
    let device = &state.device;
    let queue = &state.queue;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Rustty host capture"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: state.target_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let stride = (size[0] * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Rustty host readback"),
        size: u64::from(stride) * u64::from(size[1]),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    let descriptor = egui_wgpu::ScreenDescriptor {
        size_in_pixels: size,
        pixels_per_point: scale,
    };
    let mut renderer = state.renderer.write();
    for (id, deltas) in &delta.set {
        for delta in deltas {
            renderer.update_texture(device, queue, *id, delta);
        }
    }
    let commands = renderer.update_buffers(device, queue, &mut encoder, primitives, &descriptor);
    {
        let view = texture.create_view(&Default::default());
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Rustty host capture"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
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
        renderer.render(&mut pass.forget_lifetime(), primitives, &descriptor);
    }
    drop(renderer);
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: None,
            },
        },
        texture.size(),
    );
    queue.submit(commands.into_iter().chain([encoder.finish()]));
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    device.poll(wgpu::PollType::wait_indefinitely())?;
    rx.recv_timeout(Duration::from_secs(5))??;
    let mapped = buffer.slice(..).get_mapped_range()?;
    let mut pixels = Vec::with_capacity((size[0] * size[1] * 4) as usize);
    for row in mapped.chunks_exact(stride as usize) {
        for pixel in row[..size[0] as usize * 4].as_chunks::<4>().0 {
            if state.target_format == wgpu::TextureFormat::Bgra8Unorm {
                pixels.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
            } else {
                pixels.extend_from_slice(pixel);
            }
        }
    }
    drop(mapped);
    buffer.unmap();
    let mut encoder = png::Encoder::new(fs::File::create(path)?, size[0], size[1]);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&pixels)?;
    Ok(())
}
