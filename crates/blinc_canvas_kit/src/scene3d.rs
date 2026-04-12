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
use std::sync::Arc;

use blinc_core::draw::{Material, MeshData, Vertex};
use blinc_core::events::event_types;
use blinc_core::layer::CubemapData;
use blinc_core::{
    BlincContextState, Camera, CameraProjection, DrawContext, Light, Mat4, SignalId, State, Vec3,
};
use blinc_layout::canvas::{canvas, CanvasBounds};
use blinc_layout::div::{div, Div};

// ─────────────────────────────────────────────────────────────────────────────
// Scene Objects
// ─────────────────────────────────────────────────────────────────────────────

/// Opaque handle to a mesh in the scene. Returned by `SceneKit3D::add`
/// and used to update transforms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MeshHandle(usize);

/// A single object in the scene graph.
#[derive(Clone)]
struct SceneObject {
    mesh: Arc<MeshData>,
    position: Vec3,
    rotation: Vec3,
    scale: Vec3,
    visible: bool,
}

impl SceneObject {
    fn transform(&self) -> Mat4 {
        let t = Mat4::translation(self.position.x, self.position.y, self.position.z);
        let s = Mat4::scale(self.scale.x, self.scale.y, self.scale.z);
        let ry = Mat4::rotation_y(self.rotation.y);
        // T × Ry × S (rotation around other axes can be added later)
        t.mul(&ry).mul(&s)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Procedural Environment Cubemap
// ─────────────────────────────────────────────────────────────────────────────

/// Pre-generated cubemap face data for the IBL environment.
///
/// Each entry in `faces` is one mip level of one face (f16 RGBA bytes).
/// Layout: `faces[face * mip_count + mip]` for face 0..6, mip 0..mip_count.
#[derive(Clone)]
pub struct EnvironmentData {
    /// Underlying cubemap data (face bytes, base size, mip count).
    pub cubemap: Arc<CubemapData>,
}

/// Generate a studio environment cubemap at the given base resolution.
///
/// Produces a warm key / cool fill / ground bounce lighting setup that
/// gives PBR metals and glass surfaces ambient reflections proportional
/// to roughness, mimicking the studio HDRI setups used by Sketchfab and
/// Marmoset Toolbag.
pub fn generate_studio_environment(size: u32) -> EnvironmentData {
    let mip_count = (size as f32).log2() as u32 + 1;
    let mut faces = Vec::with_capacity((6 * mip_count) as usize);
    for face in 0..6u32 {
        for mip in 0..mip_count {
            let mip_size = (size >> mip).max(1);
            faces.push(generate_cubemap_face(face, mip_size));
        }
    }
    EnvironmentData {
        cubemap: Arc::new(CubemapData {
            faces,
            size,
            mip_count,
        }),
    }
}

/// Generate one face of a studio environment cubemap at the given
/// resolution. Returns **f16 RGBA** bytes (4 x u16 per texel) for the
/// `Rgba16Float` cubemap format. The environment has:
///
/// - A warm key area light from the upper-right front
/// - A cool fill area light from the left-behind
/// - A subtle warm ground bounce from below
/// - A neutral gradient sky base
///
/// The area lights produce HDR values > 1.0 so the mesh shader's
/// Cook-Torrance specular picks up distinct bright reflections
/// instead of reflecting a featureless gradient.
fn generate_cubemap_face(face: u32, size: u32) -> Vec<u8> {
    // 4 x f16 per texel = 8 bytes per texel
    let mut data = Vec::with_capacity((size * size * 8) as usize);
    for y in 0..size {
        for x in 0..size {
            let u = (x as f32 + 0.5) / size as f32 * 2.0 - 1.0;
            let v = (y as f32 + 0.5) / size as f32 * 2.0 - 1.0;

            let (dx, dy, dz) = match face {
                0 => (1.0, -v, -u),
                1 => (-1.0, -v, u),
                2 => (u, 1.0, v),
                3 => (u, -1.0, -v),
                4 => (u, -v, 1.0),
                _ => (-u, -v, -1.0),
            };

            let len = (dx * dx + dy * dy + dz * dz).sqrt();
            let (nx, ny, nz) = (dx / len, dy / len, dz / len);

            // Base sky gradient
            let (mut r, mut g, mut b) = if ny > 0.0 {
                let t = (1.0 - ny).powf(4.0);
                (0.08 + t * 0.35, 0.08 + t * 0.33, 0.10 + t * 0.28)
            } else {
                let t = (1.0 + ny).powf(2.0);
                (0.06 + t * 0.20, 0.05 + t * 0.18, 0.04 + t * 0.15)
            };

            // Virtual area lights — soft gaussian blobs on the cubemap sphere
            let area_lights: &[(f32, f32, f32, f32, f32, f32, f32)] = &[
                //  dir_x  dir_y  dir_z  radius  R     G     B
                (0.5, 0.4, 0.7, 0.15, 5.0, 4.5, 3.8), // warm key (upper-right front)
                (-0.7, 0.2, -0.4, 0.20, 1.2, 1.4, 1.8), // cool fill (left-behind)
                (0.0, -0.7, 0.3, 0.30, 0.8, 0.6, 0.5), // warm ground bounce
                (-0.3, 0.8, 0.0, 0.25, 0.5, 0.55, 0.6), // subtle top fill (cool)
                (0.8, 0.0, -0.5, 0.12, 2.5, 2.3, 2.0), // rim accent (right side)
            ];

            for &(lx, ly, lz, radius, lr, lg, lb) in area_lights {
                let ll = (lx * lx + ly * ly + lz * lz).sqrt();
                let dot = (nx * lx + ny * ly + nz * lz) / ll;
                let cos_edge = 1.0 - radius;
                if dot > cos_edge {
                    let t = ((dot - cos_edge) / radius).min(1.0);
                    let intensity = t * t;
                    r += lr * intensity;
                    g += lg * intensity;
                    b += lb * intensity;
                }
            }

            for &val in &[r, g, b, 1.0f32] {
                data.extend_from_slice(&f32_to_f16(val).to_le_bytes());
            }
        }
    }
    data
}

/// Convert f32 to IEEE 754 half-precision (f16) as a u16.
fn f32_to_f16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xFF) as i32;
    let mantissa = bits & 0x7FFFFF;

    if exp == 0 {
        return sign;
    }
    if exp == 0xFF {
        return sign | 0x7C00 | if mantissa != 0 { 0x0200 } else { 0 };
    }

    let new_exp = exp - 127 + 15;
    if new_exp >= 31 {
        return sign | 0x7C00;
    }
    if new_exp <= 0 {
        return sign;
    }

    sign | ((new_exp as u16) << 10) | ((mantissa >> 13) as u16)
}

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
    objects: Rc<RefCell<Vec<SceneObject>>>,
    drag_sensitivity: f32,
    zoom_sensitivity: f32,
    momentum_decay: f32,
    environment: Arc<CubemapData>,
}

impl SceneKit3D {
    pub fn new(key: &str) -> Self {
        let ctx = BlincContextState::get();
        let env = generate_studio_environment(128);
        Self {
            camera: ctx.use_state_keyed(&format!("{key}_cam"), OrbitCamera::default),
            lights: Rc::new(RefCell::new(Vec::new())),
            objects: Rc::new(RefCell::new(Vec::new())),
            drag_sensitivity: 0.002,
            zoom_sensitivity: 0.001,
            momentum_decay: 0.95,
            environment: env.cubemap,
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

    /// Replace the IBL environment cubemap used for reflections.
    pub fn with_environment(mut self, env: EnvironmentData) -> Self {
        self.environment = env.cubemap;
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

    /// Add an infinity ground-plane grid rendered on Y=0 via the
    /// `CustomRenderPass` system at the `Scene3D` stage. The grid
    /// shader uses analytical ray-plane intersection with anti-aliased
    /// lines and distance fade — no geometry, just a fullscreen triangle.
    ///
    /// The pass is registered with the GPU renderer via
    /// `BlincContextState::register_custom_pass` so it works from
    /// closures without needing direct renderer access.
    pub fn with_grid(self) -> Self {
        let grid = crate::grid_pass::GridPass::new();
        let boxed: Box<dyn blinc_gpu::custom_pass::CustomRenderPass> = Box::new(grid);
        // On wasm32, CustomRenderPass is !Send (wgpu types are !Send
        // on the browser main thread). Wrapping in a Send shim is safe
        // because wasm is single-threaded — there's no other thread to
        // send to. On native, CustomRenderPass: Send so no shim needed.
        #[cfg(target_arch = "wasm32")]
        {
            #[allow(dead_code)]
            struct SendWrapper(Box<dyn blinc_gpu::custom_pass::CustomRenderPass>);
            // SAFETY: wasm32 is single-threaded; Send is vacuously safe.
            unsafe impl Send for SendWrapper {}
            let wrapper = SendWrapper(boxed);
            let type_erased: Box<dyn std::any::Any + Send> = Box::new(wrapper);
            BlincContextState::get().register_custom_pass(type_erased);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let type_erased: Box<dyn std::any::Any + Send> = Box::new(boxed);
            BlincContextState::get().register_custom_pass(type_erased);
        }
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

    // ── Scene object management ─────────────────────────────────────

    /// Add a mesh from geometry + material. Returns a handle for
    /// updating the object's transform later.
    pub fn add(
        &self,
        geometry: (Vec<Vertex>, Vec<u32>),
        material: impl Into<Material>,
    ) -> MeshHandle {
        let (vertices, indices) = geometry;
        let mesh = Arc::new(MeshData {
            vertices,
            indices,
            material: material.into(),
            skin: None,
        });
        let mut objects = self.objects.borrow_mut();
        let handle = MeshHandle(objects.len());
        objects.push(SceneObject {
            mesh,
            position: Vec3::ZERO,
            rotation: Vec3::ZERO,
            scale: Vec3::ONE,
            visible: true,
        });
        handle
    }

    /// Add a pre-built `MeshData` (e.g. loaded from glTF).
    pub fn add_mesh(&self, mesh: Arc<MeshData>) -> MeshHandle {
        let mut objects = self.objects.borrow_mut();
        let handle = MeshHandle(objects.len());
        objects.push(SceneObject {
            mesh,
            position: Vec3::ZERO,
            rotation: Vec3::ZERO,
            scale: Vec3::ONE,
            visible: true,
        });
        handle
    }

    pub fn set_position(&self, handle: MeshHandle, position: Vec3) {
        if let Some(obj) = self.objects.borrow_mut().get_mut(handle.0) {
            obj.position = position;
        }
    }

    pub fn set_rotation(&self, handle: MeshHandle, rotation: Vec3) {
        if let Some(obj) = self.objects.borrow_mut().get_mut(handle.0) {
            obj.rotation = rotation;
        }
    }

    pub fn set_scale(&self, handle: MeshHandle, scale: Vec3) {
        if let Some(obj) = self.objects.borrow_mut().get_mut(handle.0) {
            obj.scale = scale;
        }
    }

    pub fn set_visible(&self, handle: MeshHandle, visible: bool) {
        if let Some(obj) = self.objects.borrow_mut().get_mut(handle.0) {
            obj.visible = visible;
        }
    }

    /// Render all scene objects. Call this inside the `element()`
    /// render closure, or use the no-arg `element_auto()` which calls
    /// it for you.
    pub fn render_scene(&self, ctx: &mut dyn DrawContext) {
        let objects = self.objects.borrow();
        for obj in objects.iter() {
            if obj.visible {
                ctx.draw_mesh_data(Arc::clone(&obj.mesh), obj.transform());
            }
        }
    }

    /// Build a fully wired Div that automatically renders all meshes
    /// added via `add()` / `add_mesh()`. No render closure needed —
    /// the scene manages its own drawing.
    pub fn element_auto(&self) -> Div {
        let kit = self.clone();
        self.element(move |ctx, _bounds| {
            kit.render_scene(ctx);
        })
    }

    pub fn element<F>(&self, render_fn: F) -> Div
    where
        F: Fn(&mut dyn DrawContext, CanvasBounds) + 'static,
    {
        let camera_drag = self.camera.clone();
        let camera_scroll = self.camera.clone();
        let camera_render = self.camera.clone();
        let lights_render = Rc::clone(&self.lights);
        let env_data = Arc::clone(&self.environment);
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
                    ctx.set_environment_cubemap(Arc::clone(&env_data));
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
