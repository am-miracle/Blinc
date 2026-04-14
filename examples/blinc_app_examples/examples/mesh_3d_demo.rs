//! 3D Mesh Demo — renders the Khronos glTF `DamagedHelmet` sample model
//! through Blinc's mesh pipeline.
//!
//! Demonstrates:
//! - `blinc_canvas_kit::SceneKit3D` — orbit camera + light rig wrapped
//!   around a `canvas` element, with drag/scroll input wired for free.
//! - `DrawContext::draw_mesh_data` — the direct-render mesh path. The
//!   canvas closure just calls `ctx.draw_mesh_data(&mesh, transform)`;
//!   everything behind that (camera capture, pending-mesh queue,
//!   GpuPaintContext → GpuRenderer dispatch, PBR shading) is plumbing.
//! - Inline glTF loading — no external `gltf` crate dep. The sample
//!   model has a fixed layout (single mesh, single primitive, packed
//!   f32 attributes at known bufferView offsets, u16 indices), so
//!   parsing is a handful of offset reads plus a `blinc_image::ImageData`
//!   call for the albedo texture.
//!
//!
//! Run with:
//!
//! ```sh
//! cargo run -p blinc_app_examples --example mesh_3d_demo --features windowed
//! ```
//!
//! Controls: drag the viewport to orbit, scroll to zoom.
//!
//! # Damaged Helmet
//!
//! <https://github.com/KhronosGroup/glTF-Sample-Models/tree/master/2.0/DamagedHelmet>
//!
//! ## License Information
//!
//! Battle Damaged Sci-fi Helmet - PBR by
//! [theblueturtle_](https://sketchfab.com/theblueturtle_), published
//! under a Creative Commons Attribution-NonCommercial license.
//!
//! <https://sketchfab.com/models/b81008d513954189a063ff901f7abfe4>

use std::sync::Arc;

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_canvas_kit::prelude::*;
use blinc_core::{AlphaMode, Color, Light, Mat4, Material, MeshData, TextureData, Vec3, Vertex};

/// Workspace-relative asset paths. Matches the convention other blinc
/// examples use — `cargo run -p blinc_app_examples --example ...` is invoked
/// from the workspace root, so relative paths resolve against the
/// repo root, not `crates/blinc_app/`.
const HELMET_GLTF_DIR: &str = "examples/blinc_app_examples/examples/assets/3d/DamagedHelmet";
const ASSETS_3D_DIR: &str = "examples/blinc_app_examples/examples/assets/3d";

// ── glTF binary layout constants ────────────────────────────────────────
//
// All four values come from `DamagedHelmet.gltf`'s bufferView table.
// The file layout is fixed in the Khronos sample repo and won't drift;
// if you regenerate the mesh from a different exporter you'll need to
// re-read these from the JSON.

/// Indices (`u16`, SCALAR). 46356 entries / 15452 triangles.
const IDX_OFFSET: usize = 0;
const IDX_COUNT: usize = 46356;

/// Vertex positions (`f32`, VEC3). 14556 vertices × 12 bytes.
const POS_OFFSET: usize = 92712;

/// Vertex normals (`f32`, VEC3). Same shape as positions.
const NRM_OFFSET: usize = 267384;

/// UV0 texture coordinates (`f32`, VEC2). 14556 × 8 bytes.
const UV_OFFSET: usize = 442056;

/// Vertex count shared across all three attributes.
const VTX_COUNT: usize = 14556;

/// Load `DamagedHelmet.bin`, parse out positions/normals/uvs/indices
/// at the fixed glTF buffer view offsets, rotate the mesh upright
/// (the sample was authored Z-up and its glTF node applies +90°
/// around X at runtime — we bake the rotation into the vertex data
/// so the demo transform stays `Mat4::IDENTITY`), and build a
/// `MeshData` with the albedo texture baked into the PBR material.
///
/// Runs once at startup. The resulting `MeshData` is handed to the
/// canvas render closure through an `Rc` so each frame re-uses the
/// same heap allocation.
/// Decode an image file into the `TextureData` struct the mesh pipeline
/// expects. Uses `blinc_image::ImageData` for cross-platform image
/// decoding. Panics on failure — these are build-time asset paths and a
/// missing file means the demo checkout is incomplete.
fn load_texture(path: &str) -> TextureData {
    let img = blinc_image::ImageData::load(blinc_image::ImageSource::File(path.into()))
        .unwrap_or_else(|e| panic!("failed to load {path}: {e}"));
    let (width, height) = img.dimensions();
    TextureData::new(img.into_pixels(), width, height)
}

fn load_helmet() -> MeshData {
    let bin_path = format!("{HELMET_GLTF_DIR}/DamagedHelmet.bin");

    let bin = blinc_platform::assets::load_asset(&bin_path)
        .unwrap_or_else(|e| panic!("failed to read {bin_path}: {e}"));

    // Little-endian readers for the two primitive types the glTF
    // uses. `try_into` + `unwrap` is fine here because the offsets are
    // compile-time constants derived from a known-good file; any panic
    // would point at a bug in the demo, not runtime input.
    let read_f32 =
        |off: usize| -> f32 { f32::from_le_bytes(bin[off..off + 4].try_into().unwrap()) };
    let read_u16 =
        |off: usize| -> u16 { u16::from_le_bytes(bin[off..off + 2].try_into().unwrap()) };

    // The glTF node carries quaternion `(0.7071, 0, 0, 0.7071)` —
    // that's a +90° rotation around the X axis (see the sample's
    // `DamagedHelmet.gltf`, `nodes[0].rotation`). Applying this to
    // a right-handed +Y-up point maps `(x, y, z) → (x, -z, y)`,
    // which takes the Z-up source data into our Y-up convention.
    //
    // Baking the rotation here lets the demo draw the mesh at
    // `Mat4::IDENTITY`, which keeps the camera math intuitive: the
    // helmet sits at the origin, the orbit camera rotates around
    // `(0, 0, 0)` unchanged.
    let rotate_x90 = |p: [f32; 3]| -> [f32; 3] { [p[0], -p[2], p[1]] };

    let mut vertices = Vec::with_capacity(VTX_COUNT);
    for i in 0..VTX_COUNT {
        let pos = rotate_x90([
            read_f32(POS_OFFSET + i * 12),
            read_f32(POS_OFFSET + i * 12 + 4),
            read_f32(POS_OFFSET + i * 12 + 8),
        ]);
        let nrm = rotate_x90([
            read_f32(NRM_OFFSET + i * 12),
            read_f32(NRM_OFFSET + i * 12 + 4),
            read_f32(NRM_OFFSET + i * 12 + 8),
        ]);
        // DamagedHelmet's TEXCOORD_0 accessor has V in the range
        // [1.0, 2.0] — perfectly legal glTF, since the spec allows
        // UVs outside [0, 1] and expects the sampler's wrap mode to
        // handle it. The Blinc mesh pipeline sampler is clamp-to-edge
        // rather than repeat, so any V > 1.0 lands on the top edge of
        // the texture, which paints the whole upper half of the helmet
        // with the top row of the albedo atlas and produces horizontal
        // streaks across the dome.
        //
        // Normalizing the V into [0, 1) here via `fract` sidesteps the
        // sampler mismatch entirely — the mesh renders correctly no
        // matter what wrap mode the pipeline chooses. `fract` (instead
        // of `v - 1.0`) keeps this robust against other assets whose
        // UVs wrap multiple times or start below zero.
        let raw_u = read_f32(UV_OFFSET + i * 8);
        let raw_v = read_f32(UV_OFFSET + i * 8 + 4);
        let uv = [raw_u - raw_u.floor(), raw_v - raw_v.floor()];
        vertices.push(Vertex::new(pos).with_normal(nrm).with_uv(uv));
    }

    let mut indices = Vec::with_capacity(IDX_COUNT);
    for i in 0..IDX_COUNT {
        indices.push(read_u16(IDX_OFFSET + i * 2) as u32);
    }

    // Load the full PBR texture stack. All five textures decode to
    // RGBA via `blinc_image::ImageData` (cross-platform loader). The
    // mesh pipeline samples each one — see
    // `crates/blinc_gpu/src/shaders/mesh.wgsl::fs_main` for the
    // Cook-Torrance BRDF math and the per-texel MR / emissive / AO
    // sampling blocks.
    //
    // Scalar factors stay at 1.0 because the texture values ARE the
    // PBR inputs. `metallic: 1.0` + `roughness: 1.0` preserves the
    // metallic-roughness texture unchanged per-texel; reducing either
    // scalar would attenuate the corresponding channel globally.
    let albedo_tex = load_texture(&format!("{HELMET_GLTF_DIR}/Default_albedo.jpg"));
    let normal_tex = load_texture(&format!("{HELMET_GLTF_DIR}/Default_normal.jpg"));
    let metal_rough_tex = load_texture(&format!("{HELMET_GLTF_DIR}/Default_metalRoughness.jpg"));
    let emissive_tex = load_texture(&format!("{HELMET_GLTF_DIR}/Default_emissive.jpg"));
    let ao_tex = load_texture(&format!("{HELMET_GLTF_DIR}/Default_AO.jpg"));

    let material = Material {
        base_color: [1.0, 1.0, 1.0, 1.0],
        metallic: 1.0,
        roughness: 1.0,
        // Scalar emissive multiplier. DamagedHelmet's emissive
        // texture encodes the HUD at low intensity — multiplying by a
        // small scalar (say 2.0, 2.0, 2.0) can amplify it once HDR
        // tonemapping lands in Stage 2. Leaving at 1.0 for now.
        emissive: [1.0, 1.0, 1.0],
        base_color_texture: Some(albedo_tex),
        normal_map: Some(normal_tex),
        normal_scale: 1.0,
        metallic_roughness_texture: Some(metal_rough_tex),
        emissive_texture: Some(emissive_tex),
        occlusion_texture: Some(ao_tex),
        occlusion_strength: 1.0,
        displacement_map: None,
        displacement_scale: 0.0,
        unlit: false,
        alpha_mode: AlphaMode::Opaque,
        receives_shadows: false,
        casts_shadows: false,
    };

    MeshData {
        vertices,
        indices,
        material,
        skin: None,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let config = WindowConfig {
        title: "3D Mesh Demo — DamagedHelmet".to_string(),
        width: 960,
        height: 720,
        ..Default::default()
    };

    blinc_app::windowed::WindowedApp::run(config, build_ui)
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    // Load the mesh once at UI-build time. `build_ui` runs on every
    // tree rebuild, so in theory this re-reads the file each time —
    // in practice the rebuild cadence on this demo is "window resize
    // only" because the rest of the UI is static. Follow-up: cache
    // via `BlincContextState::use_state_keyed` if any demo ever
    // changes that pattern.
    let helmet = Arc::new(load_helmet());

    // Build the scene kit with a default framing: distance
    // chosen so the DamagedHelmet (~1.9m tall by its own AABB) fits
    // comfortably in a 45° FOV viewport, and a warm key light from
    // the upper-front-left.
    // Load HDRI environment for realistic reflections
    let hdr_bytes =
        blinc_platform::assets::load_asset(format!("{ASSETS_3D_DIR}/rogland_clear_night_2k.hdr"))
            .unwrap_or_else(|e| panic!("failed to load HDRI: {e}"));

    let kit = SceneKit3D::new("mesh_3d_demo")
        .with_camera(
            OrbitCamera::default()
                .with_distance(3.2)
                .with_elevation(0.2)
                .with_azimuth(0.4)
                .with_target(Vec3::new(0.0, 0.0, 0.187)),
        )
        .with_hdri(&hdr_bytes, 256)
        .with_light(Light::Directional {
            direction: Vec3::new(-0.4, -1.0, -0.3).normalize(),
            color: Color::WHITE,
            intensity: 2.5,
            cast_shadows: false,
        });

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.05, 0.06, 0.10, 1.0))
        .flex_col()
        .child(header_bar())
        .child(
            div()
                .flex_grow()
                .w_full()
                .overflow_clip()
                .child(kit.element(move |ctx, _bounds| {
                    // The only line that matters. Camera and lights
                    // are already set by `SceneKit3D::element`, so
                    // the scene closure just emits mesh draws.
                    ctx.draw_mesh_data(helmet.clone(), Mat4::default());
                })),
        )
        .child(hint_bar())
}

fn header_bar() -> Div {
    div()
        .w_full()
        .h(48.0)
        .bg(Color::rgba(0.09, 0.10, 0.14, 1.0))
        .flex_row()
        .items_center()
        .justify_center()
        .gap(12.0)
        .child(
            text("DamagedHelmet — glTF 2.0 PBR sample")
                .size(16.0)
                .weight(FontWeight::SemiBold)
                .color(Color::rgba(0.95, 0.95, 1.0, 1.0)),
        )
}

fn hint_bar() -> Div {
    div()
        .w_full()
        .h(32.0)
        .bg(Color::rgba(0.07, 0.08, 0.12, 1.0))
        .flex_row()
        .items_center()
        .justify_center()
        .child(
            text("Drag to orbit · Scroll to zoom")
                .size(12.0)
                .color(Color::rgba(0.55, 0.58, 0.70, 1.0)),
        )
}
