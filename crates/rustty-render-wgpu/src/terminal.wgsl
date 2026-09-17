@group(0) @binding(0) var atlas: texture_2d<f32>;
@group(0) @binding(1) var atlas_sampler: sampler;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) mode: u32,
}

@vertex fn vertex_main(
    @builtin(vertex_index) vertex: u32,
    @location(0) bounds: vec4<f32>,
    @location(1) texture_bounds: vec4<f32>,
    @location(2) color: vec4<f32>,
    @location(3) mode: u32,
) -> VertexOut {
    // TR, BR, TL, BL keeps the original two triangles and their diagonal.
    let corner = vec2<bool>(vertex < 2u, (vertex & 1u) != 0u);
    let position = select(bounds.xy, bounds.zw, corner);
    let uv = select(texture_bounds.xy, texture_bounds.zw, corner);
    return VertexOut(vec4<f32>(position, 0.0, 1.0), uv, color, mode);
}

fn encode_srgb(value: vec3<f32>) -> vec3<f32> {
    return select(12.92 * value, 1.055 * pow(max(value, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055,
        value > vec3<f32>(0.0031308));
}

@fragment fn fragment_main(input: VertexOut) -> @location(0) vec4<f32> {
    var color = input.color.rgb;
    var alpha = input.color.a;
    if input.mode == 1u {
        // Ghostty samples rasterized glyph masks without filtering. Blending
        // with transparent atlas padding opens seams between block glyphs at
        // fractional pane origins, even though their cell rectangles touch.
        let size = vec2<i32>(textureDimensions(atlas));
        let pixel = clamp(vec2<i32>(input.uv * vec2<f32>(size)), vec2<i32>(0), size - 1);
        alpha *= textureLoad(atlas, pixel, 0).a;
    } else if input.mode == 2u {
        // Explicit LOD keeps scaled RGBA content filtered in this branch.
        let texel = textureSampleLevel(atlas, atlas_sampler, input.uv, 0.0);
        alpha *= texel.a;
        color = texel.rgb;
    }
    // egui commonly supplies an UNORM target; also support an sRGB host target.
    if !TARGET_SRGB { color = encode_srgb(color); }
    return vec4<f32>(color * alpha, alpha);
}
