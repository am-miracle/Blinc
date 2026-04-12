//! 3D scene canvas — orbit / zoom / pan camera over a user-drawn mesh scene.
//!
//! `SceneKit3D` is the 3D sibling of [`crate::CanvasKit`]. It owns a
//! persistent [`OrbitCamera`] and wires drag/scroll/pan input on a
//! canvas element so the demo code only has to draw.
//!
//! Momentum uses simple exponential velocity decay ticked in the canvas
//! render closure. `State<OrbitCamera>::set()` dirties the reactive
//! flag, which triggers the next frame's redraw — creating a
//! self-sustaining animation loop until velocity drops below threshold.

use std::cell::RefCell;
use std::rc::Rc;

use blinc_core::events::event_types;
use blinc_core::{
    BlincContextState, Camera, CameraProjection, DrawContext, Light, SignalId, State, Vec3,
};
use blinc_layout::canvas::{canvas, CanvasBounds};
use blinc_layout::div::{div, Div};

#[derive(Clone, Copy, Debug)]
pub struct OrbitCamera {
    pub azimuth: f32,
    pub elevation: f32,
    pub distance: f32,
    pub target: Vec3,
    pub fov_y: f32,
    pub near: f32,
    pub far: f32,
    pub vel_azimuth: f32,
    pub vel_elevation: f32,
    pub vel_zoom: f32,
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
        }
    }
}

impl OrbitCamera {
    pub fn with_distance(mut self, distance: f32) -> Self {
        self.distance = distance;
        self
    }
    pub fn with_azimuth(mut self, azimuth: f32) -> Self {
        self.azimuth = azimuth;
        self
    }
    pub fn with_elevation(mut self, elevation: f32) -> Self {
        self.elevation = elevation;
        self
    }
    pub fn with_target(mut self, target: Vec3) -> Self {
        self.target = target;
        self
    }
    pub fn with_fov_y(mut self, fov_y: f32) -> Self {
        self.fov_y = fov_y;
        self
    }

    pub fn eye(&self) -> Vec3 {
        let ce = self.elevation.cos();
        Vec3::new(
            self.target.x + self.distance * self.azimuth.sin() * ce,
            self.target.y + self.distance * self.elevation.sin(),
            self.target.z + self.distance * self.azimuth.cos() * ce,
        )
    }

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

    pub fn orbit(&mut self, dx_rad: f32, dy_rad: f32) {
        self.azimuth -= dx_rad;
        self.elevation = (self.elevation + dy_rad).clamp(-1.4, 1.4);
    }

    pub fn zoom(&mut self, factor: f32) {
        self.distance = (self.distance * factor).clamp(0.5, 200.0);
    }
}

#[derive(Clone)]
pub struct SceneKit3D {
    camera: State<OrbitCamera>,
    lights: Rc<RefCell<Vec<Light>>>,
    drag_sensitivity: f32,
    zoom_sensitivity: f32,
    /// Per-frame velocity decay. 0.95 = slow smooth deceleration,
    /// 0.85 = quick stop. Default 0.95.
    momentum_decay: f32,
}

impl SceneKit3D {
    pub fn new(key: &str) -> Self {
        let ctx = BlincContextState::get();
        Self {
            camera: ctx.use_state_keyed(&format!("{key}_cam"), OrbitCamera::default),
            lights: Rc::new(RefCell::new(Vec::new())),
            drag_sensitivity: 0.002,
            zoom_sensitivity: 0.001,
            momentum_decay: 0.95,
        }
    }

    pub fn with_camera(self, camera: OrbitCamera) -> Self {
        self.camera.set(camera);
        self
    }

    pub fn with_light(self, light: Light) -> Self {
        self.lights.borrow_mut().push(light);
        self
    }

    pub fn with_drag_sensitivity(mut self, sens: f32) -> Self {
        self.drag_sensitivity = sens;
        self
    }

    pub fn with_zoom_sensitivity(mut self, sens: f32) -> Self {
        self.zoom_sensitivity = sens;
        self
    }

    pub fn with_momentum_decay(mut self, decay: f32) -> Self {
        self.momentum_decay = decay;
        self
    }

    pub fn camera(&self) -> OrbitCamera {
        self.camera.get()
    }

    pub fn update_camera(&self, f: impl FnOnce(&mut OrbitCamera)) {
        self.camera.update(|mut c| {
            f(&mut c);
            c
        });
    }

    pub fn camera_signal(&self) -> SignalId {
        self.camera.signal_id()
    }

    pub fn set_lights(&self, lights: Vec<Light>) {
        *self.lights.borrow_mut() = lights;
    }

    pub fn element<F>(&self, render_fn: F) -> Div
    where
        F: Fn(&mut dyn DrawContext, CanvasBounds) + 'static,
    {
        let camera_drag = self.camera.clone();
        let camera_scroll = self.camera.clone();
        let camera_render = self.camera.clone();
        let lights_render = Rc::clone(&self.lights);
        let drag_sens = self.drag_sensitivity;
        let zoom_sens = self.zoom_sensitivity;
        let decay = self.momentum_decay;
        let render = Rc::new(render_fn);

        div()
            .w_full()
            .h_full()
            .on_drag(move |evt| {
                // Only SET velocity — don't call orbit(). The momentum
                // tick in the canvas render handles all movement. This
                // means mouse-down without drag doesn't interrupt
                // existing spin (no drag event = velocity persists).
                camera_drag.update(|mut c| {
                    c.vel_azimuth = evt.drag_delta_x * drag_sens;
                    c.vel_elevation = evt.drag_delta_y * drag_sens;
                    c
                });
            })
            .on_scroll(move |evt| {
                camera_scroll.update(|mut c| {
                    // Accumulate into zoom velocity — the momentum
                    // tick applies it smoothly across frames instead
                    // of jumping on each discrete scroll event.
                    c.vel_zoom += evt.scroll_delta_y * zoom_sens;
                    c
                });
            })
            .on_event(event_types::POINTER_DOWN, |_| {})
            .child(
                canvas(move |ctx, bounds| {
                    ctx.set_3d_viewport_bounds(bounds.width, bounds.height);

                    let threshold = 0.00005;
                    let mut cam = camera_render.get();

                    // Momentum: always running. The drag handler sets
                    // velocity; we apply it and decay it every frame.
                    // Mouse-down without drag doesn't fire drag events,
                    // so velocity persists → spin continues undisturbed.
                    let orbit_speed = (cam.vel_azimuth * cam.vel_azimuth
                        + cam.vel_elevation * cam.vel_elevation)
                        .sqrt();
                    let has_orbit = orbit_speed > threshold;
                    let has_zoom = cam.vel_zoom.abs() > threshold;
                    if has_orbit || has_zoom {
                        if has_orbit {
                            cam.orbit(cam.vel_azimuth, cam.vel_elevation);
                            let t = (orbit_speed * 200.0).min(1.0);
                            let adaptive = 0.80 + (decay - 0.80) * t;
                            cam.vel_azimuth *= adaptive;
                            cam.vel_elevation *= adaptive;
                        }
                        if has_zoom {
                            cam.zoom(1.0 - cam.vel_zoom);
                            cam.vel_zoom *= 0.85; // zoom decays faster for snappy feel
                        }
                        camera_render.set(cam);
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
