// PBR mesh shader — Cook-Torrance BRDF with metallic/roughness/emissive/AO
// texture sampling, shadow mapping, normal mapping, parallax displacement,
// skeletal skinning.
//
// Vertex format: position[3], normal[3], uv[2], color[4], tangent[4], joints[4], weights[4]
//
// The BRDF implementation follows glTF 2.0's metallic-roughness workflow:
//   - D  = Trowbridge-Reitz (GGX) normal distribution function
//   - G  = Smith's method with Schlick-GGX geometry term
//   - F  = Schlick's Fresnel approximation
//   - kd = (1 - F) * (1 - metallic) for energy-conserving diffuse
//
// Per-texel metallic/roughness/emissive/AO samples are multiplied against
// the scalar factors from `MaterialUniforms`. When a texture is absent the
// caller binds a 1×1 white default so the multiplication is identity —
// the shader never branches on `has_*` flags for the texture samples
// themselves, only for the scalar override fallback.

struct Uniforms {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    light_view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    _pad: f32,
    // Up to 4 directional lights. `light_count` tells the fragment
    // shader how many slots are active; inactive slots are zeroed
    // but the loop skips them via the bound. Shadow pass uses
    // `lights[0]` (the key) only; fill / rim lights don't cast.
    //
    // Layout per light (std140 packing):
    //   direction: vec3<f32>  (16-byte aligned)
    //   intensity: f32        (fills the vec4 tail)
    //   color:     vec3<f32>
    //   _pad:      f32
    // 32 bytes per light × 4 = 128 bytes total.
    lights: array<DirLight, 4>,
    light_count: u32,
    _pad_lc0: f32,
    _pad_lc1: f32,
    _pad_lc2: f32,
    viewport_size: vec2<f32>,
    has_texture: f32,
    has_normal_map: f32,
    shadow_enabled: f32,
    displacement_scale: f32,
    normal_scale: f32,
    has_skinning: f32,
    // Morph-target count for this mesh. Zero means "no morphs" and
    // the vertex-stage loop runs zero iterations, so the default
    // zero-sized dummy buffers bound at bindings 14/15 are never
    // actually sampled. Non-zero means bindings 14/15 hold valid
    // morph data; `morph_vertex_count` is the base-mesh vertex
    // count, needed to index into the flattened delta array.
    morph_target_count: u32,
    morph_vertex_count: u32,
    _pad_morph0: u32,
    _pad_morph1: u32,
}

struct DirLight {
    direction: vec3<f32>,
    intensity: f32,
    color: vec3<f32>,
    _pad: f32,
}

struct MaterialUniforms {
    base_color: vec4<f32>,
    metallic_roughness: vec2<f32>,
    emissive: vec3<f32>,
    unlit: f32,
    // Texture presence flags: 1.0 = sample the texture and multiply,
    // 0.0 = skip the sample (the bound texture is a 1×1 default).
    has_metallic_roughness_texture: f32,
    has_emissive_texture: f32,
    has_occlusion_texture: f32,
    occlusion_strength: f32,
    // glTF alphaMode encoded as a float: 0 = Opaque, 1 = Mask, 2 = Blend.
    // Opaque/Mask force the output alpha to 1.0 so the pipeline's
    // `ALPHA_BLENDING` state multiplies src.rgb by 1, writing the fully
    // lit color. Blend passes the sampled alpha through. Without this
    // branch, assets that mark opaque paint materials as `BLEND` (or
    // simply leave an unused alpha channel in the base-color texture)
    // render attenuated against the dark HDR intermediate.
    alpha_mode: f32,
    // Mask-mode alpha cutoff (glTF default 0.5). Sampled-alpha
    // below this value gets `discard`ed. Ignored when alpha_mode
    // isn't Mask.
    alpha_cutoff: f32,
    _pad1: f32,
    _pad2: f32,
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
@group(0) @binding(9) var metallic_roughness_texture: texture_2d<f32>;
@group(0) @binding(10) var emissive_texture: texture_2d<f32>;
@group(0) @binding(11) var occlusion_texture: texture_2d<f32>;
@group(0) @binding(12) var env_cubemap: texture_cube<f32>;
@group(0) @binding(13) var env_sampler: sampler;
// Morph-target data. Interleaved per (target, vertex): two vec4s per
// entry — `[pos_delta.xyz, 0]` then `[nrm_delta.xyz, 0]`. Flattened
// as `(target_idx * morph_vertex_count + vertex_idx) * 2` → pos,
// `+ 1` → normal. Targets without authored normals carry zero
// normal deltas (the blinc_gltf parser fills them in per-target).
@group(0) @binding(14) var<storage, read> morph_deltas: array<vec4<f32>>;
// Per-frame morph weights; one float per target. Callers update it
// from `Pose::morph_weights_for_node`.
@group(0) @binding(15) var<storage, read> morph_weights: array<f32>;

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
fn vs_main(input: VertexInput, @builtin(vertex_index) vertex_idx: u32) -> VertexOutput {
    var out: VertexOutput;

    // Start with base vertex attributes.
    var pos_local = input.position;
    var nrm_local = input.normal;

    // Morph-target blend — glTF convention is that morph deltas apply
    // against the *rest* pose, before skinning. `morph_target_count`
    // is zero for meshes without morph data, so the loop is a no-op
    // and the bound dummy buffers at bindings 14/15 are never
    // sampled. Deltas are interleaved: two vec4 slots per
    // (target, vertex) — position delta then normal delta.
    for (var t: u32 = 0u; t < uniforms.morph_target_count; t = t + 1u) {
        let w = morph_weights[t];
        if (w == 0.0) {
            continue;
        }
        let base = (t * uniforms.morph_vertex_count + vertex_idx) * 2u;
        let dp = morph_deltas[base];
        let dn = morph_deltas[base + 1u];
        pos_local = pos_local + w * dp.xyz;
        nrm_local = nrm_local + w * dn.xyz;
    }
    // Re-normalise the morphed normal — accumulated linear deltas
    // don't preserve unit length. Tangent would need the same
    // treatment if morph-target tangent deltas were fed in; the
    // blinc_gltf parser ignores those slots for now, so we leave
    // the base tangent untouched.
    if (uniforms.morph_target_count > 0u) {
        nrm_local = normalize(nrm_local);
    }

    var position = vec4(pos_local, 1.0);
    var normal = vec4(nrm_local, 0.0);
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
    // 4-tap PCF for soft shadow edges. Uses textureSampleCompareLevel
    // (explicit LOD 0) instead of textureSampleCompare because this
    // function is called from non-uniform control flow (the shadow_uv
    // bounds check in fs_main). Chrome's WebGPU validator rejects
    // implicit-LOD texture sampling in non-uniform flow.
    let texel_size = 1.0 / 2048.0;
    var shadow = 0.0;
    let offsets = array<vec2<f32>, 4>(
        vec2(-texel_size, -texel_size),
        vec2( texel_size, -texel_size),
        vec2(-texel_size,  texel_size),
        vec2( texel_size,  texel_size),
    );
    for (var i = 0u; i < 4u; i++) {
        shadow += textureSampleCompareLevel(
            shadow_map, shadow_sampler,
            shadow_coord.xy + offsets[i],
            shadow_coord.z - 0.002
        );
    }
    return shadow * 0.25;
}

// ─── Cook-Torrance BRDF components ───────────────────────────────────────

/// GGX / Trowbridge-Reitz normal distribution function. Describes how
/// microfacet normals cluster around the macro-surface normal at a
/// given roughness. `roughness = 0` → delta spike (mirror);
/// `roughness = 1` → hemispherical lobe (matte).
fn d_ggx(n_dot_h: f32, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let n_dot_h2 = n_dot_h * n_dot_h;
    let denom = n_dot_h2 * (a2 - 1.0) + 1.0;
    return a2 / (3.14159265 * denom * denom);
}

/// Schlick-GGX geometry term for a single direction. Combined with
/// itself for `G = G1(V) * G1(L)` (Smith's method, height-uncorrelated).
fn g_schlick_ggx(n_dot_v: f32, roughness: f32) -> f32 {
    // Direct-lighting remap (Disney): k = (r + 1)^2 / 8
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    return n_dot_v / (n_dot_v * (1.0 - k) + k);
}

/// Smith's method geometry term combining view and light attenuation.
fn g_smith(n_dot_v: f32, n_dot_l: f32, roughness: f32) -> f32 {
    return g_schlick_ggx(n_dot_v, roughness) * g_schlick_ggx(n_dot_l, roughness);
}

/// Schlick's Fresnel approximation. `F0` is the base reflectance at
/// normal incidence — `vec3(0.04)` for dielectrics, `base_color` for
/// pure metals. Returns the reflectance at the given half-vector angle.
fn f_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (vec3(1.0) - f0) * pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
}

// ─── Parallax occlusion mapping ──────────────────────────────────────────

fn parallax_mapping(uv: vec2<f32>, view_dir_ts: vec3<f32>, scale: f32) -> vec2<f32> {
    let num_layers = 16.0;
    let layer_depth = 1.0 / num_layers;
    let delta_uv = view_dir_ts.xy * scale / (view_dir_ts.z * num_layers);

    var current_uv = uv;
    var current_depth = 0.0;
    // Use textureSampleLevel (explicit LOD 0) instead of textureSample
    // inside the loop — textureSample requires implicit derivatives
    // which aren't available in non-uniform control flow (the break
    // condition depends on the texture value). WebGPU validators
    // reject textureSample here; native drivers silently accept it
    // but produce undefined LOD selection.
    var current_height = textureSampleLevel(displacement_texture, base_sampler, current_uv, 0.0).r;

    for (var i = 0u; i < 16u; i++) {
        if current_depth >= current_height {
            break;
        }
        current_uv -= delta_uv;
        current_height = textureSampleLevel(displacement_texture, base_sampler, current_uv, 0.0).r;
        current_depth += layer_depth;
    }

    // Interpolate between last two layers for smoother result
    let prev_uv = current_uv + delta_uv;
    let after_depth = current_height - current_depth;
    let before_depth = textureSampleLevel(displacement_texture, base_sampler, prev_uv, 0.0).r
                       - current_depth + layer_depth;
    let weight = after_depth / (after_depth - before_depth);
    return mix(current_uv, prev_uv, weight);
}

// Result of the shared shading body. `rgb_premult` is the
// premultiplied-alpha color (reflected * alpha + emissive); `alpha`
// is the surface alpha after the mask/opaque branch. `emissive_rgb`
// is returned separately so the OIT path can decide how to weight
// emissive vs reflected (keeping emissive un-attenuated through
// transparent layers).
struct ShadedFragment {
    rgb_premult: vec3<f32>,
    alpha: f32,
    emissive_rgb: vec3<f32>,
    /// Raw unlit base color when `material.unlit > 0.5`. Only the
    /// unlit branch consumes it — lit materials leave it unused.
    unlit_rgb: vec3<f32>,
    /// Set to 1.0 when the material is unlit; callers should use
    /// `unlit_rgb` instead of `rgb_premult` in that case.
    is_unlit: f32,
}

// Shared lighting body. Called by both `fs_main` (opaque/mask/blend
// direct-write path) and `fs_main_oit` (weighted blended OIT path).
// Any `discard` inside here propagates out of both entry points.
fn shade(input: VertexOutput) -> ShadedFragment {
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

    // Sample base color. We multiply the material base_color factor
    // by the sampled texel (when a texture is bound) AND the
    // interpolated vertex color — this matches the glTF spec and
    // `KHR_materials_unlit` expectations.
    var base_color = material.base_color * input.vertex_color;
    if uniforms.has_texture > 0.5 {
        let tex_color = textureSample(base_texture, base_sampler, uv);
        base_color = base_color * tex_color;
    }

    // Alpha-mode branch: Mask discards below cutoff (glTF default 0.5),
    // Opaque/Mask force alpha to 1, Blend passes sampled alpha through.
    // Kept out of the unlit early-return so unlit materials also get
    // the right alpha treatment.
    if material.alpha_mode < 0.5 {
        base_color.a = 1.0;                                   // Opaque
    } else if material.alpha_mode < 1.5 {
        if base_color.a < material.alpha_cutoff { discard; }  // Mask
        base_color.a = 1.0;
    }
    // else: Blend — leave base_color.a as sampled.

    if material.unlit > 0.5 {
        var out: ShadedFragment;
        out.rgb_premult = base_color.rgb * base_color.a;
        out.alpha = base_color.a;
        out.emissive_rgb = vec3<f32>(0.0);
        out.unlit_rgb = base_color.rgb;
        out.is_unlit = 1.0;
        return out;
    }

    // Normal mapping
    var shading_normal = N;
    if uniforms.has_normal_map > 0.5 {
        // Sample RG only and reconstruct Z in-shader. Works
        // identically for RGBA8 normal maps (B channel is
        // dropped — for a unit-length tangent-space normal the
        // stored B equals the reconstruction) and BC5RgUnorm
        // normal maps (B is implementation-defined; must be
        // reconstructed). `max(..., 0)` guards against tiny
        // negative values from round-off, which sqrt would NaN on.
        let nm_rg = textureSample(normal_texture, base_sampler, uv).rg;
        let rg = nm_rg * 2.0 - 1.0;
        let b = sqrt(max(1.0 - dot(rg, rg), 0.0));
        var tangent_normal = vec3<f32>(rg.x, rg.y, b);
        tangent_normal.x *= uniforms.normal_scale;
        tangent_normal.y *= uniforms.normal_scale;
        shading_normal = normalize(TBN * tangent_normal);
    }

    // ─── Sample per-texel PBR inputs ─────────────────────────────────
    //
    // glTF 2.0 metallic-roughness convention:
    //   metallicRoughnessTexture.b = metallic
    //   metallicRoughnessTexture.g = roughness
    //   occlusionTexture.r         = AO
    //
    // The scalar factors from `MaterialUniforms` multiply the per-texel
    // samples, so `metallic: 1.0, roughness: 1.0` preserves the texture
    // values unchanged while smaller scalars attenuate them globally.
    // When `has_*` is 0.0 the caller has bound a 1×1 white default so
    // the multiply is effectively a no-op and we skip the
    // `textureSample` call for clarity.
    var metallic = material.metallic_roughness.x;
    var roughness = material.metallic_roughness.y;
    if material.has_metallic_roughness_texture > 0.5 {
        let mr = textureSample(metallic_roughness_texture, base_sampler, uv);
        metallic = metallic * mr.b;
        roughness = roughness * mr.g;
    }
    // Clamp roughness away from 0 — GGX's 1/α² term explodes at
    // zero, producing NaNs and blown-out highlights. 0.04 matches the
    // glTF spec's minimum perceptual roughness.
    roughness = clamp(roughness, 0.04, 1.0);

    var emissive_value = material.emissive;
    if material.has_emissive_texture > 0.5 {
        let em = textureSample(emissive_texture, base_sampler, uv).rgb;
        emissive_value = emissive_value * em;
    }

    var ao = 1.0;
    if material.has_occlusion_texture > 0.5 {
        let sampled_ao = textureSample(occlusion_texture, base_sampler, uv).r;
        // glTF semantic: `color = mix(color, color * ao, strength)` — a
        // strength of 1 fully applies AO, 0 bypasses it.
        ao = mix(1.0, sampled_ao, material.occlusion_strength);
    }

    // ─── Cook-Torrance BRDF ──────────────────────────────────────────
    //
    // Iterate over all active directional lights. `kd` is material-
    // dependent and `f0` depends only on metallic/base_color, so we
    // compute them once outside the loop. Per-light we recompute the
    // half-vector + NdotL/NdotH/VdotH and the micro-facet BRDF terms.
    //
    // `Nsh` is the shading normal (post-normal-map); `N` earlier in
    // the function is the raw geometric normal used to build the TBN
    // matrix. Use a distinct name here to avoid shadowing that one.
    let Nsh = shading_normal;
    let V = normalize(uniforms.camera_pos - input.world_pos);
    let n_dot_v = max(dot(Nsh, V), 0.0001);

    // Base reflectance: 4% for dielectrics, full base color for metals.
    // glTF convention: F0 = mix(0.04, baseColor, metallic).
    let f0 = mix(vec3(0.04), base_color.rgb, metallic);

    var direct_lighting = vec3(0.0);
    // Cap the loop at the array bound so WGSL / naga can statically
    // bound it; `light_count` is guaranteed ≤ array size by the CPU
    // side, so the extra iterations are skipped via the `break`.
    for (var li: u32 = 0u; li < 4u; li = li + 1u) {
        if li >= uniforms.light_count { break; }
        let light = uniforms.lights[li];
        let L = normalize(-light.direction);
        let H = normalize(L + V);
        let n_dot_l = max(dot(Nsh, L), 0.0);
        if n_dot_l <= 0.0 { continue; }
        let n_dot_h = max(dot(Nsh, H), 0.0);
        let v_dot_h = max(dot(V, H), 0.0);

        // Cook-Torrance: D * G * F
        let d = d_ggx(n_dot_h, roughness);
        let g = g_smith(n_dot_v, n_dot_l, roughness);
        let f = f_schlick(v_dot_h, f0);

        // Specular BRDF: (D * G * F) / (4 * NdotL * NdotV).
        let specular_brdf = (d * g * f) / (4.0 * n_dot_l * n_dot_v + 0.0001);

        // Diffuse BRDF: energy-conserving Lambertian, zero for metals.
        let kd = (vec3(1.0) - f) * (1.0 - metallic);
        let diffuse_brdf = kd * base_color.rgb / 3.14159265;

        direct_lighting = direct_lighting
            + (diffuse_brdf + specular_brdf) * light.color * light.intensity * n_dot_l;
    }

    // ─── Shadow ──────────────────────────────────────────────────────
    var shadow_factor = 1.0;
    if uniforms.shadow_enabled > 0.5 {
        let shadow_ndc = input.shadow_pos.xyz / input.shadow_pos.w;
        let shadow_uv = vec3(
            shadow_ndc.x * 0.5 + 0.5,
            -shadow_ndc.y * 0.5 + 0.5,
            shadow_ndc.z,
        );
        if shadow_uv.x >= 0.0 && shadow_uv.x <= 1.0 &&
           shadow_uv.y >= 0.0 && shadow_uv.y <= 1.0 &&
           shadow_uv.z >= 0.0 && shadow_uv.z <= 1.0 {
            shadow_factor = sample_shadow(shadow_uv);
        }
    }

    // ─── IBL ambient (environment cubemap reflection) ──────────────
    //
    // Sample the procedural environment cubemap for both diffuse and
    // specular indirect lighting. The cubemap has mipmaps generated at
    // decreasing resolution; sampling at `roughness * max_mip` blurs
    // the reflection proportionally — smooth glass gets a sharp
    // horizon reflection, rough metal gets a soft ambient tint.
    //
    // Diffuse: sample at the shading normal direction at the highest
    // mip (fully blurred = irradiance-like) and attenuate by kd.
    //
    // Specular: sample at the reflection vector at a mip proportional
    // to roughness, weighted by Fresnel.
    let max_mip = 7.0;
    let R = reflect(-V, Nsh);
    let irradiance = textureSampleLevel(env_cubemap, env_sampler, Nsh, max_mip).rgb;
    let prefiltered = textureSampleLevel(env_cubemap, env_sampler, R, roughness * max_mip).rgb;
    let env_fresnel = f_schlick(n_dot_v, f0);

    // For IBL, `kd` is computed against the view direction (not a
    // specific light dir) — the per-light loop's `kd` is out of
    // scope, so derive a view-space one here. Same formula:
    // `kd = (1 - F_view) * (1 - metallic)`.
    let kd_ambient = (vec3(1.0) - env_fresnel) * (1.0 - metallic);
    let ambient_diffuse = kd_ambient * base_color.rgb * irradiance;
    let ambient_specular = prefiltered * env_fresnel;
    let ambient = (ambient_diffuse + ambient_specular) * ao;

    // Premultiplied-alpha output. Surface-reflected light (ambient +
    // direct) is attenuated by the material's sampled alpha because a
    // transparent surface lets you see *through* it; but emissive is
    // self-emitted light — a glowing pixel visible regardless of how
    // transparent the surrounding material is. Separating the two
    // terms here + premultiplied-alpha blending on the pipeline side
    // means a BLEND material with a low-alpha base color (e.g. the
    // body decal mask) can still show its bright emissive eye /
    // accent glows.
    let reflected = ambient + direct_lighting * shadow_factor;
    var out: ShadedFragment;
    out.rgb_premult = reflected * base_color.a + emissive_value;
    out.alpha = base_color.a;
    out.emissive_rgb = emissive_value;
    out.unlit_rgb = vec3<f32>(0.0);
    out.is_unlit = 0.0;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let s = shade(input);
    if s.is_unlit > 0.5 {
        return vec4(s.unlit_rgb, s.alpha);
    }
    return vec4(s.rgb_premult, s.alpha);
}

// Weighted Blended OIT (McGuire & Bavoil 2013) fragment entry.
//
// Writes two MRTs:
//
//   location 0 — accum (Rgba16Float):
//       rgb = premultiplied_color * weight
//       a   = alpha               * weight
//
//   location 1 — reveal (R8Unorm):
//       r   = alpha   (combined with a ZERO / OneMinusSrc blend to
//                      produce product(1 - alpha_i) across all
//                      overlapping BLEND fragments)
//
// Paired with `oit_composite.wgsl` which divides out the accumulated
// weight and blends the result over the opaque HDR using the reveal
// term as "background transmission".
//
// Depth test is on (so opaque geometry in front occludes BLEND
// fragments correctly) but depth write is off (BLEND fragments don't
// occlude each other — OIT handles ordering statistically).
struct OitOutput {
    @location(0) accum: vec4<f32>,
    @location(1) reveal: vec4<f32>,
}

// McGuire & Bavoil weight (Eq. 10 / 2013 paper), reworked to use
// *linear view-space depth* instead of NDC depth.
//
// NDC depth (`clip_position.z`) is heavily non-linear under a
// standard perspective projection — most of the `[0, 1]` range maps
// to fragments near the far plane. With a cubic weight in NDC z,
// everything past roughly view_z > 3 collapses into the same tiny
// weight, and stacked BLEND layers at grazing view angles (e.g. a
// drone's landing leg seen through a fan's transparent housing)
// mutually annihilate — each layer multiplies reveal by (1 - α) but
// contributes almost nothing to accum, so the composite outputs
// coverage ≈ 1 with a near-zero average colour.
//
// The fragment shader's `clip_position.w` component, per WebGPU
// convention, holds `1 / clip_w` after perspective divide. For a
// standard perspective matrix `clip_w == view_z` (positive, distance
// from camera), so `1.0 / clip_position.w` recovers linear
// view-space depth.
fn oit_weight(alpha: f32, view_z: f32) -> f32 {
    // Normalise by 20 so "typical scene depth" (5-10 metres for a
    // character-scale scene) maps onto the cubic falloff's knee
    // rather than into the clamp floor. Weights at view_z = 1m land
    // near the 3e3 clamp (near-plane fragments dominate), weights at
    // view_z = 5m are around 60, at 10m around 8, at 20m around 1 —
    // a ~3000× spread that preserves ordering without any single
    // layer overwhelming the sum.
    let scaled = view_z / 20.0;
    let w_depth = clamp(1.0 / (1e-5 + pow(scaled, 3.0)), 1e-2, 3e3);
    return clamp(alpha * w_depth, 1e-3, 3e3);
}

@fragment
fn fs_main_oit(input: VertexOutput) -> OitOutput {
    let s = shade(input);

    // Unlit materials bypass the premultiplied convention used by
    // the lit path — shade() stores the raw colour in `unlit_rgb`.
    // For OIT we still feed premultiplied values into accum so the
    // composite divide works identically for both paths.
    var color = s.rgb_premult;
    if s.is_unlit > 0.5 {
        color = s.unlit_rgb * s.alpha;
    }

    // Alpha=0 fragments carry no signal and would only dilute the
    // accumulator (they contribute 0 to color but still bump weight
    // denominators at the clamp floor). Skip them.
    if s.alpha <= 0.001 {
        discard;
    }

    let view_z = 1.0 / max(input.clip_position.w, 1e-6);
    let w = oit_weight(s.alpha, view_z);

    var out: OitOutput;
    out.accum = vec4(color * w, s.alpha * w);
    // reveal uses a `src = 0, dst = 1 - src` blend factor in the
    // pipeline, so writing `alpha` here multiplies the destination
    // by (1 - alpha) — after N fragments, reveal.r = product(1 - α_i).
    out.reveal = vec4(s.alpha, 0.0, 0.0, 0.0);
    return out;
}
