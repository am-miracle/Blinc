// ============================================================================
// Blinc SDF Text Shader — Data Texture Variant (WebGL2)
// ============================================================================
// Replaces storage buffer glyph data with textureLoad from Rgba32Float texture.
// Supports grayscale text and color emoji via separate atlases.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) world_pos: vec2<f32>,
    @location(3) @interpolate(flat) clip_bounds: vec4<f32>,
    @location(4) @interpolate(flat) clip_fade: vec4<f32>,
    @location(5) @interpolate(flat) is_color: f32,
}

struct TextUniforms {
    viewport_size: vec2<f32>,
    _padding: vec2<f32>,
}

struct GlyphInstance {
    bounds: vec4<f32>,
    uv_bounds: vec4<f32>,
    color: vec4<f32>,
    clip_bounds: vec4<f32>,
    clip_fade: vec4<f32>,
    flags: vec4<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: TextUniforms;
// Glyph data packed into an RGBA32F texture (width=6, height=max_glyphs)
@group(0) @binding(1) var glyph_data: texture_2d<f32>;
@group(0) @binding(2) var glyph_atlas: texture_2d<f32>;
@group(0) @binding(3) var glyph_sampler: sampler;
@group(0) @binding(4) var color_atlas: texture_2d<f32>;

// Load a glyph from the data texture.
// Layout: width = 6 texels (one vec4 per field), height = max_glyphs.
fn load_glyph(index: u32) -> GlyphInstance {
    var g: GlyphInstance;
    let y = i32(index);
    g.bounds = textureLoad(glyph_data, vec2<i32>(0, y), 0);
    g.uv_bounds = textureLoad(glyph_data, vec2<i32>(1, y), 0);
    g.color = textureLoad(glyph_data, vec2<i32>(2, y), 0);
    g.clip_bounds = textureLoad(glyph_data, vec2<i32>(3, y), 0);
    g.clip_fade = textureLoad(glyph_data, vec2<i32>(4, y), 0);
    g.flags = textureLoad(glyph_data, vec2<i32>(5, y), 0);
    return g;
}

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> VertexOutput {
    var out: VertexOutput;

    let glyph = load_glyph(instance_index);

    // Generate quad vertices
    // PowerVR Vulkan codegen bug workaround — see SDF_SHADER vs_main
    var local_uv: vec2<f32>;
    switch vertex_index {
        case 0u: { local_uv = vec2<f32>(0.0, 0.0); } // 0 - top-left
        case 1u: { local_uv = vec2<f32>(1.0, 0.0); } // 1 - top-right
        case 2u: { local_uv = vec2<f32>(0.0, 1.0); } // 3 - bottom-left
        case 3u: { local_uv = vec2<f32>(1.0, 0.0); } // 1 - top-right
        case 4u: { local_uv = vec2<f32>(1.0, 1.0); } // 2 - bottom-right
        default: { local_uv = vec2<f32>(0.0, 1.0); } // 3 - bottom-left
    }

    // Position in screen space
    let pos = vec2<f32>(
        glyph.bounds.x + local_uv.x * glyph.bounds.z,
        glyph.bounds.y + local_uv.y * glyph.bounds.w
    );

    // UV in atlas
    let uv = vec2<f32>(
        glyph.uv_bounds.x + local_uv.x * (glyph.uv_bounds.z - glyph.uv_bounds.x),
        glyph.uv_bounds.y + local_uv.y * (glyph.uv_bounds.w - glyph.uv_bounds.y)
    );

    // Convert to clip space
    let clip_pos = vec2<f32>(
        (pos.x / uniforms.viewport_size.x) * 2.0 - 1.0,
        1.0 - (pos.y / uniforms.viewport_size.y) * 2.0
    );

    out.position = vec4<f32>(clip_pos, 0.0, 1.0);
    out.uv = uv;
    out.color = glyph.color;
    out.world_pos = pos;
    out.clip_bounds = glyph.clip_bounds;
    out.clip_fade = glyph.clip_fade;
    out.is_color = glyph.flags.x;

    return out;
}

// Calculate clip alpha for rectangular clip region
fn calculate_clip_alpha(p: vec2<f32>, clip_bounds: vec4<f32>, clip_fade: vec4<f32>) -> f32 {
    if clip_bounds.x < -5000.0 {
        return 1.0;
    }

    let clip_min = clip_bounds.xy;
    let clip_max = clip_bounds.xy + clip_bounds.zw;

    let d_left = p.x - clip_min.x;
    let d_right = clip_max.x - p.x;
    let d_top = p.y - clip_min.y;
    let d_bottom = clip_max.y - p.y;

    let d = min(min(d_left, d_right), min(d_top, d_bottom));

    var alpha = clamp(d + 0.5, 0.0, 1.0);

    if clip_fade.x > 0.0 { alpha *= saturate(d_top / clip_fade.x); }
    if clip_fade.y > 0.0 { alpha *= saturate(d_right / clip_fade.y); }
    if clip_fade.z > 0.0 { alpha *= saturate(d_bottom / clip_fade.z); }
    if clip_fade.w > 0.0 { alpha *= saturate(d_left / clip_fade.w); }

    return alpha;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let clip_alpha = calculate_clip_alpha(in.world_pos, in.clip_bounds, in.clip_fade);
    if clip_alpha < 0.001 {
        discard;
    }

    if in.is_color > 0.5 {
        let emoji_color = textureSampleLevel(color_atlas, glyph_sampler, in.uv, 0.0);
        return vec4<f32>(emoji_color.rgb, emoji_color.a * clip_alpha);
    } else {
        let coverage = textureSampleLevel(glyph_atlas, glyph_sampler, in.uv, 0.0).r;
        let aa_alpha = pow(coverage, 0.7);
        return vec4<f32>(in.color.rgb, in.color.a * aa_alpha * clip_alpha);
    }
}
