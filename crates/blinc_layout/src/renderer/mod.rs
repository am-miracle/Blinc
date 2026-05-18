//! RenderTree bridge connecting layout to rendering
//!
//! This module provides the bridge between Taffy layout computation
//! and the DrawContext rendering API.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};

use blinc_animation::AnimationScheduler;
use indexmap::IndexMap;

use blinc_core::{
    BlendMode, BlurQuality, Brush, ClipShape, Color, CornerRadius, DrawContext, GlassStyle,
    LayerConfig, LayerEffect, Point, Rect, Shadow, Stroke, Transform, Vec2,
};
use taffy::prelude::*;
use taffy::Overflow;

use crate::canvas::CanvasData;
use crate::css_parser::{
    Combinator, ComplexSelector, CompoundSelector, ElementState, SelectorPart, StructuralPseudo,
    Stylesheet,
};
use crate::diff::{render_props_eq, ChangeCategory, DivHash};
use crate::div::{ElementBuilder, ElementTypeId};
use crate::element::{ElementBounds, GlassMaterial, Material, RenderLayer, RenderProps};
use crate::layout_animation::{LayoutAnimationConfig, LayoutAnimationState};
use crate::selector::{ElementRegistry, ScrollRef};
use crate::tree::{LayoutNodeId, LayoutTree};
use crate::visual_animation::{AnimatedRenderBounds, VisualAnimation, VisualAnimationConfig};

// ─────────────────────────────────────────────────────────────────────────────
// Submodules
// ─────────────────────────────────────────────────────────────────────────────
//
// `RenderTree` is split across the modules below. Each submodule
// contributes one or more `impl RenderTree` blocks for the methods
// in its area; the struct definition itself, all fields, the
// `Default`/`new` constructors, and accessors stay here. The split is
// purely organisational — no public API changes — so external callers
// are unaffected.

mod animation;
mod build;
mod cursor;
mod events;
mod paint;
mod queries;
mod registries;
mod scroll;
mod stylesheet;
mod transfers;
mod types;

// Re-export the type surface so existing `crate::renderer::TextData`
// / `::ElementType` / `::RenderNode` paths keep resolving without
// any change to the rest of the crate or external callers.
#[allow(deprecated)]
pub use types::GlassPanel;
pub use types::{
    ElementType, ImageData, LayoutBoundsCallback, LayoutBoundsEntry, LayoutBoundsStorage,
    LayoutRenderer, NodeStateStorage, OnReadyCallback, OnReadyEntry, RenderNode,
    RenderTreeDebugStats, StyledTextData, StyledTextSpan, SvgData, TextData,
};

// Compositor-path metadata recorded for one motion-bound node during
// paint. Lets a follow-up "animation-only" frame patch the cached
// `GpuPrimitive` buffer in place — without re-walking the tree —
// by knowing which primitives the binding's subtree owns and what
// motion values were baked into them at last paint.
// =============================================================
// Compositor v2 — DynamicRegion / AnimationStatus
// =============================================================
//
// Per-node `AnimationStatus` partitions the render tree into a
// **static set** (cacheable in the static-layer texture) and a
// **dynamic set** (re-emitted every frame). The static-layer cache
// is the one source of truth for static pixels; transitions in the
// dynamic set invalidate ONLY their own screen-AABB region of the
// cache, not the whole texture.
//
// See `/Users/amaterasu/.claude/plans/purring-juggling-tulip.md`
// for the full architecture brief. These types are added in Phase 1
// alongside the legacy `CanvasPaintRecord` / `CompositeBindingMeta`
// / `MotionSubtreeRecord` structures; Phase 3 replaces them.

/// Per-node animation classification, recomputed at the start of
/// every compositor frame from the union of motion bindings,
/// canvas presence, and the CSS-animation store.
///
/// Hysteresis: once a node is `Animating`, it stays animating until
/// the `settled_streak` counter on `RenderTree` reaches
/// `SETTLED_STREAK_THRESHOLD` frames of stability. This avoids
/// thrashing on sub-pixel spring oscillation around a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationStatus {
    /// Node's pixels are stable; safe to bake into the static-layer
    /// cache. Walker emits primitives normally.
    Static,
    /// Node has at least one mid-flight motion binding,
    /// always-playing rotation timeline, canvas closure, or active
    /// CSS keyframe / transition. Walker skips primitive emission
    /// and the compositor re-walks the subtree (or invokes the
    /// canvas closure) into the per-frame overlay batch.
    Animating(AnimatedKind),
}

/// What's driving a node's animation. Precedence when multiple
/// sources apply: `Canvas` > `Motion` > `Css` — the deepest
/// per-frame work wins. Stored as the `kind` field of the resulting
/// `DynamicRegion` for dispatch routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimatedKind {
    /// `ElementType::Canvas` with a `render_fn` — closure is
    /// invoked into a scratch context each frame.
    Canvas,
    /// `MotionBindings` whose `is_any_animating` returns true (a
    /// mid-flight spring, opacity tween, or always-playing
    /// `rotation_timeline`) — subtree is re-walked each frame with
    /// the current binding values pushed onto the paint context.
    Motion,
    /// Node has a CSS keyframe or transition currently advancing.
    /// Same dispatch path as `Motion` but driven by the CSS-anim
    /// store rather than `MotionBindings`.
    Css,
}

/// The number of consecutive frames a node must be classified as
/// `Static` (no binding mid-flight, no CSS anim active, etc.)
/// before it can leave the dynamic set. Prevents flapping on
/// under-damped springs that oscillate at sub-pixel amplitude near
/// their target for many seconds before formally settling.
pub const SETTLED_STREAK_THRESHOLD: u32 = 30;

/// Ambient paint-context state captured at the moment the walker
/// reached a dynamic region's root, for the overlay pass to replay
/// when re-emitting the subtree's primitives next frame.
#[derive(Clone, Copy, Debug)]
pub struct AmbientPaintState {
    /// Composed affine `[a, b, c, d, tx, ty]` on the paint stack.
    pub affine: [f32; 6],
    /// Combined opacity multiplier from ancestor opacity stack.
    pub opacity: f32,
    /// Intersected screen-coord AABB of the ancestor clip stack.
    /// Used as the scissor for the overlay dispatch so a canvas
    /// scrolled out of its parent scroll container stays hidden.
    /// `None` when no ancestor clip is active.
    pub clip_aabb: Option<[f32; 4]>,
    /// Z-layer in effect at paint time.
    pub z_layer: u32,
}

/// Routing payload identifying what dynamic-region kind a record
/// represents and how the compositor should re-emit it each frame.
pub enum DynamicKind {
    /// Canvas region — clone the `render_fn` to re-invoke per frame.
    Canvas {
        render_fn: crate::canvas::CanvasRenderFn,
        /// Whether the canvas's own bounds should be pushed as a
        /// clip before invoking `render_fn` (mirrors the walker's
        /// `clips_content` handling).
        clips_content: bool,
        /// Local-coord bounds passed to `render_fn`.
        bounds_wh: (f32, f32),
    },
    /// Motion-bound subtree — re-walk the subtree with the current
    /// binding values pushed by the walker's normal binding-
    /// transform logic.
    MotionSubtree,
    /// CSS-animated node — re-walk like `MotionSubtree`, but
    /// distinguished so future damage-rect logic can treat CSS
    /// timelines separately if needed.
    CssAnimated,
}

impl Clone for DynamicKind {
    fn clone(&self) -> Self {
        match self {
            DynamicKind::Canvas {
                render_fn,
                clips_content,
                bounds_wh,
            } => DynamicKind::Canvas {
                render_fn: render_fn.clone(),
                clips_content: *clips_content,
                bounds_wh: *bounds_wh,
            },
            DynamicKind::MotionSubtree => DynamicKind::MotionSubtree,
            DynamicKind::CssAnimated => DynamicKind::CssAnimated,
        }
    }
}

impl std::fmt::Debug for DynamicKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DynamicKind::Canvas {
                clips_content,
                bounds_wh,
                ..
            } => f
                .debug_struct("Canvas")
                .field("clips_content", clips_content)
                .field("bounds_wh", bounds_wh)
                .finish(),
            DynamicKind::MotionSubtree => write!(f, "MotionSubtree"),
            DynamicKind::CssAnimated => write!(f, "CssAnimated"),
        }
    }
}

/// One animated region in the tree. Replaces the separate
/// `CanvasPaintRecord` and `MotionSubtreeRecord` records under the
/// Compositor v2 model: every node whose `AnimationStatus` is
/// `Animating` gets exactly one of these on each full paint.
///
/// The compositor reads the dynamic-region set every frame and:
/// 1. Blits the static-layer texture onto the surface.
/// 2. For each region: re-walks (or re-invokes the canvas closure)
///    with the saved ambient state, dispatches the resulting
///    primitives with `scissor = region.screen_aabb` and
///    `LoadOp::Load`.
/// 3. Presents.
///
/// The `screen_aabb` is the pixel bounds the region's primitives
/// occupy *after* applying the saved affine. Used both for the
/// dispatch scissor and (when the region's animation status flips)
/// for the damage-rect static-cache rebuild.
#[derive(Clone, Debug)]
pub struct DynamicRegion {
    /// Layout node id of the subtree's root.
    pub root: LayoutNodeId,
    /// Screen-coord AABB of the region's primitives (or canvas
    /// bounds), measured in physical pixels. Used as the dispatch
    /// scissor every frame and as the damage rect on transitions.
    pub screen_aabb: [f32; 4],
    /// Paint-context state at the moment the walker reached the
    /// region's root.
    pub ambient: AmbientPaintState,
    /// What kind of dynamic content this region holds + the data
    /// needed to re-emit it.
    pub kind: DynamicKind,
}

///
/// Mirrors the browser compositor's per-layer cached transform: the
/// Per-canvas state captured by the paint walker so the compositor
/// fast path can re-invoke the `render_fn` next frame without
/// re-walking the tree. The walker records this on every
/// `ElementType::Canvas` node that intersects the viewport; the
/// fast path reads it back to splice fresh canvas primitives into
/// the cached batch in place.
///
/// The `render_fn` is reference-counted so cloning the record is
/// cheap (just bumps an Rc). `affine` is the composed transform
/// stack at paint time — pushing it onto a scratch
/// `GpuPaintContext` reproduces the same coordinate frame the
/// walker had when it last invoked the closure.
#[derive(Clone)]
pub struct CanvasPaintRecord {
    /// Inclusive-exclusive range into the cached primitive batch
    /// covering every primitive the `render_fn` emitted on the last
    /// full paint. The fast path splices new primitives into this
    /// range; if the new count differs from `len(range)`, it must
    /// either rebuild adjacent ranges or fall back to a full paint.
    pub primitive_range: std::ops::Range<usize>,
    /// Composed affine `[a, b, c, d, tx, ty]` on the paint stack
    /// when the walker reached the canvas. Pre-multiplies the
    /// transform-rect math inside `render_fn`'s `DrawContext::fill_*`
    /// calls.
    pub affine: [f32; 6],
    /// Local-coord bounds (origin always `(0, 0)` — the affine
    /// carries the absolute position) passed to the `render_fn`.
    pub bounds_wh: (f32, f32),
    /// Closure the canvas wants invoked. Cloned `Rc` — fast.
    pub render_fn: crate::canvas::CanvasRenderFn,
    /// Whether the walker pushed an own-bounds clip before invoking
    /// `render_fn`. The replay must mirror this — otherwise spillover
    /// from a draw callback that intentionally over-draws would
    /// leak past the canvas's box.
    pub clips_content: bool,
    /// Intersected AABB of all ancestor clips that were active when
    /// the walker reached this canvas, in screen coordinates. Used
    /// by the compositor overlay pass as a scissor rect so canvas
    /// content scrolled out of its parent viewport stays hidden —
    /// without this, the cached static texture has the right
    /// (empty) region but the per-frame overlay draws on top
    /// unconditionally, producing "spinner floats above the scroll
    /// region" artifacts. `None` means no ancestor clip was active
    /// (root-level canvas).
    pub ancestor_clip_aabb: Option<[f32; 4]>,
    /// Z-layer the walker assigned when emitting the canvas. The
    /// scratch context replays at the same layer so the splice
    /// preserves the cached batch's draw order.
    pub z_layer: u32,
    /// Combined opacity multiplier (from ancestor opacity stack)
    /// at paint time. Replayed onto the scratch context so colours
    /// inside the canvas closure pick up the same opacity.
    pub opacity: f32,
}

impl std::fmt::Debug for CanvasPaintRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanvasPaintRecord")
            .field("primitive_range", &self.primitive_range)
            .field("affine", &self.affine)
            .field("bounds_wh", &self.bounds_wh)
            .field("clips_content", &self.clips_content)
            .field("ancestor_clip_aabb", &self.ancestor_clip_aabb)
            .field("z_layer", &self.z_layer)
            .field("opacity", &self.opacity)
            .finish()
    }
}

/// expensive paint pass runs once, and per-vsync we just delta the
/// transform/opacity on the affected primitives.
#[derive(Clone, Debug)]
pub struct CompositeBindingMeta {
    /// Inclusive-exclusive range into the cached primitive buffer
    /// covering every primitive emitted while this binding's
    /// transform stack was active (the binding's owning node plus
    /// every descendant). Empty range means the subtree emitted no
    /// SDF primitives (text-only subtrees, glass overlay, etc.) and
    /// the fast path can skip the entry.
    pub primitive_range: std::ops::Range<usize>,
    /// Motion translate baked into the primitive bounds at last paint
    /// (logical pixels, pre-DPI). The fast path computes
    /// `new_translate - last_translate` and shifts each primitive's
    /// `bounds.xy` by that delta.
    pub last_translate: (f32, f32),
    /// Motion uniform scale baked at last paint. Identity = `(1.0, 1.0)`.
    /// Scale changes require both bounds and `local_affine` updates,
    /// computed around the binding's centre point — captured below.
    pub last_scale: (f32, f32),
    /// Motion rotation in radians baked at last paint.
    pub last_rotation_rad: f32,
    /// Motion opacity baked at last paint. Each primitive's
    /// `color.a` / `border_color.a` / `shadow_color.a` was multiplied
    /// by this; the fast path scales by `new_opacity / last_opacity`.
    pub last_opacity: f32,
    /// Centre point (logical pixels, absolute) used for scale and
    /// rotation. Same coordinate frame as `last_translate`.
    pub centre: (f32, f32),
    /// Union AABB of every primitive in `primitive_range` at last
    /// paint, in screen pixels (post-DPI). `None` if the range was
    /// empty or the context didn't track primitive bounds.
    ///
    /// Used by the compositor v2 damage-rect path: the fast path
    /// reads this on each motion-binding tick, computes the new AABB
    /// (translate / scale / rotation deltas applied), and re-renders
    /// `union(last_screen_aabb, new_aabb)` of the static cache instead
    /// of invalidating the whole layer. Lets motion-bound elements
    /// move without forcing a full slow-path re-paint every frame.
    pub last_screen_aabb: Option<[f32; 4]>,
}

/// RenderTree - bridges layout computation and rendering
pub struct RenderTree {
    /// The underlying layout tree
    pub layout_tree: LayoutTree,
    /// Render data for each node (ordered by insertion/tree order)
    render_nodes: IndexMap<LayoutNodeId, RenderNode>,
    /// Root node ID
    root: Option<LayoutNodeId>,
    /// Event handlers registry for dispatching events
    handler_registry: crate::event_handler::HandlerRegistry,
    /// Dirty tracker for incremental rebuilds
    dirty_tracker: crate::interactive::DirtyTracker,
    /// Per-node state storage (survives across rebuilds if tree is reused)
    node_states: HashMap<LayoutNodeId, NodeStateStorage>,
    /// Scroll offsets for scroll containers (node_id -> (offset_x, offset_y))
    scroll_offsets: HashMap<LayoutNodeId, (f32, f32)>,
    /// Scroll physics for scroll containers (keyed by node_id)
    scroll_physics: HashMap<LayoutNodeId, crate::scroll::SharedScrollPhysics>,
    /// Scroll containers that opted in to viewport culling. The paint
    /// walker skips any descendant whose post-scroll bounds don't
    /// intersect the viewport (plus a small overscan buffer). Layout
    /// is still computed for every child — only paint is culled.
    viewport_cull_scrolls: std::collections::HashSet<LayoutNodeId>,
    /// Active cull rect (in tree-local coords) for the current paint
    /// walk. Set when a viewport-cull scroll is entered, restored on
    /// exit. Read by the child-recursion sites in `render_layer_with_motion`
    /// / `render_node` to skip subtrees whose bounds (offset by the
    /// cumulative scroll the child will inherit) don't intersect.
    /// `Cell` not `RefCell` because the value is `Copy` — read/write
    /// is one atomic load/store, no borrow tracking needed.
    cull_viewport: Cell<Option<(f32, f32, f32, f32)>>,
    /// Whether the current paint pass touched any node that drives a
    /// per-frame redraw — a `Canvas` element, a node with motion
    /// bindings, or a node with an active motion state. Reset to
    /// `false` at the start of `render_with_motion`, set to `true`
    /// from inside `render_layer_with_motion` whenever such a node
    /// is actually painted (i.e. not skipped by viewport culling).
    /// Read at the end of the frame to decide whether the redraw
    /// chain should fire: if the only active animations are tied to
    /// off-screen nodes, the chain stops until something brings them
    /// back into view.
    visible_anim_active: Cell<bool>,
    /// True if the paint walker painted at least one in-viewport
    /// `ElementType::Canvas` node this frame. Canvases re-run their
    /// draw callback every frame off the scheduler's timelines /
    /// continuous values, so the cached primitive batch goes stale
    /// instantly. The compositor fast path checks this flag and
    /// bails to a full paint whenever it's set — without that bail
    /// the walker never runs, the canvas draw callback never fires,
    /// and the animation freezes until the user moves the mouse.
    /// Reset to `false` at the top of `render_with_motion`; set to
    /// `true` from `render_layer_with_motion` when a Canvas node
    /// intersects the viewport.
    had_canvas_painted: Cell<bool>,
    /// When `true`, the paint walker skips invoking each
    /// `Canvas` node's `render_fn` while still recording the
    /// `CanvasPaintRecord` for later replay. Used by the layer
    /// compositor: the static-layer cache pass paints everything
    /// EXCEPT canvas content (so the cache has transparent regions
    /// where the canvases live), and each frame the renderer
    /// overlays fresh canvas content on top. Reset to `false` at
    /// the start of every full paint; set via
    /// [`Self::set_skip_canvas_drawing`].
    skip_canvas_drawing: Cell<bool>,
    /// Set of node ids that the paint walker actually rendered in the
    /// current frame (after viewport culling, motion-skip, and
    /// occlusion gates). Read by the windowed app at the end of the
    /// frame to decide which animating Statefuls are visible — an
    /// off-screen spinner whose node didn't make it into this set
    /// stops keeping the redraw chain alive.
    ///
    /// `RefCell` rather than `Cell` because `HashSet` isn't `Copy`;
    /// the paint walker is single-threaded so the borrow contract is
    /// trivially upheld. Cleared at the top of each `render_with_motion`
    /// pass and grown back during the recursive walk.
    painted_node_ids: RefCell<HashSet<LayoutNodeId>>,
    /// Motion bindings for continuous animations (keyed by node_id)
    motion_bindings: HashMap<LayoutNodeId, crate::motion::MotionBindings>,
    /// Compositor-path metadata captured during paint for nodes whose
    /// transform / opacity is driven by motion bindings. Records the
    /// range of primitives the paint walker emitted for the binding's
    /// subtree plus the motion values that were baked into them, so a
    /// follow-up "animation-only" frame can patch the cached
    /// `GpuPrimitive` buffer in place — delta-applying the change to
    /// just those primitives — instead of re-running the walker and
    /// re-uploading the whole batch. Mirrors the browser compositor
    /// path: paint runs once, composition just shuffles cached layers
    /// per vsync.
    ///
    /// Cleared at the top of every full paint and grown back during
    /// the recursive walk. The downstream consumer (Phase-4 fast
    /// path) reads from this AND from the primitive cache the
    /// renderer keeps after upload. When the cache is invalidated
    /// (rebuild, layout change, structural state change) this map
    /// stays alive but its entries are ignored — the next full paint
    /// clears and repopulates.
    composite_bindings: RefCell<HashMap<LayoutNodeId, CompositeBindingMeta>>,
    /// Per-canvas paint state captured during full paint so the
    /// fast path can re-invoke each canvas's `render_fn` next frame
    /// without re-walking the rest of the tree. Same lifecycle as
    /// `composite_bindings` — cleared at the top of every full
    /// paint, populated by `render_layer_with_motion`, drained by
    /// the fast path's canvas-splice step. Stale entries are
    /// ignored after a cache invalidation; the next full paint
    /// repopulates from scratch.
    canvas_paint_records: RefCell<HashMap<LayoutNodeId, CanvasPaintRecord>>,
    // --- Compositor v2 storage (Phase 1; populated by Phase 2) ---
    /// Unified per-region map: every node classified as
    /// `AnimationStatus::Animating` produces one entry here. The
    /// compositor's per-frame dispatch iterates this map, re-emits
    /// each region's primitives, and uses `screen_aabb` as the
    /// scissor rect. Cleared at the top of every full paint;
    /// repopulated by `render_layer_with_motion`.
    dynamic_regions: RefCell<HashMap<LayoutNodeId, DynamicRegion>>,
    /// Previous frame's animation classification per node. Used to
    /// detect transitions (Static ↔ Animating) for the
    /// damage-rect cache-rebuild path. Persists across full paints
    /// — Phase 1 only writes this from
    /// `compute_animation_status`; Phase 4 consumes it.
    previous_animation_status: RefCell<HashMap<LayoutNodeId, AnimationStatus>>,
    /// Current frame's animation classification per node — the live
    /// map the walker reads from while emitting primitives. Written
    /// once at the top of every full paint by
    /// `compute_animation_status` (which also returns a `Vec` for
    /// the compositor's transition-detection path). Cleared and
    /// repopulated each frame, never persists.
    ///
    /// Stored as a `HashMap` rather than the `Vec` produced by
    /// `compute_animation_status` because the walker does point
    /// lookups: `current_animation_status.get(&node)` once per
    /// painted node, ~thousands of times per frame.
    current_animation_status: RefCell<HashMap<LayoutNodeId, AnimationStatus>>,
    /// Hysteresis counter — frames spent classified as `Static`
    /// since the node last appeared `Animating`. Once the count
    /// reaches `SETTLED_STREAK_THRESHOLD`, the node is allowed to
    /// move back to the static set; below the threshold the node
    /// stays in the dynamic set even though no current
    /// animation source is mid-flight. Avoids flapping on
    /// under-damped spring oscillation around a target.
    settled_streak: RefCell<HashMap<LayoutNodeId, u32>>,
    /// Last tick time for scroll physics (in milliseconds)
    last_scroll_tick_ms: Option<u64>,
    /// DPI scale factor (physical / logical pixels)
    ///
    /// When set, all layout positions and sizes are multiplied by this factor
    /// before rendering. This allows users to specify sizes in logical pixels
    /// while rendering happens at physical pixel resolution.
    scale_factor: f32,
    /// Animation scheduler for scroll bounce springs
    animations: Weak<Mutex<AnimationScheduler>>,
    /// Hash of the element tree used to build this RenderTree
    /// Used for quick equality checks to skip unnecessary rebuilds
    tree_hash: Option<DivHash>,
    /// Per-node hashes for incremental change detection
    /// Maps node_id to (own_hash, tree_hash) - own excludes children, tree includes children
    node_hashes: HashMap<LayoutNodeId, (DivHash, DivHash)>,
    /// Layout bounds storages to update after layout computation
    /// Maps node_id to entry with shared storage and optional change callback
    layout_bounds_storages: HashMap<LayoutNodeId, LayoutBoundsEntry>,
    /// Element registry for O(1) lookups by string ID
    element_registry: Arc<ElementRegistry>,
    /// Bound ScrollRefs for programmatic scroll control
    /// Note: NOT cleared on rebuild - ScrollRef inner state persists and node_id is updated
    scroll_refs: HashMap<LayoutNodeId, ScrollRef>,
    /// Active scroll refs (persists across rebuilds, keyed by inner pointer address)
    /// Maps inner pointer -> ScrollRef for persistence across rebuilds
    active_scroll_refs: Vec<ScrollRef>,
    /// Node most recently targeted by a scroll event, plus the wall-clock
    /// millis at which it received that event. When the next scroll arrives
    /// within a short window, we keep routing to this node even if its
    /// physics report it "can't consume" — this is the desktop-browser
    /// behaviour that prevents inner-scrolls from suddenly handing off to
    /// the parent mid-gesture as soon as the inner reaches an edge, which
    /// looks like the scroll "jumps" across the container boundary.
    last_scroll_target: Option<(LayoutNodeId, f64)>,
    /// On-ready callbacks for elements (fires once after first layout)
    /// Maps string_id to callback entry for stable tracking across rebuilds.
    on_ready_callbacks: HashMap<String, OnReadyEntry>,
    /// Optional stylesheet for automatic state modifier application
    /// When set, elements with IDs will automatically get :hover, :active, :focus, :disabled styles
    stylesheet: Option<Arc<Stylesheet>>,
    /// Base styles for elements (before state modifiers)
    /// Used to restore original styles when state changes
    base_styles: HashMap<LayoutNodeId, RenderProps>,
    /// Base taffy layout styles for elements (before state modifiers)
    /// Used to restore original layout when state changes affect layout properties
    base_taffy_styles: HashMap<LayoutNodeId, taffy::Style>,
    /// Layout animation configs for nodes (from element builders)
    /// Maps node_id to the LayoutAnimationConfig specifying which properties to animate
    layout_animation_configs: HashMap<LayoutNodeId, LayoutAnimationConfig>,
    /// Active layout animations (running or recently completed)
    /// Maps node_id to the active animation state with spring-driven values
    layout_animations: HashMap<LayoutNodeId, LayoutAnimationState>,
    /// Previous bounds for layout animation comparison
    /// Stores the last known layout bounds to detect changes
    previous_bounds: HashMap<LayoutNodeId, ElementBounds>,
    /// Stable key to node ID mapping for layout animations
    /// Used to transfer animation state when nodes are rebuilt with same stable key
    layout_animation_key_to_node: HashMap<String, LayoutNodeId>,
    /// Stable key based animations - state tracked by key not node ID
    /// These animations persist across Stateful rebuilds
    layout_animations_by_key: HashMap<String, LayoutAnimationState>,
    /// Previous bounds tracked by stable key
    previous_bounds_by_key: HashMap<String, ElementBounds>,

    // ========================================================================
    // Visual Animation System (FLIP-style, read-only layout)
    // ========================================================================
    /// Visual animation configs for nodes (from element builders)
    /// Maps stable_key to config specifying which properties to animate
    visual_animation_configs: HashMap<String, VisualAnimationConfig>,
    /// Stable key to node ID mapping for visual animations
    /// Updated each frame when nodes register; key→node ensures we always have current node
    visual_animation_key_to_node: HashMap<String, LayoutNodeId>,
    /// Active visual animations (by stable key)
    /// These track visual offsets from layout, never modify taffy
    visual_animations: HashMap<String, VisualAnimation>,
    /// Previous visual bounds by stable key (what was rendered last frame)
    /// Used to detect bounds changes and initiate FLIP animations
    previous_visual_bounds: HashMap<String, ElementBounds>,
    /// Pre-computed animated render bounds for this frame
    /// Calculated after layout, used during rendering
    animated_render_bounds: HashMap<LayoutNodeId, AnimatedRenderBounds>,

    // ========================================================================
    // CSS Animation/Transition System (shared with AnimationScheduler thread)
    // ========================================================================
    /// Shared CSS animation/transition store
    ///
    /// Wrapped in `Arc<Mutex<>>` so the AnimationScheduler's background thread
    /// can tick animations at 120fps while the main thread reads/writes.
    css_anim_store: Arc<Mutex<crate::render_state::CssAnimationStore>>,
    /// Nodes that currently have hover-triggered CSS animations
    hover_css_animations: HashSet<LayoutNodeId>,

    /// Nodes that were affected by complex selector state rules (e.g. .class:hover)
    /// Used to reset render props when the state rule no longer matches
    complex_state_affected: HashSet<LayoutNodeId>,

    // ========================================================================
    // FLIP Animation Support (CSS transitions on layout position changes)
    // ========================================================================
    /// Persistent element bounds by string ID, updated after every compute_layout().
    /// Used by apply_flip_transitions() to detect position changes on subtree rebuild.
    flip_previous_bounds: HashMap<String, ElementBounds>,
    /// Active FLIP animations keyed by element string ID (stable across subtree rebuilds).
    /// Unlike css_anim_store.transitions (keyed by LayoutNodeId), these survive node recreation
    /// because they resolve string IDs → LayoutNodeIds at apply time via element_registry.
    flip_animations: HashMap<String, crate::render_state::ActiveCssAnimation>,

    /// Cached "does the pointer pipeline need to run on a bare mouse-move?"
    /// predicate. Equivalent to:
    ///
    ///   handler_registry.has_any_pointer_handler()
    ///     || stylesheet.is_some_and(|s| s.has_pointer_state_rules())
    ///     || has_any_cursor_style()
    ///
    /// Encoded as `i8`: `0` = stale (recompute on next read), `1` = no
    /// pipeline needed, `2` = pipeline needed. The windowed app's
    /// pre-Event::Input prelude reads this once per mouse-move; on a
    /// static UI like `hello_blinc` with no handlers / no `:hover` /
    /// no `cursor:` styles, the read returns `1` and the entire
    /// `Event::Input` branch (including its `Box<dyn FnMut>` callback
    /// allocation) is skipped. Mouse drags over the window stay at
    /// near-zero CPU on Linux high-rate mice that fire 1 kHz cursor
    /// events.
    ///
    /// Invalidated by every tree mutation that could affect any of
    /// the three predicate inputs — see
    /// `invalidate_mouse_move_pipeline_cache`.
    mouse_move_pipeline_cache: std::sync::atomic::AtomicI8,

    // ========================================================================
    // Stable Node Identity (Phase 1 — foundation, no consumer migration yet)
    // ========================================================================
    /// `StableNodeId` → current frame's `LayoutNodeId`. Populated during
    /// build; lets subsystems that key on stable identity resolve to the
    /// live slotmap key for paint / layout queries.
    ///
    /// See `project_stable_node_id_design` for the migration plan.
    /// Today this is read-only plumbing: no internal map consumes it
    /// yet. Phase 2 (motion/animation/FLIP) is the first migration.
    stable_to_layout: HashMap<crate::tree::StableNodeId, LayoutNodeId>,
    /// Reverse: `LayoutNodeId` → `StableNodeId`. Used during the
    /// post-build sweep to evict stable entries whose backing layout
    /// node was removed.
    layout_to_stable: HashMap<LayoutNodeId, crate::tree::StableNodeId>,
    /// Monotonic build counter, bumped each time the tree is rebuilt
    /// from scratch (`from_element*`). Stable subsystems stamp the
    /// generation they last touched their entries with so a post-build
    /// sweep can evict anything that didn't get touched this pass —
    /// the replacement for today's blanket `remove_subtree_nodes`
    /// wipe. Saturates harmlessly after 2⁶⁴ builds.
    build_generation: u64,
}

/// Result of an incremental update attempt
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateResult {
    /// No changes detected, tree unchanged
    NoChanges,
    /// Only visual properties changed (no layout needed)
    VisualOnly,
    /// Layout properties changed (layout needs recomputation)
    LayoutChanged,
    /// Children changed - subtree rebuilds queued, needs layout recomputation
    ChildrenChanged,
}

impl Default for RenderTree {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderTree {
    /// Create a new empty render tree
    pub fn new() -> Self {
        Self {
            layout_tree: LayoutTree::new(),
            render_nodes: IndexMap::new(),
            root: None,
            handler_registry: crate::event_handler::HandlerRegistry::new(),
            dirty_tracker: crate::interactive::DirtyTracker::new(),
            node_states: HashMap::new(),
            scroll_offsets: HashMap::new(),
            scroll_physics: HashMap::new(),
            viewport_cull_scrolls: std::collections::HashSet::new(),
            cull_viewport: Cell::new(None),
            visible_anim_active: Cell::new(false),
            had_canvas_painted: Cell::new(false),
            skip_canvas_drawing: Cell::new(false),
            painted_node_ids: RefCell::new(HashSet::new()),
            motion_bindings: HashMap::new(),
            composite_bindings: RefCell::new(HashMap::new()),
            canvas_paint_records: RefCell::new(HashMap::new()),
            dynamic_regions: RefCell::new(HashMap::new()),
            previous_animation_status: RefCell::new(HashMap::new()),
            current_animation_status: RefCell::new(HashMap::new()),
            settled_streak: RefCell::new(HashMap::new()),
            last_scroll_tick_ms: None,
            scale_factor: 1.0,
            animations: Weak::new(),
            tree_hash: None,
            node_hashes: HashMap::new(),
            layout_bounds_storages: HashMap::new(),
            element_registry: Arc::new(ElementRegistry::new()),
            scroll_refs: HashMap::new(),
            active_scroll_refs: Vec::new(),
            last_scroll_target: None,
            on_ready_callbacks: HashMap::new(),
            stylesheet: None,
            base_styles: HashMap::new(),
            base_taffy_styles: HashMap::new(),
            layout_animation_configs: HashMap::new(),
            layout_animations: HashMap::new(),
            previous_bounds: HashMap::new(),
            layout_animation_key_to_node: HashMap::new(),
            layout_animations_by_key: HashMap::new(),
            previous_bounds_by_key: HashMap::new(),
            // Visual animation system (FLIP-style)
            visual_animation_configs: HashMap::new(),
            visual_animation_key_to_node: HashMap::new(),
            visual_animations: HashMap::new(),
            previous_visual_bounds: HashMap::new(),
            animated_render_bounds: HashMap::new(),
            // CSS animation/transition system (shared with scheduler thread)
            css_anim_store: Arc::new(Mutex::new(crate::render_state::CssAnimationStore::new())),
            hover_css_animations: HashSet::new(),
            complex_state_affected: HashSet::new(),
            flip_previous_bounds: HashMap::new(),
            flip_animations: HashMap::new(),
            mouse_move_pipeline_cache: std::sync::atomic::AtomicI8::new(0),
            // Phase 1 stable-id foundation — see `project_stable_node_id_design`.
            stable_to_layout: HashMap::new(),
            layout_to_stable: HashMap::new(),
            build_generation: 0,
        }
    }

    /// Invalidate the cached `mouse_move_pipeline_needed` predicate.
    /// Call from any mutation site that could change handler
    /// registration, stylesheet pointer-state rules, or per-node
    /// `cursor:` props. Cheap (one relaxed atomic store).
    pub fn invalidate_mouse_move_pipeline_cache(&self) {
        self.mouse_move_pipeline_cache
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// Returns `true` when a bare mouse-move event needs the full
    /// pointer pipeline (hit_test, hover diff, drag delta, cursor
    /// resolve). Returns `false` for static UIs with no handlers,
    /// no `:hover`/`:active`/`:focus` rules, and no `cursor:` styles
    /// — those can skip the entire `Event::Input` branch on every
    /// move.
    ///
    /// Lazily computed on first read after invalidation; subsequent
    /// reads are a single relaxed atomic load. The recompute walks
    /// the handler registry once and the render-node map once, so
    /// even invalidating per frame is cheap.
    pub fn mouse_move_pipeline_needed(&self) -> bool {
        use std::sync::atomic::Ordering;
        let cached = self.mouse_move_pipeline_cache.load(Ordering::Relaxed);
        if cached != 0 {
            return cached == 2;
        }
        let needed = self.handler_registry.has_any_pointer_handler()
            || self
                .stylesheet
                .as_ref()
                .is_some_and(|s| s.has_pointer_state_rules())
            || self.has_any_cursor_style();
        self.mouse_move_pipeline_cache
            .store(if needed { 2 } else { 1 }, Ordering::Relaxed);
        needed
    }

    /// Set the animation scheduler for scroll bounce animations
    pub fn set_animations(&mut self, scheduler: &Arc<Mutex<AnimationScheduler>>) {
        self.animations = Arc::downgrade(scheduler);
        // Update any existing scroll physics with the scheduler
        for physics in self.scroll_physics.values() {
            if let Some(scheduler_arc) = self.animations.upgrade() {
                physics.lock().unwrap().set_scheduler(&scheduler_arc);
            }
        }
    }

    /// Get the shared CSS animation store
    ///
    /// Used to register a tick callback on the AnimationScheduler so the
    /// background thread can tick CSS animations/transitions at 120fps.
    pub fn css_anim_store(&self) -> Arc<Mutex<crate::render_state::CssAnimationStore>> {
        Arc::clone(&self.css_anim_store)
    }

    /// Set the shared CSS animation store (to reuse across tree rebuilds)
    ///
    /// Call this after tree creation to share the store with the scheduler's
    /// tick callback. The store persists across tree rebuilds.
    pub fn set_css_anim_store(
        &mut self,
        store: Arc<Mutex<crate::render_state::CssAnimationStore>>,
    ) {
        self.css_anim_store = store;
    }

    /// Set a shared external element registry
    ///
    /// This allows the WindowedContext to share the same registry for query operations.
    /// The registry is automatically populated during tree building.
    pub fn set_element_registry(&mut self, registry: Arc<ElementRegistry>) {
        self.element_registry = registry;
    }

    /// Set the DPI scale factor for this render tree
    ///
    /// This scales all layout positions and sizes by the given factor
    /// before rendering. Use this for HiDPI/Retina display support.
    ///
    /// # Arguments
    /// * `scale_factor` - The scale factor (1.0 = no scaling, 2.0 = 2x DPI)
    pub fn set_scale_factor(&mut self, scale_factor: f32) {
        self.scale_factor = scale_factor;
    }

    /// Get the current scale factor
    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    // `debug_stats` moved to `renderer/queries.rs`.

    /// Get the root node ID
    pub fn root(&self) -> Option<LayoutNodeId> {
        self.root
    }

    // ========================================================================
    // Stable Node Identity (read API)
    // ========================================================================

    /// Resolve a `StableNodeId` to the current frame's `LayoutNodeId`.
    ///
    /// Returns `None` if the stable id was minted in a previous frame
    /// but its layout node no longer exists in the tree (its build
    /// generation didn't survive the most recent sweep).
    pub fn layout_id(&self, stable: crate::tree::StableNodeId) -> Option<LayoutNodeId> {
        self.stable_to_layout.get(&stable).copied()
    }

    /// Look up the `StableNodeId` minted for a given live `LayoutNodeId`.
    ///
    /// Returns `None` for layout nodes created outside the standard
    /// build walk (shouldn't happen — every node minted by
    /// `mint_stable_ids_walk` is registered here).
    pub fn stable_id(&self, layout: LayoutNodeId) -> Option<crate::tree::StableNodeId> {
        self.layout_to_stable.get(&layout).copied()
    }

    /// Resolve a `LayoutNodeId` to its `StableNodeId`, falling back
    /// to `StableNodeId::ROOT` with a `tracing::warn!` if no mapping
    /// is registered.
    ///
    /// Used by **registration** sites — collect_render_props inserting
    /// into handler_registry / css_anim_store / etc. — where every
    /// live node should have a minted stable id (mint runs before
    /// collect). A missing mapping there is a real bug worth
    /// surfacing.
    ///
    /// Event **dispatch** sites should NOT use this — they routinely
    /// see node ids that have just been removed by a rebuild
    /// (stale hit-test result, pending queued event). Those should
    /// use [`Self::stable_id`] (`Option`) and skip silently when
    /// the node is gone.
    pub(crate) fn stable_id_or_warn(&self, layout: LayoutNodeId) -> crate::tree::StableNodeId {
        match self.layout_to_stable.get(&layout).copied() {
            Some(stable) => stable,
            None => {
                tracing::warn!(
                    "stable_id_or_warn: layout node {:?} has no stable id mapping \
                     (mint_stable_ids_walk hasn't covered it) — falling back to ROOT",
                    layout,
                );
                crate::tree::StableNodeId::ROOT
            }
        }
    }

    /// Current build generation. Bumped each full rebuild. Subsystems
    /// that key on `StableNodeId` stamp this on their entries so a
    /// post-build sweep can evict anything that didn't survive the
    /// most recent build pass.
    pub fn build_generation(&self) -> u64 {
        self.build_generation
    }

    /// Walk the live layout tree and (re)mint a `StableNodeId` for
    /// every node, populating `stable_to_layout` and `layout_to_stable`.
    ///
    /// Called after every full rebuild from `from_element*`. Stable
    /// ids derive from `(parent_stable, sibling_index, element_id_if_set)`,
    /// so call order + DOM structure + explicit element ids together
    /// determine the id — deterministic per frame, stable across
    /// rebuilds for the same structure.
    ///
    /// Caller responsibility: bump `build_generation` before calling so
    /// the eventual sweep can compare against stamps.
    pub(crate) fn mint_stable_ids_walk(&mut self) {
        // Wipe the maps; they're entirely regenerated each rebuild
        // (consumers that need cross-frame stability key by
        // `StableNodeId` directly; the mapping cache is rebuild-local).
        self.stable_to_layout.clear();
        self.layout_to_stable.clear();

        let Some(root_id) = self.root else { return };
        let root_widget_key = self.element_registry.get_id(root_id);
        let root_stable =
            crate::tree::StableNodeId::ROOT.derive_child(0, root_widget_key.as_deref());
        self.register_stable(root_stable, root_id);

        // Pre-collect the work queue from the layout tree before
        // touching `self`'s maps mutably during the recursive descent.
        // Layout-tree reads only need `&self.layout_tree`, but the
        // recursive registration needs `&mut self`, so flatten first.
        let mut stack: Vec<(LayoutNodeId, crate::tree::StableNodeId)> =
            vec![(root_id, root_stable)];
        while let Some((node, stable)) = stack.pop() {
            let children = self.layout_tree.children(node);
            for (i, &child) in children.iter().enumerate() {
                let widget_key = self.element_registry.get_id(child);
                let child_stable = stable.derive_child(i, widget_key.as_deref());
                self.register_stable(child_stable, child);
                stack.push((child, child_stable));
            }
        }
    }

    /// Drop handler-registry entries whose stable id no longer
    /// maps to a live layout node. Called at the end of every
    /// build pass so subtrees that disappeared in this rebuild
    /// don't leak closures.
    ///
    /// Replaces today's destructive wipe-and-re-register pattern:
    /// handlers for nodes whose stable id survives the build pass
    /// stay resident in the registry, getting their closure entry
    /// overwritten in-place by the new build's
    /// `handler_registry.register(stable, fresh_handlers)`. Only
    /// genuinely-removed nodes lose their entry.
    pub(crate) fn sweep_stale_handlers(&mut self) {
        let valid: std::collections::HashSet<crate::tree::StableNodeId> =
            self.stable_to_layout.keys().copied().collect();
        self.handler_registry
            .retain(|stable| valid.contains(&stable));
    }

    /// Fill in `stable_key` on `LayoutAnimationConfig` entries that
    /// don't already have one, using the freshly-minted
    /// `StableNodeId` for each node. Run right after
    /// `mint_stable_ids_walk` so the existing keyed-survival path
    /// (`previous_bounds_by_key` + `layout_animations_by_key`)
    /// becomes the only path — animations whose user-supplied config
    /// didn't bother with `stable_key` survive rebuilds for free
    /// because the auto-key is deterministic across builds.
    ///
    /// Visual animations (`VisualAnimationConfig::key`) already
    /// require a non-empty key, so no companion pass is needed
    /// there.
    pub(crate) fn auto_fill_animation_stable_keys(&mut self) {
        // Clone the resolution map so we can mutate configs without a
        // double borrow.
        let layout_to_stable_snapshot = self.layout_to_stable.clone();
        for (node_id, config) in self.layout_animation_configs.iter_mut() {
            if config.stable_key.is_some() {
                continue;
            }
            if let Some(stable) = layout_to_stable_snapshot.get(node_id) {
                config.stable_key = Some(format!("auto:{}", stable.to_raw()));
            }
        }
    }

    /// Insert into both mapping directions. Pulled out so the walk
    /// reads as a flat recursion without map-bookkeeping noise.
    fn register_stable(&mut self, stable: crate::tree::StableNodeId, layout: LayoutNodeId) {
        // Collision check: two different layout nodes hashing to the
        // same stable id would corrupt the map (a `child_a` lookup
        // would return `child_b`'s layout id). Log and skip the second
        // insert — caller's tree shape is degenerate.
        if let Some(prev) = self.stable_to_layout.get(&stable) {
            if *prev != layout {
                tracing::warn!(
                    "StableNodeId collision: {:?} maps to {:?} and {:?}; \
                     second registration dropped (rebuild stability for the \
                     second node may suffer)",
                    stable,
                    prev,
                    layout,
                );
                return;
            }
        }
        self.stable_to_layout.insert(stable, layout);
        self.layout_to_stable.insert(layout, stable);
    }

    /// Whether the most recently completed paint pass touched any
    /// node that drives a per-frame redraw — Canvas elements,
    /// motion-bound elements, or elements with an active motion
    /// state. Reset to `false` at the start of `render_with_motion`
    /// and set during the paint walk for any non-culled node that
    /// matches the criteria above. Callers (typically the event
    /// loop's end-of-frame redraw decision) use this to gate the
    /// animation-redraw signal: if all active animations are tied
    /// to off-screen (viewport-culled) subtrees, the chain stops
    /// until input or scroll brings them back into view.
    pub fn visible_anim_active(&self) -> bool {
        self.visible_anim_active.get()
    }

    /// Manually mark the visible-animation flag. Called by the
    /// compositor fast path when `apply_binding_deltas` detects a
    /// motion binding that actually moved this frame — without it,
    /// Phase 5's redraw chain dies because the walker (which is the
    /// other writer of this flag) didn't run.
    ///
    /// The flag is reset to `false` at the start of every full paint
    /// (`render_with_motion`), so a stale `true` from a previous
    /// fast-path frame can't keep the chain alive forever — the
    /// next full paint will clear it and the walker / fast path
    /// will set it again only if there's still active work.
    pub fn set_visible_anim_active(&self, value: bool) {
        self.visible_anim_active.set(value);
    }

    /// Whether the paint walker painted at least one in-viewport
    /// canvas in the most recent full paint. Read by the windowed
    /// app's fast-path gate to bail to a full paint when any canvas
    /// is on screen — canvases re-run their draw callback every
    /// frame and the cached primitives go stale instantly. See
    /// `had_canvas_painted` field doc for more.
    pub fn had_canvas_painted(&self) -> bool {
        self.had_canvas_painted.get()
    }

    /// Setter for `had_canvas_painted`. Called by the paint walker
    /// (reset to `false` at the top of `render_with_motion`, set to
    /// `true` whenever a Canvas node intersects the viewport).
    pub fn set_had_canvas_painted(&self, value: bool) {
        self.had_canvas_painted.set(value);
    }

    /// Whether the walker should skip invoking canvas `render_fn`s
    /// on the next full paint while still recording each canvas's
    /// paint state. The layer compositor sets this to `true` before
    /// the static-cache paint so canvas regions in the cached
    /// texture stay transparent — fresh canvas content is then
    /// overlaid on top each frame.
    pub fn skip_canvas_drawing(&self) -> bool {
        self.skip_canvas_drawing.get()
    }

    /// Setter for [`Self::skip_canvas_drawing`].
    pub fn set_skip_canvas_drawing(&self, value: bool) {
        self.skip_canvas_drawing.set(value);
    }

    /// Borrow the set of node ids that the paint walker rendered in
    /// the most recent frame.
    ///
    /// Use the returned guard to filter `STATEFUL_ANIMATIONS` registry
    /// entries down to those whose node is on screen — see
    /// `stateful::has_visible_animating_statefuls`. The set is rebuilt
    /// fresh by every `render_with_motion` call, so callers should
    /// read it after the paint pass and before the next frame begins.
    pub fn painted_node_ids(&self) -> std::cell::Ref<'_, HashSet<LayoutNodeId>> {
        self.painted_node_ids.borrow()
    }

    /// Borrow the map of motion-bound nodes → composite-path metadata
    /// captured during the most recent paint. The Phase-4 fast path
    /// reads from this to delta-apply translate / scale / rotate /
    /// opacity to the cached `GpuPrimitive` buffer without re-walking
    /// the tree. Populated by `render_layer_with_motion` whenever a
    /// node with `motion_bindings.is_any_animating()` is painted;
    /// stale entries (for nodes that didn't paint this frame) are
    /// cleared at the top of every full paint.
    pub fn composite_bindings(
        &self,
    ) -> std::cell::Ref<'_, HashMap<LayoutNodeId, CompositeBindingMeta>> {
        self.composite_bindings.borrow()
    }

    /// Mutable accessor for the composite-bindings map. Used by the
    /// paint walker to insert/update entries during the walk, and by
    /// the Phase-4 fast path to update `last_*` values after a
    /// delta-apply succeeds (so the next frame's delta is computed
    /// against the value just written to the GPU, not the original
    /// paint-time value — otherwise the second fast-path frame would
    /// double-apply).
    pub fn composite_bindings_mut(
        &self,
    ) -> std::cell::RefMut<'_, HashMap<LayoutNodeId, CompositeBindingMeta>> {
        self.composite_bindings.borrow_mut()
    }

    /// Borrow the map of canvases the walker captured on the most
    /// recent full paint. The compositor fast path iterates this to
    /// re-invoke each canvas's `render_fn` and splice the resulting
    /// primitives back into the cached batch in place.
    pub fn canvas_paint_records(
        &self,
    ) -> std::cell::Ref<'_, HashMap<LayoutNodeId, CanvasPaintRecord>> {
        self.canvas_paint_records.borrow()
    }

    /// Mutable variant of [`Self::canvas_paint_records`] for the
    /// fast path to update `primitive_range` if a re-paint emits
    /// a different number of primitives than the cached entry
    /// recorded.
    pub fn canvas_paint_records_mut(
        &self,
    ) -> std::cell::RefMut<'_, HashMap<LayoutNodeId, CanvasPaintRecord>> {
        self.canvas_paint_records.borrow_mut()
    }

    /// Borrow the Compositor v2 dynamic-region map. Populated by the
    /// walker for every node classified as
    /// [`AnimationStatus::Animating`]; consumed each frame by the
    /// compositor's per-region dispatch path.
    pub fn dynamic_regions(&self) -> std::cell::Ref<'_, HashMap<LayoutNodeId, DynamicRegion>> {
        self.dynamic_regions.borrow()
    }

    /// Mutable variant of [`Self::dynamic_regions`].
    pub fn dynamic_regions_mut(
        &self,
    ) -> std::cell::RefMut<'_, HashMap<LayoutNodeId, DynamicRegion>> {
        self.dynamic_regions.borrow_mut()
    }

    /// Compute the animation status for every node that has any
    /// known animation source.
    ///
    /// Inputs consulted:
    /// - `motion_bindings` (`is_any_animating` on each)
    /// - `render_nodes` for `ElementType::Canvas` presence
    /// - [`Self::css_anim_store`] for active CSS keyframes /
    ///   transitions, looked up by `StableNodeId`
    ///
    /// Applies hysteresis via the internal `settled_streak` map:
    /// once a node is `Animating`, it must spend
    /// [`SETTLED_STREAK_THRESHOLD`] consecutive frames classified
    /// as `Static` before it's allowed to leave the dynamic set.
    /// This is what prevents under-damped springs from flapping a
    /// node in and out of the dynamic set as the value oscillates
    /// sub-pixel around the target.
    ///
    /// Returned vector is keyed by `LayoutNodeId`; only nodes with
    /// at least one possible animation source appear (nodes with
    /// none are implicitly `Static`).
    ///
    /// Cost: linear in `motion_bindings.len()` + canvas-node count
    /// + css animation/transition count. No tree traversal.
    pub fn compute_animation_status(&self) -> Vec<(LayoutNodeId, AnimationStatus)> {
        use std::collections::HashSet;

        // Step 1: collect candidate nodes from each source. A node
        // can appear in more than one; precedence resolves below.
        let mut candidates: HashSet<LayoutNodeId> = HashSet::new();
        let mut canvas_nodes: HashSet<LayoutNodeId> = HashSet::new();
        let mut motion_animating: HashSet<LayoutNodeId> = HashSet::new();
        let mut css_animating: HashSet<LayoutNodeId> = HashSet::new();

        // Motion bindings: every node with a `MotionBindings` entry
        // is a candidate; the entry's `is_any_animating` decides
        // whether it's currently Animating-ish.
        for (node, bindings) in self.motion_bindings.iter() {
            candidates.insert(*node);
            if bindings.is_any_animating() {
                motion_animating.insert(*node);
            }
        }

        // Canvases: every `ElementType::Canvas` is always animating
        // (the closure can produce different output every frame).
        for (node, render_node) in self.render_nodes.iter() {
            if matches!(
                render_node.element_type,
                crate::renderer::ElementType::Canvas(_)
            ) {
                candidates.insert(*node);
                canvas_nodes.insert(*node);
            }
        }

        // CSS animations / transitions: keyed by StableNodeId. Map
        // back to LayoutNodeId via `stable_to_layout`. Filter on
        // `is_playing` — `CssAnimationStore` deliberately keeps
        // settled / completed transitions in the map so the
        // same-target guard in `detect_and_start_transitions` can
        // match against them. Counting those settled entries as
        // `Animating(Css)` would mark every cn_demo button that
        // EVER hovered as perpetually dynamic, forcing the walker
        // to push them through `dynamic_batch` on every slow paint
        // forever and bloating the per-region re-walk set on the
        // CSS-only fast path. The active-animation check (`is_playing`)
        // matches `has_active_animations` / `has_active_transitions`
        // for the same reason.
        if let Ok(store) = self.css_anim_store.lock() {
            for (stable, anim) in store.animations.iter() {
                if !anim.is_playing {
                    continue;
                }
                if let Some(layout) = self.stable_to_layout.get(stable).copied() {
                    candidates.insert(layout);
                    css_animating.insert(layout);
                }
            }
            for (stable, trans) in store.transitions.iter() {
                if !trans.is_playing {
                    continue;
                }
                if let Some(layout) = self.stable_to_layout.get(stable).copied() {
                    candidates.insert(layout);
                    css_animating.insert(layout);
                }
            }
        }

        // Step 2: classify each candidate. Precedence:
        // Canvas > Motion > Css. Hysteresis pushes a node toward
        // `Animating` if it was animating recently.
        let mut result: Vec<(LayoutNodeId, AnimationStatus)> = Vec::with_capacity(candidates.len());
        let mut streak_writer = self.settled_streak.borrow_mut();
        for node in candidates {
            let raw_status = if canvas_nodes.contains(&node) {
                // Canvases are unconditionally animating — no
                // hysteresis concept (no target to settle against).
                AnimationStatus::Animating(AnimatedKind::Canvas)
            } else if motion_animating.contains(&node) {
                AnimationStatus::Animating(AnimatedKind::Motion)
            } else if css_animating.contains(&node) {
                AnimationStatus::Animating(AnimatedKind::Css)
            } else {
                AnimationStatus::Static
            };

            let final_status = match raw_status {
                AnimationStatus::Animating(_) => {
                    // Reset the settled-streak counter; we're moving
                    // again.
                    streak_writer.insert(node, 0);
                    raw_status
                }
                AnimationStatus::Static => {
                    // Possibly inside the hysteresis window. Bump
                    // the counter and decide whether we've waited
                    // long enough to move back to the static set.
                    let entry = streak_writer.entry(node).or_insert(0);
                    *entry = entry.saturating_add(1);
                    if *entry >= SETTLED_STREAK_THRESHOLD {
                        streak_writer.remove(&node);
                        AnimationStatus::Static
                    } else {
                        // Still in the cooldown window — keep the
                        // node in the dynamic set under whatever
                        // kind it was animating last. Defaults to
                        // `Motion` if we never saw a prior
                        // classification (e.g. first frame after a
                        // structural rebuild).
                        let prev = self
                            .previous_animation_status
                            .borrow()
                            .get(&node)
                            .copied()
                            .unwrap_or(AnimationStatus::Animating(AnimatedKind::Motion));
                        match prev {
                            AnimationStatus::Animating(_) => prev,
                            AnimationStatus::Static => {
                                AnimationStatus::Animating(AnimatedKind::Motion)
                            }
                        }
                    }
                }
            };

            result.push((node, final_status));
        }
        drop(streak_writer);

        // Mirror the result into the live map the walker reads from
        // each frame. Keeping the side-effect inside the compute
        // function means there's no way to forget to repopulate it
        // before the walker runs; callers stay pure consumers of the
        // returned `Vec` (used for transition detection, tracing,
        // etc.).
        {
            let mut current = self.current_animation_status.borrow_mut();
            current.clear();
            current.reserve(result.len());
            for (node, status) in &result {
                current.insert(*node, *status);
            }
        }

        result
    }

    /// Borrow the previous frame's animation status map. Used by
    /// the compositor to detect transitions (Static ↔ Animating)
    /// for damage-rect cache invalidation.
    pub fn previous_animation_status(
        &self,
    ) -> std::cell::Ref<'_, HashMap<LayoutNodeId, AnimationStatus>> {
        self.previous_animation_status.borrow()
    }

    /// Borrow the current frame's animation-status map. Populated as
    /// a side-effect of [`Self::compute_animation_status`] and
    /// consumed by the walker on the same frame to decide which
    /// nodes are dynamic.
    pub fn current_animation_status(
        &self,
    ) -> std::cell::Ref<'_, HashMap<LayoutNodeId, AnimationStatus>> {
        self.current_animation_status.borrow()
    }

    /// Replace the stored previous-status map with the supplied
    /// current-frame statuses. Call after a frame's compositor
    /// pass finishes, before the next frame's
    /// `compute_animation_status`.
    pub fn commit_animation_status(&self, statuses: &[(LayoutNodeId, AnimationStatus)]) {
        let mut prev = self.previous_animation_status.borrow_mut();
        prev.clear();
        prev.reserve(statuses.len());
        for (node, status) in statuses {
            prev.insert(*node, *status);
        }
    }

    /// `painted_node_ids()` projected through `layout_to_stable`.
    ///
    /// Allocates a fresh `HashSet<StableNodeId>` each call so the
    /// caller can hold it across mutating renderer calls (looking
    /// up by `StableNodeId` against the CSS animation store, etc.).
    /// Cost is proportional to the painted-set size, dominated by
    /// the same paint walk that produced it — the once-per-frame
    /// allocation is unmeasurable in practice.
    ///
    /// Migration helper for Phase 5: subsystems keyed by
    /// `StableNodeId` (`CssAnimationStore`, soon others) need the
    /// painted set in stable terms; this is the conversion point.
    pub fn painted_stable_ids(&self) -> HashSet<crate::tree::StableNodeId> {
        let painted = self.painted_node_ids.borrow();
        painted
            .iter()
            .filter_map(|n| self.layout_to_stable.get(n).copied())
            .collect()
    }

    /// Update root node dimensions for window resize.
    ///
    /// Fast path that avoids full tree rebuild — only updates the root layout
    /// node's width/height so taffy recomputes flex/block metrics with new dims.
    pub fn resize_root(&mut self, width: f32, height: f32) {
        if let Some(root_id) = self.root {
            if let Some(mut style) = self.layout_tree.get_style(root_id) {
                style.size.width = Dimension::Length(width);
                style.size.height = Dimension::Length(height);
                self.layout_tree.set_style(root_id, style);
            }
        }
    }

    /// Compute layout for the given viewport size
    pub fn compute_layout(&mut self, width: f32, height: f32) {
        if let Some(root) = self.root {
            // Step 1: Check for existing collapsing animations and apply their constraints
            // This ensures children are laid out at the larger (animated) size during collapse
            let style_overrides = self.apply_collapsing_animation_constraints();
            let had_collapsing = !style_overrides.is_empty();

            // Step 2: Run taffy layout with potentially overridden styles
            self.layout_tree.compute_layout(
                root,
                Size {
                    width: AvailableSpace::Definite(width),
                    height: AvailableSpace::Definite(height),
                },
            );

            // Step 3: Restore original styles (cleanup for next frame)
            self.restore_style_overrides(style_overrides);

            // Update scroll physics with computed content dimensions
            self.update_scroll_content_dimensions();

            // Update registered layout bounds storages
            self.update_layout_bounds_storages();

            // Trigger layout animations for elements with changed bounds
            self.update_layout_animations();

            // Step 4: If new collapsing animations were created, re-layout with constraints
            // This handles the first frame of a collapse animation where children need
            // to be laid out at the larger (start) size, not the smaller (target) size.
            if !had_collapsing {
                let new_overrides = self.apply_collapsing_animation_constraints();
                if !new_overrides.is_empty() {
                    tracing::debug!(
                        "Re-running layout for {} new collapsing animations",
                        new_overrides.len()
                    );
                    self.layout_tree.compute_layout(
                        root,
                        Size {
                            width: AvailableSpace::Definite(width),
                            height: AvailableSpace::Definite(height),
                        },
                    );
                    self.restore_style_overrides(new_overrides);

                    // Re-update bounds storages after second layout pass
                    self.update_layout_bounds_storages();
                }
            }

            // Cache element bounds for ElementHandle.bounds() queries
            self.cache_element_bounds();

            // Process on_ready callbacks for newly laid out elements
            self.process_on_ready_callbacks();

            // =========================================================
            // Visual Animation System (FLIP-style, read-only layout)
            // =========================================================
            // This runs AFTER layout is complete and does NOT modify taffy.
            // It detects bounds changes and creates FLIP-style animations.
            self.update_visual_animations();

            // Pre-compute animated render bounds for all nodes
            // This propagates parent animation offsets to children.
            self.compute_animated_render_bounds();
        }
    }

    /// Apply style overrides for nodes with active collapsing animations
    ///
    /// During collapse, we want children to be laid out at the larger (animated) size
    /// so there's content to clip as the animation progresses.
    ///
    /// Returns a vec of (node_id, original_style) pairs for restoration.
    fn apply_collapsing_animation_constraints(&mut self) -> Vec<(LayoutNodeId, Style)> {
        let mut overrides = Vec::new();

        tracing::trace!(
            "apply_collapsing_animation_constraints: checking {} stable-key animations",
            self.layout_animations_by_key.len()
        );

        // Check stable-key based animations
        for (stable_key, anim_state) in &self.layout_animations_by_key {
            let is_collapsing = anim_state.is_collapsing();
            tracing::trace!(
                "  key='{}': is_collapsing={}, current_height={:.1}, target_height={:.1}",
                stable_key,
                is_collapsing,
                anim_state.current_height(),
                anim_state.end_bounds.height
            );
            if !is_collapsing {
                continue;
            }

            // Find the node ID for this stable key
            let Some(&node_id) = self.layout_animation_key_to_node.get(stable_key) else {
                continue;
            };

            // Get current style
            let Some(mut style) = self.layout_tree.get_style(node_id) else {
                continue;
            };

            // Save original style for restoration
            overrides.push((node_id, style.clone()));

            // Get the constraint bounds (larger of animated or target)
            let constraint_bounds = anim_state.layout_constraint_bounds();

            // Override size to animated bounds (the larger size during collapse)
            if anim_state.is_width_collapsing() {
                style.size.width = Dimension::Length(constraint_bounds.width);
            }
            if anim_state.is_height_collapsing() {
                style.size.height = Dimension::Length(constraint_bounds.height);
            }

            // Apply overridden style
            self.layout_tree.set_style(node_id, style);

            tracing::trace!(
                "Applied collapsing constraint for key='{}': width={}, height={}",
                stable_key,
                constraint_bounds.width,
                constraint_bounds.height
            );
        }

        // Also check node-ID based animations
        for (&node_id, anim_state) in &self.layout_animations {
            if !anim_state.is_collapsing() {
                continue;
            }

            let Some(mut style) = self.layout_tree.get_style(node_id) else {
                continue;
            };

            overrides.push((node_id, style.clone()));

            let constraint_bounds = anim_state.layout_constraint_bounds();

            if anim_state.is_width_collapsing() {
                style.size.width = Dimension::Length(constraint_bounds.width);
            }
            if anim_state.is_height_collapsing() {
                style.size.height = Dimension::Length(constraint_bounds.height);
            }

            self.layout_tree.set_style(node_id, style);
        }

        overrides
    }

    /// Restore original styles after layout computation
    fn restore_style_overrides(&mut self, overrides: Vec<(LayoutNodeId, Style)>) {
        for (node_id, original_style) in overrides {
            self.layout_tree.set_style(node_id, original_style);
        }
    }

    /// Cache element bounds for all elements with string IDs
    ///
    /// This populates the ElementRegistry's bounds cache so that
    /// `ElementHandle.bounds()` can return computed bounds.
    fn cache_element_bounds(&self) {
        // Clear the previous cache
        self.element_registry.clear_bounds();

        // Iterate through all render nodes and cache bounds for those with string IDs
        for (node_id, _render_node) in &self.render_nodes {
            if let Some(string_id) = self.element_registry.get_id(*node_id) {
                if let Some(bounds) = self.get_bounds(*node_id) {
                    self.element_registry.update_bounds(
                        &string_id,
                        blinc_core::Bounds::new(bounds.x, bounds.y, bounds.width, bounds.height),
                    );
                }
            }
        }
    }

    // Layout-bounds-storage methods (`register_layout_bounds_storage*`,
    // `unregister_*`, `register_element_bounds_storage`,
    // `update_layout_bounds_storages`) moved to `renderer/registries.rs`.

    /// Update layout animations for nodes with changed bounds
    ///
    /// This compares the new layout bounds with the previous bounds and triggers
    /// spring animations for any changes. Called after layout computation.
    ///
    /// Supports two tracking modes:
    /// 1. **Node ID tracking** (default): Animation tracked by LayoutNodeId
    /// 2. **Stable key tracking**: Animation tracked by stable key string
    ///
    /// Stable key tracking is essential for Stateful components where nodes
    /// are rebuilt on state change. The stable key allows recognizing that
    /// a new node represents the same logical element.
    fn update_layout_animations(&mut self) {
        // Early exit if no layout animation configs are registered
        if self.layout_animation_configs.is_empty() {
            return;
        }

        tracing::debug!(
            "update_layout_animations: {} configs registered, {} animations active",
            self.layout_animation_configs.len(),
            self.layout_animations_by_key.len()
        );

        // Get animation scheduler handle
        let scheduler_handle = if let Some(arc) = self.animations.upgrade() {
            arc.lock().unwrap().handle()
        } else if let Some(handle) = crate::render_state::get_global_scheduler() {
            handle
        } else {
            tracing::trace!("update_layout_animations: no scheduler available");
            return;
        };

        // Collect updates to avoid borrowing issues
        // Tuple: (node_id, new_bounds, config, stable_key_option)
        let mut updates: Vec<(LayoutNodeId, ElementBounds, LayoutAnimationConfig)> = Vec::new();

        // Track which stable keys are still in use this frame
        let mut active_stable_keys: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for (&node_id, config) in &self.layout_animation_configs {
            let Some(new_bounds) = self.layout_tree.get_bounds(node_id, (0.0, 0.0)) else {
                continue;
            };

            // Track stable key if present
            if let Some(ref key) = config.stable_key {
                active_stable_keys.insert(key.clone());
                // Update key->node mapping
                self.layout_animation_key_to_node
                    .insert(key.clone(), node_id);
            }

            updates.push((node_id, new_bounds, config.clone()));
        }

        // Process updates
        for (node_id, new_bounds, config) in updates {
            if let Some(ref stable_key) = config.stable_key {
                // ===== STABLE KEY TRACKING =====
                // Use key-based storage instead of node ID
                let old_bounds = self.previous_bounds_by_key.get(stable_key).cloned();
                let is_first_layout = old_bounds.is_none();

                // Store new bounds by key
                self.previous_bounds_by_key
                    .insert(stable_key.clone(), new_bounds);

                if is_first_layout {
                    tracing::debug!(
                        "Layout animation (keyed): first layout for key='{}', bounds={:?}",
                        stable_key,
                        new_bounds
                    );
                    continue;
                }

                let old = old_bounds.unwrap();

                // Check if there's an existing animation for this key
                if let Some(existing_anim) = self.layout_animations_by_key.get_mut(stable_key) {
                    // Update existing animation's target
                    let old_target = existing_anim.end_bounds.height;
                    existing_anim.update_target(new_bounds, &config);
                    tracing::info!(
                        "Layout animation (keyed): updating target for key='{}': old_target={:.1} -> new_target={:.1}, is_collapsing={}",
                        stable_key,
                        old_target,
                        new_bounds.height,
                        existing_anim.is_collapsing()
                    );
                } else {
                    // Try to create new animation
                    if let Some(anim_state) = LayoutAnimationState::from_bounds_change(
                        old,
                        new_bounds,
                        &config,
                        scheduler_handle.clone(),
                    ) {
                        tracing::info!(
                            "Layout animation (keyed): triggered for key='{}': {:?} -> {:?}",
                            stable_key,
                            old,
                            new_bounds
                        );
                        self.layout_animations_by_key
                            .insert(stable_key.clone(), anim_state);
                    } else {
                        tracing::debug!(
                            "Layout animation (keyed): no change for key='{}': old={:?}, new={:?}",
                            stable_key,
                            old,
                            new_bounds
                        );
                    }
                }
            } else {
                // ===== NODE ID TRACKING (original behavior) =====
                let old_bounds = self.previous_bounds.get(&node_id).cloned();
                let is_first_layout = old_bounds.is_none();

                self.previous_bounds.insert(node_id, new_bounds);

                if is_first_layout {
                    self.layout_animations.remove(&node_id);
                    continue;
                }

                let old = old_bounds.unwrap();

                if let Some(anim_state) = LayoutAnimationState::from_bounds_change(
                    old,
                    new_bounds,
                    &config,
                    scheduler_handle.clone(),
                ) {
                    tracing::trace!(
                        "Layout animation triggered for {:?}: {:?} -> {:?}",
                        node_id,
                        old,
                        new_bounds
                    );
                    self.layout_animations.insert(node_id, anim_state);
                } else if let Some(existing) = self.layout_animations.get(&node_id) {
                    if !existing.is_animating() {
                        self.layout_animations.remove(&node_id);
                    }
                }
            }
        }

        // Clean up completed animations (node ID based)
        // Clean up completed animations (node ID based)
        self.layout_animations
            .retain(|_, state| state.is_animating());

        // Clean up completed animations (stable key based)
        self.layout_animations_by_key.retain(|key, state| {
            let is_anim = state.is_animating();
            if !is_anim {
                tracing::debug!(
                    "Layout animation (keyed): cleaning up settled animation for key='{}'",
                    key
                );
            }
            is_anim
        });
    }

    // =========================================================================
    // On-Ready Callbacks
    // =========================================================================

    /// Process all pending on_ready callbacks
    ///
    /// This is called after layout computation. Each callback is invoked with
    /// the element's computed bounds, then marked as triggered so it won't
    /// fire again on subsequent layouts.
    ///
    /// Callbacks registered via the query API (ElementHandle.on_ready()) are
    /// tracked by string ID for stability across tree rebuilds.
    ///
    /// Callbacks are invoked after a short delay (200ms) to allow the window
    /// to finish resizing/animating on platforms like macOS where fullscreen
    /// transitions cause rapid resize events.
    fn process_on_ready_callbacks(&mut self) {
        // Pick up any pending callbacks from the registry (via query API)
        // These are already keyed by string ID for stable tracking
        let pending_from_registry = self.element_registry.take_pending_on_ready();
        for (string_id, callback) in pending_from_registry {
            // Only add if not already registered (avoid duplicates)
            self.on_ready_callbacks
                .entry(string_id)
                .or_insert(OnReadyEntry {
                    callback,
                    triggered: false,
                });
        }

        // Collect callbacks that need invocation
        // Look up node_id from string_id via registry for bounds lookup
        let registry = self.element_registry.clone();
        let to_trigger: Vec<(String, OnReadyCallback, ElementBounds)> = self
            .on_ready_callbacks
            .iter()
            .filter(|(_, entry)| !entry.triggered)
            .filter_map(|(string_id, entry)| {
                // Look up node_id from string_id
                let node_id = registry.get(string_id)?;

                self.layout_tree
                    .get_bounds(node_id, (0.0, 0.0))
                    .map(|bounds| (string_id.clone(), entry.callback.clone(), bounds))
            })
            .collect();

        // Mark as triggered before invoking (in case callback triggers rebuild)
        // Also mark in the registry for cross-rebuild deduplication
        for (string_id, _, _) in &to_trigger {
            if let Some(entry) = self.on_ready_callbacks.get_mut(string_id) {
                entry.triggered = true;
            }
            self.element_registry.mark_on_ready_triggered(string_id);
        }

        // Invoke callbacks with bounds after a short delay so any
        // window-resize / fullscreen animation has settled and the
        // bounds are stable when the user's animation kicks off.
        //
        // wasm32 has no `std::thread::spawn` (the stdlib path
        // panics with `operation not supported on this platform`),
        // so on the web target we just fire the callbacks
        // synchronously inline. The 200ms delay was a stability
        // workaround for desktop window-manager redraw races that
        // doesn't apply in the browser — there's no separate
        // window-resize animation; the rAF tick that mutated the
        // tree IS the resize completion.
        if !to_trigger.is_empty() {
            #[cfg(not(target_arch = "wasm32"))]
            std::thread::spawn(move || {
                // Magic delay to let the window settle
                std::thread::sleep(std::time::Duration::from_millis(200));

                for (string_id, callback, bounds) in to_trigger {
                    tracing::trace!("on_ready callback invoked for '{}'", string_id);
                    callback(bounds);
                }
            });

            #[cfg(target_arch = "wasm32")]
            for (string_id, callback, bounds) in to_trigger {
                tracing::trace!("on_ready callback invoked for '{}'", string_id);
                callback(bounds);
            }
        }
    }

    /// Get the layout tree for inspection
    pub fn layout(&self) -> &LayoutTree {
        &self.layout_tree
    }

    /// Get the event handler registry
    pub fn handler_registry(&self) -> &crate::event_handler::HandlerRegistry {
        &self.handler_registry
    }

    /// Get the event handler registry mutably
    pub fn handler_registry_mut(&mut self) -> &mut crate::event_handler::HandlerRegistry {
        &mut self.handler_registry
    }

    /// Get the element registry for ID-based lookups
    pub fn element_registry(&self) -> &Arc<ElementRegistry> {
        &self.element_registry
    }

    // `query_by_id` moved to `renderer/queries.rs`.
    //
    // Event-dispatch surface (`dispatch_event*`,
    // `dispatch_text_input_event*`, `dispatch_key_event*`,
    // `broadcast_text_input_event`, `broadcast_key_event`,
    // `dispatch_scroll_event`) moved to `renderer/events.rs`.
    //
    // Scroll machinery (offset management, chain dispatch,
    // pinch chain, physics ticking, scrollbar overlay,
    // `ScrollRef` plumbing) moved to `renderer/scroll.rs`.

    // =========================================================================
    // Motion Animation Initialization
    // =========================================================================

    /// Initialize motion animations for nodes with motion config
    ///
    /// Call this after building/rebuilding the tree to start enter animations
    /// for any nodes wrapped in motion() containers.
    ///
    /// For nodes with a `motion_stable_id`, the animation state is tracked by
    /// stable key instead of node_id. This allows animations to persist across
    /// tree rebuilds (essential for overlays which are rebuilt every frame).
    pub fn initialize_motion_animations(
        &self,
        render_state: &mut crate::render_state::RenderState,
    ) {
        for (&node_id, render_node) in &self.render_nodes {
            if let Some(ref motion_config) = render_node.props.motion {
                // Use stable key if available (for overlays), otherwise use node_id
                if let Some(ref stable_key) = render_node.props.motion_stable_id {
                    // Check if this motion should start suspended
                    if render_node.props.motion_is_suspended {
                        // Start in suspended state - waits for explicit start()
                        // Returns true if the motion was newly created or reset from Visible
                        let needs_on_ready = render_state
                            .start_stable_motion_suspended(stable_key, motion_config.clone());

                        // Register on_ready callback if provided and motion was created/reset
                        // This will fire once the element is laid out, allowing
                        // the callback to trigger the suspended animation start
                        if needs_on_ready {
                            if let Some(ref callback) = render_node.props.motion_on_ready_callback {
                                // Clear any previous triggered state so the callback can fire again
                                self.element_registry.clear_on_ready_triggered(stable_key);
                                // Register the stable_key with the node_id so that
                                // process_on_ready_callbacks can look up bounds
                                self.element_registry.register(stable_key, node_id);
                                self.element_registry
                                    .register_on_ready_for_id(stable_key, callback.clone());
                            }
                        }
                    } else {
                        // Start or replay stable motion based on replay flag
                        // Motion exit is now triggered explicitly via MotionHandle.exit()
                        render_state.start_stable_motion(
                            stable_key,
                            motion_config.clone(),
                            render_node.props.motion_should_replay,
                        );
                    }
                } else {
                    render_state.start_enter_motion(node_id, motion_config.clone());
                }
            }
        }
    }

    /// Get nodes with motion config (for external initialization)
    pub fn nodes_with_motion(&self) -> Vec<(LayoutNodeId, crate::element::MotionAnimation)> {
        self.render_nodes
            .iter()
            .filter_map(|(&node_id, render_node)| {
                render_node.props.motion.clone().map(|m| (node_id, m))
            })
            .collect()
    }

    /// Get the motion translation for a node (if it has motion bindings)
    ///
    /// Returns the current translation transform from any bound AnimatedValue(s).
    /// This is sampled every frame, enabling continuous smooth animations.
    pub fn get_motion_transform(&self, node_id: LayoutNodeId) -> Option<Transform> {
        self.motion_bindings
            .get(&node_id)
            .and_then(|b| b.get_transform())
    }

    /// Get the motion scale for a node (if it has motion bindings)
    ///
    /// Returns (scale_x, scale_y) if scale bindings are present.
    pub fn get_motion_scale(&self, node_id: LayoutNodeId) -> Option<(f32, f32)> {
        self.motion_bindings
            .get(&node_id)
            .and_then(|b| b.get_scale())
    }

    /// Get the motion rotation for a node (if it has motion bindings)
    ///
    /// Returns rotation in degrees if rotation binding is present.
    pub fn get_motion_rotation(&self, node_id: LayoutNodeId) -> Option<f32> {
        self.motion_bindings
            .get(&node_id)
            .and_then(|b| b.get_rotation())
    }

    /// Get the motion opacity for a node (if it has motion bindings)
    pub fn get_motion_opacity(&self, node_id: LayoutNodeId) -> Option<f32> {
        self.motion_bindings
            .get(&node_id)
            .and_then(|b| b.get_opacity())
    }

    /// Check if a node has motion bindings
    pub fn has_motion_bindings(&self, node_id: LayoutNodeId) -> bool {
        self.motion_bindings.contains_key(&node_id)
    }

    /// Borrow the full motion-bindings map. Used by the
    /// compositor-path fast-paint helper to sample the current
    /// spring values for every bound node without going through
    /// the per-node lookup helpers (which fetch one property at a
    /// time and would lock each `SharedAnimatedValue` 4 times per
    /// node).
    pub fn motion_bindings_map(&self) -> &HashMap<LayoutNodeId, crate::motion::MotionBindings> {
        &self.motion_bindings
    }
    /// Check if the tree has any dirty nodes (needs rebuild)
    pub fn needs_rebuild(&self) -> bool {
        self.dirty_tracker.has_dirty()
    }

    /// Clear dirty tracking state
    ///
    /// Call this after rebuilding the UI.
    pub fn clear_dirty(&mut self) {
        self.dirty_tracker.clear_all();
    }

    /// Get the dirty tracker for more granular control
    pub fn dirty_tracker(&self) -> &crate::interactive::DirtyTracker {
        &self.dirty_tracker
    }

    /// Get the dirty tracker mutably
    pub fn dirty_tracker_mut(&mut self) -> &mut crate::interactive::DirtyTracker {
        &mut self.dirty_tracker
    }

    // =========================================================================
    // Node State Storage (for Stateful elements)
    // =========================================================================

    /// Get or create state for a node
    ///
    /// If state doesn't exist for this node, creates it with the provided initial value.
    /// Returns a clone of the Arc handle to the state.
    pub fn get_or_create_state<S: Send + 'static>(
        &mut self,
        node_id: LayoutNodeId,
        initial: S,
    ) -> Arc<Mutex<S>> {
        // Check if state already exists
        if let Some(existing) = self.node_states.get(&node_id) {
            // Try to downcast to the expected type
            let guard = existing.lock().unwrap();
            if guard.downcast_ref::<S>().is_some() {
                drop(guard);
                // Clone and downcast the Arc
                let cloned = Arc::clone(existing);
                // SAFETY: We just verified the type matches
                return unsafe { Arc::from_raw(Arc::into_raw(cloned) as *const Mutex<S>) };
            }
        }

        // Create new state
        let state: Arc<Mutex<S>> = Arc::new(Mutex::new(initial));
        let erased: NodeStateStorage = state.clone();
        self.node_states.insert(node_id, erased);
        state
    }

    /// Get existing state for a node (if any)
    pub fn get_state<S: Send + 'static>(&self, node_id: LayoutNodeId) -> Option<Arc<Mutex<S>>> {
        self.node_states.get(&node_id).and_then(|existing| {
            let guard = existing.lock().unwrap();
            if guard.downcast_ref::<S>().is_some() {
                drop(guard);
                let cloned = Arc::clone(existing);
                // SAFETY: We just verified the type matches
                Some(unsafe { Arc::from_raw(Arc::into_raw(cloned) as *const Mutex<S>) })
            } else {
                None
            }
        })
    }

    /// Update render props for a node
    ///
    /// This allows event handlers to modify visual properties without
    /// triggering a full tree rebuild.
    pub fn update_render_props<F>(&mut self, node_id: LayoutNodeId, f: F)
    where
        F: FnOnce(&mut RenderProps),
    {
        if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
            f(&mut render_node.props);
        }
    }

    // =========================================================================
    // Stylesheet Integration
    // =========================================================================

    /// Set the stylesheet for automatic state modifier application
    ///
    /// When a stylesheet is set, elements with IDs will automatically get
    /// `:hover`, `:active`, `:focus`, `:disabled` styles applied based on
    /// their current interaction state.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let css = r#"
    ///     #button { background: blue; }
    ///     #button:hover { opacity: 0.9; }
    ///     #button:active { transform: scale(0.98); }
    /// "#;
    /// let stylesheet = Stylesheet::parse_with_errors(css).stylesheet;
    /// tree.set_stylesheet(stylesheet);
    /// ```
    pub fn set_stylesheet(&mut self, stylesheet: Stylesheet) {
        self.stylesheet = Some(Arc::new(stylesheet));
        self.invalidate_mouse_move_pipeline_cache();
    }

    /// Set a shared stylesheet reference
    pub fn set_stylesheet_arc(&mut self, stylesheet: Arc<Stylesheet>) {
        // Also update the global stylesheet for form widget CSS override resolution
        crate::css_parser::set_active_stylesheet(Arc::clone(&stylesheet));
        self.stylesheet = Some(stylesheet);
        self.invalidate_mouse_move_pipeline_cache();
    }

    /// Get the current stylesheet, if any
    pub fn stylesheet(&self) -> Option<&Stylesheet> {
        self.stylesheet.as_ref().map(|s| s.as_ref())
    }

    // ========================================================================
    // FLIP Animation for Subtree Rebuilds
    // ========================================================================

    // FLIP animation methods (`update_flip_bounds`, `apply_flip_transitions`,
    // `tick_flip_animations`, `apply_flip_animation_props`,
    // `has_active_flip_animations`, `has_active_visible_flip_animations`)
    // moved to `renderer/animation/flip.rs`.

    // `transfer_states_from` moved to `renderer/transfers.rs`.

    // `node_states` moved to `renderer/queries.rs`.

    // `get_bounds`, `get_absolute_bounds`, `get_render_node`,
    // `get_node_padding`, `iter_nodes` moved to `renderer/queries.rs`.
    // `get_cursor`, `has_any_cursor_style`, `get_cursor_at` moved to
    // `renderer/cursor.rs`.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::div::div;

    #[test]
    fn test_render_tree_from_element() {
        let ui = div().w(100.0).h(100.0).child(div().w(50.0).h(50.0));

        let tree = RenderTree::from_element(&ui);
        assert!(tree.root().is_some());
    }

    #[test]
    fn test_compute_layout() {
        let ui = div()
            .w(200.0)
            .h(200.0)
            .flex_col()
            .child(div().h(50.0).w_full())
            .child(div().flex_grow().w_full());

        let mut tree = RenderTree::from_element(&ui);
        tree.compute_layout(200.0, 200.0);

        let root = tree.root().unwrap();
        let bounds = tree.get_bounds(root).unwrap();

        assert_eq!(bounds.width, 200.0);
        assert_eq!(bounds.height, 200.0);
    }

    #[test]
    fn stale_subtree_rebuilds_are_dropped_after_parent_rebuild() {
        let _ = crate::stateful::take_pending_subtree_rebuilds();

        let ui = div().child(div().child(div()));
        let mut tree = RenderTree::from_element(&ui);
        let root = tree.root().unwrap();
        let parent = tree.layout_tree.children(root)[0];
        let stale_child = tree.layout_tree.children(parent)[0];

        crate::stateful::queue_subtree_rebuild(parent, div().child(div()));
        crate::stateful::queue_subtree_rebuild(stale_child, div());

        assert!(crate::stateful::has_pending_subtree_rebuilds());
        assert!(tree.process_pending_subtree_rebuilds());
        assert!(!crate::stateful::has_pending_subtree_rebuilds());

        let _ = crate::stateful::take_pending_subtree_rebuilds();
    }

    #[test]
    fn descendant_subtree_rebuilds_are_dropped_before_parent_rebuild() {
        let _ = crate::stateful::take_pending_subtree_rebuilds();

        let ui = div().child(div().child(div()));
        let mut tree = RenderTree::from_element(&ui);
        let root = tree.root().unwrap();
        let parent = tree.layout_tree.children(root)[0];
        let stale_child = tree.layout_tree.children(parent)[0];

        crate::stateful::queue_subtree_rebuild(stale_child, div().child(div()));
        crate::stateful::queue_subtree_rebuild(parent, div().child(div()));

        assert!(crate::stateful::has_pending_subtree_rebuilds());
        assert!(tree.process_pending_subtree_rebuilds());
        assert!(!crate::stateful::has_pending_subtree_rebuilds());
        assert!(tree.layout_tree.node_exists(parent));
        assert!(!tree.layout_tree.node_exists(stale_child));

        let _ = crate::stateful::take_pending_subtree_rebuilds();
    }
}
