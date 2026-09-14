//! WGPU terminal rendering into host-owned render passes.
//!
//! No window, event loop, native view or UI toolkit is owned here. The host
//! supplies the device/queue and sets its viewport and scissor before `paint`.

use bytemuck::{Pod, Zeroable};
use rustty_render::{Frame, Paint};
use std::{fmt, ops::Range};

#[derive(Debug)]
pub struct RenderError(String);
impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for RenderError {}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
    mode: u32,
}

struct Atlas {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    size: u32,
    revision: u64,
}
struct Batch {
    atlas: usize,
    vertices: Range<u32>,
}

pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    atlases: Vec<Atlas>,
    empty_atlas: Atlas,
    vertex_buffer: wgpu::Buffer,
    vertex_capacity: u64,
    batches: Vec<Batch>,
    generation: Option<u64>,
}

impl Renderer {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rustty atlas layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rustty glyph sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rustty pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let source = format!(
            "const TARGET_SRGB: bool = {};\n{}",
            target_format.is_srgb(),
            include_str!("terminal.wgsl")
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rustty terminal shader"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rustty terminal pipeline"),layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState { module: &shader,entry_point: Some("vertex_main"),
                compilation_options: Default::default(),buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Vertex>() as u64,step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0=>Float32x2,1=>Float32x2,2=>Float32x4,3=>Uint32],
                })] },
            fragment: Some(wgpu::FragmentState { module: &shader,entry_point: Some("fragment_main"),
                compilation_options: Default::default(),targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),write_mask: wgpu::ColorWrites::ALL,
                })] }),
            primitive: Default::default(),depth_stencil: None,multisample: Default::default(),multiview_mask: None,cache: None,
        });
        let empty_atlas = atlas(device, &layout, &sampler, 1);
        let vertex_capacity = 256;
        let vertex_buffer = vertex_buffer(device, vertex_capacity);
        Self {
            pipeline,
            layout,
            sampler,
            atlases: Vec::new(),
            empty_atlas,
            vertex_buffer,
            vertex_capacity,
            batches: Vec::new(),
            generation: None,
        }
    }

    /// Upload changed glyphs and replace the vertex stream. Repeated preparation
    /// of a retained Frame does not upload unchanged atlas regions.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &Frame,
    ) -> Result<(), RenderError> {
        self.batches.clear();
        if frame.size.contains(&0) {
            return Ok(());
        }
        if self.generation != Some(frame.generation) {
            self.atlases.clear();
            self.generation = Some(frame.generation);
        }
        for update in &frame.atlas_uploads {
            let required_bytes = update.size[0] as u64 * update.size[1] as u64 * 4;
            if required_bytes != update.pixels.len() as u64
                || update.page_size == 0
                || update.page_size > device.limits().max_texture_dimension_2d
                || update.origin[0]
                    .checked_add(update.size[0])
                    .is_none_or(|v| v > update.page_size)
                || update.origin[1]
                    .checked_add(update.size[1])
                    .is_none_or(|v| v > update.page_size)
                || update.page > self.atlases.len()
            {
                return Err(RenderError("invalid glyph atlas upload".into()));
            }
            if update.page == self.atlases.len() {
                self.atlases
                    .push(atlas(device, &self.layout, &self.sampler, update.page_size));
            }
            let atlas = &mut self.atlases[update.page];
            if atlas.size != update.page_size {
                return Err(RenderError(
                    "atlas size changed without a new generation".into(),
                ));
            }
            if update.revision <= atlas.revision {
                continue;
            }
            if !update.size.contains(&0) {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &atlas.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: update.origin[0],
                            y: update.origin[1],
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &update.pixels,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(update.size[0] * 4),
                        rows_per_image: None,
                    },
                    wgpu::Extent3d {
                        width: update.size[0],
                        height: update.size[1],
                        depth_or_array_layers: 1,
                    },
                );
            }
            atlas.revision = update.revision;
        }
        let mut vertices = Vec::with_capacity(frame.quads.len().saturating_mul(6));
        for quad in &frame.quads {
            if quad
                .rect
                .iter()
                .chain(quad.uv.iter())
                .chain(quad.color.0.iter())
                .any(|v| !v.is_finite())
            {
                return Err(RenderError("non-finite terminal draw coordinates".into()));
            }
            if quad.rect[2] <= 0.0 || quad.rect[3] <= 0.0 {
                continue;
            }
            if quad.paint != Paint::Solid && quad.atlas >= self.atlases.len() {
                return Err(RenderError("glyph references an absent atlas".into()));
            }
            let page = if quad.paint == Paint::Solid {
                0
            } else {
                quad.atlas
            };
            let start = vertices.len() as u32;
            let [x, y, w, h] = quad.rect;
            let [u0, v0, u1, v1] = quad.uv;
            let mode = match quad.paint {
                Paint::Solid => 0,
                Paint::Mask => 1,
                Paint::Color => 2,
            };
            for (position, uv) in [
                ([x, y], [u0, v0]),
                ([x + w, y], [u1, v0]),
                ([x + w, y + h], [u1, v1]),
                ([x, y], [u0, v0]),
                ([x + w, y + h], [u1, v1]),
                ([x, y + h], [u0, v1]),
            ] {
                vertices.push(Vertex {
                    position: [
                        position[0] * 2.0 / frame.size[0] as f32 - 1.0,
                        1.0 - position[1] * 2.0 / frame.size[1] as f32,
                    ],
                    uv,
                    color: quad.color.0,
                    mode,
                });
            }
            if let Some(batch) = self.batches.last_mut().filter(|batch| batch.atlas == page) {
                batch.vertices.end += 6;
            } else {
                self.batches.push(Batch {
                    atlas: page,
                    vertices: start..start + 6,
                });
            }
        }
        let bytes = bytemuck::cast_slice(&vertices);
        if bytes.len() as u64 > device.limits().max_buffer_size {
            return Err(RenderError(
                "terminal frame exceeds GPU buffer limit".into(),
            ));
        }
        if bytes.len() as u64 > self.vertex_capacity {
            self.vertex_capacity = (bytes.len() as u64).next_power_of_two();
            self.vertex_buffer = vertex_buffer(device, self.vertex_capacity);
        }
        if !bytes.is_empty() {
            queue.write_buffer(&self.vertex_buffer, 0, bytes);
        }
        Ok(())
    }

    /// Draw into the caller's active pass, using its viewport and scissor.
    pub fn paint(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.batches.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        for batch in &self.batches {
            let atlas = self.atlases.get(batch.atlas).unwrap_or(&self.empty_atlas);
            pass.set_bind_group(0, &atlas.bind_group, &[]);
            pass.draw(batch.vertices.clone(), 0..1);
        }
    }
}

fn vertex_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rustty terminal vertices"),
        size,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn atlas(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    size: u32,
) -> Atlas {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rustty glyph atlas"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rustty glyph atlas"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    Atlas {
        texture,
        bind_group,
        size,
        revision: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustty_render::{AtlasUpload, Color, Quad};
    use std::sync::Arc;

    #[test]
    fn offscreen_render_preserves_alpha_color_and_tiled_glyph_coverage() {
        const HEIGHT: u32 = 52;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .expect("GPU adapter");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = Renderer::new(&device, format);
        let mut frame = Frame::empty([32, HEIGHT]);
        frame.atlas_uploads.push(AtlasUpload {
            revision: 1,
            page: 0,
            page_size: 2,
            origin: [0, 0],
            size: [2, 1],
            pixels: Arc::from([255, 255, 255, 128, 0, 255, 0, 255]),
        });
        frame.quads.push(Quad::solid(
            [0.0, 0.0, 32.0, HEIGHT as f32],
            Color::rgb([255, 255, 255]),
        ));
        frame.quads.push(Quad {
            rect: [0.0, 0.0, 8.0, 8.0],
            uv: [0.25, 0.25, 0.25, 0.25],
            color: Color::rgb([0, 0, 255]),
            paint: Paint::Mask,
            atlas: 0,
        });
        frame.quads.push(Quad {
            rect: [16.0, 0.0, 8.0, 8.0],
            uv: [0.75, 0.25, 0.75, 0.25],
            color: Color::rgb([255, 0, 0]),
            paint: Paint::Color,
            atlas: 0,
        });
        // Scaled RGBA content still interpolates between texels.
        frame.quads.push(Quad {
            rect: [0.0, 8.0, 8.0, 4.0],
            uv: [0.0, 0.25, 1.0, 0.25],
            color: Color::rgb([255; 3]),
            paint: Paint::Color,
            atlas: 0,
        });
        // Like the glyph atlas, each full-block mask has transparent padding.
        // Pane splits can place these otherwise contiguous cells between pixels.
        frame.atlas_uploads.push(AtlasUpload {
            revision: 2,
            page: 1,
            page_size: 8,
            origin: [1, 1],
            size: [4, 4],
            pixels: Arc::from([255; 4 * 4 * 4]),
        });
        for (index, fraction) in [0.0, 0.25, 0.5, 0.75].into_iter().enumerate() {
            for row in 0..2 {
                for col in 0..6 {
                    frame.quads.push(Quad {
                        rect: [
                            2.0 + col as f32 * 4.0 + fraction,
                            16.0 + index as f32 * 8.0 + row as f32 * 4.0 + fraction,
                            4.0,
                            4.0,
                        ],
                        uv: [0.125, 0.125, 0.625, 0.625],
                        color: Color::rgb([0, 255, 0]),
                        paint: Paint::Mask,
                        atlas: 1,
                    });
                }
            }
        }
        renderer.prepare(&device, &queue, &frame).unwrap();
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen terminal test"),
            size: wgpu::Extent3d {
                width: 32,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * u64::from(HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            renderer.paint(&mut pass);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: 32,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                sender.send(result).unwrap();
            });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        receiver.recv().unwrap().unwrap();
        let bytes = readback.slice(..).get_mapped_range().unwrap();
        let covered = &bytes[4 * 4..4 * 4 + 4];
        assert!(
            (180..=195).contains(&covered[0]) && (180..=195).contains(&covered[1]),
            "{covered:?}"
        );
        assert_eq!(&covered[2..], &[255, 255]);
        assert_eq!(&bytes[20 * 4..20 * 4 + 4], &[0, 255, 0, 255]);
        assert_eq!(&bytes[28 * 4..28 * 4 + 4], &[255, 255, 255, 255]);
        let pixel = |x: usize, y: usize| &bytes[y * 256 + x * 4..y * 256 + x * 4 + 4];
        let interpolated = pixel(3, 9);
        assert!((1..255).contains(&interpolated[0]), "{interpolated:?}");
        for index in 0..4 {
            for y in 18 + index * 8..23 + index * 8 {
                for x in 4..25 {
                    assert_eq!(
                        pixel(x, y),
                        &[0, 255, 0, 255],
                        "gap at ({x}, {y}), phase {index}/4"
                    );
                }
            }
        }
    }
}
