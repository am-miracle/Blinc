// Fixed screen-space gradient backdrop for 3D scenes.
//
// NOT a sky dome — the gradient is pinned to the screen so it never
// moves with orbit, zoom, or pan. The IBL cubemap (used for mesh
// reflections) is a separate texture that the mesh shader samples
// with camera-dependent reflection vectors; this shader only draws
// the visible background behind the mesh.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    out.position = vec4(x, y, 1.0, 1.0);
    out.uv = vec2((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // Neutral studio gradient: dark edges, subtly brighter center.
    // uv.y = 0 at top, 1 at bottom.
    let y = input.uv.y;

    // Vertical gradient: dark top → slightly brighter mid → dark bottom
    let vert = 1.0 - 4.0 * (y - 0.5) * (y - 0.5); // parabola peaking at y=0.5

    // Horizontal vignette: slightly dimmer at the edges
    let x = input.uv.x;
    let horiz = 1.0 - 2.0 * (x - 0.5) * (x - 0.5);

    let brightness = 0.08 + 0.12 * vert * horiz; // range ~0.08 to 0.20
    let color = vec3(brightness * 0.9, brightness * 0.92, brightness); // subtle cool tint

    return vec4(color, 1.0);
}
