// Bloom — brightness threshold extraction + Kawase blur.
//
// Used in two modes controlled by `uniforms.mode`:
//   mode 0: threshold + downsample — extracts bright pixels from the
//           HDR source, writing only contributions above `threshold`.
//   mode 1: Kawase blur — samples 4 diagonal neighbors + center for
//           a cheap wide blur. Run multiple iterations for wider glow.

struct BloomUniforms {
    texel_size: vec2<f32>,
    threshold: f32,
    mode: f32,
}

@group(0) @binding(0) var<uniform> uniforms: BloomUniforms;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    out.position = vec4(x, y, 0.0, 1.0);
    out.uv = vec2((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let uv = input.uv;

    if uniforms.mode < 0.5 {
        // Mode 0: threshold extraction. Only bright pixels contribute
        // to bloom — everything below the threshold goes to black.
        let color = textureSample(source, samp, uv).rgb;
        let brightness = max(color.r, max(color.g, color.b));
        let soft = clamp(brightness - uniforms.threshold, 0.0, 1.0);
        let contrib = soft / max(brightness, 0.001);
        return vec4(color * contrib, 1.0);
    } else {
        // Mode 1: Kawase blur — 4 diagonal taps + weighted center.
        // Each iteration spreads the glow wider. Two iterations at
        // half-res gives a visually smooth bloom for specular and
        // emissive without the cost of a full Gaussian kernel.
        let ts = uniforms.texel_size;
        var color = textureSample(source, samp, uv).rgb * 4.0;
        color += textureSample(source, samp, uv + vec2(-ts.x, -ts.y)).rgb;
        color += textureSample(source, samp, uv + vec2( ts.x, -ts.y)).rgb;
        color += textureSample(source, samp, uv + vec2(-ts.x,  ts.y)).rgb;
        color += textureSample(source, samp, uv + vec2( ts.x,  ts.y)).rgb;
        return vec4(color / 8.0, 1.0);
    }
}
