//! End-to-end 3D demo wiring Blinc's SceneKit3D renderer up to
//! [`blinc_gltf`] for asset loading and [`blinc_skeleton`] for
//! runtime posing.
//!
//! - [`blinc_canvas_kit::SceneKit3D`] — the camera + light + mesh
//!   dispatch front-end used by any Blinc app that wants to drop
//!   3D content into a `canvas()`. Same primitive demos use for a
//!   single spinning cube scale up to a full character rig
//!   unchanged.
//! - [`blinc_gltf`] — glTF 2.0 loader. Parses the file tree once at
//!   startup into a `GltfScene` (meshes, nodes, skeletons,
//!   animation clips) that the demo holds behind an `Arc<Mutex<>>`
//!   and borrows per frame.
//! - [`blinc_skeleton`] — runtime poser. `animate_scene_nodes`
//!   samples the clip's TRS channels into the live node tree;
//!   `scene_skinning_data` walks the posed tree to build the joint
//!   matrices the mesh shader consumes;
//!   `animate_scene_morph_weights` drives per-node blend-shape
//!   weights for facial expression.
//!
//! The asset is "The Strangler" by Jungle Jim (CC-BY-4.0;
//! <https://sketchfab.com/3d-models/the-strangler-06d56efabf7445e89bb1bf41a99d08cc>),
//! shipped in the repo for offline reproducibility. Full
//! attribution lives alongside the asset in
//! `examples/.../assets/3d/the_strangler/license.txt`.
//!
//! Per-frame flow:
//!
//! 1. `animate_scene_nodes(&mut scene, anim, t)` — writes sampled
//!    TRS onto scene nodes
//! 2. `scene_skinning_data(&scene, &skeleton)` — returns
//!    `SkinningData` (joint world matrices × inverse-bind)
//! 3. `animate_scene_morph_weights(anim, t)` — returns a
//!    `HashMap<node_index, Vec<f32>>` of current weights
//! 4. For each drawable node: shallow-clone its `MeshData`
//!    (`Arc<Vec<_>>` inners → refcount bumps, no vertex copy),
//!    stamp the frame's skinning + morph_weights, dispatch via
//!    `DrawContext::draw_mesh_data`.
//!
//! Ordering (OPAQUE before BLEND) is enforced framework-side in
//! `blinc_app::dispatch_pending_meshes`, so the demo submits in
//! scene-graph order without its own sort.
//!
//! ```sh
//! cargo run -p blinc_app_examples --example strangler_demo \
//!     --features gltf --release
//! ```
//!
//! [`blinc_canvas_kit::SceneKit3D`]: https://docs.rs/blinc_canvas_kit/latest/blinc_canvas_kit/struct.SceneKit3D.html
//! [`blinc_gltf`]: https://github.com/project-blinc/blinc_gltf
//! [`blinc_skeleton`]: https://github.com/project-blinc/blinc_skeleton

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use blinc_animation::get_scheduler;
use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_canvas_kit::prelude::*;
use blinc_canvas_kit::AutoFramer;
use blinc_core::events::KeyCode;
use blinc_core::{Color, DrawContext, Light, Mat4, MeshData, State, Vec3};
use blinc_gltf::{GltfAnimation, GltfScene};
use blinc_input::{DivInputExt, InputState};
use blinc_layout::prelude::text;
use web_time::Instant;

const GLTF_PATH: &str = "examples/blinc_app_examples/examples/assets/3d/the_strangler/scene.gltf";

const VIEWPORT_ID: &str = "strangler-viewport";

// ─────────────────────────────────────────────────────────────────────────────
// Shared state
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AsyncHandle {
    slot: Arc<OnceLock<SceneState>>,
    input: InputState,
    autoplay_pending: Arc<AtomicBool>,
    auto_framer: AutoFramer,
    /// Main-thread timestamp of the previous render pass. Advancing
    /// `Playback.time` in the render closure (not the scheduler tick)
    /// keeps playback phase-locked with vsync.
    last_frame: Arc<Mutex<Option<Instant>>>,
}

#[derive(Clone)]
struct SceneState {
    scene: Arc<Mutex<GltfScene>>,
    base_meshes: Arc<Vec<Vec<MeshData>>>,
    animation: Arc<Option<GltfAnimation>>,
    playback: Arc<Mutex<Playback>>,
    duration: f32,
}

struct Playback {
    time: f32,
    duration: f32,
    paused: bool,
}

impl SceneState {
    /// Native: synchronously load through `blinc_platform` + build.
    #[cfg(not(target_arch = "wasm32"))]
    fn try_load(path: &str) -> Option<Self> {
        let opts = blinc_gltf::LoadOptions {
            max_texture_size: Some(2048),
        };
        match blinc_gltf::load_asset_with_options(path, &opts) {
            Ok(scene) => Some(Self::from_scene(scene)),
            Err(e) => {
                tracing::error!("the_strangler asset not loadable ({e:?})");
                None
            }
        }
    }

    /// Build a `SceneState` from an already-loaded `GltfScene`.
    /// Shared between the native synchronous path and the wasm
    /// async path — the heavy work (`blinc_gltf::load_asset_async`)
    /// is isolated above this, so every target agrees on how to
    /// turn a loaded scene into a ready-to-render state.
    fn from_scene(mut scene: GltfScene) -> Self {
        let mut total_inserted = 0usize;
        for anim in scene.animations.iter_mut() {
            total_inserted += blinc_skeleton::densify_rotation_channels(anim);
        }
        if total_inserted > 0 {
            tracing::info!("densified rotation channels: {total_inserted} keyframes inserted");
        }

        let duration = scene.animations.first().map(clip_duration).unwrap_or(0.0);
        let morph_mesh_count = scene
            .meshes
            .iter()
            .filter(|m| m.primitives.iter().any(|p| !p.morph_targets.is_empty()))
            .count();
        tracing::info!(
            "loaded {} meshes ({} with morph targets) / {} nodes / clip 0 duration = {:.2}s",
            scene.meshes.len(),
            morph_mesh_count,
            scene.nodes.len(),
            duration
        );

        let base_meshes: Vec<Vec<MeshData>> = scene
            .meshes
            .iter_mut()
            .map(|m| std::mem::take(&mut m.primitives))
            .collect();
        let animation = scene.animations.first().cloned();
        Self {
            scene: Arc::new(Mutex::new(scene)),
            base_meshes: Arc::new(base_meshes),
            animation: Arc::new(animation),
            playback: Arc::new(Mutex::new(Playback {
                time: 0.0,
                duration,
                paused: true,
            })),
            duration,
        }
    }
}

impl AsyncHandle {
    fn new() -> Self {
        Self {
            slot: Arc::new(OnceLock::new()),
            input: InputState::new(),
            autoplay_pending: Arc::new(AtomicBool::new(false)),
            auto_framer: AutoFramer::new(),
            last_frame: Arc::new(Mutex::new(None)),
        }
    }

    fn spawn_load(&self, path: &'static str, scene_ready: State<bool>) {
        let slot = self.slot.clone();

        #[cfg(not(target_arch = "wasm32"))]
        std::thread::spawn(move || {
            if let Some(state) = SceneState::try_load(path) {
                register_scheduler_tick();
                let _ = slot.set(state);
                scene_ready.set(true);
                get_scheduler().request_redraw();
            }
        });

        #[cfg(target_arch = "wasm32")]
        {
            // `blinc_gltf::load_asset_with_options_async` folds the
            // old retry loop into the library: it waits for preload
            // to settle, loads, and yields between stages so the
            // browser can paint a loading overlay. The single await
            // replaces ~20 lines of 100 ms polling + preload_settled
            // escape-hatch logic the demo used to carry.
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
                    Err(e) => {
                        tracing::error!("the_strangler async load failed: {e:?}");
                    }
                }
            });
        }
    }

    fn get(&self) -> Option<&SceneState> {
        self.slot.get()
    }

    fn autoplay_if_ready(&self) {
        if let Some(state) = self.slot.get() {
            if self.autoplay_pending.swap(false, Ordering::AcqRel) {
                state.playback.lock().unwrap().paused = false;
            }
        }
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
        title: "The Strangler — full-body skin + morph demo".to_string(),
        width: 1024,
        height: 768,
        ..Default::default()
    };

    blinc_app::windowed::WindowedApp::run(config, build_ui)
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    let scene_ready = ctx.use_state_keyed("strangler_scene_ready", || false);
    let camera_signal = ctx.use_state_keyed("strangler_demo_cam", OrbitCamera::default);
    let kit = SceneKit3D::new("strangler_demo")
        .with_camera(
            // Full-body character — farther pull-back than a portrait
            // demo and a target height around mid-torso. AutoFramer
            // refines once the scene AABB resolves.
            OrbitCamera::default()
                .with_distance(6.0)
                .with_elevation(0.1)
                // The strangler rig faces -Z at import, so the default
                // azimuth=0 camera (looking down -Z from +Z) already
                // sees his face.
                .with_azimuth(0.0)
                .with_target(Vec3::new(0.0, 1.0, 0.0)),
        )
        // Two-point rig: front key + soft side fill. Intensities
        // are tuned for linear-space lighting (~2.2× what felt
        // right when the renderer was accidentally skipping the
        // sRGB→linear decode on diffuse textures).
        .with_light(Light::Directional {
            direction: Vec3::new(-0.3, -0.4, -1.0).normalize(),
            color: Color::WHITE,
            intensity: 3.0,
            cast_shadows: false,
        })
        .with_light(Light::Directional {
            direction: Vec3::new(0.7, -0.2, -0.3).normalize(),
            color: Color::rgba(1.0, 0.95, 0.9, 1.0),
            intensity: 1.1,
            cast_shadows: false,
        });

    let handle = ctx
        .use_state_keyed("strangler_demo_handle", {
            let scene_ready = scene_ready.clone();
            move || {
                let handle = AsyncHandle::new();
                handle.spawn_load(GLTF_PATH, scene_ready);
                handle
            }
        })
        .try_get()
        .expect("handle signal should exist after use_state_keyed init");

    let autoplay_latch = handle.autoplay_pending.clone();
    ctx.query(VIEWPORT_ID).on_ready(move |_| {
        autoplay_latch.store(true, Ordering::Release);
    });

    let handle_ren = handle.clone();
    let camera_signal_ren = camera_signal.clone();
    let viewport = kit
        .element(move |ctx: &mut dyn DrawContext, _bounds| {
            handle_ren.autoplay_if_ready();

            let Some(state) = handle_ren.get() else {
                handle_ren.input.frame_end();
                return;
            };

            handle_ren
                .auto_framer
                .apply(&camera_signal_ren, state.scene.lock().unwrap().world_aabb());

            let now = Instant::now();
            let dt = {
                let mut slot = handle_ren.last_frame.lock().unwrap();
                let dt = slot
                    .map(|prev| now.duration_since(prev).as_secs_f32())
                    .unwrap_or(0.0);
                *slot = Some(now);
                dt
            };

            let mut pb = state.playback.lock().unwrap();
            if handle_ren.input.is_key_just_pressed(KeyCode::SPACE) {
                pb.paused = !pb.paused;
            }
            if handle_ren.input.is_key_just_pressed(KeyCode(b'R' as u32)) {
                pb.time = 0.0;
            }
            if handle_ren.input.is_key_down(KeyCode::LEFT) {
                pb.time = (pb.time - 1.0 / 60.0).max(0.0);
            }
            if handle_ren.input.is_key_down(KeyCode::RIGHT) {
                pb.time += 1.0 / 60.0;
            }
            if !pb.paused && pb.duration > 0.0 {
                pb.time += dt;
                if pb.time > pb.duration {
                    pb.time = pb.time.rem_euclid(pb.duration);
                }
            }
            let t = pb.time;
            drop(pb);

            let (draws, skinning, weights_by_node) = {
                let mut scene_mut = state.scene.lock().unwrap();
                let (skinning, weights_by_node) = match state.animation.as_ref().as_ref() {
                    Some(anim) => {
                        blinc_skeleton::animate_scene_nodes(&mut scene_mut, anim, t);
                        match scene_mut.skeletons.first() {
                            Some(skel) => {
                                let sd = blinc_skeleton::scene_skinning_data(&scene_mut, skel);
                                let morphs = blinc_skeleton::animate_scene_morph_weights(anim, t);
                                (Some(sd), morphs)
                            }
                            None => (None, std::collections::HashMap::new()),
                        }
                    }
                    None => (None, std::collections::HashMap::new()),
                };
                let world = scene_mut.compute_world_transforms();
                let draws: Vec<(usize, usize, Option<usize>, Mat4)> = scene_mut
                    .nodes
                    .iter()
                    .enumerate()
                    .filter_map(|(i, n)| n.mesh.map(|m| (i, m, n.skin, world[i])))
                    .collect();
                (draws, skinning, weights_by_node)
            };

            let identity: Mat4 = Mat4::IDENTITY;

            // Alpha-mode ordering is enforced inside the framework
            // (`dispatch_pending_meshes` in blinc_app sorts OPAQUE +
            // MASK before BLEND) — submit in scene-graph order.
            for (node_idx, mesh_idx, node_skin, xf) in draws {
                let morph = weights_by_node.get(&node_idx);
                let is_skinned = node_skin.is_some();
                let draw_xf = if is_skinned { identity } else { xf };
                for prim in &state.base_meshes[mesh_idx] {
                    let has_morphs = !prim.morph_targets.is_empty();
                    if !has_morphs && !is_skinned {
                        ctx.draw_mesh_data(Arc::new(prim.clone()), draw_xf);
                        continue;
                    }
                    let mut per_draw = prim.clone();
                    if is_skinned {
                        if let Some(sd) = skinning.as_ref() {
                            per_draw.skin = Some(sd.clone());
                        }
                    }
                    if has_morphs {
                        let target_count = prim.morph_targets.len();
                        per_draw.morph_weights = match morph {
                            Some(w) if w.len() >= target_count => w[..target_count].to_vec(),
                            Some(w) => {
                                let mut v = w.clone();
                                v.resize(target_count, 0.0);
                                v
                            }
                            None => vec![0.0; target_count],
                        };
                    }
                    ctx.draw_mesh_data(Arc::new(per_draw), draw_xf);
                }
            }

            handle_ren.input.frame_end();
        })
        .capture_input(&handle.input)
        .id(VIEWPORT_ID);

    // Reactive header — reads `duration` lazily from the async-loaded
    // SceneState. The initial `build_ui` call runs before the scene
    // load thread finishes, so reading `handle.get()` directly here
    // gives 0. Wrapping in a `stateful` keyed on `scene_ready` re-runs
    // the render closure once the load completes and the title picks
    // up the real clip length.
    let handle_header = handle.clone();
    let scene_ready_h = scene_ready.clone();
    let header = stateful::<NoState>()
        .deps([scene_ready.signal_id()])
        .on_state(move |_ctx| {
            let _ = scene_ready_h.get();
            let d = handle_header.get().map(|s| s.duration).unwrap_or(0.0);
            header_bar(d)
        });

    let scene_ready_s = scene_ready.clone();
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
                    text("Loading the_strangler…")
                        .size(16.0)
                        .color(Color::rgba(0.95, 0.95, 0.95, 1.0)),
                );
            if scene_ready_s.get() {
                d = d.hidden();
            }
            d
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
        .justify_center()
        .child(header)
        .child(viewport_stack)
        .child(hint_bar())
}

fn clip_duration(anim: &GltfAnimation) -> f32 {
    anim.channels
        .iter()
        .filter_map(|ch| ch.sampler.times.last().copied())
        .fold(0.0f32, f32::max)
}

fn header_bar(duration: f32) -> Div {
    let title = format!("The Strangler · TempMotion clip = {duration:.2}s");
    div()
        .w_full()
        .h(44.0)
        .bg(Color::rgba(0.09, 0.10, 0.14, 1.0))
        .flex_row()
        .items_center()
        .justify_center()
        .px(16.0)
        .child(
            text(title)
                .size(14.0)
                .color(Color::rgba(0.85, 0.85, 0.9, 1.0)),
        )
}

fn hint_bar() -> Div {
    let hints = "drag: orbit · scroll: zoom · space: pause · ←/→: scrub · r: reset";
    div()
        .w_full()
        .h(32.0)
        .bg(Color::rgba(0.07, 0.08, 0.12, 1.0))
        .flex_row()
        .items_center()
        .justify_center()
        .px(16.0)
        .child(
            text(hints)
                .size(12.0)
                .color(Color::rgba(0.55, 0.6, 0.7, 1.0)),
        )
}
