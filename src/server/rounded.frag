precision mediump float;
varying vec2 v_coords;
uniform vec2 size;
uniform float alpha;
uniform vec4 fill;
uniform vec4 border;
uniform float radius;
uniform float border_width;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

float distance_to_edge(vec2 point, vec2 half_size, float corner) {
    vec2 q = abs(point) - half_size + corner;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - corner;
}

void main() {
    vec2 point = v_coords * size - size * 0.5;
    float distance = distance_to_edge(point, size * 0.5, radius);
    float outside = clamp(0.5 - distance, 0.0, 1.0);
    float inside = clamp(0.5 - (distance + border_width), 0.0, 1.0);
    vec4 color = mix(border, fill, inside) * outside;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.3, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color * alpha;
}
