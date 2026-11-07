struct Uniforms {
    viewport: vec2<f32>,
    cell: vec2<f32>,
    atlas: vec2<f32>,
    slots_per_row: u32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct Cell {
    @location(0) place: vec4<u32>,
    @location(1) bg: vec4<f32>,
    @location(2) fg: vec4<f32>,
}

struct Vertex {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) bg: vec4<f32>,
    @location(2) fg: vec4<f32>,
}

@vertex
fn vs(@builtin(vertex_index) index: u32, cell: Cell) -> Vertex {
    let right = index == 1u || index == 3u || index == 4u;
    let bottom = index == 2u || index == 4u || index == 5u;
    let corner = vec2<f32>(f32(right), f32(bottom));
    let origin = vec2<f32>(f32(cell.place.x), f32(cell.place.y));
    let px = (origin + corner) * uniforms.cell;
    let slot = cell.place.z;
    let slot_origin = vec2<f32>(
        f32(slot % uniforms.slots_per_row),
        f32(slot / uniforms.slots_per_row),
    );
    var out: Vertex;
    out.position = vec4<f32>(
        px.x / uniforms.viewport.x * 2.0 - 1.0,
        1.0 - px.y / uniforms.viewport.y * 2.0,
        0.0,
        1.0,
    );
    out.uv = (slot_origin + corner) * uniforms.cell / uniforms.atlas;
    out.bg = cell.bg;
    out.fg = cell.fg;
    return out;
}

@fragment
fn fs(vertex: Vertex) -> @location(0) vec4<f32> {
    let alpha = textureSample(atlas, atlas_sampler, vertex.uv).r;
    return mix(vertex.bg, vertex.fg, alpha);
}
