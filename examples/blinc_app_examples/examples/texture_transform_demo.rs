//! KHR_texture_transform Demo
//!
//! Loads Poly Haven's `marble_cliff_02` asset (CC0) — a displaced
//! rock chunk with a tiling PBR material — and showcases the
//! `KHR_texture_transform` glTF extension support added in
//! `blinc_core::TextureTransform` + `blinc_gpu::mesh_pipeline` +
//! `blinc_gltf::parse_material`.
//!
//! The asset's glTF JSON was patched to include
//! `"extensions": { "KHR_texture_transform": { "scale": [3, 3] } }`
//! on every texture binding, so `parse_material` reads a 3× tile
//! transform and the shader multiplies UVs accordingly before every
//! sample. Press **T** to toggle the transform off for a side-by-side
//! comparison — toggling swaps between the parsed `Material` and a
//! clone with `texture_transform: None`, exercising the shader's
//! identity path.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p blinc_app_examples --example texture_transform_demo --features gltf
//! ```
//!
//! # License
//!
//! Asset: `marble_cliff_02` by Rob Tuytel, published under CC0
//! (<https://polyhaven.com/a/marble_cliff_02>).

use std::sync::{Arc, OnceLock};

use blinc_animation::get_scheduler;
use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_canvas_kit::prelude::*;
use blinc_core::events::KeyCode;
use blinc_core::{Color, Light, Mat4, MeshData, State, Vec3};
use blinc_gltf::GltfScene;
use blinc_canvas_kit::InputState;

const GLTF_PATH: &str = "examples/blinc_app_examples/examples/assets/3d/marble_cliff_02_2k.gltf/marble_cliff_02_2k.gltf";

const VIEWPORT_ID: &str = "texture-transform-viewport";

// ─────────────────────────────────────────────────────────────────────────────
// Shared state — two material variants pre-built at load time so the
// toggle is a pointer swap, not a per-frame material clone.
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AsyncHandle {
    slot: Arc<OnceLock<SceneState>>,
    input: InputState,
    /// Flipped by `T` keypress. `true` = parsed KHR_texture_transform
    /// (3× tile), `false` = identity (1:1 UV).
    transform_enabled: State<bool>,
}

struct SceneState {
    /// `(mesh_idx, world_transform)` draw list, resolved from
    /// `GltfScene::compute_world_transforms` at load time. The scene
    /// is static (no animation) so we only walk it once.
    draws: Vec<(usize, Mat4)>,
    /// Primitives with the parsed `texture_transform` intact.
    arc_meshes_transform: Arc<Vec<Vec<Arc<MeshData>>>>,
    /// Same primitives with `texture_transform` cleared — for the
    /// toggle-off branch. Share the underlying vertex / index buffers
    /// via `Arc`; only the material differs.
    arc_meshes_identity: Arc<Vec<Vec<Arc<MeshData>>>>,
}

impl SceneState {
    fn from_scene(scene: GltfScene) -> Self {
        // Draw list: every node with a mesh contributes one draw at
        // its world transform. Baked once at load since the asset
        // has no animations.
        let world = scene.compute_world_transforms();
        let draws: Vec<(usize, Mat4)> = scene
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| n.mesh.map(|m| (m, world[i])))
            .collect();

        // Two mesh variants, built up-front:
        //
        // - `arc_meshes_transform` keeps whatever `texture_transform`
        //   `parse_material` decoded from the JSON (3× scale for the
        //   marble_cliff_02 asset).
        // - `arc_meshes_identity` is a full shallow clone with every
        //   primitive's `material.texture_transform` cleared. The
        //   material's `TextureData` handles are `Arc<[u8]>`-backed,
        //   so this is essentially free — one refcount bump per
        //   texture slot.
        let arc_meshes_transform: Vec<Vec<Arc<MeshData>>> = scene
            .meshes
            .iter()
            .map(|m| m.primitives.iter().cloned().map(Arc::new).collect())
            .collect();

        let arc_meshes_identity: Vec<Vec<Arc<MeshData>>> = arc_meshes_transform
            .iter()
            .map(|prims| {
                prims
                    .iter()
                    .map(|p| {
                        let mut cloned = (**p).clone();
                        cloned.material.texture_transform = None;
                        Arc::new(cloned)
                    })
                    .collect()
            })
            .collect();

        tracing::info!(
            "loaded {} meshes / {} nodes / {} draws — KHR_texture_transform: {}",
            scene.meshes.len(),
            scene.nodes.len(),
            draws.len(),
            scene
                .meshes
                .first()
                .and_then(|m| m.primitives.first())
                .and_then(|p| p.material.texture_transform)
                .map(|t| format!(
                    "parsed (scale={:?}, offset={:?}, rot={:.3})",
                    t.scale, t.offset, t.rotation
                ))
                .unwrap_or_else(|| "(not in asset)".to_string()),
        );

        Self {
            draws,
            arc_meshes_transform: Arc::new(arc_meshes_transform),
            arc_meshes_identity: Arc::new(arc_meshes_identity),
        }
    }
}

impl AsyncHandle {
    fn new(transform_enabled: State<bool>) -> Self {
        Self {
            slot: Arc::new(OnceLock::new()),
            input: InputState::new(),
            transform_enabled,
        }
    }

    fn spawn_load(&self, path: &'static str, scene_ready: State<bool>) {
        let slot = self.slot.clone();

        #[cfg(not(target_arch = "wasm32"))]
        std::thread::spawn(move || {
            let opts = blinc_gltf::LoadOptions {
                max_texture_size: Some(2048),
            };
            match blinc_gltf::load_asset_with_options(path, &opts) {
                Ok(scene) => {
                    register_scheduler_tick();
                    let _ = slot.set(SceneState::from_scene(scene));
                    scene_ready.set(true);
                    get_scheduler().request_redraw();
                }
                Err(e) => tracing::error!("texture_transform_demo load failed: {e:?}"),
            }
        });

        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            let opts = blinc_gltf::LoadOptions {
                max_texture_size: Some(2048),
            };
            match blinc_gltf::load_asset_with_options_async(path, &opts, |_| {}).await {
                Ok(scene) => {
                    register_scheduler_tick();
                    let _ = slot.set(SceneState::from_scene(scene));
                    scene_ready.set(true);
                    get_scheduler().request_redraw();
                }
                Err(e) => tracing::error!("texture_transform_demo async load failed: {e:?}"),
            }
        });
    }

    fn get(&self) -> Option<&SceneState> {
        self.slot.get()
    }
}

fn register_scheduler_tick() {
    let scheduler_for_redraw = get_scheduler();
    get_scheduler().register_tick_callback(move |_dt: f32| {
        scheduler_for_redraw.request_redraw();
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// Main
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let config = WindowConfig {
        title: "KHR_texture_transform — marble_cliff_02".to_string(),
        width: 960,
        height: 720,
        ..Default::default()
    };
    blinc_app::windowed::WindowedApp::run(config, build_ui)
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder + use<> {
    let scene_ready = ctx.use_state_keyed("texture_transform_demo_scene_ready", || false);
    let transform_enabled = ctx.use_state_keyed("texture_transform_demo_enabled", || true);

    let handle = ctx
        .use_state_keyed("texture_transform_demo_handle", {
            let scene_ready = scene_ready.clone();
            let transform_enabled = transform_enabled.clone();
            move || {
                let h = AsyncHandle::new(transform_enabled);
                h.spawn_load(GLTF_PATH, scene_ready);
                h
            }
        })
        .try_get()
        .expect("handle exists after use_state_keyed init");

    let kit = SceneKit3D::new("texture_transform_demo")
        .with_camera(
            OrbitCamera::default()
                .with_distance(12.0)
                .with_elevation(0.25)
                .with_azimuth(0.8)
                .with_target(Vec3::new(0.0, 0.0, 0.0)),
        )
        .with_light(Light::Directional {
            direction: Vec3::new(-0.4, -1.0, -0.3).normalize(),
            color: Color::WHITE,
            intensity: 3.0,
            cast_shadows: false,
        })
        .with_input(&handle.input);

    let handle_ren = handle.clone();
    let viewport = kit
        .element(move |ctx, _bounds| {
            let Some(state) = handle_ren.get() else {
                return;
            };
            // Toggle the transform on 'T' keypress. Branching the
            // draw list between pre-built `arc_meshes_transform` /
            // `_identity` vectors keeps the render closure allocation-
            // free — the only per-frame cost is the one-Arc-clone per
            // primitive that `draw_mesh_data` would do anyway.
            if handle_ren.input.is_key_just_pressed(KeyCode(b'T' as u32)) {
                let prev = handle_ren.transform_enabled.get();
                handle_ren.transform_enabled.set(!prev);
                tracing::info!("KHR_texture_transform: {}", !prev);
            }
            let meshes = if handle_ren.transform_enabled.get() {
                &state.arc_meshes_transform
            } else {
                &state.arc_meshes_identity
            };
            for (mesh_idx, xf) in &state.draws {
                for prim in &meshes[*mesh_idx] {
                    ctx.draw_mesh_data(prim.clone(), *xf);
                }
            }
        })
        .id(VIEWPORT_ID);

    // Loading overlay dismisses on `scene_ready`.
    let scene_ready_ov = scene_ready.clone();
    let overlay = stateful::<NoState>()
        .deps([scene_ready.signal_id()])
        .on_state(move |_ctx| {
            let mut d = div()
                .absolute()
                .top(0.0)
                .left(0.0)
                .w_full()
                .h_full()
                .bg(Color::rgba(0.0, 0.0, 0.0, 0.6))
                .flex_col()
                .items_center()
                .justify_center()
                .child(
                    text("Loading marble_cliff_02…")
                        .size(18.0)
                        .color(Color::rgba(0.95, 0.95, 0.95, 1.0)),
                );
            if scene_ready_ov.get() {
                d = d.hidden();
            }
            d
        });

    // Reactive status line showing current toggle state.
    let transform_enabled_status = transform_enabled.clone();
    let status_bar = stateful::<NoState>()
        .deps([transform_enabled.signal_id()])
        .on_state(move |_ctx| {
            let on = transform_enabled_status.get();
            let label = if on {
                "KHR_texture_transform: ON (3× tile)"
            } else {
                "KHR_texture_transform: OFF (identity)"
            };
            div()
                .w_full()
                .h(32.0)
                .bg(Color::rgba(0.07, 0.08, 0.12, 1.0))
                .flex_row()
                .items_center()
                .justify_center()
                .gap_px(16.0)
                .child(
                    text(label)
                        .size(13.0)
                        .color(Color::rgba(0.92, 0.92, 0.95, 1.0))
                        .monospace(),
                )
                .child(
                    text("Press T to toggle · Drag to orbit · Scroll to zoom")
                        .size(11.0)
                        .color(Color::rgba(0.55, 0.58, 0.70, 1.0)),
                )
        });

    let viewport_stack = div()
        .relative()
        .flex_grow()
        .w_full()
        .overflow_clip()
        .child(viewport)
        .child(overlay);

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.05, 0.06, 0.10, 1.0))
        .flex_col()
        .child(header_bar())
        .child(viewport_stack)
        .child(status_bar)
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
            text("KHR_texture_transform · marble_cliff_02 (CC0 by Rob Tuytel)")
                .size(15.0)
                .weight(FontWeight::SemiBold)
                .color(Color::rgba(0.95, 0.95, 1.0, 1.0)),
        )
}
