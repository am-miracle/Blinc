# Blinc Render Architecture

This document describes how a Blinc frame turns input and reactive state into
pixels. The pipeline is **damage-aware and cache-first**: after the initial
paint, most frames patch a cached GPU texture and re-dispatch only the
primitives that actually changed.

## High-level diagram

```
Input / signal change
        │
        ▼
┌─────────────────────────────────────────────────────────┐
│  Phase 1  diff + (maybe) subtree rebuild                │
│  Phase 2  layout (Taffy, only on dirty nodes)           │
│  Phase 3  tick: springs, CSS keyframes, transitions     │
│  Phase 4  paint                                         │
│           ├─ try_fast_paint?                            │
│           │    yes ──► apply_binding_deltas (motion)    │
│           │            apply_css_deltas    (CSS anims)  │
│           │            render_static_layer_damaged ────►│
│           │                                             │
│           └─ no  ──► full walker → static cache rebuild │
│  Phase 4b overlay / motion subtree blits                │
│  Phase 5  composite_frame   blit cache + overlays       │
│  Phase 5  present                                       │
│           │                                             │
│           ▼                                             │
│  Phase 5  request_redraw chain if anything mid-flight   │
└─────────────────────────────────────────────────────────┘
```

The five-phase split is implemented in the desktop runner, with mirrored
loops on each platform (Android, iOS, Fuchsia, web).

## Why this matters

A naive UI renderer clears the swapchain and re-rasterises every primitive
every vsync. Blinc avoids that for the common case (hover, focus, scroll,
CSS keyframe tick, spring tick on a single element) by keeping the last
painted state in a GPU-side cache and only repainting what changed.

Net effect for a tree with `M` primitives and `N` changing this frame:

| Frame kind                                | GPU work                                |
|-------------------------------------------|-----------------------------------------|
| First paint of a tree state               | Full walker, `M` SDF dispatches         |
| Hover / animation tick on cached subtree  | Blit cache + `N` dispatches scissored to damage rect |
| Layout change (size, padding, flex, …)    | Invalidate cache, full re-walk          |
| Tree rebuild (stateful flip, signal-driven structural change) | Diff + targeted subtree rebuild + cache invalidation |

The swapchain image still gets a complete frame every vsync (swapchain
textures aren't content-preserving). What's avoided is the fragment-shader
cost of re-rasterising every primitive.

## The five phases

The phase numbering is load-bearing. Code, traces (`frame_timing`), and
gotcha docs all reference it.

### Phase 1: diff

Reactive signals fired since the last frame determine whether a subtree
needs a structural rebuild. `incremental_update` compares hashes per node
and classifies the change:

- `VisualOnly`: apply prop updates, no rebuild.
- `ChildrenChanged`: rebuild the affected subtree, mark layout dirty.

Most reactive updates don't cause a tree rebuild. They land in dynamic
bindings the compositor patches in Phase 4 instead.

### Phase 2: layout

Taffy `compute_layout()` runs on dirty nodes only. Stylesheet layout
overrides (`apply_stylesheet_layout_overrides`) are applied *before*
`compute_layout` so CSS-driven width, padding, and gap take effect.

If a subtree was rebuilt or stylesheet layout overrides changed, the render
cache is invalidated here.

### Phase 3: tick

Springs, CSS keyframe animations, CSS transitions, scroll physics, and
motion bindings all advance.

The order matters:

```
tick_css_animations
tick_css_transitions
apply_stylesheet_state_styles
apply_all_css_animation_props      ← overrides base state styles
apply_all_css_transition_props
apply_animated_layout_props        ← compute_layout() if a layout prop animated
```

### Phase 4: paint

The frame loop branches on a `try_fast_paint` predicate:

```rust
try_fast_paint =
       !did_rebuild
    && !needs_relayout
    && !css_blocks_fast            // no layout-affecting CSS change
    && !scroll_animating
    && !bounds_anim_active         // no bounds / clip / size animation
    && !new_overlay_active
    && last_paint_time_ms != 0
    && has_render_cache();
```

If any gate trips, the full walker runs. Otherwise the compositor fast path
takes over.

#### Fast path A: motion bindings (`apply_binding_deltas`)

Patches the **dynamic batch** (motion-bound primitives) in place, no tree
walk. The properties it can rewrite directly into GPU primitive slots:

| Property                       | Field patched                                                                                |
|--------------------------------|----------------------------------------------------------------------------------------------|
| `translate_x` / `translate_y`  | `bounds[0..2]`                                                                               |
| `scale`, `scale_x`, `scale_y`  | `bounds` + `local_affine` around stored centre                                               |
| `rotation` (2D)                | `rotation[0..1]` (sin/cos)                                                                   |
| `opacity`                      | `color[3]`, `border_color[3]`, `shadow_color[3]` (or `LayerConfig.opacity` when layered)     |

Damage rects (pre and post AABB of each changed primitive, padded) are
accumulated into `last_binding_damage_rects` for the GPU pass.

Bails if: cache absent, opacity ~ 0 (would divide by zero), or scale
degenerate.

#### Fast path B: CSS deltas (`apply_css_deltas`)

Patches the **static batch** in place for CSS-animated properties:
`opacity`, `background_color`, `border_color`, `border_width`,
`corner_radius`, `shadow_params`, `shadow_color`, `rotate_x` / `rotate_y` /
`rotate_z`, plus `translate` and `scale` on composite-promotable subtrees.

Bails to a walker re-emit if the active animation set includes a property
the fast path can't rewrite in place: `clip_inset`, `clip_circle_radius`,
`filter_blur`, `width` / `height`, `margin` / `padding` / `gap`,
`backdrop_blur`, `backdrop_saturation`, `backdrop_brightness`.

Damage rects accumulate into `last_css_damage_rects`.

#### Damage-rect re-dispatch (`render_static_layer_damaged`)

Given a set of damage rects, the GPU pass:

1. Computes the **union** of the damage rects (padded by `AABB_PAD = 4 px`
   to cover SDF antialias edges) and clamps it to the layer extent.
2. Sets that as the `scissor_rect` on a `LoadOp::Load` pass.
3. Dispatches a REPLACE-blend clear quad to zero the scissor region.
4. Re-dispatches **only** SDF primitives that intersect the scissor.

Bails to a full layer rerender when the batch contains primitives the
damage path can't yet handle: vector paths, 3D viewports, particles. Text,
SVG, and image dispatch through the damage path is scaffolded but gated
behind `BLINC_DAMAGE_RECT=1`.

### Phase 5: composite and present

`composite_frame` finishes the frame:

1. `copy_texture_to_texture` from the cached static texture onto the
   backbuffer or swapchain target.
2. A single overlay pass (`LoadOp::Load`) dispatches the dynamic batch plus
   any motion-subtree blits on top.
3. Submit and present.

If any animation is still mid-flight
(`visible_anim_paint || visible_anim_stateful`), Phase 5 calls
`request_redraw()` for the next frame. Off-screen animations are gated out
by viewport-clipped `painted_node_ids` so we don't burn frames on elements
the user can't see.

## The cache

The render cache is two GPU-side `PrimitiveBatch`es on the render context:

- `cached_bg_batch`: static layer. Primitives whose pixels won't change
  without a structural, layout, or state event.
- `cached_dynamic_batch`: motion-bound primitives. Rewritten in place by
  `apply_binding_deltas`.

Classification happens at walker time via `AnimationStatus` and
`DynamicKind`. A node becomes dynamic if it has an active motion binding,
a live CSS keyframe, or a canvas draw closure.

`DynamicKind` variants:

| Variant                  | Role                                                                                                     |
|--------------------------|----------------------------------------------------------------------------------------------------------|
| `Canvas`                 | User draw closure re-invoked every frame                                                                 |
| `MotionSubtree`          | Subtree re-walked per frame with current binding values                                                  |
| `MotionSubtreeTexture`   | Subtree baked to an offscreen texture at motion-start, then transformed-blitted each frame               |
| `CssAnimated`            | CSS-animated subtree baked to a composite texture                                                        |

`has_render_cache()` is the predicate the fast-path gate reads.
`invalidate_render_cache_tagged()` is the canonical invalidation point. It
gets called by Phase 1 on structural change, by Phase 2 on layout-prop
animation start, and by Phase 3 on motion settle.

## Viewport culling

Scroll containers opted into `.viewport_cull(true)` register themselves in
`viewport_cull_scrolls`. During the paint walker, children of a culled
container are tested against the container's bounds (plus a 200 px
overscan). Children that don't intersect the cull rect are skipped: they
emit zero primitives, so they don't enter the static batch, the dynamic
batch, or any damage rect.

Fixed and sticky children opt out of culling.

For animation gating, the same viewport intersection clips
`painted_node_ids`. Off-screen animations still tick on the background
thread, but Phase 5 doesn't request the next paint if every animating
element is off-screen.

## Motion-subtree bake

A subtree with its own motion binding and no independently-animating
descendants is a bake candidate. When motion starts, the subtree is
rasterised once into a `LayerTexture`. Subsequent frames blit that texture
with the current motion transform applied. The subtree's internal
primitives never re-rasterise during motion.

The bake stores a stripped AABB (ancestor clips removed) as the subtree's
`ambient_clip`, which the overlay pass uses as the blit scissor so the
animation sweeps across the ancestor clip instead of dragging a baked-in
clipped slice.

## Drawing API surface

### `DrawContext` trait

Unified imperative drawing surface, used by user canvas closures and by
internal renderers alike:

```rust
pub trait DrawContext {
    // Transform stack
    fn push_transform(&mut self, transform: Transform);
    fn pop_transform(&mut self);

    // Opacity / clip / blend stacks
    fn push_opacity(&mut self, opacity: f32);
    fn push_blend_mode(&mut self, mode: BlendMode);
    fn push_clip(&mut self, shape: ClipShape);

    // 2D primitives
    fn fill_rect(&mut self, rect: Rect, corner_radius: CornerRadius, brush: Brush);
    fn stroke_rect(&mut self, rect: Rect, corner_radius: CornerRadius, stroke: &Stroke, brush: Brush);
    fn fill_circle(&mut self, center: Point, radius: f32, brush: Brush);
    fn draw_shadow(&mut self, rect: Rect, corner_radius: CornerRadius, shadow: Shadow);
    fn draw_text(&mut self, text: &str, origin: Point, style: &TextStyle);

    // SDF builder for optimised UI emission
    fn sdf_build(&mut self, f: &mut dyn FnMut(&mut dyn SdfBuilder));

    // 3D
    fn set_camera(&mut self, camera: &Camera);
    fn draw_mesh(&mut self, mesh: MeshId, material: MaterialId, transform: Mat4);

    // 2D ↔ 3D bridging
    fn billboard_draw(&mut self, size: Size, transform: Mat4, facing: BillboardFacing, f: ...);
    fn viewport_3d_draw(&mut self, rect: Rect, camera: &Camera, f: ...);

    // Layer management
    fn push_layer(&mut self, config: LayerConfig);
    fn pop_layer(&mut self);
}
```

> **Gotcha: adding methods to the trait.** When you add a new method with a
> default no-op impl, you MUST override it on
> `impl DrawContext for GpuPaintContext`. Inherent methods on
> `GpuPaintContext` are *not* trait overrides; calls via
> `&mut dyn DrawContext` dispatch to the no-op default.

### Implementations

| Type                | Role                                                            |
|---------------------|-----------------------------------------------------------------|
| `RecordingContext`  | Captures `DrawCommand`s for deferred replay (used by canvases)  |
| `GpuPaintContext`   | Emits `GpuPrimitive`s into a `PrimitiveBatch`                   |
| `PaintContext`      | Canvas-style convenience API wrapping `RecordingContext`        |

## GPU primitive

A single `GpuPrimitive` carries everything the SDF shaders need to render
one shape: bounds, corner radii, fill colours, border, shadow, clip, plus
the 3D, transform, and filter fields that grew in over time:

```rust
#[repr(C)]
pub struct GpuPrimitive {
    pub bounds: [f32; 4],
    pub corner_radius: [f32; 4],
    pub color: [f32; 4],
    pub color2: [f32; 4],        // gradient end
    pub border: [f32; 4],
    pub border_color: [f32; 4],
    pub shadow: [f32; 4],
    pub shadow_color: [f32; 4],
    pub rotation: [f32; 4],      // 2D rot (sin/cos × Z, Y)
    pub perspective: [f32; 4],   // 3D rot_x, persp_d, shape_type
    pub sdf_3d: [f32; 4],        // depth, ambient, specular, translate_z
    pub light: [f32; 4],         // 3D light dir + intensity
    pub filter_a: [f32; 4],      // grayscale / invert / sepia / hue
    pub filter_b: [f32; 4],      // brightness / contrast / saturate
    pub type_info: [u32; 4],     // primitive_type, fill_type, …
    // … plus clip + local_affine fields
}
```

Primitives are batched by pipeline and dispatched via GPU instancing: one
draw call per pipeline per layer.

## Shader pipelines

Each pipeline is driven by a WGSL program. They divide into:

- **SDF core.** The primary primitive renderer. Handles rounded rects,
  circles, ellipses, generic shapes via `sd_shaped_rect`.
- **3D SDF.** Per-primitive raymarched shapes (box, sphere, cylinder, torus,
  capsule, plus boolean groups) with Blinn-Phong lighting.
- **Specialised SDF.** Notched / pie-slice shapes, dedicated shadow pass.
- **Glass / vibrancy.** Backdrop blur + saturation + tint + noise.
- **Layer effects.** Blur, bloom, drop shadow, glow, colour matrix as
  post-process passes around stacked-layer textures.
- **Other primitive paths.** Text (glyph atlas sampling), image / SVG,
  vector paths, 3D meshes, particles.
- **3D scene support.** Skybox, tonemap, shadow.
- **Composite / utility.** Scissored REPLACE clear for the damage-rect path,
  static-cache blit, order-independent transparency composite.

## SDF essentials

The core 2D primitive renderer is still SDF-based. For a rounded rect:

```wgsl
fn sd_rounded_rect(p: vec2<f32>, b: vec2<f32>, r: vec4<f32>) -> f32 {
    let q = select(r.xy, r.zw, p.x > 0.0);
    let corner = select(q.x, q.y, p.y > 0.0);
    let d = abs(p) - b + corner;
    return min(max(d.x, d.y), 0.0) + length(max(d, vec2(0.0))) - corner;
}
```

Antialiasing uses fragment-derivative width:

```wgsl
let aa_width = fwidth(distance) * 0.5;
let alpha    = smoothstep(aa_width, -aa_width, distance);
```

Shadows are analytical. An `erf` approximation gives the Gaussian integral
without texture lookups. 3D shapes raymarch with up to 32 steps in the
fragment shader with Blinn-Phong lighting; UVs are mapped onto hit points
so gradients painted onto a 3D surface stay coherent (box = face
projection, sphere = spherical, cylinder/torus/capsule = cylindrical).

`corner-shape` lets corners morph between circular and squircle, scoop, or
bevel via a single exponent. `n=1` is round, `n=2` is squircle, higher
gets closer to a true square.

## Glass / vibrancy

Apple-style frosted glass: backdrop blur + saturation + tint + procedural
noise. Five presets:

| Type        | Blur | Saturation | Use case          |
|-------------|------|------------|-------------------|
| `UltraThin` | 10px | 1.8×       | Subtle overlays   |
| `Thin`      | 15px | 1.6×       | Light panels      |
| `Regular`   | 20px | 1.4×       | Default glass     |
| `Thick`     | 30px | 1.2×       | Strong blur       |
| `Chrome`    | 25px | 0.0×       | Metallic effect   |

The glass pass reads from a backbuffer (or per-layer texture for stacked
glass), so glass and damage-rect repainting coexist: the backbuffer is
re-blitted before the glass pass dispatches.

## Layer model

```rust
pub enum Layer {
    Ui { root: UiNode },
    Canvas2D { commands: Vec<DrawCommand> },
    Scene3D { camera: Camera, lights: Vec<Light>, … },

    // Composition
    Stack { layers: Vec<LayerId>, blend_mode: BlendMode },
    Transform2D { transform: Affine2D, child: LayerId },
    Transform3D { transform: Mat4, child: LayerId },
    Clip { shape: ClipShape, child: LayerId },
    Opacity { opacity: f32, child: LayerId },
    Offscreen { child: LayerId, effects: Vec<PostEffect> },

    // Dimension bridging
    Billboard { content: LayerId, transform: Mat4, facing: BillboardFacing },
    Viewport3D { scene: LayerId, camera: Camera },
    Portal { target: LayerId },
}
```

`ClipShape` covers the full CSS `clip-path` spec: circle, ellipse, inset,
rect, xywh, polygon, SVG path. Polygon clip vertices are packed into an
auxiliary storage buffer and resolved via winding number in the shader.

## Performance characteristics

| Resource         | Soft cap        |
|------------------|-----------------|
| SDF primitives   | ~10 000 / batch |
| Glass primitives | ~1 000 / batch  |
| Glyphs           | ~50 000 / batch |

**MSAA.** Configurable 1× / 2× / 4×, resolved during composite.

**Backbuffer.** Double- or triple-buffered for effects that need the prior
frame as input (glass), and required on platforms where the swapchain can't
be sampled directly (web, wasm).

## Where the model breaks

A few situations bypass the cache entirely and need a full repaint:

- Layout-affecting CSS changes (width, padding, gap, flex direction).
- Scroll physics that move the visible window.
- Bounds, clip, or size animations.
- Overlay open / close transitions.
- Canvas closures (they're called every frame by design; that's the contract).
- Structural rebuilds from stateful flips or signal-driven tree changes.

These cases re-walk the tree, repopulate the cache, and dispatch normally.
They show up in `frame_timing` traces as `did_rebuild=true` or
`needs_relayout=true`.

## See also

- [GPU Rendering chapter](book/src/architecture/gpu-rendering.md): the
  book-form version of this doc.
- [Architecture overview](book/src/architecture/overview.md): system-wide
  diagram.
- [Performance tips](book/src/advanced/performance.md): guidance for
  writing code that stays on the fast path.
