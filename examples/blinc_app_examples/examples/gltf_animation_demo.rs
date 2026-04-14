//! Smoke test for the downstream-packages canvas stack.
//!
//! Exercises, end-to-end:
//!
//! - `blinc_gltf`: load a real glTF asset (buster_drone) — 39 meshes,
//!   92 nodes, 1 animation with 100 transform channels.
//! - `blinc_skeleton`: sample the animation clip into node transforms
//!   each frame (no skins in this asset — pure transform animation).
//! - `blinc_canvas_kit::SceneKit3D`: orbit camera, HDRI lighting, PBR
//!   rendering via the immediate-mode `draw_mesh_data` path.
//! - `blinc_canvas_kit::SketchEvents::on_canvas_events`: one-call event
//!   forwarding from the scene's `Div` into…
//! - `blinc_input::InputState`: polling keys inside the render closure.
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

use std::sync::{Arc, Mutex};

use blinc_animation::get_scheduler;
use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_canvas_kit::prelude::*;
use blinc_core::events::KeyCode;
use blinc_core::{Color, DrawContext, Light, Mat4, MeshData, Vec3};
use blinc_gltf::{GltfAnimation, GltfScene};
use blinc_input::{DivInputExt, InputState};

// Workspace-relative paths — `cargo run -p blinc_app_examples --example ...`
// resolves from the repo root, not the crate root.
const ASSETS_3D: &str = "examples/blinc_app_examples/examples/assets/3d";
const GLTF_PATH: &str = "examples/blinc_app_examples/examples/assets/3d/buster_drone/scene.gltf";

/// ID on the viewport Div — used by `ctx.query(...).on_ready(...)` to
/// kick off animation playback the first time layout completes.
const VIEWPORT_ID: &str = "gltf-animation-viewport";

// ─────────────────────────────────────────────────────────────────────────────
// Shared state — all UI-rebuild-persistent data lives on a single
// struct so we can stash it once in `BlincContextState` and clone the
// shared-Arc handle into each closure.
// ─────────────────────────────────────────────────────────────────────────────

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
    input: InputState,
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
    fn load(path: &str) -> Self {
        // Cross-platform: goes through `blinc_platform::assets` so the
        // same demo code runs on desktop (filesystem), Android (APK
        // assets), iOS (bundle), and web (HTTP) once the right
        // platform loader is registered. On desktop the default
        // `FilesystemAssetLoader` reads from the CWD, which matches
        // `cargo run` resolving from the repo root.
        let mut scene =
            blinc_gltf::load_asset(path).unwrap_or_else(|e| panic!("failed to load {path}: {e}"));

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
        Self {
            scene: Arc::new(Mutex::new(scene)),
            arc_meshes: Arc::new(arc_meshes),
            animation: Arc::new(animation),
            playback: Arc::new(Mutex::new(Playback {
                time: 0.0,
                duration,
                paused: true,
            })),
            input: InputState::new(),
            duration,
        }
    }
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
    // Cache the loaded scene across UI rebuilds. `use_state_keyed`'s
    // init closure runs exactly once for this string key, so the
    // 9-second glTF parse + texture decode happens once at startup
    // and subsequent rebuilds (window resize, etc.) reuse the same
    // `SceneState` handle. The same closure also registers a tick
    // callback on the global animation scheduler and turns on
    // continuous redraw — both one-shot setups that would double up
    // if attached per-`build_ui` invocation.
    let state = ctx
        .use_state_keyed("gltf_animation_demo_state", || {
            let state = SceneState::load(GLTF_PATH);
            register_scheduler_tick(&state);
            state
        })
        .try_get()
        .expect("state signal should exist after use_state_keyed init");
    let duration = state.duration;

    // ── on_ready: kick off playback once the viewport is laid out ─────
    //
    // The scene starts paused so the first frame renders the rest
    // pose, then `on_ready` unpauses (fires exactly once per stable
    // element id, persisting through rebuilds). The scheduler tick
    // itself was registered by `SceneState::load`'s caller above — it
    // advances time on its own thread whether or not the viewport is
    // visible, but respects `paused`.
    let start_playback = state.playback.clone();
    ctx.query(VIEWPORT_ID).on_ready(move |_| {
        start_playback.lock().unwrap().paused = false;
    });

    // ── Scene kit: orbit camera + HDRI ────────────────────────────────
    //
    // Framing is computed from the loaded scene's world-space AABB
    // instead of being hardcoded — buster_drone's ground plane is
    // ±180 units wide and its drone body ~40 units across, so a
    // hardcoded `distance = 8.0` (authored for DamagedHelmet-scale
    // meshes) would sit inside the ground-plane geometry. Framing the
    // diagonal times ~1.1 pulls the camera just outside the scene
    // AABB, keeping the whole asset in view regardless of authoring
    // units.
    let (cam_target, cam_distance) = frame_camera(state.scene.lock().unwrap().world_aabb());
    tracing::info!(
        "camera: target=({:.2},{:.2},{:.2}) distance={:.2}",
        cam_target.x,
        cam_target.y,
        cam_target.z,
        cam_distance
    );

    let hdr_path = format!("{ASSETS_3D}/rogland_clear_night_2k.hdr");
    let hdr_bytes = blinc_platform::assets::load_asset(&hdr_path)
        .unwrap_or_else(|e| panic!("failed to load HDRI {hdr_path}: {e}"));
    let kit = SceneKit3D::new("gltf_animation_demo")
        .with_camera(
            OrbitCamera::default()
                .with_distance(cam_distance)
                .with_elevation(0.25)
                .with_azimuth(0.6)
                .with_target(cam_target),
        )
        // Larger face size (512) + brighter key light — the
        // buster_drone's mostly-metallic surfaces pick up almost all
        // of their visible color from IBL reflections, so resolution
        // matters. Intensity bumped from the mesh_3d_demo default
        // because this scene has more surface area competing for the
        // same key light, plus a night-sky HDRI that barely
        // contributes ambient on its own.
        .with_hdri(&hdr_bytes, 512)
        .with_light(Light::Directional {
            direction: Vec3::new(-0.4, -1.0, -0.3).normalize(),
            color: Color::WHITE,
            intensity: 6.0,
            cast_shadows: false,
        });

    // ── Viewport render closure ───────────────────────────────────────
    //
    // The render path doesn't advance time or call `request_redraw` —
    // both responsibilities live on the scheduler tick registered in
    // `register_scheduler_tick`. Input polling stays here because the
    // user's intent is expressed at render cadence (pressing space
    // between two frames pauses for the next frame), and clearing
    // edge-triggered state at the tail matches that.
    let state_ren = state.clone();
    let viewport = kit
        .element(move |ctx: &mut dyn DrawContext, _bounds| {
            // ── Input → playback state ────────────────────────────────
            let mut pb = state_ren.playback.lock().unwrap();
            if state_ren.input.is_key_just_pressed(KeyCode::SPACE) {
                pb.paused = !pb.paused;
            }
            if state_ren.input.is_key_just_pressed(KeyCode(b'R' as u32)) {
                pb.time = 0.0;
            }
            if state_ren.input.is_key_down(KeyCode::LEFT) {
                pb.time = (pb.time - 1.0 / 60.0).max(0.0);
            }
            if state_ren.input.is_key_down(KeyCode::RIGHT) {
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
                let mut scene_mut = state_ren.scene.lock().unwrap();
                if let Some(anim) = state_ren.animation.as_ref() {
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
                for prim in &state_ren.arc_meshes[mesh_idx] {
                    ctx.draw_mesh_data(prim.clone(), xf);
                }
            }

            state_ren.input.frame_end();
        })
        .capture_input(&state.input)
        .id(VIEWPORT_ID);

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

/// Pick orbit-camera (target, distance) from an optional scene AABB.
/// Falls back to a safe default if the scene has no mesh-bearing nodes.
fn frame_camera(aabb: Option<([f32; 3], [f32; 3])>) -> (Vec3, f32) {
    let (min, max) = match aabb {
        Some(v) => v,
        None => return (Vec3::new(0.0, 0.0, 0.0), 8.0),
    };
    let center = Vec3::new(
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    );
    let dx = max[0] - min[0];
    let dy = max[1] - min[1];
    let dz = max[2] - min[2];
    let diag = (dx * dx + dy * dy + dz * dz).sqrt();
    // Multiplier tuned so the AABB's corners sit just inside the
    // default 45° FOV without clipping — roughly `tan(22.5°)^-1`
    // plus some padding.
    let distance = (diag * 1.1).max(1.0);
    (center, distance)
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
