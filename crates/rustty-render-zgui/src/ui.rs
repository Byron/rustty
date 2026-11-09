//! Egui's meshes on zgui's graphics device; event handling stays in the desktop host.

use crate::{FORMAT, Result};
use egui::{ColorImage, TextureId, TextureOptions, TexturesDelta};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};
use wgpu::util::DeviceExt as _;
use zgui_render_wgpu::{Gpu, wgpu};

const MAX_IMAGE_BYTES: usize = 256 * 1024 * 1024;
const MAX_GEOMETRY: usize = 6_000_000;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Mesh {
    pub clip: egui::Rect,
    pub mesh: egui::Mesh,
}

struct Image {
    pixels: Arc<ColorImage>,
    options: TextureOptions,
    revision: u64,
}

#[derive(Default)]
pub(super) struct Textures {
    images: HashMap<TextureId, Image>,
    revision: u64,
}

impl Textures {
    pub fn update(&mut self, delta: &TexturesDelta) -> Result<()> {
        for (id, update) in delta
            .set
            .iter()
            .flat_map(|(id, updates)| updates.iter().map(move |update| (id, update)))
        {
            let egui::ImageData::Color(image) = &update.image;
            let [width, height] = image.size;
            if width == 0
                || height == 0
                || width > 16_384
                || height > 16_384
                || width
                    .checked_mul(height)
                    .and_then(|pixels| pixels.checked_mul(4))
                    .is_none_or(|bytes| bytes > MAX_IMAGE_BYTES)
                || width * height != image.pixels.len()
            {
                return Err("invalid zgui UI texture dimensions".into());
            }
            self.revision += 1;
            if let Some([x, y]) = update.pos {
                let target = self
                    .images
                    .get_mut(id)
                    .ok_or("zgui texture patch has no base image")?;
                if x.checked_add(width)
                    .is_none_or(|end| end > target.pixels.size[0])
                    || y.checked_add(height)
                        .is_none_or(|end| end > target.pixels.size[1])
                {
                    return Err("zgui texture patch exceeds its image".into());
                }
                let pixels = Arc::make_mut(&mut target.pixels);
                for row in 0..height {
                    let start = (y + row) * pixels.size[0] + x;
                    pixels.pixels[start..start + width]
                        .copy_from_slice(&image.pixels[row * width..(row + 1) * width]);
                }
                target.options = update.options;
                target.revision = self.revision;
            } else {
                let current = self
                    .images
                    .values()
                    .map(|image| image.pixels.pixels.len() * 4)
                    .sum::<usize>();
                let old = self
                    .images
                    .get(id)
                    .map_or(0, |image| image.pixels.pixels.len() * 4);
                if current - old + image.pixels.len() * 4 > MAX_IMAGE_BYTES
                    || self.images.len() >= 4096 && !self.images.contains_key(id)
                {
                    return Err("zgui UI textures exceed the memory budget".into());
                }
                self.images.insert(
                    *id,
                    Image {
                        pixels: image.clone(),
                        options: update.options,
                        revision: self.revision,
                    },
                );
            }
        }
        Ok(())
    }

    pub fn free<'a>(&mut self, ids: impl IntoIterator<Item = &'a TextureId>) {
        for id in ids {
            self.images.remove(id);
        }
    }

    pub fn versions(&self, meshes: &[Mesh]) -> Result<Vec<(TextureId, u64)>> {
        let mut versions = BTreeMap::new();
        for mesh in meshes {
            let id = mesh.mesh.texture_id;
            versions.insert(
                id,
                self.images
                    .get(&id)
                    .ok_or("zgui mesh references an absent texture")?
                    .revision,
            );
        }
        Ok(versions.into_iter().collect())
    }
}

struct Texture {
    _texture: wgpu::Texture,
    binding: wgpu::BindGroup,
    revision: u64,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: [u8; 4],
}

pub(super) struct Renderer {
    pipeline: wgpu::RenderPipeline,
    textures: HashMap<TextureId, Texture>,
    texture_layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
    uniform_binding: wgpu::BindGroup,
}

impl Renderer {
    pub fn new(device: &wgpu::Device) -> Self {
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rustty zgui UI viewport"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rustty zgui UI viewport"),
            contents: bytemuck::cast_slice(&[0f32; 4]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let uniform_binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rustty zgui UI viewport"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rustty zgui UI textures"),
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
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rustty zgui UI pipeline"),
            bind_group_layouts: &[Some(&uniform_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rustty zgui UI"),
            source: wgpu::ShaderSource::Wgsl(include_str!("ui.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rustty zgui UI"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0=>Float32x2,1=>Float32x2,2=>Unorm8x4],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            textures: HashMap::new(),
            texture_layout,
            uniform,
            uniform_binding,
        }
    }

    pub fn collect(&mut self, textures: &Textures) {
        self.textures
            .retain(|id, _| textures.images.contains_key(id));
    }

    fn upload(&mut self, gpu: &Gpu, textures: &Textures, meshes: &[Mesh]) -> Result<()> {
        for (id, revision) in textures.versions(meshes)? {
            if self
                .textures
                .get(&id)
                .is_some_and(|texture| texture.revision == revision)
            {
                continue;
            }
            let image = &textures.images[&id];
            if image
                .pixels
                .size
                .iter()
                .any(|size| *size > gpu.device().limits().max_texture_dimension_2d as usize)
            {
                return Err("zgui UI texture exceeds the GPU size limit".into());
            }
            let size = wgpu::Extent3d {
                width: image.pixels.size[0] as u32,
                height: image.pixels.size[1] as u32,
                depth_or_array_layers: 1,
            };
            let texture = gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: Some("rustty zgui UI image"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let pixels = image
                .pixels
                .pixels
                .iter()
                .flat_map(|pixel| pixel.to_array())
                .collect::<Vec<_>>();
            gpu.queue().write_texture(
                texture.as_image_copy(),
                &pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size.width * 4),
                    rows_per_image: Some(size.height),
                },
                size,
            );
            let address = match image.options.wrap_mode {
                egui::TextureWrapMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
                egui::TextureWrapMode::Repeat => wgpu::AddressMode::Repeat,
                egui::TextureWrapMode::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
            };
            let sampler = gpu.device().create_sampler(&wgpu::SamplerDescriptor {
                label: Some("rustty zgui UI sampler"),
                mag_filter: filter(image.options.magnification),
                min_filter: filter(image.options.minification),
                address_mode_u: address,
                address_mode_v: address,
                address_mode_w: address,
                ..Default::default()
            });
            let binding = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("rustty zgui UI image"),
                layout: &self.texture_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(
                            &texture.create_view(&Default::default()),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });
            self.textures.insert(
                id,
                Texture {
                    _texture: texture,
                    binding,
                    revision,
                },
            );
        }
        Ok(())
    }

    pub fn paint(
        &mut self,
        gpu: &Gpu,
        target: &wgpu::Texture,
        meshes: &[Mesh],
        textures: &Textures,
        scale: f32,
    ) -> Result<()> {
        let vertex_count = meshes
            .iter()
            .map(|mesh| mesh.mesh.vertices.len())
            .sum::<usize>();
        let index_count = meshes
            .iter()
            .map(|mesh| mesh.mesh.indices.len())
            .sum::<usize>();
        if vertex_count > MAX_GEOMETRY || index_count > MAX_GEOMETRY {
            return Err("zgui UI geometry exceeds its budget".into());
        }
        let mut vertices = Vec::with_capacity(vertex_count);
        let mut indices = Vec::with_capacity(index_count);
        let mut draws = Vec::with_capacity(meshes.len());
        for mesh in meshes {
            if !mesh.clip.is_finite() || !mesh.mesh.is_valid() {
                return Err("invalid zgui UI mesh".into());
            }
            let start = indices.len() as u32;
            let base = vertices.len() as u32;
            for vertex in &mesh.mesh.vertices {
                if !vertex.pos.is_finite() || !vertex.uv.is_finite() {
                    return Err("invalid zgui UI vertex".into());
                }
                vertices.push(Vertex {
                    position: [vertex.pos.x, vertex.pos.y],
                    uv: [vertex.uv.x, vertex.uv.y],
                    color: vertex.color.to_array(),
                });
            }
            indices.extend(mesh.mesh.indices.iter().map(|index| index + base));
            let clip = mesh.clip * scale;
            let left = clip.left().round().clamp(0.0, target.width() as f32) as u32;
            let top = clip.top().round().clamp(0.0, target.height() as f32) as u32;
            let right = clip
                .right()
                .round()
                .clamp(left as f32, target.width() as f32) as u32;
            let bottom = clip
                .bottom()
                .round()
                .clamp(top as f32, target.height() as f32) as u32;
            if right > left && bottom > top {
                draws.push((
                    start..indices.len() as u32,
                    mesh.mesh.texture_id,
                    [left, top, right - left, bottom - top],
                ));
            }
        }
        self.upload(gpu, textures, meshes)?;
        let vertex_buffer = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("rustty zgui UI vertices"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("rustty zgui UI indices"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        gpu.queue().write_buffer(
            &self.uniform,
            0,
            bytemuck::cast_slice(&[
                target.width() as f32 / scale,
                target.height() as f32 / scale,
                0.0,
                0.0,
            ]),
        );
        let view = target.create_view(&Default::default());
        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rustty zgui UI"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rustty zgui UI"),
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.uniform_binding, &[]);
            pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            for (indices, texture, [left, top, width, height]) in draws {
                pass.set_scissor_rect(left, top, width, height);
                pass.set_bind_group(1, &self.textures[&texture].binding, &[]);
                pass.draw_indexed(indices, 0, 0..1);
            }
        }
        gpu.queue().submit([encoder.finish()]);
        Ok(())
    }
}

fn filter(filter: egui::TextureFilter) -> wgpu::FilterMode {
    match filter {
        egui::TextureFilter::Nearest => wgpu::FilterMode::Nearest,
        egui::TextureFilter::Linear => wgpu::FilterMode::Linear,
    }
}
