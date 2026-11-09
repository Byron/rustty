//! The same terminal renderer on zgui's WGPU version.

pub use wgpu29 as wgpu;

// Compile the same shader and validation for the two hosts' distinct WGPU types.
#[allow(clippy::duplicate_mod)]
#[path = "renderer.rs"]
mod renderer;
pub use renderer::{RenderError, Renderer};

fn vertex_layout(layout: wgpu::VertexBufferLayout<'_>) -> wgpu::VertexBufferLayout<'_> {
    layout
}

fn submit_uploads(
    belt: &mut wgpu::util::StagingBelt,
    encoder: wgpu::CommandEncoder,
    queue: &wgpu::Queue,
) {
    belt.finish();
    queue.submit([encoder.finish()]);
    belt.recall();
}

fn write_rows(
    slice: &wgpu::BufferSlice<'_>,
    stride: usize,
    update: &rustty_render::AtlasUpload,
) -> Result<(), RenderError> {
    let mut mapped = slice.get_mapped_range_mut();
    for (row, source) in update
        .pixels
        .chunks_exact(update.size[0] as usize * 4)
        .enumerate()
    {
        let start = row * stride;
        mapped
            .slice(start..start + source.len())
            .copy_from_slice(source);
    }
    Ok(())
}

#[cfg(test)]
fn read_mapped(slice: &wgpu::BufferSlice<'_>) -> Vec<u8> {
    slice.get_mapped_range().to_vec()
}
