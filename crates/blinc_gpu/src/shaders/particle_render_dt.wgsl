// ============================================================================
// Blinc GPU Particle Render Shader — Data-Texture (DT) variant
// ============================================================================
//
// Replaces the `particles` storage buffer (binding 0) with a RGBA32F
// texture_2d for WebGL2 compatibility.  Each Particle has 4 vec4 fields =
// 4 texels.  Layout: width=4 (one texel per field), height=max_particles.

struct Particle {
    position_life: vec4<f32>,
    velocity_max_life: vec4<f32>,
    color: vec4<f32>,
    size_rotation: vec4<f32>,
}

struct RenderUniforms {
    view_proj: mat4x4<f32>,
    camera_pos_fov: vec4<f32>,
    camera_right_aspect: vec4<f32>,
    camera_up: vec4<f32>,
    viewport_config: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}

@group(0) @binding(0) var particle_data: texture_2d<f32>;
@group(0) @binding(1) var<uniform> uniforms: RenderUniforms;

fn load_particle(index: u32) -> Particle {
    let y = i32(index);
    var p: Particle;
    p.position_life     = textureLoad(particle_data, vec2<i32>(0, y), 0);
    p.velocity_max_life = textureLoad(particle_data, vec2<i32>(1, y), 0);
    p.color             = textureLoad(particle_data, vec2<i32>(2, y), 0);
    p.size_rotation     = textureLoad(particle_data, vec2<i32>(3, y), 0);
    return p;
}

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> VertexOutput {
    var out: VertexOutput;

    let p = load_particle(instance_index);

    // Skip dead particles (move to clip space far away)
    if (p.position_life.w <= 0.0) {
        out.position = vec4<f32>(0.0, 0.0, 1000.0, 1.0);
        out.uv = vec2<f32>(0.0);
        out.color = vec4<f32>(0.0);
        return out;
    }

    // Billboard quad vertices
    let quad_verts = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
        vec2<f32>(-1.0,  1.0),
    );

    let quad_uvs = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 0.0),
    );

    let local_pos = quad_verts[vertex_index];
    let size = p.size_rotation.x;

    // Calculate billboard orientation
    let camera_right = uniforms.camera_right_aspect.xyz;
    let camera_up = uniforms.camera_up.xyz;

    // World position with billboard offset
    let world_pos = p.position_life.xyz +
                    camera_right * local_pos.x * size +
                    camera_up * local_pos.y * size;

    // Project to clip space
    out.position = uniforms.view_proj * vec4<f32>(world_pos, 1.0);
    out.uv = quad_uvs[vertex_index];
    out.color = p.color;

    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Circular particle with soft edges
    let center = vec2<f32>(0.5);
    let dist = length(in.uv - center) * 2.0;

    // Soft circle falloff
    let alpha = 1.0 - smoothstep(0.8, 1.0, dist);

    // Discard if too far from center
    if (alpha < 0.01) {
        discard;
    }

    return vec4<f32>(in.color.rgb, in.color.a * alpha);
}
