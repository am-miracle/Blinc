//! 3D scene canvas — orbit / zoom / pan camera over a user-drawn mesh scene.
//!
//! `SceneKit3D` is the 3D sibling of [`crate::CanvasKit`]. It owns a
//! persistent [`OrbitCamera`] plus an optional light rig and wires drag /
//! scroll / pan input on a [`blinc_layout::canvas::canvas`] so the demo
//! code only has to draw — no input handlers, no camera math, no state
//! juggling. Matches the `kit.element(|ctx, bounds| { ... })` pattern the
//! 2D kit exposes.
//!
//! Every draw happens inside a [`blinc_layout::canvas::canvas`] callback,
//! so the context the closure receives is the same `GpuPaintContext`
//! [`crate::CanvasKit::element`] hands out. Calling
//! [`blinc_core::DrawContext::draw_mesh_data`] captures the mesh + the
//! camera + the active lights into `GpuPaintContext::pending_meshes`; the
//! outer `blinc_app` render loop drains that list after `take_batch` and
//! dispatches to `GpuRenderer::render_mesh_data` against the real frame
//! target. See `crates/blinc_app/src/context.rs` for the dispatch site.
//!
//! # Example
//!
//! ```ignore
//! use blinc_canvas_kit::prelude::*;
//! use blinc_core::{Color, Light, Mat4, MeshData, Vec3};
//!
//! let kit = SceneKit3D::new("helmet_demo")
//!     .with_camera(OrbitCamera::default().with_distance(4.5))
//!     .with_light(Light::Directional {
//!         direction: Vec3::new(-0.4, -1.0, -0.3).normalize(),
//!         color: Color::WHITE,
//!         intensity: 1.2,
//!         cast_shadows: false,
//!     });
//!
//! kit.element(move |ctx, _bounds| {
//!     ctx.draw_mesh_data(&helmet, Mat4::IDENTITY);
//! })
//! ```
//!
//! # State persistence
//!
//! Like `CanvasKit`, the orbit camera is stored in a `State<OrbitCamera>`
//! keyed on the name passed to [`SceneKit3D::new`]. The state survives
//! parent rebuilds, signal-triggered redraws, and window resizes — the
//! user can drag + zoom for a minute, trigger an unrelated UI rebuild,
//! and the camera pose stays exactly where they left it.

use std::cell::RefCell;
use std::rc::Rc;

use blinc_core::events::event_types;
use blinc_core::{
    BlincContextState, Camera, CameraProjection, DrawContext, Light, SignalId, State, Vec3,
};
use blinc_layout::canvas::{canvas, CanvasBounds};
use blinc_layout::div::{div, Div};

/// Orbit camera state persisted across rebuilds by [`SceneKit3D`].
///
/// `azimuth` + `elevation` are spherical angles in radians. `distance`
/// is the linear distance from the target point in world units. All
/// orbit math stays in right-handed +Y-up coordinates, matching the
/// convention the mesh shader and the `blinc_app` view-projection
/// dispatch expect.
#[derive(Clone, Copy, Debug)]
pub struct OrbitCamera {
    /// Horizontal angle around the Y axis, in radians. 0 looks toward
    /// -Z (standard "forward" in right-handed +Y-up space).
    pub azimuth: f32,
    /// Vertical angle above the horizon, in radians. Clamped in
    /// [`OrbitCamera::orbit`] to `[-1.4, 1.4]` (~±80°) so the user
    /// can't flip past straight-up or straight-down and invert the
    /// up-vector mid-drag.
    pub elevation: f32,
    /// Linear distance from `target` to the camera eye, in world
    /// units. Clamped in [`OrbitCamera::zoom`] to `[0.5, 200.0]`.
    pub distance: f32,
    /// World-space look-at point. Panning translates this vector.
    pub target: Vec3,
    /// Vertical field of view, in radians. Stored here (not on the
    /// perspective projection struct) so the same orbit camera can be
    /// reused across multiple viewports with different aspect ratios;
    /// the actual `aspect` is computed per-frame by the dispatch loop.
    pub fov_y: f32,
    /// Near clip plane distance.
    pub near: f32,
    /// Far clip plane distance.
    pub far: f32,
    /// Orbit velocity (radians/frame) for momentum after drag release.
    pub vel_azimuth: f32,
    /// Elevation velocity for momentum.
    pub vel_elevation: f32,
    /// Zoom velocity for scroll momentum.
    pub vel_zoom: f32,
    /// Whether a drag is actively in progress. Momentum only applies
    /// when `false` — during drag the handler drives orbit directly.
    pub dragging: bool,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            azimuth: 0.0,
            elevation: 0.3,
            distance: 4.0,
            target: Vec3::ZERO,
            fov_y: 45.0_f32.to_radians(),
            near: 0.1,
            far: 100.0,
            vel_azimuth: 0.0,
            vel_elevation: 0.0,
            vel_zoom: 0.0,
            dragging: false,
        }
    }
}

impl OrbitCamera {
    /// Builder: set the initial distance from the target. Useful when
    /// you know the mesh's rough size and want to frame it cleanly.
    pub fn with_distance(mut self, distance: f32) -> Self {
        self.distance = distance;
        self
    }

    /// Builder: set the initial azimuth (radians).
    pub fn with_azimuth(mut self, azimuth: f32) -> Self {
        self.azimuth = azimuth;
        self
    }

    /// Builder: set the initial elevation (radians).
    pub fn with_elevation(mut self, elevation: f32) -> Self {
        self.elevation = elevation;
        self
    }

    /// Builder: set the look-at target in world space. Defaults to the
    /// origin, which is right for meshes centered at `(0, 0, 0)`.
    pub fn with_target(mut self, target: Vec3) -> Self {
        self.target = target;
        self
    }

    /// Builder: set the vertical field of view (radians).
    pub fn with_fov_y(mut self, fov_y: f32) -> Self {
        self.fov_y = fov_y;
        self
    }

    /// Compute the world-space eye position from the spherical
    /// `(azimuth, elevation, distance)` around `target`.
    pub fn eye(&self) -> Vec3 {
        let ce = self.elevation.cos();
        Vec3::new(
            self.target.x + self.distance * self.azimuth.sin() * ce,
            self.target.y + self.distance * self.elevation.sin(),
            self.target.z + self.distance * self.azimuth.cos() * ce,
        )
    }

    /// Turn the orbit state into a `blinc_core::Camera` the
    /// `DrawContext::set_camera` path accepts. The `aspect` baked into
    /// the perspective projection is a placeholder — the dispatch
    /// loop in `blinc_app::context::dispatch_pending_meshes` overrides
    /// it with the real frame aspect before computing view-projection.
    pub fn to_camera(&self) -> Camera {
        Camera {
            position: self.eye(),
            target: self.target,
            up: Vec3::UP,
            projection: CameraProjection::Perspective {
                fov_y: self.fov_y,
                aspect: 1.0,
                near: self.near,
                far: self.far,
            },
        }
    }

    /// Apply an orbit delta in radians. Elevation is clamped to
    /// `[-1.4, 1.4]` (~±80°) to prevent flipping past vertical, which
    /// would invert the camera up-vector mid-drag and make the scene
    /// jump discontinuously.
    pub fn orbit(&mut self, dx_rad: f32, dy_rad: f32) {
        self.azimuth -= dx_rad;
        self.elevation = (self.elevation + dy_rad).clamp(-1.4, 1.4);
    }

    /// Scale distance by `factor`. Typical wheel scroll feeds in
    /// values like `1.0 - scroll_delta_y * 0.001`, where positive
    /// `scroll_delta_y` (scroll up) zooms in (distance shrinks).
    /// Distance is clamped to `[0.5, 200.0]`.
    pub fn zoom(&mut self, factor: f32) {
        self.distance = (self.distance * factor).clamp(0.5, 200.0);
    }

    /// Pan the target point in world-space XY. For a first-cut demo
    /// this treats the pan axes as world axes rather than the camera's
    /// local right/up; it works for a mostly-horizontal orbit and
    /// breaks down when the camera is looking straight down. Proper
    /// camera-space pan is a follow-up when a demo needs it.
    pub fn pan(&mut self, dx: f32, dy: f32) {
        self.target.x -= dx;
        self.target.y += dy;
    }
}

/// 3D scene canvas kit: persistent orbit camera + light rig + a
/// `kit.element(|ctx, bounds| { ... })` builder matching the 2D
/// [`crate::CanvasKit`] shape.
///
/// All state survives UI rebuilds via `BlincContextState::use_state_keyed`.
/// Clone the kit freely — every clone points at the same underlying
/// state signal, so event handlers and render closures captured in
/// different places all see the same camera.
#[derive(Clone)]
pub struct SceneKit3D {
    /// Persistent orbit camera state. Event handlers mutate this via
    /// `.update(...)`, which dirties the reactive signal and schedules
    /// a redraw automatically.
    camera: State<OrbitCamera>,
    /// Lights attached to the kit, rendered every frame. `RefCell`
    /// rather than `State` because lights rarely change at runtime and
    /// the reactive overhead isn't worth it for a fixed light rig.
    /// Wrapped in `Rc` so clones share the same list.
    lights: Rc<RefCell<Vec<Light>>>,
    /// Radians-per-pixel for drag orbit. Tuned so a ~200px drag on a
    /// typical viewport rotates roughly 60°. The original `0.01` was
    /// too fast — a slow deliberate drag flew past the intended
    /// facing. `0.005` lands in the "1:1 with thumb speed" range on
    /// a trackpad, which is what most DCC tools target.
    drag_sensitivity: f32,
    /// Multiplicative zoom factor per scroll-wheel tick. Small so the
    /// motion feels damped; the sign is flipped inside `on_scroll` so
    /// scroll-up = zoom-in.
    zoom_sensitivity: f32,
    /// Per-frame velocity decay factor for orbit/zoom momentum after
    /// drag release. `0.0` = instant stop, `1.0` = no decay (infinite
    /// coast). Default `0.92` gives a quick but visible deceleration
    /// that feels like a weighted turntable.
    momentum_decay: f32,
}

impl SceneKit3D {
    /// Create a scene kit with persistent state keyed by `key`.
    ///
    /// The camera state is stored at `"{key}_cam"` and survives UI
    /// rebuilds. Pick a distinct key per viewport so two `SceneKit3D`
    /// instances on the same page don't share a camera.
    pub fn new(key: &str) -> Self {
        let ctx = BlincContextState::get();
        Self {
            camera: ctx.use_state_keyed(&format!("{key}_cam"), OrbitCamera::default),
            lights: Rc::new(RefCell::new(Vec::new())),
            drag_sensitivity: 0.002,
            zoom_sensitivity: 0.001,
            momentum_decay: 0.92,
        }
    }

    /// Builder: replace the initial camera with a tuned one. Only
    /// takes effect on first creation — subsequent rebuilds see the
    /// user-mutated state unchanged.
    pub fn with_camera(self, camera: OrbitCamera) -> Self {
        // Overwrite even if the state was already initialized — this
        // lets a demo switch camera framing on a reset button without
        // leaking stale orbit values.
        self.camera.set(camera);
        self
    }

    /// Builder: attach a light to the rig. Called multiple times to
    /// add directional + ambient + point sources. The current mesh
    /// pipeline only uses the first `Light::Directional` for shading,
    /// but storing the full list here lets future pipeline upgrades
    /// pick them up without touching demo code.
    pub fn with_light(self, light: Light) -> Self {
        self.lights.borrow_mut().push(light);
        self
    }

    /// Builder: override the drag-to-orbit radians-per-pixel sensitivity.
    /// Default is `0.01`.
    pub fn with_drag_sensitivity(mut self, sens: f32) -> Self {
        self.drag_sensitivity = sens;
        self
    }

    /// Builder: override the scroll-to-zoom sensitivity (per wheel tick).
    /// Default is `0.001`.
    pub fn with_zoom_sensitivity(mut self, sens: f32) -> Self {
        self.zoom_sensitivity = sens;
        self
    }

    /// Current orbit camera snapshot. Use this for HUD overlays that
    /// display distance / angles, same pattern as `CanvasKit::viewport`.
    pub fn camera(&self) -> OrbitCamera {
        self.camera.get()
    }

    /// Mutate the orbit camera from outside an event handler (e.g. a
    /// reset button, a programmatic fly-to-target animation).
    pub fn update_camera(&self, f: impl FnOnce(&mut OrbitCamera)) {
        self.camera.update(|mut c| {
            f(&mut c);
            c
        });
    }

    /// Signal ID for the camera state. Pass into a `stateful` widget's
    /// `.deps([...])` list if you want a sibling UI element (HUD, label,
    /// inspector) to rebuild every time the camera moves.
    pub fn camera_signal(&self) -> SignalId {
        self.camera.signal_id()
    }

    /// Replace the entire light list. Rarely needed — `with_light`
    /// accumulates, which is the usual flow.
    pub fn set_lights(&self, lights: Vec<Light>) {
        *self.lights.borrow_mut() = lights;
    }

    /// Build a fully wired `Div` containing a `canvas` that draws the
    /// 3D scene. The render closure receives the same
    /// `&mut dyn DrawContext` the 2D `kit.element` path does — except
    /// the camera and all lights attached to this kit are pushed onto
    /// the context *before* the closure runs, so the closure body can
    /// just call `ctx.draw_mesh_data(&mesh, transform)` and be done.
    ///
    /// Input handling wired in the returned Div:
    /// - `on_drag` → [`OrbitCamera::orbit`]
    /// - `on_scroll` → [`OrbitCamera::zoom`]
    /// - `POINTER_DOWN` + shift modifier → future pan (not wired yet)
    ///
    /// State mutations dirty the reactive flag, so the canvas
    /// re-renders on the next frame automatically. No `stateful`
    /// wrapper needed on the caller side — the canvas render closure
    /// reads `camera.get()` fresh each frame.
    pub fn element<F>(&self, render_fn: F) -> Div
    where
        F: Fn(&mut dyn DrawContext, CanvasBounds) + 'static,
    {
        let camera_drag = self.camera.clone();
        let camera_drag_end = self.camera.clone();
        let camera_scroll = self.camera.clone();
        let camera_render = self.camera.clone();
        let lights_render = Rc::clone(&self.lights);
        let drag_sens = self.drag_sensitivity;
        let zoom_sens = self.zoom_sensitivity;
        let momentum_decay = self.momentum_decay;
        let render = Rc::new(render_fn);

        div()
            .w_full()
            .h_full()
            .on_drag(move |evt| {
                camera_drag.update(|mut c| {
                    let vx = evt.drag_delta_x * drag_sens;
                    let vy = evt.drag_delta_y * drag_sens;
                    c.orbit(vx, vy);
                    c.vel_azimuth = vx;
                    c.vel_elevation = vy;
                    c.dragging = true;
                    c
                });
            })
            .on_drag_end(move |_| {
                camera_drag_end.update(|mut c| {
                    c.dragging = false;
                    c
                });
            })
            .on_scroll(move |evt| {
                camera_scroll.update(|mut c| {
                    let zd = evt.scroll_delta_y * zoom_sens;
                    c.zoom(1.0 - zd);
                    c.vel_zoom = -zd;
                    c
                });
            })
            .on_event(event_types::POINTER_DOWN, |_| {})
            .child(
                canvas(move |ctx, bounds| {
                    ctx.set_3d_viewport_bounds(bounds.width, bounds.height);

                    // ── Momentum tick ────────────────────────────────
                    // Apply residual velocity with exponential decay
                    // when the user isn't actively dragging. The state
                    // mutation triggers a next-frame redraw, creating a
                    // self-sustaining animation loop until velocity
                    // drops below the threshold.
                    let threshold = 0.00005;
                    let mut cam = camera_render.get();
                    if !cam.dragging {
                        let has_momentum = cam.vel_azimuth.abs() > threshold
                            || cam.vel_elevation.abs() > threshold
                            || cam.vel_zoom.abs() > threshold;
                        if has_momentum {
                            cam.orbit(cam.vel_azimuth, cam.vel_elevation);
                            if cam.vel_zoom.abs() > threshold {
                                cam.zoom(1.0 + cam.vel_zoom);
                            }
                            cam.vel_azimuth *= momentum_decay;
                            cam.vel_elevation *= momentum_decay;
                            cam.vel_zoom *= momentum_decay;
                            camera_render.set(cam);
                        }
                    }

                    let camera = cam.to_camera();
                    ctx.set_camera(&camera);
                    for light in lights_render.borrow().iter() {
                        ctx.add_light(light.clone());
                    }
                    render(ctx, bounds);
                })
                .w_full()
                .h_full(),
            )
    }
}
