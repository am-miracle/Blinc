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

mod cursor;
mod events;
mod queries;
mod registries;

/// A computed glass panel ready for GPU rendering
///
/// This contains all the information needed to render a glass effect,
/// with bounds computed from the layout system.
///
/// # Deprecated
/// Use `Brush::Glass` instead. Glass is now rendered as part of the
/// normal render pipeline - just use `fill_rect` with a glass brush.
#[deprecated(
    since = "0.2.0",
    note = "Use Brush::Glass instead. Glass is now integrated into the normal render pipeline."
)]
#[derive(Clone, Debug)]
pub struct GlassPanel {
    /// Absolute bounds (x, y, width, height)
    pub bounds: ElementBounds,
    /// Corner radii
    pub corner_radius: CornerRadius,
    /// Glass material properties
    pub material: GlassMaterial,
    /// The layout node this panel belongs to
    pub node_id: LayoutNodeId,
}

/// Debug statistics for the render tree
///
/// Used by `BLINC_DEBUG=motion` to display animation stats.
#[derive(Clone, Debug, Default)]
pub struct RenderTreeDebugStats {
    /// Number of active visual (FLIP) animations
    pub visual_animation_count: usize,
    /// Number of registered visual animation configs
    pub visual_animation_config_count: usize,
    /// Number of active layout animations
    pub layout_animation_count: usize,
    /// Number of pre-computed animated bounds
    pub animated_bounds_count: usize,
    /// Total number of render nodes
    pub render_node_count: usize,
    /// Number of scroll physics instances
    pub scroll_physics_count: usize,
}

/// Stores an element's type for rendering
#[derive(Clone)]
pub enum ElementType {
    /// A div/container element
    Div,
    /// A text element with content
    Text(TextData),
    /// Styled text with multiple color spans (for syntax highlighting)
    StyledText(StyledTextData),
    /// An SVG element
    Svg(SvgData),
    /// An image element
    Image(ImageData),
    /// A canvas element with custom render callback
    Canvas(CanvasData),
}

/// Text data for rendering
#[derive(Clone)]
pub struct TextData {
    pub content: String,
    pub font_size: f32,
    pub color: [f32; 4],
    pub align: crate::div::TextAlign,
    pub weight: crate::div::FontWeight,
    /// Whether to use italic style
    pub italic: bool,
    pub v_align: crate::div::TextVerticalAlign,
    /// Whether to wrap text at container bounds
    pub wrap: bool,
    /// Line height multiplier
    pub line_height: f32,
    /// Measured width (before layout constraints)
    pub measured_width: f32,
    /// Font family category
    pub font_family: crate::div::FontFamily,
    /// Word spacing in pixels (0.0 = normal)
    pub word_spacing: f32,
    /// Letter spacing in pixels (0.0 = normal)
    pub letter_spacing: f32,
    /// Font ascender in pixels (distance from baseline to top)
    pub ascender: f32,
    /// Whether text has strikethrough decoration
    pub strikethrough: bool,
    /// Whether text has underline decoration
    pub underline: bool,
}

/// A styled span within rich text
#[derive(Clone, Debug)]
pub struct StyledTextSpan {
    /// Start byte index in text
    pub start: usize,
    /// End byte index in text (exclusive)
    pub end: usize,
    /// RGBA color
    pub color: [f32; 4],
    /// Whether text is bold
    pub bold: bool,
    /// Whether text is italic
    pub italic: bool,
    /// Whether text has underline decoration
    pub underline: bool,
    /// Whether text has strikethrough decoration
    pub strikethrough: bool,
    /// Optional link URL (for clickable spans)
    pub link_url: Option<String>,
}

impl StyledTextSpan {
    /// Create a new styled text span with just color (no decorations)
    pub fn new(start: usize, end: usize, color: [f32; 4]) -> Self {
        Self {
            start,
            end,
            color,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            link_url: None,
        }
    }

    /// Create from a TextSpan (from styled_text module)
    pub fn from_text_span(span: &crate::styled_text::TextSpan) -> Self {
        Self {
            start: span.start,
            end: span.end,
            color: span.color.to_array(),
            bold: span.bold,
            italic: span.italic,
            underline: span.underline,
            strikethrough: span.strikethrough,
            link_url: span.link_url.clone(),
        }
    }
}

/// Styled text data for rendering with multiple color spans
#[derive(Clone)]
pub struct StyledTextData {
    /// The full text content
    pub content: String,
    /// Color spans (must cover entire text, sorted by start position)
    pub spans: Vec<StyledTextSpan>,
    /// Default color for unspanned regions
    pub default_color: [f32; 4],
    /// Font size
    pub font_size: f32,
    /// Text alignment
    pub align: crate::div::TextAlign,
    /// Vertical alignment
    pub v_align: crate::div::TextVerticalAlign,
    /// Font family
    pub font_family: crate::div::FontFamily,
    /// Line height multiplier
    pub line_height: f32,
    /// Default font weight (for unspanned regions)
    pub weight: crate::div::FontWeight,
    /// Default italic style (for unspanned regions)
    pub italic: bool,
    /// Measured ascender for consistent baseline alignment
    pub ascender: f32,
}

/// SVG data for rendering
#[derive(Clone)]
pub struct SvgData {
    pub source: Arc<str>,
    pub tint: Option<Color>,
    pub fill: Option<Color>,
    pub stroke: Option<Color>,
    pub stroke_width: Option<f32>,
}

/// Image data for rendering
#[derive(Clone)]
pub struct ImageData {
    /// Image source (file path, URL, or base64 data)
    pub source: String,
    /// Object-fit mode (0=cover, 1=contain, 2=fill, 3=scale-down, 4=none)
    pub object_fit: u8,
    /// Object position (x: 0.0-1.0, y: 0.0-1.0)
    pub object_position: [f32; 2],
    /// Opacity (0.0 - 1.0)
    pub opacity: f32,
    /// Border radius for rounded corners
    pub border_radius: f32,
    /// Tint color [r, g, b, a]
    pub tint: [f32; 4],
    /// Filter: [grayscale, sepia, brightness, contrast, saturate, hue_rotate, invert, blur]
    pub filter: [f32; 8],
    /// Loading strategy: 0 = Eager (default), 1 = Lazy
    pub loading_strategy: u8,
    /// Placeholder type: 0 = None, 1 = Color, 2 = Image, 3 = Skeleton
    pub placeholder_type: u8,
    /// Placeholder color [r, g, b, a]
    pub placeholder_color: [f32; 4],
    /// Placeholder image source (only used when placeholder_type == 2)
    pub placeholder_image: Option<String>,
    /// Fade-in duration in milliseconds (0 = no fade)
    pub fade_duration_ms: u32,
}

/// Node data for rendering
#[derive(Clone)]
pub struct RenderNode {
    /// Render properties
    pub props: RenderProps,
    /// Element type
    pub element_type: ElementType,
}

/// Trait for rendering layout trees with text, SVG, and glass support
///
/// Implement this trait to provide custom rendering for your platform.
/// The renderer handles:
/// - Background/foreground DrawContext separation for glass effects
/// - Text rendering at layout-computed positions
/// - SVG rendering at layout-computed positions
pub trait LayoutRenderer {
    /// Get the background DrawContext (for elements behind glass)
    fn background(&mut self) -> &mut dyn DrawContext;

    /// Get the foreground DrawContext (for elements on top of glass)
    fn foreground(&mut self) -> &mut dyn DrawContext;

    /// Render text to the foreground layer at absolute position
    ///
    /// Called for text elements that are children of glass elements.
    /// Position is absolute (after applying all parent transforms).
    #[allow(clippy::too_many_arguments)]
    fn render_text_foreground(
        &mut self,
        content: &str,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        font_size: f32,
        color: [f32; 4],
        align: crate::div::TextAlign,
        weight: crate::div::FontWeight,
    );

    /// Render text to the background layer at absolute position
    #[allow(clippy::too_many_arguments)]
    fn render_text_background(
        &mut self,
        content: &str,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        font_size: f32,
        color: [f32; 4],
        align: crate::div::TextAlign,
        weight: crate::div::FontWeight,
    );

    /// Render an SVG to the foreground layer at absolute position
    fn render_svg_foreground(
        &mut self,
        source: &str,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        tint: Option<Color>,
    );

    /// Render an SVG to the background layer at absolute position
    fn render_svg_background(
        &mut self,
        source: &str,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        tint: Option<Color>,
    );
}

/// Type-erased node state storage
pub type NodeStateStorage = Arc<Mutex<dyn Any + Send>>;

/// Storage for computed layout bounds (shared with ElementRef)
pub type LayoutBoundsStorage = Arc<Mutex<Option<ElementBounds>>>;

/// Callback type for layout bounds change notifications
pub type LayoutBoundsCallback = Arc<dyn Fn(ElementBounds) + Send + Sync>;

/// Entry for layout bounds storage with optional change callback
pub struct LayoutBoundsEntry {
    /// The shared storage for bounds
    pub storage: LayoutBoundsStorage,
    /// Optional callback when bounds change (width or height differ from previous)
    pub on_change: Option<LayoutBoundsCallback>,
}

/// Callback type for on_ready notifications when an element is laid out and rendered
///
/// The callback receives the element's computed bounds after layout.
/// This is triggered once per element after its first successful layout computation.
pub type OnReadyCallback = Arc<dyn Fn(ElementBounds) + Send + Sync>;

/// Entry for on_ready callbacks
pub struct OnReadyEntry {
    /// The callback to invoke when the element is ready
    pub callback: OnReadyCallback,
    /// Whether this callback has been triggered (only fires once)
    pub triggered: bool,
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
            painted_node_ids: RefCell::new(HashSet::new()),
            motion_bindings: HashMap::new(),
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
        }
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

    /// Build a render tree from an element builder
    pub fn from_element<E: ElementBuilder>(element: &E) -> Self {
        let mut tree = Self::new();
        // Compute tree hash for change detection
        tree.tree_hash = Some(DivHash::compute_element_tree(element));
        tree.root = Some(tree.build_element(element));
        tree
    }

    /// Build a render tree from an element builder with a shared element registry
    ///
    /// This ensures element IDs are registered to the shared registry during build,
    /// rather than to an internal registry that gets replaced later.
    pub fn from_element_with_registry<E: ElementBuilder>(
        element: &E,
        registry: Arc<ElementRegistry>,
    ) -> Self {
        let mut tree = Self::new();
        // Clear the shared registry before building to avoid duplicate ID warnings
        registry.clear();
        // Set shared registry BEFORE building so IDs are registered correctly
        tree.element_registry = registry;
        // Compute tree hash for change detection
        tree.tree_hash = Some(DivHash::compute_element_tree(element));
        tree.root = Some(tree.build_element(element));
        tree
    }

    // `tree_hash`, `matches_element` moved to `renderer/queries.rs`.

    /// Update the render tree from a new element if it has changed
    ///
    /// Returns `true` if the tree was updated, `false` if no changes were detected.
    /// This is an optimization to skip full rebuilds when the UI hasn't changed.
    pub fn update_if_changed<E: ElementBuilder>(&mut self, element: &E) -> bool {
        let new_hash = DivHash::compute_element_tree(element);

        // If hash matches, no changes - skip rebuild
        if self.tree_hash == Some(new_hash) {
            return false;
        }

        // Hash differs - need to rebuild
        // For now, do a full rebuild. Future optimization: use diff for incremental updates
        self.tree_hash = Some(new_hash);

        // Clear existing data that will be repopulated during rebuild
        self.render_nodes.clear();
        self.handler_registry = crate::event_handler::HandlerRegistry::new();
        self.element_registry.clear();
        // Clear scroll_refs HashMap (node_id keyed) - it will be repopulated during rebuild
        // but active_scroll_refs persists for process_pending_scroll_refs
        self.scroll_refs.clear();

        // Preserve node_states, scroll_offsets, scroll_physics, motion_bindings, active_scroll_refs
        // as these should survive rebuilds

        // Rebuild the layout tree
        self.layout_tree = LayoutTree::new();
        self.root = Some(self.build_element(element));

        true
    }

    /// Incrementally update the render tree from a new element
    ///
    /// This method attempts to apply minimal updates based on what changed:
    /// - If nothing changed: returns NoChanges, no work done
    /// - If only visual props changed: updates render props, returns VisualOnly
    /// - If layout changed: updates props + needs relayout, returns LayoutChanged
    /// - If children changed: rebuilds affected subtrees, returns ChildrenChanged
    ///
    /// The caller should:
    /// - NoChanges: skip layout and just render
    /// - VisualOnly: skip layout, just render with updated props
    /// - LayoutChanged: call compute_layout(), then render
    /// - ChildrenChanged: call compute_layout(), then render
    pub fn incremental_update<E: ElementBuilder>(&mut self, element: &E) -> UpdateResult {
        let new_tree_hash = DivHash::compute_element_tree(element);

        // Quick path: if tree hash matches, nothing changed
        if self.tree_hash == Some(new_tree_hash) {
            return UpdateResult::NoChanges;
        }

        // Tree hash differs - analyze what kind of changes occurred
        // Walk the tree comparing per-node hashes to detect change categories
        let Some(root_id) = self.root else {
            // No existing tree - build it (this is initial build, not an update)
            self.tree_hash = Some(new_tree_hash);
            self.root = Some(self.build_element(element));
            return UpdateResult::ChildrenChanged;
        };

        // Analyze changes by comparing stored hashes with new element
        let changes = self.analyze_changes(element, root_id);

        tracing::trace!(
            "incremental_update: layout={}, visual={}, children={}, handlers={}",
            changes.layout,
            changes.visual,
            changes.children,
            changes.handlers
        );

        // Update tree hash
        self.tree_hash = Some(new_tree_hash);

        // Determine update strategy based on change category
        if changes.children {
            // Children changed - rebuild affected subtrees in place
            // Walk tree and rebuild nodes with changed children
            self.rebuild_changed_subtrees(element, root_id);
            // Also update props for nodes that didn't get rebuilt
            self.update_render_props_in_place(element, root_id);
            UpdateResult::ChildrenChanged
        } else if changes.layout {
            // Layout changed - update props and need relayout
            self.update_render_props_in_place(element, root_id);
            UpdateResult::LayoutChanged
        } else if changes.visual || changes.handlers {
            // Only visual/handler changes - update props in place, no layout needed
            self.update_render_props_in_place(element, root_id);
            UpdateResult::VisualOnly
        } else {
            // No changes detected (shouldn't happen if tree hash differed)
            UpdateResult::NoChanges
        }
    }

    /// Rebuild subtrees for nodes with changed children
    ///
    /// This walks the tree comparing stored hashes with the new element tree.
    /// When it finds a node whose children have changed (different count),
    /// it rebuilds that subtree in place.
    fn rebuild_changed_subtrees<E: ElementBuilder>(&mut self, element: &E, node_id: LayoutNodeId) {
        let child_node_ids = self.layout_tree.children(node_id);
        let child_builders = element.children_builders();

        // Check if children count changed - rebuild children of this node
        if child_node_ids.len() != child_builders.len() {
            self.rebuild_children_in_place(node_id, child_builders);
            return;
        }

        // Same child count - check each child for deeper changes
        for (child_builder, &child_node_id) in child_builders.iter().zip(child_node_ids.iter()) {
            // Get stored hash for this child
            if let Some(&(_, stored_tree_hash)) = self.node_hashes.get(&child_node_id) {
                let new_tree_hash = DivHash::compute_element_tree(child_builder.as_ref());
                if stored_tree_hash != new_tree_hash {
                    // Child's subtree changed - check if it's the child count or deeper changes
                    let child_children_count = self.layout_tree.children(child_node_id).len();
                    let new_children_count = child_builder.children_builders().len();

                    if child_children_count != new_children_count {
                        // This child's children changed - rebuild its children
                        self.rebuild_children_in_place(
                            child_node_id,
                            child_builder.children_builders(),
                        );
                    } else {
                        // Recurse to find deeper changes
                        self.rebuild_changed_subtrees_boxed(child_builder.as_ref(), child_node_id);
                    }
                }
            }
        }
    }

    /// Rebuild subtrees for boxed element builder
    fn rebuild_changed_subtrees_boxed(
        &mut self,
        element: &dyn ElementBuilder,
        node_id: LayoutNodeId,
    ) {
        let child_node_ids = self.layout_tree.children(node_id);
        let child_builders = element.children_builders();

        if child_node_ids.len() != child_builders.len() {
            self.rebuild_children_in_place(node_id, child_builders);
            return;
        }

        for (child_builder, &child_node_id) in child_builders.iter().zip(child_node_ids.iter()) {
            if let Some(&(_, stored_tree_hash)) = self.node_hashes.get(&child_node_id) {
                let new_tree_hash = DivHash::compute_element_tree(child_builder.as_ref());
                if stored_tree_hash != new_tree_hash {
                    let child_children_count = self.layout_tree.children(child_node_id).len();
                    let new_children_count = child_builder.children_builders().len();

                    if child_children_count != new_children_count {
                        self.rebuild_children_in_place(
                            child_node_id,
                            child_builder.children_builders(),
                        );
                    } else {
                        self.rebuild_changed_subtrees_boxed(child_builder.as_ref(), child_node_id);
                    }
                }
            }
        }
    }

    /// Rebuild children of a node in place
    ///
    /// This removes old children and builds new ones from the provided element builders.
    fn rebuild_children_in_place(
        &mut self,
        parent_id: LayoutNodeId,
        new_children: &[Box<dyn ElementBuilder>],
    ) {
        // Remove old children
        let old_children = self.layout_tree.children(parent_id);
        for child_id in &old_children {
            self.remove_subtree_nodes(*child_id);
        }
        self.layout_tree.clear_children(parent_id);

        // Build new children
        for child in new_children {
            let child_id = child.build(&mut self.layout_tree);
            self.layout_tree.add_child(parent_id, child_id);
            self.collect_render_props_boxed(child.as_ref(), child_id);
        }
    }

    /// Analyze what categories of changes occurred between stored tree and new element
    fn analyze_changes<E: ElementBuilder>(
        &self,
        element: &E,
        node_id: LayoutNodeId,
    ) -> ChangeCategory {
        let mut changes = ChangeCategory::none();

        // Get stored hash for this node
        let Some(&(stored_own_hash, stored_tree_hash)) = self.node_hashes.get(&node_id) else {
            // No stored hash - treat as everything changed
            changes.layout = true;
            changes.visual = true;
            changes.children = true;
            return changes;
        };

        // Compute new hashes
        let new_own_hash = DivHash::compute_element(element);
        let new_tree_hash = DivHash::compute_element_tree(element);

        // If tree hashes match, nothing changed in this subtree
        if stored_tree_hash == new_tree_hash {
            return changes;
        }

        // Tree hash differs - analyze further
        if stored_own_hash != new_own_hash {
            // This node's own properties changed
            // Check render props to distinguish visual vs layout
            if let Some(old_render_node) = self.render_nodes.get(&node_id) {
                let new_props = element.render_props();
                let old_props = &old_render_node.props;

                // Visual change detection: compare render-only properties
                if !Self::props_visually_equal(old_props, &new_props) {
                    changes.visual = true;
                }

                // Layout change: if hash differs but not just visual, assume layout changed
                // (We can't access Style directly from ElementBuilder, so we infer)
                if !changes.visual {
                    changes.layout = true;
                }
            } else {
                // No old render node - everything changed
                changes.layout = true;
                changes.visual = true;
            }
        }

        // Check children
        let child_node_ids = self.layout_tree.children(node_id);
        let child_builders = element.children_builders();

        // Different number of children = structural change
        if child_node_ids.len() != child_builders.len() {
            changes.children = true;
            return changes;
        }

        // Recursively check children
        for (child_builder, &child_node_id) in child_builders.iter().zip(child_node_ids.iter()) {
            let child_changes = self.analyze_changes_boxed(child_builder.as_ref(), child_node_id);
            changes.layout = changes.layout || child_changes.layout;
            changes.visual = changes.visual || child_changes.visual;
            changes.children = changes.children || child_changes.children;
            changes.handlers = changes.handlers || child_changes.handlers;

            // Short circuit if children changed (need full rebuild anyway)
            if changes.children {
                return changes;
            }
        }

        changes
    }

    /// Analyze changes for a boxed element builder
    fn analyze_changes_boxed(
        &self,
        element: &dyn ElementBuilder,
        node_id: LayoutNodeId,
    ) -> ChangeCategory {
        let mut changes = ChangeCategory::none();

        let Some(&(stored_own_hash, stored_tree_hash)) = self.node_hashes.get(&node_id) else {
            changes.layout = true;
            changes.visual = true;
            changes.children = true;
            return changes;
        };

        let new_own_hash = DivHash::compute_element(element);
        let new_tree_hash = DivHash::compute_element_tree(element);

        if stored_tree_hash == new_tree_hash {
            return changes;
        }

        if stored_own_hash != new_own_hash {
            if let Some(old_render_node) = self.render_nodes.get(&node_id) {
                let new_props = element.render_props();
                let old_props = &old_render_node.props;

                if !Self::props_visually_equal(old_props, &new_props) {
                    changes.visual = true;
                }
                if !changes.visual {
                    changes.layout = true;
                }
            } else {
                changes.layout = true;
                changes.visual = true;
            }
        }

        let child_node_ids = self.layout_tree.children(node_id);
        let child_builders = element.children_builders();

        if child_node_ids.len() != child_builders.len() {
            changes.children = true;
            return changes;
        }

        for (child_builder, &child_node_id) in child_builders.iter().zip(child_node_ids.iter()) {
            let child_changes = self.analyze_changes_boxed(child_builder.as_ref(), child_node_id);
            changes.layout = changes.layout || child_changes.layout;
            changes.visual = changes.visual || child_changes.visual;
            changes.children = changes.children || child_changes.children;
            changes.handlers = changes.handlers || child_changes.handlers;

            if changes.children {
                return changes;
            }
        }

        changes
    }

    /// Compare render props for visual equality
    fn props_visually_equal(old: &RenderProps, new: &RenderProps) -> bool {
        render_props_eq(old, new)
    }

    /// Update render props in place without rebuilding the tree
    fn update_render_props_in_place<E: ElementBuilder>(
        &mut self,
        element: &E,
        node_id: LayoutNodeId,
    ) {
        // Update this node's props
        if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
            let mut new_props = element.render_props();
            new_props.node_id = Some(node_id);
            // Preserve motion from old props (set by parent)
            new_props.motion = render_node.props.motion.clone();
            render_node.props = new_props;
        } else {
            // Render node doesn't exist - create it
            tracing::debug!(
                "update_render_props_in_place: creating missing render_node for {:?}",
                node_id
            );
            let mut new_props = element.render_props();
            new_props.node_id = Some(node_id);
            let element_type = Self::determine_element_type(element);
            self.render_nodes.insert(
                node_id,
                RenderNode {
                    props: new_props,
                    element_type,
                },
            );
        }

        // Update taffy node's layout style if element provides one
        // This is critical for layout changes (width, height, padding, etc.)
        if let Some(style) = element.layout_style() {
            self.layout_tree.set_style(node_id, style.clone());
        }

        // Update stored hash
        let own_hash = DivHash::compute_element(element);
        let tree_hash = DivHash::compute_element_tree(element);
        self.node_hashes.insert(node_id, (own_hash, tree_hash));

        // Update event handlers
        if let Some(handlers) = element.event_handlers() {
            self.handler_registry.register(node_id, handlers.clone());
        }

        // Update scroll physics if this is a scroll element
        if let Some(physics) = element.scroll_physics() {
            tracing::trace!("Registering scroll physics for node {:?}", node_id);
            // Set the animation scheduler for bounce springs
            if let Some(scheduler) = self.animations.upgrade() {
                physics.lock().unwrap().set_scheduler(&scheduler);
            }
            self.scroll_physics.insert(node_id, physics);
            if element.viewport_cull() {
                self.viewport_cull_scrolls.insert(node_id);
            }
        }

        // Update motion bindings if this element has continuous animations
        if let Some(bindings) = element.motion_bindings() {
            self.motion_bindings.insert(node_id, bindings);
        }

        // Register layout bounds storage if element wants bounds updates
        self.register_element_bounds_storage(node_id, element);

        // Recursively update children
        let child_node_ids = self.layout_tree.children(node_id);
        let child_builders = element.children_builders();

        // Handle mismatch between layout children and builder children
        if child_node_ids.len() != child_builders.len() {
            // Rebuild children in place to fix the mismatch
            self.rebuild_children_in_place(node_id, child_builders);
        } else {
            for (child_builder, &child_node_id) in child_builders.iter().zip(child_node_ids.iter())
            {
                self.update_render_props_in_place_boxed(child_builder.as_ref(), child_node_id);
            }
        }
    }

    /// Update render props for a boxed element builder
    fn update_render_props_in_place_boxed(
        &mut self,
        element: &dyn ElementBuilder,
        node_id: LayoutNodeId,
    ) {
        if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
            let mut new_props = element.render_props();
            new_props.node_id = Some(node_id);
            new_props.motion = render_node.props.motion.clone();
            render_node.props = new_props;
        } else {
            // Render node doesn't exist - this can happen if the tree structure changed
            // but rebuild_children_in_place wasn't called for this subtree.
            // Create a new render node entry.
            tracing::debug!(
                "update_render_props_in_place_boxed: creating missing render_node for {:?}",
                node_id
            );
            let mut new_props = element.render_props();
            new_props.node_id = Some(node_id);
            let element_type = Self::determine_element_type_boxed(element);
            self.render_nodes.insert(
                node_id,
                RenderNode {
                    props: new_props,
                    element_type,
                },
            );
        }

        // Update taffy node's layout style if element provides one
        // This is critical for layout changes (width, height, padding, etc.)
        if let Some(style) = element.layout_style() {
            self.layout_tree.set_style(node_id, style.clone());
        }

        let own_hash = DivHash::compute_element(element);
        let tree_hash = DivHash::compute_element_tree(element);
        self.node_hashes.insert(node_id, (own_hash, tree_hash));

        if let Some(handlers) = element.event_handlers() {
            self.handler_registry.register(node_id, handlers.clone());
        }

        // Update scroll physics if this is a scroll element
        if let Some(physics) = element.scroll_physics() {
            // Set the animation scheduler for bounce springs
            if let Some(scheduler) = self.animations.upgrade() {
                physics.lock().unwrap().set_scheduler(&scheduler);
            }
            self.scroll_physics.insert(node_id, physics);
            if element.viewport_cull() {
                self.viewport_cull_scrolls.insert(node_id);
            }
        }

        // Update motion bindings if this element has continuous animations
        if let Some(bindings) = element.motion_bindings() {
            self.motion_bindings.insert(node_id, bindings);
        }

        // Register layout bounds storage if element wants bounds updates
        self.register_element_bounds_storage(node_id, element);

        let child_node_ids = self.layout_tree.children(node_id);
        let child_builders = element.children_builders();

        // Handle mismatch between layout children and builder children
        if child_node_ids.len() != child_builders.len() {
            // Rebuild children in place to fix the mismatch
            self.rebuild_children_in_place(node_id, child_builders);
        } else {
            for (child_builder, &child_node_id) in child_builders.iter().zip(child_node_ids.iter())
            {
                self.update_render_props_in_place_boxed(child_builder.as_ref(), child_node_id);
            }
        }
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

    /// Recursively build elements into the tree
    ///
    /// This builds the layout tree first (via element.build()), then walks the
    /// element tree again to collect render properties for each node.
    fn build_element<E: ElementBuilder>(&mut self, element: &E) -> LayoutNodeId {
        // First, build the entire layout tree (this creates all nodes and parent-child relationships)
        let root_id = element.build(&mut self.layout_tree);

        // Now walk the element tree to collect render props for each node
        self.collect_render_props(element, root_id);

        root_id
    }

    /// Collect render properties from an element and its children
    fn collect_render_props<E: ElementBuilder>(&mut self, element: &E, node_id: LayoutNodeId) {
        let mut props = element.render_props();
        props.node_id = Some(node_id);

        // Apply base CSS styles and animation from stylesheet if element has an ID
        if let Some(ref stylesheet) = self.stylesheet {
            if let Some(id) = element.element_id() {
                // Apply base styles (background, opacity, border-radius, etc.)
                if let Some(base_style) = stylesheet.get(id) {
                    Self::apply_element_style_to_props(&mut props, base_style);
                }
                // Apply CSS animation (only if no motion animation is already set)
                if props.motion.is_none() {
                    if let Some(motion) = stylesheet.resolve_animation(id) {
                        props.motion = Some(motion);
                    }
                }
            }
        }

        // Inherit CSS text properties from parent (text-decoration, white-space, etc.)
        self.inherit_text_props_from_parent(&mut props, node_id);

        // Determine element type using the trait methods
        let element_type = Self::determine_element_type(element);

        self.render_nodes.insert(
            node_id,
            RenderNode {
                props,
                element_type,
            },
        );

        // Store per-node hashes for incremental update detection
        let own_hash = DivHash::compute_element(element);
        let tree_hash = DivHash::compute_element_tree(element);
        self.node_hashes.insert(node_id, (own_hash, tree_hash));

        // Register event handlers if present
        if let Some(handlers) = element.event_handlers() {
            self.handler_registry.register(node_id, handlers.clone());
        }

        // Store scroll physics if this is a scroll element
        if let Some(physics) = element.scroll_physics() {
            // Set the animation scheduler for bounce springs
            if let Some(scheduler) = self.animations.upgrade() {
                physics.lock().unwrap().set_scheduler(&scheduler);
            }
            self.scroll_physics.insert(node_id, physics);
            if element.viewport_cull() {
                self.viewport_cull_scrolls.insert(node_id);
            }
        }

        // Store motion bindings if this element has continuous animations
        if let Some(bindings) = element.motion_bindings() {
            self.motion_bindings.insert(node_id, bindings);
        }

        // Register layout bounds storage if element wants bounds updates
        self.register_element_bounds_storage(node_id, element);

        // Register layout animation config if element wants animated layout transitions
        if let Some(config) = element.layout_animation_config() {
            tracing::debug!(
                "collect_render_props: registered layout animation config for {:?}",
                node_id
            );
            self.layout_animation_configs.insert(node_id, config);
        }

        // Register visual animation config for new FLIP-style system
        if let Some(config) = element.visual_animation_config() {
            tracing::trace!(
                "[VISUAL_ANIM] collect_render_props: registering config for {:?}, key={:?}",
                node_id,
                config.key
            );
            self.register_visual_animation_config(node_id, config);
        }

        // Register element ID if present (for selector API)
        if let Some(id) = element.element_id() {
            self.element_registry.register(id, node_id);
        }

        // Register CSS classes for complex selector matching
        let classes = element.element_classes();
        if !classes.is_empty() {
            self.element_registry
                .register_classes(node_id, classes.to_vec());
        }

        // Register semantic element type for CSS type selector matching
        if let Some(type_name) = element.semantic_type_name() {
            self.element_registry
                .register_element_type(node_id, type_name);
        }

        // Bind ScrollRef if present (for scroll containers)
        if let Some(scroll_ref) = element.bound_scroll_ref() {
            self.register_scroll_ref(node_id, scroll_ref);
        }

        // Get child node IDs from the layout tree
        let child_node_ids = self.layout_tree.children(node_id);
        let child_builders = element.children_builders();

        // Log mismatch to help debug stateful/motion issues (in collect_render_props)
        if child_node_ids.len() != child_builders.len() && !child_node_ids.is_empty() {
            tracing::warn!(
                "collect_render_props: node {:?} has {} layout children but {} builder children (mismatch!)",
                node_id, child_node_ids.len(), child_builders.len()
            );
        }

        let total_children = child_node_ids.len();

        // Match children by index (they were built in order)
        for (index, (child_builder, &child_node_id)) in
            child_builders.iter().zip(child_node_ids.iter()).enumerate()
        {
            // Register parent-child relationship and child index
            self.element_registry
                .register_parent(child_node_id, node_id);
            self.element_registry
                .register_child_index(child_node_id, index, total_children);
            self.collect_render_props_boxed(child_builder.as_ref(), child_node_id);
        }
    }

    /// Collect render props from a boxed element builder
    fn collect_render_props_boxed(&mut self, element: &dyn ElementBuilder, node_id: LayoutNodeId) {
        // Debug: See all element types being collected
        let eid = element.element_type_id();
        // eprintln!("collect_render_props_boxed: node={:?}, type_id={:?}", node_id, eid);

        let mut props = element.render_props();
        props.node_id = Some(node_id);

        // Apply base CSS styles and animation from stylesheet if element has an ID
        if let Some(ref stylesheet) = self.stylesheet {
            if let Some(id) = element.element_id() {
                // Apply base styles (background, opacity, border-radius, etc.)
                if let Some(base_style) = stylesheet.get(id) {
                    Self::apply_element_style_to_props(&mut props, base_style);
                }
                // Apply CSS animation (only if no motion animation is already set)
                if props.motion.is_none() {
                    if let Some(motion) = stylesheet.resolve_animation(id) {
                        props.motion = Some(motion);
                    }
                }
            }
        }

        // Inherit CSS text properties from parent (text-decoration, white-space, etc.)
        self.inherit_text_props_from_parent(&mut props, node_id);

        // Use the element_type_id to determine type
        let type_id_boxed = element.element_type_id();
        if matches!(type_id_boxed, ElementTypeId::Canvas) {
            let render_fn = element.canvas_render_info();
            // eprintln!(
            //     "collect_render_props_boxed: ElementTypeId::Canvas detected! has_render_fn={}",
            //     render_fn.is_some()
            // );
        }
        let element_type = match type_id_boxed {
            ElementTypeId::Text => {
                if let Some(info) = element.text_render_info() {
                    ElementType::Text(Self::build_text_data(info, &props))
                } else {
                    ElementType::Div
                }
            }
            ElementTypeId::Svg => {
                if let Some(info) = element.svg_render_info() {
                    ElementType::Svg(SvgData {
                        source: info.source,
                        tint: info.tint,
                        fill: info.fill,
                        stroke: info.stroke,
                        stroke_width: info.stroke_width,
                    })
                } else {
                    ElementType::Div
                }
            }
            ElementTypeId::Image => {
                if let Some(info) = element.image_render_info() {
                    ElementType::Image(ImageData {
                        source: info.source,
                        object_fit: info.object_fit,
                        object_position: info.object_position,
                        opacity: info.opacity,
                        border_radius: info.border_radius,
                        tint: info.tint,
                        filter: info.filter,
                        loading_strategy: info.loading_strategy,
                        placeholder_type: info.placeholder_type,
                        placeholder_color: info.placeholder_color,
                        placeholder_image: info.placeholder_image,
                        fade_duration_ms: info.fade_duration_ms,
                    })
                } else {
                    ElementType::Div
                }
            }
            ElementTypeId::Canvas => ElementType::Canvas(CanvasData {
                render_fn: element.canvas_render_info(),
            }),
            ElementTypeId::StyledText => {
                if let Some(info) = element.styled_text_render_info() {
                    ElementType::StyledText(StyledTextData {
                        content: info.content,
                        spans: info
                            .spans
                            .into_iter()
                            .map(|s| StyledTextSpan {
                                start: s.start,
                                end: s.end,
                                color: s.color,
                                bold: s.bold,
                                italic: s.italic,
                                underline: s.underline,
                                strikethrough: s.strikethrough,
                                link_url: s.link_url,
                            })
                            .collect(),
                        default_color: info.default_color,
                        font_size: info.font_size,
                        align: info.align,
                        v_align: info.v_align,
                        font_family: info.font_family,
                        line_height: info.line_height,
                        weight: info.weight,
                        italic: info.italic,
                        ascender: info.ascender,
                    })
                } else {
                    ElementType::Div
                }
            }
            ElementTypeId::Div => ElementType::Div,
            ElementTypeId::Motion => ElementType::Div, // Motion is a transparent container
        };

        self.render_nodes.insert(
            node_id,
            RenderNode {
                props,
                element_type,
            },
        );

        // Store per-node hashes for incremental update detection
        let own_hash = DivHash::compute_element(element);
        let tree_hash = DivHash::compute_element_tree(element);
        self.node_hashes.insert(node_id, (own_hash, tree_hash));

        // Register event handlers if present
        if let Some(handlers) = element.event_handlers() {
            self.handler_registry.register(node_id, handlers.clone());
        }

        // Store scroll physics if this is a scroll element
        if let Some(physics) = element.scroll_physics() {
            // Set the animation scheduler for bounce springs
            if let Some(scheduler) = self.animations.upgrade() {
                physics.lock().unwrap().set_scheduler(&scheduler);
            }
            self.scroll_physics.insert(node_id, physics);
            if element.viewport_cull() {
                self.viewport_cull_scrolls.insert(node_id);
            }
        }

        // Store motion bindings if this element has continuous animations
        if let Some(bindings) = element.motion_bindings() {
            self.motion_bindings.insert(node_id, bindings);
        }

        // Register layout bounds storage if element wants bounds updates
        self.register_element_bounds_storage(node_id, element);

        // Register layout animation config if element wants animated layout transitions
        if let Some(config) = element.layout_animation_config() {
            tracing::debug!(
                "collect_render_props_boxed: registered layout animation config for {:?}",
                node_id
            );
            self.layout_animation_configs.insert(node_id, config);
        }

        // Register visual animation config for new FLIP-style system
        if let Some(config) = element.visual_animation_config() {
            tracing::trace!(
                "[VISUAL_ANIM] collect_render_props_boxed: registering config for {:?}, key={:?}",
                node_id,
                config.key
            );
            self.register_visual_animation_config(node_id, config);
        }

        // Register element ID if present (for selector API)
        if let Some(id) = element.element_id() {
            self.element_registry.register(id, node_id);
        }

        // Register CSS classes for complex selector matching
        let classes = element.element_classes();
        if !classes.is_empty() {
            self.element_registry
                .register_classes(node_id, classes.to_vec());
        }

        // Register semantic element type for CSS type selector matching
        if let Some(type_name) = element.semantic_type_name() {
            self.element_registry
                .register_element_type(node_id, type_name);
        }

        // Bind ScrollRef if present (for scroll containers)
        if let Some(scroll_ref) = element.bound_scroll_ref() {
            self.register_scroll_ref(node_id, scroll_ref);
        }

        // Get child node IDs from the layout tree
        let child_node_ids = self.layout_tree.children(node_id);
        let child_builders = element.children_builders();

        // Debug: warn on mismatch (in collect_render_props_boxed)
        if child_node_ids.len() != child_builders.len() {
            tracing::warn!(
                "collect_render_props_boxed: node {:?} has {} layout children but {} builder children",
                node_id,
                child_node_ids.len(),
                child_builders.len()
            );
        }

        let total_children = child_node_ids.len();

        // Check if this is a Motion container
        let is_motion = element.element_type_id() == ElementTypeId::Motion;
        // Get stable ID from Motion container (for overlay animations that survive tree rebuilds)
        let motion_stable_id = if is_motion {
            element.motion_stable_id().map(|s| s.to_string())
        } else {
            None
        };
        // Get replay, suspended, and exiting flags from Motion container
        let motion_should_replay = if is_motion {
            element.motion_should_replay()
        } else {
            false
        };
        let motion_is_suspended = if is_motion {
            element.motion_is_suspended()
        } else {
            false
        };
        // DEPRECATED: motion_is_exiting is no longer used for triggering exit.
        // Motion exit is now triggered explicitly via MotionHandle.exit().
        // This field is kept for backwards compatibility but always false.
        #[allow(deprecated)]
        let motion_is_exiting = if is_motion {
            element.motion_is_exiting()
        } else {
            false
        };
        // Get on_ready callback from Motion container for suspended animations
        let motion_on_ready_callback = if is_motion {
            element.motion_on_ready_callback()
        } else {
            None
        };

        // Match children by index (they were built in order)
        for (index, (child_builder, &child_node_id)) in
            child_builders.iter().zip(child_node_ids.iter()).enumerate()
        {
            // Register parent-child relationship and child index
            self.element_registry
                .register_parent(child_node_id, node_id);
            self.element_registry
                .register_child_index(child_node_id, index, total_children);

            // If parent is Motion, propagate motion animation to child
            if is_motion {
                if let Some(motion_config) = element.motion_animation_for_child(index) {
                    // Append child index to stable key for unique stagger animations
                    let child_stable_id = motion_stable_id
                        .as_ref()
                        .map(|key| format!("{}:child:{}", key, index));
                    self.collect_render_props_boxed_with_motion(
                        child_builder.as_ref(),
                        child_node_id,
                        Some(motion_config),
                        child_stable_id,
                        motion_should_replay,
                        motion_is_suspended,
                        motion_is_exiting,
                        motion_on_ready_callback.clone(),
                    );
                    continue;
                }
            }
            self.collect_render_props_boxed(child_builder.as_ref(), child_node_id);
        }
    }

    /// Collect render props with motion animation config from parent
    #[allow(deprecated, clippy::too_many_arguments)]
    fn collect_render_props_boxed_with_motion(
        &mut self,
        element: &dyn ElementBuilder,
        node_id: LayoutNodeId,
        motion_config: Option<crate::element::MotionAnimation>,
        motion_stable_id: Option<String>,
        motion_should_replay: bool,
        motion_is_suspended: bool,
        motion_is_exiting: bool,
        motion_on_ready_callback: Option<
            std::sync::Arc<dyn Fn(crate::element::ElementBounds) + Send + Sync>,
        >,
    ) {
        let mut props = element.render_props();
        props.node_id = Some(node_id);

        // Motion config from parent takes precedence
        if motion_config.is_some() {
            props.motion = motion_config;
            props.motion_stable_id = motion_stable_id.clone();
            props.motion_should_replay = motion_should_replay;
            props.motion_is_suspended = motion_is_suspended;
            props.motion_on_ready_callback = motion_on_ready_callback;
            // DEPRECATED: motion_is_exiting is no longer used for triggering exit.
            // Motion exit is now triggered explicitly via MotionHandle.exit().
            props.motion_is_exiting = motion_is_exiting;

            // Queue replay with the CHILD's stable key (includes :child:N suffix)
            // This ensures replay uses the same key as initialize_motion_animations
            if motion_should_replay {
                if let Some(ref key) = motion_stable_id {
                    crate::render_state::queue_global_motion_replay(key.clone());
                }
            }
        } else {
            // Apply base CSS styles and animation from stylesheet if element has an ID
            if let Some(ref stylesheet) = self.stylesheet {
                if let Some(id) = element.element_id() {
                    // Apply base styles (background, opacity, border-radius, etc.)
                    if let Some(base_style) = stylesheet.get(id) {
                        Self::apply_element_style_to_props(&mut props, base_style);
                    }
                    // Apply CSS animation (only if no motion animation is already set)
                    if props.motion.is_none() {
                        if let Some(motion) = stylesheet.resolve_animation(id) {
                            props.motion = Some(motion);
                        }
                    }
                }
            }
        }

        // Inherit CSS text properties from parent (text-decoration, white-space, etc.)
        self.inherit_text_props_from_parent(&mut props, node_id);

        // Use the element_type_id to determine type
        let element_type = match element.element_type_id() {
            ElementTypeId::Text => {
                if let Some(info) = element.text_render_info() {
                    ElementType::Text(Self::build_text_data(info, &props))
                } else {
                    ElementType::Div
                }
            }
            ElementTypeId::Svg => {
                if let Some(info) = element.svg_render_info() {
                    ElementType::Svg(SvgData {
                        source: info.source,
                        tint: info.tint,
                        fill: info.fill,
                        stroke: info.stroke,
                        stroke_width: info.stroke_width,
                    })
                } else {
                    ElementType::Div
                }
            }
            ElementTypeId::Image => {
                if let Some(info) = element.image_render_info() {
                    ElementType::Image(ImageData {
                        source: info.source,
                        object_fit: info.object_fit,
                        object_position: info.object_position,
                        opacity: info.opacity,
                        border_radius: info.border_radius,
                        tint: info.tint,
                        filter: info.filter,
                        loading_strategy: info.loading_strategy,
                        placeholder_type: info.placeholder_type,
                        placeholder_color: info.placeholder_color,
                        placeholder_image: info.placeholder_image,
                        fade_duration_ms: info.fade_duration_ms,
                    })
                } else {
                    ElementType::Div
                }
            }
            ElementTypeId::Canvas => ElementType::Canvas(CanvasData {
                render_fn: element.canvas_render_info(),
            }),
            ElementTypeId::StyledText => {
                if let Some(info) = element.styled_text_render_info() {
                    ElementType::StyledText(StyledTextData {
                        content: info.content,
                        spans: info
                            .spans
                            .into_iter()
                            .map(|s| StyledTextSpan {
                                start: s.start,
                                end: s.end,
                                color: s.color,
                                bold: s.bold,
                                italic: s.italic,
                                underline: s.underline,
                                strikethrough: s.strikethrough,
                                link_url: s.link_url,
                            })
                            .collect(),
                        default_color: info.default_color,
                        font_size: info.font_size,
                        align: info.align,
                        v_align: info.v_align,
                        font_family: info.font_family,
                        line_height: info.line_height,
                        weight: info.weight,
                        italic: info.italic,
                        ascender: info.ascender,
                    })
                } else {
                    ElementType::Div
                }
            }
            ElementTypeId::Div => ElementType::Div,
            ElementTypeId::Motion => ElementType::Div,
        };

        self.render_nodes.insert(
            node_id,
            RenderNode {
                props,
                element_type,
            },
        );

        // Store per-node hashes for incremental update detection
        let own_hash = DivHash::compute_element(element);
        let tree_hash = DivHash::compute_element_tree(element);
        self.node_hashes.insert(node_id, (own_hash, tree_hash));

        // Register event handlers if present
        if let Some(handlers) = element.event_handlers() {
            self.handler_registry.register(node_id, handlers.clone());
        }

        // Store scroll physics if this is a scroll element
        if let Some(physics) = element.scroll_physics() {
            // Set the animation scheduler for bounce springs
            if let Some(scheduler) = self.animations.upgrade() {
                physics.lock().unwrap().set_scheduler(&scheduler);
            }
            self.scroll_physics.insert(node_id, physics);
            if element.viewport_cull() {
                self.viewport_cull_scrolls.insert(node_id);
            }
        }

        // Store motion bindings if this element has continuous animations
        if let Some(bindings) = element.motion_bindings() {
            self.motion_bindings.insert(node_id, bindings);
        }

        // Register layout bounds storage if element wants bounds updates
        self.register_element_bounds_storage(node_id, element);

        // Register layout animation config if element wants animated layout transitions
        if let Some(config) = element.layout_animation_config() {
            self.layout_animation_configs.insert(node_id, config);
        }

        // Register visual animation config for new FLIP-style system
        if let Some(config) = element.visual_animation_config() {
            self.register_visual_animation_config(node_id, config);
        }

        // Register element ID if present (for selector API)
        if let Some(id) = element.element_id() {
            self.element_registry.register(id, node_id);
        }

        // Register CSS classes for complex selector matching
        let classes = element.element_classes();
        if !classes.is_empty() {
            self.element_registry
                .register_classes(node_id, classes.to_vec());
        }

        // Register semantic element type for CSS type selector matching
        if let Some(type_name) = element.semantic_type_name() {
            self.element_registry
                .register_element_type(node_id, type_name);
        }

        // Bind ScrollRef if present (for scroll containers)
        if let Some(scroll_ref) = element.bound_scroll_ref() {
            self.register_scroll_ref(node_id, scroll_ref);
        }

        // Recursively process children (without motion - motion only applies to direct children)
        let child_node_ids = self.layout_tree.children(node_id);
        let child_builders = element.children_builders();
        let total_children = child_node_ids.len();

        for (index, (child_builder, &child_node_id)) in
            child_builders.iter().zip(child_node_ids.iter()).enumerate()
        {
            self.element_registry
                .register_parent(child_node_id, node_id);
            self.element_registry
                .register_child_index(child_node_id, index, total_children);
            self.collect_render_props_boxed(child_builder.as_ref(), child_node_id);
        }
    }

    /// Determine element type from an element builder
    fn determine_element_type<E: ElementBuilder>(element: &E) -> ElementType {
        let type_id = element.element_type_id();
        if matches!(type_id, ElementTypeId::Canvas) {
            tracing::trace!("determine_element_type: ElementTypeId::Canvas detected!");
        }
        let default_props = RenderProps::default();
        match type_id {
            ElementTypeId::Text => {
                if let Some(info) = element.text_render_info() {
                    ElementType::Text(Self::build_text_data(info, &default_props))
                } else {
                    ElementType::Div
                }
            }
            ElementTypeId::Svg => {
                if let Some(info) = element.svg_render_info() {
                    ElementType::Svg(SvgData {
                        source: info.source,
                        tint: info.tint,
                        fill: info.fill,
                        stroke: info.stroke,
                        stroke_width: info.stroke_width,
                    })
                } else {
                    ElementType::Div
                }
            }
            ElementTypeId::Image => {
                if let Some(info) = element.image_render_info() {
                    ElementType::Image(ImageData {
                        source: info.source,
                        object_fit: info.object_fit,
                        object_position: info.object_position,
                        opacity: info.opacity,
                        border_radius: info.border_radius,
                        tint: info.tint,
                        filter: info.filter,
                        loading_strategy: info.loading_strategy,
                        placeholder_type: info.placeholder_type,
                        placeholder_color: info.placeholder_color,
                        placeholder_image: info.placeholder_image,
                        fade_duration_ms: info.fade_duration_ms,
                    })
                } else {
                    ElementType::Div
                }
            }
            ElementTypeId::Canvas => ElementType::Canvas(CanvasData {
                render_fn: element.canvas_render_info(),
            }),
            ElementTypeId::StyledText => {
                if let Some(info) = element.styled_text_render_info() {
                    ElementType::StyledText(StyledTextData {
                        content: info.content,
                        spans: info
                            .spans
                            .into_iter()
                            .map(|s| StyledTextSpan {
                                start: s.start,
                                end: s.end,
                                color: s.color,
                                bold: s.bold,
                                italic: s.italic,
                                underline: s.underline,
                                strikethrough: s.strikethrough,
                                link_url: s.link_url,
                            })
                            .collect(),
                        default_color: info.default_color,
                        font_size: info.font_size,
                        align: info.align,
                        v_align: info.v_align,
                        font_family: info.font_family,
                        line_height: info.line_height,
                        weight: info.weight,
                        italic: info.italic,
                        ascender: info.ascender,
                    })
                } else {
                    ElementType::Div
                }
            }
            ElementTypeId::Div => ElementType::Div,
            ElementTypeId::Motion => ElementType::Div, // Motion is a transparent container
        }
    }

    /// Determine element type from a boxed element builder
    fn determine_element_type_boxed(element: &dyn ElementBuilder) -> ElementType {
        let type_id = element.element_type_id();
        if matches!(type_id, ElementTypeId::Canvas) {
            tracing::trace!("determine_element_type_boxed: ElementTypeId::Canvas detected!");
        }
        let default_props = RenderProps::default();
        match type_id {
            ElementTypeId::Text => {
                if let Some(info) = element.text_render_info() {
                    ElementType::Text(Self::build_text_data(info, &default_props))
                } else {
                    ElementType::Div
                }
            }
            ElementTypeId::StyledText => {
                if let Some(info) = element.styled_text_render_info() {
                    ElementType::StyledText(StyledTextData {
                        content: info.content,
                        spans: info
                            .spans
                            .into_iter()
                            .map(|s| StyledTextSpan {
                                start: s.start,
                                end: s.end,
                                color: s.color,
                                bold: s.bold,
                                italic: s.italic,
                                underline: s.underline,
                                strikethrough: s.strikethrough,
                                link_url: s.link_url,
                            })
                            .collect(),
                        default_color: info.default_color,
                        font_size: info.font_size,
                        align: info.align,
                        v_align: info.v_align,
                        font_family: info.font_family,
                        line_height: info.line_height,
                        weight: info.weight,
                        italic: info.italic,
                        ascender: info.ascender,
                    })
                } else {
                    ElementType::Div
                }
            }
            ElementTypeId::Svg => {
                if let Some(info) = element.svg_render_info() {
                    ElementType::Svg(SvgData {
                        source: info.source,
                        tint: info.tint,
                        fill: info.fill,
                        stroke: info.stroke,
                        stroke_width: info.stroke_width,
                    })
                } else {
                    ElementType::Div
                }
            }
            ElementTypeId::Image => {
                if let Some(info) = element.image_render_info() {
                    ElementType::Image(ImageData {
                        source: info.source,
                        object_fit: info.object_fit,
                        object_position: info.object_position,
                        opacity: info.opacity,
                        border_radius: info.border_radius,
                        tint: info.tint,
                        filter: info.filter,
                        loading_strategy: info.loading_strategy,
                        placeholder_type: info.placeholder_type,
                        placeholder_color: info.placeholder_color,
                        placeholder_image: info.placeholder_image,
                        fade_duration_ms: info.fade_duration_ms,
                    })
                } else {
                    ElementType::Div
                }
            }
            ElementTypeId::Canvas => ElementType::Canvas(CanvasData {
                render_fn: element.canvas_render_info(),
            }),
            ElementTypeId::Div => ElementType::Div,
            ElementTypeId::Motion => ElementType::Div,
        }
    }

    /// Get the root node ID
    pub fn root(&self) -> Option<LayoutNodeId> {
        self.root
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

    /// Check if any layout animations are currently active
    pub fn has_active_layout_animations(&self) -> bool {
        self.layout_animations
            .values()
            .any(|state| state.is_animating())
            || self
                .layout_animations_by_key
                .values()
                .any(|state| state.is_animating())
    }

    /// Get animated bounds for a node if a layout animation is active
    ///
    /// Returns the current animated bounds, or None if no animation is active.
    /// Checks both node ID based and stable key based animations.
    pub fn get_animated_bounds(&self, node_id: LayoutNodeId) -> Option<ElementBounds> {
        // First check node ID based animations
        if let Some(state) = self.layout_animations.get(&node_id) {
            return Some(state.current_bounds());
        }

        // Check stable key based animations
        // Look up if this node has a config with a stable key
        if let Some(config) = self.layout_animation_configs.get(&node_id) {
            if let Some(ref stable_key) = config.stable_key {
                if let Some(state) = self.layout_animations_by_key.get(stable_key) {
                    return Some(state.current_bounds());
                }
            }
        }

        None
    }

    /// Get bounds for rendering, using animated bounds if available
    ///
    /// This returns the animated bounds if a layout animation is active,
    /// otherwise returns the layout bounds from taffy.
    pub fn get_render_bounds(
        &self,
        node_id: LayoutNodeId,
        parent_offset: (f32, f32),
    ) -> Option<ElementBounds> {
        // Check if this node has an ACTIVE visual animation
        // Apply animation offsets to layout bounds (keeps parent-relative coordinates)
        if let Some(key) = self
            .visual_animation_key_to_node
            .iter()
            .find(|(_, &n)| n == node_id)
            .map(|(k, _)| k.clone())
        {
            if let Some(anim) = self.visual_animations.get(&key) {
                if anim.is_animating() {
                    // Get layout bounds (relative to parent)
                    let layout = self.layout_tree.get_bounds(node_id, parent_offset)?;
                    // Apply animation offsets to layout bounds
                    return Some(ElementBounds {
                        x: layout.x + anim.offset.get_x(),
                        y: layout.y + anim.offset.get_y(),
                        width: (layout.width + anim.size_delta.get_width()).max(0.0),
                        height: (layout.height + anim.size_delta.get_height()).max(0.0),
                    });
                }
            }
        }

        // Then try old layout animations (node ID based) - deprecated
        if let Some(anim_bounds) = self.layout_animations.get(&node_id) {
            let current = anim_bounds.current_bounds();
            return Some(ElementBounds {
                x: current.x + parent_offset.0,
                y: current.y + parent_offset.1,
                width: current.width,
                height: current.height,
            });
        }

        // Try stable key based animations - deprecated
        if let Some(config) = self.layout_animation_configs.get(&node_id) {
            if let Some(ref stable_key) = config.stable_key {
                if let Some(anim_state) = self.layout_animations_by_key.get(stable_key) {
                    let current = anim_state.current_bounds();
                    return Some(ElementBounds {
                        x: current.x + parent_offset.0,
                        y: current.y + parent_offset.1,
                        width: current.width,
                        height: current.height,
                    });
                }
            }
        }

        // Fall back to layout bounds
        self.layout_tree.get_bounds(node_id, parent_offset)
    }

    /// Check if a specific node has an active layout animation
    pub fn is_layout_animating(&self, node_id: LayoutNodeId) -> bool {
        // Check node ID based
        if self
            .layout_animations
            .get(&node_id)
            .map(|s| s.is_animating())
            .unwrap_or(false)
        {
            return true;
        }

        // Check stable key based
        if let Some(config) = self.layout_animation_configs.get(&node_id) {
            if let Some(ref stable_key) = config.stable_key {
                if self
                    .layout_animations_by_key
                    .get(stable_key)
                    .map(|s| s.is_animating())
                    .unwrap_or(false)
                {
                    return true;
                }
            }
        }

        false
    }

    // `clear_layout_bounds_storages` moved to `renderer/registries.rs`.

    // =========================================================================
    // Visual Animation System (FLIP-style, read-only layout)
    // =========================================================================

    /// Register a visual animation config for a node
    ///
    /// This associates a VisualAnimationConfig with a node for FLIP-style animations.
    /// The animation tracks visual offsets from layout bounds, never modifying taffy.
    pub fn register_visual_animation_config(
        &mut self,
        node_id: LayoutNodeId,
        config: VisualAnimationConfig,
    ) {
        let key = config
            .key
            .clone()
            .unwrap_or_else(|| format!("node_{:?}", node_id));

        tracing::trace!(
            "[VISUAL_ANIM] Registering config: node={:?}, key={}",
            node_id,
            key
        );

        self.visual_animation_configs.insert(key.clone(), config);
        // key→node direction ensures we always have the current node for each key
        // (overwrites any stale node_id from previous rebuild)
        self.visual_animation_key_to_node.insert(key, node_id);
    }

    /// Update visual animations for nodes with changed bounds
    ///
    /// This implements the FLIP technique:
    /// 1. Compare current layout bounds vs previous visual bounds
    /// 2. Calculate offset = previous - current (the "Invert" step)
    /// 3. Create animation that plays offset back to 0 (the "Play" step)
    ///
    /// Called after layout computation but before rendering.
    fn update_visual_animations(&mut self) {
        if !self.visual_animation_configs.is_empty() {
            tracing::trace!(
                "[VISUAL_ANIM] update_visual_animations: {} configs registered",
                self.visual_animation_configs.len()
            );
        }
        if self.visual_animation_configs.is_empty() {
            return;
        }

        // Get animation scheduler handle
        let scheduler_handle = if let Some(arc) = self.animations.upgrade() {
            arc.lock().unwrap().handle()
        } else if let Some(handle) = crate::render_state::get_global_scheduler() {
            handle
        } else {
            tracing::warn!("No animation scheduler available for visual animations");
            return;
        };

        // Process each registered config
        for (key, config) in &self.visual_animation_configs {
            // Get the current node ID for this key (directly from key→node map)
            let Some(&node_id) = self.visual_animation_key_to_node.get(key) else {
                continue;
            };

            // Get current layout bounds from taffy
            let Some(layout_bounds) = self.layout_tree.get_bounds(node_id, (0.0, 0.0)) else {
                continue;
            };

            // Get previous visual bounds (what was rendered last frame)
            let prev_visual = self.previous_visual_bounds.get(key).copied();

            // Check if we have an existing animation
            if let Some(existing_anim) = self.visual_animations.get_mut(key) {
                // Animation in progress - update target if layout changed
                if (existing_anim.to_bounds.width - layout_bounds.width).abs() > 0.5
                    || (existing_anim.to_bounds.height - layout_bounds.height).abs() > 0.5
                    || (existing_anim.to_bounds.x - layout_bounds.x).abs() > 0.5
                    || (existing_anim.to_bounds.y - layout_bounds.y).abs() > 0.5
                {
                    tracing::debug!(
                        "Visual animation: updating target for key='{}', to_bounds changed",
                        key
                    );
                    existing_anim.update_target(layout_bounds, scheduler_handle.clone());
                }

                // Store current visual bounds for next frame
                let current_visual = existing_anim.current_visual_bounds();
                self.previous_visual_bounds
                    .insert(key.clone(), current_visual);
            } else if let Some(prev) = prev_visual {
                // No animation yet - check if bounds changed significantly
                let bounds_changed = (prev.width - layout_bounds.width).abs() > config.threshold
                    || (prev.height - layout_bounds.height).abs() > config.threshold
                    || (prev.x - layout_bounds.x).abs() > config.threshold
                    || (prev.y - layout_bounds.y).abs() > config.threshold;

                if bounds_changed {
                    // Create new FLIP animation: from prev visual, to current layout
                    if let Some(anim) = VisualAnimation::from_bounds_change(
                        key.clone(),
                        prev,
                        layout_bounds,
                        config,
                        scheduler_handle.clone(),
                    ) {
                        tracing::debug!(
                            "Visual animation: created for key='{}', from={:?} to={:?}, direction={:?}",
                            key,
                            prev,
                            layout_bounds,
                            anim.direction
                        );
                        self.visual_animations.insert(key.clone(), anim);
                    }
                }

                // Store current visual bounds for next frame
                // (use layout since no animation is active)
                self.previous_visual_bounds
                    .insert(key.clone(), layout_bounds);
            } else {
                // First frame - just store current layout bounds
                self.previous_visual_bounds
                    .insert(key.clone(), layout_bounds);
            }
        }

        // Cleanup completed animations
        self.visual_animations.retain(|key, anim| {
            let is_animating = anim.is_animating();
            if !is_animating {
                tracing::debug!(
                    "Visual animation: cleaning up completed animation for key='{}'",
                    key
                );
            }
            is_animating
        });
    }

    /// Compute animated render bounds for all nodes
    ///
    /// This is the hierarchical computation phase:
    /// 1. Start from root with identity bounds
    /// 2. For each node, calculate its animated bounds accounting for:
    ///    - Own animation state (if any)
    ///    - Parent's animated offset (inherited)
    /// 3. Store pre-computed bounds for use during rendering
    ///
    /// Called after update_visual_animations().
    fn compute_animated_render_bounds(&mut self) {
        // Clear previous computation
        self.animated_render_bounds.clear();

        // Get root node
        let Some(root_id) = self.root else {
            return;
        };

        // Start recursive computation from root
        self.compute_bounds_recursive(
            root_id, 0.0, 0.0, // Parent offset starts at screen origin
        );
    }

    /// Recursively compute animated render bounds for a subtree
    fn compute_bounds_recursive(&mut self, node_id: LayoutNodeId, parent_x: f32, parent_y: f32) {
        // Get layout bounds relative to parent (from taffy)
        let Some(layout_bounds) = self.layout_tree.get_bounds(node_id, (0.0, 0.0)) else {
            return;
        };

        // Check if this node has an active visual animation
        // Find the key for this node (reverse lookup - O(n) but typically few animated nodes)
        let stable_key = self
            .visual_animation_key_to_node
            .iter()
            .find(|(_, &n)| n == node_id)
            .map(|(k, _)| k.clone());
        let animation = stable_key
            .as_ref()
            .and_then(|k| self.visual_animations.get(k));
        let config = stable_key
            .as_ref()
            .and_then(|k| self.visual_animation_configs.get(k));

        // Calculate this node's animated bounds
        let animated_bounds = if let Some(anim) = animation {
            // Node has active animation - apply visual offset
            let dx = anim.offset.get_x();
            let dy = anim.offset.get_y();
            let dw = anim.size_delta.get_width();
            let dh = anim.size_delta.get_height();

            // Position: layout + parent offset + animation offset
            let x = parent_x + layout_bounds.x + dx;
            let y = parent_y + layout_bounds.y + dy;

            // Size: layout + animation delta
            let width = layout_bounds.width + dw;
            let height = layout_bounds.height + dh;

            // Determine clip rect based on animation direction and config
            let clip_rect = if let Some(cfg) = config {
                use crate::visual_animation::ClipBehavior;
                match cfg.clip_behavior {
                    ClipBehavior::ClipToAnimated => {
                        // Clip to animated (current) bounds
                        Some(Rect::new(0.0, 0.0, width.max(0.0), height.max(0.0)))
                    }
                    ClipBehavior::ClipToLayout => {
                        // Clip to layout (target) bounds
                        Some(Rect::new(
                            0.0,
                            0.0,
                            layout_bounds.width,
                            layout_bounds.height,
                        ))
                    }
                    ClipBehavior::NoClip => None,
                }
            } else {
                // Default: clip to animated bounds
                Some(Rect::new(0.0, 0.0, width.max(0.0), height.max(0.0)))
            };

            AnimatedRenderBounds {
                x,
                y,
                width,
                height,
                clip_rect,
            }
        } else {
            // No animation - use layout bounds + parent offset
            AnimatedRenderBounds {
                x: parent_x + layout_bounds.x,
                y: parent_y + layout_bounds.y,
                width: layout_bounds.width,
                height: layout_bounds.height,
                clip_rect: None,
            }
        };

        // Store computed bounds
        let child_parent_x = animated_bounds.x;
        let child_parent_y = animated_bounds.y;
        self.animated_render_bounds.insert(node_id, animated_bounds);

        // Recursively compute for children
        // Children use THIS node's animated position as their parent offset
        let children = self.layout_tree.children(node_id);
        for child_id in children {
            self.compute_bounds_recursive(child_id, child_parent_x, child_parent_y);
        }
    }

    /// Get visual animated render bounds for a node
    ///
    /// Returns pre-computed animated bounds if available, otherwise None.
    /// Use this during rendering to get hierarchically-correct animated positions.
    pub fn get_visual_render_bounds(&self, node_id: LayoutNodeId) -> Option<&AnimatedRenderBounds> {
        self.animated_render_bounds.get(&node_id)
    }

    /// Check if any visual animations are currently active
    pub fn has_active_visual_animations(&self) -> bool {
        self.visual_animations.values().any(|a| a.is_animating())
    }

    /// Visibility-gated counterpart of `has_active_visual_animations`.
    /// Only counts animations whose target node was painted in the
    /// most recent frame (i.e. the node id is present in `painted`).
    /// See `CssAnimationStore::has_visible_active` for the rationale.
    pub fn has_active_visible_visual_animations(&self, painted: &HashSet<LayoutNodeId>) -> bool {
        self.visual_animations.iter().any(|(key, anim)| {
            if !anim.is_animating() {
                return false;
            }
            self.visual_animation_key_to_node
                .get(key)
                .is_some_and(|n| painted.contains(n))
        })
    }

    /// Check if a specific node has an active visual animation
    pub fn is_visual_animating(&self, node_id: LayoutNodeId) -> bool {
        // Find the key for this node (reverse lookup)
        self.visual_animation_key_to_node
            .iter()
            .find(|(_, &n)| n == node_id)
            .and_then(|(key, _)| self.visual_animations.get(key))
            .map(|a| a.is_animating())
            .unwrap_or(false)
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

    /// Update scroll physics with content dimensions from layout
    fn update_scroll_content_dimensions(&mut self) {
        // Collect node_ids to avoid borrowing issues
        let node_ids: Vec<_> = self.scroll_physics.keys().copied().collect();

        for node_id in node_ids {
            // Get viewport bounds (the scroll container's own size)
            let bounds = self.layout_tree.get_bounds(node_id, (0.0, 0.0));
            let viewport_width = bounds.map(|b| b.width).unwrap_or(0.0);
            let viewport_height = bounds.map(|b| b.height).unwrap_or(0.0);

            // Get content size from Taffy's content_size (enabled via feature)
            // This tells us the total size of all content that may overflow
            let (content_width, content_height) = self
                .layout_tree
                .get_content_size(node_id)
                .unwrap_or((viewport_width, viewport_height));

            // Update physics with dimensions
            if let Some(physics) = self.scroll_physics.get(&node_id) {
                if let Ok(mut p) = physics.lock() {
                    p.viewport_width = viewport_width;
                    p.viewport_height = viewport_height;
                    p.content_width = content_width;
                    p.content_height = content_height;
                }
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

    /// Get a bound ScrollRef by node ID
    pub fn scroll_ref(&self, node_id: LayoutNodeId) -> Option<&ScrollRef> {
        self.scroll_refs.get(&node_id)
    }

    /// Register a ScrollRef for a scroll container node
    ///
    /// This binds the ScrollRef to the node and adds it to both the node-keyed
    /// HashMap (for quick lookup) and the active_scroll_refs Vec (for persistence
    /// across rebuilds).
    fn register_scroll_ref(&mut self, node_id: LayoutNodeId, scroll_ref: &ScrollRef) {
        scroll_ref.bind_to_node(node_id, Arc::downgrade(&self.element_registry));
        self.scroll_refs.insert(node_id, scroll_ref.clone());
        // Also track in active_scroll_refs for persistence across rebuilds
        // Check if already present by comparing inner pointer
        let inner_ptr = Arc::as_ptr(&scroll_ref.inner());
        if !self
            .active_scroll_refs
            .iter()
            .any(|sr| Arc::as_ptr(&sr.inner()) == inner_ptr)
        {
            self.active_scroll_refs.push(scroll_ref.clone());
        }
    }

    /// Process all pending scroll operations from bound ScrollRefs
    ///
    /// This should be called each frame before rendering to apply any
    /// programmatic scroll commands (scroll_to, scroll_to_bottom, etc.).
    ///
    /// Returns true if any scroll state was modified.
    pub fn process_pending_scroll_refs(&mut self) -> bool {
        use crate::selector::PendingScroll;

        let mut any_modified = false;

        // Collect scroll refs that have pending operations from active_scroll_refs
        // (active_scroll_refs persists across rebuilds, unlike scroll_refs HashMap)
        let pending: Vec<_> = self
            .active_scroll_refs
            .iter()
            .filter_map(|scroll_ref| {
                let node_id = scroll_ref.node_id()?;
                scroll_ref
                    .take_pending_scroll()
                    .map(|pending| (node_id, pending))
            })
            .collect();
        for (node_id, pending_scroll) in pending {
            let Some(physics) = self.scroll_physics.get(&node_id) else {
                continue;
            };

            let mut physics = physics.lock().unwrap();
            any_modified = true;

            match pending_scroll {
                PendingScroll::ToOffset { x, y, smooth: _ } => {
                    // For now, instant scroll (smooth animation TBD)
                    physics.offset_x = -x;
                    physics.offset_y = -y;
                }
                PendingScroll::ByAmount { dx, dy, smooth: _ } => {
                    physics.apply_scroll_delta(dx, dy);
                }
                PendingScroll::ToTop { smooth: _ } => {
                    physics.offset_y = 0.0;
                }
                PendingScroll::ToBottom { smooth: _ } => {
                    physics.offset_y = physics.max_offset_y();
                }
                PendingScroll::ToElement {
                    element_id,
                    options,
                } => {
                    // Look up element bounds and scroll to make it visible
                    if let Some(target_node) = self.element_registry.get(&element_id) {
                        // Get target element's bounds
                        if let Some(target_bounds) = self.get_bounds(target_node) {
                            // Get scroll container's bounds
                            if let Some(container_bounds) = self.get_bounds(node_id) {
                                // Calculate scroll offset to bring element into view
                                // Element's position relative to scroll container
                                let relative_y = target_bounds.y - container_bounds.y;
                                let relative_x = target_bounds.x - container_bounds.x;

                                // Scroll to center the element (or just make it visible)
                                let viewport_height = physics.viewport_height;
                                let viewport_width = physics.viewport_width;

                                // Calculate target offsets
                                // Center vertically
                                let target_center_y =
                                    relative_y + target_bounds.height / 2.0 - viewport_height / 2.0;
                                let target_offset_y = (-target_center_y)
                                    .clamp(physics.max_offset_y(), physics.min_offset_y());

                                // Center horizontally
                                let target_center_x =
                                    relative_x + target_bounds.width / 2.0 - viewport_width / 2.0;
                                let target_offset_x = (-target_center_x)
                                    .clamp(physics.max_offset_x(), physics.min_offset_x());

                                // Use smooth animation if requested
                                if options.behavior == crate::selector::ScrollBehavior::Smooth {
                                    physics.scroll_to_animated(target_offset_x, target_offset_y);
                                } else {
                                    // Instant scroll
                                    physics.offset_y = target_offset_y;
                                    if matches!(
                                        physics.config.direction,
                                        crate::scroll::ScrollDirection::Horizontal
                                            | crate::scroll::ScrollDirection::Both
                                    ) {
                                        physics.offset_x = target_offset_x;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Update ScrollRef with current state
            if let Some(scroll_ref) = self.scroll_refs.get(&node_id) {
                scroll_ref.update_state(
                    (physics.offset_x.abs(), physics.offset_y.abs()),
                    (physics.content_width, physics.content_height),
                    (physics.viewport_width, physics.viewport_height),
                );
            }
        }

        any_modified
    }

    // Event-dispatch surface (`dispatch_event*`,
    // `dispatch_text_input_event*`, `dispatch_key_event*`,
    // `broadcast_text_input_event`, `broadcast_key_event`,
    // `dispatch_scroll_event`) moved to `renderer/events.rs`. Scroll
    // chain dispatch (`dispatch_scroll_chain*`, `dispatch_pinch_chain`)
    // stays here for now — it's tightly coupled to scroll_physics
    // consumption and will go with the scroll module in a later phase.

    /// Dispatch scroll event through ancestor chain with consumption tracking
    ///
    /// For nested scrolls, inner scrolls consume delta for their direction,
    /// and outer scrolls only receive the remaining delta.
    ///
    /// - `hit_node`: The innermost node under the cursor
    /// - `ancestors`: The ancestor chain from root to hit_node
    /// - Returns the remaining delta after all consumption
    pub fn dispatch_scroll_chain(
        &mut self,
        hit_node: LayoutNodeId,
        ancestors: &[LayoutNodeId],
        mouse_x: f32,
        mouse_y: f32,
        mut delta_x: f32,
        mut delta_y: f32,
    ) -> (f32, f32) {
        // Routing rule: the scroll goes to whichever scrollable the
        // cursor is *over*. No chaining to ancestors when the inner
        // container reaches its edge — that behaviour (CSS-style scroll
        // chaining) reads as the parent "stealing" the gesture
        // mid-scroll, especially with high-rate wheel events where the
        // handoff happens in a single tick. If the user wants to scroll
        // the parent, they move the cursor off the inner container.
        //
        // Find the first node in the hit chain (leaf → root) that has a
        // scroll handler or physics. That's the sole target.
        let mut chain: Vec<LayoutNodeId> = vec![hit_node];
        for &ancestor in ancestors.iter().rev() {
            if ancestor != hit_node {
                chain.push(ancestor);
            }
        }
        let now_ms = crate::widgets::text_input::elapsed_ms() as f64;

        let mut target: Option<LayoutNodeId> = None;
        for &node_id in &chain {
            let has_handler = self
                .handler_registry
                .has_handler(node_id, blinc_core::events::event_types::SCROLL);
            let has_registered_physics = self.scroll_physics.contains_key(&node_id);
            if has_handler || has_registered_physics {
                target = Some(node_id);
                break;
            }
        }

        let Some(node_id) = target else {
            return (delta_x, delta_y);
        };

        let direction = self.get_scroll_direction(node_id);
        let has_scroll_physics = direction.is_some();
        let handles_x = direction.map_or(true, |d| {
            matches!(
                d,
                crate::scroll::ScrollDirection::Horizontal | crate::scroll::ScrollDirection::Both
            )
        });
        let handles_y = direction.map_or(true, |d| {
            matches!(
                d,
                crate::scroll::ScrollDirection::Vertical | crate::scroll::ScrollDirection::Both
            )
        });

        let dispatch_x = if handles_x { delta_x } else { 0.0 };
        let dispatch_y = if handles_y { delta_y } else { 0.0 };

        tracing::trace!(
            "scroll_disp node={:?} dir={:?} handles=({},{}) dispatch=({:.1},{:.1})",
            node_id,
            direction,
            handles_x,
            handles_y,
            dispatch_x,
            dispatch_y
        );

        if dispatch_x.abs() > 0.001 || dispatch_y.abs() > 0.001 {
            if has_scroll_physics {
                if let Some(physics) = self.scroll_physics.get(&node_id) {
                    let mut p = physics.lock().unwrap();
                    p.apply_touch_scroll_delta(dispatch_x, dispatch_y, now_ms);
                    p.on_scroll_activity();
                }
                self.last_scroll_target = Some((node_id, now_ms));
            } else {
                let ctx = crate::event_handler::EventContext::new(
                    blinc_core::events::event_types::SCROLL,
                    node_id,
                )
                .with_mouse_pos(mouse_x, mouse_y)
                .with_scroll_delta(dispatch_x, dispatch_y);
                self.handler_registry.dispatch(&ctx);
                self.last_scroll_target = Some((node_id, now_ms));
            }
        }

        // If the target didn't handle an axis (direction mismatch), let
        // that axis return as unconsumed so a caller that cares about
        // "nothing handled this wheel event" (e.g. a horizontal-only
        // inner over a vertical parent — classic case) can still do
        // something with it. We *don't* apply to anyone else in the
        // chain; cross-axis passthrough is the only allowed handoff.
        if handles_x {
            delta_x = 0.0;
        }
        if handles_y {
            delta_y = 0.0;
        }

        (delta_x, delta_y)
    }

    /// Dispatch scroll with time for touch velocity tracking (mobile)
    ///
    /// Same as dispatch_scroll_chain but includes time for momentum scrolling.
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch_scroll_chain_with_time(
        &mut self,
        hit_node: LayoutNodeId,
        ancestors: &[LayoutNodeId],
        mouse_x: f32,
        mouse_y: f32,
        delta_x: f32,
        delta_y: f32,
        scroll_time: f64,
    ) -> (f32, f32) {
        // Build the chain from leaf to root
        let mut chain: Vec<LayoutNodeId> = vec![hit_node];
        for &ancestor in ancestors.iter().rev() {
            if ancestor != hit_node {
                chain.push(ancestor);
            }
        }

        // See `dispatch_scroll_chain` for the routing rationale: the
        // cursor's current scrollable gets the delta, with no chaining.
        let mut target: Option<LayoutNodeId> = None;
        for &node_id in &chain {
            let has_handler = self
                .handler_registry
                .has_handler(node_id, blinc_core::events::event_types::SCROLL);
            let has_registered_physics = self.scroll_physics.contains_key(&node_id);
            if has_handler || has_registered_physics {
                target = Some(node_id);
                break;
            }
        }

        let Some(node_id) = target else {
            return (delta_x, delta_y);
        };

        let direction = self.get_scroll_direction(node_id);
        let has_scroll_physics = direction.is_some();
        let handles_x = direction.map_or(true, |d| {
            matches!(
                d,
                crate::scroll::ScrollDirection::Horizontal | crate::scroll::ScrollDirection::Both
            )
        });
        let handles_y = direction.map_or(true, |d| {
            matches!(
                d,
                crate::scroll::ScrollDirection::Vertical | crate::scroll::ScrollDirection::Both
            )
        });

        let dispatch_x = if handles_x { delta_x } else { 0.0 };
        let dispatch_y = if handles_y { delta_y } else { 0.0 };

        let mut remaining_dx = delta_x;
        let mut remaining_dy = delta_y;

        if dispatch_x.abs() > 0.001 || dispatch_y.abs() > 0.001 {
            if has_scroll_physics {
                if let Some(physics) = self.scroll_physics.get(&node_id) {
                    let mut p = physics.lock().unwrap();
                    p.apply_touch_scroll_delta(dispatch_x, dispatch_y, scroll_time);
                    p.on_scroll_activity();
                }
                self.last_scroll_target = Some((node_id, scroll_time));
            } else {
                let ctx = crate::event_handler::EventContext::new(
                    blinc_core::events::event_types::SCROLL,
                    node_id,
                )
                .with_mouse_pos(mouse_x, mouse_y)
                .with_scroll_delta(dispatch_x, dispatch_y)
                .with_scroll_time(scroll_time);
                self.handler_registry.dispatch(&ctx);
                self.last_scroll_target = Some((node_id, scroll_time));
            }
        }

        // Mark handled axes consumed; cross-axis falls through unchanged.
        if handles_x {
            remaining_dx = 0.0;
        }
        if handles_y {
            remaining_dy = 0.0;
        }

        (remaining_dx, remaining_dy)
    }

    /// Dispatch a pinch event to the first handler in the hit chain (leaf -> root)
    pub fn dispatch_pinch_chain(
        &mut self,
        hit: &crate::event_router::HitTestResult,
        center_x: f32,
        center_y: f32,
        scale: f32,
    ) {
        let event_type = blinc_core::events::event_types::PINCH;

        let chain = std::iter::once(hit.node).chain(
            hit.ancestors
                .iter()
                .rev()
                .copied()
                .filter(|ancestor| *ancestor != hit.node),
        );

        for node_id in chain {
            if !self.handler_registry.has_handler(node_id, event_type) {
                continue;
            }

            let (bounds_x, bounds_y, bounds_width, bounds_height, local_x, local_y) =
                if node_id == hit.node {
                    (
                        hit.bounds_x,
                        hit.bounds_y,
                        hit.bounds_width,
                        hit.bounds_height,
                        hit.local_x,
                        hit.local_y,
                    )
                } else if let Some((bx, by, bw, bh)) = hit.ancestor_bounds.get(&node_id.to_raw()) {
                    (*bx, *by, *bw, *bh, center_x - *bx, center_y - *by)
                } else {
                    continue;
                };

            self.dispatch_event_full(
                node_id,
                event_type,
                center_x,
                center_y,
                local_x,
                local_y,
                bounds_x,
                bounds_y,
                bounds_width,
                bounds_height,
                0.0,
                0.0,
                scale,
            );
            return;
        }
    }

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

    // =========================================================================
    // Scroll Offset Management
    // =========================================================================

    /// Apply a scroll delta to a node's scroll offset (without bounds checking)
    pub fn apply_scroll_delta(&mut self, node_id: LayoutNodeId, delta_x: f32, delta_y: f32) {
        let (current_x, current_y) = self
            .scroll_offsets
            .get(&node_id)
            .copied()
            .unwrap_or((0.0, 0.0));
        self.scroll_offsets
            .insert(node_id, (current_x + delta_x, current_y + delta_y));
    }

    /// Apply a scroll delta with bounds checking based on viewport and content size
    pub fn apply_scroll_delta_with_bounds(
        &mut self,
        node_id: LayoutNodeId,
        delta_x: f32,
        delta_y: f32,
    ) {
        let (current_x, current_y) = self
            .scroll_offsets
            .get(&node_id)
            .copied()
            .unwrap_or((0.0, 0.0));

        // Get the viewport bounds for this node (parent offset doesn't matter for size)
        let bounds = self.layout_tree.get_bounds(node_id, (0.0, 0.0));
        let viewport_width = bounds.map(|b| b.width).unwrap_or(0.0);
        let viewport_height = bounds.map(|b| b.height).unwrap_or(0.0);

        // Get content size from Taffy's content_size
        let (content_width, content_height) = self
            .layout_tree
            .get_content_size(node_id)
            .unwrap_or((viewport_width, viewport_height));

        // Calculate scroll limits
        let min_offset_x = 0.0;
        let max_offset_x = if content_width > viewport_width {
            -(content_width - viewport_width)
        } else {
            0.0
        };
        let min_offset_y = 0.0;
        let max_offset_y = if content_height > viewport_height {
            -(content_height - viewport_height)
        } else {
            0.0
        };

        // Apply delta with clamping
        let new_x = (current_x + delta_x).clamp(max_offset_x, min_offset_x);
        let new_y = (current_y + delta_y).clamp(max_offset_y, min_offset_y);

        tracing::debug!(
            "Scroll bounds: viewport=({:.0}, {:.0}) content=({:.0}, {:.0}) limits_y=({:.0}, {:.0}) delta_y={:.1} current={:.1} new={:.1}",
            viewport_width, viewport_height, content_width, content_height,
            max_offset_y, min_offset_y, delta_y, current_y, new_y
        );

        self.scroll_offsets.insert(node_id, (new_x, new_y));
    }

    /// Set the scroll offset for a node
    pub fn set_scroll_offset(&mut self, node_id: LayoutNodeId, offset_x: f32, offset_y: f32) {
        self.scroll_offsets.insert(node_id, (offset_x, offset_y));
    }

    /// Scroll the currently-focused text input (or text area) into view above
    /// the soft keyboard.
    ///
    /// Called by mobile platform runners (`blinc_app::android`,
    /// `blinc_app::ios`) whenever the soft-keyboard inset changes — usually
    /// in response to `UIKeyboardWillChangeFrameNotification` (iOS) or a
    /// `WindowInsets.Type.ime()` callback (Android).
    ///
    /// Behavior:
    ///
    /// 1. Look up the currently focused text input via the global focus
    ///    tracker in `widgets::text_input` (or `widgets::text_area`). If
    ///    nothing is focused, return without doing anything.
    /// 2. Walk the focused node's ancestors looking for the nearest enclosing
    ///    scroll container. If none is found, return — there's no scroll
    ///    surface to adjust.
    /// 3. Compute how much the input is currently obscured by the keyboard:
    ///    `obstruction = max(0, input.bottom + margin - (viewport.height - keyboard_inset))`
    ///    where `viewport.height` is the full window logical height. The
    ///    margin (default 16 px) keeps a comfortable gap between the input
    ///    and the keyboard top edge.
    /// 4. If `obstruction > 0`, scroll the container up by that amount,
    ///    clamping to the container's content size so we don't over-scroll.
    ///    Scroll offsets in Blinc are negative for "content moved up" so
    ///    we subtract from the current y offset.
    /// 5. If the keyboard hides (`keyboard_inset == 0`), do not auto-scroll
    ///    back — the user can keep their current position. The original
    ///    position would require remembering pre-keyboard scroll state per
    ///    container, which is fragile across rebuilds.
    ///
    /// `viewport_height` is the **logical** window height in the same units
    /// the layout tree uses (UIKit points on iOS, density-independent
    /// pixels on Android). `keyboard_inset` is the keyboard's height in
    /// the same units. Both come from `WindowedContext`.
    ///
    /// Returns `true` if any scroll offset was updated (so the caller knows
    /// to request a redraw); `false` otherwise.
    pub fn scroll_focused_text_input_above_keyboard(
        &mut self,
        viewport_height: f32,
        keyboard_inset: f32,
    ) -> bool {
        if keyboard_inset <= 0.0 {
            // Nothing to scroll above — the keyboard is hidden.
            return false;
        }

        // Find the focused text-editable node.
        //
        // The generic `focused_editable_node_id` is the modern lookup —
        // every text-editable widget (`text_input`, `text_area`,
        // `code_editor`, `rich_text_editor`) writes its layout node id
        // there on focus, so a single lookup covers all of them. The
        // typed `focused_text_input_node_id` / `focused_text_area_node_id`
        // calls are kept as fallbacks in case any widget grows a focus
        // path that bypasses the generic atomic (or for older code that
        // sets the typed trackers but not the generic one).
        let focused_node = crate::widgets::text_input::focused_editable_node_id()
            .or_else(crate::widgets::text_input::focused_text_input_node_id)
            .or_else(crate::widgets::text_input::focused_text_area_node_id);

        let Some(focused_node) = focused_node else {
            return false;
        };

        // Walk ancestors to find the nearest scroll container.
        let scroll_container = self
            .layout_tree
            .ancestors(focused_node)
            .into_iter()
            .find(|&ancestor| self.is_scroll_container(ancestor));

        let Some(scroll_container) = scroll_container else {
            // The focused input isn't inside any scroll container — there's
            // no surface to scroll. Caller falls back to other strategies
            // (e.g. shrinking the safe area or letting the keyboard cover
            // the input).
            return false;
        };

        // Get absolute bounds for the focused input. `get_absolute_bounds`
        // already accounts for ancestor scroll offsets, so the returned
        // y is the input's actual on-screen position right now.
        let Some(input_bounds) = self.get_absolute_bounds(focused_node) else {
            return false;
        };

        // Visible bottom edge of the screen — anything below this is
        // covered by the soft keyboard.
        const MARGIN: f32 = 16.0;
        let visible_bottom = viewport_height - keyboard_inset;
        let input_bottom = input_bounds.y + input_bounds.height;
        let obstruction = (input_bottom + MARGIN) - visible_bottom;

        if obstruction <= 0.0 {
            // Already fully visible above the keyboard.
            return false;
        }

        // Apply the scroll. Blinc scroll offsets are negative for
        // "content moved up", so we subtract `obstruction` from the
        // current Y offset.
        let (current_x, current_y) = self.get_scroll_offset(scroll_container);
        let target_y = current_y - obstruction;

        // Clamp to the container's max scroll. The viewport / content
        // sizes come from the layout tree directly so the calculation
        // matches `dispatch_scroll_chain_with_time`'s clamping logic.
        let scroll_bounds = self.layout_tree.get_bounds(scroll_container, (0.0, 0.0));
        let scroll_viewport_h = scroll_bounds.map(|b| b.height).unwrap_or(viewport_height);
        let (_content_w, content_h) = self
            .layout_tree
            .get_content_size(scroll_container)
            .unwrap_or((0.0, scroll_viewport_h));
        let max_offset_y = if content_h > scroll_viewport_h {
            -(content_h - scroll_viewport_h)
        } else {
            0.0
        };
        let clamped_y = target_y.clamp(max_offset_y, 0.0);

        if (clamped_y - current_y).abs() < 0.5 {
            // Effectively unchanged.
            return false;
        }

        tracing::debug!(
            "scroll_focused_text_input_above_keyboard: container={:?} \
             input_bottom={:.1} visible_bottom={:.1} obstruction={:.1} \
             current_y={:.1} -> {:.1}",
            scroll_container,
            input_bottom,
            visible_bottom,
            obstruction,
            current_y,
            clamped_y,
        );

        // Write through both the legacy `scroll_offsets` map AND the
        // physics state if it exists, so the next frame samples the
        // updated value via `get_scroll_offset` regardless of which
        // path is active.
        self.scroll_offsets
            .insert(scroll_container, (current_x, clamped_y));
        if let Some(physics) = self.scroll_physics.get(&scroll_container) {
            if let Ok(mut p) = physics.try_lock() {
                p.offset_x = current_x;
                p.offset_y = clamped_y;
                // Snap velocity to zero so the scroll doesn't keep
                // drifting after we set the offset programmatically.
                p.velocity_x = 0.0;
                p.velocity_y = 0.0;
            }
        }

        true
    }

    /// Get the scroll offset for a node
    ///
    /// Reads from scroll physics if available (has direction-aware bounds),
    /// falls back to legacy scroll_offsets.
    ///
    /// Note: Returns rounded values to prevent subpixel jitter during scrolling.
    /// Fractional scroll offsets cause content to shift between pixel boundaries,
    /// resulting in wobbling text and lines.
    /// Check if a node is a scroll container
    pub fn is_scroll_container(&self, node_id: LayoutNodeId) -> bool {
        self.scroll_physics.contains_key(&node_id)
    }

    pub fn get_scroll_offset(&self, node_id: LayoutNodeId) -> (f32, f32) {
        // Check scroll physics first (has direction-aware scroll from element)
        let (x, y) = if let Some(physics) = self.scroll_physics.get(&node_id) {
            if let Ok(p) = physics.try_lock() {
                (p.offset_x, p.offset_y)
            } else {
                self.scroll_offsets
                    .get(&node_id)
                    .copied()
                    .unwrap_or((0.0, 0.0))
            }
        } else {
            // Fallback to legacy scroll_offsets
            self.scroll_offsets
                .get(&node_id)
                .copied()
                .unwrap_or((0.0, 0.0))
        };

        // Round to whole pixels to prevent subpixel jitter
        (x.round(), y.round())
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

    /// Render scrollbar overlay for a scroll container
    fn render_scrollbar(
        &self,
        ctx: &mut dyn DrawContext,
        viewport_width: f32,
        viewport_height: f32,
        info: &crate::scroll::ScrollbarRenderInfo,
    ) {
        let config = &info.config;
        let scrollbar_width = config.width();
        let edge_padding = config.edge_padding;

        // Apply opacity to colors
        let opacity = info.opacity;
        let thumb_color = Color::rgba(
            config.thumb_color[0],
            config.thumb_color[1],
            config.thumb_color[2],
            config.thumb_color[3] * opacity,
        );
        let track_color = Color::rgba(
            config.track_color[0],
            config.track_color[1],
            config.track_color[2],
            config.track_color[3] * opacity,
        );

        // Calculate corner radius for thumb
        let thumb_radius = CornerRadius::uniform(scrollbar_width * config.corner_radius);

        // Render vertical scrollbar
        if info.show_vertical {
            // Track position (right edge)
            let track_x = viewport_width - scrollbar_width - edge_padding;
            let track_y = edge_padding;
            let track_height = viewport_height - edge_padding * 2.0;

            // Draw track
            let track_rect = Rect::new(track_x, track_y, scrollbar_width, track_height);
            ctx.fill_rect(track_rect, thumb_radius, Brush::Solid(track_color));

            // Draw thumb
            let thumb_rect = Rect::new(
                track_x,
                track_y + info.vertical_thumb_y - edge_padding,
                scrollbar_width,
                info.vertical_thumb_height,
            );
            ctx.fill_rect(thumb_rect, thumb_radius, Brush::Solid(thumb_color));
        }

        // Render horizontal scrollbar
        if info.show_horizontal {
            // Track position (bottom edge)
            let track_x = edge_padding;
            let track_y = viewport_height - scrollbar_width - edge_padding;
            let track_width = viewport_width - edge_padding * 2.0;

            // Adjust for vertical scrollbar if present
            let track_width = if info.show_vertical {
                track_width - scrollbar_width - edge_padding
            } else {
                track_width
            };

            // Draw track
            let track_rect = Rect::new(track_x, track_y, track_width, scrollbar_width);
            ctx.fill_rect(track_rect, thumb_radius, Brush::Solid(track_color));

            // Draw thumb
            let thumb_rect = Rect::new(
                track_x + info.horizontal_thumb_x - edge_padding,
                track_y,
                info.horizontal_thumb_width,
                scrollbar_width,
            );
            ctx.fill_rect(thumb_rect, thumb_radius, Brush::Solid(thumb_color));
        }
    }

    /// Get the scroll direction for a node (if it's a scroll container)
    ///
    /// Returns None if the node is not a scroll container.
    pub fn get_scroll_direction(
        &self,
        node_id: LayoutNodeId,
    ) -> Option<crate::scroll::ScrollDirection> {
        self.scroll_physics
            .get(&node_id)
            .and_then(|physics| physics.try_lock().ok().map(|p| p.config.direction))
    }

    /// Check if a scroll container can scroll in the given delta direction
    ///
    /// Returns true if the scroll container handles that axis.
    /// Used for nested scroll event handling.
    ///
    /// A scroll container consumes scroll for its direction(s) unless:
    /// - It has no scrollable content (content fits within viewport)
    /// - It's at an edge AND scrolling further into that edge AND bounce is disabled
    pub fn can_consume_scroll(
        &self,
        node_id: LayoutNodeId,
        delta_x: f32,
        delta_y: f32,
    ) -> (bool, bool) {
        let Some(physics) = self.scroll_physics.get(&node_id) else {
            return (false, false);
        };

        let Ok(p) = physics.try_lock() else {
            return (false, false);
        };

        let can_x = match p.config.direction {
            crate::scroll::ScrollDirection::Horizontal | crate::scroll::ScrollDirection::Both => {
                // Check if there's any scrollable content
                let scrollable_x = p.content_width - p.viewport_width;
                if scrollable_x <= 0.0 {
                    // No scrollable content - don't consume
                    false
                } else if delta_x.abs() < 0.001 {
                    // No horizontal delta to consume
                    false
                } else if delta_x < 0.0 {
                    // Scrolling left - can consume if not at left edge
                    // With bounce: only consume if we can still scroll OR are bouncing back
                    // Without bounce: only consume if not at edge
                    let at_left_edge = p.offset_x <= p.max_offset_x();
                    !at_left_edge || p.is_overscrolling_x()
                } else {
                    // Scrolling right - can consume if not at right edge
                    let at_right_edge = p.offset_x >= p.min_offset_x();
                    !at_right_edge || p.is_overscrolling_x()
                }
            }
            _ => false,
        };

        let can_y = match p.config.direction {
            crate::scroll::ScrollDirection::Vertical | crate::scroll::ScrollDirection::Both => {
                // Check if there's any scrollable content
                let scrollable_y = p.content_height - p.viewport_height;
                if scrollable_y <= 0.0 {
                    // No scrollable content - don't consume
                    false
                } else if delta_y.abs() < 0.001 {
                    // No vertical delta to consume
                    false
                } else if delta_y < 0.0 {
                    // Scrolling up (content moves down) - can consume if not at bottom edge
                    // With bounce: only consume if we can still scroll OR are bouncing back
                    // Without bounce: only consume if not at edge
                    let at_bottom_edge = p.offset_y <= p.max_offset_y();
                    !at_bottom_edge || p.is_overscrolling_y()
                } else {
                    // Scrolling down (content moves up) - can consume if not at top edge
                    let at_top_edge = p.offset_y >= p.min_offset_y();
                    !at_top_edge || p.is_overscrolling_y()
                }
            }
            _ => false,
        };

        (can_x, can_y)
    }

    /// Transfer scroll offsets from another tree (preserves scroll position across rebuilds)
    pub fn transfer_scroll_offsets_from(&mut self, other: &RenderTree) {
        for (node_id, offset) in &other.scroll_offsets {
            self.scroll_offsets.insert(*node_id, *offset);
        }
    }

    /// Transfer scroll physics from another tree (preserves scroll physics across rebuilds)
    pub fn transfer_scroll_physics_from(&mut self, other: &RenderTree) {
        for (node_id, physics) in &other.scroll_physics {
            self.scroll_physics.insert(*node_id, physics.clone());
        }
        for node_id in &other.viewport_cull_scrolls {
            self.viewport_cull_scrolls.insert(*node_id);
        }
    }

    /// Cancel any running scroll animation (momentum deceleration,
    /// bounce spring, rebound) on the first scrollable in the hit
    /// chain. Intended for the pointer-down / touch-down path so a tap
    /// on a coasting list halts it immediately, matching the native
    /// "grab-to-stop" affordance on every major toolkit.
    ///
    /// Walks leaf → root and cancels the first scroll container found
    /// that is actively animating. No-op if nothing under the cursor
    /// is animating.
    pub fn cancel_scroll_animation_in_chain(
        &mut self,
        hit_node: LayoutNodeId,
        ancestors: &[LayoutNodeId],
    ) {
        let mut chain: Vec<LayoutNodeId> = vec![hit_node];
        for &ancestor in ancestors.iter().rev() {
            if ancestor != hit_node {
                chain.push(ancestor);
            }
        }
        for node_id in chain {
            if let Some(physics) = self.scroll_physics.get(&node_id) {
                let mut p = physics.lock().unwrap();
                if p.is_animating() {
                    p.cancel_active_animation();
                    // Clear capture so the halted container doesn't
                    // keep absorbing subsequent scrolls as the "active"
                    // target after the tap cancelled its animation.
                    self.last_scroll_target = None;
                    return;
                }
            }
        }
    }

    /// Notify the most recently scrolled container that scrolling has
    /// ended.
    ///
    /// Fires only on the last scroll target (stored by `dispatch_scroll_chain`
    /// on each wheel/touch event) rather than every registered scroll
    /// physics in the tree. Broadcasting to all physics was the old
    /// behaviour and meant every scroll container in the app got its
    /// rebound spring kicked the instant the user released the mouse,
    /// which made untouched siblings / ancestors visibly spring from
    /// their offset — the "it springs on as soon as I release" bug.
    /// Clears the stored target after firing so subsequent gestures
    /// start fresh.
    pub fn on_scroll_end(&mut self) {
        let Some((node_id, _)) = self.last_scroll_target.take() else {
            return;
        };
        if let Some(physics) = self.scroll_physics.get(&node_id) {
            physics.lock().unwrap().on_scroll_end();
        }
    }

    /// Notify the most recently scrolled container that the scroll
    /// gesture has ended (finger lifted).
    ///
    /// Same target-scoped behaviour as [`Self::on_scroll_end`] — this
    /// used to iterate over every physics in the tree, which fired
    /// rebound springs on scrolls the user never touched.
    pub fn on_gesture_end(&mut self) {
        let Some((node_id, _)) = self.last_scroll_target.take() else {
            return;
        };
        if let Some(physics) = self.scroll_physics.get(&node_id) {
            physics.lock().unwrap().on_gesture_end();
        }
    }

    /// Returns `true` if any registered scroll physics is currently in
    /// the `Bouncing` state — i.e. an edge bounce-back spring is
    /// actively animating.
    ///
    /// Used by the web runner to absorb the macOS trackpad's ~800ms
    /// of OS-level momentum-scroll wheel events that arrive *after*
    /// a bounce has started: instead of letting them re-trigger
    /// `start_bounce` (which restarts the spring with a new initial
    /// position and produces a wobble), the runner drops the wheel
    /// event entirely while this returns true.
    pub fn has_bouncing_scroll(&self) -> bool {
        self.scroll_physics
            .values()
            .any(|p| p.lock().unwrap().state == crate::stateful::ScrollState::Bouncing)
    }

    /// Returns `true` if any registered scroll physics is currently
    /// past its scroll bounds (rubber-band overscroll).
    ///
    /// Used by the web runner to shorten the wheel-end debounce when
    /// the user is staring at a stuck rubber-band stretch — there is
    /// nothing more to scroll past the edge, so the bounce-back can
    /// fire after only a couple of frames of wheel-event silence
    /// instead of the full debounce window the runner uses to absorb
    /// gaps between adjacent in-bounds wheel events.
    pub fn has_overscrolling_scroll(&self) -> bool {
        self.scroll_physics
            .values()
            .any(|p| p.lock().unwrap().is_overscrolling())
    }

    /// Tick all scroll physics and return true if any are animating
    ///
    /// Call this each frame with the current time in milliseconds.
    /// Uses actual time delta for smooth, frame-rate independent animation.
    pub fn tick_scroll_physics(&mut self, current_time_ms: u64) -> bool {
        // Calculate actual delta time
        let dt_secs = if let Some(last_time) = self.last_scroll_tick_ms {
            (current_time_ms.saturating_sub(last_time)) as f32 / 1000.0
        } else {
            1.0 / 60.0 // Assume ~60fps for first frame
        };
        self.last_scroll_tick_ms = Some(current_time_ms);

        // Clamp dt to prevent huge jumps if app was paused
        let dt_secs = dt_secs.min(0.1);

        // Collect node_ids to iterate (avoid borrow conflicts)
        let node_ids: Vec<_> = self.scroll_physics.keys().copied().collect();

        let mut any_animating = false;
        for node_id in node_ids {
            let Some(physics_arc) = self.scroll_physics.get(&node_id) else {
                continue;
            };

            let mut physics = physics_arc.lock().unwrap();

            // Detect "scroll ended" for inputs without an explicit end phase
            // (mouse wheel, Windows/Linux trackpad drivers). If the user has
            // been idle past the threshold while overscrolled, synthesise an
            // `on_scroll_end` so the spring rebounds.
            physics.check_idle_bounce(current_time_ms as f64);

            // Tick the physics
            if physics.tick(dt_secs) {
                any_animating = true;
            }

            // Tick scrollbar animations (opacity fade in/out)
            if physics.tick_scrollbar(dt_secs) {
                any_animating = true;
            }

            // Sync ScrollRef state with current physics (for scrollbar position updates)
            if let Some(scroll_ref) = self.scroll_refs.get(&node_id) {
                scroll_ref.update_state(
                    (physics.offset_x.abs(), physics.offset_y.abs()),
                    (physics.content_width, physics.content_height),
                    (physics.viewport_width, physics.viewport_height),
                );
            }
        }

        any_animating
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
    }

    /// Set a shared stylesheet reference
    pub fn set_stylesheet_arc(&mut self, stylesheet: Arc<Stylesheet>) {
        // Also update the global stylesheet for form widget CSS override resolution
        crate::css_parser::set_active_stylesheet(Arc::clone(&stylesheet));
        self.stylesheet = Some(stylesheet);
    }

    /// Get the current stylesheet, if any
    pub fn stylesheet(&self) -> Option<&Stylesheet> {
        self.stylesheet.as_ref().map(|s| s.as_ref())
    }

    /// Apply state-specific styles from the stylesheet to a node
    ///
    /// This is called when a node's interaction state changes (hover, pressed, focused).
    /// It looks up the node's string ID and applies any matching state styles.
    ///
    /// # Arguments
    /// * `node_id` - The node whose state changed
    /// * `hovered` - Whether the node is currently hovered
    /// * `pressed` - Whether the node is currently pressed
    /// * `focused` - Whether the node is currently focused
    ///
    /// # Returns
    /// `true` if styles were applied, `false` if no stylesheet or no matching styles
    pub fn apply_state_styles(
        &mut self,
        node_id: LayoutNodeId,
        hovered: bool,
        pressed: bool,
        focused: bool,
    ) -> bool {
        // Early return if no stylesheet
        let stylesheet = match &self.stylesheet {
            Some(s) => s.clone(),
            None => return false,
        };

        // Look up the node's string ID from the registry
        let element_id = match self.element_registry.get_id(node_id) {
            Some(id) => id,
            None => return false, // Node has no ID, can't apply stylesheet styles
        };

        // Get or store base style for this node
        if !self.base_styles.contains_key(&node_id) {
            if let Some(render_node) = self.render_nodes.get(&node_id) {
                self.base_styles.insert(node_id, render_node.props.clone());
            }
        }
        // Get or store base taffy style for layout transition support
        if !self.base_taffy_styles.contains_key(&node_id) {
            if let Some(style) = self.layout_tree.get_style(node_id) {
                self.base_taffy_styles.insert(node_id, style);
            }
        }

        // Start with base style
        let base_props = match self.base_styles.get(&node_id) {
            Some(props) => props.clone(),
            None => return false,
        };

        // Check if this element has transitions defined
        let transition_set = stylesheet
            .get(&element_id)
            .and_then(|s| s.transition.clone());

        // Snapshot before-props for transition detection (visual + layout).
        // Uses snapshot_before_keyframe_properties to avoid QR decomposition
        // drift on transform fields when an active transition exists.
        let before_kp = if transition_set.is_some() {
            self.snapshot_before_keyframe_properties(node_id)
        } else {
            None
        };

        // Apply state-specific styles in order of precedence
        let mut applied = false;
        let render_node = match self.render_nodes.get_mut(&node_id) {
            Some(node) => node,
            None => return false,
        };

        // Reset to base style first
        render_node.props = base_props;

        // Reset taffy style to base before applying state layout overrides
        if let Some(base_taffy) = self.base_taffy_styles.get(&node_id) {
            self.layout_tree.set_style(node_id, base_taffy.clone());
        }

        // Apply base stylesheet style (if any)
        if let Some(base_style) = stylesheet.get(&element_id) {
            Self::apply_element_style_to_props(&mut render_node.props, base_style);
            if base_style.has_layout_props() {
                if let Some(mut taffy_style) = self.layout_tree.get_style(node_id) {
                    Self::apply_element_style_to_taffy(&mut taffy_style, base_style);
                    self.layout_tree.set_style(node_id, taffy_style);
                }
            }
            applied = true;
        }

        // Apply hover style
        if hovered {
            if let Some(hover_style) = stylesheet.get_with_state(&element_id, ElementState::Hover) {
                Self::apply_element_style_to_props(&mut render_node.props, hover_style);
                if hover_style.has_layout_props() {
                    if let Some(mut taffy_style) = self.layout_tree.get_style(node_id) {
                        Self::apply_element_style_to_taffy(&mut taffy_style, hover_style);
                        self.layout_tree.set_style(node_id, taffy_style);
                    }
                }
                applied = true;
            }
        }

        // Apply active/pressed style (takes precedence over hover)
        if pressed {
            if let Some(active_style) = stylesheet.get_with_state(&element_id, ElementState::Active)
            {
                Self::apply_element_style_to_props(&mut render_node.props, active_style);
                if active_style.has_layout_props() {
                    if let Some(mut taffy_style) = self.layout_tree.get_style(node_id) {
                        Self::apply_element_style_to_taffy(&mut taffy_style, active_style);
                        self.layout_tree.set_style(node_id, taffy_style);
                    }
                }
                applied = true;
            }
        }

        // Apply focus style
        if focused {
            if let Some(focus_style) = stylesheet.get_with_state(&element_id, ElementState::Focus) {
                Self::apply_element_style_to_props(&mut render_node.props, focus_style);
                if focus_style.has_layout_props() {
                    if let Some(mut taffy_style) = self.layout_tree.get_style(node_id) {
                        Self::apply_element_style_to_taffy(&mut taffy_style, focus_style);
                        self.layout_tree.set_style(node_id, taffy_style);
                    }
                }
                applied = true;
            }
        }

        // Detect and start transitions for changed properties (visual + layout)
        if let (Some(before_kp), Some(transition_set)) = (before_kp, transition_set) {
            if let Some(after_kp) = self.snapshot_keyframe_properties(node_id) {
                self.detect_and_start_transitions(node_id, &before_kp, &after_kp, &transition_set);
            }
        }

        applied
    }

    /// Inherit CSS text properties from the parent RenderNode.
    ///
    /// CSS text properties like text-decoration, white-space, text-overflow, etc.
    /// need to cascade from parent divs to child text elements. Without this,
    /// CSS like `.my-class { text-decoration: underline; }` on a parent div
    /// wouldn't affect the child text node.
    fn inherit_text_props_from_parent(&self, props: &mut RenderProps, node_id: LayoutNodeId) {
        let parent_id = match self.element_registry.get_parent(node_id) {
            Some(id) => id,
            None => return,
        };
        let parent_props = match self.render_nodes.get(&parent_id) {
            Some(node) => &node.props,
            None => return,
        };

        // text-decoration (CSS spec: not inherited, but decorations paint across inline content)
        if props.text_decoration.is_none() {
            if let Some(td) = parent_props.text_decoration {
                props.text_decoration = Some(td);
            }
        }
        if props.text_decoration_color.is_none() {
            if let Some(c) = parent_props.text_decoration_color {
                props.text_decoration_color = Some(c);
            }
        }
        if props.text_decoration_thickness.is_none() {
            if let Some(t) = parent_props.text_decoration_thickness {
                props.text_decoration_thickness = Some(t);
            }
        }
        // white-space (CSS spec: inherited)
        if props.white_space.is_none() {
            if let Some(ws) = parent_props.white_space {
                props.white_space = Some(ws);
            }
        }
        // text-overflow (CSS spec: not inherited, but child text must know)
        if props.text_overflow.is_none() {
            if let Some(to) = parent_props.text_overflow {
                props.text_overflow = Some(to);
            }
        }
        // color (CSS spec: inherited)
        if props.text_color.is_none() {
            if let Some(c) = parent_props.text_color {
                props.text_color = Some(c);
            }
        }
        // text-align (CSS spec: inherited)
        if props.text_align.is_none() {
            if let Some(ta) = parent_props.text_align {
                props.text_align = Some(ta);
            }
        }
        // SVG fill (CSS spec: inherited in SVG)
        if props.fill.is_none() {
            if let Some(f) = parent_props.fill {
                props.fill = Some(f);
            }
        }
        // SVG stroke (CSS spec: inherited in SVG)
        if props.stroke.is_none() {
            if let Some(s) = parent_props.stroke {
                props.stroke = Some(s);
            }
        }
        // SVG stroke-width (CSS spec: inherited in SVG)
        if props.stroke_width.is_none() {
            if let Some(sw) = parent_props.stroke_width {
                props.stroke_width = Some(sw);
            }
        }
    }

    /// Build TextData from TextRenderInfo, applying CSS overrides from RenderProps
    fn build_text_data(info: crate::div::TextRenderInfo, props: &RenderProps) -> TextData {
        let mut strikethrough = info.strikethrough;
        let mut underline = info.underline;
        let mut wrap = info.wrap;
        // CSS text-decoration overrides builder values
        if let Some(td) = props.text_decoration {
            use crate::element_style::TextDecoration;
            match td {
                TextDecoration::Underline => underline = true,
                TextDecoration::LineThrough => strikethrough = true,
                TextDecoration::None => {
                    underline = false;
                    strikethrough = false;
                }
            }
        }
        // CSS white-space overrides wrap
        if let Some(ws) = props.white_space {
            use crate::element_style::WhiteSpace;
            match ws {
                WhiteSpace::Nowrap | WhiteSpace::Pre => wrap = false,
                WhiteSpace::Normal | WhiteSpace::PreWrap => wrap = true,
            }
        }
        // CSS text-align overrides builder value
        let align = props.text_align.unwrap_or(info.align);
        TextData {
            content: info.content,
            font_size: info.font_size,
            color: info.color,
            align,
            weight: info.weight,
            italic: info.italic,
            v_align: info.v_align,
            wrap,
            line_height: info.line_height,
            measured_width: info.measured_width,
            font_family: info.font_family,
            word_spacing: info.word_spacing,
            letter_spacing: info.letter_spacing,
            ascender: info.ascender,
            strikethrough,
            underline,
        }
    }

    /// Apply ElementStyle properties to RenderProps
    fn apply_element_style_to_props(
        props: &mut RenderProps,
        style: &crate::element_style::ElementStyle,
    ) {
        if let Some(ref bg) = style.background {
            props.background = Some(bg.clone());
        }
        if let Some(ref cr) = style.corner_radius {
            props.border_radius = *cr;
            props.border_radius_explicit = true;
        }
        if let Some(cs) = style.corner_shape {
            props.corner_shape = cs;
        }
        if let Some(fade) = style.overflow_fade {
            props.overflow_fade = fade;
        }
        if let Some(shadow) = style.shadow {
            props.shadow = Some(shadow);
        }
        if let Some(ref transform) = style.transform {
            props.transform = Some(transform.clone());
        }
        if let Some(opacity) = style.opacity {
            props.opacity = opacity;
        }
        if let Some(ref render_layer) = style.render_layer {
            props.layer = *render_layer;
        }
        if let Some(ref material) = style.material {
            props.material = Some(material.clone());
        }
        // 3D transform properties
        if let Some(rx) = style.rotate_x {
            props.rotate_x = Some(rx);
        }
        if let Some(ry) = style.rotate_y {
            props.rotate_y = Some(ry);
        }
        if let Some(p) = style.perspective {
            props.perspective = Some(p);
        }
        if let Some(ref s) = style.shape_3d {
            props.shape_3d = Some(crate::css_parser::shape_3d_to_float(s));
        }
        if let Some(d) = style.depth {
            props.depth = Some(d);
        }
        if let Some(dir) = style.light_direction {
            props.light_direction = Some(dir);
        }
        if let Some(v) = style.light_intensity {
            props.light_intensity = Some(v);
        }
        if let Some(v) = style.ambient {
            props.ambient = Some(v);
        }
        if let Some(v) = style.specular {
            props.specular = Some(v);
        }
        if let Some(v) = style.translate_z {
            props.translate_z = Some(v);
        }
        if let Some(ref op) = style.op_3d {
            props.op_3d = Some(crate::css_parser::op_3d_to_float(op));
        }
        if let Some(v) = style.blend_3d {
            props.blend_3d = Some(v);
        }
        if let Some(ref cp) = style.clip_path {
            props.clip_path = Some(cp.clone());
        }
        if let Some(filter) = style.filter {
            props.filter = Some(filter);
            // Convert blur filter to LayerEffect for GPU processing
            if filter.blur > 0.0 {
                // Remove any existing blur effect before adding updated one
                props
                    .layer_effects
                    .retain(|e| !matches!(e, LayerEffect::Blur { .. }));
                props.layer_effects.push(LayerEffect::Blur {
                    radius: filter.blur,
                    quality: BlurQuality::Medium,
                });
            } else {
                props
                    .layer_effects
                    .retain(|e| !matches!(e, LayerEffect::Blur { .. }));
            }
            // Convert drop-shadow filter to LayerEffect
            if let Some(ds) = &filter.drop_shadow {
                props
                    .layer_effects
                    .retain(|e| !matches!(e, LayerEffect::DropShadow { .. }));
                props.layer_effects.push(LayerEffect::DropShadow {
                    offset_x: ds.offset_x,
                    offset_y: ds.offset_y,
                    blur: ds.blur,
                    spread: ds.spread,
                    color: ds.color,
                });
            } else {
                props
                    .layer_effects
                    .retain(|e| !matches!(e, LayerEffect::DropShadow { .. }));
            }
        }
        // Mask image → LayerEffect (URL only; gradients handled per-primitive)
        if let Some(blinc_core::MaskImage::Url(ref url)) = style.mask_image {
            props
                .layer_effects
                .retain(|e| !matches!(e, LayerEffect::MaskImage { .. }));
            props.layer_effects.push(LayerEffect::MaskImage {
                image_url: url.clone(),
                mask_mode: style.mask_mode.clone().unwrap_or_default(),
            });
        }
        // Text color
        if let Some(c) = &style.text_color {
            props.text_color = Some([c.r, c.g, c.b, c.a]);
        }
        // Text shadow
        if let Some(ts) = &style.text_shadow {
            props.text_shadow = Some(*ts);
        }
        // Font size
        if let Some(fs) = style.font_size {
            props.font_size = Some(fs);
        }
        // Font weight
        if let Some(fw) = style.font_weight {
            props.font_weight = Some(fw);
        }
        // Text decoration
        if let Some(td) = style.text_decoration {
            props.text_decoration = Some(td);
        }
        // Line height
        if let Some(lh) = style.line_height {
            props.line_height = Some(lh);
        }
        // Text align
        if let Some(ta) = style.text_align {
            props.text_align = Some(ta);
        }
        // Letter spacing
        if let Some(ls) = style.letter_spacing {
            props.letter_spacing = Some(ls);
        }
        // SVG properties
        if let Some(fill) = style.fill {
            props.fill = Some([fill.r, fill.g, fill.b, fill.a]);
        }
        if let Some(stroke) = style.stroke {
            props.stroke = Some([stroke.r, stroke.g, stroke.b, stroke.a]);
        }
        if let Some(sw) = style.stroke_width {
            props.stroke_width = Some(sw);
        }
        if let Some(ref da) = style.stroke_dasharray {
            props.stroke_dasharray = Some(da.clone());
        }
        if let Some(offset) = style.stroke_dashoffset {
            props.stroke_dashoffset = Some(offset);
        }
        if let Some(ref path_data) = style.svg_path_data {
            props.svg_path_data = Some(path_data.clone());
        }
        // Transform origin
        if let Some(to) = style.transform_origin {
            props.transform_origin = Some(to);
        }
        // Border
        if let Some(bw) = style.border_width {
            props.border_width = bw;
        }
        if let Some(bc) = style.border_color {
            props.border_color = Some(bc);
        }
        // Outline
        if let Some(ow) = style.outline_width {
            props.outline_width = ow;
        }
        if let Some(ref oc) = style.outline_color {
            props.outline_color = Some(*oc);
        }
        if let Some(offset) = style.outline_offset {
            props.outline_offset = offset;
        }
        // Skew (composed into existing transform as Affine2D)
        if style.skew_x.is_some() || style.skew_y.is_some() {
            use blinc_core::Affine2D;
            let mut skew_affine = Affine2D::IDENTITY;
            if let Some(sx) = style.skew_x {
                skew_affine = skew_affine.then(&Affine2D::skew_x(sx.to_radians()));
            }
            if let Some(sy) = style.skew_y {
                skew_affine = skew_affine.then(&Affine2D::skew_y(sy.to_radians()));
            }
            // Compose with existing transform or set new
            if let Some(blinc_core::Transform::Affine2D(existing)) = &props.transform {
                props.transform =
                    Some(blinc_core::Transform::Affine2D(existing.then(&skew_affine)));
            } else {
                props.transform = Some(blinc_core::Transform::Affine2D(skew_affine));
            }
        }
        // Position flags (fixed/sticky are rendering concerns, not layout)
        if let Some(pos) = style.position {
            use crate::element_style::StylePosition;
            match pos {
                StylePosition::Fixed => {
                    props.is_fixed = true;
                    props.is_sticky = false;
                }
                StylePosition::Sticky => {
                    props.is_fixed = false;
                    props.is_sticky = true;
                    props.sticky_top = style.top;
                    props.sticky_bottom = style.bottom;
                }
                _ => {
                    props.is_fixed = false;
                    props.is_sticky = false;
                }
            }
        }
        // Numeric z-index for stacking order
        if let Some(z) = style.z_index {
            props.z_index = z;
        }
        // Overflow → clips_content (Clip or Scroll means clip children)
        if let Some(overflow) = style.overflow {
            use crate::element_style::StyleOverflow;
            if matches!(overflow, StyleOverflow::Clip | StyleOverflow::Scroll) {
                props.clips_content = true;
            }
        }
        if let Some(ox) = style.overflow_x {
            use crate::element_style::StyleOverflow;
            if matches!(ox, StyleOverflow::Clip | StyleOverflow::Scroll) {
                props.clips_content = true;
            }
        }
        if let Some(oy) = style.overflow_y {
            use crate::element_style::StyleOverflow;
            if matches!(oy, StyleOverflow::Clip | StyleOverflow::Scroll) {
                props.clips_content = true;
            }
        }
        // Visibility
        if let Some(vis) = style.visibility {
            use crate::element_style::StyleVisibility;
            props.visible = matches!(vis, StyleVisibility::Visible);
        }
        // Image properties
        if let Some(of) = style.object_fit {
            props.object_fit = Some(of);
        }
        if let Some(op) = style.object_position {
            props.object_position = Some(op);
        }
        if let Some(ls) = style.loading_strategy {
            props.loading_strategy = Some(ls);
        }
        if let Some(pt) = style.image_placeholder_type {
            props.placeholder_type = Some(pt);
        }
        if let Some(pc) = style.image_placeholder_color {
            props.placeholder_color = Some(pc);
        }
        if let Some(ref pi) = style.image_placeholder_image {
            props.placeholder_image = Some(pi.clone());
        }
        if let Some(fd) = style.fade_duration_ms {
            props.fade_duration_ms = Some(fd);
        }
        // Pointer events
        if let Some(pe) = style.pointer_events {
            props.pointer_events_none = matches!(pe, blinc_core::PointerEvents::None);
        }
        // Cursor
        if let Some(cursor) = style.cursor {
            props.cursor = Some(cursor);
        }
        // Mix blend mode
        if let Some(mode) = style.mix_blend_mode {
            props.mix_blend_mode = Some(mode);
        }
        // Text decoration enhancements
        if let Some(c) = style.text_decoration_color {
            props.text_decoration_color = Some([c.r, c.g, c.b, c.a]);
        }
        if let Some(t) = style.text_decoration_thickness {
            props.text_decoration_thickness = Some(t);
        }
        // Text overflow
        if let Some(to) = style.text_overflow {
            props.text_overflow = Some(to);
        }
        if let Some(ws) = style.white_space {
            props.white_space = Some(ws);
        }
        if let Some(ref mask) = style.mask_image {
            props.mask_image = Some(mask.clone());
        }
        if let Some(ref mode) = style.mask_mode {
            props.mask_mode = Some(mode.clone());
        }
        // @flow shader
        if let Some(ref flow_name) = style.flow {
            props.flow = Some(flow_name.clone());
        }
    }

    /// Apply layout properties from an ElementStyle to a taffy Style.
    ///
    /// This mirrors `apply_element_style_to_props` but for layout properties
    /// that live in the taffy layout system rather than RenderProps.
    fn apply_element_style_to_taffy(
        taffy_style: &mut taffy::Style,
        es: &crate::element_style::ElementStyle,
    ) {
        use taffy::prelude::*;

        if let Some(w) = es.width {
            taffy_style.size.width = match w {
                crate::element_style::StyleDimension::Length(px) => Dimension::Length(px),
                crate::element_style::StyleDimension::Percent(p) => Dimension::Percent(p),
                crate::element_style::StyleDimension::Auto => Dimension::Auto,
            };
            if matches!(w, crate::element_style::StyleDimension::Auto) {
                taffy_style.flex_basis = Dimension::Auto;
                taffy_style.flex_grow = 0.0;
                taffy_style.flex_shrink = 0.0;
            }
        }
        if let Some(h) = es.height {
            taffy_style.size.height = match h {
                crate::element_style::StyleDimension::Length(px) => Dimension::Length(px),
                crate::element_style::StyleDimension::Percent(p) => Dimension::Percent(p),
                crate::element_style::StyleDimension::Auto => Dimension::Auto,
            };
            if matches!(h, crate::element_style::StyleDimension::Auto) {
                taffy_style.flex_basis = Dimension::Auto;
                taffy_style.flex_grow = 0.0;
                taffy_style.flex_shrink = 0.0;
            }
        }
        if let Some(v) = es.min_width {
            taffy_style.min_size.width = Dimension::Length(v);
        }
        if let Some(v) = es.max_width {
            taffy_style.max_size.width = Dimension::Length(v);
        }
        if let Some(v) = es.min_height {
            taffy_style.min_size.height = Dimension::Length(v);
        }
        if let Some(v) = es.max_height {
            taffy_style.max_size.height = Dimension::Length(v);
        }
        if let Some(ref p) = es.padding {
            taffy_style.padding = taffy::geometry::Rect {
                top: LengthPercentage::Length(p.top),
                right: LengthPercentage::Length(p.right),
                bottom: LengthPercentage::Length(p.bottom),
                left: LengthPercentage::Length(p.left),
            };
        }
        if let Some(ref m) = es.margin {
            taffy_style.margin = taffy::geometry::Rect {
                top: LengthPercentageAuto::Length(m.top),
                right: LengthPercentageAuto::Length(m.right),
                bottom: LengthPercentageAuto::Length(m.bottom),
                left: LengthPercentageAuto::Length(m.left),
            };
        }
        if let Some(g) = es.gap {
            taffy_style.gap = taffy::geometry::Size {
                width: LengthPercentage::Length(g),
                height: LengthPercentage::Length(g),
            };
        }
        if let Some(v) = es.flex_grow {
            taffy_style.flex_grow = v;
        }
        if let Some(v) = es.flex_shrink {
            taffy_style.flex_shrink = v;
        }
        if let Some(v) = es.top {
            taffy_style.inset.top = LengthPercentageAuto::Length(v);
        }
        if let Some(v) = es.right {
            taffy_style.inset.right = LengthPercentageAuto::Length(v);
        }
        if let Some(v) = es.bottom {
            taffy_style.inset.bottom = LengthPercentageAuto::Length(v);
        }
        if let Some(v) = es.left {
            taffy_style.inset.left = LengthPercentageAuto::Length(v);
        }
        // visibility: hidden collapses element from layout (Blinc is always flex)
        if let Some(vis) = es.visibility {
            use crate::element_style::StyleVisibility;
            match vis {
                StyleVisibility::Hidden => taffy_style.display = Display::None,
                StyleVisibility::Visible => {
                    if taffy_style.display == Display::None {
                        taffy_style.display = Display::Flex;
                    }
                }
            }
        }
    }

    /// Snapshot all animatable properties (visual + layout) for a node.
    ///
    /// Combines visual properties from RenderProps with layout properties
    /// from the taffy Style to create a complete KeyframeProperties snapshot
    /// suitable for transition detection.
    fn snapshot_keyframe_properties(
        &self,
        node_id: LayoutNodeId,
    ) -> Option<blinc_animation::KeyframeProperties> {
        let render_node = self.render_nodes.get(&node_id)?;
        let mut kp = Self::render_props_to_keyframe_properties(&render_node.props);

        // Also extract layout properties from taffy style
        if let Some(style) = self.layout_tree.get_style(node_id) {
            if let taffy::Dimension::Length(w) = style.size.width {
                kp.width = Some(w);
            }
            if let taffy::Dimension::Length(h) = style.size.height {
                kp.height = Some(h);
            }
            // Extract padding
            let pt = Self::taffy_lp_to_f32(&style.padding.top);
            let pr = Self::taffy_lp_to_f32(&style.padding.right);
            let pb = Self::taffy_lp_to_f32(&style.padding.bottom);
            let pl = Self::taffy_lp_to_f32(&style.padding.left);
            if pt.is_some() || pr.is_some() || pb.is_some() || pl.is_some() {
                kp.padding = Some([
                    pt.unwrap_or(0.0),
                    pr.unwrap_or(0.0),
                    pb.unwrap_or(0.0),
                    pl.unwrap_or(0.0),
                ]);
            }
            // Extract margin
            let mt = Self::taffy_lpa_to_f32(&style.margin.top);
            let mr = Self::taffy_lpa_to_f32(&style.margin.right);
            let mb = Self::taffy_lpa_to_f32(&style.margin.bottom);
            let ml = Self::taffy_lpa_to_f32(&style.margin.left);
            if mt.is_some() || mr.is_some() || mb.is_some() || ml.is_some() {
                kp.margin = Some([
                    mt.unwrap_or(0.0),
                    mr.unwrap_or(0.0),
                    mb.unwrap_or(0.0),
                    ml.unwrap_or(0.0),
                ]);
            }
            // Extract gap
            if let taffy::LengthPercentage::Length(g) = style.gap.width {
                kp.gap = Some(g);
            }
            // Extract min/max constraints
            if let taffy::Dimension::Length(v) = style.min_size.width {
                kp.min_width = Some(v);
            }
            if let taffy::Dimension::Length(v) = style.max_size.width {
                kp.max_width = Some(v);
            }
            if let taffy::Dimension::Length(v) = style.min_size.height {
                kp.min_height = Some(v);
            }
            if let taffy::Dimension::Length(v) = style.max_size.height {
                kp.max_height = Some(v);
            }
            // Extract flex grow/shrink
            if style.flex_grow != 0.0 {
                kp.flex_grow = Some(style.flex_grow);
            }
            if style.flex_shrink != 1.0 {
                kp.flex_shrink = Some(style.flex_shrink);
            }
            // Extract inset (top/right/bottom/left)
            if let Some(v) = Self::taffy_lpa_to_f32(&style.inset.top) {
                kp.inset_top = Some(v);
            }
            if let Some(v) = Self::taffy_lpa_to_f32(&style.inset.right) {
                kp.inset_right = Some(v);
            }
            if let Some(v) = Self::taffy_lpa_to_f32(&style.inset.bottom) {
                kp.inset_bottom = Some(v);
            }
            if let Some(v) = Self::taffy_lpa_to_f32(&style.inset.left) {
                kp.inset_left = Some(v);
            }
        }

        Some(kp)
    }

    /// Snapshot keyframe properties for transition "before" state.
    ///
    /// When an active transition exists, overlays the transition's current
    /// interpolated values for transform fields. This avoids QR decomposition
    /// drift: the affine→decompose roundtrip introduces tiny floating-point
    /// errors that would cause spurious property changes and defeat the
    /// same-target guard in `detect_and_start_transitions`.
    fn snapshot_before_keyframe_properties(
        &self,
        node_id: LayoutNodeId,
    ) -> Option<blinc_animation::KeyframeProperties> {
        self.snapshot_keyframe_properties(node_id).map(|mut kp| {
            let store = self.css_anim_store.lock().unwrap();
            if let Some(active) = store.transitions.get(&node_id) {
                let cp = &active.current_properties;
                if cp.rotate.is_some() {
                    kp.rotate = cp.rotate;
                }
                if cp.scale_x.is_some() {
                    kp.scale_x = cp.scale_x;
                }
                if cp.scale_y.is_some() {
                    kp.scale_y = cp.scale_y;
                }
                if cp.translate_x.is_some() {
                    kp.translate_x = cp.translate_x;
                }
                if cp.translate_y.is_some() {
                    kp.translate_y = cp.translate_y;
                }
                if cp.skew_x.is_some() {
                    kp.skew_x = cp.skew_x;
                }
                if cp.skew_y.is_some() {
                    kp.skew_y = cp.skew_y;
                }
                if cp.transform_origin.is_some() {
                    kp.transform_origin = cp.transform_origin;
                }
                // Overlay gradient fields from active transition to avoid
                // snapshot mismatch (render props may lag 1 frame behind)
                if cp.gradient_start_color.is_some() {
                    kp.gradient_start_color = cp.gradient_start_color;
                }
                if cp.gradient_end_color.is_some() {
                    kp.gradient_end_color = cp.gradient_end_color;
                }
                if cp.gradient_angle.is_some() {
                    kp.gradient_angle = cp.gradient_angle;
                }
                if cp.background_color.is_some() {
                    kp.background_color = cp.background_color;
                }
                // Mask gradient
                if cp.mask_gradient.is_some() {
                    kp.mask_gradient = cp.mask_gradient;
                }
            }
            kp
        })
    }

    fn taffy_lp_to_f32(lp: &taffy::LengthPercentage) -> Option<f32> {
        match lp {
            taffy::LengthPercentage::Length(v) => Some(*v),
            _ => None,
        }
    }

    fn taffy_lpa_to_f32(lpa: &taffy::LengthPercentageAuto) -> Option<f32> {
        match lpa {
            taffy::LengthPercentageAuto::Length(v) => Some(*v),
            _ => None,
        }
    }

    /// Resolve a CSS `ClipPath` into a concrete `ClipShape` given element bounds.
    ///
    /// For simple shapes (circle, ellipse, inset, rect, xywh), this produces a
    /// ClipShape that the existing GPU clip infrastructure handles directly.
    /// For polygon and path, returns a polygon-based ClipShape (handled in Phase 6).
    fn resolve_clip_path(
        clip_path: &blinc_core::ClipPath,
        bounds: &crate::element::ElementBounds,
    ) -> Option<ClipShape> {
        use blinc_core::{ClipLength, ClipPath};

        let w = bounds.width;
        let h = bounds.height;

        match clip_path {
            ClipPath::Circle { radius, center } => {
                let cx = center.0.resolve(w);
                let cy = center.1.resolve(h);
                let r = radius
                    .map(|r| r.resolve(w.min(h)))
                    .unwrap_or_else(|| w.min(h) * 0.5);
                Some(ClipShape::circle(Point::new(cx, cy), r))
            }
            ClipPath::Ellipse { rx, ry, center } => {
                let cx = center.0.resolve(w);
                let cy = center.1.resolve(h);
                let rx_val = rx.map(|r| r.resolve(w)).unwrap_or(w * 0.5);
                let ry_val = ry.map(|r| r.resolve(h)).unwrap_or(h * 0.5);
                Some(ClipShape::ellipse(
                    Point::new(cx, cy),
                    Vec2::new(rx_val, ry_val),
                ))
            }
            ClipPath::Inset {
                top,
                right,
                bottom,
                left,
                round,
            } => {
                let t = top.resolve(h);
                let r = right.resolve(w);
                let b = bottom.resolve(h);
                let l = left.resolve(w);
                let rect = Rect::new(l, t, w - l - r, h - t - b);
                if let Some(radius) = round {
                    Some(ClipShape::rounded_rect(rect, *radius))
                } else {
                    Some(ClipShape::rect(rect))
                }
            }
            ClipPath::Rect {
                top,
                right,
                bottom,
                left,
                round,
            } => {
                // CSS rect() uses absolute edge positions
                let t = top.resolve(h);
                let r = right.resolve(w);
                let b = bottom.resolve(h);
                let l = left.resolve(w);
                let rect = Rect::new(l, t, r - l, b - t);
                if let Some(radius) = round {
                    Some(ClipShape::rounded_rect(rect, *radius))
                } else {
                    Some(ClipShape::rect(rect))
                }
            }
            ClipPath::Xywh {
                x,
                y,
                w: cw,
                h: ch,
                round,
            } => {
                let rx = x.resolve(w);
                let ry = y.resolve(h);
                let rw = cw.resolve(w);
                let rh = ch.resolve(h);
                let rect = Rect::new(rx, ry, rw, rh);
                if let Some(radius) = round {
                    Some(ClipShape::rounded_rect(rect, *radius))
                } else {
                    Some(ClipShape::rect(rect))
                }
            }
            ClipPath::Polygon { points } => {
                let resolved: Vec<Point> = points
                    .iter()
                    .map(|(px, py)| Point::new(px.resolve(w), py.resolve(h)))
                    .collect();
                if resolved.len() < 3 {
                    return None;
                }
                Some(ClipShape::Polygon(resolved))
            }
            ClipPath::Path { vertices } => {
                if vertices.len() < 3 {
                    return None;
                }
                let points: Vec<Point> = vertices.iter().map(|(x, y)| Point::new(*x, *y)).collect();
                Some(ClipShape::Polygon(points))
            }
        }
    }

    /// Apply layout properties from the stylesheet to taffy nodes
    ///
    /// This must be called after tree build and before compute_layout().
    /// It iterates all registered element IDs and applies layout overrides
    /// (width, height, padding, margin, gap, flex-direction, alignment, etc.)
    /// from the stylesheet to the corresponding taffy nodes.
    pub fn apply_stylesheet_layout_overrides(&mut self) {
        use crate::element_style::{
            SpacingRect, StyleAlign, StyleDisplay, StyleFlexDirection, StyleJustify, StyleOverflow,
            StylePosition,
        };

        let stylesheet = match &self.stylesheet {
            Some(s) => s.clone(),
            None => return,
        };

        // Collect (node_id, style) pairs that have layout overrides.
        // Apply complex rules (class/type selectors) FIRST — lower specificity.
        // Then apply simple ID rules LAST — higher specificity overrides.
        let mut overrides: Vec<(LayoutNodeId, crate::element_style::ElementStyle)> = Vec::new();

        // Complex rules: class, type, and combinator selectors
        let complex_rules = stylesheet.complex_rules();
        if !complex_rules.is_empty() {
            let empty_set = std::collections::HashSet::new();

            let mut base_rules: Vec<&(
                crate::css_parser::ComplexSelector,
                crate::element_style::ElementStyle,
            )> = complex_rules
                .iter()
                .filter(|(selector, _)| !selector.has_state())
                .collect();
            base_rules.sort_by_key(|(selector, _)| Self::selector_specificity(selector));

            // Build inverted class index once for O(1) lookups
            let class_to_nodes = self.element_registry.class_to_nodes_index();

            for (selector, style) in base_rules {
                if !style.has_layout_props() {
                    continue;
                }

                // Fast path: simple `.class` selectors use inverted index
                if let Some(class_name) = selector.simple_class_name() {
                    if let Some(node_ids) = class_to_nodes.get(class_name) {
                        for &node_id in node_ids {
                            overrides.push((node_id, style.clone()));
                        }
                    }
                    continue;
                }

                // Slow path: complex selectors need full matching
                let all_node_ids: Vec<LayoutNodeId> = self.render_nodes.keys().copied().collect();
                for &node_id in &all_node_ids {
                    if self
                        .complex_selector_matches(selector, node_id, &empty_set, &empty_set, None)
                    {
                        overrides.push((node_id, style.clone()));
                    }
                }
            }
        }

        // Simple ID rules: highest specificity, applied last
        let all_ids = self.element_registry.all_ids();
        for id in &all_ids {
            if let Some(node_id) = self.element_registry.get(id) {
                if let Some(style) = stylesheet.get(id) {
                    if style.has_layout_props() {
                        overrides.push((node_id, style.clone()));
                    }
                }
            }
        }

        for (node_id, es) in overrides {
            let Some(mut style) = self.layout_tree.get_style(node_id) else {
                continue;
            };

            // Sizing
            if let Some(w) = es.width {
                style.size.width = match w {
                    crate::element_style::StyleDimension::Length(px) => Dimension::Length(px),
                    crate::element_style::StyleDimension::Percent(p) => Dimension::Percent(p),
                    crate::element_style::StyleDimension::Auto => Dimension::Auto,
                };
                if matches!(w, crate::element_style::StyleDimension::Auto) {
                    style.flex_basis = Dimension::Auto;
                    style.flex_grow = 0.0;
                    style.flex_shrink = 0.0;
                }
            }
            if let Some(h) = es.height {
                style.size.height = match h {
                    crate::element_style::StyleDimension::Length(px) => Dimension::Length(px),
                    crate::element_style::StyleDimension::Percent(p) => Dimension::Percent(p),
                    crate::element_style::StyleDimension::Auto => Dimension::Auto,
                };
                if matches!(h, crate::element_style::StyleDimension::Auto) {
                    style.flex_basis = Dimension::Auto;
                    style.flex_grow = 0.0;
                    style.flex_shrink = 0.0;
                }
            }
            if let Some(w) = es.min_width {
                style.min_size.width = Dimension::Length(w);
            }
            if let Some(h) = es.min_height {
                style.min_size.height = Dimension::Length(h);
            }
            if let Some(w) = es.max_width {
                style.max_size.width = Dimension::Length(w);
            }
            if let Some(h) = es.max_height {
                style.max_size.height = Dimension::Length(h);
            }

            // Display & flex direction
            if let Some(display) = es.display {
                style.display = match display {
                    StyleDisplay::Flex => Display::Flex,
                    StyleDisplay::Block => Display::Block,
                    StyleDisplay::None => Display::None,
                };
            }
            if let Some(dir) = es.flex_direction {
                style.flex_direction = match dir {
                    StyleFlexDirection::Row => FlexDirection::Row,
                    StyleFlexDirection::Column => FlexDirection::Column,
                    StyleFlexDirection::RowReverse => FlexDirection::RowReverse,
                    StyleFlexDirection::ColumnReverse => FlexDirection::ColumnReverse,
                };
            }
            if let Some(wrap) = es.flex_wrap {
                style.flex_wrap = if wrap {
                    FlexWrap::Wrap
                } else {
                    FlexWrap::NoWrap
                };
            }
            if let Some(grow) = es.flex_grow {
                style.flex_grow = grow;
            }
            if let Some(shrink) = es.flex_shrink {
                style.flex_shrink = shrink;
            }

            // Alignment
            if let Some(align) = es.align_items {
                style.align_items = Some(match align {
                    StyleAlign::Start => AlignItems::Start,
                    StyleAlign::Center => AlignItems::Center,
                    StyleAlign::End => AlignItems::End,
                    StyleAlign::Stretch => AlignItems::Stretch,
                    StyleAlign::Baseline => AlignItems::Baseline,
                });
            }
            if let Some(justify) = es.justify_content {
                style.justify_content = Some(match justify {
                    StyleJustify::Start => JustifyContent::Start,
                    StyleJustify::Center => JustifyContent::Center,
                    StyleJustify::End => JustifyContent::End,
                    StyleJustify::SpaceBetween => JustifyContent::SpaceBetween,
                    StyleJustify::SpaceAround => JustifyContent::SpaceAround,
                    StyleJustify::SpaceEvenly => JustifyContent::SpaceEvenly,
                });
            }
            if let Some(align) = es.align_self {
                style.align_self = Some(match align {
                    StyleAlign::Start => AlignSelf::Start,
                    StyleAlign::Center => AlignSelf::Center,
                    StyleAlign::End => AlignSelf::End,
                    StyleAlign::Stretch => AlignSelf::Stretch,
                    StyleAlign::Baseline => AlignSelf::Baseline,
                });
            }

            // Spacing
            if let Some(SpacingRect {
                top,
                right,
                bottom,
                left,
            }) = es.padding
            {
                style.padding.top = LengthPercentage::Length(top);
                style.padding.right = LengthPercentage::Length(right);
                style.padding.bottom = LengthPercentage::Length(bottom);
                style.padding.left = LengthPercentage::Length(left);
            }
            if let Some(SpacingRect {
                top,
                right,
                bottom,
                left,
            }) = es.margin
            {
                style.margin.top = LengthPercentageAuto::Length(top);
                style.margin.right = LengthPercentageAuto::Length(right);
                style.margin.bottom = LengthPercentageAuto::Length(bottom);
                style.margin.left = LengthPercentageAuto::Length(left);
            }
            if let Some(gap) = es.gap {
                style.gap = taffy::Size {
                    width: LengthPercentage::Length(gap),
                    height: LengthPercentage::Length(gap),
                };
            }

            // Overflow (shorthand sets both axes)
            if let Some(overflow) = es.overflow {
                let val = match overflow {
                    StyleOverflow::Visible => Overflow::Visible,
                    StyleOverflow::Clip => Overflow::Clip,
                    StyleOverflow::Scroll => Overflow::Scroll,
                };
                style.overflow.x = val;
                style.overflow.y = val;
            }
            // Per-axis overflow overrides
            if let Some(ox) = es.overflow_x {
                style.overflow.x = match ox {
                    StyleOverflow::Visible => Overflow::Visible,
                    StyleOverflow::Clip => Overflow::Clip,
                    StyleOverflow::Scroll => Overflow::Scroll,
                };
            }
            if let Some(oy) = es.overflow_y {
                style.overflow.y = match oy {
                    StyleOverflow::Visible => Overflow::Visible,
                    StyleOverflow::Clip => Overflow::Clip,
                    StyleOverflow::Scroll => Overflow::Scroll,
                };
            }

            // Visibility: hidden collapses element from layout
            if let Some(vis) = es.visibility {
                use crate::element_style::StyleVisibility;
                match vis {
                    StyleVisibility::Hidden => style.display = Display::None,
                    StyleVisibility::Visible => {
                        if style.display == Display::None {
                            style.display = Display::Flex;
                        }
                    }
                }
            }

            // Position
            if let Some(pos) = es.position {
                style.position = match pos {
                    StylePosition::Static | StylePosition::Relative | StylePosition::Sticky => {
                        taffy::Position::Relative
                    }
                    StylePosition::Absolute | StylePosition::Fixed => taffy::Position::Absolute,
                };
            }

            // Inset (top, right, bottom, left)
            // For sticky elements, inset values are scroll-lock thresholds, not layout offsets.
            // They go into RenderProps instead (via apply_element_style_to_props).
            let is_sticky = es.position == Some(StylePosition::Sticky);
            if !is_sticky {
                if let Some(top) = es.top {
                    style.inset.top = LengthPercentageAuto::Length(top);
                }
                if let Some(right) = es.right {
                    style.inset.right = LengthPercentageAuto::Length(right);
                }
                if let Some(bottom) = es.bottom {
                    style.inset.bottom = LengthPercentageAuto::Length(bottom);
                }
                if let Some(left) = es.left {
                    style.inset.left = LengthPercentageAuto::Length(left);
                }
            }

            self.layout_tree.set_style(node_id, style);
        }

        // Auto-create scroll physics for any nodes with overflow: scroll
        self.auto_create_css_scroll_physics();
    }

    /// Combined application of both CSS visual (base) styles and layout overrides.
    ///
    /// This is an optimized alternative to calling `apply_stylesheet_base_styles()`
    /// followed by `apply_stylesheet_layout_overrides()` separately. It:
    /// - Builds the class-to-nodes inverted index **once** (not twice)
    /// - Sorts and filters complex rules **once** (not twice)
    /// - Iterates complex rules **once**, applying both visual and layout styles
    ///
    /// Use this in the full rebuild path where both operations are needed together.
    pub fn apply_all_stylesheet_styles(&mut self) {
        use crate::element_style::{
            SpacingRect, StyleAlign, StyleDisplay, StyleFlexDirection, StyleJustify, StyleOverflow,
            StylePosition,
        };

        let stylesheet = match &self.stylesheet {
            Some(s) => s.clone(),
            None => return,
        };

        // Collect layout overrides to apply after the visual pass
        let mut layout_overrides: Vec<(LayoutNodeId, crate::element_style::ElementStyle)> =
            Vec::new();

        // =====================================================================
        // Complex rules: class, type, and combinator selectors
        // Build inverted class index ONCE for both visual + layout
        // =====================================================================
        let complex_rules = stylesheet.complex_rules();
        if !complex_rules.is_empty() {
            let empty_set = std::collections::HashSet::new();

            // Collect non-state rules and sort by specificity (ascending)
            let mut base_rules: Vec<&(
                crate::css_parser::ComplexSelector,
                crate::element_style::ElementStyle,
            )> = complex_rules
                .iter()
                .filter(|(selector, _)| !selector.has_state())
                .collect();
            base_rules.sort_by_key(|(selector, _)| Self::selector_specificity(selector));

            // Build inverted class index ONCE (single lock acquisition)
            let class_to_nodes = self.element_registry.class_to_nodes_index();

            for (selector, style) in base_rules {
                let has_layout = style.has_layout_props();

                // Fast path: simple `.class` selectors use inverted index
                if let Some(class_name) = selector.simple_class_name() {
                    if let Some(node_ids) = class_to_nodes.get(class_name) {
                        for &node_id in node_ids {
                            // Apply visual styles
                            if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                                Self::apply_element_style_to_props(&mut render_node.props, style);
                            }
                            // Collect layout overrides
                            if has_layout {
                                layout_overrides.push((node_id, style.clone()));
                            }
                        }
                    }
                    continue;
                }

                // Slow path: complex selectors (combinators, structural pseudos)
                let all_node_ids: Vec<LayoutNodeId> = self.render_nodes.keys().copied().collect();
                for &node_id in &all_node_ids {
                    if self
                        .complex_selector_matches(selector, node_id, &empty_set, &empty_set, None)
                    {
                        // Apply visual styles
                        if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                            Self::apply_element_style_to_props(&mut render_node.props, style);
                        }
                        // Collect layout overrides
                        if has_layout {
                            layout_overrides.push((node_id, style.clone()));
                        }
                    }
                }
            }

            // NOTE: Do NOT eagerly save base_styles here. This function runs for
            // the entire tree during layout recomputation, at which point hovered
            // nodes may still carry hover styles. Saving contaminated props as
            // "base" causes hover to stick (trailing artifacts, blinking).
            // Eager base_styles saves belong in apply_stylesheet_base_styles() and
            // apply_stylesheet_base_styles_for_subtree() which run when props are clean.
        }

        // =====================================================================
        // Simple ID rules: highest specificity, applied last (overrides class selectors)
        // =====================================================================
        let registered_ids: Vec<(String, LayoutNodeId)> = self
            .element_registry
            .all_ids()
            .into_iter()
            .filter_map(|id| self.element_registry.get(&id).map(|node_id| (id, node_id)))
            .collect();

        for (element_id, node_id) in &registered_ids {
            if let Some(base_style) = stylesheet.get(element_id) {
                // Apply visual styles
                if let Some(render_node) = self.render_nodes.get_mut(node_id) {
                    Self::apply_element_style_to_props(&mut render_node.props, base_style);
                }
                // Collect layout overrides
                if base_style.has_layout_props() {
                    layout_overrides.push((*node_id, base_style.clone()));
                }
            }
        }

        // =====================================================================
        // Post-pass: sync text-align into baked TextData
        // =====================================================================
        for render_node in self.render_nodes.values_mut() {
            if let Some(ta) = render_node.props.text_align {
                if let ElementType::Text(ref mut text_data) = render_node.element_type {
                    text_data.align = ta;
                }
            }
        }

        // =====================================================================
        // Update Stateful base_render_props with CSS-applied values
        // =====================================================================
        for (&node_id, render_node) in &self.render_nodes {
            if crate::stateful::has_stateful_base_updater(node_id) {
                crate::stateful::update_stateful_base_props(node_id, render_node.props.clone());
            }
        }

        // =====================================================================
        // Apply base (non-state) SVG tag-name rules to SVG nodes
        // =====================================================================
        let svg_tag_rules = stylesheet.svg_tag_rules();
        if !svg_tag_rules.is_empty() {
            let svg_nodes: Vec<LayoutNodeId> = self
                .render_nodes
                .keys()
                .copied()
                .filter(|&nid| self.element_registry.get_element_type(nid) == Some("svg"))
                .collect();

            for &svg_node in &svg_nodes {
                let empty_set: std::collections::HashSet<LayoutNodeId> =
                    std::collections::HashSet::new();
                let mut tag_styles: std::collections::HashMap<String, crate::element::SvgTagStyle> =
                    std::collections::HashMap::new();
                for &(tag_name, ancestor_segments, style) in &svg_tag_rules {
                    let has_state = ancestor_segments.iter().any(|(c, _)| c.has_state());
                    if has_state {
                        continue;
                    }
                    let matches = if ancestor_segments.is_empty() {
                        true
                    } else {
                        let last_idx = ancestor_segments.len() - 1;
                        let (last_compound, _) = &ancestor_segments[last_idx];
                        if !self.compound_matches(last_compound, svg_node, false, false, false) {
                            false
                        } else if ancestor_segments.len() == 1 {
                            true
                        } else {
                            let mut current_node = svg_node;
                            let mut all_matched = true;
                            for i in (0..last_idx).rev() {
                                let (compound, combinator) = &ancestor_segments[i];
                                let combinator =
                                    combinator.unwrap_or(crate::css_parser::Combinator::Descendant);
                                match combinator {
                                    crate::css_parser::Combinator::Child => {
                                        match self.element_registry.get_parent(current_node) {
                                            Some(parent) => {
                                                if !self.compound_matches(
                                                    compound, parent, false, false, false,
                                                ) {
                                                    all_matched = false;
                                                    break;
                                                }
                                                current_node = parent;
                                            }
                                            None => {
                                                all_matched = false;
                                                break;
                                            }
                                        }
                                    }
                                    crate::css_parser::Combinator::Descendant => {
                                        let ancestors =
                                            self.element_registry.ancestors(current_node);
                                        let mut found = false;
                                        for ancestor in &ancestors {
                                            if self.compound_matches(
                                                compound, *ancestor, false, false, false,
                                            ) {
                                                current_node = *ancestor;
                                                found = true;
                                                break;
                                            }
                                        }
                                        if !found {
                                            all_matched = false;
                                            break;
                                        }
                                    }
                                    _ => {
                                        all_matched = false;
                                        break;
                                    }
                                }
                            }
                            all_matched
                        }
                    };
                    if matches {
                        let entry = tag_styles.entry(tag_name.to_string()).or_default();
                        if let Some(fill) = style.fill {
                            entry.fill = Some([fill.r, fill.g, fill.b, fill.a]);
                        }
                        if let Some(stroke) = style.stroke {
                            entry.stroke = Some([stroke.r, stroke.g, stroke.b, stroke.a]);
                        }
                        if let Some(sw) = style.stroke_width {
                            entry.stroke_width = Some(sw);
                        }
                        if let Some(ref da) = style.stroke_dasharray {
                            entry.stroke_dasharray = Some(da.clone());
                        }
                        if let Some(offset) = style.stroke_dashoffset {
                            entry.stroke_dashoffset = Some(offset);
                        }
                        if let Some(opacity) = style.opacity {
                            entry.opacity = Some(opacity);
                        }
                    }
                }
                if !tag_styles.is_empty() {
                    if let Some(render_node) = self.render_nodes.get_mut(&svg_node) {
                        render_node.props.svg_tag_styles = tag_styles;
                    }
                }
            }
        }

        // =====================================================================
        // Post-pass: propagate inherited text properties from parent to child
        // =====================================================================
        let all_node_ids: Vec<LayoutNodeId> = self.render_nodes.keys().copied().collect();
        for node_id in all_node_ids {
            let parent_id = match self.element_registry.get_parent(node_id) {
                Some(id) => id,
                None => continue,
            };
            let parent_text_props = self.render_nodes.get(&parent_id).map(|n| {
                (
                    n.props.text_decoration,
                    n.props.text_decoration_color,
                    n.props.text_decoration_thickness,
                    n.props.white_space,
                    n.props.text_overflow,
                    n.props.text_color,
                    n.props.text_align,
                    n.props.fill,
                    n.props.stroke,
                    n.props.stroke_width,
                )
            });
            if let Some((td, td_color, td_thick, ws, to, tc, ta, fill, stroke, stroke_w)) =
                parent_text_props
            {
                if let Some(node) = self.render_nodes.get_mut(&node_id) {
                    if node.props.text_decoration.is_none() {
                        node.props.text_decoration = td;
                    }
                    if node.props.text_decoration_color.is_none() {
                        node.props.text_decoration_color = td_color;
                    }
                    if node.props.text_decoration_thickness.is_none() {
                        node.props.text_decoration_thickness = td_thick;
                    }
                    if node.props.white_space.is_none() {
                        node.props.white_space = ws;
                    }
                    if node.props.text_overflow.is_none() {
                        node.props.text_overflow = to;
                    }
                    if node.props.text_color.is_none() {
                        node.props.text_color = tc;
                    }
                    if node.props.text_align.is_none() {
                        if let Some(ta) = ta {
                            node.props.text_align = Some(ta);
                            if let ElementType::Text(ref mut text_data) = node.element_type {
                                text_data.align = ta;
                            }
                        }
                    }
                    if node.props.fill.is_none() {
                        node.props.fill = fill;
                    }
                    if node.props.stroke.is_none() {
                        node.props.stroke = stroke;
                    }
                    if node.props.stroke_width.is_none() {
                        node.props.stroke_width = stroke_w;
                    }
                }
            }
        }

        // =====================================================================
        // Apply collected layout overrides to taffy styles
        // =====================================================================
        for (node_id, es) in layout_overrides {
            let Some(mut style) = self.layout_tree.get_style(node_id) else {
                continue;
            };

            // Sizing
            if let Some(w) = es.width {
                style.size.width = match w {
                    crate::element_style::StyleDimension::Length(px) => Dimension::Length(px),
                    crate::element_style::StyleDimension::Percent(p) => Dimension::Percent(p),
                    crate::element_style::StyleDimension::Auto => Dimension::Auto,
                };
                if matches!(w, crate::element_style::StyleDimension::Auto) {
                    style.flex_basis = Dimension::Auto;
                    style.flex_grow = 0.0;
                    style.flex_shrink = 0.0;
                }
            }
            if let Some(h) = es.height {
                style.size.height = match h {
                    crate::element_style::StyleDimension::Length(px) => Dimension::Length(px),
                    crate::element_style::StyleDimension::Percent(p) => Dimension::Percent(p),
                    crate::element_style::StyleDimension::Auto => Dimension::Auto,
                };
                if matches!(h, crate::element_style::StyleDimension::Auto) {
                    style.flex_basis = Dimension::Auto;
                    style.flex_grow = 0.0;
                    style.flex_shrink = 0.0;
                }
            }
            if let Some(w) = es.min_width {
                style.min_size.width = Dimension::Length(w);
            }
            if let Some(h) = es.min_height {
                style.min_size.height = Dimension::Length(h);
            }
            if let Some(w) = es.max_width {
                style.max_size.width = Dimension::Length(w);
            }
            if let Some(h) = es.max_height {
                style.max_size.height = Dimension::Length(h);
            }

            // Display & flex direction
            if let Some(display) = es.display {
                style.display = match display {
                    StyleDisplay::Flex => Display::Flex,
                    StyleDisplay::Block => Display::Block,
                    StyleDisplay::None => Display::None,
                };
            }
            if let Some(dir) = es.flex_direction {
                style.flex_direction = match dir {
                    StyleFlexDirection::Row => FlexDirection::Row,
                    StyleFlexDirection::Column => FlexDirection::Column,
                    StyleFlexDirection::RowReverse => FlexDirection::RowReverse,
                    StyleFlexDirection::ColumnReverse => FlexDirection::ColumnReverse,
                };
            }
            if let Some(wrap) = es.flex_wrap {
                style.flex_wrap = if wrap {
                    FlexWrap::Wrap
                } else {
                    FlexWrap::NoWrap
                };
            }
            if let Some(grow) = es.flex_grow {
                style.flex_grow = grow;
            }
            if let Some(shrink) = es.flex_shrink {
                style.flex_shrink = shrink;
            }

            // Alignment
            if let Some(align) = es.align_items {
                style.align_items = Some(match align {
                    StyleAlign::Start => AlignItems::Start,
                    StyleAlign::Center => AlignItems::Center,
                    StyleAlign::End => AlignItems::End,
                    StyleAlign::Stretch => AlignItems::Stretch,
                    StyleAlign::Baseline => AlignItems::Baseline,
                });
            }
            if let Some(justify) = es.justify_content {
                style.justify_content = Some(match justify {
                    StyleJustify::Start => JustifyContent::Start,
                    StyleJustify::Center => JustifyContent::Center,
                    StyleJustify::End => JustifyContent::End,
                    StyleJustify::SpaceBetween => JustifyContent::SpaceBetween,
                    StyleJustify::SpaceAround => JustifyContent::SpaceAround,
                    StyleJustify::SpaceEvenly => JustifyContent::SpaceEvenly,
                });
            }
            if let Some(align) = es.align_self {
                style.align_self = Some(match align {
                    StyleAlign::Start => AlignSelf::Start,
                    StyleAlign::Center => AlignSelf::Center,
                    StyleAlign::End => AlignSelf::End,
                    StyleAlign::Stretch => AlignSelf::Stretch,
                    StyleAlign::Baseline => AlignSelf::Baseline,
                });
            }

            // Spacing
            if let Some(SpacingRect {
                top,
                right,
                bottom,
                left,
            }) = es.padding
            {
                style.padding.top = LengthPercentage::Length(top);
                style.padding.right = LengthPercentage::Length(right);
                style.padding.bottom = LengthPercentage::Length(bottom);
                style.padding.left = LengthPercentage::Length(left);
            }
            if let Some(SpacingRect {
                top,
                right,
                bottom,
                left,
            }) = es.margin
            {
                style.margin.top = LengthPercentageAuto::Length(top);
                style.margin.right = LengthPercentageAuto::Length(right);
                style.margin.bottom = LengthPercentageAuto::Length(bottom);
                style.margin.left = LengthPercentageAuto::Length(left);
            }
            if let Some(gap) = es.gap {
                style.gap = taffy::Size {
                    width: LengthPercentage::Length(gap),
                    height: LengthPercentage::Length(gap),
                };
            }

            // Overflow
            if let Some(overflow) = es.overflow {
                let val = match overflow {
                    StyleOverflow::Visible => Overflow::Visible,
                    StyleOverflow::Clip => Overflow::Clip,
                    StyleOverflow::Scroll => Overflow::Scroll,
                };
                style.overflow.x = val;
                style.overflow.y = val;
            }
            if let Some(ox) = es.overflow_x {
                style.overflow.x = match ox {
                    StyleOverflow::Visible => Overflow::Visible,
                    StyleOverflow::Clip => Overflow::Clip,
                    StyleOverflow::Scroll => Overflow::Scroll,
                };
            }
            if let Some(oy) = es.overflow_y {
                style.overflow.y = match oy {
                    StyleOverflow::Visible => Overflow::Visible,
                    StyleOverflow::Clip => Overflow::Clip,
                    StyleOverflow::Scroll => Overflow::Scroll,
                };
            }

            // Visibility
            if let Some(vis) = es.visibility {
                use crate::element_style::StyleVisibility;
                match vis {
                    StyleVisibility::Hidden => style.display = Display::None,
                    StyleVisibility::Visible => {
                        if style.display == Display::None {
                            style.display = Display::Flex;
                        }
                    }
                }
            }

            // Position
            if let Some(pos) = es.position {
                style.position = match pos {
                    StylePosition::Static | StylePosition::Relative | StylePosition::Sticky => {
                        taffy::Position::Relative
                    }
                    StylePosition::Absolute | StylePosition::Fixed => taffy::Position::Absolute,
                };
            }

            // Inset
            let is_sticky = es.position == Some(StylePosition::Sticky);
            if !is_sticky {
                if let Some(top) = es.top {
                    style.inset.top = LengthPercentageAuto::Length(top);
                }
                if let Some(right) = es.right {
                    style.inset.right = LengthPercentageAuto::Length(right);
                }
                if let Some(bottom) = es.bottom {
                    style.inset.bottom = LengthPercentageAuto::Length(bottom);
                }
                if let Some(left) = es.left {
                    style.inset.left = LengthPercentageAuto::Length(left);
                }
            }

            self.layout_tree.set_style(node_id, style);
        }

        // Auto-create scroll physics for any nodes with overflow: scroll
        self.auto_create_css_scroll_physics();
    }

    /// Auto-create scroll physics for nodes with `overflow: scroll` set via CSS.
    ///
    /// Scans all layout nodes and creates scroll physics + event handlers
    /// for any node that has `overflow: scroll` on either axis but doesn't
    /// already have scroll physics registered (e.g., from the `scroll()` widget
    /// or the `overflow_scroll()` Div builder).
    pub fn auto_create_css_scroll_physics(&mut self) {
        use crate::scroll::{Scroll, ScrollConfig, ScrollDirection, ScrollPhysics};

        // Collect nodes that need scroll physics
        let all_ids = self.element_registry.all_ids();
        let mut needs_physics: Vec<(LayoutNodeId, ScrollDirection)> = Vec::new();

        for id in &all_ids {
            let Some(node_id) = self.element_registry.get(id) else {
                continue;
            };
            // Skip if already has scroll physics (from Scroll widget or Div builder)
            if self.scroll_physics.contains_key(&node_id) {
                continue;
            }
            let Some(style) = self.layout_tree.get_style(node_id) else {
                continue;
            };
            let scroll_x = matches!(style.overflow.x, Overflow::Scroll);
            let scroll_y = matches!(style.overflow.y, Overflow::Scroll);
            if !scroll_x && !scroll_y {
                continue;
            }
            let direction = match (scroll_x, scroll_y) {
                (true, true) => ScrollDirection::Both,
                (true, false) => ScrollDirection::Horizontal,
                (false, true) => ScrollDirection::Vertical,
                _ => unreachable!(),
            };
            needs_physics.push((node_id, direction));
        }

        // Create physics and register handlers
        for (node_id, direction) in needs_physics {
            tracing::trace!(
                "auto_create_css_scroll_physics: creating for {:?} direction={:?}",
                node_id,
                direction
            );
            let config = ScrollConfig {
                direction,
                ..Default::default()
            };
            let physics = Arc::new(Mutex::new(ScrollPhysics::new(config)));
            // Set animation scheduler for bounce springs
            if let Some(scheduler) = self.animations.upgrade() {
                physics.lock().unwrap().set_scheduler(&scheduler);
            }
            let handlers = Scroll::create_internal_handlers(Arc::clone(&physics));
            self.scroll_physics.insert(node_id, physics);
            self.handler_registry.register(node_id, handlers);
        }
    }

    /// Check if a node has stylesheet state styles defined
    ///
    /// Returns true if the node has an ID and the stylesheet has any
    /// state-specific styles (`:hover`, `:active`, `:focus`, `:disabled`) for it.
    pub fn has_state_styles(&self, node_id: LayoutNodeId) -> bool {
        let stylesheet = match &self.stylesheet {
            Some(s) => s,
            None => return false,
        };

        let element_id = match self.element_registry.get_id(node_id) {
            Some(id) => id,
            None => return false,
        };

        // Check if any simple state styles exist
        if stylesheet.contains_with_state(&element_id, ElementState::Hover)
            || stylesheet.contains_with_state(&element_id, ElementState::Active)
            || stylesheet.contains_with_state(&element_id, ElementState::Focus)
            || stylesheet.contains_with_state(&element_id, ElementState::Disabled)
        {
            return true;
        }

        // Check if any complex rules with state exist that could affect this node
        stylesheet.has_complex_state_rules()
    }

    // =========================================================================
    // Complex Selector Matching Engine
    // =========================================================================

    /// Check if a compound selector matches a given node
    fn compound_matches(
        &self,
        compound: &CompoundSelector,
        node_id: LayoutNodeId,
        hovered: bool,
        pressed: bool,
        focused: bool,
    ) -> bool {
        for part in &compound.parts {
            match part {
                SelectorPart::Type(type_name) => {
                    let node_type = self.element_registry.get_element_type(node_id);
                    if node_type != Some(type_name.as_str()) {
                        return false;
                    }
                }
                SelectorPart::Id(id) => {
                    let node_id_str = self.element_registry.get_id(node_id);
                    if node_id_str.as_deref() != Some(id.as_str()) {
                        return false;
                    }
                }
                SelectorPart::Class(class) => {
                    if !self.element_registry.has_class(node_id, class) {
                        return false;
                    }
                }
                SelectorPart::State(state) => {
                    let matches = match state {
                        ElementState::Hover => hovered,
                        ElementState::Active => pressed,
                        ElementState::Focus => focused,
                        ElementState::Disabled => false, // TODO: track disabled state
                        ElementState::Checked => false, // checked state managed by widget callbacks
                    };
                    if !matches {
                        return false;
                    }
                }
                SelectorPart::Universal => {
                    // Universal selector matches everything — continue
                }
                SelectorPart::Not(inner_compound) => {
                    // :not(selector) — matches if the inner compound does NOT match
                    if self.compound_matches(inner_compound, node_id, hovered, pressed, focused) {
                        return false;
                    }
                }
                SelectorPart::Is(selectors) => {
                    // :is(sel1, sel2, ...) — matches if ANY inner selector matches
                    let any_match = selectors
                        .iter()
                        .any(|s| self.compound_matches(s, node_id, hovered, pressed, focused));
                    if !any_match {
                        return false;
                    }
                }
                SelectorPart::PseudoClass(pseudo) => {
                    let matches = match pseudo {
                        StructuralPseudo::FirstChild => {
                            self.element_registry.get_child_index(node_id) == Some(0)
                        }
                        StructuralPseudo::LastChild => {
                            let index = self.element_registry.get_child_index(node_id);
                            let count = self.element_registry.get_sibling_count(node_id);
                            match (index, count) {
                                (Some(i), Some(c)) if c > 0 => i == c - 1,
                                _ => false,
                            }
                        }
                        StructuralPseudo::NthChild(n) => {
                            // nth-child is 1-based in CSS
                            self.element_registry.get_child_index(node_id)
                                == Some(n.saturating_sub(1))
                        }
                        StructuralPseudo::NthLastChild(n) => {
                            // nth-last-child is 1-based from the end
                            let index = self.element_registry.get_child_index(node_id);
                            let count = self.element_registry.get_sibling_count(node_id);
                            match (index, count) {
                                (Some(i), Some(c)) if c > 0 => i == c - n,
                                _ => false,
                            }
                        }
                        StructuralPseudo::OnlyChild => {
                            self.element_registry.get_sibling_count(node_id) == Some(1)
                        }
                        StructuralPseudo::Empty => !self.element_registry.has_children(node_id),
                        StructuralPseudo::Root => self.element_registry.is_root(node_id),
                        // *-of-type: In Blinc all elements are Div, so equivalent to *-child
                        StructuralPseudo::FirstOfType => {
                            self.element_registry.get_child_index(node_id) == Some(0)
                        }
                        StructuralPseudo::LastOfType => {
                            let index = self.element_registry.get_child_index(node_id);
                            let count = self.element_registry.get_sibling_count(node_id);
                            match (index, count) {
                                (Some(i), Some(c)) if c > 0 => i == c - 1,
                                _ => false,
                            }
                        }
                        StructuralPseudo::NthOfType(n) => {
                            self.element_registry.get_child_index(node_id)
                                == Some(n.saturating_sub(1))
                        }
                        StructuralPseudo::NthLastOfType(n) => {
                            let index = self.element_registry.get_child_index(node_id);
                            let count = self.element_registry.get_sibling_count(node_id);
                            match (index, count) {
                                (Some(i), Some(c)) if c > 0 => i == c - n,
                                _ => false,
                            }
                        }
                        StructuralPseudo::OnlyOfType => {
                            self.element_registry.get_sibling_count(node_id) == Some(1)
                        }
                    };
                    if !matches {
                        return false;
                    }
                }
                SelectorPart::PseudoElement(_) => {
                    // Pseudo-elements like ::placeholder are handled at the rule storage level
                    // (stored with key "id::placeholder"), not during runtime matching.
                    // If we encounter one here, it doesn't match element nodes.
                    return false;
                }
            }
        }
        true
    }

    /// Check if a complex selector matches a given target node
    ///
    /// Walks the selector chain right-to-left (target element first),
    /// checking combinators against the ancestor chain.
    /// Calculate CSS specificity for a complex selector as (ids, classes, types).
    /// Used for sorting: lower specificity rules apply first, higher ones override.
    fn selector_specificity(selector: &ComplexSelector) -> (u32, u32, u32) {
        let (mut ids, mut classes, mut types) = (0u32, 0u32, 0u32);
        for (compound, _) in &selector.segments {
            for part in &compound.parts {
                match part {
                    SelectorPart::Id(_) => ids += 1,
                    SelectorPart::Class(_)
                    | SelectorPart::State(_)
                    | SelectorPart::PseudoClass(_)
                    | SelectorPart::Not(_)
                    | SelectorPart::Is(_) => classes += 1,
                    SelectorPart::Type(_) => types += 1,
                    SelectorPart::Universal | SelectorPart::PseudoElement(_) => {}
                }
            }
        }
        (ids, classes, types)
    }

    fn complex_selector_matches(
        &self,
        selector: &ComplexSelector,
        target_node: LayoutNodeId,
        hovered_nodes: &std::collections::HashSet<LayoutNodeId>,
        pressed_nodes: &std::collections::HashSet<LayoutNodeId>,
        focused_node: Option<LayoutNodeId>,
    ) -> bool {
        if selector.segments.is_empty() {
            return false;
        }

        // Start from the rightmost segment (the target)
        let segments = &selector.segments;
        let (target_compound, _) = &segments[segments.len() - 1];

        // Check target node against the rightmost compound
        let target_hovered = hovered_nodes.contains(&target_node);
        let target_pressed = pressed_nodes.contains(&target_node);
        let target_focused = focused_node == Some(target_node);

        if !self.compound_matches(
            target_compound,
            target_node,
            target_hovered,
            target_pressed,
            target_focused,
        ) {
            return false;
        }

        // Walk backwards through remaining segments (ancestors)
        if segments.len() == 1 {
            return true; // Simple selector, already matched
        }

        let mut current_node = target_node;

        // Walk from right-to-left through the selector segments (skip the last which is the target)
        for i in (0..segments.len() - 1).rev() {
            let (compound, combinator) = &segments[i];
            let combinator = combinator.unwrap_or(Combinator::Descendant);

            match combinator {
                Combinator::Child => {
                    // Must match the immediate parent
                    let parent = match self.element_registry.get_parent(current_node) {
                        Some(p) => p,
                        None => return false,
                    };
                    let parent_hovered = hovered_nodes.contains(&parent);
                    let parent_pressed = pressed_nodes.contains(&parent);
                    let parent_focused = focused_node == Some(parent);

                    if !self.compound_matches(
                        compound,
                        parent,
                        parent_hovered,
                        parent_pressed,
                        parent_focused,
                    ) {
                        return false;
                    }
                    current_node = parent;
                }
                Combinator::Descendant => {
                    // Walk up ancestors until a match is found
                    let ancestors = self.element_registry.ancestors(current_node);
                    let mut found = false;
                    for ancestor in &ancestors {
                        let anc_hovered = hovered_nodes.contains(ancestor);
                        let anc_pressed = pressed_nodes.contains(ancestor);
                        let anc_focused = focused_node == Some(*ancestor);

                        if self.compound_matches(
                            compound,
                            *ancestor,
                            anc_hovered,
                            anc_pressed,
                            anc_focused,
                        ) {
                            current_node = *ancestor;
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        return false;
                    }
                }
                Combinator::AdjacentSibling => {
                    // Must match the immediately preceding sibling
                    let prev_sibling =
                        match self.element_registry.get_previous_sibling(current_node) {
                            Some(s) => s,
                            None => return false,
                        };
                    let sib_hovered = hovered_nodes.contains(&prev_sibling);
                    let sib_pressed = pressed_nodes.contains(&prev_sibling);
                    let sib_focused = focused_node == Some(prev_sibling);

                    if !self.compound_matches(
                        compound,
                        prev_sibling,
                        sib_hovered,
                        sib_pressed,
                        sib_focused,
                    ) {
                        return false;
                    }
                    current_node = prev_sibling;
                }
                Combinator::GeneralSibling => {
                    // Walk preceding siblings until a match is found
                    let preceding = self.element_registry.get_preceding_siblings(current_node);
                    let mut found = false;
                    for sibling in preceding.iter().rev() {
                        let sib_hovered = hovered_nodes.contains(sibling);
                        let sib_pressed = pressed_nodes.contains(sibling);
                        let sib_focused = focused_node == Some(*sibling);

                        if self.compound_matches(
                            compound,
                            *sibling,
                            sib_hovered,
                            sib_pressed,
                            sib_focused,
                        ) {
                            current_node = *sibling;
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        return false;
                    }
                }
            }
        }

        true
    }

    /// Apply complex selector rules that match the current state
    ///
    /// Returns true if any styles were applied.
    fn apply_complex_selector_styles(&mut self, router: &crate::event_router::EventRouter) -> bool {
        let stylesheet = match &self.stylesheet {
            Some(s) => s.clone(),
            None => return false,
        };

        let complex_rules = stylesheet.complex_rules();
        if complex_rules.is_empty() {
            return false;
        }

        // Collect current interaction state into sets for efficient lookup
        let hovered_nodes: std::collections::HashSet<LayoutNodeId> =
            router.hovered_nodes().collect();
        let pressed_nodes: std::collections::HashSet<LayoutNodeId> =
            router.pressed_target().into_iter().collect();
        let focused_node: Option<LayoutNodeId> = {
            // Check all registered nodes for focus
            let mut focused = None;
            for id in self.element_registry.all_ids() {
                if let Some(nid) = self.element_registry.get(&id) {
                    if router.is_focused(nid) {
                        focused = Some(nid);
                        break;
                    }
                }
            }
            focused
        };

        // Collect ALL render node IDs (not just those with IDs — .class selectors can match any node)
        let all_node_ids: Vec<LayoutNodeId> = self.render_nodes.keys().copied().collect();

        // Reset previously state-affected nodes to their base props so that
        // styles from rules that no longer match (e.g. :hover ended) are removed
        let prev_affected: HashSet<LayoutNodeId> = std::mem::take(&mut self.complex_state_affected);

        // Snapshot BEFORE the reset for prev_affected nodes — captures the actual
        // visual state (e.g. hover values after a transition completed).  This way
        // when the same hover state is re-applied, before == after → no spurious
        // re-transition.
        let mut pre_reset_snapshots: HashMap<LayoutNodeId, blinc_animation::KeyframeProperties> =
            HashMap::new();
        for &node_id in &prev_affected {
            if let Some(kp) = self.snapshot_before_keyframe_properties(node_id) {
                pre_reset_snapshots.insert(node_id, kp);
            }
        }

        for &node_id in &prev_affected {
            if let Some(base) = self.base_styles.get(&node_id) {
                if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                    render_node.props = base.clone();
                }
            }
            // Reset taffy layout to base for nodes leaving state
            if let Some(base_taffy) = self.base_taffy_styles.get(&node_id) {
                self.layout_tree.set_style(node_id, base_taffy.clone());
            }
        }

        // Re-apply all base (non-state) complex rules for reset nodes,
        // sorted by specificity so type < class < id.
        {
            let mut base_rules: Vec<&(ComplexSelector, crate::element_style::ElementStyle)> =
                complex_rules
                    .iter()
                    .filter(|(selector, _)| !selector.has_state())
                    .collect();
            base_rules.sort_by_key(|(selector, _)| Self::selector_specificity(selector));

            for (selector, style) in &base_rules {
                for &node_id in &prev_affected {
                    if self.complex_selector_matches(
                        selector,
                        node_id,
                        &hovered_nodes,
                        &pressed_nodes,
                        focused_node,
                    ) {
                        if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                            Self::apply_element_style_to_props(&mut render_node.props, style);
                        }
                    }
                }
            }
        }

        // Re-apply simple ID rules on top (highest specificity)
        if let Some(stylesheet) = &self.stylesheet {
            let stylesheet = stylesheet.clone();
            for &node_id in &prev_affected {
                if let Some(element_id) = self.element_registry.get_id(node_id) {
                    if let Some(base_style) = stylesheet.get(&element_id) {
                        if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                            Self::apply_element_style_to_props(&mut render_node.props, base_style);
                        }
                    }
                }
            }
        }

        // If any prev_affected nodes were reset, styles changed
        let mut any_applied = !prev_affected.is_empty();

        for (selector, style) in complex_rules {
            // Base (non-state) rules are already applied:
            //   - At tree build time by apply_stylesheet_base_styles()
            //   - For prev_affected (leaving-state) nodes in the reset section above
            // Only state-dependent rules (:hover, :active, :focus, etc.) need per-frame matching.
            let is_state_rule = selector.has_state();
            if !is_state_rule {
                continue;
            }

            for &node_id in &all_node_ids {
                if self.complex_selector_matches(
                    selector,
                    node_id,
                    &hovered_nodes,
                    &pressed_nodes,
                    focused_node,
                ) {
                    // Save base styles for nodes affected by state rules (for future reset)
                    if is_state_rule {
                        if !self.base_styles.contains_key(&node_id) {
                            if let Some(render_node) = self.render_nodes.get(&node_id) {
                                self.base_styles.insert(node_id, render_node.props.clone());
                            }
                        }
                        if !self.base_taffy_styles.contains_key(&node_id) {
                            if let Some(taffy_style) = self.layout_tree.get_style(node_id) {
                                self.base_taffy_styles.insert(node_id, taffy_style);
                            }
                        }
                        self.complex_state_affected.insert(node_id);
                    }

                    // Check for transition support — look in both ID-based and class-based rules
                    let transition_set = {
                        // First try by element ID
                        let by_id = self.element_registry.get_id(node_id).and_then(|eid| {
                            stylesheet.get(&eid).and_then(|s| s.transition.clone())
                        });
                        // If not found, check if any matching base complex rule has a transition
                        by_id.or_else(|| {
                            complex_rules.iter().find_map(|(sel, sty)| {
                                if !sel.has_state()
                                    && self.complex_selector_matches(
                                        sel,
                                        node_id,
                                        &hovered_nodes,
                                        &pressed_nodes,
                                        focused_node,
                                    )
                                {
                                    sty.transition.clone()
                                } else {
                                    None
                                }
                            })
                        })
                    };

                    // For nodes already in prev_affected (sustaining a state from
                    // last frame), skip transition detection — the existing transition
                    // is already in progress or completed. Re-detecting every frame
                    // can cause spurious restarts due to snapshot mismatches between
                    // overlaid and non-overlaid properties.
                    let is_sustaining = prev_affected.contains(&node_id);

                    let before_kp = if !is_sustaining && transition_set.is_some() {
                        self.snapshot_before_keyframe_properties(node_id)
                    } else {
                        None
                    };

                    if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                        Self::apply_element_style_to_props(&mut render_node.props, style);
                        any_applied = true;
                    }
                    // Apply layout changes from complex selector styles
                    if style.has_layout_props() {
                        if let Some(mut taffy_style) = self.layout_tree.get_style(node_id) {
                            Self::apply_element_style_to_taffy(&mut taffy_style, style);
                            self.layout_tree.set_style(node_id, taffy_style);
                        }
                    }

                    // Detect transitions only for newly entering state (not sustaining)
                    if let (Some(before_kp), Some(transition_set)) = (before_kp, transition_set) {
                        if let Some(after_kp) = self.snapshot_keyframe_properties(node_id) {
                            self.detect_and_start_transitions(
                                node_id,
                                &before_kp,
                                &after_kp,
                                &transition_set,
                            );
                        }
                    }
                }
            }
        }

        // Detect reverse transitions for nodes that WERE state-affected but
        // are no longer matched (e.g. mouse left a :hover element).  These nodes
        // were reset to base above but the transition detection inside the main
        // loop only runs for *matching* rules, so we handle the leave case here.
        for &node_id in &prev_affected {
            if self.complex_state_affected.contains(&node_id) {
                continue; // still matched — already handled above
            }
            // Look up transition set for this node
            let transition_set = {
                let by_id = self
                    .element_registry
                    .get_id(node_id)
                    .and_then(|eid| stylesheet.get(&eid).and_then(|s| s.transition.clone()));
                by_id.or_else(|| {
                    complex_rules.iter().find_map(|(sel, sty)| {
                        if !sel.has_state()
                            && self.complex_selector_matches(
                                sel,
                                node_id,
                                &hovered_nodes,
                                &pressed_nodes,
                                focused_node,
                            )
                        {
                            sty.transition.clone()
                        } else {
                            None
                        }
                    })
                })
            };
            if let Some(transition_set) = transition_set {
                if let Some(before_kp) = pre_reset_snapshots.get(&node_id) {
                    if let Some(after_kp) = self.snapshot_keyframe_properties(node_id) {
                        self.detect_and_start_transitions(
                            node_id,
                            before_kp,
                            &after_kp,
                            &transition_set,
                        );
                    }
                }
            }
        }

        any_applied
    }

    /// Apply SVG tag-name CSS rules (e.g., `path { fill: red; }`, `#my-svg circle { stroke: blue; }`)
    ///
    /// For each complex rule targeting an SVG tag name, finds SVG layout nodes whose
    /// ancestor chain matches the remaining selector segments and stores per-tag
    /// style overrides on those nodes' RenderProps.
    fn apply_svg_tag_styles(&mut self, router: &crate::event_router::EventRouter) -> bool {
        let stylesheet = match &self.stylesheet {
            Some(s) => s.clone(),
            None => return false,
        };

        let svg_tag_rules = stylesheet.svg_tag_rules();
        if svg_tag_rules.is_empty() {
            return false;
        }

        // Collect interaction state
        let hovered_nodes: std::collections::HashSet<LayoutNodeId> =
            router.hovered_nodes().collect();
        let pressed_nodes: std::collections::HashSet<LayoutNodeId> =
            router.pressed_target().into_iter().collect();
        let focused_node: Option<LayoutNodeId> = {
            let mut focused = None;
            for id in self.element_registry.all_ids() {
                if let Some(nid) = self.element_registry.get(&id) {
                    if router.is_focused(nid) {
                        focused = Some(nid);
                        break;
                    }
                }
            }
            focused
        };

        // Find all SVG layout nodes
        let svg_nodes: Vec<LayoutNodeId> = self
            .render_nodes
            .keys()
            .copied()
            .filter(|&nid| self.element_registry.get_element_type(nid) == Some("svg"))
            .collect();

        if svg_nodes.is_empty() {
            return false;
        }

        let mut any_changed = false;

        for &svg_node in &svg_nodes {
            let mut tag_styles: std::collections::HashMap<String, crate::element::SvgTagStyle> =
                std::collections::HashMap::new();

            for &(tag_name, ancestor_segments, style) in &svg_tag_rules {
                // Check if ancestor segments match the SVG node
                let matches = if ancestor_segments.is_empty() {
                    // Bare tag selector (e.g., `path { ... }`) — matches all SVGs
                    true
                } else {
                    // Build a temporary ComplexSelector with ancestor segments + SVG node as target
                    // The last ancestor segment targets the SVG node itself
                    let last_idx = ancestor_segments.len() - 1;
                    let (last_compound, _) = &ancestor_segments[last_idx];

                    // Check if the SVG node matches the last ancestor compound
                    let svg_hovered = hovered_nodes.contains(&svg_node);
                    let svg_pressed = pressed_nodes.contains(&svg_node);
                    let svg_focused = focused_node == Some(svg_node);

                    if !self.compound_matches(
                        last_compound,
                        svg_node,
                        svg_hovered,
                        svg_pressed,
                        svg_focused,
                    ) {
                        false
                    } else if ancestor_segments.len() == 1 {
                        // Only one ancestor segment and it matched the SVG node
                        true
                    } else {
                        // Walk remaining ancestor segments up the tree
                        let mut current_node = svg_node;
                        let mut all_matched = true;
                        for i in (0..last_idx).rev() {
                            let (compound, combinator) = &ancestor_segments[i];
                            let combinator =
                                combinator.unwrap_or(crate::css_parser::Combinator::Descendant);
                            match combinator {
                                crate::css_parser::Combinator::Child => {
                                    let parent =
                                        match self.element_registry.get_parent(current_node) {
                                            Some(p) => p,
                                            None => {
                                                all_matched = false;
                                                break;
                                            }
                                        };
                                    let p_hov = hovered_nodes.contains(&parent);
                                    let p_prs = pressed_nodes.contains(&parent);
                                    let p_foc = focused_node == Some(parent);
                                    if !self.compound_matches(compound, parent, p_hov, p_prs, p_foc)
                                    {
                                        all_matched = false;
                                        break;
                                    }
                                    current_node = parent;
                                }
                                crate::css_parser::Combinator::Descendant => {
                                    let ancestors = self.element_registry.ancestors(current_node);
                                    let mut found = false;
                                    for ancestor in &ancestors {
                                        let a_hov = hovered_nodes.contains(ancestor);
                                        let a_prs = pressed_nodes.contains(ancestor);
                                        let a_foc = focused_node == Some(*ancestor);
                                        if self.compound_matches(
                                            compound, *ancestor, a_hov, a_prs, a_foc,
                                        ) {
                                            current_node = *ancestor;
                                            found = true;
                                            break;
                                        }
                                    }
                                    if !found {
                                        all_matched = false;
                                        break;
                                    }
                                }
                                _ => {
                                    all_matched = false;
                                    break;
                                }
                            }
                        }
                        all_matched
                    }
                };

                if matches {
                    // Extract SVG-relevant properties from ElementStyle into SvgTagStyle
                    let entry = tag_styles.entry(tag_name.to_string()).or_default();
                    if let Some(fill) = style.fill {
                        entry.fill = Some([fill.r, fill.g, fill.b, fill.a]);
                    }
                    if let Some(stroke) = style.stroke {
                        entry.stroke = Some([stroke.r, stroke.g, stroke.b, stroke.a]);
                    }
                    if let Some(sw) = style.stroke_width {
                        entry.stroke_width = Some(sw);
                    }
                    if let Some(ref da) = style.stroke_dasharray {
                        entry.stroke_dasharray = Some(da.clone());
                    }
                    if let Some(offset) = style.stroke_dashoffset {
                        entry.stroke_dashoffset = Some(offset);
                    }
                    if let Some(opacity) = style.opacity {
                        entry.opacity = Some(opacity);
                    }
                }
            }

            // Apply tag styles to render props
            if !tag_styles.is_empty() {
                if let Some(render_node) = self.render_nodes.get_mut(&svg_node) {
                    if render_node.props.svg_tag_styles != tag_styles {
                        render_node.props.svg_tag_styles = tag_styles;
                        any_changed = true;
                    }
                }
            } else if let Some(render_node) = self.render_nodes.get_mut(&svg_node) {
                if !render_node.props.svg_tag_styles.is_empty() {
                    render_node.props.svg_tag_styles.clear();
                    any_changed = true;
                }
            }
        }

        any_changed
    }

    // =========================================================================
    // CSS Keyframe Animation Methods
    // =========================================================================

    /// Start a CSS keyframe animation for a node
    ///
    /// Looks up the element's animation from the stylesheet and starts it.
    /// Returns true if an animation was started.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // CSS: #button { animation: pulse 1000ms infinite; }
    /// tree.start_css_animation_for_element(node_id);
    /// ```
    pub fn start_css_animation_for_element(&mut self, node_id: LayoutNodeId) -> bool {
        let stylesheet = match &self.stylesheet {
            Some(s) => s.clone(),
            None => return false,
        };

        let element_id = match self.element_registry.get_id(node_id) {
            Some(id) => id,
            None => return false,
        };

        // Resolve the full keyframe animation
        if let Some(animation) = stylesheet.resolve_keyframe_animation(&element_id) {
            self.css_anim_store.lock().unwrap().animations.insert(
                node_id,
                crate::render_state::ActiveCssAnimation::new(animation),
            );
            return true;
        }

        false
    }

    /// Start a CSS keyframe animation for a node with a specific state
    ///
    /// Used when element state changes (hover, active, etc.) and the state
    /// has an animation defined.
    pub fn start_css_animation_for_state(
        &mut self,
        node_id: LayoutNodeId,
        state: ElementState,
    ) -> bool {
        let stylesheet = match &self.stylesheet {
            Some(s) => s.clone(),
            None => return false,
        };

        let element_id = match self.element_registry.get_id(node_id) {
            Some(id) => id,
            None => return false,
        };

        // Resolve animation for the specific state
        if let Some(animation) =
            stylesheet.resolve_keyframe_animation_with_state(&element_id, state)
        {
            self.css_anim_store.lock().unwrap().animations.insert(
                node_id,
                crate::render_state::ActiveCssAnimation::new(animation),
            );
            return true;
        }

        false
    }

    /// Apply current CSS animation values to a node's render props
    ///
    /// Call this during rendering to apply animated values.
    pub fn apply_css_animation_to_props(&self, node_id: LayoutNodeId, props: &mut RenderProps) {
        let store = self.css_anim_store.lock().unwrap();
        if let Some(active_anim) = store.animations.get(&node_id) {
            let anim_props = &active_anim.current_properties;

            // Apply animated opacity
            if let Some(opacity) = anim_props.opacity {
                props.opacity = opacity;
            }

            // Apply animated transform components
            let has_transform = anim_props.translate_x.is_some()
                || anim_props.translate_y.is_some()
                || anim_props.scale_x.is_some()
                || anim_props.scale_y.is_some()
                || anim_props.rotate.is_some();

            if has_transform {
                use blinc_core::{Affine2D, Transform};

                // Build transform by composing: scale -> rotate -> translate
                let mut affine = Affine2D::IDENTITY;

                // Apply scale
                if let (Some(sx), Some(sy)) = (anim_props.scale_x, anim_props.scale_y) {
                    affine = affine.then(&Affine2D::scale(sx, sy));
                } else if let Some(sx) = anim_props.scale_x {
                    affine = affine.then(&Affine2D::scale(sx, 1.0));
                } else if let Some(sy) = anim_props.scale_y {
                    affine = affine.then(&Affine2D::scale(1.0, sy));
                }

                // Apply rotation (in degrees)
                if let Some(rotate) = anim_props.rotate {
                    affine = affine.then(&Affine2D::rotation(rotate.to_radians()));
                }

                // Apply translation
                if let (Some(tx), Some(ty)) = (anim_props.translate_x, anim_props.translate_y) {
                    affine = affine.then(&Affine2D::translation(tx, ty));
                } else if let Some(tx) = anim_props.translate_x {
                    affine = affine.then(&Affine2D::translation(tx, 0.0));
                } else if let Some(ty) = anim_props.translate_y {
                    affine = affine.then(&Affine2D::translation(0.0, ty));
                }

                props.transform = Some(Transform::Affine2D(affine));
            }
        }
    }

    /// Get current CSS animation properties for a node (if any)
    pub fn get_css_animation_properties(
        &self,
        node_id: LayoutNodeId,
    ) -> Option<blinc_animation::KeyframeProperties> {
        let store = self.css_anim_store.lock().unwrap();
        store
            .animations
            .get(&node_id)
            .map(|a| a.current_properties.clone())
    }

    /// Check if a node has an active CSS animation
    pub fn has_css_animation(&self, node_id: LayoutNodeId) -> bool {
        let store = self.css_anim_store.lock().unwrap();
        store
            .animations
            .get(&node_id)
            .map(|a| a.is_playing)
            .unwrap_or(false)
    }

    /// Stop CSS animation for a node
    pub fn stop_css_animation(&mut self, node_id: LayoutNodeId) {
        self.css_anim_store
            .lock()
            .unwrap()
            .animations
            .remove(&node_id);
    }

    /// Check if there are no active CSS animations
    pub fn css_animations_empty(&self) -> bool {
        self.css_anim_store.lock().unwrap().animations.is_empty()
    }

    /// Check if the CSS animation store has any active work (animations or transitions)
    pub fn css_has_active(&self) -> bool {
        let store = self.css_anim_store.lock().unwrap();
        store.has_active_animations() || store.has_active_transitions()
    }

    /// Start CSS animations for all registered elements that have animations defined
    ///
    /// Scans all registered element IDs, checks the stylesheet for animation properties,
    /// and starts `ActiveCssAnimation` instances for each matching element.
    pub fn start_all_css_animations(&mut self) {
        if self.stylesheet.is_none() {
            return;
        }

        let registered_ids: Vec<(String, LayoutNodeId)> = self
            .element_registry
            .all_ids()
            .into_iter()
            .filter_map(|id| self.element_registry.get(&id).map(|node_id| (id, node_id)))
            .collect();

        for (element_id, node_id) in &registered_ids {
            // Skip if already has an active animation (lock briefly, release before calling start)
            let already_has = self
                .css_anim_store
                .lock()
                .unwrap()
                .animations
                .contains_key(node_id);
            if already_has {
                continue;
            }
            let started = self.start_css_animation_for_element(*node_id);
            if started {
                tracing::debug!(
                    "CSS animation started for element '{}' (node={:?})",
                    element_id,
                    node_id
                );
            }
        }
    }

    /// Apply all active CSS animation values to their respective render props
    ///
    /// This mutates render props in-place with current animation values (opacity, transform).
    /// The background thread ticks animations; this reads the latest values and applies them.
    pub fn apply_all_css_animation_props(&mut self) {
        // Collect animation data under the lock, then release before modifying render_nodes
        let anim_data: Vec<(LayoutNodeId, blinc_animation::KeyframeProperties)> = {
            let store = self.css_anim_store.lock().unwrap();
            store
                .animations
                .iter()
                .map(|(nid, a)| (*nid, a.current_properties.clone()))
                .collect()
        };
        for (node_id, anim_props) in anim_data {
            if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                Self::apply_keyframe_props_to_render(&mut render_node.props, &anim_props);
            }
        }
    }

    /// Apply animated layout properties from CSS animations/transitions to taffy styles
    ///
    /// Returns true if any layout properties were modified (requiring layout recomputation).
    pub fn apply_animated_layout_props(&mut self) -> bool {
        use taffy::prelude::*;
        let mut needs_layout = false;

        // Collect all nodes with active animations or transitions that have layout properties
        let anim_nodes: Vec<(LayoutNodeId, blinc_animation::KeyframeProperties)> = {
            let store = self.css_anim_store.lock().unwrap();
            store
                .animations
                .iter()
                .map(|(nid, anim)| (*nid, anim.current_properties.clone()))
                .chain(
                    store
                        .transitions
                        .iter()
                        .map(|(nid, anim)| (*nid, anim.current_properties.clone())),
                )
                .collect()
        };

        for (node_id, anim_props) in anim_nodes {
            let has_layout_props = anim_props.width.is_some()
                || anim_props.height.is_some()
                || anim_props.padding.is_some()
                || anim_props.margin.is_some()
                || anim_props.gap.is_some()
                || anim_props.min_width.is_some()
                || anim_props.max_width.is_some()
                || anim_props.min_height.is_some()
                || anim_props.max_height.is_some()
                || anim_props.flex_grow.is_some()
                || anim_props.flex_shrink.is_some()
                || anim_props.inset_top.is_some()
                || anim_props.inset_right.is_some()
                || anim_props.inset_bottom.is_some()
                || anim_props.inset_left.is_some();

            if !has_layout_props {
                continue;
            }

            if let Some(mut style) = self.layout_tree.get_style(node_id) {
                let mut changed = false;

                if let Some(w) = anim_props.width {
                    style.size.width = Dimension::Length(w);
                    changed = true;
                }
                if let Some(h) = anim_props.height {
                    style.size.height = Dimension::Length(h);
                    changed = true;
                }
                if let Some(v) = anim_props.min_width {
                    style.min_size.width = Dimension::Length(v);
                    changed = true;
                }
                if let Some(v) = anim_props.max_width {
                    style.max_size.width = Dimension::Length(v);
                    changed = true;
                }
                if let Some(v) = anim_props.min_height {
                    style.min_size.height = Dimension::Length(v);
                    changed = true;
                }
                if let Some(v) = anim_props.max_height {
                    style.max_size.height = Dimension::Length(v);
                    changed = true;
                }
                if let Some([top, right, bottom, left]) = anim_props.padding {
                    style.padding = taffy::geometry::Rect {
                        top: LengthPercentage::Length(top),
                        right: LengthPercentage::Length(right),
                        bottom: LengthPercentage::Length(bottom),
                        left: LengthPercentage::Length(left),
                    };
                    changed = true;
                }
                if let Some([top, right, bottom, left]) = anim_props.margin {
                    style.margin = taffy::geometry::Rect {
                        top: LengthPercentageAuto::Length(top),
                        right: LengthPercentageAuto::Length(right),
                        bottom: LengthPercentageAuto::Length(bottom),
                        left: LengthPercentageAuto::Length(left),
                    };
                    changed = true;
                }
                if let Some(g) = anim_props.gap {
                    style.gap = taffy::geometry::Size {
                        width: LengthPercentage::Length(g),
                        height: LengthPercentage::Length(g),
                    };
                    changed = true;
                }
                if let Some(v) = anim_props.flex_grow {
                    style.flex_grow = v;
                    changed = true;
                }
                if let Some(v) = anim_props.flex_shrink {
                    style.flex_shrink = v;
                    changed = true;
                }
                if anim_props.inset_top.is_some()
                    || anim_props.inset_right.is_some()
                    || anim_props.inset_bottom.is_some()
                    || anim_props.inset_left.is_some()
                {
                    if let Some(v) = anim_props.inset_top {
                        style.inset.top = LengthPercentageAuto::Length(v);
                    }
                    if let Some(v) = anim_props.inset_right {
                        style.inset.right = LengthPercentageAuto::Length(v);
                    }
                    if let Some(v) = anim_props.inset_bottom {
                        style.inset.bottom = LengthPercentageAuto::Length(v);
                    }
                    if let Some(v) = anim_props.inset_left {
                        style.inset.left = LengthPercentageAuto::Length(v);
                    }
                    changed = true;
                }

                if changed {
                    self.layout_tree.set_style(node_id, style);
                    needs_layout = true;
                }
            }
        }

        needs_layout
    }

    // =========================================================================
    // CSS Transition Methods
    // =========================================================================

    /// Detect property changes and start transitions where applicable
    ///
    /// Compares before/after keyframe properties, finds changed properties that
    /// have a matching `transition:` spec, and creates a 2-keyframe animation
    /// for each transitioning property group.
    fn detect_and_start_transitions(
        &mut self,
        node_id: LayoutNodeId,
        before: &blinc_animation::KeyframeProperties,
        after: &blinc_animation::KeyframeProperties,
        transition_set: &crate::css_parser::CssTransitionSet,
    ) {
        use blinc_animation::{
            FillMode, KeyframeProperties, MultiKeyframe, MultiKeyframeAnimation,
        };

        // Lock the shared store for the duration of this method
        let mut store = self.css_anim_store.lock().unwrap();

        // Build "from" properties: only include fields that differ AND have a transition spec
        let mut from = KeyframeProperties::default();
        let mut to = KeyframeProperties::default();
        let mut has_any = false;
        let mut duration_ms: u32 = 300;
        let mut delay_ms: u32 = 0;
        let mut easing = blinc_animation::Easing::EaseInOut;

        // Helper macro: check if a property changed and is covered by transition
        macro_rules! check_transition {
            ($field:ident, $prop_name:expr) => {
                if before.$field != after.$field {
                    if let Some(t) = transition_set.get($prop_name) {
                        // If a transition is already active, use current interpolated value as "from"
                        let from_val = if let Some(active) = store.transitions.get(&node_id) {
                            if active.current_properties.$field.is_some() {
                                active.current_properties.$field.clone()
                            } else {
                                before.$field.clone()
                            }
                        } else {
                            before.$field.clone()
                        };
                        from.$field = from_val;
                        to.$field = after.$field.clone();
                        duration_ms = t.duration_ms;
                        delay_ms = t.delay_ms;
                        easing = t.timing.to_easing();
                        has_any = true;
                    }
                }
            };
            // Variant with an identity default — used for filter properties where
            // None means "use default" (e.g. brightness=1.0) rather than "not set"
            ($field:ident, $prop_name:expr, default $def:expr) => {
                if before.$field != after.$field {
                    if let Some(t) = transition_set.get($prop_name) {
                        let from_val = if let Some(active) = store.transitions.get(&node_id) {
                            if active.current_properties.$field.is_some() {
                                active.current_properties.$field.clone()
                            } else {
                                Some(before.$field.unwrap_or($def))
                            }
                        } else {
                            Some(before.$field.unwrap_or($def))
                        };
                        from.$field = from_val;
                        to.$field = Some(after.$field.unwrap_or($def));
                        duration_ms = t.duration_ms;
                        delay_ms = t.delay_ms;
                        easing = t.timing.to_easing();
                        has_any = true;
                    }
                }
            };
        }

        check_transition!(opacity, "opacity");
        check_transition!(background_color, "background");
        check_transition!(gradient_start_color, "background");
        check_transition!(gradient_end_color, "background");
        check_transition!(gradient_angle, "background");
        check_transition!(text_color, "color");
        check_transition!(text_shadow_params, "text-shadow");
        check_transition!(text_shadow_color, "text-shadow");
        check_transition!(font_size, "font-size");
        check_transition!(border_color, "border-color");
        check_transition!(border_width, "border-width");
        check_transition!(outline_color, "outline-color");
        check_transition!(outline_width, "outline-width");
        check_transition!(outline_offset, "outline-offset");
        check_transition!(corner_radius, "border-radius");
        check_transition!(corner_shape, "corner-shape");
        check_transition!(overflow_fade, "overflow-fade");
        check_transition!(shadow_params, "box-shadow");
        check_transition!(shadow_color, "box-shadow");
        check_transition!(clip_inset, "clip-path");
        check_transition!(clip_circle_radius, "clip-path");
        check_transition!(clip_ellipse_radii, "clip-path");
        check_transition!(translate_x, "transform", default 0.0);
        check_transition!(translate_y, "transform", default 0.0);
        check_transition!(scale_x, "transform", default 1.0);
        check_transition!(scale_y, "transform", default 1.0);
        check_transition!(rotate, "transform", default 0.0);
        check_transition!(rotate_x, "rotate-x", default 0.0);
        check_transition!(rotate_y, "rotate-y", default 0.0);
        check_transition!(perspective, "perspective");
        check_transition!(depth, "depth");
        check_transition!(translate_z, "translate-z", default 0.0);
        check_transition!(light_intensity, "light-intensity");
        check_transition!(ambient, "ambient");
        check_transition!(specular, "specular");
        check_transition!(light_direction, "light-direction");
        check_transition!(filter_grayscale, "filter", default 0.0);
        check_transition!(filter_invert, "filter", default 0.0);
        check_transition!(filter_sepia, "filter", default 0.0);
        check_transition!(filter_brightness, "filter", default 1.0);
        check_transition!(filter_contrast, "filter", default 1.0);
        check_transition!(filter_saturate, "filter", default 1.0);
        check_transition!(filter_hue_rotate, "filter", default 0.0);
        check_transition!(filter_blur, "filter", default 0.0);

        // Backdrop filter
        check_transition!(backdrop_blur, "backdrop-filter", default 0.0);
        check_transition!(backdrop_saturation, "backdrop-filter", default 1.0);
        check_transition!(backdrop_brightness, "backdrop-filter", default 1.0);

        // Layout properties (require layout recomputation when transitioning)
        check_transition!(width, "width");
        check_transition!(height, "height");
        check_transition!(padding, "padding");
        check_transition!(margin, "margin");
        check_transition!(gap, "gap");
        check_transition!(min_width, "min-width");
        check_transition!(max_width, "max-width");
        check_transition!(min_height, "min-height");
        check_transition!(max_height, "max-height");
        check_transition!(flex_grow, "flex-grow");
        check_transition!(flex_shrink, "flex-shrink");
        check_transition!(inset_top, "top");
        check_transition!(inset_right, "right");
        check_transition!(inset_bottom, "bottom");
        check_transition!(inset_left, "left");
        check_transition!(z_index, "z-index");
        check_transition!(skew_x, "transform", default 0.0);
        check_transition!(skew_y, "transform", default 0.0);
        check_transition!(transform_origin, "transform-origin");

        // Mask gradient
        check_transition!(mask_gradient, "mask-image");

        // SVG properties
        check_transition!(svg_fill, "fill");
        check_transition!(svg_stroke, "stroke");
        check_transition!(svg_stroke_width, "stroke-width");
        check_transition!(svg_stroke_dashoffset, "stroke-dashoffset");

        if has_any && duration_ms > 0 {
            // If a transition already exists heading to the same target, let it continue
            // rather than restarting with a fresh duration each frame
            if let Some(existing) = store.transitions.get(&node_id) {
                if let Some(last_kf) = existing.animation.last_keyframe() {
                    if last_kf.properties == to {
                        return; // Same target — let existing transition finish
                    }
                }
            }
            let anim = MultiKeyframeAnimation::new(duration_ms)
                .keyframe(0.0, from, easing)
                .keyframe(1.0, to, easing)
                .delay(delay_ms)
                .fill_mode(FillMode::Forwards);
            store
                .transitions
                .insert(node_id, crate::render_state::ActiveCssAnimation::new(anim));
        }
    }

    /// Apply all active CSS transition values to their respective render props
    ///
    /// The background thread ticks transitions; this reads the latest values and applies them.
    pub fn apply_all_css_transition_props(&mut self) {
        // Collect transition data under the lock, then release before modifying render_nodes
        let trans_data: Vec<(LayoutNodeId, blinc_animation::KeyframeProperties)> = {
            let store = self.css_anim_store.lock().unwrap();
            store
                .transitions
                .iter()
                .map(|(nid, a)| (*nid, a.current_properties.clone()))
                .collect()
        };
        for (node_id, anim_props) in trans_data {
            if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                Self::apply_keyframe_props_to_render(&mut render_node.props, &anim_props);
            }
        }
    }

    /// Check if there are no active CSS transitions
    pub fn css_transitions_empty(&self) -> bool {
        self.css_anim_store.lock().unwrap().transitions.is_empty()
    }

    /// Remove completed transitions from the store.
    /// Must be called AFTER `apply_all_css_transition_props()`.
    pub fn remove_completed_transitions(&mut self) {
        self.css_anim_store
            .lock()
            .unwrap()
            .remove_completed_transitions();
    }

    /// Rebuild gradient stops, interpolating the first and last stop colors
    /// while preserving the positions of any intermediate stops.
    fn rebuild_two_stop_gradient(
        existing_stops: &[blinc_core::GradientStop],
        start_color: blinc_core::Color,
        end_color: blinc_core::Color,
    ) -> Vec<blinc_core::GradientStop> {
        if existing_stops.len() <= 2 {
            vec![
                blinc_core::GradientStop::new(0.0, start_color),
                blinc_core::GradientStop::new(1.0, end_color),
            ]
        } else {
            // Keep intermediate stops but lerp their colors proportionally
            let mut new_stops = Vec::with_capacity(existing_stops.len());
            for stop in existing_stops {
                let t = stop.offset;
                let c = blinc_core::Color::rgba(
                    start_color.r + (end_color.r - start_color.r) * t,
                    start_color.g + (end_color.g - start_color.g) * t,
                    start_color.b + (end_color.b - start_color.b) * t,
                    start_color.a + (end_color.a - start_color.a) * t,
                );
                new_stops.push(blinc_core::GradientStop::new(stop.offset, c));
            }
            new_stops
        }
    }

    /// Apply keyframe animation properties to render props
    fn apply_keyframe_props_to_render(
        props: &mut RenderProps,
        anim_props: &blinc_animation::KeyframeProperties,
    ) {
        if let Some(opacity) = anim_props.opacity {
            props.opacity = opacity;
        }

        let has_transform = anim_props.translate_x.is_some()
            || anim_props.translate_y.is_some()
            || anim_props.scale_x.is_some()
            || anim_props.scale_y.is_some()
            || anim_props.rotate.is_some()
            || anim_props.skew_x.is_some()
            || anim_props.skew_y.is_some();

        if has_transform {
            use blinc_core::{Affine2D, Transform};

            let mut affine = Affine2D::IDENTITY;

            if let (Some(sx), Some(sy)) = (anim_props.scale_x, anim_props.scale_y) {
                affine = affine.then(&Affine2D::scale(sx, sy));
            } else if let Some(sx) = anim_props.scale_x {
                affine = affine.then(&Affine2D::scale(sx, 1.0));
            } else if let Some(sy) = anim_props.scale_y {
                affine = affine.then(&Affine2D::scale(1.0, sy));
            }

            if let Some(rotate) = anim_props.rotate {
                affine = affine.then(&Affine2D::rotation(rotate.to_radians()));
            }

            // Skew
            if let Some(skx) = anim_props.skew_x {
                affine = affine.then(&Affine2D::skew_x(skx.to_radians()));
            }
            if let Some(sky) = anim_props.skew_y {
                affine = affine.then(&Affine2D::skew_y(sky.to_radians()));
            }

            if let (Some(tx), Some(ty)) = (anim_props.translate_x, anim_props.translate_y) {
                affine = affine.then(&Affine2D::translation(tx, ty));
            } else if let Some(tx) = anim_props.translate_x {
                affine = affine.then(&Affine2D::translation(tx, 0.0));
            } else if let Some(ty) = anim_props.translate_y {
                affine = affine.then(&Affine2D::translation(0.0, ty));
            }

            props.transform = Some(Transform::Affine2D(affine));
        }

        // 3D animation properties
        if let Some(rx) = anim_props.rotate_x {
            props.rotate_x = Some(rx);
        }
        if let Some(ry) = anim_props.rotate_y {
            props.rotate_y = Some(ry);
        }
        if let Some(p) = anim_props.perspective {
            props.perspective = Some(p);
        }
        if let Some(d) = anim_props.depth {
            props.depth = Some(d);
        }
        if let Some(tz) = anim_props.translate_z {
            props.translate_z = Some(tz);
        }
        if let Some(b) = anim_props.blend_3d {
            props.blend_3d = Some(b);
        }

        // Clip-path animation
        if let Some(inset) = &anim_props.clip_inset {
            use blinc_core::{ClipLength, ClipPath};
            props.clip_path = Some(ClipPath::Inset {
                top: ClipLength::Percent(inset[0]),
                right: ClipLength::Percent(inset[1]),
                bottom: ClipLength::Percent(inset[2]),
                left: ClipLength::Percent(inset[3]),
                round: None,
            });
        }
        if let Some(r) = anim_props.clip_circle_radius {
            use blinc_core::{ClipLength, ClipPath};
            props.clip_path = Some(ClipPath::Circle {
                radius: Some(ClipLength::Percent(r)),
                center: (ClipLength::Percent(50.0), ClipLength::Percent(50.0)),
            });
        }
        if let Some(radii) = &anim_props.clip_ellipse_radii {
            use blinc_core::{ClipLength, ClipPath};
            props.clip_path = Some(ClipPath::Ellipse {
                rx: Some(ClipLength::Percent(radii[0])),
                ry: Some(ClipLength::Percent(radii[1])),
                center: (ClipLength::Percent(50.0), ClipLength::Percent(50.0)),
            });
        }

        // Background color (solid)
        if let Some([r, g, b, a]) = anim_props.background_color {
            props.background = Some(blinc_core::Brush::Solid(blinc_core::Color::rgba(
                r, g, b, a,
            )));
        }

        // Gradient color stops animation
        if anim_props.gradient_start_color.is_some() || anim_props.gradient_end_color.is_some() {
            // Derive fallback colors from existing gradient stops to avoid
            // flashing to black/white when only one color is in the transition
            let (existing_start, existing_end) = match &props.background {
                Some(blinc_core::Brush::Gradient(g)) => {
                    let stops = g.stops();
                    let s = stops
                        .first()
                        .map(|s| [s.color.r, s.color.g, s.color.b, s.color.a]);
                    let e = stops
                        .last()
                        .map(|s| [s.color.r, s.color.g, s.color.b, s.color.a]);
                    (
                        s.unwrap_or([0.0, 0.0, 0.0, 1.0]),
                        e.unwrap_or([0.0, 0.0, 0.0, 1.0]),
                    )
                }
                _ => ([0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0, 1.0]),
            };
            let start_color = anim_props.gradient_start_color.unwrap_or(existing_start);
            let end_color = anim_props.gradient_end_color.unwrap_or(existing_end);
            let sc = blinc_core::Color::rgba(
                start_color[0],
                start_color[1],
                start_color[2],
                start_color[3],
            );
            let ec =
                blinc_core::Color::rgba(end_color[0], end_color[1], end_color[2], end_color[3]);

            // Reconstruct gradient preserving the existing type if possible
            match &props.background {
                Some(blinc_core::Brush::Gradient(existing)) => {
                    let new_gradient = match existing {
                        blinc_core::Gradient::Linear {
                            start,
                            end,
                            stops,
                            space,
                            spread,
                        } => {
                            let (start_pt, end_pt) = if let Some(angle) = anim_props.gradient_angle
                            {
                                crate::css_parser::angle_to_gradient_points(angle)
                            } else {
                                (*start, *end)
                            };
                            let new_stops = Self::rebuild_two_stop_gradient(stops, sc, ec);
                            blinc_core::Gradient::Linear {
                                start: start_pt,
                                end: end_pt,
                                stops: new_stops,
                                space: *space,
                                spread: *spread,
                            }
                        }
                        blinc_core::Gradient::Radial {
                            center,
                            radius,
                            focal,
                            stops,
                            space,
                            spread,
                        } => {
                            let new_stops = Self::rebuild_two_stop_gradient(stops, sc, ec);
                            blinc_core::Gradient::Radial {
                                center: *center,
                                radius: *radius,
                                focal: *focal,
                                stops: new_stops,
                                space: *space,
                                spread: *spread,
                            }
                        }
                        blinc_core::Gradient::Conic {
                            center,
                            start_angle,
                            stops,
                            space,
                        } => {
                            let new_stops = Self::rebuild_two_stop_gradient(stops, sc, ec);
                            blinc_core::Gradient::Conic {
                                center: *center,
                                start_angle: *start_angle,
                                stops: new_stops,
                                space: *space,
                            }
                        }
                    };
                    props.background = Some(blinc_core::Brush::Gradient(new_gradient));
                }
                _ => {
                    // No existing gradient — create a linear from the animated angle
                    let angle = anim_props.gradient_angle.unwrap_or(180.0);
                    let (start_pt, end_pt) = crate::css_parser::angle_to_gradient_points(angle);
                    props.background =
                        Some(blinc_core::Brush::Gradient(blinc_core::Gradient::Linear {
                            start: start_pt,
                            end: end_pt,
                            stops: vec![
                                blinc_core::GradientStop::new(0.0, sc),
                                blinc_core::GradientStop::new(1.0, ec),
                            ],
                            space: blinc_core::GradientSpace::ObjectBoundingBox,
                            spread: blinc_core::GradientSpread::Pad,
                        }));
                }
            }
        }

        // Text color
        if let Some(tc) = anim_props.text_color {
            props.text_color = Some(tc);
        }

        // Text shadow
        if let Some([ox, oy, blur, spread]) = anim_props.text_shadow_params {
            let color = anim_props
                .text_shadow_color
                .map(|[r, g, b, a]| blinc_core::Color::rgba(r, g, b, a))
                .or_else(|| props.text_shadow.as_ref().map(|s| s.color))
                .unwrap_or(blinc_core::Color::rgba(0.0, 0.0, 0.0, 0.5));
            props.text_shadow = Some(Shadow {
                offset_x: ox,
                offset_y: oy,
                blur,
                spread,
                color,
            });
        } else if let Some([r, g, b, a]) = anim_props.text_shadow_color {
            if let Some(ts) = &mut props.text_shadow {
                ts.color = blinc_core::Color::rgba(r, g, b, a);
            }
        }

        // Font size
        if let Some(fs) = anim_props.font_size {
            props.font_size = Some(fs);
        }

        // Corner radius
        if let Some([tl, tr, br, bl]) = anim_props.corner_radius {
            props.border_radius = blinc_core::CornerRadius {
                top_left: tl,
                top_right: tr,
                bottom_right: br,
                bottom_left: bl,
            };
        }

        // Corner shape (superellipse)
        if let Some([tl, tr, br, bl]) = anim_props.corner_shape {
            props.corner_shape = blinc_core::CornerShape::new(tl, tr, br, bl);
        }

        // Overflow fade
        if let Some([t, r, b, l]) = anim_props.overflow_fade {
            props.overflow_fade = blinc_core::OverflowFade::new(t, r, b, l);
        }

        // Border
        if let Some(bw) = anim_props.border_width {
            props.border_width = bw;
        }
        if let Some([r, g, b, a]) = anim_props.border_color {
            props.border_color = Some(blinc_core::Color::rgba(r, g, b, a));
        }

        // Outline
        if let Some(ow) = anim_props.outline_width {
            props.outline_width = ow;
        }
        if let Some([r, g, b, a]) = anim_props.outline_color {
            props.outline_color = Some(blinc_core::Color::rgba(r, g, b, a));
        }
        if let Some(offset) = anim_props.outline_offset {
            props.outline_offset = offset;
        }

        // Shadow
        if let Some([ox, oy, blur, spread]) = anim_props.shadow_params {
            let color = if let Some([r, g, b, a]) = anim_props.shadow_color {
                blinc_core::Color::rgba(r, g, b, a)
            } else if let Some(ref existing) = props.shadow {
                existing.color
            } else {
                blinc_core::Color::rgba(0.0, 0.0, 0.0, 0.5)
            };
            props.shadow = Some(blinc_core::Shadow {
                offset_x: ox,
                offset_y: oy,
                blur,
                spread,
                color,
            });
        } else if let Some([r, g, b, a]) = anim_props.shadow_color {
            if let Some(ref mut shadow) = props.shadow {
                shadow.color = blinc_core::Color::rgba(r, g, b, a);
            }
        }

        // 3D lighting
        if let Some(li) = anim_props.light_intensity {
            props.light_intensity = Some(li);
        }
        if let Some(a) = anim_props.ambient {
            props.ambient = Some(a);
        }
        if let Some(s) = anim_props.specular {
            props.specular = Some(s);
        }
        if let Some(ld) = anim_props.light_direction {
            props.light_direction = Some(ld);
        }

        // CSS filter properties
        let has_filter = anim_props.filter_grayscale.is_some()
            || anim_props.filter_invert.is_some()
            || anim_props.filter_sepia.is_some()
            || anim_props.filter_brightness.is_some()
            || anim_props.filter_contrast.is_some()
            || anim_props.filter_saturate.is_some()
            || anim_props.filter_hue_rotate.is_some()
            || anim_props.filter_blur.is_some();
        if has_filter {
            let existing = props.filter.unwrap_or_default();
            let blur = anim_props.filter_blur.unwrap_or(existing.blur);
            props.filter = Some(crate::element_style::CssFilter {
                grayscale: anim_props.filter_grayscale.unwrap_or(existing.grayscale),
                invert: anim_props.filter_invert.unwrap_or(existing.invert),
                sepia: anim_props.filter_sepia.unwrap_or(existing.sepia),
                hue_rotate: anim_props.filter_hue_rotate.unwrap_or(existing.hue_rotate),
                brightness: anim_props.filter_brightness.unwrap_or(existing.brightness),
                contrast: anim_props.filter_contrast.unwrap_or(existing.contrast),
                saturate: anim_props.filter_saturate.unwrap_or(existing.saturate),
                blur,
                drop_shadow: existing.drop_shadow,
            });
            // Update LayerEffect for blur
            if blur > 0.0 {
                props
                    .layer_effects
                    .retain(|e| !matches!(e, LayerEffect::Blur { .. }));
                props.layer_effects.push(LayerEffect::Blur {
                    radius: blur,
                    quality: BlurQuality::Medium,
                });
            } else {
                props
                    .layer_effects
                    .retain(|e| !matches!(e, LayerEffect::Blur { .. }));
            }
        }

        // Backdrop filter (glass material)
        let has_backdrop = anim_props.backdrop_blur.is_some()
            || anim_props.backdrop_saturation.is_some()
            || anim_props.backdrop_brightness.is_some();
        if has_backdrop {
            let existing = match &props.material {
                Some(Material::Glass(g)) => g.clone(),
                _ => GlassMaterial {
                    blur: 0.0,
                    tint: blinc_core::Color::rgba(1.0, 1.0, 1.0, 0.1),
                    saturation: 1.0,
                    brightness: 1.0,
                    noise: 0.0,
                    border_thickness: 0.0,
                    shadow: None,
                    simple: true,
                },
            };
            let mut glass = existing;
            if let Some(b) = anim_props.backdrop_blur {
                glass.blur = b;
            }
            if let Some(s) = anim_props.backdrop_saturation {
                glass.saturation = s;
            }
            if let Some(br) = anim_props.backdrop_brightness {
                glass.brightness = br;
            }
            props.material = Some(Material::Glass(glass));
            props.layer = RenderLayer::Glass;
        }

        // z-index (round from f32 to i32)
        if let Some(z) = anim_props.z_index {
            props.z_index = z.round() as i32;
        }

        // Transform origin
        if let Some(to) = anim_props.transform_origin {
            props.transform_origin = Some(to);
        }

        // Mask gradient: reconstruct MaskImage::Gradient from combined [f32; 8]
        if let Some(mg) = anim_props.mask_gradient {
            let mask_type = mg[0];
            let start_alpha = mg[1];
            let end_alpha = mg[2];
            let gradient = if mask_type < 1.5 {
                blinc_core::Gradient::Linear {
                    start: blinc_core::Point::new(mg[4], mg[5]),
                    end: blinc_core::Point::new(mg[6], mg[7]),
                    stops: vec![
                        blinc_core::GradientStop::new(
                            0.0,
                            blinc_core::Color::rgba(0.0, 0.0, 0.0, start_alpha),
                        ),
                        blinc_core::GradientStop::new(
                            1.0,
                            blinc_core::Color::rgba(0.0, 0.0, 0.0, end_alpha),
                        ),
                    ],
                    space: blinc_core::GradientSpace::ObjectBoundingBox,
                    spread: blinc_core::GradientSpread::Pad,
                }
            } else {
                blinc_core::Gradient::Radial {
                    center: blinc_core::Point::new(mg[4], mg[5]),
                    radius: mg[6],
                    focal: None,
                    stops: vec![
                        blinc_core::GradientStop::new(
                            0.0,
                            blinc_core::Color::rgba(0.0, 0.0, 0.0, start_alpha),
                        ),
                        blinc_core::GradientStop::new(
                            1.0,
                            blinc_core::Color::rgba(0.0, 0.0, 0.0, end_alpha),
                        ),
                    ],
                    space: blinc_core::GradientSpace::ObjectBoundingBox,
                    spread: blinc_core::GradientSpread::Pad,
                }
            };
            props.mask_image = Some(blinc_core::MaskImage::Gradient(gradient));
        }

        // SVG properties
        if let Some([r, g, b, a]) = anim_props.svg_fill {
            props.fill = Some([r, g, b, a]);
        }
        if let Some([r, g, b, a]) = anim_props.svg_stroke {
            props.stroke = Some([r, g, b, a]);
        }
        if let Some(sw) = anim_props.svg_stroke_width {
            props.stroke_width = Some(sw);
        }
        if let Some(offset) = anim_props.svg_stroke_dashoffset {
            props.stroke_dashoffset = Some(offset);
        }
        if let Some(ref path_data) = anim_props.svg_path_data {
            props.svg_path_data = Some(path_data.clone());
        }
    }

    /// Extract animatable properties from RenderProps into KeyframeProperties
    ///
    /// This is the reverse of `apply_keyframe_props_to_render()` — used to snapshot
    /// the current visual state before/after a state change for transition detection.
    fn render_props_to_keyframe_properties(
        props: &RenderProps,
    ) -> blinc_animation::KeyframeProperties {
        use blinc_animation::KeyframeProperties;

        let mut kp = KeyframeProperties {
            opacity: Some(props.opacity),
            ..Default::default()
        };

        // Extract transform components via QR decomposition
        // Decomposes the 2x2 portion [a,c; b,d] into scale, rotation, and skew.
        if let Some(blinc_core::Transform::Affine2D(affine)) = &props.transform {
            let [a, b, c, d, tx, ty] = affine.elements;
            kp.translate_x = Some(tx);
            kp.translate_y = Some(ty);

            // QR decomposition of the 2x2 matrix:
            // Step 1: scale_x = length of first column
            let scale_x = (a * a + b * b).sqrt().max(1e-6);
            // Step 2: rotation from normalized first column
            let rotation = b.atan2(a);
            kp.rotate = Some(rotation.to_degrees());
            // Step 3: XY shear = dot(normalized_col0, col1)
            let skew_xy = (a * c + b * d) / (scale_x * scale_x);
            // Step 4: scale_y = det / scale_x (preserves sign)
            let det = a * d - b * c;
            let scale_y = det / scale_x;

            kp.scale_x = Some(scale_x);
            kp.scale_y = Some(scale_y);

            // Only set skew if non-trivial
            if skew_xy.abs() > 0.0001 {
                kp.skew_x = Some(skew_xy.atan().to_degrees());
            }
        } else {
            kp.translate_x = Some(0.0);
            kp.translate_y = Some(0.0);
            kp.scale_x = Some(1.0);
            kp.scale_y = Some(1.0);
            kp.rotate = Some(0.0);
        }

        // 3D
        kp.rotate_x = props.rotate_x;
        kp.rotate_y = props.rotate_y;
        kp.perspective = props.perspective;
        kp.depth = props.depth;
        kp.translate_z = props.translate_z;
        kp.blend_3d = props.blend_3d;

        // Clip-path
        match &props.clip_path {
            Some(blinc_core::ClipPath::Inset {
                top,
                right,
                bottom,
                left,
                ..
            }) => {
                kp.clip_inset = Some([
                    Self::clip_length_to_percent(top),
                    Self::clip_length_to_percent(right),
                    Self::clip_length_to_percent(bottom),
                    Self::clip_length_to_percent(left),
                ]);
            }
            Some(blinc_core::ClipPath::Circle {
                radius: Some(r), ..
            }) => {
                kp.clip_circle_radius = Some(Self::clip_length_to_percent(r));
            }
            Some(blinc_core::ClipPath::Ellipse {
                rx: Some(rx),
                ry: Some(ry),
                ..
            }) => {
                kp.clip_ellipse_radii = Some([
                    Self::clip_length_to_percent(rx),
                    Self::clip_length_to_percent(ry),
                ]);
            }
            _ => {}
        }

        // Background color (solid or gradient)
        match &props.background {
            Some(blinc_core::Brush::Solid(c)) => {
                kp.background_color = Some([c.r, c.g, c.b, c.a]);
            }
            Some(blinc_core::Brush::Gradient(gradient)) => {
                let stops = gradient.stops();
                if let Some(first) = stops.first() {
                    kp.gradient_start_color =
                        Some([first.color.r, first.color.g, first.color.b, first.color.a]);
                }
                if let Some(last) = stops.last() {
                    kp.gradient_end_color =
                        Some([last.color.r, last.color.g, last.color.b, last.color.a]);
                }
                if let blinc_core::Gradient::Linear { start, end, .. } = gradient {
                    kp.gradient_angle =
                        Some(crate::css_parser::gradient_points_to_angle(*start, *end));
                }
            }
            _ => {}
        }

        // Text color
        if let Some(tc) = &props.text_color {
            kp.text_color = Some(*tc);
        }

        // Text shadow
        if let Some(ts) = &props.text_shadow {
            kp.text_shadow_params = Some([ts.offset_x, ts.offset_y, ts.blur, ts.spread]);
            kp.text_shadow_color = Some([ts.color.r, ts.color.g, ts.color.b, ts.color.a]);
        }

        // Font size
        kp.font_size = props.font_size;

        // Corner radius
        let cr = &props.border_radius;
        kp.corner_radius = Some([cr.top_left, cr.top_right, cr.bottom_right, cr.bottom_left]);

        // Corner shape (superellipse) — always snapshot for transitions
        kp.corner_shape = Some(props.corner_shape.to_array());

        // Overflow fade — always snapshot for transitions
        kp.overflow_fade = Some(props.overflow_fade.to_array());

        // Border
        kp.border_width = Some(props.border_width);
        if let Some(bc) = &props.border_color {
            kp.border_color = Some([bc.r, bc.g, bc.b, bc.a]);
        }

        // Outline
        kp.outline_width = Some(props.outline_width);
        if let Some(oc) = &props.outline_color {
            kp.outline_color = Some([oc.r, oc.g, oc.b, oc.a]);
        }
        kp.outline_offset = Some(props.outline_offset);

        // Shadow
        if let Some(shadow) = &props.shadow {
            kp.shadow_params = Some([shadow.offset_x, shadow.offset_y, shadow.blur, shadow.spread]);
            kp.shadow_color = Some([
                shadow.color.r,
                shadow.color.g,
                shadow.color.b,
                shadow.color.a,
            ]);
        }

        // 3D lighting
        kp.light_intensity = props.light_intensity;
        kp.ambient = props.ambient;
        kp.specular = props.specular;
        kp.light_direction = props.light_direction;

        // CSS filter properties
        if let Some(f) = &props.filter {
            kp.filter_grayscale = Some(f.grayscale);
            kp.filter_invert = Some(f.invert);
            kp.filter_sepia = Some(f.sepia);
            kp.filter_brightness = Some(f.brightness);
            kp.filter_contrast = Some(f.contrast);
            kp.filter_saturate = Some(f.saturate);
            kp.filter_hue_rotate = Some(f.hue_rotate);
            kp.filter_blur = Some(f.blur);
        }

        // Backdrop filter (glass material)
        if let Some(Material::Glass(glass)) = &props.material {
            kp.backdrop_blur = Some(glass.blur);
            kp.backdrop_saturation = Some(glass.saturation);
            kp.backdrop_brightness = Some(glass.brightness);
        }

        // z-index (as f32 for smooth interpolation)
        if props.z_index != 0 {
            kp.z_index = Some(props.z_index as f32);
        }

        // Transform origin
        kp.transform_origin = props.transform_origin;

        // Mask gradient (combined [mask_type, start_alpha, end_alpha, 0, p0, p1, p2, p3])
        if let Some(blinc_core::MaskImage::Gradient(ref gradient)) = props.mask_image {
            let lum = matches!(props.mask_mode, Some(blinc_core::MaskMode::Luminance));
            kp.mask_gradient = Some(match gradient {
                blinc_core::Gradient::Linear {
                    start, end, stops, ..
                } => {
                    let (sa, ea) = Self::extract_mask_alphas(stops, lum);
                    [1.0, sa, ea, 0.0, start.x, start.y, end.x, end.y]
                }
                blinc_core::Gradient::Radial {
                    center,
                    radius,
                    stops,
                    ..
                } => {
                    let (sa, ea) = Self::extract_mask_alphas(stops, lum);
                    [2.0, sa, ea, 0.0, center.x, center.y, *radius, 0.0]
                }
                blinc_core::Gradient::Conic { center, stops, .. } => {
                    let (sa, ea) = Self::extract_mask_alphas(stops, lum);
                    [2.0, sa, ea, 0.0, center.x, center.y, 0.5, 0.0]
                }
            });
        }

        // SVG properties
        if let Some(fill) = &props.fill {
            kp.svg_fill = Some(*fill);
        }
        if let Some(stroke) = &props.stroke {
            kp.svg_stroke = Some(*stroke);
        }
        if let Some(sw) = props.stroke_width {
            kp.svg_stroke_width = Some(sw);
        }
        if let Some(offset) = props.stroke_dashoffset {
            kp.svg_stroke_dashoffset = Some(offset);
        }
        if let Some(ref path_data) = props.svg_path_data {
            kp.svg_path_data = Some(path_data.clone());
        }

        kp
    }

    /// Helper: convert ClipLength to percent value
    fn clip_length_to_percent(len: &blinc_core::ClipLength) -> f32 {
        match len {
            blinc_core::ClipLength::Percent(p) => *p,
            blinc_core::ClipLength::Px(px) => *px,
        }
    }

    /// Apply base stylesheet styles to all registered elements
    ///
    /// This must be called after `set_stylesheet_arc()` when the stylesheet
    /// was set AFTER tree construction. During tree build, `collect_render_props`
    /// applies base styles if the stylesheet is already set. But when the stylesheet
    /// is set after `from_element_with_registry()`, the base styles (background,
    /// border-radius, opacity, etc.) are missing. This method fixes that by
    /// iterating all registered elements and applying their base CSS styles.
    pub fn apply_stylesheet_base_styles(&mut self) {
        let stylesheet = match &self.stylesheet {
            Some(s) => s.clone(),
            None => return,
        };

        // CSS specificity order: type(0,0,1) < class(0,1,0) < id(1,0,0)
        // Apply complex base rules FIRST (lower specificity: type, class selectors)
        // sorted by ascending specificity so higher-specificity rules overwrite lower.
        // Then apply simple ID rules LAST (highest specificity, always override).
        let complex_rules = stylesheet.complex_rules();
        if !complex_rules.is_empty() {
            let empty_set = std::collections::HashSet::new();

            // Collect non-state rules and sort by specificity (ascending)
            let mut base_rules: Vec<&(
                crate::css_parser::ComplexSelector,
                crate::element_style::ElementStyle,
            )> = complex_rules
                .iter()
                .filter(|(selector, _)| !selector.has_state())
                .collect();
            base_rules.sort_by_key(|(selector, _)| Self::selector_specificity(selector));

            // Build inverted class index once (single lock acquisition) for O(1) lookups
            let class_to_nodes = self.element_registry.class_to_nodes_index();

            for (selector, style) in base_rules {
                // Fast path: simple `.class` selectors use inverted index — O(matched_nodes)
                if let Some(class_name) = selector.simple_class_name() {
                    if let Some(node_ids) = class_to_nodes.get(class_name) {
                        for &node_id in node_ids {
                            if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                                Self::apply_element_style_to_props(&mut render_node.props, style);
                            }
                        }
                    }
                    continue;
                }

                // Slow path: complex selectors (combinators, structural pseudos) — O(all_nodes)
                let all_node_ids: Vec<LayoutNodeId> = self.render_nodes.keys().copied().collect();
                for &node_id in &all_node_ids {
                    if self
                        .complex_selector_matches(selector, node_id, &empty_set, &empty_set, None)
                    {
                        if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                            Self::apply_element_style_to_props(&mut render_node.props, style);
                        }
                    }
                }
            }

            // Eagerly save base_styles for nodes matching classes that also have
            // state rules (:hover, :active, :focus). This prevents the lazy save
            // in apply_complex_selector_styles() from capturing contaminated props
            // (e.g. inline hover backgrounds set by Stateful component rebuilds).
            // Only save if not already present — this function runs for the entire
            // tree and nodes outside a rebuild may still carry hover/active styles.
            let state_class_names: std::collections::HashSet<&str> = complex_rules
                .iter()
                .filter(|(sel, _)| sel.has_state())
                .filter_map(|(sel, _)| sel.class_name_with_state())
                .collect();
            for class_name in &state_class_names {
                if let Some(node_ids) = class_to_nodes.get(*class_name) {
                    for &node_id in node_ids {
                        if !self.base_styles.contains_key(&node_id) {
                            if let Some(render_node) = self.render_nodes.get(&node_id) {
                                self.base_styles.insert(node_id, render_node.props.clone());
                            }
                        }
                    }
                }
            }
        }

        // Apply simple ID rules LAST — #id has highest specificity and overrides
        // type/class selectors applied above.
        let registered_ids: Vec<(String, LayoutNodeId)> = self
            .element_registry
            .all_ids()
            .into_iter()
            .filter_map(|id| self.element_registry.get(&id).map(|node_id| (id, node_id)))
            .collect();

        for (element_id, node_id) in &registered_ids {
            if let Some(base_style) = stylesheet.get(element_id) {
                if let Some(render_node) = self.render_nodes.get_mut(node_id) {
                    Self::apply_element_style_to_props(&mut render_node.props, base_style);
                }
            }
        }

        // Sync CSS text-align into baked TextData for text nodes.
        // text-align may have been set by CSS above but TextData was built before CSS.
        for render_node in self.render_nodes.values_mut() {
            if let Some(ta) = render_node.props.text_align {
                if let ElementType::Text(ref mut text_data) = render_node.element_type {
                    text_data.align = ta;
                }
            }
        }

        // Update Stateful base_render_props with CSS-applied values.
        // This ensures that state changes (hover, press) start from CSS-enhanced
        // base props, preserving CSS overrides like border-radius across state changes.
        for (&node_id, render_node) in &self.render_nodes {
            if crate::stateful::has_stateful_base_updater(node_id) {
                crate::stateful::update_stateful_base_props(node_id, render_node.props.clone());
            }
        }

        // Apply base (non-state) SVG tag-name rules to SVG nodes
        let svg_tag_rules = stylesheet.svg_tag_rules();
        if !svg_tag_rules.is_empty() {
            let empty_set: std::collections::HashSet<LayoutNodeId> =
                std::collections::HashSet::new();
            let svg_nodes: Vec<LayoutNodeId> = self
                .render_nodes
                .keys()
                .copied()
                .filter(|&nid| self.element_registry.get_element_type(nid) == Some("svg"))
                .collect();

            for &svg_node in &svg_nodes {
                let mut tag_styles: std::collections::HashMap<String, crate::element::SvgTagStyle> =
                    std::collections::HashMap::new();
                for &(tag_name, ancestor_segments, style) in &svg_tag_rules {
                    // Skip state-dependent rules (handled by apply_svg_tag_styles)
                    let has_state = ancestor_segments.iter().any(|(c, _)| c.has_state());
                    if has_state {
                        continue;
                    }
                    let matches = if ancestor_segments.is_empty() {
                        true
                    } else {
                        // Check if ancestor segments match the SVG node's chain
                        let last_idx = ancestor_segments.len() - 1;
                        let (last_compound, _) = &ancestor_segments[last_idx];
                        if !self.compound_matches(last_compound, svg_node, false, false, false) {
                            false
                        } else if ancestor_segments.len() == 1 {
                            true
                        } else {
                            let mut current_node = svg_node;
                            let mut all_matched = true;
                            for i in (0..last_idx).rev() {
                                let (compound, combinator) = &ancestor_segments[i];
                                let combinator =
                                    combinator.unwrap_or(crate::css_parser::Combinator::Descendant);
                                match combinator {
                                    crate::css_parser::Combinator::Child => {
                                        match self.element_registry.get_parent(current_node) {
                                            Some(parent) => {
                                                if !self.compound_matches(
                                                    compound, parent, false, false, false,
                                                ) {
                                                    all_matched = false;
                                                    break;
                                                }
                                                current_node = parent;
                                            }
                                            None => {
                                                all_matched = false;
                                                break;
                                            }
                                        }
                                    }
                                    crate::css_parser::Combinator::Descendant => {
                                        let ancestors =
                                            self.element_registry.ancestors(current_node);
                                        let mut found = false;
                                        for ancestor in &ancestors {
                                            if self.compound_matches(
                                                compound, *ancestor, false, false, false,
                                            ) {
                                                current_node = *ancestor;
                                                found = true;
                                                break;
                                            }
                                        }
                                        if !found {
                                            all_matched = false;
                                            break;
                                        }
                                    }
                                    _ => {
                                        all_matched = false;
                                        break;
                                    }
                                }
                            }
                            all_matched
                        }
                    };
                    if matches {
                        let entry = tag_styles.entry(tag_name.to_string()).or_default();
                        if let Some(fill) = style.fill {
                            entry.fill = Some([fill.r, fill.g, fill.b, fill.a]);
                        }
                        if let Some(stroke) = style.stroke {
                            entry.stroke = Some([stroke.r, stroke.g, stroke.b, stroke.a]);
                        }
                        if let Some(sw) = style.stroke_width {
                            entry.stroke_width = Some(sw);
                        }
                        if let Some(ref da) = style.stroke_dasharray {
                            entry.stroke_dasharray = Some(da.clone());
                        }
                        if let Some(offset) = style.stroke_dashoffset {
                            entry.stroke_dashoffset = Some(offset);
                        }
                        if let Some(opacity) = style.opacity {
                            entry.opacity = Some(opacity);
                        }
                    }
                }
                if !tag_styles.is_empty() {
                    if let Some(render_node) = self.render_nodes.get_mut(&svg_node) {
                        render_node.props.svg_tag_styles = tag_styles;
                    }
                }
            }
        }

        // Post-pass: propagate inherited text properties (text-decoration, white-space,
        // text-overflow, text-align) from parent to child nodes. This must run AFTER all
        // CSS styles are applied above, because during initial tree construction the
        // stylesheet wasn't set yet and inherit_text_props_from_parent found no parent values.
        let all_node_ids: Vec<LayoutNodeId> = self.render_nodes.keys().copied().collect();
        for node_id in all_node_ids {
            let parent_id = match self.element_registry.get_parent(node_id) {
                Some(id) => id,
                None => continue,
            };
            // Read parent text props (need separate borrow)
            let parent_text_props = self.render_nodes.get(&parent_id).map(|n| {
                (
                    n.props.text_decoration,
                    n.props.text_decoration_color,
                    n.props.text_decoration_thickness,
                    n.props.white_space,
                    n.props.text_overflow,
                    n.props.text_color,
                    n.props.text_align,
                    n.props.fill,
                    n.props.stroke,
                    n.props.stroke_width,
                )
            });
            if let Some((td, td_color, td_thick, ws, to, tc, ta, fill, stroke, stroke_w)) =
                parent_text_props
            {
                if let Some(node) = self.render_nodes.get_mut(&node_id) {
                    if node.props.text_decoration.is_none() {
                        node.props.text_decoration = td;
                    }
                    if node.props.text_decoration_color.is_none() {
                        node.props.text_decoration_color = td_color;
                    }
                    if node.props.text_decoration_thickness.is_none() {
                        node.props.text_decoration_thickness = td_thick;
                    }
                    if node.props.white_space.is_none() {
                        node.props.white_space = ws;
                    }
                    if node.props.text_overflow.is_none() {
                        node.props.text_overflow = to;
                    }
                    if node.props.text_color.is_none() {
                        node.props.text_color = tc;
                    }
                    if node.props.text_align.is_none() {
                        if let Some(ta) = ta {
                            node.props.text_align = Some(ta);
                            // Also update baked TextData.align so rendering uses the
                            // inherited value (TextData is built before CSS post-pass)
                            if let ElementType::Text(ref mut text_data) = node.element_type {
                                text_data.align = ta;
                            }
                        }
                    }
                    // SVG fill/stroke (CSS spec: inherited in SVG)
                    if node.props.fill.is_none() {
                        node.props.fill = fill;
                    }
                    if node.props.stroke.is_none() {
                        node.props.stroke = stroke;
                    }
                    if node.props.stroke_width.is_none() {
                        node.props.stroke_width = stroke_w;
                    }
                }
            }
        }
    }

    /// Apply CSS base styles (class and ID selectors) to a subtree after rebuild.
    ///
    /// Called after `process_pending_subtree_rebuilds` builds new child nodes.
    /// `collect_render_props_boxed` only applies `#id` styles inline; class-based
    /// selectors (`.sort-item`, `.grid-item`, etc.) are resolved by
    /// `apply_stylesheet_base_styles()` which only runs at full tree creation.
    /// This method fills that gap for incrementally rebuilt subtrees.
    fn apply_stylesheet_base_styles_for_subtree(&mut self, parent_id: LayoutNodeId) {
        let stylesheet = match &self.stylesheet {
            Some(s) => s.clone(),
            None => return,
        };

        // Collect all node IDs in the subtree (parent + descendants)
        let mut subtree_nodes = Vec::new();
        self.collect_subtree_ids(parent_id, &mut subtree_nodes);

        if subtree_nodes.is_empty() {
            return;
        }

        // Apply complex base rules (class selectors, combinators) — lower specificity first
        let complex_rules = stylesheet.complex_rules();
        if !complex_rules.is_empty() {
            let empty_set = std::collections::HashSet::new();

            let mut base_rules: Vec<&(
                crate::css_parser::ComplexSelector,
                crate::element_style::ElementStyle,
            )> = complex_rules
                .iter()
                .filter(|(selector, _)| !selector.has_state())
                .collect();
            base_rules.sort_by_key(|(selector, _)| Self::selector_specificity(selector));

            // Build inverted class index for the subtree nodes
            let class_to_nodes = self.element_registry.class_to_nodes_index();
            // Filter to only subtree nodes for simple class lookups
            let subtree_set: std::collections::HashSet<LayoutNodeId> =
                subtree_nodes.iter().copied().collect();

            for (selector, style) in base_rules {
                // Fast path: simple `.class` selectors use inverted index
                if let Some(class_name) = selector.simple_class_name() {
                    if let Some(node_ids) = class_to_nodes.get(class_name) {
                        for &node_id in node_ids {
                            if subtree_set.contains(&node_id) {
                                if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                                    Self::apply_element_style_to_props(
                                        &mut render_node.props,
                                        style,
                                    );
                                }
                            }
                        }
                    }
                    continue;
                }

                // Slow path: complex selectors need full matching
                for &node_id in &subtree_nodes {
                    if self
                        .complex_selector_matches(selector, node_id, &empty_set, &empty_set, None)
                    {
                        if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                            Self::apply_element_style_to_props(&mut render_node.props, style);
                        }
                    }
                }
            }

            // Eagerly save base_styles for subtree nodes matching classes that
            // also have :hover/:active/:focus state rules. This prevents the
            // lazy save in apply_complex_selector_styles() from capturing
            // contaminated props set by Stateful component rebuilds.
            let state_class_names: std::collections::HashSet<&str> = complex_rules
                .iter()
                .filter(|(sel, _)| sel.has_state())
                .filter_map(|(sel, _)| sel.class_name_with_state())
                .collect();
            for class_name in &state_class_names {
                if let Some(node_ids) = class_to_nodes.get(*class_name) {
                    for &node_id in node_ids {
                        if subtree_set.contains(&node_id) {
                            if let Some(render_node) = self.render_nodes.get(&node_id) {
                                self.base_styles.insert(node_id, render_node.props.clone());
                            }
                        }
                    }
                }
            }
        }

        // Apply simple ID rules (highest specificity, overrides class selectors)
        for &node_id in &subtree_nodes {
            if let Some(element_id) = self.element_registry.get_id(node_id) {
                if let Some(base_style) = stylesheet.get(&element_id) {
                    if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                        Self::apply_element_style_to_props(&mut render_node.props, base_style);
                    }
                }
            }
        }

        // Update Stateful base_render_props for subtree nodes with CSS-applied values
        for &node_id in &subtree_nodes {
            if crate::stateful::has_stateful_base_updater(node_id) {
                if let Some(render_node) = self.render_nodes.get(&node_id) {
                    crate::stateful::update_stateful_base_props(node_id, render_node.props.clone());
                }
            }
        }

        // Propagate inherited text properties (color, text-decoration, etc.)
        // from parent to child nodes within the rebuilt subtree.
        for &node_id in &subtree_nodes {
            let parent_id = match self.element_registry.get_parent(node_id) {
                Some(id) => id,
                None => continue,
            };
            let parent_text_props = self.render_nodes.get(&parent_id).map(|n| {
                (
                    n.props.text_decoration,
                    n.props.text_decoration_color,
                    n.props.text_decoration_thickness,
                    n.props.white_space,
                    n.props.text_overflow,
                    n.props.text_color,
                    n.props.text_align,
                )
            });
            if let Some((td, td_color, td_thick, ws, to, tc, ta)) = parent_text_props {
                if let Some(node) = self.render_nodes.get_mut(&node_id) {
                    if node.props.text_decoration.is_none() {
                        node.props.text_decoration = td;
                    }
                    if node.props.text_decoration_color.is_none() {
                        node.props.text_decoration_color = td_color;
                    }
                    if node.props.text_decoration_thickness.is_none() {
                        node.props.text_decoration_thickness = td_thick;
                    }
                    if node.props.white_space.is_none() {
                        node.props.white_space = ws;
                    }
                    if node.props.text_overflow.is_none() {
                        node.props.text_overflow = to;
                    }
                    if node.props.text_color.is_none() {
                        node.props.text_color = tc;
                    }
                    if node.props.text_align.is_none() {
                        if let Some(ta) = ta {
                            node.props.text_align = Some(ta);
                            if let ElementType::Text(ref mut text_data) = node.element_type {
                                text_data.align = ta;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Collect all node IDs in a subtree (the node itself + all descendants).
    fn collect_subtree_ids(&self, node_id: LayoutNodeId, out: &mut Vec<LayoutNodeId>) {
        out.push(node_id);
        for child_id in self.layout_tree.children(node_id) {
            self.collect_subtree_ids(child_id, out);
        }
    }

    // ========================================================================
    // FLIP Animation for Subtree Rebuilds
    // ========================================================================

    /// Update persistent element bounds for FLIP tracking.
    ///
    /// Called after every `compute_layout()` to record current absolute positions
    /// by element string ID. This data survives across subtree rebuilds since it's
    /// keyed by stable string IDs, not volatile LayoutNodeIds.
    pub fn update_flip_bounds(&mut self) {
        self.flip_previous_bounds.clear();
        for (node_id, _render_node) in &self.render_nodes {
            if let Some(element_id) = self.element_registry.get_id(*node_id) {
                if let Some(bounds) = self.layout_tree.get_absolute_bounds(*node_id) {
                    self.flip_previous_bounds.insert(element_id, bounds);
                }
            }
        }
    }

    /// Apply FLIP transitions for elements that moved during subtree rebuild.
    ///
    /// Called AFTER layout recomputation (in windowed.rs, after compute_layout()).
    /// Compares new layout positions to snapshots taken before the rebuild.
    /// For elements that moved AND have a CSS `transition` on `transform`,
    /// creates a CSS transition from `translate(dx, dy)` to `translate(0, 0)`.
    pub fn apply_flip_transitions(&mut self) {
        tracing::trace!(
            "FLIP: apply_flip_transitions called, flip_previous_bounds has {} entries",
            self.flip_previous_bounds.len()
        );
        if self.flip_previous_bounds.is_empty() {
            return;
        }

        let stylesheet = self.stylesheet.clone();

        // Collect elements that moved: compare previous bounds with current absolute bounds
        let mut moved: Vec<(String, f32, f32, crate::tree::LayoutNodeId)> = Vec::new();

        for (node_id, render_node) in &self.render_nodes {
            // Skip elements that already have a transform set by the app (e.g. the dragged item).
            // FLIP should only animate elements whose position changed passively due to reflow,
            // not elements being actively positioned via Transform::translate().
            if render_node.props.transform.is_some() {
                continue;
            }

            let Some(element_id) = self.element_registry.get_id(*node_id) else {
                continue;
            };
            let Some(old_bounds) = self.flip_previous_bounds.get(&element_id) else {
                continue;
            };
            let Some(new_bounds) = self.layout_tree.get_absolute_bounds(*node_id) else {
                continue;
            };

            let dx = old_bounds.x - new_bounds.x;
            let dy = old_bounds.y - new_bounds.y;

            if dx.abs() < 1.0 && dy.abs() < 1.0 {
                continue;
            }

            tracing::trace!("FLIP: '{}' moved: delta=({:.1},{:.1})", element_id, dx, dy);
            moved.push((element_id, dx, dy, *node_id));
        }

        if moved.is_empty() {
            return;
        }

        tracing::debug!("FLIP: {} elements moved, creating transitions", moved.len());

        for (element_id, dx, dy, new_node_id) in &moved {
            // Find CSS transition spec for "transform" (CssTransitionSet::get also matches "all")
            // Check: 1) ID-based style, 2) class-based complex selector rules
            let transition = stylesheet.as_ref().and_then(|ss| {
                // Check by element ID first
                ss.get(element_id)
                    .and_then(|style| style.transition.as_ref())
                    .and_then(|ts| ts.get("transform"))
                    .or_else(|| {
                        // Check class-based complex selector rules
                        let empty = std::collections::HashSet::new();
                        for rule in ss.complex_rules() {
                            if rule.0.has_state() {
                                continue;
                            }
                            if self.complex_selector_matches(
                                &rule.0,
                                *new_node_id,
                                &empty,
                                &empty,
                                None,
                            ) {
                                if let Some(ts) = rule.1.transition.as_ref() {
                                    if let Some(t) = ts.get("transform") {
                                        return Some(t);
                                    }
                                }
                            }
                        }
                        None
                    })
            });

            let Some(transition) = transition else {
                tracing::trace!(
                    "FLIP: '{}' has no CSS transition for 'transform'",
                    element_id
                );
                continue;
            };

            let duration_ms = transition.duration_ms;
            let delay_ms = transition.delay_ms;
            let easing = transition.timing.to_easing();

            if duration_ms == 0 {
                continue;
            }

            tracing::debug!(
                "FLIP: '{}' translate({:.1}, {:.1}) → (0,0) over {}ms",
                element_id,
                dx,
                dy,
                duration_ms
            );

            // Create a CSS transition from translate(dx, dy) to translate(0, 0)
            use blinc_animation::{FillMode, KeyframeProperties, MultiKeyframeAnimation};

            let from = KeyframeProperties {
                translate_x: Some(*dx),
                translate_y: Some(*dy),
                ..Default::default()
            };

            let to = KeyframeProperties {
                translate_x: Some(0.0),
                translate_y: Some(0.0),
                ..Default::default()
            };

            let anim = MultiKeyframeAnimation::new(duration_ms)
                .keyframe(0.0, from, easing)
                .keyframe(1.0, to, easing)
                .delay(delay_ms)
                .fill_mode(FillMode::Forwards);

            // Store in flip_animations keyed by string ID (survives subtree rebuilds).
            // Unlike css_anim_store.transitions which are keyed by LayoutNodeId,
            // these persist when nodes are recreated because we resolve to the
            // current LayoutNodeId at apply time via element_registry.
            self.flip_animations.insert(
                element_id.clone(),
                crate::render_state::ActiveCssAnimation::new(anim),
            );
        }
    }

    /// Tick all active FLIP animations by `dt_ms` milliseconds.
    /// Removes completed animations. Returns `true` if any are still playing.
    pub fn tick_flip_animations(&mut self, dt_ms: f32) -> bool {
        if self.flip_animations.is_empty() {
            return false;
        }
        let mut any_playing = false;
        for anim in self.flip_animations.values_mut() {
            if anim.tick(dt_ms) {
                any_playing = true;
            }
        }
        // Remove completed animations
        self.flip_animations.retain(|_, a| a.is_playing);
        any_playing
    }

    /// Apply current FLIP animation values to render props.
    /// Resolves string element IDs → LayoutNodeIds via element_registry.
    pub fn apply_flip_animation_props(&mut self) {
        if self.flip_animations.is_empty() {
            return;
        }
        // Collect data first to avoid borrow conflict with render_nodes
        let props_data: Vec<(
            crate::tree::LayoutNodeId,
            blinc_animation::KeyframeProperties,
        )> = self
            .flip_animations
            .iter()
            .filter_map(|(element_id, anim)| {
                let node_id = self.element_registry.get(element_id)?;
                Some((node_id, anim.current_properties.clone()))
            })
            .collect();

        for (node_id, anim_props) in props_data {
            if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                Self::apply_keyframe_props_to_render(&mut render_node.props, &anim_props);
            }
        }
    }

    /// Check if any FLIP animations are currently active.
    pub fn has_active_flip_animations(&self) -> bool {
        !self.flip_animations.is_empty()
    }

    /// Visibility-gated counterpart of `has_active_flip_animations`.
    /// FLIP entries are keyed by `element_id: String` (they survive
    /// subtree rebuilds), so resolve to the current `LayoutNodeId`
    /// through the element registry before checking visibility.
    /// Element ids without a current binding are skipped — if the
    /// node is gone, the animation can't be on screen anyway.
    pub fn has_active_visible_flip_animations(&self, painted: &HashSet<LayoutNodeId>) -> bool {
        self.flip_animations.keys().any(|element_id| {
            self.element_registry
                .get(element_id)
                .is_some_and(|nid| painted.contains(&nid))
        })
    }

    /// Visibility-gated counterpart of `css_transitions_empty`.
    /// Returns `true` when there's at least one transition whose
    /// target node was painted in the most recent frame, i.e. the
    /// chain should keep firing for it.
    pub fn css_has_visible_transitions(&self, painted: &HashSet<LayoutNodeId>) -> bool {
        let store = self.css_anim_store.lock().unwrap();
        store.transitions.keys().any(|n| painted.contains(n))
    }

    /// Apply stylesheet state styles based on EventRouter state
    ///
    /// This should be called after mouse events to update styles for nodes
    /// whose interaction state has changed. It applies `:hover`, `:active`,
    /// and `:focus` styles from the stylesheet.
    ///
    /// # Arguments
    /// * `router` - The event router containing current interaction state
    ///
    /// # Returns
    /// `true` if any styles were applied, `false` otherwise
    pub fn apply_stylesheet_state_styles(
        &mut self,
        router: &crate::event_router::EventRouter,
    ) -> bool {
        // Early return if no stylesheet
        if self.stylesheet.is_none() {
            return false;
        }

        let mut any_applied = false;

        // Get all registered element IDs and their node IDs
        let registered_ids: Vec<(String, crate::tree::LayoutNodeId)> = self
            .element_registry
            .all_ids()
            .into_iter()
            .filter_map(|id| self.element_registry.get(&id).map(|node_id| (id, node_id)))
            .collect();

        // Apply state styles for each registered element
        for (element_id, node_id) in registered_ids {
            // Check if this element has any state styles in the stylesheet
            if !self.has_state_styles(node_id) {
                continue;
            }

            // Get current interaction state from router
            let hovered = router.is_hovered(node_id);
            let pressed = router.is_pressed(node_id);
            let focused = router.is_focused(node_id);

            // Apply state styles
            if self.apply_state_styles(node_id, hovered, pressed, focused) {
                any_applied = true;
                tracing::trace!(
                    "Applied stylesheet state styles to #{}: hovered={}, pressed={}, focused={}",
                    element_id,
                    hovered,
                    pressed,
                    focused
                );
            }

            // Trigger/stop hover CSS animations
            let stylesheet = self.stylesheet.as_ref().unwrap();
            if hovered && !self.hover_css_animations.contains(&node_id) {
                let has_hover_anim = stylesheet
                    .get_with_state(&element_id, ElementState::Hover)
                    .and_then(|s| s.animation.as_ref())
                    .is_some();
                if has_hover_anim {
                    self.start_css_animation_for_state(node_id, ElementState::Hover);
                    self.hover_css_animations.insert(node_id);
                    any_applied = true;
                }
            } else if !hovered && self.hover_css_animations.remove(&node_id) {
                // Hover left — remove hover animation if no base animation exists
                let base_has_anim = stylesheet
                    .get(&element_id)
                    .and_then(|s| s.animation.as_ref())
                    .is_some();
                if base_has_anim {
                    self.start_css_animation_for_element(node_id);
                } else {
                    self.css_anim_store
                        .lock()
                        .unwrap()
                        .animations
                        .remove(&node_id);
                }
                any_applied = true;
            }
        }

        // Apply complex selector rules (class selectors, descendant/child combinators, etc.)
        if self.apply_complex_selector_styles(router) {
            any_applied = true;
        }

        // Apply SVG tag-name CSS rules (e.g., `path { fill: red; }`)
        if self.apply_svg_tag_styles(router) {
            any_applied = true;
        }

        any_applied
    }

    /// Evaluate dynamic `calc(env(...))` properties for pointer-tracked elements.
    ///
    /// Called per-frame after `apply_stylesheet_state_styles()` and before rendering.
    /// For each element in the pointer query, collects dynamic properties from the
    /// active stylesheet entries (base + hover/active/focus) and evaluates them with
    /// the current pointer state, writing results directly to RenderProps.
    pub fn apply_pointer_styles(
        &mut self,
        pointer_query: &crate::pointer_query::PointerQueryState,
        router: &crate::event_router::EventRouter,
    ) {
        let stylesheet = match &self.stylesheet {
            Some(s) => s.clone(),
            None => return,
        };

        for (element_id, pointer_state) in pointer_query.iter() {
            let node_id = match self.element_registry.get(element_id) {
                Some(id) => id,
                None => continue,
            };

            // Build CalcContext with pointer env vars
            let mut ctx = crate::calc::CalcContext::default();
            for name in &[
                "pointer-x",
                "pointer-y",
                "pointer-vx",
                "pointer-vy",
                "pointer-speed",
                "pointer-distance",
                "pointer-angle",
                "pointer-inside",
                "pointer-active",
                "pointer-pressure",
                "pointer-touch-count",
                "pointer-hover-duration",
            ] {
                if let Some(val) = pointer_state.resolve_env(name) {
                    ctx.env_vars.insert(name.to_string(), val);
                }
            }

            // Collect dynamic properties from applicable stylesheet entries
            // (base first, then state overrides in precedence order)
            let mut dynamic_props: Vec<&crate::element_style::DynamicProperty> = Vec::new();

            // Base style
            if let Some(base) = stylesheet.get(element_id) {
                if let Some(ref dps) = base.dynamic_properties {
                    dynamic_props.extend(dps.iter());
                }
            }

            // Hover state (overrides base)
            if router.is_hovered(node_id) {
                if let Some(hover) = stylesheet.get_with_state(element_id, ElementState::Hover) {
                    if let Some(ref dps) = hover.dynamic_properties {
                        dynamic_props.extend(dps.iter());
                    }
                }
            }

            // Active/pressed state (overrides hover)
            if router.is_pressed(node_id) {
                if let Some(active) = stylesheet.get_with_state(element_id, ElementState::Active) {
                    if let Some(ref dps) = active.dynamic_properties {
                        dynamic_props.extend(dps.iter());
                    }
                }
            }

            // Focus state
            if router.is_focused(node_id) {
                if let Some(focus) = stylesheet.get_with_state(element_id, ElementState::Focus) {
                    if let Some(ref dps) = focus.dynamic_properties {
                        dynamic_props.extend(dps.iter());
                    }
                }
            }

            if dynamic_props.is_empty() {
                continue;
            }

            // Evaluate and apply to RenderProps
            if let Some(render_node) = self.render_nodes.get_mut(&node_id) {
                // If any dynamic properties are transform-related (SkewX, SkewY, Rotate, etc.),
                // reset props.transform to its base value first to prevent frame-compounding.
                // Without this, compose_affine() would accumulate onto the previous frame's
                // dynamic transform, causing exponential growth.
                let has_transform_dynamics = dynamic_props.iter().any(|dp| dp.is_transform());
                if has_transform_dynamics {
                    let base_transform = self
                        .base_styles
                        .get(&node_id)
                        .and_then(|base| base.transform.clone());
                    render_node.props.transform = base_transform;
                }

                for dp in &dynamic_props {
                    dp.apply(&mut render_node.props, &ctx);
                }
            }
        }
    }

    /// Rebuild only the children of a specific node
    ///
    /// This is used for incremental updates when a stateful element's
    /// dependencies change. Instead of rebuilding the entire tree,
    /// we only rebuild the affected subtree.
    ///
    /// # Arguments
    /// * `parent_id` - The node whose children should be rebuilt
    /// * `new_child` - The new child element builder
    ///
    /// # Returns
    /// The ID of the new child node
    pub fn rebuild_children<E: ElementBuilder>(
        &mut self,
        parent_id: LayoutNodeId,
        new_child: &E,
    ) -> LayoutNodeId {
        // 1. Remove old children from layout tree and render nodes
        let old_children = self.layout_tree.children(parent_id);
        for child_id in &old_children {
            self.remove_subtree_nodes(*child_id);
        }
        self.layout_tree.clear_children(parent_id);

        // 2. Build the new child element into the layout tree
        let new_child_id = new_child.build(&mut self.layout_tree);

        // 3. Add the new child to the parent
        self.layout_tree.add_child(parent_id, new_child_id);

        // 4. Collect render props for the new subtree
        self.collect_render_props(new_child, new_child_id);

        new_child_id
    }

    /// Remove render nodes for a subtree (but don't touch layout tree)
    fn remove_subtree_nodes(&mut self, node_id: LayoutNodeId) {
        // Remove children first
        let children = self.layout_tree.children(node_id);
        for child_id in children {
            self.remove_subtree_nodes(child_id);
        }

        // Remove this node's render data
        self.render_nodes.swap_remove(&node_id);
        self.handler_registry.remove(node_id);
        self.node_states.remove(&node_id);
        self.scroll_offsets.remove(&node_id);
        self.scroll_physics.remove(&node_id);
        self.scroll_refs.remove(&node_id);
        // Unregister from element registry (removes by node_id)
        self.element_registry.unregister(node_id);
        // Remove layout animation config (but keep stable-key animations running)
        self.layout_animation_configs.remove(&node_id);
        self.layout_animations.remove(&node_id);
        self.previous_bounds.remove(&node_id);

        // Remove CSS state tracking data (prevents accumulation across rebuilds)
        self.base_styles.remove(&node_id);
        self.base_taffy_styles.remove(&node_id);
        self.node_hashes.remove(&node_id);
        self.layout_bounds_storages.remove(&node_id);
        self.animated_render_bounds.remove(&node_id);
        self.motion_bindings.remove(&node_id);
        self.hover_css_animations.remove(&node_id);
        self.complex_state_affected.remove(&node_id);

        // Remove CSS animations/transitions for this node from the shared store
        if let Ok(mut store) = self.css_anim_store.lock() {
            store.animations.remove(&node_id);
            store.transitions.remove(&node_id);
        }
    }

    /// Process all pending subtree rebuilds
    ///
    /// This is called by the windowed app after processing events.
    /// It applies queued child rebuilds without rebuilding the entire tree.
    /// Process pending subtree rebuilds
    ///
    /// Returns true if any rebuild requires layout recomputation.
    /// Visual-only rebuilds (hover/press) return false.
    ///
    /// Processes only rebuilds for nodes that exist in this tree.
    /// Rebuilds for nodes in other trees (e.g., overlay) are put back in the queue.
    pub fn process_pending_subtree_rebuilds(&mut self) -> bool {
        let pending = crate::stateful::take_pending_subtree_rebuilds();
        if pending.is_empty() {
            return false;
        }

        tracing::debug!("Processing {} pending subtree rebuilds", pending.len());

        let mut needs_layout = false;
        let mut not_in_this_tree = Vec::new();

        for rebuild in pending {
            // Skip if this node doesn't exist in this tree - save for other trees
            if !self.layout_tree.node_exists(rebuild.parent_id) {
                tracing::debug!(
                    "Subtree rebuild: node {:?} not in this tree, requeuing",
                    rebuild.parent_id
                );
                not_in_this_tree.push(rebuild);
                continue;
            }
            tracing::debug!(
                "Subtree rebuild: processing node {:?}, needs_layout={}",
                rebuild.parent_id,
                rebuild.needs_layout
            );
            if rebuild.needs_layout {
                // Full structural rebuild - remove old children and build new ones
                needs_layout = true;

                // Update the parent node's own render props AND layout style
                // This is critical for overlay layer where size changes from 0x0 to full viewport
                if let Some(render_node) = self.render_nodes.get_mut(&rebuild.parent_id) {
                    let mut new_props = rebuild.new_child.render_props();
                    new_props.node_id = Some(rebuild.parent_id);
                    new_props.motion = render_node.props.motion.clone();
                    render_node.props = new_props;
                }
                // Update parent node's CSS class registrations so that
                // apply_stylesheet_base_styles_for_subtree matches the current
                // classes (e.g., cn-checkbox--checked added/removed on toggle).
                let parent_classes = rebuild.new_child.element_classes();
                if !parent_classes.is_empty() {
                    self.element_registry
                        .register_classes(rebuild.parent_id, parent_classes.to_vec());
                } else {
                    self.element_registry.clear_classes(rebuild.parent_id);
                }
                self.base_styles.remove(&rebuild.parent_id);
                // Also update the taffy layout style (width, height, padding, etc.)
                if let Some(style) = rebuild.new_child.layout_style() {
                    self.layout_tree.set_style(rebuild.parent_id, style.clone());
                }

                // Re-register scroll_physics and event_handlers for the parent node.
                // Without this, Stateful containers with overflow_y_scroll() lose their
                // scroll state during rebuilds because only children get collect_render_props_boxed.
                if let Some(physics) = rebuild.new_child.scroll_physics() {
                    if let Some(scheduler) = self.animations.upgrade() {
                        physics.lock().unwrap().set_scheduler(&scheduler);
                    }
                    self.scroll_physics.insert(rebuild.parent_id, physics);
                    if rebuild.new_child.viewport_cull() {
                        self.viewport_cull_scrolls.insert(rebuild.parent_id);
                    }
                }
                {
                    let handlers = rebuild.new_child.event_handlers();
                    if !handlers.is_empty() {
                        self.handler_registry
                            .register(rebuild.parent_id, handlers.clone());
                    }
                }

                // Always remove old children first (even if new children is empty)
                // This fixes the bug where SVG checkmarks would persist after unchecking
                let old_children = self.layout_tree.children(rebuild.parent_id);
                for child_id in &old_children {
                    self.remove_subtree_nodes(*child_id);
                }
                self.layout_tree.clear_children(rebuild.parent_id);

                // Build new children (if any)
                let children = rebuild.new_child.children_builders();
                for child in children {
                    let child_id = child.build(&mut self.layout_tree);
                    self.layout_tree.add_child(rebuild.parent_id, child_id);
                    self.collect_render_props_boxed(child.as_ref(), child_id);
                }

                // Apply CSS base styles (class/complex selectors) to new subtree nodes.
                // collect_render_props_boxed only applies #id styles; class-based
                // styles are applied by apply_stylesheet_base_styles() which only
                // runs at full tree creation. Without this, new children from
                // stateful rebuilds lose CSS class styles (border-radius, etc.).
                self.apply_stylesheet_base_styles_for_subtree(rebuild.parent_id);
            } else {
                // Visual-only update - just update render props of existing children
                // Don't remove/rebuild, just walk the tree and update props
                self.update_subtree_props_recursive(rebuild.parent_id, &rebuild.new_child);
            }
        }

        // Put back rebuilds for nodes not in this tree (for other trees to process)
        if !not_in_this_tree.is_empty() {
            crate::stateful::requeue_subtree_rebuilds(not_in_this_tree);
        }

        needs_layout
    }

    /// Recursively update render props for existing children without rebuilding
    ///
    /// This walks the existing layout tree children alongside the new element definition
    /// and updates render props for matching nodes (by position in child order).
    fn update_subtree_props_recursive(
        &mut self,
        parent_id: LayoutNodeId,
        new_element: &crate::div::Div,
    ) {
        self.update_subtree_props_from_builder(parent_id, new_element);
    }

    /// Update subtree props from a generic ElementBuilder (for recursion)
    ///
    /// Uses full replacement (not merge) so that properties cleared back to defaults
    /// (e.g. transform removed on drag end) are properly reflected. Preserves node_id
    /// and motion which are not set by builders.
    fn update_subtree_props_from_builder(
        &mut self,
        parent_id: LayoutNodeId,
        new_element: &dyn crate::div::ElementBuilder,
    ) {
        let existing_children = self.layout_tree.children(parent_id);
        let new_children = new_element.children_builders();

        for (i, child_id) in existing_children.iter().enumerate() {
            if let Some(new_child) = new_children.get(i) {
                // Full replace of visual props, preserving node_id and motion
                let mut new_props = new_child.render_props();
                if let Some(render_node) = self.render_nodes.get_mut(child_id) {
                    new_props.node_id = render_node.props.node_id;
                    new_props.motion = render_node.props.motion.clone();
                    render_node.props = new_props;
                    // Also update element_type (SVG tint, text content, image data, etc.)
                    // Without this, visual-only rebuilds leave stale element data.
                    render_node.element_type =
                        Self::determine_element_type_boxed(new_child.as_ref());
                }

                // Re-register event handlers from the new element builder.
                // During visual-only rebuilds the tree structure doesn't change,
                // but callbacks may capture new closure state that needs updating.
                if let Some(handlers) = new_child.event_handlers() {
                    self.handler_registry.register(*child_id, handlers.clone());
                }

                // Update CSS class registrations so apply_stylesheet_base_styles_for_subtree
                // uses the current classes (not stale ones from the previous build).
                // Without this, adding/removing classes (e.g. cn-sidebar-item--active)
                // wouldn't take effect during visual-only rebuilds.
                let new_classes = new_child.element_classes();
                let old_classes = self.element_registry.get_classes(*child_id);
                let classes_changed = new_classes != old_classes.as_deref().unwrap_or(&[]);
                if !new_classes.is_empty() {
                    self.element_registry
                        .register_classes(*child_id, new_classes.to_vec());
                } else {
                    self.element_registry.clear_classes(*child_id);
                }
                // Invalidate base_styles cache when classes change so that
                // apply_complex_selector_styles resets to the correct base
                // (e.g., node gaining --active needs its new base to include that)
                if classes_changed {
                    self.base_styles.remove(child_id);
                }

                // Recursively update grandchildren
                if !new_child.children_builders().is_empty() {
                    self.update_subtree_props_from_builder(*child_id, new_child.as_ref());
                }
            }
        }

        // Re-apply CSS base styles since the full replace cleared them
        self.apply_stylesheet_base_styles_for_subtree(parent_id);
    }

    /// Transfer node states from another tree
    ///
    /// This preserves state across rebuilds by copying the state storage
    /// from the old tree to the new one.
    pub fn transfer_states_from(&mut self, other: &RenderTree) {
        for (node_id, state) in &other.node_states {
            self.node_states.insert(*node_id, Arc::clone(state));
        }
    }

    // `node_states` moved to `renderer/queries.rs`.

    /// Render the entire tree to a DrawContext
    pub fn render(&self, ctx: &mut dyn DrawContext) {
        tracing::trace!(
            "render: motion_bindings count = {}",
            self.motion_bindings.len()
        );
        if let Some(root) = self.root {
            self.render_node(ctx, root, (0.0, 0.0), (0.0, 0.0));
        }
    }

    /// Render a single node and its children
    fn render_node(
        &self,
        ctx: &mut dyn DrawContext,
        node: LayoutNodeId,
        parent_offset: (f32, f32),
        cumulative_scroll: (f32, f32),
    ) {
        let Some(bounds) = self.layout_tree.get_bounds(node, parent_offset) else {
            return;
        };

        let Some(render_node) = self.render_nodes.get(&node) else {
            return;
        };

        // CSS visibility: hidden — skip rendering but preserve layout space
        if !render_node.props.visible {
            return;
        }

        // Push transform for this node's position
        ctx.push_transform(Transform::translate(bounds.x, bounds.y));

        // Apply element-specific transform if present (static, set at build time)
        // Transforms are applied around the element's center (like CSS transform-origin: 50% 50%)
        let has_element_transform = render_node.props.transform.is_some();
        if let Some(ref transform) = render_node.props.transform {
            // To center transforms:
            // 1. Translate so element center is at origin
            // 2. Apply the user's transform
            // 3. Translate back
            let center_x = bounds.width / 2.0;
            let center_y = bounds.height / 2.0;
            ctx.push_transform(Transform::translate(center_x, center_y));
            ctx.push_transform(transform.clone());
            ctx.push_transform(Transform::translate(-center_x, -center_y));
        }

        // Apply motion binding translation if present (dynamic, sampled every frame)
        // Translation is NOT centered (moves element from its position)
        let motion_transform = self.get_motion_transform(node);
        let has_motion_transform = motion_transform.is_some();
        if let Some(ref transform) = motion_transform {
            // Log to verify animation is running
            if let Transform::Affine2D(a) = transform {
                tracing::debug!(
                    "paint_node: applying motion transform to {:?}: tx={}, ty={}",
                    node,
                    a.elements[4],
                    a.elements[5]
                );
            }
            ctx.push_transform(transform.clone());
        }

        // Apply motion binding scale if present (centered around element)
        let motion_scale = self.get_motion_scale(node);
        let has_motion_scale = motion_scale.is_some();
        if let Some((sx, sy)) = motion_scale {
            let center_x = bounds.width / 2.0;
            let center_y = bounds.height / 2.0;
            ctx.push_transform(Transform::translate(center_x, center_y));
            ctx.push_transform(Transform::scale(sx, sy));
            ctx.push_transform(Transform::translate(-center_x, -center_y));
        }

        // Apply motion binding rotation if present (centered around element)
        let motion_rotation = self.get_motion_rotation(node);
        let has_motion_rotation = motion_rotation.is_some();
        if let Some(deg) = motion_rotation {
            let center_x = bounds.width / 2.0;
            let center_y = bounds.height / 2.0;
            ctx.push_transform(Transform::translate(center_x, center_y));
            ctx.push_transform(Transform::rotate(deg.to_radians()));
            ctx.push_transform(Transform::translate(-center_x, -center_y));
        }

        let rect = Rect::new(0.0, 0.0, bounds.width, bounds.height);
        let radius = render_node.props.border_radius;
        let is_glass = matches!(render_node.props.material, Some(Material::Glass(_)));

        // Corner shape setup — must be before draw_shadow so shadows match fill shape
        let has_corner_shape_l = !render_node.props.corner_shape.is_round();
        if has_corner_shape_l {
            ctx.set_corner_shape(render_node.props.corner_shape.to_array());
        }

        // Check if this node has a glass material - if so, render as glass with shadow
        if let Some(Material::Glass(glass)) = &render_node.props.material {
            // For glass elements, pass shadow through GlassStyle to use GPU glass shadow system
            let glass_brush = Brush::Glass(GlassStyle {
                blur: glass.blur,
                tint: glass.tint,
                saturation: glass.saturation,
                brightness: glass.brightness,
                noise: glass.noise,
                border_thickness: glass.border_thickness,
                shadow: render_node.props.shadow,
                simple: glass.simple,
                depth: 0,
                border_color: render_node.props.border_color,
            });
            ctx.fill_rect(rect, radius, glass_brush);
        } else {
            // For non-glass elements, draw shadow first (renders behind the element)
            if let Some(ref shadow) = render_node.props.shadow {
                ctx.draw_shadow(rect, radius, *shadow);
            }

            // Merge fill + border into a single SDF primitive when possible.
            // This avoids AA fringe from overlapping fill + border at corners.
            let sides = &render_node.props.border_sides;
            let has_per_side = sides.has_any();
            let has_uniform = !has_per_side
                && render_node.props.border_width > 0.0
                && render_node.props.border_color.is_some();

            if has_per_side {
                let uw = render_node.props.border_width;
                let uc = render_node.props.border_color.unwrap_or(Color::TRANSPARENT);
                let top = sides
                    .top
                    .as_ref()
                    .map(|b| (b.width, b.color))
                    .unwrap_or((uw, uc));
                let right = sides
                    .right
                    .as_ref()
                    .map(|b| (b.width, b.color))
                    .unwrap_or((uw, uc));
                let bottom = sides
                    .bottom
                    .as_ref()
                    .map(|b| (b.width, b.color))
                    .unwrap_or((uw, uc));
                let left = sides
                    .left
                    .as_ref()
                    .map(|b| (b.width, b.color))
                    .unwrap_or((uw, uc));
                let all_same = top.1 == right.1 && right.1 == bottom.1 && bottom.1 == left.1;

                if all_same {
                    // Same color — single merged primitive
                    let brush = render_node
                        .props
                        .background
                        .clone()
                        .unwrap_or(Brush::Solid(Color::TRANSPARENT));
                    ctx.fill_rect_with_per_side_border(
                        rect,
                        radius,
                        brush,
                        [top.0, right.0, bottom.0, left.0],
                        top.1,
                    );
                } else {
                    // Different colors — draw fill then 4x fill_rect
                    if let Some(ref bg) = render_node.props.background {
                        ctx.fill_rect(rect, radius, bg.clone());
                    }
                    let has_radius = radius.top_left > 0.0
                        || radius.top_right > 0.0
                        || radius.bottom_left > 0.0
                        || radius.bottom_right > 0.0;
                    if has_radius {
                        ctx.push_clip(ClipShape::rounded_rect(rect, radius));
                    }
                    if let Some(ref b) = sides.left {
                        if b.is_visible() {
                            ctx.fill_rect(
                                Rect::new(0.0, 0.0, b.width, rect.height()),
                                CornerRadius::default(),
                                Brush::Solid(b.color),
                            );
                        }
                    }
                    if let Some(ref b) = sides.right {
                        if b.is_visible() {
                            ctx.fill_rect(
                                Rect::new(rect.width() - b.width, 0.0, b.width, rect.height()),
                                CornerRadius::default(),
                                Brush::Solid(b.color),
                            );
                        }
                    }
                    if let Some(ref b) = sides.top {
                        if b.is_visible() {
                            ctx.fill_rect(
                                Rect::new(0.0, 0.0, rect.width(), b.width),
                                CornerRadius::default(),
                                Brush::Solid(b.color),
                            );
                        }
                    }
                    if let Some(ref b) = sides.bottom {
                        if b.is_visible() {
                            ctx.fill_rect(
                                Rect::new(0.0, rect.height() - b.width, rect.width(), b.width),
                                CornerRadius::default(),
                                Brush::Solid(b.color),
                            );
                        }
                    }
                    if has_radius {
                        ctx.pop_clip();
                    }
                }
            } else if has_uniform {
                // Uniform border — merge with fill
                let bw = render_node.props.border_width;
                let bc = *render_node.props.border_color.as_ref().unwrap();
                let brush = render_node
                    .props
                    .background
                    .clone()
                    .unwrap_or(Brush::Solid(Color::TRANSPARENT));
                ctx.fill_rect_with_per_side_border(rect, radius, brush, [bw, bw, bw, bw], bc);
            } else {
                // No border — just fill
                if let Some(ref bg) = render_node.props.background {
                    ctx.fill_rect(rect, radius, bg.clone());
                }
            }
        }

        // Only glass needs foreground borders (special compositing).
        let border_in_foreground = is_glass;
        if border_in_foreground {
            ctx.set_foreground_layer(true);
        }

        // Draw outline outside the border (CSS outlines don't affect layout)
        if render_node.props.outline_width > 0.0 {
            if let Some(ref outline_color) = render_node.props.outline_color {
                let ow = render_node.props.outline_width;
                let offset = render_node.props.outline_offset;
                let expand = offset + ow / 2.0;
                let outline_rect = Rect::new(
                    -expand,
                    -expand,
                    bounds.width + expand * 2.0,
                    bounds.height + expand * 2.0,
                );
                // Expand corner radius to follow the outline curve
                let outline_radius = CornerRadius {
                    top_left: (radius.top_left + expand).max(0.0),
                    top_right: (radius.top_right + expand).max(0.0),
                    bottom_right: (radius.bottom_right + expand).max(0.0),
                    bottom_left: (radius.bottom_left + expand).max(0.0),
                };
                let stroke = Stroke::new(ow);
                ctx.stroke_rect(
                    outline_rect,
                    outline_radius,
                    &stroke,
                    Brush::Solid(*outline_color),
                );
            }
        }

        // Restore foreground layer state after border/outline rendering
        if border_in_foreground {
            ctx.set_foreground_layer(false);
        }

        // Push clip if this element clips its children (e.g., scroll containers)
        // Clip to content area (inset by border width so children don't render over border)
        // This matches CSS overflow:hidden behavior which clips to the padding box
        let clips_content = render_node.props.clips_content;
        if clips_content {
            // Calculate border insets from either uniform border or per-side borders
            let sides = &render_node.props.border_sides;
            let uniform_border = render_node.props.border_width;

            let left_inset = sides
                .left
                .as_ref()
                .map(|b| b.width)
                .unwrap_or(uniform_border);
            let right_inset = sides
                .right
                .as_ref()
                .map(|b| b.width)
                .unwrap_or(uniform_border);
            let top_inset = sides
                .top
                .as_ref()
                .map(|b| b.width)
                .unwrap_or(uniform_border);
            let bottom_inset = sides
                .bottom
                .as_ref()
                .map(|b| b.width)
                .unwrap_or(uniform_border);

            // Inset clip by border width to exclude border area from clipping region
            let clip_rect = Rect::new(
                left_inset,
                top_inset,
                (bounds.width - left_inset - right_inset).max(0.0),
                (bounds.height - top_inset - bottom_inset).max(0.0),
            );
            // Adjust corner radius for inset - use max border width for corner adjustment
            let max_border = left_inset.max(right_inset).max(top_inset).max(bottom_inset);
            let inset_radius = if radius.is_uniform() && radius.top_left > max_border {
                CornerRadius::uniform((radius.top_left - max_border).max(0.0))
            } else {
                CornerRadius::default()
            };
            // Set overflow fade before pushing clip
            if !render_node.props.overflow_fade.is_none() {
                ctx.set_overflow_fade(render_node.props.overflow_fade.to_array());
            }
            let clip_shape = if inset_radius.top_left > 0.0 {
                ClipShape::rounded_rect(clip_rect, inset_radius)
            } else {
                ClipShape::rect(clip_rect)
            };
            ctx.push_clip(clip_shape);
        }

        // Check if this node has scroll and apply the offset
        let scroll_offset = self.get_scroll_offset(node);
        let has_scroll = scroll_offset.0.abs() > 0.001 || scroll_offset.1.abs() > 0.001;

        if has_scroll {
            // Apply scroll offset as a transform
            // Positive offset_y = scrolled down = content moves up = negative translation
            ctx.push_transform(Transform::translate(scroll_offset.0, scroll_offset.1));
        }

        // Clear corner shape before rendering children — not inherited
        if has_corner_shape_l {
            ctx.clear_corner_shape();
        }

        // Render children (relative to this node's transform + scroll offset)
        // Reset cumulative scroll when entering a scroll container.
        let is_scroll_container = self.scroll_physics.contains_key(&node);
        let new_cumulative = if is_scroll_container {
            (scroll_offset.0, scroll_offset.1)
        } else {
            (
                cumulative_scroll.0 + scroll_offset.0,
                cumulative_scroll.1 + scroll_offset.1,
            )
        };
        for child_id in self.layout_tree.children(node) {
            let child_render = self.render_nodes.get(&child_id);
            let child_is_fixed = child_render.map(|n| n.props.is_fixed).unwrap_or(false);
            let child_is_sticky = child_render.map(|n| n.props.is_sticky).unwrap_or(false);

            let has_counter = child_is_fixed
                && (new_cumulative.0.abs() > 0.001 || new_cumulative.1.abs() > 0.001);
            if has_counter {
                ctx.push_transform(Transform::translate(-new_cumulative.0, -new_cumulative.1));
            }

            // Sticky: compute corrective offset when element would scroll past threshold
            let mut has_sticky_correction = false;
            if child_is_sticky {
                if let Some(threshold) = child_render.and_then(|n| n.props.sticky_top) {
                    if let Some(cb) = self.get_render_bounds(child_id, (0.0, 0.0)) {
                        let visual_y = cb.y + new_cumulative.1;
                        if visual_y < threshold {
                            let correction = threshold - visual_y;
                            ctx.push_transform(Transform::translate(0.0, correction));
                            has_sticky_correction = true;
                        }
                    }
                }
            }

            let child_cum = if child_is_fixed {
                (0.0, 0.0)
            } else {
                new_cumulative
            };
            self.render_node(ctx, child_id, (0.0, 0.0), child_cum);
            if has_sticky_correction {
                ctx.pop_transform();
            }
            if has_counter {
                ctx.pop_transform();
            }
        }

        // Pop scroll transform if we pushed one
        if has_scroll {
            ctx.pop_transform();
        }

        // Render scrollbar overlay if this is a scroll container with visible scrollbar
        if let Some(physics) = self.scroll_physics.get(&node) {
            if let Ok(p) = physics.try_lock() {
                let info = p.scrollbar_render_info();
                tracing::trace!(
                    "Scrollbar: opacity={:.2}, show_v={}, show_h={}, state={:?}, content_h={:.0}, viewport_h={:.0}",
                    info.opacity,
                    info.show_vertical,
                    info.show_horizontal,
                    info.state,
                    p.content_height,
                    p.viewport_height
                );
                // Only render if scrollbar is visible (opacity > 0)
                if info.opacity > 0.01 {
                    tracing::trace!("Rendering scrollbar with opacity {:.2}", info.opacity);
                    self.render_scrollbar(ctx, bounds.width, bounds.height, &info);
                }
            }
        }

        // Pop clip if we pushed one
        if clips_content {
            ctx.pop_clip();
        }

        // Pop motion binding rotation (3 transforms for centering)
        if has_motion_rotation {
            ctx.pop_transform();
            ctx.pop_transform();
            ctx.pop_transform();
        }

        // Pop motion binding scale (3 transforms for centering)
        if has_motion_scale {
            ctx.pop_transform();
            ctx.pop_transform();
            ctx.pop_transform();
        }

        // Pop motion binding translation (1 transform)
        if has_motion_transform {
            ctx.pop_transform();
        }

        // Pop element-specific transforms if we pushed them (3 transforms for centering)
        if has_element_transform {
            ctx.pop_transform(); // pop translate(-center_x, -center_y)
            ctx.pop_transform(); // pop the actual transform
            ctx.pop_transform(); // pop translate(center_x, center_y)
        }

        // Pop transform
        ctx.pop_transform();
    }

    /// Render with layer separation for glass effects
    ///
    /// This method renders elements in three passes:
    /// 1. Background elements (will be blurred behind glass)
    /// 2. Glass elements (blur effect via Brush::Glass)
    /// 3. Foreground elements (on top, not blurred)
    ///
    /// **Important:** Children of glass elements are automatically rendered
    /// in the foreground pass - no need to mark them with `.foreground()`.
    ///
    /// All three layers are rendered to the same context. Glass elements
    /// are rendered as `Brush::Glass` which the GPU renderer handles
    /// by pushing to the glass primitive batch for multi-pass rendering.
    pub fn render_layered_simple(&self, ctx: &mut dyn DrawContext) {
        if let Some(root) = self.root {
            // Pass 1: Background (excludes children of glass elements)
            ctx.set_foreground_layer(false);
            self.render_layer(
                ctx,
                root,
                (0.0, 0.0),
                RenderLayer::Background,
                0,
                false,
                (0.0, 0.0),
            );

            // Pass 2: Glass - these render as Brush::Glass which becomes glass primitives
            self.render_layer(
                ctx,
                root,
                (0.0, 0.0),
                RenderLayer::Glass,
                0,
                false,
                (0.0, 0.0),
            );

            // Pass 3: Foreground (includes children of glass elements, rendered after glass)
            ctx.set_foreground_layer(true);
            self.render_layer(
                ctx,
                root,
                (0.0, 0.0),
                RenderLayer::Foreground,
                0,
                false,
                (0.0, 0.0),
            );
            ctx.set_foreground_layer(false);
        }
    }

    /// Render with motion animations from RenderState
    ///
    /// This method applies animated opacity, scale, and translation from motion
    /// animations stored in RenderState. Use this when you have elements wrapped
    /// in motion() containers.
    pub fn render_with_motion(
        &self,
        ctx: &mut dyn DrawContext,
        render_state: &crate::render_state::RenderState,
    ) {
        // Reset the visible-animation flag for this frame. Set inside
        // `render_layer_with_motion` whenever a node that drives a
        // per-frame redraw (Canvas, motion bindings, active motion
        // state) is actually painted. Read by callers via
        // `visible_anim_active()` after this returns to gate the
        // end-of-frame redraw chain.
        self.visible_anim_active.set(false);
        // Same lifecycle for the painted-node set: cleared here, grown
        // by the walk, queried via `painted_node_ids()` to filter
        // animating Statefuls down to those whose node is actually on
        // screen this frame.
        self.painted_node_ids.borrow_mut().clear();

        if let Some(root) = self.root {
            // Apply DPI scale factor if set (for HiDPI display support)
            let has_scale = self.scale_factor != 1.0;
            if has_scale {
                ctx.push_transform(Transform::scale(self.scale_factor, self.scale_factor));
            }

            // Pass 1: Background (primitives go to background batch)
            ctx.set_foreground_layer(false);
            self.render_layer_with_motion(
                ctx,
                root,
                (0.0, 0.0),
                RenderLayer::Background,
                0,     // glass_depth
                false, // inside_foreground
                render_state,
                1.0, // Start with full opacity at root
                (0.0, 0.0),
            );

            // Pass 2: Glass (primitives go to glass batch)
            self.render_layer_with_motion(
                ctx,
                root,
                (0.0, 0.0),
                RenderLayer::Glass,
                0,     // glass_depth
                false, // inside_foreground
                render_state,
                1.0, // Start with full opacity at root
                (0.0, 0.0),
            );

            // Pass 3: Foreground (primitives go to foreground batch, rendered after glass)
            ctx.set_foreground_layer(true);
            self.render_layer_with_motion(
                ctx,
                root,
                (0.0, 0.0),
                RenderLayer::Foreground,
                0,     // glass_depth
                false, // inside_foreground
                render_state,
                1.0, // Start with full opacity at root
                (0.0, 0.0),
            );
            ctx.set_foreground_layer(false);

            // Pop the DPI scale transform
            if has_scale {
                ctx.pop_transform();
            }
        }
    }

    /// Render a layer with motion animation support
    ///
    /// The `inherited_opacity` parameter allows parent motion containers to pass
    /// their opacity down to children, ensuring the entire motion group fades together.
    ///
    /// The `inside_foreground` parameter tracks whether we're inside a foreground element,
    /// ensuring all descendants of foreground elements also render in the foreground pass.
    #[allow(clippy::too_many_arguments)]
    fn render_layer_with_motion(
        &self,
        ctx: &mut dyn DrawContext,
        node: LayoutNodeId,
        parent_offset: (f32, f32),
        target_layer: RenderLayer,
        glass_depth: u32,
        inside_foreground: bool,
        render_state: &crate::render_state::RenderState,
        inherited_opacity: f32,
        cumulative_scroll: (f32, f32),
    ) {
        // Debug: uncomment to trace all nodes
        // eprintln!("render_layer_with_motion: visiting node {:?}, target_layer={:?}", node, target_layer);

        // Use animated bounds if a layout animation is active, otherwise use layout bounds
        let Some(bounds) = self.get_render_bounds(node, parent_offset) else {
            return;
        };

        // Check if this node has an active layout animation (for clipping children)
        // Need to check both node ID based and stable key based animations
        let has_layout_animation = self.is_layout_animating(node);

        let Some(render_node) = self.render_nodes.get(&node) else {
            tracing::trace!(
                "render_layer_with_motion: no render_node for {:?}, skipping",
                node
            );
            // eprintln!("render_layer_with_motion: no render_node for {:?}", node);
            return;
        };

        // Check if this node should be skipped (motion removed)
        // For stable-keyed motions, check by key; for node-based, check by node_id
        let motion_removed = if let Some(ref stable_key) = render_node.props.motion_stable_id {
            render_state.is_stable_motion_removed(stable_key)
        } else {
            render_state.is_motion_removed(node)
        };
        if motion_removed {
            return;
        }

        // CSS visibility: hidden — skip rendering but preserve layout space
        if !render_node.props.visible {
            return;
        }

        // Past every cull/skip gate above — this node is being painted
        // this frame. Record it so the windowed app can intersect with
        // animating Statefuls / CSS animations and skip the redraw
        // chain when their node is off-screen.
        //
        // We additionally clip the recorded set against the window
        // viewport: scroll containers without `viewport_cull(true)`
        // still walk and paint their off-screen children (the GPU
        // clips them at draw time), but for redraw-gating purposes
        // those children are NOT visible. Without this filter the
        // styling_demo — which has ~25 `infinite` keyframes laid out
        // far below the fold — kept the redraw chain alive at idle
        // even though the user couldn't see any of them.
        //
        // We MUST use absolute bounds here (`get_absolute_bounds`),
        // not the `bounds` variable above. `bounds` comes from
        // `get_render_bounds(node, parent_offset)` which the recursion
        // calls with `parent_offset = (0, 0)` — the parent's actual
        // offset is captured in the draw context's transform stack,
        // not in the bounds value. Comparing parent-relative bounds
        // against the absolute window viewport produced false
        // negatives for nested elements (an `#anim-pulse` deep inside
        // a section was excluded even when visually on screen),
        // breaking every keyframe animation.
        //
        // If the viewport hasn't been initialised yet (rect is empty
        // — true on the very first frame, before
        // `RenderState::set_viewport_size` is called) we fall back to
        // recording every painted node. Otherwise the gate would
        // filter the entire tree out and the chain would never start.
        let viewport = render_state.viewport();
        let viewport_known = viewport.width() > 0.0 && viewport.height() > 0.0;
        let intersects_viewport = !viewport_known
            || match self.layout_tree.get_absolute_bounds(node) {
                Some(abs) => {
                    let on_screen_x = abs.x + cumulative_scroll.0;
                    let on_screen_y = abs.y + cumulative_scroll.1;
                    on_screen_x < viewport.x() + viewport.width()
                        && on_screen_x + abs.width > viewport.x()
                        && on_screen_y < viewport.y() + viewport.height()
                        && on_screen_y + abs.height > viewport.y()
                }
                // No absolute bounds resolved — conservatively include
                // the node rather than filtering it out. Same posture
                // as the `viewport_known == false` branch above.
                None => true,
            };
        if intersects_viewport {
            self.painted_node_ids.borrow_mut().insert(node);
        }

        // Get motion values from RenderState (for entry/exit animations)
        // For stable-keyed motions (overlays), look up by key; otherwise by node_id
        let motion_values = if let Some(ref stable_key) = render_node.props.motion_stable_id {
            render_state.get_stable_motion_values(stable_key)
        } else {
            render_state.get_motion_values(node)
        };

        // Get motion bindings from RenderTree (for continuous AnimatedValue animations).
        //
        // Single HashMap lookup, then field-level queries on the reference.
        // Previously each of `get_motion_transform/opacity/scale/rotation`
        // did its own `motion_bindings.get(&node)` — for the ~95% of
        // nodes without bindings we paid 4 lookups every render pass to
        // get four `None`s. The `and_then` chains short-circuit at the
        // outer Option so non-bound nodes never reach the mutex-locked
        // accessors at all.
        let motion_bindings_ref = self.motion_bindings.get(&node);
        let binding_transform = motion_bindings_ref.and_then(|b| b.get_transform());
        let binding_opacity = motion_bindings_ref.and_then(|b| b.get_opacity());

        // We've passed all the cull / visibility / motion-removed
        // gates; this node is going to paint. Record whether it
        // drives a per-frame redraw — that flag is consulted at end
        // of frame to decide whether the animation-redraw signal
        // should keep the chain alive. Without this gate, an
        // off-screen spinner whose paint is culled still pinned the
        // chain at vsync because the scheduler's needs_redraw stays
        // true regardless of visibility.
        if !self.visible_anim_active.get() {
            let canvas_paints = matches!(render_node.element_type, ElementType::Canvas(_));
            // Bindings only count as a redraw signal when the
            // underlying animated value is *actually* mid-flight.
            // A settled spring binding (e.g. `cn::progress_animated`
            // after it reached 75 %) leaves the binding in place but
            // the value is now constant — including it here pinned
            // the chain at vsync forever.
            let has_active_binding = motion_bindings_ref.is_some_and(|b| b.is_any_animating());
            let has_active_motion = motion_values.is_some();
            if canvas_paints || has_active_binding || has_active_motion {
                self.visible_anim_active.set(true);
            }
        }

        // Calculate this node's motion opacity (combine motion values, bindings, and element opacity)
        let node_motion_opacity = motion_values
            .and_then(|m| m.opacity)
            .unwrap_or_else(|| binding_opacity.unwrap_or(1.0))
            * render_node.props.opacity;

        // Combine with inherited opacity from parent motion containers
        // This ensures children fade together with their parent motion container
        let motion_opacity = inherited_opacity * node_motion_opacity;

        // Skip rendering if completely transparent
        if motion_opacity <= 0.001 {
            return;
        }

        // Push position transform
        ctx.push_transform(Transform::translate(bounds.x, bounds.y));

        // Apply motion translation
        if let Some(motion) = motion_values {
            let (tx, ty) = motion.resolved_translate();
            if tx.abs() > 0.001 || ty.abs() > 0.001 {
                ctx.push_transform(Transform::translate(tx, ty));
            }
        }

        // Apply motion scale (centered)
        let has_motion_scale = motion_values
            .map(|m| {
                let (sx, sy) = m.resolved_scale();
                (sx - 1.0).abs() > 0.001 || (sy - 1.0).abs() > 0.001
            })
            .unwrap_or(false);

        if has_motion_scale {
            let (sx, sy) = motion_values.unwrap().resolved_scale();
            let center_x = bounds.width / 2.0;
            let center_y = bounds.height / 2.0;
            ctx.push_transform(Transform::translate(center_x, center_y));
            ctx.push_transform(Transform::scale(sx, sy));
            ctx.push_transform(Transform::translate(-center_x, -center_y));
        }

        // Apply motion binding transform if present (continuous AnimatedValue-driven animation)
        // Translation is NOT centered (moves element from its position)
        let has_binding_transform = binding_transform.is_some();
        if let Some(ref transform) = binding_transform {
            ctx.push_transform(transform.clone());
        }

        // Apply motion binding scale if present (centered around element).
        // Reuses the bindings reference fetched above — no extra HashMap lookup.
        let binding_scale = motion_bindings_ref.and_then(|b| b.get_scale());
        let has_binding_scale = binding_scale.is_some();
        if let Some((sx, sy)) = binding_scale {
            let center_x = bounds.width / 2.0;
            let center_y = bounds.height / 2.0;
            ctx.push_transform(Transform::translate(center_x, center_y));
            ctx.push_transform(Transform::scale(sx, sy));
            ctx.push_transform(Transform::translate(-center_x, -center_y));
        }

        // Apply motion binding rotation if present (centered around element).
        // Reuses the bindings reference fetched above — no extra HashMap lookup.
        let binding_rotation = motion_bindings_ref.and_then(|b| b.get_rotation());
        let has_binding_rotation = binding_rotation.is_some();
        if let Some(deg) = binding_rotation {
            let center_x = bounds.width / 2.0;
            let center_y = bounds.height / 2.0;
            ctx.push_transform(Transform::translate(center_x, center_y));
            ctx.push_transform(Transform::rotate(deg.to_radians()));
            ctx.push_transform(Transform::translate(-center_x, -center_y));
        }

        // Apply element-specific transform if present
        let has_element_transform = render_node.props.transform.is_some();
        if let Some(ref transform) = render_node.props.transform {
            // Use transform-origin if set, otherwise default to center
            let (origin_x, origin_y) =
                if let Some([ox_pct, oy_pct]) = render_node.props.transform_origin {
                    (
                        bounds.width * ox_pct / 100.0,
                        bounds.height * oy_pct / 100.0,
                    )
                } else {
                    (bounds.width / 2.0, bounds.height / 2.0)
                };
            ctx.push_transform(Transform::translate(origin_x, origin_y));
            ctx.push_transform(transform.clone());
            ctx.push_transform(Transform::translate(-origin_x, -origin_y));
        }

        // Determine if this node is a glass element
        let is_glass = matches!(render_node.props.material, Some(Material::Glass(_)));
        let children_glass_depth = if is_glass {
            glass_depth + 1
        } else {
            glass_depth
        };

        // Determine if this node is a foreground element
        let is_foreground = render_node.props.layer == RenderLayer::Foreground;
        let children_inside_foreground = inside_foreground || is_foreground;

        // Increment z_layer for Stack children for proper interleaved rendering
        // This ensures primitives AND text in each Stack layer render together
        let is_stack_layer = render_node.props.is_stack_layer;
        if is_stack_layer {
            let current_z = ctx.z_layer();
            ctx.set_z_layer(current_z + 1);
        }

        // Apply CSS z-index to z_layer for stacking order
        // Save current z_layer so we can restore it after this subtree
        let saved_z_layer = ctx.z_layer();
        let has_z_index = render_node.props.z_index > 0;
        if has_z_index {
            ctx.set_z_layer(render_node.props.z_index as u32);
        }

        // Determine effective layer:
        // - Children of glass elements (that aren't glass themselves) render in foreground
        // - Children of foreground elements also render in foreground
        // - Glass elements render in glass layer (both top-level and nested)
        // - Otherwise, use the node's explicit layer setting
        let effective_layer = if (glass_depth > 0 && !is_glass) || inside_foreground {
            RenderLayer::Foreground
        } else if is_glass {
            RenderLayer::Glass
        } else {
            render_node.props.layer
        };

        // Push layer if this node has partial opacity OR layer effects OR 3D CSS transform.
        // Children inside the layer automatically inherit the opacity via GPU composition.
        // Layer effects (blur, drop shadow, glow, color matrix) are applied when layer is composited.
        // 3D CSS transforms (rotate-x/rotate-y) use layer-based compositing: the entire subtree
        // (including text) renders flat to a texture, then the texture is composited with perspective
        // distortion. This ensures ALL children visually transform with the parent.
        // IMPORTANT: Only push layer when element's layer matches current target to avoid duplicate
        // layer commands across multiple render passes
        let has_layer_effects = !render_node.props.layer_effects.is_empty();
        let node_blend_mode = render_node
            .props
            .mix_blend_mode
            .unwrap_or(BlendMode::Normal);
        let has_blend_mode = node_blend_mode != BlendMode::Normal;
        // Detect 3D CSS transform (rotate-x/rotate-y on a FLAT container, not a 3D SDF shape)
        let has_3d_css_transform =
            render_node.props.rotate_x.is_some() || render_node.props.rotate_y.is_some();
        let has_3d_shape =
            render_node.props.depth.unwrap_or(0.0) > 0.0 || render_node.props.shape_3d.is_some();
        let use_3d_layer = has_3d_css_transform && !has_3d_shape;
        let has_opacity_layer =
            node_motion_opacity < 1.0 || has_layer_effects || has_blend_mode || use_3d_layer;
        let should_push_layer = has_opacity_layer && effective_layer == target_layer;
        if should_push_layer {
            // Scale layer effect radii by DPI factor (CSS px → physical px)
            let scaled_effects: Vec<LayerEffect> = render_node
                .props
                .layer_effects
                .iter()
                .map(|e| match e {
                    LayerEffect::Blur { radius, quality } => LayerEffect::Blur {
                        radius: radius * self.scale_factor,
                        quality: *quality,
                    },
                    LayerEffect::DropShadow {
                        offset_x,
                        offset_y,
                        blur,
                        spread,
                        color,
                    } => LayerEffect::DropShadow {
                        offset_x: offset_x * self.scale_factor,
                        offset_y: offset_y * self.scale_factor,
                        blur: blur * self.scale_factor,
                        spread: spread * self.scale_factor,
                        color: *color,
                    },
                    other => other.clone(),
                })
                .collect();
            // Build 3D transform params for layer compositing
            let transform_3d = if use_3d_layer {
                let rx = render_node.props.rotate_x.unwrap_or(0.0).to_radians();
                let ry = render_node.props.rotate_y.unwrap_or(0.0).to_radians();
                let d = render_node.props.perspective.unwrap_or(800.0);
                Some(blinc_core::Transform3DParams {
                    sin_rx: rx.sin(),
                    cos_rx: rx.cos(),
                    sin_ry: ry.sin(),
                    cos_ry: ry.cos(),
                    perspective_d: d * self.scale_factor,
                })
            } else {
                None
            };
            ctx.push_layer(LayerConfig {
                id: None,
                position: Some(blinc_core::Point::new(bounds.x, bounds.y)),
                size: Some(blinc_core::Size::new(bounds.width, bounds.height)),
                blend_mode: node_blend_mode,
                opacity: node_motion_opacity,
                depth: false,
                effects: scaled_effects,
                transform_3d,
            });
        }

        // Corner shape setup (superellipse per-corner) — MUST be set before draw_shadow
        // so shadows use the same corner_shape as the fill+border SDF.
        let has_corner_shape = !render_node.props.corner_shape.is_round();
        if has_corner_shape {
            ctx.set_corner_shape(render_node.props.corner_shape.to_array());
        }

        // Draw shadow BEFORE pushing clip (shadows extend beyond element bounds)
        // This must be done before the clip is applied so shadows aren't clipped
        let rect = Rect::new(0.0, 0.0, bounds.width, bounds.height);
        let radius = render_node.props.border_radius;
        if effective_layer == target_layer {
            // Glass elements have shadows handled by the GPU glass system
            if !matches!(render_node.props.material, Some(Material::Glass(_))) {
                if let Some(ref shadow) = render_node.props.shadow {
                    // When using opacity layer, draw shadow at full opacity (layer handles it)
                    // Otherwise, apply motion opacity to shadow color for fallback
                    let shadow = if !has_opacity_layer && motion_opacity < 1.0 {
                        Shadow {
                            color: Color::rgba(
                                shadow.color.r,
                                shadow.color.g,
                                shadow.color.b,
                                shadow.color.a * motion_opacity,
                            ),
                            ..*shadow
                        }
                    } else {
                        *shadow
                    };
                    ctx.draw_shadow(rect, radius, shadow);
                }
            }
        }

        // Determine if this element clips its content (overflow:hidden, scroll, or layout animation).
        // The actual clip push is deferred to after border/outline drawing so that the
        // overflow clip doesn't double-AA with the border SDF at the same boundary.
        // Per CSS spec, overflow clips the element's *content* (children), not its decoration
        // (background/border), which are already SDF-constrained to the element bounds.
        let clips_content = render_node.props.clips_content || has_layout_animation;

        // Push clip-path if set on this element
        let has_clip_path = render_node.props.clip_path.is_some();
        if has_clip_path {
            if let Some(cs) =
                Self::resolve_clip_path(render_node.props.clip_path.as_ref().unwrap(), &bounds)
            {
                ctx.push_clip(cs);
            }
        }

        // Render if this node matches target layer
        // Debug: see what layers we're checking
        let is_canvas = matches!(&render_node.element_type, ElementType::Canvas(_));
        if is_canvas {
            let matches = effective_layer == target_layer;
            // eprintln!(
            //     "render_layer_with_motion: Canvas node {:?}, effective_layer={:?}, target_layer={:?}, matches={}",
            //     node, effective_layer, target_layer, matches
            // );
            // if matches {
            //     eprintln!("  >>> Canvas layer MATCHES - will invoke callback");
            // }
        }
        // Set up 3D transform params on the paint context if this element has any.
        // When use_3d_layer is true, 3D CSS rotation is handled by layer compositing
        // (perspective distortion applied to the blit quad), NOT per-primitive.
        let has_3d = render_node.props.rotate_x.is_some()
            || render_node.props.rotate_y.is_some()
            || render_node.props.perspective.is_some()
            || render_node.props.depth.unwrap_or(0.0) > 0.0
            || render_node.props.translate_z.is_some()
            || render_node.props.shape_3d.is_some();

        if has_3d && !use_3d_layer {
            let rx = render_node.props.rotate_x.unwrap_or(0.0).to_radians();
            let ry = render_node.props.rotate_y.unwrap_or(0.0).to_radians();
            let d = render_node.props.perspective.unwrap_or(800.0);
            ctx.set_3d_transform(rx, ry, d);

            let is_3d_group = render_node.props.shape_3d == Some(6.0);
            if render_node.props.depth.unwrap_or(0.0) > 0.0 || is_3d_group {
                ctx.set_3d_shape(
                    render_node.props.shape_3d.unwrap_or(1.0),
                    render_node.props.depth.unwrap_or(0.0),
                    render_node.props.ambient.unwrap_or(0.3),
                    render_node.props.specular.unwrap_or(32.0),
                );
                ctx.set_3d_light(
                    render_node
                        .props
                        .light_direction
                        .unwrap_or([-0.5, -1.0, 0.5]),
                    render_node.props.light_intensity.unwrap_or(0.8),
                );
            }

            if let Some(tz) = render_node.props.translate_z {
                ctx.set_3d_translate_z(tz);
            }
        }

        // CSS filter setup
        let has_filter = render_node.props.filter.is_some();
        if let Some(f) = &render_node.props.filter {
            if !f.is_identity() {
                ctx.set_css_filter(
                    f.grayscale,
                    f.invert,
                    f.sepia,
                    f.hue_rotate,
                    f.brightness,
                    f.contrast,
                    f.saturate,
                );
            }
        }

        // Mask gradient setup (gradient masks are per-primitive, URL masks use LayerEffect)
        let has_mask_gradient = matches!(
            render_node.props.mask_image,
            Some(blinc_core::MaskImage::Gradient(_))
        );
        if let Some(blinc_core::MaskImage::Gradient(ref gradient)) = render_node.props.mask_image {
            let mask_mode_luminance = matches!(
                render_node.props.mask_mode,
                Some(blinc_core::MaskMode::Luminance)
            );
            match gradient {
                blinc_core::Gradient::Linear {
                    start, end, stops, ..
                } => {
                    let (start_alpha, end_alpha) =
                        Self::extract_mask_alphas(stops, mask_mode_luminance);
                    ctx.set_mask_gradient(
                        [start.x, start.y, end.x, end.y],
                        [1.0, start_alpha, end_alpha, 0.0],
                    );
                }
                blinc_core::Gradient::Radial {
                    center,
                    radius,
                    stops,
                    ..
                } => {
                    let (start_alpha, end_alpha) =
                        Self::extract_mask_alphas(stops, mask_mode_luminance);
                    ctx.set_mask_gradient(
                        [center.x, center.y, *radius, 0.0],
                        [2.0, start_alpha, end_alpha, 0.0],
                    );
                }
                blinc_core::Gradient::Conic { center, stops, .. } => {
                    // Treat conic as radial for mask purposes
                    let (start_alpha, end_alpha) =
                        Self::extract_mask_alphas(stops, mask_mode_luminance);
                    ctx.set_mask_gradient(
                        [center.x, center.y, 0.5, 0.0],
                        [2.0, start_alpha, end_alpha, 0.0],
                    );
                }
            }
        }

        // (corner_shape already set above, before draw_shadow)

        // 3D Group composition: collect child shapes into compound SDF
        // MUST happen before fill_rect so the primitive gets the group shape descriptors.
        let is_3d_group = render_node.props.shape_3d == Some(6.0);
        let mut group_3d_children: Vec<LayoutNodeId> = Vec::new();

        if is_3d_group {
            let mut raw_descs: Vec<[f32; 16]> = Vec::new();
            let group_cx = bounds.x + bounds.width * 0.5;
            let group_cy = bounds.y + bounds.height * 0.5;

            for child_id in self.layout_tree.children(node) {
                if let Some(child_node) = self.render_nodes.get(&child_id) {
                    if let Some(child_shape) = child_node.props.shape_3d {
                        if child_shape > 0.0 && child_shape < 6.0 {
                            group_3d_children.push(child_id);
                            let child_bounds = self.get_render_bounds(child_id, (0.0, 0.0));
                            if let Some(cb) = child_bounds {
                                let ox = cb.x + cb.width * 0.5 - group_cx;
                                let oy = cb.y + cb.height * 0.5 - group_cy;
                                let oz = child_node.props.translate_z.unwrap_or(0.0);
                                let cr = child_node
                                    .props
                                    .border_radius
                                    .top_left
                                    .min(child_node.props.depth.unwrap_or(20.0) * 0.5);
                                let child_depth = child_node.props.depth.unwrap_or(20.0);
                                let half_w = cb.width * 0.5;
                                let half_h = cb.height * 0.5;
                                let half_d = child_depth * 0.5;
                                let op_type = child_node.props.op_3d.unwrap_or(0.0);
                                let blend = child_node.props.blend_3d.unwrap_or(0.0);

                                // Get child color for per-shape coloring
                                let color = if let Some(blinc_core::Brush::Solid(c)) =
                                    &child_node.props.background
                                {
                                    [c.r, c.g, c.b, c.a]
                                } else {
                                    [0.8, 0.8, 0.8, 1.0]
                                };

                                // Pack as [offset(4), params(4), half_ext(4), color(4)]
                                raw_descs.push([
                                    ox,
                                    oy,
                                    oz,
                                    cr,
                                    child_shape,
                                    child_depth,
                                    op_type,
                                    blend,
                                    half_w,
                                    half_h,
                                    half_d,
                                    0.0,
                                    color[0],
                                    color[1],
                                    color[2],
                                    color[3],
                                ]);
                            }
                        }
                    }
                }
            }

            if !raw_descs.is_empty() {
                ctx.set_3d_group_raw(&raw_descs);
            }
        }

        if effective_layer == target_layer {
            // Motion opacity is now handled via push_layer when has_opacity_layer=true
            // The opacity layer applies opacity to all content via GPU composition

            // Pre-resolve per-side border widths and color.
            // When all border colors are the same, we merge into a single SDF primitive
            // (fill_rect_with_per_side_border) to avoid AA fringe from overlapping
            // fill + border primitives at rounded/squircle corners.
            let has_per_side_border = render_node.props.border_sides.has_any();
            let per_side_data: Option<([f32; 4], Color, bool)> = if has_per_side_border {
                let sides = &render_node.props.border_sides;
                let uw = render_node.props.border_width;
                let uc = render_node.props.border_color.unwrap_or(Color::TRANSPARENT);
                let top = sides
                    .top
                    .as_ref()
                    .map(|b| (b.width, b.color))
                    .unwrap_or((uw, uc));
                let right = sides
                    .right
                    .as_ref()
                    .map(|b| (b.width, b.color))
                    .unwrap_or((uw, uc));
                let bottom = sides
                    .bottom
                    .as_ref()
                    .map(|b| (b.width, b.color))
                    .unwrap_or((uw, uc));
                let left = sides
                    .left
                    .as_ref()
                    .map(|b| (b.width, b.color))
                    .unwrap_or((uw, uc));
                let widths = [top.0, right.0, bottom.0, left.0];
                let all_same_color = top.1 == right.1 && right.1 == bottom.1 && bottom.1 == left.1;
                let dominant = top.1; // all same when mergeable, otherwise pick first
                Some((widths, dominant, all_same_color))
            } else {
                None
            };
            // Merge per-side borders into the fill SDF when all border colors match.
            // Glass elements need separate foreground borders for compositing.
            // For clips_content elements, children are already clipped to inside the border
            // (inset clip at padding box), so merging is safe — no child can render over it.
            let all_same_per_side = per_side_data.as_ref().map(|d| d.2).unwrap_or(false);
            let merge_per_side = has_per_side_border && all_same_per_side && !is_glass;

            if let Some(Material::Glass(glass)) = &render_node.props.material {
                let glass_brush = Brush::Glass(GlassStyle {
                    blur: glass.blur,
                    tint: glass.tint,
                    saturation: glass.saturation,
                    brightness: glass.brightness,
                    noise: glass.noise,
                    border_thickness: glass.border_thickness,
                    shadow: render_node.props.shadow,
                    simple: glass.simple,
                    depth: glass_depth,
                    border_color: render_node.props.border_color,
                });
                ctx.fill_rect(rect, radius, glass_brush);
            } else {
                // Shadow already drawn before clip was pushed

                // Merge border into the fill primitive to avoid AA fringe at corners.
                // Only glass needs separate foreground borders (special compositing).
                // For clips_content: children are clipped to inside the border (inset clip),
                // so merging the border with the fill is safe.
                let has_uniform_border = !has_per_side_border
                    && render_node.props.border_width > 0.0
                    && render_node.props.border_color.is_some();
                let merge_border = (has_uniform_border && !is_glass) || merge_per_side;

                if let Some(ref bg) = render_node.props.background {
                    // When using opacity layer, draw at full opacity (layer handles it)
                    // Otherwise, apply motion opacity to brush for fallback
                    let brush = if !has_opacity_layer && motion_opacity < 1.0 {
                        apply_opacity_to_brush(bg, motion_opacity)
                    } else {
                        bg.clone()
                    };
                    if merge_per_side {
                        // Per-side border merged with fill for squircle/bevel/scoop support
                        let (widths, mut bc, _) = per_side_data.unwrap();
                        if !has_opacity_layer && motion_opacity < 1.0 {
                            bc.a *= motion_opacity;
                        }
                        ctx.fill_rect_with_per_side_border(rect, radius, brush, widths, bc);
                    } else if merge_border {
                        // Single primitive with fill + border — no AA overlap
                        let bw = render_node.props.border_width;
                        let mut bc = *render_node.props.border_color.as_ref().unwrap();
                        if !has_opacity_layer && motion_opacity < 1.0 {
                            bc.a *= motion_opacity;
                        }
                        ctx.fill_rect_with_per_side_border(
                            rect,
                            radius,
                            brush,
                            [bw, bw, bw, bw],
                            bc,
                        );
                    } else {
                        ctx.fill_rect(rect, radius, brush);
                    }
                } else if is_3d_group {
                    // 3D group elements need a primitive even without a background —
                    // the shader renders the compound SDF from child shape descriptors.
                    ctx.fill_rect(rect, radius, Brush::Solid(Color::TRANSPARENT));
                } else if merge_per_side {
                    // No background but per-side border with squircle — transparent fill
                    let (widths, mut bc, _) = per_side_data.unwrap();
                    if !has_opacity_layer && motion_opacity < 1.0 {
                        bc.a *= motion_opacity;
                    }
                    ctx.fill_rect_with_per_side_border(
                        rect,
                        radius,
                        Brush::Solid(Color::TRANSPARENT),
                        widths,
                        bc,
                    );
                } else if merge_border {
                    // No background but has uniform border — merge with transparent fill
                    let bw = render_node.props.border_width;
                    let mut bc = *render_node.props.border_color.as_ref().unwrap();
                    if !has_opacity_layer && motion_opacity < 1.0 {
                        bc.a *= motion_opacity;
                    }
                    ctx.fill_rect_with_per_side_border(
                        rect,
                        radius,
                        Brush::Solid(Color::TRANSPARENT),
                        [bw, bw, bw, bw],
                        bc,
                    );
                }
            }

            // Only glass needs foreground borders (special compositing).
            // For clips_content: children are clipped to inside the border by the inset
            // clip pushed later (padding box), so the merged border is never covered.
            let border_in_foreground = is_glass;
            if border_in_foreground {
                ctx.set_foreground_layer(true);
            }

            // Draw borders that weren't merged with the fill.
            // This only runs for per-side borders with different colors (can't merge)
            // or glass foreground borders.
            if has_per_side_border && !merge_per_side {
                if all_same_per_side {
                    // Same color but not merged (glass) — single SDF border primitive
                    let (widths, mut bc, _) = per_side_data.unwrap();
                    if !has_opacity_layer && motion_opacity < 1.0 {
                        bc.a *= motion_opacity;
                    }
                    ctx.fill_rect_with_per_side_border(
                        rect,
                        radius,
                        Brush::Solid(Color::TRANSPARENT),
                        widths,
                        bc,
                    );
                } else {
                    // Different colors per side — group by color, one SDF primitive per group.
                    // Each fill_rect_with_per_side_border call gets proper corner radius
                    // from the shader instead of using rectangular strips with clip.
                    let sides = &render_node.props.border_sides;
                    let uniform_width = render_node.props.border_width;
                    let uniform_color =
                        render_node.props.border_color.unwrap_or(Color::TRANSPARENT);

                    let apply_motion = |color: Color| -> Color {
                        if !has_opacity_layer && motion_opacity < 1.0 {
                            Color::rgba(color.r, color.g, color.b, color.a * motion_opacity)
                        } else {
                            color
                        }
                    };

                    // Resolve each side: (width, color)
                    let side_data: [(f32, Color); 4] = [
                        sides
                            .top
                            .as_ref()
                            .map(|b| (b.width, b.color))
                            .unwrap_or((uniform_width, uniform_color)),
                        sides
                            .right
                            .as_ref()
                            .map(|b| (b.width, b.color))
                            .unwrap_or((uniform_width, uniform_color)),
                        sides
                            .bottom
                            .as_ref()
                            .map(|b| (b.width, b.color))
                            .unwrap_or((uniform_width, uniform_color)),
                        sides
                            .left
                            .as_ref()
                            .map(|b| (b.width, b.color))
                            .unwrap_or((uniform_width, uniform_color)),
                    ];

                    // Group sides by color: collect unique colors and their widths
                    let mut color_groups: Vec<(Color, [f32; 4])> = Vec::with_capacity(4);
                    for (i, &(w, c)) in side_data.iter().enumerate() {
                        if w <= 0.0 {
                            continue;
                        }
                        if let Some(group) = color_groups.iter_mut().find(|(gc, _)| *gc == c) {
                            group.1[i] = w;
                        } else {
                            let mut widths = [0.0f32; 4];
                            widths[i] = w;
                            color_groups.push((c, widths));
                        }
                    }

                    for (color, widths) in color_groups {
                        ctx.fill_rect_with_per_side_border(
                            rect,
                            radius,
                            Brush::Solid(Color::TRANSPARENT),
                            widths,
                            apply_motion(color),
                        );
                    }
                }
            } else if render_node.props.border_width > 0.0 && border_in_foreground {
                // Glass uniform border — rendered in foreground on top of glass compositing
                if let Some(ref border_color) = render_node.props.border_color {
                    let bw = render_node.props.border_width;
                    let mut bc = *border_color;
                    if !has_opacity_layer && motion_opacity < 1.0 {
                        bc.a *= motion_opacity;
                    }
                    ctx.fill_rect_with_per_side_border(
                        rect,
                        radius,
                        Brush::Solid(Color::TRANSPARENT),
                        [bw, bw, bw, bw],
                        bc,
                    );
                }
            }

            // Draw outline outside the border
            if render_node.props.outline_width > 0.0 {
                if let Some(ref outline_color) = render_node.props.outline_color {
                    let ow = render_node.props.outline_width;
                    let offset = render_node.props.outline_offset;
                    let expand = offset + ow / 2.0;
                    let outline_rect = Rect::new(
                        -expand,
                        -expand,
                        bounds.width + expand * 2.0,
                        bounds.height + expand * 2.0,
                    );
                    let outline_radius = CornerRadius {
                        top_left: (radius.top_left + expand).max(0.0),
                        top_right: (radius.top_right + expand).max(0.0),
                        bottom_right: (radius.bottom_right + expand).max(0.0),
                        bottom_left: (radius.bottom_left + expand).max(0.0),
                    };
                    let stroke = Stroke::new(ow);
                    let brush = if !has_opacity_layer && motion_opacity < 1.0 {
                        let mut color = *outline_color;
                        color.a *= motion_opacity;
                        Brush::Solid(color)
                    } else {
                        Brush::Solid(*outline_color)
                    };
                    ctx.stroke_rect(outline_rect, outline_radius, &stroke, brush);
                }
            }

            // Restore foreground layer state after border/outline rendering
            if border_in_foreground {
                ctx.set_foreground_layer(false);
            }

            // Handle canvas elements.
            //
            // Only push a clip if the element explicitly opts into
            // overflow clipping (via `overflow_clip`). Unconditionally
            // clipping to the element's bbox breaks elements like the
            // notch, whose custom render emits primitives whose vertex
            // bounds LEGITIMATELY extend past the layout box (concave
            // corner expansion for the flares, blur expansion for a
            // drop shadow, etc.). Parent clips (e.g. scroll containers)
            // still apply via the clip stack, so honouring the
            // element's own overflow setting is enough.
            if let ElementType::Canvas(canvas_data) = &render_node.element_type {
                if let Some(render_fn) = &canvas_data.render_fn {
                    let should_clip = render_node.props.clips_content;
                    if should_clip {
                        let clip_rect = Rect::new(0.0, 0.0, bounds.width, bounds.height);
                        ctx.push_clip(ClipShape::rect(clip_rect));
                    }

                    // `bounds.x` / `bounds.y` are already translated
                    // onto the DrawContext by the `push_transform` at
                    // the top of `render_node`, so in canvas-local
                    // space the origin is (0, 0). Surfacing the
                    // pre-translate offset to the callback is a
                    // diagnostic breadcrumb, not a correction; forward
                    // zero for x/y so `Rect::new(bounds.x, bounds.y,
                    // …)` in callback code resolves to the canvas's
                    // actual origin without double-offsetting.
                    let canvas_bounds = crate::canvas::CanvasBounds {
                        x: 0.0,
                        y: 0.0,
                        width: bounds.width,
                        height: bounds.height,
                    };
                    render_fn(ctx, canvas_bounds);

                    if should_clip {
                        ctx.pop_clip();
                    }
                }
            }
        }

        // Clear corner shape before rendering children — corner-shape is NOT inherited.
        // It only affects the current node's own fill_rect/stroke_rect primitives.
        // Without this, a parent's corner-shape (e.g. squircle on .chat-card) would
        // leak into all descendant nodes that don't set their own corner-shape.
        if has_corner_shape {
            ctx.clear_corner_shape();
        }

        // Determine if this element has a border (needed for clip decisions below).
        let has_border =
            render_node.props.border_width > 0.0 || render_node.props.border_sides.has_any();

        // Push overflow clip for children. This is deferred from before the render block
        // so that the border/outline SDF doesn't get double-AA'd by an overlapping clip.
        // Background and borders are SDF-constrained; only children need the overflow clip.
        //
        // When there IS a border, skip the outer rounded clip entirely: the inset clip
        // (padding box) already prevents children from overflowing, and a rounded clip
        // at the same boundary as the border SDF creates visible AA doubling at corners.
        let push_outer_clip = clips_content && !has_border;
        if push_outer_clip {
            // Set overflow fade before pushing clip — fade distances consumed by push_clip
            if !render_node.props.overflow_fade.is_none() {
                ctx.set_overflow_fade(render_node.props.overflow_fade.to_array());
            }
            let clip_rect = Rect::new(0.0, 0.0, bounds.width, bounds.height);
            let clip_shape = if radius.is_uniform() && radius.top_left > 0.0 {
                ClipShape::rounded_rect(clip_rect, radius)
            } else {
                ClipShape::rect(clip_rect)
            };
            ctx.push_clip(clip_shape);
        }

        // Push inset clip for children if this element has borders.
        // This prevents children (including their shadows) from rendering
        // over the parent's border stroke.  The clip is at the padding box
        // (inside border, but padding area is still visible) per CSS spec.
        //
        // IMPORTANT: This clip must be pushed BEFORE the scroll transform so it
        // stays fixed in the element's viewport space.  If pushed after the
        // scroll transform the clip would drift with the scrolled content.
        let push_children_clip = clips_content && has_border;
        if push_children_clip {
            // Set overflow fade before pushing clip (when outer clip was skipped)
            if !push_outer_clip && !render_node.props.overflow_fade.is_none() {
                ctx.set_overflow_fade(render_node.props.overflow_fade.to_array());
            }
            // Calculate border insets from either uniform border or per-side borders
            let sides = &render_node.props.border_sides;
            let uniform_border = render_node.props.border_width;

            let border_left = sides
                .left
                .as_ref()
                .map(|b| b.width)
                .unwrap_or(uniform_border);
            let border_right = sides
                .right
                .as_ref()
                .map(|b| b.width)
                .unwrap_or(uniform_border);
            let border_top = sides
                .top
                .as_ref()
                .map(|b| b.width)
                .unwrap_or(uniform_border);
            let border_bottom = sides
                .bottom
                .as_ref()
                .map(|b| b.width)
                .unwrap_or(uniform_border);

            let clip_rect = Rect::new(
                border_left,
                border_top,
                (bounds.width - border_left - border_right).max(0.0),
                (bounds.height - border_top - border_bottom).max(0.0),
            );

            // Adjust corner radius for border inset
            let radius = render_node.props.border_radius;
            let max_inset = border_left
                .max(border_right)
                .max(border_top)
                .max(border_bottom);
            let inset_radius = if radius.is_uniform() && radius.top_left > max_inset {
                CornerRadius::uniform((radius.top_left - max_inset).max(0.0))
            } else {
                CornerRadius::default()
            };

            let clip_shape = if inset_radius.top_left > 0.0 {
                ClipShape::rounded_rect(clip_rect, inset_radius)
            } else {
                ClipShape::rect(clip_rect)
            };
            ctx.push_clip(clip_shape);
        }

        // Apply scroll offset (AFTER children inset clip so clip stays fixed)
        let scroll_offset = self.get_scroll_offset(node);
        let has_scroll = scroll_offset.0.abs() > 0.001 || scroll_offset.1.abs() > 0.001;
        if has_scroll {
            ctx.push_transform(Transform::translate(scroll_offset.0, scroll_offset.1));
        }

        // Render children, passing down the effective opacity and layer inheritance
        // When we pushed an opacity layer, pass 1.0 to children (layer handles the opacity)
        // Otherwise, pass the combined opacity for brush-based fallback
        let child_inherited_opacity = if has_opacity_layer {
            1.0
        } else {
            motion_opacity
        };

        // Compute new cumulative scroll for children
        // Reset cumulative scroll when entering a scroll container.
        // Sticky/fixed positioning is relative to the nearest scroll ancestor, not all ancestors.
        let is_scroll_container = self.scroll_physics.contains_key(&node);
        let new_cumulative_scroll = if is_scroll_container {
            (scroll_offset.0, scroll_offset.1)
        } else {
            (
                cumulative_scroll.0 + scroll_offset.0,
                cumulative_scroll.1 + scroll_offset.1,
            )
        };

        // Viewport culling: when this node opted in (`scroll().viewport_cull(true)`),
        // set the cull rect to its absolute layout bounds. The intersect
        // test below also reads absolute bounds for each child, so both
        // sides live in the same coordinate frame regardless of how
        // deeply nested the child is. The scroll's *offset* (which moves
        // children visually but not their layout coords) is applied to
        // each child's absolute position before the test — that's what
        // makes scrolled-out children fall outside the rect.
        let prev_cull_viewport = self.cull_viewport.get();
        let entered_cull = self.viewport_cull_scrolls.contains(&node);
        if entered_cull {
            if let Some(abs) = self.layout_tree.get_absolute_bounds(node) {
                self.cull_viewport
                    .set(Some((abs.x, abs.y, abs.width, abs.height)));
            }
        }

        for child_id in self.layout_tree.children(node) {
            // Skip 3D children of a group node — they're composed into the group SDF
            if is_3d_group && group_3d_children.contains(&child_id) {
                continue;
            }

            let child_render = self.render_nodes.get(&child_id);
            let child_is_fixed = child_render.map(|n| n.props.is_fixed).unwrap_or(false);
            let child_is_sticky = child_render.map(|n| n.props.is_sticky).unwrap_or(false);

            // Viewport cull: skip painting children whose post-scroll
            // *visual* position falls outside the active cull viewport.
            // Both `cb` and the cull rect are in absolute layout coords;
            // `new_cumulative_scroll` is the offset that the renderer
            // will apply via the transform stack when drawing this
            // descendant, so adding it to the absolute layout position
            // gives the child's actual on-screen rect. Fixed and sticky
            // children opt out — their visual position isn't determined
            // by `new_cumulative_scroll` alone.
            if let Some((cx, cy, cw, ch)) = self.cull_viewport.get() {
                if !child_is_fixed && !child_is_sticky {
                    if let Some(cb) = self.layout_tree.get_absolute_bounds(child_id) {
                        // 200 px overscan on each axis so a smooth scroll
                        // doesn't pop content in/out at the viewport edge.
                        const OVERSCAN: f32 = 200.0;
                        let vx0 = cx - OVERSCAN;
                        let vy0 = cy - OVERSCAN;
                        let vx1 = cx + cw + OVERSCAN;
                        let vy1 = cy + ch + OVERSCAN;
                        let bx0 = cb.x + new_cumulative_scroll.0;
                        let by0 = cb.y + new_cumulative_scroll.1;
                        let bx1 = bx0 + cb.width;
                        let by1 = by0 + cb.height;
                        let intersects = bx1 > vx0 && bx0 < vx1 && by1 > vy0 && by0 < vy1;
                        if !intersects {
                            continue;
                        }
                    }
                }
            }

            // Fixed: push counter-scroll to cancel ALL accumulated scroll
            let has_fixed_counter = child_is_fixed
                && (new_cumulative_scroll.0.abs() > 0.001 || new_cumulative_scroll.1.abs() > 0.001);
            if has_fixed_counter {
                ctx.push_transform(Transform::translate(
                    -new_cumulative_scroll.0,
                    -new_cumulative_scroll.1,
                ));
            }

            // Sticky: compute corrective offset when element would scroll past threshold
            let mut has_sticky_correction = false;
            if child_is_sticky {
                if let Some(threshold) = child_render.and_then(|n| n.props.sticky_top) {
                    if let Some(cb) = self.get_render_bounds(child_id, (0.0, 0.0)) {
                        // cb.y = element's layout y relative to parent
                        // new_cumulative_scroll.1 = total scroll from ALL ancestors
                        let visual_y = cb.y + new_cumulative_scroll.1;
                        if visual_y < threshold {
                            let correction = threshold - visual_y;
                            ctx.push_transform(Transform::translate(0.0, correction));
                            has_sticky_correction = true;
                        }
                    }
                }
            }

            let child_cumulative = if child_is_fixed {
                (0.0, 0.0) // Fixed cancels all accumulated scroll
            } else {
                new_cumulative_scroll
            };

            self.render_layer_with_motion(
                ctx,
                child_id,
                (0.0, 0.0),
                target_layer,
                children_glass_depth,
                children_inside_foreground,
                render_state,
                child_inherited_opacity,
                child_cumulative,
            );

            // Pop sticky correction
            if has_sticky_correction {
                ctx.pop_transform();
            }
            // Pop fixed counter-scroll
            if has_fixed_counter {
                ctx.pop_transform();
            }
        }

        // Restore the parent scope's cull viewport now that this
        // subtree is fully rendered. Pairs with the `set` above.
        if entered_cull {
            self.cull_viewport.set(prev_cull_viewport);
        }

        // Pop scroll transform (reverse of push order: scroll was pushed after children clip)
        if has_scroll {
            ctx.pop_transform();
        }

        // Render scrollbar overlay if this is a scroll container
        // Scrollbar is rendered after scroll transform is popped (in viewport space)
        // but before children inset clip is popped (clipped within content area)
        if effective_layer == target_layer {
            if let Some(physics) = self.scroll_physics.get(&node) {
                if let Ok(p) = physics.try_lock() {
                    let info = p.scrollbar_render_info();
                    if info.opacity > 0.01 {
                        self.render_scrollbar(ctx, bounds.width, bounds.height, &info);
                    }
                }
            }
        }

        // Pop children inset clip (pushed before scroll, so popped after)
        if push_children_clip {
            ctx.pop_clip();
        }

        // Pop outer overflow clip (only pushed for non-bordered elements)
        if push_outer_clip {
            ctx.pop_clip();
        }

        // Pop clip-path
        if has_clip_path {
            ctx.pop_clip();
        }

        // Pop opacity layer (must be after clips, before transforms)
        if should_push_layer {
            ctx.pop_layer();
        }

        // Pop element transforms
        if has_element_transform {
            ctx.pop_transform();
            ctx.pop_transform();
            ctx.pop_transform();
        }

        // Pop motion binding rotation (3 transforms for centering)
        if has_binding_rotation {
            ctx.pop_transform();
            ctx.pop_transform();
            ctx.pop_transform();
        }

        // Pop motion binding scale (3 transforms for centering)
        if has_binding_scale {
            ctx.pop_transform();
            ctx.pop_transform();
            ctx.pop_transform();
        }

        // Pop motion binding translation (1 transform)
        if has_binding_transform {
            ctx.pop_transform();
        }

        // Pop motion scale transforms (from RenderState motion)
        if has_motion_scale {
            ctx.pop_transform();
            ctx.pop_transform();
            ctx.pop_transform();
        }

        // Pop motion translation
        if motion_values
            .map(|m| {
                let (tx, ty) = m.resolved_translate();
                tx.abs() > 0.001 || ty.abs() > 0.001
            })
            .unwrap_or(false)
        {
            ctx.pop_transform();
        }

        // Clear 3D transient state
        if has_3d {
            ctx.clear_3d();
        }

        // Clear CSS filter transient state
        if has_filter {
            ctx.clear_css_filter();
        }

        // Clear mask gradient transient state
        if has_mask_gradient {
            ctx.clear_mask_gradient();
        }

        // (corner_shape already cleared before children — see above)

        // Restore z_layer after this subtree
        if has_z_index {
            ctx.set_z_layer(saved_z_layer);
        }

        // Pop position transform
        ctx.pop_transform();
    }

    /// Extract start and end alpha values from gradient stops for mask gradient
    fn extract_mask_alphas(stops: &[blinc_core::GradientStop], luminance: bool) -> (f32, f32) {
        if stops.is_empty() {
            return (1.0, 0.0);
        }
        let first = &stops[0].color;
        let last = &stops[stops.len() - 1].color;
        if luminance {
            // Luminance mode: use perceived luminance * alpha
            let lum_first = (0.2126 * first.r + 0.7152 * first.g + 0.0722 * first.b) * first.a;
            let lum_last = (0.2126 * last.r + 0.7152 * last.g + 0.0722 * last.b) * last.a;
            (lum_first, lum_last)
        } else {
            // Alpha mode: use color's alpha channel directly
            (first.a, last.a)
        }
    }

    /// Render with layer separation and explicit context control
    ///
    /// For cases where you need separate DrawContext instances for
    /// background and foreground (e.g., different render targets).
    ///
    /// **Important:** Children of glass elements are automatically rendered
    /// in the foreground pass - no need to mark them with `.foreground()`.
    ///
    /// Note: Glass elements are rendered to `glass_ctx` using `Brush::Glass`
    /// which the GPU renderer collects as glass primitives.
    pub fn render_layered(
        &self,
        background_ctx: &mut dyn DrawContext,
        glass_ctx: &mut dyn DrawContext,
        foreground_ctx: &mut dyn DrawContext,
    ) {
        if let Some(root) = self.root {
            // Pass 1: Background (excludes children of glass elements)
            self.render_layer(
                background_ctx,
                root,
                (0.0, 0.0),
                RenderLayer::Background,
                0,
                false,
                (0.0, 0.0),
            );

            // Pass 2: Glass - render as Brush::Glass
            self.render_layer(
                glass_ctx,
                root,
                (0.0, 0.0),
                RenderLayer::Glass,
                0,
                false,
                (0.0, 0.0),
            );

            // Pass 3: Foreground (includes children of glass elements)
            self.render_layer(
                foreground_ctx,
                root,
                (0.0, 0.0),
                RenderLayer::Foreground,
                0,
                false,
                (0.0, 0.0),
            );
        }
    }

    /// Render only elements in a specific layer to a DrawContext
    ///
    /// This is useful when you need to render background+glass to one context
    /// and foreground to another context (e.g., for proper glass compositing).
    ///
    /// **Important:** Children of glass elements are automatically considered
    /// as foreground - no need to mark them with `.foreground()`.
    pub fn render_to_layer(&self, ctx: &mut dyn DrawContext, target_layer: RenderLayer) {
        if let Some(root) = self.root {
            // Apply DPI scale factor if set (for HiDPI display support)
            let has_scale = self.scale_factor != 1.0;
            if has_scale {
                ctx.push_transform(Transform::scale(self.scale_factor, self.scale_factor));
            }

            self.render_layer(ctx, root, (0.0, 0.0), target_layer, 0, false, (0.0, 0.0));

            // Pop the DPI scale transform
            if has_scale {
                ctx.pop_transform();
            }
        }
    }

    /// Render only nodes in a specific layer
    ///
    /// The `inside_glass` flag tracks whether we're descending through a glass element.
    /// Children of glass elements are automatically rendered in the foreground pass.
    ///
    /// The `inside_foreground` flag tracks whether we're descending through a foreground element.
    /// Children of foreground elements are also rendered in the foreground pass.
    #[allow(clippy::too_many_arguments)]
    fn render_layer(
        &self,
        ctx: &mut dyn DrawContext,
        node: LayoutNodeId,
        parent_offset: (f32, f32),
        target_layer: RenderLayer,
        glass_depth: u32,
        inside_foreground: bool,
        cumulative_scroll: (f32, f32),
    ) {
        let Some(bounds) = self.layout_tree.get_bounds(node, parent_offset) else {
            return;
        };

        let Some(render_node) = self.render_nodes.get(&node) else {
            return;
        };

        // Always push transform for proper child positioning
        ctx.push_transform(Transform::translate(bounds.x, bounds.y));

        // Apply element-specific transform if present
        // Transforms are applied around the element's center (like CSS transform-origin: 50% 50%)
        let has_element_transform = render_node.props.transform.is_some();
        if let Some(ref transform) = render_node.props.transform {
            // To center transforms:
            // 1. Translate so element center is at origin
            // 2. Apply the user's transform
            // 3. Translate back
            let center_x = bounds.width / 2.0;
            let center_y = bounds.height / 2.0;
            ctx.push_transform(Transform::translate(center_x, center_y));
            ctx.push_transform(transform.clone());
            ctx.push_transform(Transform::translate(-center_x, -center_y));
        }

        // Determine if this node is a glass element
        let is_glass = matches!(render_node.props.material, Some(Material::Glass(_)));
        let children_glass_depth = if is_glass {
            glass_depth + 1
        } else {
            glass_depth
        };

        // Track if children should be considered inside foreground
        // Once inside foreground, stay inside foreground for all descendants
        let is_foreground = render_node.props.layer == RenderLayer::Foreground;
        let children_inside_foreground = inside_foreground || is_foreground;

        // Compute the effective layer for the layer-effect push gate
        // below — children inside glass / foreground render through the
        // foreground layer regardless of this node's authored setting.
        // (Same precedence the per-node render gate uses further down.)
        let effective_layer_for_push = if (glass_depth > 0 && !is_glass) || inside_foreground {
            RenderLayer::Foreground
        } else if is_glass {
            RenderLayer::Glass
        } else {
            render_node.props.layer
        };

        // Push a Blinc layer for any node that authored
        // `layer_effects` — `Div::blur` / `Div::layer_effect` and
        // anything reading `props.layer_effects`. Without this push
        // the effect entries on this node ride into the batch as
        // dead data and `apply_layer_effects` never runs (no
        // `LayerCommand::Push { effects: !empty }` is queued).
        // Symmetric with `render_layer_with_motion`'s richer push,
        // minus the motion-opacity / blend-mode / 3D plumbing this
        // simpler path doesn't track. Effect radii are scaled by
        // the DPI factor so CSS px line up with physical px in the
        // GPU effect kernels.
        let has_layer_effects_node = !render_node.props.layer_effects.is_empty();
        let should_push_layer = has_layer_effects_node && effective_layer_for_push == target_layer;
        if should_push_layer {
            let scaled_effects: Vec<blinc_core::LayerEffect> = render_node
                .props
                .layer_effects
                .iter()
                .map(|e| match e {
                    blinc_core::LayerEffect::Blur { radius, quality } => {
                        blinc_core::LayerEffect::Blur {
                            radius: radius * self.scale_factor,
                            quality: *quality,
                        }
                    }
                    blinc_core::LayerEffect::DropShadow {
                        offset_x,
                        offset_y,
                        blur,
                        spread,
                        color,
                    } => blinc_core::LayerEffect::DropShadow {
                        offset_x: offset_x * self.scale_factor,
                        offset_y: offset_y * self.scale_factor,
                        blur: blur * self.scale_factor,
                        spread: spread * self.scale_factor,
                        color: *color,
                    },
                    other => other.clone(),
                })
                .collect();
            ctx.push_layer(blinc_core::LayerConfig {
                id: None,
                position: Some(blinc_core::Point::new(bounds.x, bounds.y)),
                size: Some(blinc_core::Size::new(bounds.width, bounds.height)),
                blend_mode: blinc_core::BlendMode::Normal,
                opacity: 1.0,
                depth: false,
                effects: scaled_effects,
                transform_3d: None,
            });
        }

        // Push clip BEFORE rendering content if this element clips its children
        // Clip to content area (inset by border width so children don't render over border)
        // This matches CSS overflow:hidden behavior which clips to the padding box
        let clips_content = render_node.props.clips_content;
        if clips_content {
            // Calculate border insets from either uniform border or per-side borders
            let sides = &render_node.props.border_sides;
            let uniform_border = render_node.props.border_width;
            let radius = render_node.props.border_radius;

            let left_inset = sides
                .left
                .as_ref()
                .map(|b| b.width)
                .unwrap_or(uniform_border);
            let right_inset = sides
                .right
                .as_ref()
                .map(|b| b.width)
                .unwrap_or(uniform_border);
            let top_inset = sides
                .top
                .as_ref()
                .map(|b| b.width)
                .unwrap_or(uniform_border);
            let bottom_inset = sides
                .bottom
                .as_ref()
                .map(|b| b.width)
                .unwrap_or(uniform_border);

            // Inset clip by border width to exclude border area from clipping region
            let clip_rect = Rect::new(
                left_inset,
                top_inset,
                (bounds.width - left_inset - right_inset).max(0.0),
                (bounds.height - top_inset - bottom_inset).max(0.0),
            );
            // Adjust corner radius for inset - use max border width for corner adjustment
            let max_border = left_inset.max(right_inset).max(top_inset).max(bottom_inset);
            let inset_radius = if radius.is_uniform() && radius.top_left > max_border {
                CornerRadius::uniform((radius.top_left - max_border).max(0.0))
            } else {
                CornerRadius::default()
            };
            // Set overflow fade before pushing clip
            if !render_node.props.overflow_fade.is_none() {
                ctx.set_overflow_fade(render_node.props.overflow_fade.to_array());
            }
            let clip_shape = if inset_radius.top_left > 0.0 {
                ClipShape::rounded_rect(clip_rect, inset_radius)
            } else {
                ClipShape::rect(clip_rect)
            };
            ctx.push_clip(clip_shape);
        }

        // Determine the effective layer for this node:
        // - If we're inside a glass element, children render as foreground
        // - If we're inside a foreground element, children also render as foreground
        // - Otherwise, use the node's explicit layer setting
        let effective_layer = if (glass_depth > 0 && !is_glass) || inside_foreground {
            RenderLayer::Foreground
        } else if is_glass {
            RenderLayer::Glass
        } else {
            render_node.props.layer
        };

        // Corner shape setup — must be before draw_shadow so shadows match fill shape
        let has_corner_shape_n = !render_node.props.corner_shape.is_round();
        if has_corner_shape_n {
            ctx.set_corner_shape(render_node.props.corner_shape.to_array());
        }

        // Only render if this node matches the target layer
        if effective_layer == target_layer {
            let rect = Rect::new(0.0, 0.0, bounds.width, bounds.height);
            let radius = render_node.props.border_radius;

            // Check if this node has a glass material - if so, render as glass with shadow
            if let Some(Material::Glass(glass)) = &render_node.props.material {
                // For glass elements, pass shadow through GlassStyle to use GPU glass shadow system
                let glass_brush = Brush::Glass(GlassStyle {
                    blur: glass.blur,
                    tint: glass.tint,
                    saturation: glass.saturation,
                    brightness: glass.brightness,
                    noise: glass.noise,
                    border_thickness: glass.border_thickness,
                    shadow: render_node.props.shadow,
                    simple: glass.simple,
                    depth: glass_depth,
                    border_color: render_node.props.border_color,
                });
                ctx.fill_rect(rect, radius, glass_brush);
            } else {
                // For non-glass elements, draw shadow first (renders behind the element)
                if let Some(ref shadow) = render_node.props.shadow {
                    ctx.draw_shadow(rect, radius, *shadow);
                }

                // Pre-resolve border info for merging with fill
                let has_per_side_n = render_node.props.border_sides.has_any();
                let has_uniform_n = !has_per_side_n
                    && render_node.props.border_width > 0.0
                    && render_node.props.border_color.is_some();

                // Merge fill + border into single SDF primitive to avoid AA fringe at corners
                if let Some(ref bg) = render_node.props.background {
                    if has_per_side_n {
                        let sides = &render_node.props.border_sides;
                        let uw = render_node.props.border_width;
                        let uc = render_node.props.border_color.unwrap_or(Color::TRANSPARENT);
                        let top = sides
                            .top
                            .as_ref()
                            .map(|b| (b.width, b.color))
                            .unwrap_or((uw, uc));
                        let right = sides
                            .right
                            .as_ref()
                            .map(|b| (b.width, b.color))
                            .unwrap_or((uw, uc));
                        let bottom = sides
                            .bottom
                            .as_ref()
                            .map(|b| (b.width, b.color))
                            .unwrap_or((uw, uc));
                        let left = sides
                            .left
                            .as_ref()
                            .map(|b| (b.width, b.color))
                            .unwrap_or((uw, uc));
                        let all_same =
                            top.1 == right.1 && right.1 == bottom.1 && bottom.1 == left.1;
                        if all_same {
                            let widths = [top.0, right.0, bottom.0, left.0];
                            ctx.fill_rect_with_per_side_border(
                                rect,
                                radius,
                                bg.clone(),
                                widths,
                                top.1,
                            );
                        } else {
                            // Different colors — draw fill separately, borders as foreground later
                            ctx.fill_rect(rect, radius, bg.clone());
                        }
                    } else if has_uniform_n {
                        let bw = render_node.props.border_width;
                        let bc = *render_node.props.border_color.as_ref().unwrap();
                        ctx.fill_rect_with_per_side_border(
                            rect,
                            radius,
                            bg.clone(),
                            [bw, bw, bw, bw],
                            bc,
                        );
                    } else {
                        ctx.fill_rect(rect, radius, bg.clone());
                    }
                } else if has_per_side_n {
                    // No background but has border — transparent fill with border
                    let sides = &render_node.props.border_sides;
                    let uw = render_node.props.border_width;
                    let uc = render_node.props.border_color.unwrap_or(Color::TRANSPARENT);
                    let top = sides
                        .top
                        .as_ref()
                        .map(|b| (b.width, b.color))
                        .unwrap_or((uw, uc));
                    let right = sides
                        .right
                        .as_ref()
                        .map(|b| (b.width, b.color))
                        .unwrap_or((uw, uc));
                    let bottom = sides
                        .bottom
                        .as_ref()
                        .map(|b| (b.width, b.color))
                        .unwrap_or((uw, uc));
                    let left = sides
                        .left
                        .as_ref()
                        .map(|b| (b.width, b.color))
                        .unwrap_or((uw, uc));
                    let all_same = top.1 == right.1 && right.1 == bottom.1 && bottom.1 == left.1;
                    if all_same {
                        let widths = [top.0, right.0, bottom.0, left.0];
                        ctx.fill_rect_with_per_side_border(
                            rect,
                            radius,
                            Brush::Solid(Color::TRANSPARENT),
                            widths,
                            top.1,
                        );
                    }
                    // Different colors with no bg: handled below in foreground section
                } else if has_uniform_n {
                    let bw = render_node.props.border_width;
                    let bc = *render_node.props.border_color.as_ref().unwrap();
                    ctx.fill_rect_with_per_side_border(
                        rect,
                        radius,
                        Brush::Solid(Color::TRANSPARENT),
                        [bw, bw, bw, bw],
                        bc,
                    );
                }
            }

            // Only glass needs foreground borders (special compositing).
            // clips_content elements have children clipped to padding box (inset clip),
            // so the merged border is never covered.
            let border_in_foreground = is_glass;
            if border_in_foreground {
                ctx.set_foreground_layer(true);
            }

            // Draw borders that weren't merged with the fill above.
            // For non-glass: only different-color per-side borders need separate rendering.
            // For glass: all borders render as foreground.
            let has_per_side = render_node.props.border_sides.has_any();
            let has_uniform = !has_per_side
                && render_node.props.border_width > 0.0
                && render_node.props.border_color.is_some();
            let has_border = has_per_side || has_uniform;

            // Non-glass different-color per-side: need separate rendering
            let needs_separate_per_side = has_per_side && !border_in_foreground && {
                let sides = &render_node.props.border_sides;
                let uw = render_node.props.border_width;
                let uc = render_node.props.border_color.unwrap_or(Color::TRANSPARENT);
                let top = sides.top.as_ref().map(|b| b.color).unwrap_or(uc);
                let right = sides.right.as_ref().map(|b| b.color).unwrap_or(uc);
                let bottom = sides.bottom.as_ref().map(|b| b.color).unwrap_or(uc);
                let left = sides.left.as_ref().map(|b| b.color).unwrap_or(uc);
                !(top == right && right == bottom && bottom == left)
            };

            if needs_separate_per_side {
                // Different-color per-side borders — 4x fill_rect with clip
                let sides = &render_node.props.border_sides;
                let uw = render_node.props.border_width;
                let uc = render_node.props.border_color.unwrap_or(Color::TRANSPARENT);
                let top = sides
                    .top
                    .as_ref()
                    .map(|b| (b.width, b.color))
                    .unwrap_or((uw, uc));
                let right = sides
                    .right
                    .as_ref()
                    .map(|b| (b.width, b.color))
                    .unwrap_or((uw, uc));
                let bottom = sides
                    .bottom
                    .as_ref()
                    .map(|b| (b.width, b.color))
                    .unwrap_or((uw, uc));
                let left = sides
                    .left
                    .as_ref()
                    .map(|b| (b.width, b.color))
                    .unwrap_or((uw, uc));
                let has_radius = radius.top_left > 0.0
                    || radius.top_right > 0.0
                    || radius.bottom_left > 0.0
                    || radius.bottom_right > 0.0;
                if has_radius {
                    ctx.push_clip(ClipShape::rounded_rect(rect, radius));
                }
                if left.0 > 0.0 {
                    ctx.fill_rect(
                        Rect::new(0.0, 0.0, left.0, rect.height()),
                        CornerRadius::default(),
                        Brush::Solid(left.1),
                    );
                }
                if right.0 > 0.0 {
                    ctx.fill_rect(
                        Rect::new(rect.width() - right.0, 0.0, right.0, rect.height()),
                        CornerRadius::default(),
                        Brush::Solid(right.1),
                    );
                }
                if top.0 > 0.0 {
                    ctx.fill_rect(
                        Rect::new(0.0, 0.0, rect.width(), top.0),
                        CornerRadius::default(),
                        Brush::Solid(top.1),
                    );
                }
                if bottom.0 > 0.0 {
                    ctx.fill_rect(
                        Rect::new(0.0, rect.height() - bottom.0, rect.width(), bottom.0),
                        CornerRadius::default(),
                        Brush::Solid(bottom.1),
                    );
                }
                if has_radius {
                    ctx.pop_clip();
                }
            } else if has_border && border_in_foreground {
                // Glass foreground border — drawn separately on top of glass compositing
                if has_per_side {
                    let sides = &render_node.props.border_sides;
                    let uw = render_node.props.border_width;
                    let uc = render_node.props.border_color.unwrap_or(Color::TRANSPARENT);
                    let top = sides
                        .top
                        .as_ref()
                        .map(|b| (b.width, b.color))
                        .unwrap_or((uw, uc));
                    let right = sides
                        .right
                        .as_ref()
                        .map(|b| (b.width, b.color))
                        .unwrap_or((uw, uc));
                    let bottom = sides
                        .bottom
                        .as_ref()
                        .map(|b| (b.width, b.color))
                        .unwrap_or((uw, uc));
                    let left = sides
                        .left
                        .as_ref()
                        .map(|b| (b.width, b.color))
                        .unwrap_or((uw, uc));
                    let all_same = top.1 == right.1 && right.1 == bottom.1 && bottom.1 == left.1;
                    if all_same {
                        let widths = [top.0, right.0, bottom.0, left.0];
                        ctx.fill_rect_with_per_side_border(
                            rect,
                            radius,
                            Brush::Solid(Color::TRANSPARENT),
                            widths,
                            top.1,
                        );
                    } else {
                        let has_radius = radius.top_left > 0.0
                            || radius.top_right > 0.0
                            || radius.bottom_left > 0.0
                            || radius.bottom_right > 0.0;
                        if has_radius {
                            ctx.push_clip(ClipShape::rounded_rect(rect, radius));
                        }
                        if left.0 > 0.0 {
                            ctx.fill_rect(
                                Rect::new(0.0, 0.0, left.0, rect.height()),
                                CornerRadius::default(),
                                Brush::Solid(left.1),
                            );
                        }
                        if right.0 > 0.0 {
                            ctx.fill_rect(
                                Rect::new(rect.width() - right.0, 0.0, right.0, rect.height()),
                                CornerRadius::default(),
                                Brush::Solid(right.1),
                            );
                        }
                        if top.0 > 0.0 {
                            ctx.fill_rect(
                                Rect::new(0.0, 0.0, rect.width(), top.0),
                                CornerRadius::default(),
                                Brush::Solid(top.1),
                            );
                        }
                        if bottom.0 > 0.0 {
                            ctx.fill_rect(
                                Rect::new(0.0, rect.height() - bottom.0, rect.width(), bottom.0),
                                CornerRadius::default(),
                                Brush::Solid(bottom.1),
                            );
                        }
                        if has_radius {
                            ctx.pop_clip();
                        }
                    }
                } else if has_uniform {
                    let bw = render_node.props.border_width;
                    let bc = *render_node.props.border_color.as_ref().unwrap();
                    ctx.fill_rect_with_per_side_border(
                        rect,
                        radius,
                        Brush::Solid(Color::TRANSPARENT),
                        [bw, bw, bw, bw],
                        bc,
                    );
                }
            }

            // Draw outline outside the border
            if render_node.props.outline_width > 0.0 {
                if let Some(ref outline_color) = render_node.props.outline_color {
                    let ow = render_node.props.outline_width;
                    let offset = render_node.props.outline_offset;
                    let expand = offset + ow / 2.0;
                    let outline_rect = Rect::new(
                        -expand,
                        -expand,
                        bounds.width + expand * 2.0,
                        bounds.height + expand * 2.0,
                    );
                    let outline_radius = CornerRadius {
                        top_left: (radius.top_left + expand).max(0.0),
                        top_right: (radius.top_right + expand).max(0.0),
                        bottom_right: (radius.bottom_right + expand).max(0.0),
                        bottom_left: (radius.bottom_left + expand).max(0.0),
                    };
                    let stroke = Stroke::new(ow);
                    ctx.stroke_rect(
                        outline_rect,
                        outline_radius,
                        &stroke,
                        Brush::Solid(*outline_color),
                    );
                }
            }

            // Restore foreground layer state after border/outline rendering
            if border_in_foreground {
                ctx.set_foreground_layer(false);
            }

            // Handle canvas element rendering
            // Push clip to ensure canvas content respects parent bounds (e.g., scroll containers)
            if let ElementType::Canvas(canvas_data) = &render_node.element_type {
                if let Some(render_fn) = &canvas_data.render_fn {
                    // Push clip for canvas bounds - this ensures content doesn't render outside
                    let clip_rect = Rect::new(0.0, 0.0, bounds.width, bounds.height);
                    ctx.push_clip(ClipShape::rect(clip_rect));

                    // `bounds.x` / `bounds.y` are already translated
                    // onto the DrawContext by the `push_transform` at
                    // the top of `render_node`, so in canvas-local
                    // space the origin is (0, 0). Surfacing the
                    // pre-translate offset to the callback is a
                    // diagnostic breadcrumb, not a correction; forward
                    // zero for x/y so `Rect::new(bounds.x, bounds.y,
                    // …)` in callback code resolves to the canvas's
                    // actual origin without double-offsetting.
                    let canvas_bounds = crate::canvas::CanvasBounds {
                        x: 0.0,
                        y: 0.0,
                        width: bounds.width,
                        height: bounds.height,
                    };
                    render_fn(ctx, canvas_bounds);

                    ctx.pop_clip();
                }
            }
        }

        // Check if this node has a scroll offset and apply it to children
        let scroll_offset = self.get_scroll_offset(node);
        let has_scroll = scroll_offset.0.abs() > 0.001 || scroll_offset.1.abs() > 0.001;

        if has_scroll {
            // Apply scroll offset as a transform
            // Positive offset_y = scrolled down = content moves up = negative translation
            ctx.push_transform(Transform::translate(scroll_offset.0, scroll_offset.1));
        }

        // Clear corner shape before rendering children — not inherited
        if has_corner_shape_n {
            ctx.clear_corner_shape();
        }

        // Traverse children (they inherit our transform and layer inheritance)
        // Reset cumulative scroll when entering a scroll container.
        let is_scroll_container = self.scroll_physics.contains_key(&node);
        let new_cumulative = if is_scroll_container {
            (scroll_offset.0, scroll_offset.1)
        } else {
            (
                cumulative_scroll.0 + scroll_offset.0,
                cumulative_scroll.1 + scroll_offset.1,
            )
        };
        for child_id in self.layout_tree.children(node) {
            let child_render = self.render_nodes.get(&child_id);
            let child_is_fixed = child_render.map(|n| n.props.is_fixed).unwrap_or(false);
            let child_is_sticky = child_render.map(|n| n.props.is_sticky).unwrap_or(false);

            let has_counter = child_is_fixed
                && (new_cumulative.0.abs() > 0.001 || new_cumulative.1.abs() > 0.001);
            if has_counter {
                ctx.push_transform(Transform::translate(-new_cumulative.0, -new_cumulative.1));
            }

            // Sticky: compute corrective offset when element would scroll past threshold
            let mut has_sticky_correction = false;
            if child_is_sticky {
                if let Some(threshold) = child_render.and_then(|n| n.props.sticky_top) {
                    if let Some(cb) = self.get_render_bounds(child_id, (0.0, 0.0)) {
                        let visual_y = cb.y + new_cumulative.1;
                        if visual_y < threshold {
                            let correction = threshold - visual_y;
                            ctx.push_transform(Transform::translate(0.0, correction));
                            has_sticky_correction = true;
                        }
                    }
                }
            }

            let child_cum = if child_is_fixed {
                (0.0, 0.0)
            } else {
                new_cumulative
            };
            self.render_layer(
                ctx,
                child_id,
                (0.0, 0.0),
                target_layer,
                children_glass_depth,
                children_inside_foreground,
                child_cum,
            );
            if has_sticky_correction {
                ctx.pop_transform();
            }
            if has_counter {
                ctx.pop_transform();
            }
        }

        // Pop scroll transform if we pushed one
        if has_scroll {
            ctx.pop_transform();
        }

        // Pop clip if we pushed one
        if clips_content {
            ctx.pop_clip();
        }

        // Pop the layer-effects layer (must be after the clip pop so
        // primitives clipped by `clips_content` still land inside the
        // layer's offscreen, but before the element-transform pop so
        // the GPU effect bounds calc reads the right transform stack).
        if should_push_layer {
            ctx.pop_layer();
        }

        // Pop element-specific transforms if we pushed them (3 transforms for centering)
        if has_element_transform {
            ctx.pop_transform(); // pop translate(-center_x, -center_y)
            ctx.pop_transform(); // pop the actual transform
            ctx.pop_transform(); // pop translate(center_x, center_y)
        }

        ctx.pop_transform();
    }

    // `get_bounds`, `get_absolute_bounds`, `get_render_node`,
    // `get_node_padding`, `iter_nodes` moved to `renderer/queries.rs`.
    // `get_cursor`, `has_any_cursor_style`, `get_cursor_at` moved to
    // `renderer/cursor.rs`.

    /// Check if this tree contains any glass elements
    pub fn has_glass(&self) -> bool {
        self.render_nodes
            .values()
            .any(|node| matches!(node.props.material, Some(Material::Glass(_))))
    }

    /// Render the tree using a LayoutRenderer
    ///
    /// This is the primary rendering method. The LayoutRenderer handles:
    /// - Background/foreground layer separation (automatically if glass is present)
    /// - Text rendering at layout-computed positions
    /// - SVG rendering at layout-computed positions
    ///
    /// Example:
    /// ```ignore
    /// tree.render_to(&mut my_renderer);
    /// ```
    pub fn render_to<R: LayoutRenderer>(&self, renderer: &mut R) {
        if let Some(root) = self.root {
            // Pass 1: Background elements
            {
                let ctx = renderer.background();
                self.render_layer_with_content(
                    ctx,
                    root,
                    (0.0, 0.0),
                    RenderLayer::Background,
                    0,
                    (0.0, 0.0),
                );
            }

            // Pass 2: Glass elements (to background context)
            {
                let ctx = renderer.background();
                self.render_layer_with_content(
                    ctx,
                    root,
                    (0.0, 0.0),
                    RenderLayer::Glass,
                    0,
                    (0.0, 0.0),
                );
            }

            // Pass 3: Foreground elements (including glass children)
            {
                let ctx = renderer.foreground();
                self.render_layer_with_content(
                    ctx,
                    root,
                    (0.0, 0.0),
                    RenderLayer::Foreground,
                    0,
                    (0.0, 0.0),
                );
            }

            // Pass 4: Render text elements
            self.render_text_elements(renderer);

            // Pass 5: Render SVG elements
            self.render_svg_elements(renderer);
        }
    }

    /// Render a layer (divs only - text/SVG handled separately)
    fn render_layer_with_content(
        &self,
        ctx: &mut dyn DrawContext,
        node: LayoutNodeId,
        parent_offset: (f32, f32),
        target_layer: RenderLayer,
        glass_depth: u32,
        cumulative_scroll: (f32, f32),
    ) {
        let Some(bounds) = self.layout_tree.get_bounds(node, parent_offset) else {
            return;
        };

        let Some(render_node) = self.render_nodes.get(&node) else {
            return;
        };

        // Always push transform for proper child positioning
        ctx.push_transform(Transform::translate(bounds.x, bounds.y));

        // Apply element-specific transform if present
        // Transforms are applied around the element's center (like CSS transform-origin: 50% 50%)
        let has_element_transform = render_node.props.transform.is_some();
        if let Some(ref transform) = render_node.props.transform {
            // To center transforms:
            // 1. Translate so element center is at origin
            // 2. Apply the user's transform
            // 3. Translate back
            let center_x = bounds.width / 2.0;
            let center_y = bounds.height / 2.0;
            ctx.push_transform(Transform::translate(center_x, center_y));
            ctx.push_transform(transform.clone());
            ctx.push_transform(Transform::translate(-center_x, -center_y));
        }

        // Determine if this node is a glass element
        let is_glass = matches!(render_node.props.material, Some(Material::Glass(_)));

        // Track glass nesting depth for children
        let children_glass_depth = if is_glass {
            glass_depth + 1
        } else {
            glass_depth
        };

        // Push clip BEFORE rendering content if this element clips its children
        // Clip to content area (inset by border width so children don't render over border)
        // This matches CSS overflow:hidden behavior which clips to the padding box
        let clips_content = render_node.props.clips_content;
        if clips_content {
            // Inset clip by border width to exclude border area from clipping region
            let border_width = render_node.props.border_width;
            let radius = render_node.props.border_radius;
            let clip_rect = Rect::new(
                border_width,
                border_width,
                (bounds.width - border_width * 2.0).max(0.0),
                (bounds.height - border_width * 2.0).max(0.0),
            );
            // Adjust corner radius for inset - reduce by border width
            let inset_radius = if radius.is_uniform() && radius.top_left > border_width {
                CornerRadius::uniform((radius.top_left - border_width).max(0.0))
            } else {
                CornerRadius::default()
            };
            // Set overflow fade before pushing clip
            if !render_node.props.overflow_fade.is_none() {
                ctx.set_overflow_fade(render_node.props.overflow_fade.to_array());
            }
            let clip_shape = if inset_radius.top_left > 0.0 {
                ClipShape::rounded_rect(clip_rect, inset_radius)
            } else {
                ClipShape::rect(clip_rect)
            };
            ctx.push_clip(clip_shape);
        }

        // Determine the effective layer for this node
        let effective_layer = if (glass_depth > 0 && !is_glass)
            || render_node.props.layer == RenderLayer::Foreground
        {
            RenderLayer::Foreground
        } else if is_glass {
            RenderLayer::Glass
        } else {
            render_node.props.layer
        };

        // Only render divs and canvas here (text/SVG handled in separate passes)
        if effective_layer == target_layer {
            match &render_node.element_type {
                ElementType::Div => {
                    let rect = Rect::new(0.0, 0.0, bounds.width, bounds.height);
                    let radius = render_node.props.border_radius;

                    // Check if this node has a glass material - if so, render as glass with shadow
                    if let Some(Material::Glass(glass)) = &render_node.props.material {
                        // For glass elements, pass shadow through GlassStyle to use GPU glass shadow system
                        let glass_brush = Brush::Glass(GlassStyle {
                            blur: glass.blur,
                            tint: glass.tint,
                            saturation: glass.saturation,
                            brightness: glass.brightness,
                            noise: glass.noise,
                            border_thickness: glass.border_thickness,
                            shadow: render_node.props.shadow,
                            simple: glass.simple,
                            depth: glass_depth,
                            border_color: render_node.props.border_color,
                        });
                        ctx.fill_rect(rect, radius, glass_brush);
                    } else {
                        // For non-glass elements, draw shadow first (renders behind the element)
                        if let Some(ref shadow) = render_node.props.shadow {
                            ctx.draw_shadow(rect, radius, *shadow);
                        }

                        // Merge fill + border into a single SDF primitive when possible.
                        let sides = &render_node.props.border_sides;
                        let has_per_side = sides.has_any();
                        let has_uniform = !has_per_side
                            && render_node.props.border_width > 0.0
                            && render_node.props.border_color.is_some();

                        if has_per_side {
                            let uw = render_node.props.border_width;
                            let uc = render_node.props.border_color.unwrap_or(Color::TRANSPARENT);
                            let top = sides
                                .top
                                .as_ref()
                                .map(|b| (b.width, b.color))
                                .unwrap_or((uw, uc));
                            let right = sides
                                .right
                                .as_ref()
                                .map(|b| (b.width, b.color))
                                .unwrap_or((uw, uc));
                            let bottom = sides
                                .bottom
                                .as_ref()
                                .map(|b| (b.width, b.color))
                                .unwrap_or((uw, uc));
                            let left = sides
                                .left
                                .as_ref()
                                .map(|b| (b.width, b.color))
                                .unwrap_or((uw, uc));
                            let all_same =
                                top.1 == right.1 && right.1 == bottom.1 && bottom.1 == left.1;

                            if all_same {
                                let brush = render_node
                                    .props
                                    .background
                                    .clone()
                                    .unwrap_or(Brush::Solid(Color::TRANSPARENT));
                                ctx.fill_rect_with_per_side_border(
                                    rect,
                                    radius,
                                    brush,
                                    [top.0, right.0, bottom.0, left.0],
                                    top.1,
                                );
                            } else {
                                // Different colors — draw fill then 4x fill_rect
                                if let Some(ref bg) = render_node.props.background {
                                    ctx.fill_rect(rect, radius, bg.clone());
                                }
                                let has_radius = radius.top_left > 0.0
                                    || radius.top_right > 0.0
                                    || radius.bottom_left > 0.0
                                    || radius.bottom_right > 0.0;
                                if has_radius {
                                    ctx.push_clip(ClipShape::rounded_rect(rect, radius));
                                }
                                if let Some(ref b) = sides.left {
                                    if b.is_visible() {
                                        ctx.fill_rect(
                                            Rect::new(0.0, 0.0, b.width, rect.height()),
                                            CornerRadius::default(),
                                            Brush::Solid(b.color),
                                        );
                                    }
                                }
                                if let Some(ref b) = sides.right {
                                    if b.is_visible() {
                                        ctx.fill_rect(
                                            Rect::new(
                                                rect.width() - b.width,
                                                0.0,
                                                b.width,
                                                rect.height(),
                                            ),
                                            CornerRadius::default(),
                                            Brush::Solid(b.color),
                                        );
                                    }
                                }
                                if let Some(ref b) = sides.top {
                                    if b.is_visible() {
                                        ctx.fill_rect(
                                            Rect::new(0.0, 0.0, rect.width(), b.width),
                                            CornerRadius::default(),
                                            Brush::Solid(b.color),
                                        );
                                    }
                                }
                                if let Some(ref b) = sides.bottom {
                                    if b.is_visible() {
                                        ctx.fill_rect(
                                            Rect::new(
                                                0.0,
                                                rect.height() - b.width,
                                                rect.width(),
                                                b.width,
                                            ),
                                            CornerRadius::default(),
                                            Brush::Solid(b.color),
                                        );
                                    }
                                }
                                if has_radius {
                                    ctx.pop_clip();
                                }
                            }
                        } else if has_uniform {
                            let bw = render_node.props.border_width;
                            let bc = *render_node.props.border_color.as_ref().unwrap();
                            let brush = render_node
                                .props
                                .background
                                .clone()
                                .unwrap_or(Brush::Solid(Color::TRANSPARENT));
                            ctx.fill_rect_with_per_side_border(
                                rect,
                                radius,
                                brush,
                                [bw, bw, bw, bw],
                                bc,
                            );
                        } else if let Some(ref bg) = render_node.props.background {
                            ctx.fill_rect(rect, radius, bg.clone());
                        }
                    }

                    // Only glass needs foreground borders.
                    let border_in_foreground = is_glass;
                    if border_in_foreground {
                        ctx.set_foreground_layer(true);
                    }

                    // Draw outline
                    if render_node.props.outline_width > 0.0 {
                        if let Some(ref outline_color) = render_node.props.outline_color {
                            let ow = render_node.props.outline_width;
                            let offset = render_node.props.outline_offset;
                            let expand = offset + ow / 2.0;
                            let outline_rect = Rect::new(
                                -expand,
                                -expand,
                                bounds.width + expand * 2.0,
                                bounds.height + expand * 2.0,
                            );
                            let outline_radius = CornerRadius {
                                top_left: (radius.top_left + expand).max(0.0),
                                top_right: (radius.top_right + expand).max(0.0),
                                bottom_right: (radius.bottom_right + expand).max(0.0),
                                bottom_left: (radius.bottom_left + expand).max(0.0),
                            };
                            let stroke = Stroke::new(ow);
                            ctx.stroke_rect(
                                outline_rect,
                                outline_radius,
                                &stroke,
                                Brush::Solid(*outline_color),
                            );
                        }
                    }

                    // Restore foreground layer state after border/outline rendering
                    if border_in_foreground {
                        ctx.set_foreground_layer(false);
                    }
                }
                ElementType::Canvas(canvas_data) => {
                    // Canvas element: invoke the render callback with DrawContext
                    // Push clip to ensure canvas content respects parent bounds (e.g., scroll containers)
                    if let Some(render_fn) = &canvas_data.render_fn {
                        // Push clip for canvas bounds - this ensures content doesn't render outside
                        let clip_rect = Rect::new(0.0, 0.0, bounds.width, bounds.height);
                        ctx.push_clip(ClipShape::rect(clip_rect));

                        let canvas_bounds = crate::canvas::CanvasBounds {
                            x: 0.0,
                            y: 0.0,
                            width: bounds.width,
                            height: bounds.height,
                        };
                        render_fn(ctx, canvas_bounds);

                        ctx.pop_clip();
                    }
                }
                // Text, SVG, Image are handled in separate passes
                _ => {}
            }
        }

        // Check if this node has a scroll offset and apply it to children
        let scroll_offset = self.get_scroll_offset(node);
        let has_scroll = scroll_offset.0.abs() > 0.001 || scroll_offset.1.abs() > 0.001;

        if has_scroll {
            // Apply scroll offset as a transform
            // Positive offset_y = scrolled down = content moves up = negative translation
            ctx.push_transform(Transform::translate(scroll_offset.0, scroll_offset.1));
        }

        // Update cumulative scroll for children
        // Reset cumulative scroll when entering a scroll container.
        // Sticky/fixed positioning is relative to the nearest scroll ancestor, not all ancestors.
        let is_scroll_container = self.scroll_physics.contains_key(&node);
        let new_cumulative_scroll = if is_scroll_container {
            (scroll_offset.0, scroll_offset.1)
        } else {
            (
                cumulative_scroll.0 + scroll_offset.0,
                cumulative_scroll.1 + scroll_offset.1,
            )
        };

        // Traverse children
        for child_id in self.layout_tree.children(node) {
            let child_render = self.render_nodes.get(&child_id);
            let child_is_fixed = child_render.map(|n| n.props.is_fixed).unwrap_or(false);
            let child_is_sticky = child_render.map(|n| n.props.is_sticky).unwrap_or(false);

            // Fixed: push counter-scroll to cancel ALL accumulated scroll
            let has_fixed_counter = child_is_fixed
                && (new_cumulative_scroll.0.abs() > 0.001 || new_cumulative_scroll.1.abs() > 0.001);
            if has_fixed_counter {
                ctx.push_transform(Transform::translate(
                    -new_cumulative_scroll.0,
                    -new_cumulative_scroll.1,
                ));
            }

            // Sticky: compute corrective offset when element would scroll past threshold
            let mut has_sticky_correction = false;
            if child_is_sticky {
                if let Some(threshold) = child_render.and_then(|n| n.props.sticky_top) {
                    if let Some(cb) = self.layout_tree.get_bounds(child_id, (0.0, 0.0)) {
                        let visual_y = cb.y + new_cumulative_scroll.1;
                        if visual_y < threshold {
                            let correction = threshold - visual_y;
                            ctx.push_transform(Transform::translate(0.0, correction));
                            has_sticky_correction = true;
                        }
                    }
                }
            }

            let child_cumulative = if child_is_fixed {
                (0.0, 0.0)
            } else {
                new_cumulative_scroll
            };

            self.render_layer_with_content(
                ctx,
                child_id,
                (0.0, 0.0),
                target_layer,
                children_glass_depth,
                child_cumulative,
            );

            // Pop sticky correction
            if has_sticky_correction {
                ctx.pop_transform();
            }
            // Pop fixed counter-scroll
            if has_fixed_counter {
                ctx.pop_transform();
            }
        }

        // Pop scroll transform if we pushed one
        if has_scroll {
            ctx.pop_transform();
        }

        // Pop clip if we pushed one
        if clips_content {
            ctx.pop_clip();
        }

        // Pop element-specific transforms if we pushed them (3 transforms for centering)
        if has_element_transform {
            ctx.pop_transform(); // pop translate(-center_x, -center_y)
            ctx.pop_transform(); // pop the actual transform
            ctx.pop_transform(); // pop translate(center_x, center_y)
        }

        ctx.pop_transform();
    }

    /// Render all text elements via the LayoutRenderer
    fn render_text_elements<R: LayoutRenderer>(&self, renderer: &mut R) {
        if let Some(root) = self.root {
            self.render_text_recursive(renderer, root, (0.0, 0.0), 0, false, (0.0, 0.0));
        }
    }

    /// Recursively render text elements
    fn render_text_recursive<R: LayoutRenderer>(
        &self,
        renderer: &mut R,
        node: LayoutNodeId,
        parent_offset: (f32, f32),
        glass_depth: u32,
        inside_foreground: bool,
        cumulative_scroll: (f32, f32),
    ) {
        // Use get_render_bounds to get animated bounds if layout animation is active
        // This ensures text respects layout animations (FLIP-style bounds animation)
        let Some(bounds) = self.get_render_bounds(node, (0.0, 0.0)) else {
            return;
        };

        let Some(render_node) = self.render_nodes.get(&node) else {
            return;
        };

        // CSS visibility: hidden — skip text rendering but preserve layout space
        if !render_node.props.visible {
            return;
        }

        let is_glass = matches!(render_node.props.material, Some(Material::Glass(_)));
        let children_glass_depth = if is_glass {
            glass_depth + 1
        } else {
            glass_depth
        };

        // Track foreground inheritance
        let is_foreground = render_node.props.layer == RenderLayer::Foreground;
        let children_inside_foreground = inside_foreground || is_foreground;

        // Text inside glass or foreground goes to foreground layer
        let to_foreground = children_glass_depth > 0 || children_inside_foreground;

        if let ElementType::Text(text_data) = &render_node.element_type {
            // Absolute position for text
            let abs_x = parent_offset.0 + bounds.x;
            let abs_y = parent_offset.1 + bounds.y;

            // Use animated/overridden text color, font size and weight if available
            let color = render_node.props.text_color.unwrap_or(text_data.color);
            let font_size = render_node.props.font_size.unwrap_or(text_data.font_size);
            let weight = render_node.props.font_weight.unwrap_or(text_data.weight);

            // Render normal text
            if to_foreground {
                renderer.render_text_foreground(
                    &text_data.content,
                    abs_x,
                    abs_y,
                    bounds.width,
                    bounds.height,
                    font_size,
                    color,
                    text_data.align,
                    weight,
                );
            } else {
                renderer.render_text_background(
                    &text_data.content,
                    abs_x,
                    abs_y,
                    bounds.width,
                    bounds.height,
                    font_size,
                    color,
                    text_data.align,
                    weight,
                );
            }
        }

        // Calculate absolute position for this node's children:
        // - parent_offset: accumulated absolute position from ancestors (includes their scroll/motion)
        // - bounds.x/y: this node's position relative to parent (from Taffy layout)
        // - scroll_offset: this node's scroll offset (for scroll containers)
        // - motion_offset: this node's motion transform translation (for animated elements)
        let scroll_offset = self.get_scroll_offset(node);

        let motion_transform = self.get_motion_transform(node);
        let motion_offset = motion_transform
            .as_ref()
            .map(|t| match t {
                Transform::Affine2D(a) => (a.elements[4], a.elements[5]),
                _ => (0.0, 0.0),
            })
            .unwrap_or((0.0, 0.0));

        let new_offset = (
            parent_offset.0 + bounds.x + scroll_offset.0 + motion_offset.0,
            parent_offset.1 + bounds.y + scroll_offset.1 + motion_offset.1,
        );

        // Reset cumulative scroll when entering a scroll container.
        // Sticky/fixed positioning is relative to the nearest scroll ancestor, not all ancestors.
        let is_scroll_container = self.scroll_physics.contains_key(&node);
        let new_cumulative_scroll = if is_scroll_container {
            (scroll_offset.0, scroll_offset.1)
        } else {
            (
                cumulative_scroll.0 + scroll_offset.0,
                cumulative_scroll.1 + scroll_offset.1,
            )
        };

        for child_id in self.layout_tree.children(node) {
            let child_render = self.render_nodes.get(&child_id);
            let child_is_fixed = child_render.map(|n| n.props.is_fixed).unwrap_or(false);
            let child_is_sticky = child_render.map(|n| n.props.is_sticky).unwrap_or(false);

            let mut child_offset = new_offset;
            let child_cumulative;

            if child_is_fixed {
                // Cancel all accumulated scroll from the offset
                child_offset.0 -= new_cumulative_scroll.0;
                child_offset.1 -= new_cumulative_scroll.1;
                child_cumulative = (0.0, 0.0);
            } else if child_is_sticky {
                if let Some(threshold) = child_render.and_then(|n| n.props.sticky_top) {
                    if let Some(cb) = self.layout_tree.get_bounds(child_id, (0.0, 0.0)) {
                        let visual_y = cb.y + new_cumulative_scroll.1;
                        if visual_y < threshold {
                            let correction = threshold - visual_y;
                            child_offset.1 += correction;
                        }
                    }
                }
                child_cumulative = new_cumulative_scroll;
            } else {
                child_cumulative = new_cumulative_scroll;
            }

            self.render_text_recursive(
                renderer,
                child_id,
                child_offset,
                children_glass_depth,
                children_inside_foreground,
                child_cumulative,
            );
        }
    }

    /// Render all SVG elements via the LayoutRenderer
    fn render_svg_elements<R: LayoutRenderer>(&self, renderer: &mut R) {
        if let Some(root) = self.root {
            self.render_svg_recursive(renderer, root, (0.0, 0.0), 0, false, (0.0, 0.0));
        }
    }

    /// Recursively render SVG elements
    fn render_svg_recursive<R: LayoutRenderer>(
        &self,
        renderer: &mut R,
        node: LayoutNodeId,
        parent_offset: (f32, f32),
        glass_depth: u32,
        inside_foreground: bool,
        cumulative_scroll: (f32, f32),
    ) {
        // Use get_render_bounds to get animated bounds if layout animation is active
        // This ensures SVG respects layout animations (FLIP-style bounds animation)
        let Some(bounds) = self.get_render_bounds(node, (0.0, 0.0)) else {
            return;
        };

        let Some(render_node) = self.render_nodes.get(&node) else {
            return;
        };

        let is_glass = matches!(render_node.props.material, Some(Material::Glass(_)));
        let children_glass_depth = if is_glass {
            glass_depth + 1
        } else {
            glass_depth
        };

        // Track foreground inheritance
        let is_foreground = render_node.props.layer == RenderLayer::Foreground;
        let children_inside_foreground = inside_foreground || is_foreground;

        // SVG inside glass or foreground goes to foreground layer
        let to_foreground = children_glass_depth > 0 || children_inside_foreground;

        if let ElementType::Svg(svg_data) = &render_node.element_type {
            // Absolute position for SVG
            let abs_x = parent_offset.0 + bounds.x;
            let abs_y = parent_offset.1 + bounds.y;

            if to_foreground {
                renderer.render_svg_foreground(
                    &svg_data.source,
                    abs_x,
                    abs_y,
                    bounds.width,
                    bounds.height,
                    svg_data.tint,
                );
            } else {
                renderer.render_svg_background(
                    &svg_data.source,
                    abs_x,
                    abs_y,
                    bounds.width,
                    bounds.height,
                    svg_data.tint,
                );
            }
        }

        // Calculate absolute position for this node's children:
        // - parent_offset: accumulated absolute position from ancestors (includes their scroll/motion)
        // - bounds.x/y: this node's position relative to parent (from Taffy layout)
        // - scroll_offset: this node's scroll offset (for scroll containers)
        // - motion_offset: this node's motion transform translation (for animated elements)
        let scroll_offset = self.get_scroll_offset(node);

        let motion_offset = self
            .get_motion_transform(node)
            .map(|t| match t {
                Transform::Affine2D(a) => (a.elements[4], a.elements[5]),
                _ => (0.0, 0.0),
            })
            .unwrap_or((0.0, 0.0));

        let new_offset = (
            parent_offset.0 + bounds.x + scroll_offset.0 + motion_offset.0,
            parent_offset.1 + bounds.y + scroll_offset.1 + motion_offset.1,
        );

        // Reset cumulative scroll when entering a scroll container.
        // Sticky/fixed positioning is relative to the nearest scroll ancestor, not all ancestors.
        let is_scroll_container = self.scroll_physics.contains_key(&node);
        let new_cumulative_scroll = if is_scroll_container {
            (scroll_offset.0, scroll_offset.1)
        } else {
            (
                cumulative_scroll.0 + scroll_offset.0,
                cumulative_scroll.1 + scroll_offset.1,
            )
        };

        for child_id in self.layout_tree.children(node) {
            let child_render = self.render_nodes.get(&child_id);
            let child_is_fixed = child_render.map(|n| n.props.is_fixed).unwrap_or(false);
            let child_is_sticky = child_render.map(|n| n.props.is_sticky).unwrap_or(false);

            let mut child_offset = new_offset;
            let child_cumulative;

            if child_is_fixed {
                child_offset.0 -= new_cumulative_scroll.0;
                child_offset.1 -= new_cumulative_scroll.1;
                child_cumulative = (0.0, 0.0);
            } else if child_is_sticky {
                if let Some(threshold) = child_render.and_then(|n| n.props.sticky_top) {
                    if let Some(cb) = self.layout_tree.get_bounds(child_id, (0.0, 0.0)) {
                        let visual_y = cb.y + new_cumulative_scroll.1;
                        if visual_y < threshold {
                            let correction = threshold - visual_y;
                            child_offset.1 += correction;
                        }
                    }
                }
                child_cumulative = new_cumulative_scroll;
            } else {
                child_cumulative = new_cumulative_scroll;
            }

            self.render_svg_recursive(
                renderer,
                child_id,
                child_offset,
                children_glass_depth,
                children_inside_foreground,
                child_cumulative,
            );
        }
    }

    /// Collect all glass panels from the layout tree
    ///
    /// # Deprecated
    /// Use `render()` or `render_layered_simple()` instead. Glass elements
    /// are now rendered as `Brush::Glass` in the normal render pipeline.
    #[deprecated(
        since = "0.2.0",
        note = "Use render() or render_layered_simple() instead. Glass is now integrated into the normal render pipeline."
    )]
    #[allow(deprecated)]
    pub fn collect_glass_panels(&self) -> Vec<GlassPanel> {
        let mut panels = Vec::new();
        if let Some(root) = self.root {
            self.collect_glass_panels_recursive(root, (0.0, 0.0), &mut panels);
        }
        panels
    }

    /// Recursively collect glass panels (deprecated)
    #[allow(deprecated)]
    fn collect_glass_panels_recursive(
        &self,
        node: LayoutNodeId,
        parent_offset: (f32, f32),
        panels: &mut Vec<GlassPanel>,
    ) {
        let Some(bounds) = self.layout_tree.get_bounds(node, parent_offset) else {
            return;
        };

        if let Some(render_node) = self.render_nodes.get(&node) {
            // Check if this node has a glass material
            if let Some(Material::Glass(glass)) = &render_node.props.material {
                panels.push(GlassPanel {
                    bounds,
                    corner_radius: render_node.props.border_radius,
                    material: glass.clone(),
                    node_id: node,
                });
            }
        }

        // Traverse children
        let new_offset = (parent_offset.0 + bounds.x, parent_offset.1 + bounds.y);
        for child_id in self.layout_tree.children(node) {
            self.collect_glass_panels_recursive(child_id, new_offset, panels);
        }
    }

    // =========================================================================
    // Element iterators - for platform-specific text/SVG rendering
    // =========================================================================

    /// Get all text elements with their computed bounds
    ///
    /// Returns an iterator of (TextData, ElementBounds) for each text element
    /// in the tree. Use this to render text with your platform's text renderer.
    ///
    /// # Example
    /// ```ignore
    /// for (text, bounds) in tree.text_elements() {
    ///     my_renderer.draw_text(&text.content, bounds.x, bounds.y, text.font_size);
    /// }
    /// ```
    pub fn text_elements(&self) -> Vec<(TextData, ElementBounds)> {
        let mut result = Vec::new();
        if let Some(root) = self.root {
            self.collect_text_elements(root, (0.0, 0.0), &mut result, (0.0, 0.0));
        }
        result
    }

    fn collect_text_elements(
        &self,
        node: LayoutNodeId,
        parent_offset: (f32, f32),
        result: &mut Vec<(TextData, ElementBounds)>,
        cumulative_scroll: (f32, f32),
    ) {
        let Some(bounds) = self.layout_tree.get_bounds(node, parent_offset) else {
            return;
        };

        if let Some(render_node) = self.render_nodes.get(&node) {
            if let ElementType::Text(text_data) = &render_node.element_type {
                let abs_bounds = ElementBounds {
                    x: parent_offset.0 + bounds.x,
                    y: parent_offset.1 + bounds.y,
                    width: bounds.width,
                    height: bounds.height,
                };
                result.push((text_data.clone(), abs_bounds));
            }
        }

        // Include scroll offset when calculating child positions
        let scroll_offset = self.get_scroll_offset(node);
        let new_offset = (
            parent_offset.0 + bounds.x + scroll_offset.0,
            parent_offset.1 + bounds.y + scroll_offset.1,
        );
        // Reset cumulative scroll when entering a scroll container.
        // Sticky/fixed positioning is relative to the nearest scroll ancestor, not all ancestors.
        let is_scroll_container = self.scroll_physics.contains_key(&node);
        let new_cumulative_scroll = if is_scroll_container {
            (scroll_offset.0, scroll_offset.1)
        } else {
            (
                cumulative_scroll.0 + scroll_offset.0,
                cumulative_scroll.1 + scroll_offset.1,
            )
        };
        for child_id in self.layout_tree.children(node) {
            let child_render = self.render_nodes.get(&child_id);
            let child_is_fixed = child_render.map(|n| n.props.is_fixed).unwrap_or(false);
            let child_is_sticky = child_render.map(|n| n.props.is_sticky).unwrap_or(false);

            let mut child_offset = new_offset;
            let child_cumulative;

            if child_is_fixed {
                child_offset.0 -= new_cumulative_scroll.0;
                child_offset.1 -= new_cumulative_scroll.1;
                child_cumulative = (0.0, 0.0);
            } else if child_is_sticky {
                if let Some(threshold) = child_render.and_then(|n| n.props.sticky_top) {
                    if let Some(cb) = self.layout_tree.get_bounds(child_id, (0.0, 0.0)) {
                        let visual_y = cb.y + new_cumulative_scroll.1;
                        if visual_y < threshold {
                            let correction = threshold - visual_y;
                            child_offset.1 += correction;
                        }
                    }
                }
                child_cumulative = new_cumulative_scroll;
            } else {
                child_cumulative = new_cumulative_scroll;
            }

            self.collect_text_elements(child_id, child_offset, result, child_cumulative);
        }
    }

    /// Get all SVG elements with their computed bounds
    ///
    /// Returns an iterator of (SvgData, ElementBounds) for each SVG element
    /// in the tree. Use this to render SVGs with your platform's SVG renderer.
    ///
    /// # Example
    /// ```ignore
    /// for (svg, bounds) in tree.svg_elements() {
    ///     my_renderer.draw_svg(&svg.source, bounds.x, bounds.y, bounds.width, bounds.height);
    /// }
    /// ```
    pub fn svg_elements(&self) -> Vec<(SvgData, ElementBounds)> {
        let mut result = Vec::new();
        if let Some(root) = self.root {
            self.collect_svg_elements(root, (0.0, 0.0), &mut result, (0.0, 0.0));
        }
        result
    }

    fn collect_svg_elements(
        &self,
        node: LayoutNodeId,
        parent_offset: (f32, f32),
        result: &mut Vec<(SvgData, ElementBounds)>,
        cumulative_scroll: (f32, f32),
    ) {
        let Some(bounds) = self.layout_tree.get_bounds(node, parent_offset) else {
            return;
        };

        if let Some(render_node) = self.render_nodes.get(&node) {
            if let ElementType::Svg(svg_data) = &render_node.element_type {
                let abs_bounds = ElementBounds {
                    x: parent_offset.0 + bounds.x,
                    y: parent_offset.1 + bounds.y,
                    width: bounds.width,
                    height: bounds.height,
                };
                result.push((svg_data.clone(), abs_bounds));
            }
        }

        // Include scroll offset when calculating child positions
        let scroll_offset = self.get_scroll_offset(node);
        let new_offset = (
            parent_offset.0 + bounds.x + scroll_offset.0,
            parent_offset.1 + bounds.y + scroll_offset.1,
        );
        // Reset cumulative scroll when entering a scroll container.
        // Sticky/fixed positioning is relative to the nearest scroll ancestor, not all ancestors.
        let is_scroll_container = self.scroll_physics.contains_key(&node);
        let new_cumulative_scroll = if is_scroll_container {
            (scroll_offset.0, scroll_offset.1)
        } else {
            (
                cumulative_scroll.0 + scroll_offset.0,
                cumulative_scroll.1 + scroll_offset.1,
            )
        };
        for child_id in self.layout_tree.children(node) {
            let child_render = self.render_nodes.get(&child_id);
            let child_is_fixed = child_render.map(|n| n.props.is_fixed).unwrap_or(false);
            let child_is_sticky = child_render.map(|n| n.props.is_sticky).unwrap_or(false);

            let mut child_offset = new_offset;
            let child_cumulative;

            if child_is_fixed {
                child_offset.0 -= new_cumulative_scroll.0;
                child_offset.1 -= new_cumulative_scroll.1;
                child_cumulative = (0.0, 0.0);
            } else if child_is_sticky {
                if let Some(threshold) = child_render.and_then(|n| n.props.sticky_top) {
                    if let Some(cb) = self.layout_tree.get_bounds(child_id, (0.0, 0.0)) {
                        let visual_y = cb.y + new_cumulative_scroll.1;
                        if visual_y < threshold {
                            let correction = threshold - visual_y;
                            child_offset.1 += correction;
                        }
                    }
                }
                child_cumulative = new_cumulative_scroll;
            } else {
                child_cumulative = new_cumulative_scroll;
            }

            self.collect_svg_elements(child_id, child_offset, result, child_cumulative);
        }
    }
}

/// Apply opacity to a brush by modifying its alpha component
fn apply_opacity_to_brush(brush: &Brush, opacity: f32) -> Brush {
    match brush {
        Brush::Solid(color) => {
            Brush::Solid(Color::rgba(color.r, color.g, color.b, color.a * opacity))
        }
        Brush::Gradient(gradient) => {
            // For gradients, we'd need to modify both start and end colors
            // For now, just return the gradient as-is
            // TODO: Apply opacity to gradient stops
            Brush::Gradient(gradient.clone())
        }
        Brush::Glass(glass) => {
            // Glass already has its own opacity handling
            Brush::Glass(*glass)
        }
        Brush::Image(image) => {
            // Image brushes - return as-is for now
            // TODO: Apply opacity to image brush
            Brush::Image(image.clone())
        }
        Brush::Blur(blur) => {
            // Blur with adjusted opacity
            let mut blur_adjusted = *blur;
            blur_adjusted.opacity *= opacity;
            Brush::Blur(blur_adjusted)
        }
    }
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
}
