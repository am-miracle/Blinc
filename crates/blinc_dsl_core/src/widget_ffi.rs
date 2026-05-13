use super::*;
use crate::host::blinc_string_decode;

// =====================================================================
// Widget primitives — value-returning externs for `Div` / `Text`
// =====================================================================
//
// Each registered widget primitive has a matching extern here that boxes a
// `blinc_layout` value and returns the raw pointer as `i64`. Consumers
// reclaim via `Box::from_raw`. Handles are leak-on-construct until owned.

/// Tagged box carrying a concrete `blinc_layout` widget across the JIT boundary.
/// `Custom` carries arbitrary `ElementBuilder` implementations registered via
/// [`BlincDsl::register_extern_widget`].
pub enum WidgetBox {
    Text(Box<blinc_layout::text::Text>),
    Div(Box<blinc_layout::div::Div>),
    Custom(Box<dyn blinc_layout::div::ElementBuilder>),
}

impl WidgetBox {
    /// Coerce into a `Box<dyn ElementBuilder>` regardless of variant.
    pub fn into_element_builder(self) -> Box<dyn blinc_layout::div::ElementBuilder> {
        match self {
            WidgetBox::Text(t) => t,
            WidgetBox::Div(d) => d,
            WidgetBox::Custom(c) => c,
        }
    }
}

/// Per-call overlay of visual styling props. `Some` fields
/// override the wrapped widget's `render_props()`; `None` fields
/// leave it alone.
#[derive(Debug, Default, Clone)]
pub struct RenderPropsOverlay {
    pub background: Option<blinc_core::layer::Brush>,
    pub opacity: Option<f32>,
    /// Uniform corner radius (px). Maps to a four-corner-equal `CornerRadius`.
    pub corner_radius: Option<f32>,
    pub border_width: Option<f32>,
    pub border_color: Option<blinc_core::layer::Color>,
}

impl RenderPropsOverlay {
    pub fn apply_to(&self, base: &mut blinc_layout::RenderProps) {
        if let Some(bg) = self.background.clone() {
            base.background = Some(bg);
        }
        if let Some(o) = self.opacity {
            base.opacity = o;
        }
        if let Some(r) = self.corner_radius {
            base.border_radius = blinc_core::layer::CornerRadius::new(r, r, r, r);
            base.border_radius_explicit = true;
        }
        if let Some(w) = self.border_width {
            base.border_width = w;
        }
        if let Some(c) = self.border_color {
            base.border_color = Some(c);
        }
    }
}

/// Wraps a widget with a per-call styling overlay. Build /
/// children delegate to the inner widget; `render_props()`
/// merges the overlay.
pub struct Styled<W: blinc_layout::div::ElementBuilder> {
    inner: W,
    overlay: RenderPropsOverlay,
}

impl<W: blinc_layout::div::ElementBuilder> Styled<W> {
    pub fn new(widget: W, overlay: RenderPropsOverlay) -> Self {
        Self {
            inner: widget,
            overlay,
        }
    }
    pub fn inner(&self) -> &W {
        &self.inner
    }
    pub fn overlay(&self) -> &RenderPropsOverlay {
        &self.overlay
    }
}

impl<W: blinc_layout::div::ElementBuilder> blinc_layout::div::ElementBuilder for Styled<W> {
    fn build(&self, tree: &mut blinc_layout::LayoutTree) -> blinc_layout::LayoutNodeId {
        self.inner.build(tree)
    }
    fn render_props(&self) -> blinc_layout::RenderProps {
        let mut base = self.inner.render_props();
        self.overlay.apply_to(&mut base);
        base
    }
    fn children_builders(&self) -> &[Box<dyn blinc_layout::div::ElementBuilder>] {
        self.inner.children_builders()
    }
    // Forward identity so CSS class/id/type-name selectors match through the wrapper.
    fn element_classes(&self) -> &[std::sync::Arc<str>] {
        self.inner.element_classes()
    }
    fn element_id(&self) -> Option<&str> {
        self.inner.element_id()
    }
    fn element_type_id(&self) -> blinc_layout::div::ElementTypeId {
        self.inner.element_type_id()
    }
    // MUST forward — without this, on_click/on_hover on the inner Div never fire.
    fn event_handlers(&self) -> Option<&blinc_layout::event_handler::EventHandlers> {
        self.inner.event_handlers()
    }
    fn scroll_physics(&self) -> Option<blinc_layout::scroll::SharedScrollPhysics> {
        self.inner.scroll_physics()
    }
    fn visual_animation_config(
        &self,
    ) -> Option<blinc_layout::visual_animation::VisualAnimationConfig> {
        self.inner.visual_animation_config()
    }
}

/// Internals for the [`extern_widget`] attribute macro's code generation. Not stable API.
#[doc(hidden)]
pub mod __extern_widget_internals {
    pub use crate::{
        BlincDsl, BlincDslError, BlincDslResult, ExternWidget, ExternWidgetSpec,
        RenderPropsOverlay, Styled, WidgetBox,
    };
    pub use blinc_runtime::component::PropDef;
    pub use zyntax_typed_ast::type_registry::{PrimitiveType, Type};

    /// Reclaim a `RenderPropsOverlay` from `__new_style_overlay__` for macro thunks.
    ///
    /// # Safety
    ///
    /// `ptr` must come from `__new_style_overlay__`.
    pub unsafe fn decode_overlay(ptr: i64) -> crate::RenderPropsOverlay {
        unsafe { crate::materialize_overlay(ptr) }
    }

    /// Wrap an `ElementBuilder` in `WidgetBox::Custom` and leak as a `WidgetHandle`.
    pub fn into_handle(widget: Box<dyn blinc_layout::div::ElementBuilder>) -> i64 {
        Box::into_raw(Box::new(WidgetBox::Custom(widget))) as i64
    }

    /// Decode a Zyntax-FFI string argument (length-prefixed
    /// UTF-8 buffer) to an owned `String`. Empty for null.
    ///
    /// # Safety
    ///
    /// `ptr` must come from Zyntax JIT's String FFI lowering.
    pub unsafe fn decode_string(ptr: *const i32) -> String {
        if ptr.is_null() {
            return String::new();
        }
        // SAFETY: forwarded from caller per fn-level doc.
        let s = unsafe { crate::host::blinc_string_decode(ptr) };
        s.to_string()
    }

    /// Decode a `__new_child_list__` pointer into a `Vec<Box<dyn ElementBuilder>>`.
    /// Null/zero pointer means no children.
    ///
    /// # Safety
    ///
    /// `ptr` must be the `i64`-encoded payload of a list minted by `__new_child_list__`.
    pub unsafe fn decode_children(ptr: i64) -> Vec<Box<dyn blinc_layout::div::ElementBuilder>> {
        if ptr == 0 {
            return Vec::new();
        }
        // SAFETY: see fn-level doc.
        let handles: Vec<i64> = *unsafe { Box::from_raw(ptr as *mut Vec<i64>) };
        handles
            .into_iter()
            .filter_map(|h| {
                unsafe { super::materialize_widget(h) }.map(|w| w.into_element_builder())
            })
            .collect()
    }
}

/// Rust types callable from Blinc DSL source. Generated by `#[extern_widget]`,
/// or implemented manually. Register via `dsl.register_extern_widget::<T>()`.
pub trait ExternWidget {
    /// User-facing identifier — what `.blinc` source types to call the widget.
    const DSL_NAME: &'static str;

    /// Build the full registration spec.
    fn extern_widget_spec() -> ExternWidgetSpec;
}

/// Description of a Rust-side widget exposed to the DSL. Passed to
/// [`BlincDsl::register_extern_widget`]. `view_symbol` convention is
/// `$Blinc$<Name>$view`. `extern_ptr` must be `extern "C" fn(...)` matching
/// `param_types`, returning a `WidgetHandle` (`0` = null/build-failed).
pub struct ExternWidgetSpec {
    /// User-facing DSL name (e.g. `"Button"`).
    pub name: String,
    /// JIT-linker-visible symbol. Convention: `$Blinc$<Name>$view`.
    pub view_symbol: String,
    /// Substrate metadata about the widget's props.
    pub props: Vec<blinc_runtime::component::PropDef>,
    /// FFI parameter types in declaration order; must match `extern_ptr` exactly.
    pub param_types: Vec<Type>,
    /// FFI return type (typically `i64` for widget handle).
    pub return_type: Type,
    /// `extern "C" fn(...)` cast to `*const u8`.
    pub extern_ptr: *const u8,
}

/// Opaque widget handle across the JIT boundary. `Box<WidgetBox>` raw as `i64`,
/// with `0` as the null sentinel.
type WidgetHandle = i64;

/// Take ownership of a `WidgetHandle` returned by a view fn. `None` for `0`.
///
/// # Safety
///
/// `handle` must be from one of this crate's `$Blinc$<X>$view` externs (or `0`).
/// Each non-zero handle may be materialised exactly once.
pub unsafe fn materialize_widget(handle: WidgetHandle) -> Option<Box<WidgetBox>> {
    if handle == 0 {
        return None;
    }
    // SAFETY: see fn-level doc.
    Some(unsafe { Box::from_raw(handle as *mut WidgetBox) })
}

fn materialize_children(children: WidgetHandle) -> Vec<Box<dyn blinc_layout::div::ElementBuilder>> {
    if children == 0 {
        return Vec::new();
    }
    let list: Box<Vec<WidgetHandle>> = unsafe { Box::from_raw(children as *mut Vec<WidgetHandle>) };
    let mut out = Vec::with_capacity(list.len());
    for handle in *list {
        if let Some(child_box) = unsafe { materialize_widget(handle) } {
            out.push(child_box.into_element_builder());
        }
    }
    out
}

fn decode_string_arg(ptr: *const i32) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        // SAFETY: all string params use Zyntax's length-prefixed UTF-8 ABI.
        unsafe { blinc_string_decode(ptr) }.to_string()
    }
}

fn decoded_class_names(class_str: *const i32) -> Vec<String> {
    decode_string_arg(class_str)
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect()
}

fn leak_custom(widget: impl blinc_layout::div::ElementBuilder + 'static) -> WidgetHandle {
    Box::into_raw(Box::new(WidgetBox::Custom(Box::new(widget)))) as WidgetHandle
}

/// `$Blinc$Text$view(content, style, class) -> WidgetHandle`
///
/// # Safety
///
/// `content_ptr` must point at a Zyntax length-prefixed UTF-8 buffer.
pub(crate) extern "C" fn blinc_text_view(
    content_ptr: *const i32,
    style: i64,
    class_str: *const i32,
) -> WidgetHandle {
    if content_ptr.is_null() {
        tracing::warn!("$Blinc$Text$view called with null content pointer");
        return 0;
    }
    // SAFETY: see fn-level doc.
    let content = unsafe { blinc_string_decode(content_ptr) };
    let mut widget = blinc_layout::text::Text::new(content);
    for name in decoded_class_names(class_str) {
        widget = widget.class(name);
    }
    if style == 0 {
        Box::into_raw(Box::new(WidgetBox::Text(Box::new(widget)))) as WidgetHandle
    } else {
        let overlay = unsafe { materialize_overlay(style) };
        leak_custom(Styled::new(widget, overlay))
    }
}

/// `$Blinc$Div$view(children, style, class_str, on_click) -> WidgetHandle`.
/// Consumes the child-list and each child handle exactly once.
pub(crate) extern "C" fn blinc_div_view(
    children: WidgetHandle,
    style: i64,
    class_str: *const i32,
    on_click_closure: i64,
) -> WidgetHandle {
    let mut widget = blinc_layout::div::Div::new();
    for child in materialize_children(children) {
        widget = widget.child_box(child);
    }
    // SAFETY: `class_str` is `*const i32` per registered sig.
    for name in decoded_class_names(class_str) {
        widget = widget.class(name);
    }
    // `on_click` closure is a raw `extern "C" fn()` pointer minted by Zyntax's
    // `CreateClosure` → `func_addr`. Signal writes inside route through
    // `__signal_set_i32` → reactive `State::set` → stateful refresh.
    if on_click_closure != 0 {
        type ClosureFn = extern "C" fn();
        let func: ClosureFn = unsafe { std::mem::transmute(on_click_closure) };
        widget = widget.cursor_pointer().on_click(move |_ctx| {
            func();
        });
    }
    let overlay = unsafe { materialize_overlay(style) };
    leak_custom(Styled::new(widget, overlay))
}

/// `$Blinc$Stack$view(children, style) -> WidgetHandle`.
pub(crate) extern "C" fn blinc_stack_view(children: WidgetHandle, style: i64) -> WidgetHandle {
    let mut widget = blinc_layout::stack::Stack::new();
    for child in materialize_children(children) {
        widget = widget.child_box(child);
    }
    let overlay = unsafe { materialize_overlay(style) };
    leak_custom(Styled::new(widget, overlay))
}

/// `$Blinc$Image$view(source, style) -> WidgetHandle`.
pub(crate) extern "C" fn blinc_image_view(source_ptr: *const i32, style: i64) -> WidgetHandle {
    if source_ptr.is_null() {
        tracing::warn!("$Blinc$Image$view called with null source pointer");
        return 0;
    }
    let source = decode_string_arg(source_ptr);
    let widget = blinc_layout::image::Image::new(source);
    let overlay = unsafe { materialize_overlay(style) };
    leak_custom(Styled::new(widget, overlay))
}

/// `$Blinc$Svg$view(source, style, class) -> WidgetHandle`.
pub(crate) extern "C" fn blinc_svg_view(
    source_ptr: *const i32,
    style: i64,
    class_str: *const i32,
) -> WidgetHandle {
    if source_ptr.is_null() {
        tracing::warn!("$Blinc$Svg$view called with null source pointer");
        return 0;
    }
    let source = decode_string_arg(source_ptr);
    let mut widget = blinc_layout::svg::Svg::new(source);
    for name in decoded_class_names(class_str) {
        widget = widget.class(name);
    }
    let overlay = unsafe { materialize_overlay(style) };
    leak_custom(Styled::new(widget, overlay))
}

/// `$Blinc$Canvas$view(style) -> WidgetHandle`.
pub(crate) extern "C" fn blinc_canvas_view(style: i64) -> WidgetHandle {
    let widget = blinc_layout::canvas::Canvas::new();
    let overlay = unsafe { materialize_overlay(style) };
    leak_custom(Styled::new(widget, overlay))
}

/// `$Blinc$RichText$view(markup, style) -> WidgetHandle`.
pub(crate) extern "C" fn blinc_rich_text_view(markup_ptr: *const i32, style: i64) -> WidgetHandle {
    if markup_ptr.is_null() {
        tracing::warn!("$Blinc$RichText$view called with null markup pointer");
        return 0;
    }
    let markup = decode_string_arg(markup_ptr);
    let widget = blinc_layout::rich_text::RichText::new(markup);
    let overlay = unsafe { materialize_overlay(style) };
    leak_custom(Styled::new(widget, overlay))
}

/// `$Blinc$Motion$view(children, style) -> WidgetHandle`.
pub(crate) extern "C" fn blinc_motion_view(children: WidgetHandle, style: i64) -> WidgetHandle {
    let mut widget = blinc_layout::motion::motion();
    for child in materialize_children(children) {
        widget = widget.child_box(child);
    }
    let overlay = unsafe { materialize_overlay(style) };
    leak_custom(Styled::new(widget, overlay))
}

/// `$Blinc$Notch$view(children, style) -> WidgetHandle`.
pub(crate) extern "C" fn blinc_notch_view(children: WidgetHandle, style: i64) -> WidgetHandle {
    let mut widget = blinc_layout::notch::Notch::new();
    for child in materialize_children(children) {
        widget = widget.child_box(child);
    }
    let overlay = unsafe { materialize_overlay(style) };
    leak_custom(Styled::new(widget, overlay))
}

/// `__new_child_list__() -> i64` — mints a fresh `Vec<WidgetHandle>` for a container.
pub(crate) extern "C" fn blinc_new_child_list() -> i64 {
    Box::into_raw(Box::new(Vec::<WidgetHandle>::new())) as i64
}

/// `__push_child__(list, child)` — appends to a list minted by `__new_child_list__`.
///
/// # Safety
///
/// `list` must come from `__new_child_list__` and remain live (reclaimed by the container).
pub(crate) extern "C" fn blinc_push_child(list: i64, child: WidgetHandle) {
    if list == 0 {
        return;
    }
    // SAFETY: keep alloc live for the container extern to reclaim.
    let vec: &mut Vec<WidgetHandle> = unsafe { &mut *(list as *mut Vec<WidgetHandle>) };
    vec.push(child);
}

// Overlay-builder externs for inline visual props (bg, opacity, …). Consumed by
// the container/widget extern, which wraps the widget in `Styled<W>`.

pub(crate) extern "C" fn blinc_new_style_overlay() -> i64 {
    Box::into_raw(Box::new(RenderPropsOverlay::default())) as i64
}

pub(crate) extern "C" fn blinc_set_overlay_bg(ptr: i64, color: i64) {
    if ptr == 0 {
        return;
    }
    let overlay: &mut RenderPropsOverlay = unsafe { &mut *(ptr as *mut RenderPropsOverlay) };
    overlay.background = Some(blinc_core::layer::Brush::Solid(
        blinc_core::layer::Color::from_hex(color as u32),
    ));
}

pub(crate) extern "C" fn blinc_set_overlay_opacity(ptr: i64, val: f64) {
    if ptr == 0 {
        return;
    }
    let overlay: &mut RenderPropsOverlay = unsafe { &mut *(ptr as *mut RenderPropsOverlay) };
    overlay.opacity = Some(val as f32);
}

pub(crate) extern "C" fn blinc_set_overlay_corner_radius(ptr: i64, val: f64) {
    if ptr == 0 {
        return;
    }
    let overlay: &mut RenderPropsOverlay = unsafe { &mut *(ptr as *mut RenderPropsOverlay) };
    overlay.corner_radius = Some(val as f32);
}

pub(crate) extern "C" fn blinc_set_overlay_border_width(ptr: i64, val: f64) {
    if ptr == 0 {
        return;
    }
    let overlay: &mut RenderPropsOverlay = unsafe { &mut *(ptr as *mut RenderPropsOverlay) };
    overlay.border_width = Some(val as f32);
}

pub(crate) extern "C" fn blinc_set_overlay_border_color(ptr: i64, color: i64) {
    if ptr == 0 {
        return;
    }
    let overlay: &mut RenderPropsOverlay = unsafe { &mut *(ptr as *mut RenderPropsOverlay) };
    overlay.border_color = Some(blinc_core::layer::Color::from_hex(color as u32));
}

/// Reclaim a `Box<RenderPropsOverlay>` from `__new_style_overlay__`. Default for null.
///
/// # Safety
///
/// `ptr` must come from `__new_style_overlay__`.
pub unsafe fn materialize_overlay(ptr: i64) -> RenderPropsOverlay {
    if ptr == 0 {
        return RenderPropsOverlay::default();
    }
    *unsafe { Box::from_raw(ptr as *mut RenderPropsOverlay) }
}
