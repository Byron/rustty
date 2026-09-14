use crate::{RenderError, Result, texture::Texture};
use egui::{Mesh, Rect, epaint::Vertex};

#[derive(Clone, Copy)]
pub(crate) struct Clip(pub [i32; 4]);
impl Clip {
    pub fn from_points(rect: Rect, scale: f32, size: [u32; 2]) -> Result<Self> {
        if [rect.min.x, rect.min.y, rect.max.x, rect.max.y]
            .iter()
            .any(|v| v.is_nan())
        {
            return Err(RenderError("invalid clipping rectangle"));
        }
        let viewport = egui::epaint::ViewportInPixels::from_points(&rect, scale, size);
        Ok(Self([
            viewport.left_px,
            viewport.top_px,
            viewport.left_px + viewport.width_px,
            viewport.top_px + viewport.height_px,
        ]))
    }
    pub fn intersect(self, other: Self) -> Self {
        Self([
            self.0[0].max(other.0[0]),
            self.0[1].max(other.0[1]),
            self.0[2].min(other.0[2]),
            self.0[3].min(other.0[3]),
        ])
    }
    pub fn bounds(self, rect: [f32; 4]) -> Self {
        self.intersect(Self([
            (rect[0] - 0.5).ceil() as i32,
            (rect[1] - 0.5).ceil() as i32,
            (rect[2] - 0.5).ceil() as i32,
            (rect[3] - 0.5).ceil() as i32,
        ]))
    }
    pub fn area(self) -> u64 {
        (self.0[2] - self.0[0]).max(0) as u64 * (self.0[3] - self.0[1]).max(0) as u64
    }
}

pub(crate) struct Canvas<'a> {
    pub pixels: &'a mut [u8],
    pub size: [u32; 2],
    pub work: u64,
    pub geometry: usize,
}
impl Canvas<'_> {
    pub fn charge(&mut self, bounds: Clip) -> Result<()> {
        self.work = self
            .work
            .checked_sub(bounds.area())
            .ok_or(RenderError("frame exceeds software pixel work limit"))?;
        Ok(())
    }
    pub fn geometry(&mut self, count: usize) -> Result<()> {
        self.geometry = self
            .geometry
            .checked_sub(count)
            .ok_or(RenderError("frame exceeds software geometry limit"))?;
        Ok(())
    }
    pub fn pixel(&mut self, x: i32, y: i32, color: [f32; 4]) {
        let offset = (y as usize * self.size[0] as usize + x as usize) * 4;
        blend(&mut self.pixels[offset..offset + 4], color);
    }
    pub fn solid(&mut self, bounds: Clip, color: [f32; 4]) {
        if bounds.area() == 0 {
            return;
        }
        let opaque = color[3] >= 255.0;
        let encoded = color.map(|v| v.round().clamp(0.0, 255.0) as u8);
        let width = self.size[0] as usize;
        for y in bounds.0[1]..bounds.0[3] {
            let start = (y as usize * width + bounds.0[0] as usize) * 4;
            let end = (y as usize * width + bounds.0[2] as usize) * 4;
            for pixel in self.pixels[start..end].as_chunks_mut::<4>().0 {
                if opaque {
                    pixel.copy_from_slice(&encoded);
                } else {
                    blend(pixel, color);
                }
            }
        }
    }
}

fn blend(target: &mut [u8], source: [f32; 4]) {
    let inverse = 1.0 - source[3].clamp(0.0, 255.0) / 255.0;
    for channel in 0..4 {
        target[channel] = (source[channel] + target[channel] as f32 * inverse)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
}

pub(crate) fn coordinate(value: f32) -> bool {
    value.is_finite() && value.abs() <= 1_000_000.0
}

pub(crate) fn mesh(
    canvas: &mut Canvas<'_>,
    clip: Clip,
    scale: f32,
    mesh: &Mesh,
    texture: &Texture,
) -> Result<()> {
    canvas.geometry(mesh.indices.len().max(mesh.vertices.len()))?;
    if !mesh.indices.len().is_multiple_of(3) || !mesh.is_valid() {
        return Err(RenderError("invalid mesh triangle indices"));
    }
    if mesh.vertices.iter().any(|v| {
        ![v.pos.x * scale, v.pos.y * scale, v.uv.x, v.uv.y]
            .into_iter()
            .all(coordinate)
    }) {
        return Err(RenderError("invalid mesh coordinates"));
    }
    let mut index = 0;
    while index < mesh.indices.len() {
        if index + 6 <= mesh.indices.len()
            && let Some((rect, uv, tint)) = rectangle(mesh, &mesh.indices[index..index + 6], scale)
        {
            mesh_rectangle(canvas, clip, rect, uv, tint, texture)?;
            index += 6;
        } else {
            let vertices = std::array::from_fn(|i| mesh.vertices[mesh.indices[index + i] as usize]);
            triangle(canvas, clip, scale, vertices, texture)?;
            index += 3;
        }
    }
    Ok(())
}

// Recognize the common pair of triangles forming an axis-aligned image/text
// rectangle. Their common edge must be the rectangle diagonal, not an outer
// edge; otherwise overlapping triangles could incorrectly become one fill.
fn rectangle(mesh: &Mesh, indices: &[u32], scale: f32) -> Option<([f32; 4], [f32; 4], [f32; 4])> {
    let vertices: [Vertex; 6] = std::array::from_fn(|i| mesh.vertices[indices[i] as usize]);
    let first = vertices[0];
    if vertices.iter().any(|v| v.color != first.color) {
        return None;
    }
    let min_x = vertices
        .iter()
        .map(|v| v.pos.x)
        .fold(f32::INFINITY, f32::min);
    let min_y = vertices
        .iter()
        .map(|v| v.pos.y)
        .fold(f32::INFINITY, f32::min);
    let max_x = vertices
        .iter()
        .map(|v| v.pos.x)
        .fold(f32::NEG_INFINITY, f32::max);
    let max_y = vertices
        .iter()
        .map(|v| v.pos.y)
        .fold(f32::NEG_INFINITY, f32::max);
    if min_x >= max_x || min_y >= max_y {
        return None;
    }
    let corner = |v: &Vertex| -> Option<usize> {
        let x = if v.pos.x == min_x {
            0
        } else if v.pos.x == max_x {
            1
        } else {
            return None;
        };
        let y = if v.pos.y == min_y {
            0
        } else if v.pos.y == max_y {
            2
        } else {
            return None;
        };
        Some(x + y)
    };
    let mut masks = [0u8; 2];
    let mut corners = [first; 4];
    for (i, vertex) in vertices.iter().enumerate() {
        let c = corner(vertex)?;
        masks[i / 3] |= 1 << c;
        corners[c] = *vertex;
    }
    if masks.iter().any(|m| m.count_ones() != 3) || masks[0] | masks[1] != 15 {
        return None;
    }
    let shared = masks[0] & masks[1];
    if shared != 0b1001 && shared != 0b0110 {
        return None;
    }
    let uv = [
        corners[0].uv.x,
        corners[0].uv.y,
        corners[3].uv.x,
        corners[3].uv.y,
    ];
    for vertex in vertices {
        let c = corner(&vertex)?;
        if vertex.uv.x != uv[if c & 1 == 0 { 0 } else { 2 }]
            || vertex.uv.y != uv[if c & 2 == 0 { 1 } else { 3 }]
        {
            return None;
        }
    }
    Some((
        [min_x * scale, min_y * scale, max_x * scale, max_y * scale],
        uv,
        first.color.to_array().map(|c| c as f32 / 255.0),
    ))
}

fn mesh_rectangle(
    canvas: &mut Canvas<'_>,
    clip: Clip,
    rect: [f32; 4],
    uv: [f32; 4],
    tint: [f32; 4],
    texture: &Texture,
) -> Result<()> {
    let bounds = clip.bounds(rect);
    canvas.charge(bounds)?;
    let dx = (uv[2] - uv[0]) / (rect[2] - rect[0]);
    let dy = (uv[3] - uv[1]) / (rect[3] - rect[1]);
    let filter = texture.filter([dx, 0.0], [0.0, dy]);
    if dx == 0.0 && dy == 0.0 {
        let texel = texture.sample([uv[0], uv[1]], filter, false);
        canvas.solid(bounds, std::array::from_fn(|c| texel[c] * tint[c]));
        return Ok(());
    }
    for y in bounds.0[1]..bounds.0[3] {
        let v = uv[1] + (y as f32 + 0.5 - rect[1]) * dy;
        for x in bounds.0[0]..bounds.0[2] {
            let u = uv[0] + (x as f32 + 0.5 - rect[0]) * dx;
            let texel = texture.sample([u, v], filter, false);
            canvas.pixel(x, y, std::array::from_fn(|c| texel[c] * tint[c]));
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Point {
    x: i64,
    y: i64,
}
fn edge(a: Point, b: Point, p: Point) -> i64 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}
fn top_left(a: Point, b: Point) -> bool {
    b.y < a.y || (b.y == a.y && b.x > a.x)
}

fn triangle(
    canvas: &mut Canvas<'_>,
    clip: Clip,
    scale: f32,
    mut vertices: [Vertex; 3],
    texture: &Texture,
) -> Result<()> {
    // Fixed subpixel coordinates make shared edges exactly complementary.
    // Interpolation uses the unbiased edge values, even for excluded edges.
    let mut points = vertices.map(|v| Point {
        x: (v.pos.x * scale * 256.0).round() as i64,
        y: (v.pos.y * scale * 256.0).round() as i64,
    });
    let mut area = edge(points[0], points[1], points[2]);
    if area == 0 {
        return Ok(());
    }
    if area < 0 {
        vertices.swap(1, 2);
        points.swap(1, 2);
        area = -area;
    }
    let bounds = clip.intersect(Clip([
        points
            .iter()
            .map(|p| p.x.div_euclid(256) as i32)
            .min()
            .unwrap(),
        points
            .iter()
            .map(|p| p.y.div_euclid(256) as i32)
            .min()
            .unwrap(),
        points
            .iter()
            .map(|p| (p.x + 255).div_euclid(256) as i32)
            .max()
            .unwrap(),
        points
            .iter()
            .map(|p| (p.y + 255).div_euclid(256) as i32)
            .max()
            .unwrap(),
    ]));
    canvas.charge(bounds)?;
    let edges = [
        (points[1], points[2]),
        (points[2], points[0]),
        (points[0], points[1]),
    ];
    let inclusive = edges.map(|(a, b)| top_left(a, b));
    let step_x = edges.map(|(a, b)| -(b.y - a.y) * 256);
    let step_y = edges.map(|(a, b)| (b.x - a.x) * 256);
    let inverse = 1.0 / area as f64;
    let gradient = |steps: [i64; 3], axis: usize| -> f32 {
        (0..3)
            .map(|i| {
                steps[i] as f64
                    * inverse
                    * if axis == 0 {
                        vertices[i].uv.x as f64
                    } else {
                        vertices[i].uv.y as f64
                    }
            })
            .sum::<f64>() as f32
    };
    let filter = texture.filter(
        [gradient(step_x, 0), gradient(step_x, 1)],
        [gradient(step_y, 0), gradient(step_y, 1)],
    );
    let colors = vertices.map(|v| v.color.to_array());
    let start = Point {
        x: bounds.0[0] as i64 * 256 + 128,
        y: bounds.0[1] as i64 * 256 + 128,
    };
    let mut row = edges.map(|(a, b)| edge(a, b, start));
    for y in bounds.0[1]..bounds.0[3] {
        let mut values = row;
        for x in bounds.0[0]..bounds.0[2] {
            if (0..3).all(|i| values[i] > 0 || (values[i] == 0 && inclusive[i])) {
                let weights = values.map(|v| (v as f64 * inverse) as f32);
                let uv = [
                    (0..3).map(|i| vertices[i].uv.x * weights[i]).sum(),
                    (0..3).map(|i| vertices[i].uv.y * weights[i]).sum(),
                ];
                let texel = texture.sample(uv, filter, false);
                let color = std::array::from_fn(|c| {
                    texel[c]
                        * (0..3)
                            .map(|i| colors[i][c] as f32 * weights[i])
                            .sum::<f32>()
                        / 255.0
                });
                canvas.pixel(x, y, color);
            }
            for i in 0..3 {
                values[i] += step_x[i];
            }
        }
        for i in 0..3 {
            row[i] += step_y[i];
        }
    }
    Ok(())
}
