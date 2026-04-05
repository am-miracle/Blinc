// Mesh rendering shader — PBR with directional light and optional texture
//
// Vertex format: position[3], normal[3], uv[2], color[4]

struct Uniforms {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    camera_pos: vec3<f32>,
    _pad: f32,
    light_dir: vec3<f32>,
    light_intensity: f32,
    viewport_size: vec2<f32>,
    has_texture: f32,     // > 0.5 means base color texture is bound
    _pad2: f32,
}

struct MaterialUniforms {
    base_color: vec4<f32>,
    metallic_roughness: vec2<f32>,
    emissive: vec3<f32>,
    unlit: f32,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

@group(0) @binding(1)
var<uniform> material: MaterialUniforms;

@group(0) @binding(2)
var base_texture: texture_2d<f32>;

@group(0) @binding(3)
var base_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) world_pos: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) vertex_color: vec4<f32>,
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let world_pos = uniforms.model * vec4(input.position, 1.0);
    out.clip_position = uniforms.view_proj * world_pos;
    out.world_pos = world_pos.xyz;

    let normal_mat = mat3x3(
        uniforms.model[0].xyz,
        uniforms.model[1].xyz,
        uniforms.model[2].xyz,
    );
    out.world_normal = normalize(normal_mat * input.normal);
    out.uv = input.uv;
    out.vertex_color = input.color;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // Sample base color from texture if available, otherwise use material color
    var base_color = material.base_color * input.vertex_color;
    if uniforms.has_texture > 0.5 {
        let tex_color = textureSample(base_texture, base_sampler, input.uv);
        base_color = tex_color * input.vertex_color;
    }

    if material.unlit > 0.5 {
        return base_color;
    }

    // PBR shading
    let N = normalize(input.world_normal);
    let L = normalize(-uniforms.light_dir);
    let V = normalize(uniforms.camera_pos - input.world_pos);
    let H = normalize(L + V);

    // Diffuse (Lambertian)
    let NdotL = max(dot(N, L), 0.0);
    let diffuse = base_color.rgb * NdotL * uniforms.light_intensity;

    // Specular (Blinn-Phong approximation)
    let NdotH = max(dot(N, H), 0.0);
    let roughness = max(material.metallic_roughness.y, 0.04);
    let spec_power = (1.0 - roughness) * 128.0;
    let specular = pow(NdotH, spec_power) * uniforms.light_intensity * material.metallic_roughness.x;

    // Fresnel (Schlick approximation)
    let F0 = mix(vec3(0.04), base_color.rgb, material.metallic_roughness.x);
    let VdotH = max(dot(V, H), 0.0);
    let fresnel = F0 + (vec3(1.0) - F0) * pow(1.0 - VdotH, 5.0);

    // Ambient
    let ambient = base_color.rgb * 0.15;

    // Emissive
    let emissive = material.emissive;

    let final_color = ambient + diffuse + fresnel * specular + emissive;
    return vec4(final_color, base_color.a);
}
