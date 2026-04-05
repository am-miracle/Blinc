// PBR mesh shader — shadow mapping, normal mapping, parallax displacement, skeletal skinning
//
// Vertex format: position[3], normal[3], uv[2], color[4], tangent[4], joints[4], weights[4]

struct Uniforms {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    light_view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    _pad: f32,
    light_dir: vec3<f32>,
    light_intensity: f32,
    viewport_size: vec2<f32>,
    has_texture: f32,
    has_normal_map: f32,
    shadow_enabled: f32,
    displacement_scale: f32,
    normal_scale: f32,
    has_skinning: f32,
}

struct MaterialUniforms {
    base_color: vec4<f32>,
    metallic_roughness: vec2<f32>,
    emissive: vec3<f32>,
    unlit: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var<uniform> material: MaterialUniforms;
@group(0) @binding(2) var base_texture: texture_2d<f32>;
@group(0) @binding(3) var base_sampler: sampler;
@group(0) @binding(4) var normal_texture: texture_2d<f32>;
@group(0) @binding(5) var shadow_map: texture_depth_2d;
@group(0) @binding(6) var shadow_sampler: sampler_comparison;
@group(0) @binding(7) var displacement_texture: texture_2d<f32>;
@group(0) @binding(8) var<storage, read> joint_matrices: array<mat4x4<f32>>;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) tangent: vec4<f32>,
    @location(5) joints: vec4<u32>,
    @location(6) weights: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) world_pos: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) vertex_color: vec4<f32>,
    @location(4) world_tangent: vec3<f32>,
    @location(5) tangent_handedness: f32,
    @location(6) shadow_pos: vec4<f32>,
}

// ─── Skinning ────────────────────────────────────────────────────────────

fn compute_skin_matrix(joints: vec4<u32>, weights: vec4<f32>) -> mat4x4<f32> {
    return joint_matrices[joints.x] * weights.x
         + joint_matrices[joints.y] * weights.y
         + joint_matrices[joints.z] * weights.z
         + joint_matrices[joints.w] * weights.w;
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;

    // Apply skeletal skinning if enabled
    var position = vec4(input.position, 1.0);
    var normal = vec4(input.normal, 0.0);
    var tangent_dir = vec4(input.tangent.xyz, 0.0);

    if uniforms.has_skinning > 0.5 {
        let skin = compute_skin_matrix(input.joints, input.weights);
        position = skin * position;
        normal = skin * normal;
        tangent_dir = skin * tangent_dir;
    }

    let world_pos = uniforms.model * position;
    out.clip_position = uniforms.view_proj * world_pos;
    out.world_pos = world_pos.xyz;

    let normal_mat = mat3x3(
        uniforms.model[0].xyz,
        uniforms.model[1].xyz,
        uniforms.model[2].xyz,
    );
    out.world_normal = normalize(normal_mat * normal.xyz);
    out.world_tangent = normalize(normal_mat * tangent_dir.xyz);
    out.tangent_handedness = input.tangent.w;
    out.uv = input.uv;
    out.vertex_color = input.color;

    // Light-space position for shadow mapping
    out.shadow_pos = uniforms.light_view_proj * world_pos;

    return out;
}

// ─── Shadow sampling with PCF ────────────────────────────────────────────

fn sample_shadow(shadow_coord: vec3<f32>) -> f32 {
    // 4-tap PCF for soft shadow edges
    let texel_size = 1.0 / 2048.0;  // shadow map resolution
    var shadow = 0.0;
    let offsets = array<vec2<f32>, 4>(
        vec2(-texel_size, -texel_size),
        vec2( texel_size, -texel_size),
        vec2(-texel_size,  texel_size),
        vec2( texel_size,  texel_size),
    );
    for (var i = 0u; i < 4u; i++) {
        shadow += textureSampleCompare(
            shadow_map, shadow_sampler,
            shadow_coord.xy + offsets[i],
            shadow_coord.z - 0.002  // bias to reduce shadow acne
        );
    }
    return shadow * 0.25;
}

// ─── Parallax occlusion mapping ──────────────────────────────────────────

fn parallax_mapping(uv: vec2<f32>, view_dir_ts: vec3<f32>, scale: f32) -> vec2<f32> {
    let num_layers = 16.0;
    let layer_depth = 1.0 / num_layers;
    let delta_uv = view_dir_ts.xy * scale / (view_dir_ts.z * num_layers);

    var current_uv = uv;
    var current_depth = 0.0;
    var current_height = textureSample(displacement_texture, base_sampler, current_uv).r;

    for (var i = 0u; i < 16u; i++) {
        if current_depth >= current_height {
            break;
        }
        current_uv -= delta_uv;
        current_height = textureSample(displacement_texture, base_sampler, current_uv).r;
        current_depth += layer_depth;
    }

    // Interpolate between last two layers for smoother result
    let prev_uv = current_uv + delta_uv;
    let after_depth = current_height - current_depth;
    let before_depth = textureSample(displacement_texture, base_sampler, prev_uv).r
                       - current_depth + layer_depth;
    let weight = after_depth / (after_depth - before_depth);
    return mix(current_uv, prev_uv, weight);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    var uv = input.uv;

    // Build TBN matrix for tangent-space transforms
    let N = normalize(input.world_normal);
    let T = normalize(input.world_tangent);
    let B = cross(N, T) * input.tangent_handedness;
    let TBN = mat3x3(T, B, N);
    let TBN_inv = transpose(TBN);  // inverse of orthonormal basis = transpose

    // Parallax displacement
    if uniforms.displacement_scale > 0.0 {
        let V_world = normalize(uniforms.camera_pos - input.world_pos);
        let V_tangent = TBN_inv * V_world;
        uv = parallax_mapping(uv, V_tangent, uniforms.displacement_scale);
        // Discard if UV went out of [0,1] range
        if uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 {
            discard;
        }
    }

    // Sample base color
    var base_color = material.base_color * input.vertex_color;
    if uniforms.has_texture > 0.5 {
        let tex_color = textureSample(base_texture, base_sampler, uv);
        base_color = tex_color * input.vertex_color;
    }

    if material.unlit > 0.5 {
        return base_color;
    }

    // Normal mapping
    var shading_normal = N;
    if uniforms.has_normal_map > 0.5 {
        let nm = textureSample(normal_texture, base_sampler, uv).rgb;
        var tangent_normal = nm * 2.0 - 1.0;
        tangent_normal.x *= uniforms.normal_scale;
        tangent_normal.y *= uniforms.normal_scale;
        shading_normal = normalize(TBN * tangent_normal);
    }

    // PBR shading
    let L = normalize(-uniforms.light_dir);
    let V = normalize(uniforms.camera_pos - input.world_pos);
    let H = normalize(L + V);

    // Diffuse (Lambertian)
    let NdotL = max(dot(shading_normal, L), 0.0);
    let diffuse = base_color.rgb * NdotL * uniforms.light_intensity;

    // Specular (Blinn-Phong approximation)
    let NdotH = max(dot(shading_normal, H), 0.0);
    let roughness = max(material.metallic_roughness.y, 0.04);
    let spec_power = (1.0 - roughness) * 128.0;
    let specular = pow(NdotH, spec_power) * uniforms.light_intensity * material.metallic_roughness.x;

    // Fresnel (Schlick approximation)
    let F0 = mix(vec3(0.04), base_color.rgb, material.metallic_roughness.x);
    let VdotH = max(dot(V, H), 0.0);
    let fresnel = F0 + (vec3(1.0) - F0) * pow(1.0 - VdotH, 5.0);

    // Shadow
    var shadow_factor = 1.0;
    if uniforms.shadow_enabled > 0.5 {
        // Convert shadow_pos from clip space to UV space
        let shadow_ndc = input.shadow_pos.xyz / input.shadow_pos.w;
        let shadow_uv = vec3(
            shadow_ndc.x * 0.5 + 0.5,
            -shadow_ndc.y * 0.5 + 0.5,  // flip Y for texture coordinates
            shadow_ndc.z,
        );
        // Only apply shadow if within shadow map bounds
        if shadow_uv.x >= 0.0 && shadow_uv.x <= 1.0 &&
           shadow_uv.y >= 0.0 && shadow_uv.y <= 1.0 &&
           shadow_uv.z >= 0.0 && shadow_uv.z <= 1.0 {
            shadow_factor = sample_shadow(shadow_uv);
        }
    }

    // Ambient
    let ambient = base_color.rgb * 0.15;

    // Emissive
    let emissive = material.emissive;

    let lit_color = diffuse * shadow_factor + fresnel * specular * shadow_factor;
    let final_color = ambient + lit_color + emissive;
    return vec4(final_color, base_color.a);
}
