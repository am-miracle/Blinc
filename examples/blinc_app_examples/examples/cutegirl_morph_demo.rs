//! Morph-target (blend-shape) GPU validation demo.
//!
//! no-web: the `cutegirl_g1` asset is CC-BY-NC-SA-4.0 and intentionally
//! gitignored (see `examples/.../assets/3d/cutegirl_g1/README.md`), so
//! it must not leak into the public web build. This marker tells the
//! cross-target example runner to skip wasm packaging.
//!
//! Exercises the Phase-2 mesh-pipeline morph path end to end:
//!
//! - 11 meshes carrying morph targets (2–152 per primitive)
//! - 1 clip (`Emote`) with 11 `MorphWeights` channels layered on top
//!   of 301 TRS channels
//! - Per-frame: `animate_scene_nodes` writes TRS, an inline sampler
//!   decodes each `MorphWeights` channel, then each draw clones the
//!   base `MeshData` (Arc<Vec> inner → shallow) and installs the
//!   fresh weights before dispatch.
//!
//! The demo degrades gracefully when the asset directory is absent —
//! the loading overlay simply never dismisses and a polite error is
//! logged. Install instructions live in the asset's README.
//!
//! Skinning is wired through `blinc_skeleton::Pose::skinning_data` —
//! every mesh whose node carries a skin index gets a per-frame
//! `SkinningData`. Cutegirl has a single skeleton shared by all
//! skinned meshes, so we build one `Pose`/`SkinningData` per frame and
//! clone into each draw. Multi-skeleton assets would index by
//! `node.skin` and build one per skin.
//!
//! ```sh
//! cargo run -p blinc_app_examples --example cutegirl_morph_demo \
//!     --features windowed --release
//! ```

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

const GLTF_PATH: &str =
    "examples/blinc_app_examples/examples/assets/3d/cutegirl_g1/scene.gltf";

const VIEWPORT_ID: &str = "cutegirl-morph-viewport";

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
    /// keeps playback phase-locked with vsync — the scheduler bg
    /// thread runs on its own cadence and produces a `dt` that does
    /// not match adjacent paint intervals.
    last_frame: Arc<Mutex<Option<Instant>>>,
}

#[derive(Clone)]
struct SceneState {
    scene: Arc<Mutex<GltfScene>>,
    /// Base (un-morphed) primitives per mesh index. Each frame we
    /// shallow-clone these and stamp the sampled morph weights before
    /// dispatch.
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
    /// Returns `None` when the asset directory is absent — the demo's
    /// overlay stays visible and logs a polite message. On wasm this
    /// also handles the preload race (poll until cache populates).
    fn try_load(path: &str) -> Option<Self> {
        let opts = blinc_gltf::LoadOptions {
            max_texture_size: Some(2048),
        };
        let mut scene = match blinc_gltf::load_asset_with_options(path, &opts) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(
                    "cutegirl_g1 asset not loadable ({e:?}) — \
                     see assets/3d/cutegirl_g1/README.md for install instructions"
                );
                return None;
            }
        };

        // Densify rotations so fast reversals don't slerp the wrong way.
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

        // Move primitives out — Arc-wrapping at the mesh level would
        // force Arc-of-MeshData mutation later, which is awkward.
        // Keeping them as `Vec<MeshData>` (with Arc<Vec> inner buffers)
        // lets us shallow-clone per-draw and stamp weights cheaply.
        let base_meshes: Vec<Vec<MeshData>> = scene
            .meshes
            .iter_mut()
            .map(|m| std::mem::take(&mut m.primitives))
            .collect();
        let animation = scene.animations.first().cloned();
        Some(Self {
            scene: Arc::new(Mutex::new(scene)),
            base_meshes: Arc::new(base_meshes),
            animation: Arc::new(animation),
            playback: Arc::new(Mutex::new(Playback {
                time: 0.0,
                duration,
                paused: true,
            })),
            duration,
        })
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
            // Defensive — this demo is marked no-web, but the cfg-gate
            // keeps the example compilable on wasm targets for tooling.
            let _ = (path, scene_ready, slot);
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
        title: "CuteGirl Morph Demo — Phase 2 vertex-stage blend shapes".to_string(),
        width: 1024,
        height: 768,
        ..Default::default()
    };

    blinc_app::windowed::WindowedApp::run(config, build_ui)
}

#[cfg(target_arch = "wasm32")]
fn main() {
    // no-web — asset redistribution is license-blocked. See module
    // header. A nop `main` keeps the target compilable for tooling.
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    let scene_ready = ctx.use_state_keyed("cutegirl_scene_ready", || false);

    let handle = ctx
        .use_state_keyed("cutegirl_morph_demo_handle", {
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

    let camera_signal = ctx.use_state_keyed("cutegirl_morph_demo_cam", OrbitCamera::default);
    let kit = SceneKit3D::new("cutegirl_morph_demo")
        .with_camera(
            OrbitCamera::default()
                .with_distance(3.0)
                .with_elevation(0.15)
                .with_azimuth(0.0)
                .with_target(Vec3::new(0.0, 1.3, 0.0)),
        )
        .with_light(Light::Directional {
            direction: Vec3::new(-0.4, -1.0, -0.3).normalize(),
            color: Color::WHITE,
            intensity: 4.0,
            cast_shadows: false,
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

            // Per-frame sampling: node TRS, the pose (for skinning),
            // and morph weights. Done inside one scene lock so borrows
            // don't overlap; outputs are owned so we can drop the lock
            // before dispatching draws.
            let (draws, skinning, weights_by_node) = {
                let mut scene_mut = state.scene.lock().unwrap();
                let (skinning, weights_by_node) = match state.animation.as_ref().as_ref() {
                    Some(anim) => {
                        blinc_skeleton::animate_scene_nodes(&mut scene_mut, anim, t);
                        // One skeleton — assume every skinned mesh in
                        // the scene shares it (true for cutegirl and
                        // most character exports). Multi-skin assets
                        // would need one SkinningData per skin index.
                        match scene_mut.skeletons.first() {
                            Some(skel) => {
                                // Seed from the scene's current joint
                                // TRS (animate_scene_nodes just wrote
                                // them) — bones not targeted by the
                                // clip keep their rest pose instead of
                                // collapsing to identity.
                                let mut pose =
                                    blinc_skeleton::Pose::from_scene(&scene_mut, skel);
                                pose.evaluate(anim, t, skel);
                                let sd = pose.skinning_data(&skel.skeleton);
                                (Some(sd), pose.morph_weights)
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

            for (node_idx, mesh_idx, node_skin, xf) in draws {
                let morph = weights_by_node.get(&node_idx);
                for prim in &state.base_meshes[mesh_idx] {
                    let has_morphs = !prim.morph_targets.is_empty();
                    let is_skinned = node_skin.is_some();
                    if !has_morphs && !is_skinned {
                        // Static primitive — reuse a fresh Arc and move on.
                        ctx.draw_mesh_data(Arc::new(prim.clone()), xf);
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
                    ctx.draw_mesh_data(Arc::new(per_draw), xf);
                }
            }

            handle_ren.input.frame_end();
        })
        .capture_input(&handle.input)
        .id(VIEWPORT_ID);

    let duration = handle.get().map(|s| s.duration).unwrap_or(0.0);

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
                    text("Loading cutegirl_g1… (asset is gitignored — see README)")
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
        .child(header_bar(duration))
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
    let title = format!("Morph Demo — CuteGirl G1 · Emote clip = {duration:.2}s");
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
