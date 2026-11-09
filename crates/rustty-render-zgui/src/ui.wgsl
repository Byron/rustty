// Egui uses premultiplied gamma-space colours on an unencoded target.
struct Viewport { size: vec2<f32>, padding: vec2<f32> }
@group(0) @binding(0) var<uniform> viewport: Viewport;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}

@vertex
fn vertex_main(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>) -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4<f32>(2.0 * position.x / viewport.size.x - 1.0, 1.0 - 2.0 * position.y / viewport.size.y, 0.0, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

@fragment
fn fragment_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let color = in.color * textureSample(image, image_sampler, in.uv);
    // Match egui-wgpu's default interleaved gradient dithering.
    let noise = fract(52.9829189 * fract(0.06711056 * in.position.x + 0.00583715 * in.position.y));
    return vec4<f32>(color.rgb + (noise - 0.5) * (0.95 / 255.0), color.a);
}
