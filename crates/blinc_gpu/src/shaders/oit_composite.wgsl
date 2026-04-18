// Weighted Blended OIT (McGuire & Bavoil 2013) composite pass.
//
// Reads the two OIT intermediate targets written by `fs_main_oit` in
// mesh.wgsl and blends the result over the existing HDR content:
//
//   accum  (Rgba16Float) — sum of (premultiplied_color * weight_i,
//                                  alpha_i * weight_i) across all
//                          overlapping BLEND fragments
//   reveal (R8Unorm)     — product(1 - alpha_i); the "background
//                          transmission" factor (1 = fully visible,
//                          0 = fully occluded by BLEND stack)
//
// Composite formula:
//
//   avg_color = accum.rgb / max(accum.a, 1e-4)
//   coverage  = 1 - reveal.r
//
// Output (color: avg_color, alpha: coverage) paired with a
// SRC_ALPHA / ONE_MINUS_SRC_ALPHA blend on the color attachment
// composites as:
//
//   final = avg_color * coverage + hdr * (1 - coverage)
//         = avg_color * (1 - reveal) + hdr * reveal
//
// which is the canonical WBOIT over-operator.

@group(0) @binding(0) var accum_tex: texture_2d<f32>;
@group(0) @binding(1) var reveal_tex: texture_2d<f32>;
@group(0) @binding(2) var composite_sampler: sampler;

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
    let accum = textureSample(accum_tex, composite_sampler, input.uv);
    let reveal = textureSample(reveal_tex, composite_sampler, input.uv).r;

    // Empty accumulator (no BLEND fragment covered this pixel) — skip
    // the composite entirely so we don't alpha-blend a zero color
    // over the HDR. `reveal` starts at 1.0 (cleared) and is only
    // reduced by fragments that passed the alpha test. coverage ≈ 0
    // means no layers contributed.
    let coverage = 1.0 - reveal;
    if coverage < 1e-4 {
        discard;
    }

    let avg_color = accum.rgb / max(accum.a, 1e-4);
    return vec4(avg_color, coverage);
}
