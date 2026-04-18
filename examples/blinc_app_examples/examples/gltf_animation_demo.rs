//! Skeleton animation with glTF + `blinc_canvas_kit`.
//!
//! Loads Sketchfab's buster_drone (39 meshes, 92 nodes, one 25-second
//! `Start_Liftoff` clip), runs the clip through `blinc_skeleton` each
//! frame, and renders the result with `SceneKit3D`. Asset load is
//! non-blocking: the UI paints a loading overlay while a background
//! thread parses the glTF, then flips a `scene_ready` signal that
//! the overlay's `Stateful` subtree dismisses itself on.
//!
//! The model is "Buster Drone" by LaVADraGoN
//! (<https://sketchfab.com/3d-models/buster-drone-294e79652f494130ad2ab00a13fdbafd>),
//! licensed CC-BY-4.0 (<http://creativecommons.org/licenses/by/4.0/>).
//! Full attribution alongside the asset in `assets/3d/buster_drone/license.txt`.
//!
//! Controls:
//! - **Drag**: orbit
//! - **Scroll**: zoom
//! - **Space**: pause / resume
//! - **R**: reset clip time
//! - **Left / Right**: scrub ±1 frame
//!
//! ```sh
//! cargo run -p blinc_app_examples --example gltf_animation_demo \
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
use blinc_input::InputState;
use blinc_layout::prelude::text;
use web_time::Instant;

// Workspace-relative — `cargo run` resolves from the repo root.
const GLTF_PATH: &str = "examples/blinc_app_examples/examples/assets/3d/buster_drone/scene.gltf";

const VIEWPORT_ID: &str = "gltf-animation-viewport";

// ─────────────────────────────────────────────────────────────────────────────
// Shared state
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AsyncHandle {
    slot: Arc<OnceLock<SceneState>>,
    input: InputState,
    autoplay_pending: Arc<AtomicBool>,
    auto_framer: AutoFramer,
    /// Main-thread `Instant` of the previous render closure call.
    /// Advanced in the render closure itself (not the scheduler bg
    /// thread) so `Playback.time` stays phase-locked with vsync —
    /// otherwise the scheduler's own tick cadence and the main
    /// thread's paint cadence drift out of sync, producing visible
    /// stutter on fast rotations (buster_drone's rotor blades were
    /// the canary). `None` means "first frame since the scene
    /// became available", so we record now and skip the advance.
    last_frame: Arc<Mutex<Option<Instant>>>,
}

#[derive(Clone)]
struct SceneState {
    scene: Arc<Mutex<GltfScene>>,
    arc_meshes: Arc<Vec<Vec<Arc<MeshData>>>>,
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
    /// Load the scene. Returns `None` on failure — most notably the
    /// web case where the asset hasn't been preloaded yet, so callers
    /// poll until it becomes `Some`.
    fn try_load(path: &str) -> Option<Self> {
        // Cap textures at 2K — buster_drone's 4K maps chew multi-GB of
        // GPU memory with no quality gain at demo viewport sizes.
        let opts = blinc_gltf::LoadOptions {
            max_texture_size: Some(2048),
        };
        let mut scene = match blinc_gltf::load_asset_with_options(path, &opts) {
            Ok(s) => s,
            Err(_) => return None,
        };

        // Densify rotation channels so fast-spinning rotors (>180°
        // per keyframe) slerp without picking the wrong hemisphere.
        let mut total_inserted = 0usize;
        for anim in scene.animations.iter_mut() {
            total_inserted += blinc_skeleton::densify_rotation_channels(anim);
        }
        tracing::info!("densified rotation channels: {total_inserted} keyframes inserted");

        // Opt the blade subtree out of shadows — the strobe otherwise
        // replaces the soft "moving disc" the reference render has.
        let rotor_mesh_ids: std::collections::HashSet<usize> = {
            let mut ids = std::collections::HashSet::new();
            let mut stack: Vec<usize> = scene
                .nodes
                .iter()
                .enumerate()
                .filter_map(|(i, n)| {
                    let name = n.name.as_deref()?;
                    (name == "Turbine_L" || name == "Turbine_R").then_some(i)
                })
                .collect();
            while let Some(idx) = stack.pop() {
                if let Some(node) = scene.nodes.get(idx) {
                    if let Some(mi) = node.mesh {
                        ids.insert(mi);
                    }
                    stack.extend(node.children.iter().copied());
                }
            }
            ids
        };
        for (i, mesh) in scene.meshes.iter_mut().enumerate() {
            if rotor_mesh_ids.contains(&i) {
                for prim in &mut mesh.primitives {
                    prim.material.casts_shadows = false;
                    prim.material.receives_shadows = false;
                }
            }
        }

        // Force red emissive on the eye/lens subtree. The FBX → glTF
        // converter lost the UV linkage for body_emissive.png's red
        // pixel; we reintroduce the glow by stamping emissive directly.
        let lens_mesh_ids: std::collections::HashSet<usize> = {
            let mut ids = std::collections::HashSet::new();
            let mut stack: Vec<usize> = scene
                .nodes
                .iter()
                .enumerate()
                .filter_map(|(i, n)| {
                    let name = n.name.as_deref()?;
                    let is_lens_root = name.contains("ILens")
                        || name.contains("IEye")
                        || name == "Eye_Pupil"
                        || name == "Eye_Controller";
                    is_lens_root.then_some(i)
                })
                .collect();
            while let Some(idx) = stack.pop() {
                if let Some(node) = scene.nodes.get(idx) {
                    if let Some(mi) = node.mesh {
                        ids.insert(mi);
                    }
                    stack.extend(node.children.iter().copied());
                }
            }
            ids
        };
        for (i, mesh) in scene.meshes.iter_mut().enumerate() {
            if lens_mesh_ids.contains(&i) {
                for prim in &mut mesh.primitives {
                    prim.material.emissive = [12.0, 0.0, 0.0];
                }
            }
        }

        let duration = scene.animations.first().map(clip_duration).unwrap_or(0.0);
        tracing::info!(
            "loaded {} meshes / {} nodes / {} animations (clip 0 duration = {:.2}s)",
            scene.meshes.len(),
            scene.nodes.len(),
            scene.animations.len(),
            duration
        );

        // `std::mem::take` the primitives so Arc-wrapping doesn't clone
        // the material textures (multi-GB on 4K-textured assets).
        let arc_meshes: Vec<Vec<Arc<MeshData>>> = scene
            .meshes
            .iter_mut()
            .map(|m| {
                std::mem::take(&mut m.primitives)
                    .into_iter()
                    .map(Arc::new)
                    .collect()
            })
            .collect();
        let animation = scene.animations.first().cloned();
        Some(Self {
            scene: Arc::new(Mutex::new(scene)),
            arc_meshes: Arc::new(arc_meshes),
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

    /// Load the scene on a background worker. Desktop uses an OS
    /// thread; wasm uses `spawn_local` polling on 100 ms ticks until
    /// the `WebAssetLoader` cache holds every referenced asset.
    /// `scene_ready` is flipped from the worker as the single source
    /// of truth for "scene available", driving the overlay dismiss.
    fn spawn_load(&self, path: &'static str, scene_ready: State<bool>) {
        let slot = self.slot.clone();

        #[cfg(not(target_arch = "wasm32"))]
        std::thread::spawn(move || {
            if let Some(state) = SceneState::try_load(path) {
                register_scheduler_tick();
                let _ = slot.set(state);
                scene_ready.set(true);
                get_scheduler().request_redraw();
            } else {
                tracing::error!("SceneState::try_load failed for {path}");
            }
        });

        #[cfg(target_arch = "wasm32")]
        {
            wasm_bindgen_futures::spawn_local(async move {
                loop {
                    if let Some(state) = SceneState::try_load(path) {
                        register_scheduler_tick();
                        let _ = slot.set(state);
                        scene_ready.set(true);
                        get_scheduler().request_redraw();
                        break;
                    }
                    // Stop polling once the platform loader declares
                    // every preload settled — otherwise a permanently
                    // missing asset keeps the loop spinning forever
                    // and the overlay never dismisses.
                    if blinc_platform::assets::preload_settled() {
                        tracing::error!(
                            "gltf_animation_demo: preload settled without scene resolving"
                        );
                        break;
                    }
                    sleep_ms(100).await;
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

#[cfg(target_arch = "wasm32")]
async fn sleep_ms(ms: u32) {
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;
    let promise = js_sys::Promise::new(&mut |resolve: js_sys::Function, _reject| {
        web_sys::window().and_then(|w| {
            w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32)
                .ok()
        });
    });
    let _ = JsFuture::from(promise).await;
}

// ─────────────────────────────────────────────────────────────────────────────
// Scheduler tick — wakes the main thread for a redraw each frame.
// `Playback.time` is advanced by the render closure instead (see
// `AsyncHandle::last_frame`); doing the time math here would tick at
// the scheduler's bg-thread cadence, which doesn't line up with vsync
// and shows up as stroboscopic stutter on fast rotors.
// ─────────────────────────────────────────────────────────────────────────────

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
        title: "glTF Animation Demo — buster_drone".to_string(),
        width: 1024,
        height: 768,
        ..Default::default()
    };

    blinc_app::windowed::WindowedApp::run(config, build_ui)
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    // Overlay-dismiss signal — the loader thread flips it once the
    // glTF is parsed and the slot is populated.
    let scene_ready = ctx.use_state_keyed("gltf_scene_ready", || false);

    // Spawn the background loader once; return a handle the UI can
    // poll every frame.
    let handle = ctx
        .use_state_keyed("gltf_animation_demo_handle", {
            let scene_ready = scene_ready.clone();
            move || {
                let handle = AsyncHandle::new();
                handle.spawn_load(GLTF_PATH, scene_ready);
                handle
            }
        })
        .try_get()
        .expect("handle signal should exist after use_state_keyed init");

    // Latch autoplay the first time the viewport lays out; the render
    // closure un-pauses playback once `handle.get()` becomes `Some`.
    let autoplay_latch = handle.autoplay_pending.clone();
    ctx.query(VIEWPORT_ID).on_ready(move |_| {
        autoplay_latch.store(true, Ordering::Release);
    });

    // Camera signal shared with `SceneKit3D` so `AutoFramer` can fit
    // the loaded scene's AABB the first frame it's available.
    let camera_signal = ctx.use_state_keyed("gltf_animation_demo_cam", OrbitCamera::default);
    let kit = SceneKit3D::new("gltf_animation_demo")
        .with_camera(
            OrbitCamera::default()
                .with_distance(570.0)
                .with_elevation(0.25)
                .with_azimuth(0.6)
                .with_target(Vec3::new(0.0, 47.0, 0.0)),
        )
        .with_light(Light::Directional {
            direction: Vec3::new(-0.4, -1.0, -0.3).normalize(),
            color: Color::WHITE,
            intensity: 6.0,
            cast_shadows: false,
        });

    // Viewport — every frame: auto-frame on first AABB, poll input,
    // sample the clip, dispatch the mesh draw list.
    let handle_ren = handle.clone();
    let camera_signal_ren = camera_signal.clone();
    // `SceneKit3D::with_input` automates `capture_input` + the
    // per-frame `input.frame_end()` call, so the render closure
    // below can read input via closure capture without bracketing
    // every code path with `frame_end()`. The old pattern
    // (pre-`with_input`) required `.capture_input(&handle.input)`
    // on the Div and a `frame_end()` call on every early-out.
    let viewport = kit
        .with_input(&handle.input)
        .element(move |ctx: &mut dyn DrawContext, _bounds| {
            handle_ren.autoplay_if_ready();

            let Some(state) = handle_ren.get() else {
                return;
            };

            handle_ren
                .auto_framer
                .apply(&camera_signal_ren, state.scene.lock().unwrap().world_aabb());

            // Main-thread `dt` from the previous paint. Advancing
            // `pb.time` here (instead of the scheduler's tick
            // callback) keeps playback phase-locked with vsync — the
            // scheduler bg-thread runs on its own cadence and feeds
            // a `dt` that doesn't match the wall-clock gap between
            // adjacent renders, producing stutter on fast rotations.
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

            let draws: Vec<(usize, Mat4)> = {
                let mut scene_mut = state.scene.lock().unwrap();
                if let Some(anim) = state.animation.as_ref() {
                    blinc_skeleton::animate_scene_nodes(&mut scene_mut, anim, t);
                }
                let world = scene_mut.compute_world_transforms();
                scene_mut
                    .nodes
                    .iter()
                    .enumerate()
                    .filter_map(|(i, n)| n.mesh.map(|m| (m, world[i])))
                    .collect()
            };

            // Alpha-mode ordering is handled inside the framework
            // (`dispatch_pending_meshes` in blinc_app sorts OPAQUE +
            // MASK before BLEND) — callers can submit in any order.
            for (mesh_idx, xf) in draws {
                for prim in &state.arc_meshes[mesh_idx] {
                    ctx.draw_mesh_data(prim.clone(), xf);
                }
            }
        })
        .id(VIEWPORT_ID);

    // Reactive header — the scene loads async, so `handle.get()` is
    // None on first build_ui and `duration` would bake in as 0. Wrap
    // the header in a `stateful` keyed on `scene_ready` so it re-runs
    // once the load completes.
    let handle_header = handle.clone();
    let scene_ready_h = scene_ready.clone();
    let header = stateful::<NoState>()
        .deps([scene_ready.signal_id()])
        .on_state(move |_ctx| {
            let _ = scene_ready_h.get();
            let d = handle_header.get().map(|s| s.duration).unwrap_or(0.0);
            header_bar(d)
        });

    // Loading overlay — same div shape every refresh, `.hidden()`
    // (display:none) toggled on ready. Display::None differs from the
    // default Flex, so `Div::merge` picks the change up cleanly; no
    // placeholder children needed.
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
                    text("Loading scene…")
                        .size(18.0)
                        .color(Color::rgba(0.95, 0.95, 0.95, 1.0)),
                );
            if scene_ready_s.get() {
                d = d.hidden();
            }
            d
        });

    // Viewport + overlay stack. `.relative()` is the containing block
    // the overlay's `.absolute()` anchors to.
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
    let title = format!("glTF Animation — buster_drone · clip = {duration:.2}s");
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
