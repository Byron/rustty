//! WGPU terminal rendering into host-owned render passes.
//!
//! No window, event loop, native view or UI toolkit is owned here. The host
//! supplies the device/queue and sets its viewport and scissor before `paint`.

mod renderer;

pub use renderer::{RenderError, Renderer};

#[cfg(feature = "zgui")]
pub mod zgui;

pub use wgpu;

fn vertex_layout(layout: wgpu::VertexBufferLayout<'_>) -> Option<wgpu::VertexBufferLayout<'_>> {
    Some(layout)
}

fn submit_uploads(
    belt: &mut wgpu::util::StagingBelt,
    encoder: wgpu::CommandEncoder,
    queue: &wgpu::Queue,
) {
    belt.finish_and_recall_on_submit(&encoder);
    queue.submit([encoder.finish()]);
}

fn write_rows(
    slice: &wgpu::BufferSlice<'_>,
    stride: usize,
    update: &rustty_render::AtlasUpload,
) -> Result<(), RenderError> {
    let mut mapped = slice
        .get_mapped_range_mut()
        .map_err(|error| RenderError(error.to_string()))?;
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
    slice.get_mapped_range().unwrap().to_vec()
}
