// Fullscreen ACES filmic tonemapping pass.
//
// Reads from an Rgba16Float HDR texture and writes tonemapped + gamma-
// corrected sRGB to the framebuffer. The vertex shader generates a
// fullscreen triangle from `vertex_index` — no vertex buffer needed.

@group(0) @binding(0) var hdr_texture: texture_2d<f32>;
@group(0) @binding(1) var hdr_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    // Fullscreen triangle: 3 vertices cover the entire clip space.
    // UV coordinates map to [0,1]² over the visible area.
    var out: VertexOutput;
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    out.position = vec4(x, y, 0.0, 1.0);
    out.uv = vec2((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

// ACES filmic tone mapping (Narkowicz 2015 fit).
// Maps [0, ∞) → [0, ~1.0] with a pleasant shoulder roll-off that
// preserves color hue better than Reinhard.
fn aces_filmic(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3(0.0), vec3(1.0));
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let hdr = textureSample(hdr_texture, hdr_sampler, input.uv).rgb;
    let mapped = aces_filmic(hdr);
    // No manual sRGB gamma here — the framebuffer is `Bgra8UnormSrgb`
    // (or equivalent `*Srgb` format), so the GPU applies the linear →
    // sRGB transfer function automatically on write. Applying pow(1/2.2)
    // on top of that would double-gamma and wash out the image.
    return vec4(mapped, 1.0);
}
