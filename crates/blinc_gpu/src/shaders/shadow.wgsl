// Shadow depth pass — renders depth only from light's perspective
//
// Used to generate a shadow map for directional/spot lights.
// Only the vertex shader runs; depth is written automatically.

struct ShadowUniforms {
    light_view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
}

@group(0) @binding(0)
var<uniform> shadow_uniforms: ShadowUniforms;

struct VertexInput {
    @location(0) position: vec3<f32>,
}

@vertex
fn vs_main(input: VertexInput) -> @builtin(position) vec4<f32> {
    let world_pos = shadow_uniforms.model * vec4(input.position, 1.0);
    return shadow_uniforms.light_view_proj * world_pos;
}
