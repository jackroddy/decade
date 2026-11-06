struct Rect {
    @location(0) min: vec2<f32>,
    @location(1) max: vec2<f32>,
    @location(2) color: vec4<f32>,
}

struct Vertex {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs(@builtin(vertex_index) index: u32, rect: Rect) -> Vertex {
    let right = index == 1u || index == 3u || index == 4u;
    let top = index == 2u || index == 4u || index == 5u;
    let x = select(rect.min.x, rect.max.x, right);
    let y = select(rect.min.y, rect.max.y, top);
    var out: Vertex;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.color = rect.color;
    return out;
}

@fragment
fn fs(vertex: Vertex) -> @location(0) vec4<f32> {
    return vertex.color;
}
