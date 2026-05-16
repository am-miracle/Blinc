// Compositor v2 damage-rect scissored clear.
//
// Draws a single full-attachment triangle that writes opaque
// black `(0, 0, 0, 1)` to every pixel inside the active scissor
// rectangle. Used by `render_static_layer_damaged` to reset the
// damaged region of the static cache before the SDF dispatch
// re-paints over it.
//
// Output is `(0, 0, 0, 1)` — opaque, not transparent — to match
// the slow path's `LoadOp::Clear` color (`[0, 0, 0, clear_alpha]`
// with `clear_alpha = 1.0` for the default opaque window). The
// SDF blend mode is non-premultiplied (SrcAlpha,
// OneMinusSrcAlpha) for color and (One, OneMinusSrcAlpha) for
// alpha. With an opaque-black destination, a semi-transparent
// primitive blends to an opaque pixel (`alpha = src.a + dst.a *
// (1 - src.a)` = `src.a + (1 - src.a)` = 1); with a transparent
// destination, the result alpha stays at `src.a` and the surface
// blit shows through. Matching the slow path's clear keeps the
// cache identical between damage-rect frames and full-paint
// frames.
//
// wgpu's `LoadOp::Clear` ignores `set_scissor_rect` (clears the
// whole attachment), so a scissored draw is the only portable
// way to do a region-only clear. Blend state is `REPLACE`
// (configured on the pipeline) so the fragment's output
// overwrites whatever was in the attachment.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    // Standard fullscreen triangle:
    //   vi=0 → (-1, -1)
    //   vi=1 → ( 3, -1)
    //   vi=2 → (-1,  3)
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    out.position = vec4(x, y, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4(0.0, 0.0, 0.0, 1.0);
}
