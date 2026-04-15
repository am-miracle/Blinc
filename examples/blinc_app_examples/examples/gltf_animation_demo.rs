//! Skeleton animation with glTF + `blinc_canvas_kit`.
//!
//! Loads a rigged glTF scene (Sketchfab's buster_drone — 39 meshes,
//! 92 nodes, one 25-second "Start_Liftoff" clip with 100 transform
//! channels), runs it through the `blinc_skeleton` poser each frame,
//! and renders the resulting transforms with `SceneKit3D`'s
//! immediate-mode PBR path. The clip drives node-level TRS channels
//! (no skins in this asset), so it exercises the pure-transform
//! animation pipeline end to end:
//!
//! - `blinc_gltf::load_asset` — cross-platform asset loading
//!   (filesystem / APK / bundle / HTTP) through the
//!   `blinc_platform::assets` global loader, plus `KHR_materials_*`
//!   support and the full PBR metallic-roughness material block.
//! - `blinc_skeleton::densify_rotation_channels` — preprocesses the
//!   clip's rotation channels so fast rotors (blade rotation > 180°
//!   per keyframe, a frequent FBX-exporter trap) slerp smoothly
//!   instead of flipping direction every keyframe.
//! - `blinc_skeleton::animate_scene_nodes` — samples the clip at the
//!   current playback time and writes interpolated TRS values into
//!   `scene.nodes[*].transform`.
//! - `blinc_canvas_kit::SceneKit3D` — orbit camera, HDRI-lit
//!   environment, and `ctx.draw_mesh_data(...)` per primitive.
//! - `blinc_input::InputState` via
//!   `blinc_canvas_kit::SketchEvents::on_canvas_events` — polling
//!   keyboard + pointer state inside the render closure with a single
//!   `.capture_input(&state.input)` call on the scene's `Div`.
//!
//! Controls:
//! - **Drag**: orbit camera (wired by `SceneKit3D`)
//! - **Scroll**: zoom in / out
//! - **Space**: pause / resume the animation
//! - **R**: reset clip time to 0
//! - **Left / Right**: scrub ±1 frame while held
//!
//! Run with:
//!
//! ```sh
//! cargo run -p blinc_app_examples --example gltf_animation_demo \
//!     --features windowed --release
//! ```
//!
//! `--release` matters — the immediate-mode path draws 39 meshes per
//! frame, and the debug profile's per-call overhead adds up. Debug is
//! playable but sluggish.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use blinc_animation::get_scheduler;
use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_canvas_kit::prelude::*;
use blinc_core::events::KeyCode;
use blinc_core::{Color, DrawContext, Light, Mat4, MeshData, Vec3};
use blinc_gltf::{GltfAnimation, GltfScene};
use blinc_input::{DivInputExt, InputState};

// Workspace-relative path — `cargo run -p blinc_app_examples --example ...`
// resolves from the repo root, not the crate root.
const GLTF_PATH: &str = "examples/blinc_app_examples/examples/assets/3d/buster_drone/scene.gltf";

/// ID on the viewport Div — used by `ctx.query(...).on_ready(...)` to
/// kick off animation playback the first time layout completes.
const VIEWPORT_ID: &str = "gltf-animation-viewport";

// ─────────────────────────────────────────────────────────────────────────────
// Shared state — all UI-rebuild-persistent data lives on a single
// struct so we can stash it once in `BlincContextState` and clone the
// shared-Arc handle into each closure.
// ─────────────────────────────────────────────────────────────────────────────

/// Async wrapper around the real `SceneState`. Gives the UI a handle
/// that exists from the very first frame, long before the glTF asset
/// has finished loading — the scene-kit element's render closure
/// checks `get()` each frame and either draws the scene or a loading
/// overlay. When the background loader finally populates the slot the
/// next frame starts rendering the real mesh.
///
/// Fields that need to exist *before* the scene is ready (input state,
/// the "kick playback once the viewport lays out" latch) live here on
/// the handle rather than on `SceneState`. Reactive fields still live
/// on `SceneState` and are reached only after `slot.get()` returns
/// `Some`.
#[derive(Clone)]
struct AsyncHandle {
    /// Set exactly once by the background loader after
    /// [`SceneState::try_load`] succeeds. Read each frame by the
    /// render closure. `OnceLock` survives the drop-lock / write-once
    /// contract without mutex overhead on hot reads.
    slot: Arc<OnceLock<SceneState>>,
    /// Polled-input state captured by the viewport `Div`. Kept on the
    /// handle (not `SceneState`) so `capture_input(&handle.input)`
    /// works from the first frame — otherwise the demo couldn't
    /// receive input while still loading.
    input: InputState,
    /// Latched "autoplay requested" flag. The viewport's `on_ready`
    /// callback can fire *before* the scene is loaded; flip this to
    /// `true` there and the render closure consumes it on the first
    /// frame the scene is available, un-pausing playback.
    autoplay_pending: Arc<AtomicBool>,
}

#[derive(Clone)]
struct SceneState {
    scene: Arc<Mutex<GltfScene>>,
    /// Pre-allocated `Arc<MeshData>` per primitive so the render
    /// closure never clones vertex data — the animation only mutates
    /// node transforms, geometry itself is invariant.
    arc_meshes: Arc<Vec<Vec<Arc<MeshData>>>>,
    /// Clone 0 of the loaded scene's animations (if any). Stored here
    /// instead of looked up from `scene.animations[0]` each frame so
    /// the render closure doesn't need to lock `scene` to read it.
    animation: Arc<Option<GltfAnimation>>,
    playback: Arc<Mutex<Playback>>,
    duration: f32,
}

struct Playback {
    time: f32,
    duration: f32,
    /// Start paused — `on_ready` flips this to `false` the first time
    /// the viewport lays out, giving the first frame something to
    /// render before the clock starts advancing.
    paused: bool,
}

impl SceneState {
    /// Attempt to load the scene. Returns `None` on *any* failure —
    /// most importantly the web case where `blinc_platform::assets`
    /// returns an error until `WebAssetLoader::preload` has
    /// populated the cache. Callers (typically a retry loop in a
    /// background task) poll until this returns `Some`.
    ///
    /// Cross-platform: goes through `blinc_platform::assets` so the
    /// same demo code runs on desktop (filesystem), Android (APK
    /// assets), iOS (bundle), and web (HTTP) once the right
    /// platform loader is registered. On desktop the default
    /// `FilesystemAssetLoader` reads from the CWD, which matches
    /// `cargo run` resolving from the repo root.
    fn try_load(path: &str) -> Option<Self> {
        // Downsample oversized textures at load. buster_drone ships
        // several 4K × 4K albedo / normal / metallic-roughness maps
        // (~64 MB each decoded RGBA8 × CPU + GPU copies); 2K is the
        // practical ceiling for the viewport sizes the demo runs at,
        // and the runtime's trilinear sampler keeps normal-viewing-
        // distance quality indistinguishable. Cuts total texture
        // memory roughly 4×.
        let opts = blinc_gltf::LoadOptions {
            max_texture_size: Some(2048),
        };
        let mut scene = match blinc_gltf::load_asset_with_options(path, &opts) {
            Ok(s) => s,
            Err(_) => return None,
        };

        // ── Densify rotation channels ─────────────────────────────────
        //
        // buster_drone's blade rotation channels are sparse relative to
        // their angular speed — many consecutive keyframes encode > 180°
        // of rotation, which standard slerp interprets as the *shorter*
        // arc going the wrong way. After takeoff this shows up as the
        // blades jittering forward and backward instead of spinning.
        //
        // `densify_rotation_channels` inserts intermediate keys
        // wherever a segment's true authored arc exceeds 60°, so the
        // runtime sampler only ever slerps unambiguous short arcs.
        // Idempotent — safe to call on already-dense channels.
        let mut total_inserted = 0usize;
        for anim in scene.animations.iter_mut() {
            total_inserted += blinc_skeleton::densify_rotation_channels(anim);
        }
        tracing::info!("densified rotation channels: {total_inserted} keyframes inserted");

        // ── Disable shadows on rotor meshes ──────────────────────────────
        //
        // The blade meshes spin fast enough that per-frame shadow detail
        // on them reads as stroboscopic stutter rather than a coherent
        // effect, and completely replaces the soft "moving disc" look
        // the reference viewer produces. Opting the rotor subtree out
        // of shadows preserves the clean body shadow we want on the
        // ground while letting the blades render without shadow-sample
        // interference. Walks every descendant of `Turbine_L/R`, tags
        // their referenced mesh indices, and clears casts/receives on
        // each primitive's material.
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
        tracing::info!(
            "rotor meshes opted out of shadows: {}",
            rotor_mesh_ids.len()
        );

        // ── Workaround: force red emissive on the drone's inner lens ──
        //
        // buster_drone's reference render shows a prominent red glow
        // at the drone's "eye" (the recessed nose). In this glTF that
        // red comes from a tiny circle in body_emissive.png, but the
        // FBX → glTF converter appears to have lost whatever UV /
        // material linkage placed it on the nose mesh — the authored
        // red pixel doesn't land on any visible geometry. We
        // reintroduce the glow by walking the subtree under any node
        // whose name identifies it as part of the eye/lens assembly
        // (`Drone_ILens`, `Drone_IEye`, `Eye_Pupil`, `Eye_Controller`)
        // and stamping bright-red emissive on every mesh reached that
        // way. Subtree walk matters because the actual eye mesh is a
        // child node generically named `"1"` — a name filter alone
        // would miss it.
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
                    // Bright HDR red so it blooms visibly through
                    // tonemap. Base color and other channels stay
                    // untouched; this only adds emissive, which
                    // premul-alpha blending preserves even where the
                    // base color has low alpha.
                    prim.material.emissive = [12.0, 0.0, 0.0];
                }
            }
        }
        tracing::info!("forced red emissive on {} lens meshes", lens_mesh_ids.len());

        let duration = scene.animations.first().map(clip_duration).unwrap_or(0.0);
        tracing::info!(
            "loaded {} meshes / {} nodes / {} animations (clip 0 duration = {:.2}s)",
            scene.meshes.len(),
            scene.nodes.len(),
            scene.animations.len(),
            duration
        );
        // Consume primitives out of the loaded scene into `Arc<MeshData>`
        // with `std::mem::take` — a `.clone()` here would duplicate
        // every Material's textures (multiple GB on multi-4K-texture
        // assets like buster_drone). Each primitive moves exactly once;
        // `scene.meshes[i].primitives` ends up empty but `scene.nodes`
        // + `scene.animations` are untouched so the render path still
        // drives node transforms off the live scene graph.
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
        }
    }

    /// Kick off the scene load on a background worker. On desktop this
    /// is a plain OS thread; on wasm it's `wasm_bindgen_futures::
    /// spawn_local` polling [`SceneState::try_load`] every ~100 ms
    /// until the `WebAssetLoader` cache has been populated by the
    /// preload task spun up in the wasm wrapper's setup closure.
    fn spawn_load(&self, path: &'static str) {
        let slot = self.slot.clone();

        #[cfg(not(target_arch = "wasm32"))]
        std::thread::spawn(move || {
            if let Some(state) = SceneState::try_load(path) {
                register_scheduler_tick(&state);
                let _ = slot.set(state);
                // Wake the renderer so the next frame swaps the
                // loading overlay out for real geometry. Without this
                // the scene wouldn't paint until some other event
                // (cursor move, resize, scheduler-driven redraw)
                // woke the main thread.
                get_scheduler().request_redraw();
            } else {
                tracing::error!(
                    "SceneState::try_load failed for {path} — scene will not render"
                );
            }
        });

        #[cfg(target_arch = "wasm32")]
        {
            wasm_bindgen_futures::spawn_local(async move {
                loop {
                    if let Some(state) = SceneState::try_load(path) {
                        register_scheduler_tick(&state);
                        let _ = slot.set(state);
                        get_scheduler().request_redraw();
                        break;
                    }
                    // Preload hasn't populated the cache yet. Yield
                    // back to the browser task queue and retry in
                    // 100 ms. The exponential-scale middleground
                    // (50–200 ms) is a decent compromise between
                    // snappy load-when-ready and avoiding wasted
                    // parse attempts.
                    sleep_ms(100).await;
                }
            });
        }
    }

    fn get(&self) -> Option<&SceneState> {
        self.slot.get()
    }

    /// If the scene just finished loading and `on_ready` already
    /// fired, un-pause playback. Called every frame from the render
    /// closure; cheap (atomic read + compare-exchange).
    fn autoplay_if_ready(&self) {
        if let Some(state) = self.slot.get() {
            if self.autoplay_pending.swap(false, Ordering::AcqRel) {
                state.playback.lock().unwrap().paused = false;
            }
        }
    }
}

/// Minimal wasm32 `setTimeout`-based sleep, without pulling in
/// `gloo_timers` just for one async function. Yields back to the JS
/// task queue for `ms` milliseconds.
#[cfg(target_arch = "wasm32")]
async fn sleep_ms(ms: u32) {
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;
    let promise = js_sys::Promise::new(&mut |resolve: js_sys::Function, _reject| {
        web_sys::window()
            .and_then(|w| {
                w.set_timeout_with_callback_and_timeout_and_arguments_0(
                    &resolve, ms as i32,
                )
                .ok()
            });
    });
    let _ = JsFuture::from(promise).await;
}

// Animation playback uses `blinc_skeleton::animate_scene_nodes` — it
// samples every clip channel at time `t` and writes the result into
// `scene.nodes[*].transform`. buster_drone has zero skins, so
// transform animation on scene-graph nodes is the only path needed;
// for skinned characters we'd drive `blinc_skeleton::Player` instead.

// ─────────────────────────────────────────────────────────────────────────────
// Scheduler tick — the scheduler's background thread owns the clock.
// We register a callback that receives the real `dt` each frame and
// advances our shared `Playback.time` (respecting the `paused` flag).
// `set_continuous_redraw(true)` keeps the scheduler waking the main
// thread for redraws while the animation is playing.
// ─────────────────────────────────────────────────────────────────────────────

fn register_scheduler_tick(state: &SceneState) {
    let playback = state.playback.clone();
    let scheduler_for_redraw = get_scheduler();
    let scheduler = get_scheduler();
    // The callback outlives this fn via the scheduler's tick-callback
    // SlotMap; we don't hold the `TickCallbackId` because the demo
    // process owns the callback for its full lifetime.
    //
    // `SchedulerHandle` doesn't expose `set_continuous_redraw` — that
    // switch lives on the owning `AnimationScheduler`. Instead, we
    // call `request_redraw` at the end of every tick: each invocation
    // flips the `needs_redraw` atomic that the main thread's event
    // loop picks up on its next iteration, yielding the same
    // sustained-redraw cadence without touching Blinc internals.
    scheduler.register_tick_callback(move |dt: f32| {
        {
            let mut pb = playback.lock().unwrap();
            if !pb.paused && pb.duration > 0.0 {
                pb.time += dt;
                if pb.time > pb.duration {
                    pb.time = pb.time.rem_euclid(pb.duration);
                }
            }
        }

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
    // `use_state_keyed` caches the `AsyncHandle` across UI rebuilds.
    // The init closure runs exactly once for this key — it spawns
    // the background loader and returns a handle that can be
    // polled every frame. `build_ui` returns immediately; the
    // first frame paints a loading overlay, and once the background
    // loader finishes the scene appears on its own (via
    // `get_scheduler().request_redraw()` fired from the loader).
    let handle = ctx
        .use_state_keyed("gltf_animation_demo_handle", || {
            let handle = AsyncHandle::new();
            handle.spawn_load(GLTF_PATH);
            handle
        })
        .try_get()
        .expect("handle signal should exist after use_state_keyed init");

    // ── on_ready: latch "kick off playback once ready" ────────────────
    //
    // `on_ready` fires as soon as the viewport lays out — which may
    // be well before the scene has finished loading. Flip the
    // autoplay latch instead of touching playback directly; the
    // render closure consumes the latch once `handle.get()` becomes
    // `Some`.
    let autoplay_latch = handle.autoplay_pending.clone();
    ctx.query(VIEWPORT_ID).on_ready(move |_| {
        autoplay_latch.store(true, Ordering::Release);
    });

    // ── Scene kit: orbit camera + light ───────────────────────────────
    //
    // Camera framing ideally derives from the loaded scene's
    // world-space AABB, but the scene isn't loaded yet. Use a
    // reasonable default that frames buster_drone's ~400-unit
    // ground plane; users can drag to reframe once the model
    // appears. Computing a proper frame post-load would require
    // re-writing the `OrbitCamera` signal from the render closure,
    // which is more invasive than it's worth for a demo.
    let (cam_target, cam_distance) = (Vec3::new(0.0, 20.0, 0.0), 250.0);

    // No custom HDRI — buster_drone's surfaces are matte-metallic
    // enough that the default 128²-face procedural studio cubemap
    // `SceneKit3D::new` installs carries all the IBL the scene
    // actually uses. Loading a real `.hdr` would add ~32 MB for the
    // decoded f32×4 panorama plus ~10 MB for a 512-face cubemap
    // (CPU + GPU copies) without visible quality gain. The directional
    // key light below does the heavy lifting.
    let kit = SceneKit3D::new("gltf_animation_demo")
        .with_camera(
            OrbitCamera::default()
                .with_distance(cam_distance)
                .with_elevation(0.25)
                .with_azimuth(0.6)
                .with_target(cam_target),
        )
        .with_light(Light::Directional {
            direction: Vec3::new(-0.4, -1.0, -0.3).normalize(),
            color: Color::WHITE,
            intensity: 6.0,
            cast_shadows: false,
        });

    // ── Viewport render closure ───────────────────────────────────────
    //
    // The render path doesn't advance time or call `request_redraw` —
    // both responsibilities live on the scheduler tick registered
    // when the background loader populates the slot. While the scene
    // is still loading the closure runs every frame (courtesy of the
    // viewport's normal repaint cadence) but skips the mesh-draw
    // path and lets the SceneKit3D environment + light render on
    // their own — the default procedural studio cubemap gives the
    // canvas a neutral gray backdrop, which reads as "loading" well
    // enough for this demo without a dedicated overlay widget.
    //
    // Input polling and edge-triggered-state clearing happen every
    // frame regardless of scene availability so key events aren't
    // dropped just because the model hasn't finished downloading.
    let handle_ren = handle.clone();
    let viewport = kit
        .element(move |ctx: &mut dyn DrawContext, _bounds| {
            handle_ren.autoplay_if_ready();

            // Scene not loaded yet — SceneKit3D's own environment +
            // light still paint, so the viewport shows a neutral
            // backdrop instead of a blank black rectangle. Skip the
            // mesh pipeline entirely.
            let Some(state) = handle_ren.get() else {
                handle_ren.input.frame_end();
                return;
            };

            // ── Input → playback state ────────────────────────────────
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
            let t = pb.time;
            drop(pb);

            // ── Sample animation → collect draw list → drop scene lock
            //
            // Narrowing the scene lock to only cover sampling +
            // world-transform computation keeps the GPU dispatch path
            // free of any lock held across `draw_mesh_data` calls.
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

            // ── GPU dispatch without holding the scene lock ───────────
            for (mesh_idx, xf) in draws {
                for prim in &state.arc_meshes[mesh_idx] {
                    ctx.draw_mesh_data(prim.clone(), xf);
                }
            }

            handle_ren.input.frame_end();
        })
        .capture_input(&handle.input)
        .id(VIEWPORT_ID);

    // Clip duration isn't known until the scene has loaded. Show a
    // placeholder on the header until then; a post-load rebuild
    // would re-render the header with the real value, but a rebuild
    // isn't triggered here (we poll state from the render closure
    // instead). Good enough for a demo.
    let duration = handle.get().map(|s| s.duration).unwrap_or(0.0);

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(Color::rgba(0.05, 0.06, 0.10, 1.0))
        .flex_col()
        .justify_center()
        .child(header_bar(duration))
        .child(div().flex_grow().w_full().overflow_clip().child(viewport))
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
            blinc_layout::prelude::text(title)
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
            blinc_layout::prelude::text(hints)
                .size(12.0)
                .color(Color::rgba(0.55, 0.6, 0.7, 1.0)),
        )
}
