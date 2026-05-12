//! Blinc DSL core — Zyntax-embedded grammar, runtime engine, and host glue.
//!
//! Pipeline: source → `Grammar2::from_source` → `TypedProgram` →
//! `ZyntaxRuntime::compile_typed_program` → JIT call → scene buffer drained
//! via `take_scene_ops()` → `ElementBuilder` tree for the renderer.
//!
//! Builtins are registered statically — no `.zrtl` plugin discovery.

use std::path::Path;
use std::sync::{Arc, Mutex};

use thiserror::Error;
use zyntax_embed::{
    Grammar2, Grammar2Error, NativeSignature, NativeType, RuntimeError, TypeTag, ZrtlSigFlags,
    ZrtlSymbolSig, ZyntaxRuntime,
};

/// Mirror of `zyntax_compiler::zrtl::MAX_PARAMS` (16). Part of `ZrtlSymbolSig`'s wire ABI.
const ZRTL_MAX_PARAMS: usize = 16;
use zyntax_typed_ast::type_registry::{PrimitiveType, Type};
use zyntax_typed_ast::{typed_node, Span, TypedProgram, TypedStatement};

/// Embedded Blinc DSL grammar source.
pub const BLINC_GRAMMAR: &str = include_str!("../grammar/blinc.zyn");

// Transitional legacy op stream. `$Blinc$text` / `$Blinc$text_int` push to a
// per-thread scene buffer drained by `render_view` / `render_component`. Goes
// away once all primitives are value-returning widget constructors.

use std::cell::RefCell;

/// One declarative draw op emitted by the DSL during `render_view`. Legacy path.
#[derive(Debug, Clone, PartialEq)]
pub enum DslOp {
    Text(String),
    IntText(i32),
}

thread_local! {
    static SCENE_BUFFER: RefCell<Vec<DslOp>> = const { RefCell::new(Vec::new()) };
}

fn push_op(op: DslOp) {
    SCENE_BUFFER.with(|b| b.borrow_mut().push(op));
}

/// Drain and return everything pushed onto the scene buffer since the last call.
pub fn take_scene_ops() -> Vec<DslOp> {
    SCENE_BUFFER.with(|b| std::mem::take(&mut *b.borrow_mut()))
}

// =====================================================================
// Builtins
// =====================================================================

/// `$Blinc$text` — pushes a string literal onto the scene buffer.
///
/// # Safety
///
/// Called by Zyntax's JIT via [`ZyntaxRuntime::register_function`]; `s_ptr`
/// points at a `ZyntaxString` (`[i32 len][utf8 bytes…]`).
extern "C" fn blinc_text(s_ptr: *const i32) {
    if s_ptr.is_null() {
        tracing::warn!("$Blinc$text called with null pointer");
        return;
    }

    // SAFETY: runtime guarantees length-prefixed UTF-8 layout for `Ptr` string args.
    let raw = unsafe {
        let len = std::ptr::read_unaligned(s_ptr) as usize;
        let body = (s_ptr as *const u8).add(std::mem::size_of::<i32>());
        let bytes = std::slice::from_raw_parts(body, len);
        std::str::from_utf8(bytes).unwrap_or("<invalid utf-8>")
    };

    // Grammar's `string_literal` preserves surrounding quotes; strip them.
    let stripped = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw);

    push_op(DslOp::Text(stripped.to_string()));
}

/// `__signal_get_i32` — i32 signal accessor synthesised by `resolve_signal_calls`.
/// Returns `0` for unset signals.
///
/// # Safety
///
/// Same contract as [`blinc_text`]: `name_ptr` points at a length-prefixed UTF-8 buffer.
extern "C" fn blinc_signal_get_i32(name_ptr: *const i32) -> i32 {
    if name_ptr.is_null() {
        tracing::warn!("__signal_get_i32 called with null name pointer");
        return 0;
    }

    // SAFETY: length-prefixed string layout for String params.
    let name = unsafe {
        let len = std::ptr::read_unaligned(name_ptr) as usize;
        let body = (name_ptr as *const u8).add(std::mem::size_of::<i32>());
        let bytes = std::slice::from_raw_parts(body, len);
        std::str::from_utf8(bytes).unwrap_or("<invalid utf-8>")
    };

    // Defensive quote-strip — the rewrite normally hands us unquoted names.
    let stripped = name
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(name);

    blinc_runtime::signal::get_i32_or_default(stripped)
}

/// `__signal_get_f64` — f64 signal accessor. Returns `0.0` for unset signals.
///
/// # Safety
///
/// Same contract as [`blinc_signal_get_i32`].
extern "C" fn blinc_signal_get_f64(name_ptr: *const i32) -> f64 {
    if name_ptr.is_null() {
        tracing::warn!("__signal_get_f64 called with null name pointer");
        return 0.0;
    }

    let name = unsafe {
        let len = std::ptr::read_unaligned(name_ptr) as usize;
        let body = (name_ptr as *const u8).add(std::mem::size_of::<i32>());
        let bytes = std::slice::from_raw_parts(body, len);
        std::str::from_utf8(bytes).unwrap_or("<invalid utf-8>")
    };
    let stripped = name
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(name);

    blinc_runtime::signal::get_f64_or_default(stripped)
}

/// `__signal_get_string` — string signal accessor. Returns a Zyntax length-prefixed
/// pointer; the buffer leaks via `blinc_string_alloc`.
///
/// # Safety
///
/// Same contract as [`blinc_signal_get_i32`].
extern "C" fn blinc_signal_get_string(name_ptr: *const i32) -> *const i32 {
    if name_ptr.is_null() {
        tracing::warn!("__signal_get_string called with null name pointer");
        return blinc_string_alloc("");
    }

    // SAFETY: length-prefixed string layout for String params.
    let name = unsafe {
        let len = std::ptr::read_unaligned(name_ptr) as usize;
        let body = (name_ptr as *const u8).add(std::mem::size_of::<i32>());
        let bytes = std::slice::from_raw_parts(body, len);
        std::str::from_utf8(bytes).unwrap_or("<invalid utf-8>")
    };
    let stripped = name
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(name);

    let value = blinc_runtime::signal::get_str_or_default(stripped);
    blinc_string_alloc(&value)
}

/// Decode a length-prefixed Zyntax string pointer to a `&str`.
fn decode_signal_name<'a>(name_ptr: *const i32) -> Option<&'a str> {
    if name_ptr.is_null() {
        return None;
    }
    // SAFETY: length-prefixed UTF-8 layout per Zyntax String param ABI.
    let raw = unsafe {
        let len = std::ptr::read_unaligned(name_ptr) as usize;
        let body = (name_ptr as *const u8).add(std::mem::size_of::<i32>());
        let bytes = std::slice::from_raw_parts(body, len);
        std::str::from_utf8(bytes).ok()?
    };
    Some(
        raw.strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(raw),
    )
}

/// `__signal_set_i32("<name>", value)` — i32 signal write side.
extern "C" fn blinc_signal_set_i32(name_ptr: *const i32, value: i32) {
    let Some(name) = decode_signal_name(name_ptr) else {
        tracing::warn!("__signal_set_i32 called with null name pointer");
        return;
    };
    blinc_runtime::signal::set_i32(name, value);
}

/// `__signal_set_f64("<name>", value)` — f64 signal write side.
extern "C" fn blinc_signal_set_f64(name_ptr: *const i32, value: f64) {
    let Some(name) = decode_signal_name(name_ptr) else {
        tracing::warn!("__signal_set_f64 called with null name pointer");
        return;
    };
    blinc_runtime::signal::set_f64(name, value);
}

/// `__signal_set_string("<name>", value)` — string signal write side.
extern "C" fn blinc_signal_set_string(name_ptr: *const i32, value_ptr: *const i32) {
    let Some(name) = decode_signal_name(name_ptr) else {
        tracing::warn!("__signal_set_string called with null name pointer");
        return;
    };
    let value = decode_signal_name(value_ptr).unwrap_or("");
    blinc_runtime::signal::set_str(name, value);
}

/// `__fsm_runtime_trigger__("<FsmName>", "<state.event>")` — dispatches `event`
/// on the default instance iff its current state matches `state`.
extern "C" fn blinc_fsm_runtime_trigger(fsm_ptr: *const i32, path_ptr: *const i32) {
    let Some(fsm) = decode_signal_name(fsm_ptr) else {
        tracing::warn!("__fsm_runtime_trigger__ called with null fsm pointer");
        return;
    };
    let Some(path) = decode_signal_name(path_ptr) else {
        tracing::warn!("__fsm_runtime_trigger__ called with null path pointer");
        return;
    };
    let Some((state, event)) = path.split_once('.') else {
        tracing::warn!(
            fsm = fsm,
            path = path,
            "trigger path must be '<State>.<Event>' — leaving fsm untouched"
        );
        return;
    };
    let state = state.trim();
    let event = event.trim();

    let current = blinc_runtime::fsm::current_state_name(fsm);
    let matches_precondition = current.as_deref().map(|c| c == state).unwrap_or(false);
    if !matches_precondition {
        return;
    }
    blinc_runtime::fsm::dispatch_default(fsm, event);
}

/// `__fsm_subscribe__("<FsmName>", "<From.Event>", closure_ptr)` — registers a
/// path-filtered subscriber closure for the FSM's default-instance transitions.
///
/// # Safety
///
/// `closure_ptr` must remain valid for the lifetime of the `ZyntaxRuntime`.
extern "C" fn blinc_fsm_subscribe(fsm_ptr: *const i32, path_ptr: *const i32, closure_ptr: i64) {
    let Some(fsm) = decode_signal_name(fsm_ptr) else {
        tracing::warn!("__fsm_subscribe__ called with null fsm pointer");
        return;
    };
    let Some(path) = decode_signal_name(path_ptr) else {
        tracing::warn!("__fsm_subscribe__ called with null path pointer");
        return;
    };
    if closure_ptr == 0 {
        tracing::warn!("__fsm_subscribe__ called with null closure pointer");
        return;
    }
    blinc_runtime::fsm::register_subscriber(fsm, path, move || {
        // SAFETY: SSA lowering produces an `extern "C" fn()` lambda body.
        type SubscriberFn = extern "C" fn();
        let func: SubscriberFn = unsafe { std::mem::transmute(closure_ptr) };
        func();
    });
}

/// `$Blinc$text_int` — integer arm of `text(...)`. Pushes an int onto the scene buffer.
extern "C" fn blinc_text_int(n: i32) {
    push_op(DslOp::IntText(n));
}

// =====================================================================
// F-string desugaring builtins
// =====================================================================
//
// `f"hi {n}"` lowers to `string_concat("hi ", __fstring_format__(n))` via the
// normalization pass. Both names must resolve to host externs at JIT time.
// Strings produced here LEAK — acceptable for the prototype; fix path is a
// per-render arena bump allocator.

/// Encode a Rust `&str` as a Zyntax length-prefixed string (leaked).
fn blinc_string_alloc(s: &str) -> *const i32 {
    let len = s.len() as u32;
    let total = 4 + s.len();
    let mut buf: Vec<u8> = Vec::with_capacity(total);
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
    let ptr = buf.as_ptr() as *const i32;
    // Leak — see module comment above.
    std::mem::forget(buf);
    ptr
}

/// Decode a Zyntax length-prefixed string back to a `&str`.
///
/// # Safety
///
/// `ptr` must come from `blinc_string_alloc` (or any producer of the same layout).
unsafe fn blinc_string_decode<'a>(ptr: *const i32) -> &'a str {
    if ptr.is_null() {
        return "";
    }
    let len = std::ptr::read_unaligned(ptr) as usize;
    let body = (ptr as *const u8).add(4);
    let bytes = std::slice::from_raw_parts(body, len);
    std::str::from_utf8(bytes).unwrap_or("<invalid utf-8>")
}

/// `__fstring_format__` for i32 — decimal string of an integer.
extern "C" fn blinc_format_int(n: i32) -> *const i32 {
    let s = n.to_string();
    blinc_string_alloc(&s)
}

/// `string_concat` — joins two Zyntax-formatted strings into a fresh leaked one.
extern "C" fn blinc_string_concat(a: *const i32, b: *const i32) -> *const i32 {
    // SAFETY: length-prefixed string layout for String params.
    let a_str = unsafe { blinc_string_decode(a) };
    let b_str = unsafe { blinc_string_decode(b) };
    let mut out = String::with_capacity(a_str.len() + b_str.len());
    out.push_str(a_str);
    out.push_str(b_str);
    blinc_string_alloc(&out)
}

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

/// `#[extern_widget]` proc-macro for declaring DSL-exposed Rust widgets.
pub use blinc_dsl_macro::extern_widget;

/// Canonical Zyntax runtime value enum, re-exported for `query` / `ViewRenderer` consumers.
pub use zyntax_embed::ZyntaxValue;

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
        let s = unsafe { super::blinc_string_decode(ptr) };
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

/// `$Blinc$Text$view(content: string) -> WidgetHandle`
///
/// # Safety
///
/// `content_ptr` must point at a Zyntax length-prefixed UTF-8 buffer.
extern "C" fn blinc_text_view(content_ptr: *const i32) -> WidgetHandle {
    if content_ptr.is_null() {
        tracing::warn!("$Blinc$Text$view called with null content pointer");
        return 0;
    }
    // SAFETY: see fn-level doc.
    let content = unsafe { blinc_string_decode(content_ptr) };
    let widget = blinc_layout::text::Text::new(content);
    Box::into_raw(Box::new(WidgetBox::Text(Box::new(widget)))) as WidgetHandle
}

/// `$Blinc$Div$view(children, style, class_str, on_click) -> WidgetHandle`.
/// Consumes the child-list and each child handle exactly once.
extern "C" fn blinc_div_view(
    children: WidgetHandle,
    style: i64,
    class_str: *const i32,
    on_click_closure: i64,
) -> WidgetHandle {
    let mut widget = blinc_layout::div::Div::new();
    if children != 0 {
        let list: Box<Vec<WidgetHandle>> =
            unsafe { Box::from_raw(children as *mut Vec<WidgetHandle>) };
        for handle in *list {
            if let Some(child_box) = unsafe { materialize_widget(handle) } {
                widget = widget.child_box(child_box.into_element_builder());
            }
        }
    }
    // SAFETY: `class_str` is `*const i32` per registered sig.
    if !class_str.is_null() {
        let raw = unsafe { blinc_string_decode(class_str) };
        for name in raw.split_whitespace() {
            widget = widget.class(name);
        }
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
    Box::into_raw(Box::new(WidgetBox::Custom(Box::new(Styled::new(
        widget, overlay,
    ))))) as WidgetHandle
}

/// `__new_child_list__() -> i64` — mints a fresh `Vec<WidgetHandle>` for a container.
extern "C" fn blinc_new_child_list() -> i64 {
    Box::into_raw(Box::new(Vec::<WidgetHandle>::new())) as i64
}

/// `__push_child__(list, child)` — appends to a list minted by `__new_child_list__`.
///
/// # Safety
///
/// `list` must come from `__new_child_list__` and remain live (reclaimed by the container).
extern "C" fn blinc_push_child(list: i64, child: WidgetHandle) {
    if list == 0 {
        return;
    }
    // SAFETY: keep alloc live for the container extern to reclaim.
    let vec: &mut Vec<WidgetHandle> = unsafe { &mut *(list as *mut Vec<WidgetHandle>) };
    vec.push(child);
}

// Overlay-builder externs for inline visual props (bg, opacity, …). Consumed by
// the container/widget extern, which wraps the widget in `Styled<W>`.

extern "C" fn blinc_new_style_overlay() -> i64 {
    Box::into_raw(Box::new(RenderPropsOverlay::default())) as i64
}

extern "C" fn blinc_set_overlay_bg(ptr: i64, color: i64) {
    if ptr == 0 {
        return;
    }
    let overlay: &mut RenderPropsOverlay = unsafe { &mut *(ptr as *mut RenderPropsOverlay) };
    overlay.background = Some(blinc_core::layer::Brush::Solid(
        blinc_core::layer::Color::from_hex(color as u32),
    ));
}

extern "C" fn blinc_set_overlay_opacity(ptr: i64, val: f64) {
    if ptr == 0 {
        return;
    }
    let overlay: &mut RenderPropsOverlay = unsafe { &mut *(ptr as *mut RenderPropsOverlay) };
    overlay.opacity = Some(val as f32);
}

extern "C" fn blinc_set_overlay_corner_radius(ptr: i64, val: f64) {
    if ptr == 0 {
        return;
    }
    let overlay: &mut RenderPropsOverlay = unsafe { &mut *(ptr as *mut RenderPropsOverlay) };
    overlay.corner_radius = Some(val as f32);
}

extern "C" fn blinc_set_overlay_border_width(ptr: i64, val: f64) {
    if ptr == 0 {
        return;
    }
    let overlay: &mut RenderPropsOverlay = unsafe { &mut *(ptr as *mut RenderPropsOverlay) };
    overlay.border_width = Some(val as f32);
}

extern "C" fn blinc_set_overlay_border_color(ptr: i64, color: i64) {
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

// =====================================================================
// Errors
// =====================================================================

/// Top-level error type for the embed API.
#[derive(Debug, Error)]
pub enum BlincDslError {
    /// `Grammar2::from_source(BLINC_GRAMMAR)` failed (Blinc-internal bug).
    #[error("blinc grammar compile failed: {0}")]
    Grammar(#[from] Grammar2Error),

    /// User's `.blinc` source has a parse / type / lowering error.
    #[error("blinc compile error: {0}")]
    Compile(String),

    /// `runtime.call::<T>(...)` failed at execution time.
    #[error("blinc runtime error: {0}")]
    Runtime(#[from] RuntimeError),

    /// Reading the source file off disk failed.
    #[error("blinc source io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type BlincDslResult<T> = std::result::Result<T, BlincDslError>;

// =====================================================================
// FSM registry
// =====================================================================
//
// `(module, TypeId)` keys so same-named fsms in different modules don't collide.

/// Identity of an fsm in the global registry: Zyntax module + type-registry `TypeId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FsmId {
    /// Zyntax module name. Currently always `"main"`.
    pub module: zyntax_typed_ast::InternedString,
    /// The fsm's enum `TypeId` from the program's `type_registry`.
    pub type_id: zyntax_typed_ast::type_registry::TypeId,
}

/// Tick-driven guard. The guard expression is lifted into a top-level function
/// `__fsm_tick_guard_<FsmName>_<idx>__` so dispatch can call it as a normal symbol.
#[derive(Debug, Clone)]
pub struct TickGuard {
    pub from: zyntax_typed_ast::InternedString,
    pub to: zyntax_typed_ast::InternedString,
    /// Synthesised guard-function symbol name.
    pub guard_fn: Option<zyntax_typed_ast::InternedString>,
}

/// One event-driven transition: `on <from>.<event> -> <to> { <action>... }`.
#[derive(Debug, Clone)]
pub struct EventTransition {
    pub from: zyntax_typed_ast::InternedString,
    pub event: zyntax_typed_ast::InternedString,
    pub to: zyntax_typed_ast::InternedString,
    /// Actions in source order.
    pub actions: Vec<blinc_runtime::fsm::TransitionAction>,
}

/// Runtime definition of an fsm — populated by the `__fsm_meta__` body.
#[derive(Debug, Clone, Default)]
pub struct FsmDefinition {
    /// Initial state name.
    pub initial: Option<zyntax_typed_ast::InternedString>,
    /// Event-driven transitions in declaration order.
    pub transitions: Vec<EventTransition>,
    /// Tick-driven guards in declaration order.
    pub tick_guards: Vec<TickGuard>,
    /// Bare fsm name (for diagnostics; authoritative identity is `FsmId`).
    pub name: Option<zyntax_typed_ast::InternedString>,
}

impl FsmDefinition {
    /// Resolve an event-driven transition. First matching rule wins (declaration order).
    pub fn step_event(
        &self,
        current: &str,
        event: &str,
    ) -> Option<zyntax_typed_ast::InternedString> {
        let current_i = zyntax_typed_ast::InternedString::new_global(current);
        let event_i = zyntax_typed_ast::InternedString::new_global(event);
        self.transitions
            .iter()
            .find(|t| t.from == current_i && t.event == event_i)
            .map(|t| t.to)
    }
}

/// Process-wide registry of fsm definitions keyed by `FsmId`.
#[derive(Debug, Default)]
pub struct FsmRegistry {
    fsms: std::collections::HashMap<FsmId, FsmDefinition>,
}

impl FsmRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert/update an fsm definition.
    pub fn upsert(&mut self, id: FsmId, def: FsmDefinition) {
        self.fsms.insert(id, def);
    }

    pub fn get(&self, id: &FsmId) -> Option<&FsmDefinition> {
        self.fsms.get(id)
    }

    pub fn get_mut(&mut self, id: &FsmId) -> Option<&mut FsmDefinition> {
        self.fsms.get_mut(id)
    }

    pub fn len(&self) -> usize {
        self.fsms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fsms.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&FsmId, &FsmDefinition)> {
        self.fsms.iter()
    }

    /// Remove an fsm — used during hot-reload to drop stale entries.
    pub fn remove(&mut self, id: &FsmId) -> Option<FsmDefinition> {
        self.fsms.remove(id)
    }

    /// Find an fsm by source-level name within a module (linear scan).
    pub fn find_by_name(
        &self,
        module: zyntax_typed_ast::InternedString,
        name: &str,
    ) -> Option<FsmId> {
        let needle = zyntax_typed_ast::InternedString::new_global(name);
        self.fsms
            .iter()
            .find(|(id, def)| id.module == module && def.name == Some(needle))
            .map(|(id, _)| *id)
    }

    /// Lookup + transition in one call. `None` if no fsm registered or no rule matches.
    pub fn step_event(
        &self,
        id: &FsmId,
        current: &str,
        event: &str,
    ) -> Option<zyntax_typed_ast::InternedString> {
        self.get(id).and_then(|d| d.step_event(current, event))
    }
}

/// Live instance of a DSL-defined fsm — pairs an `FsmId` with current state name.
/// State is `InternedString` of the variant (dynamic, no compile-time enum mapping).
///
/// # Example
///
/// ```ignore
/// let mut loader = FsmInstance::new(&dsl, "main", "Loader")?;
/// loader.dispatch_event(&dsl, "Start");
/// ```
#[derive(Debug, Clone)]
pub struct FsmInstance {
    /// Identity of the fsm definition this instance follows.
    pub id: FsmId,
    /// Current state name (mutated by `dispatch_event` / `tick`).
    pub current: zyntax_typed_ast::InternedString,
}

impl FsmInstance {
    /// Create an instance starting in the fsm's declared initial state. `None` if
    /// the fsm isn't registered or has no initial state.
    pub fn new(_dsl: &BlincDsl, module: &str, fsm_name: &str) -> Option<Self> {
        let module_i = zyntax_typed_ast::InternedString::new_global(module);
        let id = with_fsm_registry(|r| r.find_by_name(module_i, fsm_name))?;
        let initial = with_fsm_registry(|r| r.get(&id).and_then(|d| d.initial))?;
        Some(Self {
            id,
            current: initial,
        })
    }

    /// Current state name as `String`.
    pub fn current(&self) -> String {
        self.current.resolve_global().unwrap_or_default()
    }

    /// Dispatch an event by name. Returns `true` if a transition fired.
    pub fn dispatch_event(&mut self, _dsl: &BlincDsl, event: &str) -> bool {
        let current_str = self.current();
        let next = with_fsm_registry(|r| r.step_event(&self.id, &current_str, event));
        if let Some(to) = next {
            self.current = to;
            true
        } else {
            false
        }
    }

    /// Tick. JIT-evaluates registered tick-guards; returns `true` if a transition fired.
    pub fn tick(&mut self, dsl: &BlincDsl) -> BlincDslResult<bool> {
        let current_str = self.current();
        let next = dsl.step_tick(&self.id, &current_str)?;
        if let Some(to) = next {
            self.current = to;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Reset to the fsm's initial state.
    pub fn reset(&mut self) {
        if let Some(initial) = with_fsm_registry(|r| r.get(&self.id).and_then(|d| d.initial)) {
            self.current = initial;
        }
    }
}

/// Process-wide fsm registry. Multiple `BlincDsl` instances share one view.
static GLOBAL_FSM_REGISTRY: std::sync::OnceLock<std::sync::Mutex<FsmRegistry>> =
    std::sync::OnceLock::new();

fn fsm_registry_lock() -> std::sync::MutexGuard<'static, FsmRegistry> {
    GLOBAL_FSM_REGISTRY
        .get_or_init(|| std::sync::Mutex::new(FsmRegistry::new()))
        .lock()
        .expect("BlincDsl global FsmRegistry mutex poisoned")
}

/// Run a closure with shared access to the global fsm registry.
pub fn with_fsm_registry<R>(f: impl FnOnce(&FsmRegistry) -> R) -> R {
    let guard = fsm_registry_lock();
    f(&guard)
}

/// Run a closure with mutable registry access. Used internally by marker builtins.
pub fn with_fsm_registry_mut<R>(f: impl FnOnce(&mut FsmRegistry) -> R) -> R {
    let mut guard = fsm_registry_lock();
    f(&mut guard)
}

// =====================================================================
// FSM dispatch synthesis (post-parse)
// =====================================================================

/// Shape-based recognition for `signal <name>: <T>` decls: extern fn, no body,
/// no params, primitive return, `link_name: None` (so we don't catch host builtins).
fn is_signal_decl(func: &zyntax_typed_ast::typed_ast::TypedFunction) -> bool {
    func.is_external
        && func.body.is_none()
        && func.params.is_empty()
        && func.link_name.is_none()
        && matches!(func.return_type, Type::Primitive(_))
}

/// Run the JIT'd view once and box the resulting widget builder (no reactive wrapper).
fn materialize_view(
    renderer: &std::sync::Arc<dyn blinc_runtime::view::ViewRenderer>,
) -> Box<dyn blinc_layout::div::ElementBuilder> {
    use zyntax_embed::ZyntaxValue;
    let value = blinc_runtime::view::render_main(renderer).expect("render_main");
    let ZyntaxValue::Int(handle) = value else {
        return Box::new(blinc_layout::div::Div::new());
    };
    unsafe { materialize_widget(handle) }
        .map(|w| w.into_element_builder())
        .unwrap_or_else(|| Box::new(blinc_layout::div::Div::new()))
}

/// Detect view decorators and strip the synthetic marker calls.
/// Returns `(saw_stateful, explicit_signal_deps, explicit_fsms)`.
/// Empty `signal_deps` with `saw_stateful=true` means subscribe to all declared signals.
fn detect_and_strip_stateful_views(program: &mut TypedProgram) -> (bool, Vec<String>, Vec<String>) {
    use zyntax_typed_ast::typed_ast::{TypedDeclaration, TypedExpression, TypedLiteral};

    // Strip a leading marker call matching `expected_callee` and return its string args.
    fn strip_leading_marker(
        body: &mut zyntax_typed_ast::typed_ast::TypedBlock,
        expected_callee: &str,
    ) -> Option<Vec<String>> {
        let matches = body.statements.first().and_then(|s| match &s.node {
            TypedStatement::Expression(e) => match &e.node {
                TypedExpression::Call(c) => match &c.callee.node {
                    TypedExpression::Variable(name)
                        if name.resolve_global().as_deref() == Some(expected_callee) =>
                    {
                        Some(c.positional_args.clone())
                    }
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        });
        let args = matches?;
        let names: Vec<String> = args
            .into_iter()
            .filter_map(|arg| match &arg.node {
                TypedExpression::Literal(TypedLiteral::String(s)) => {
                    s.resolve_global().map(|n| n.to_string())
                }
                _ => None,
            })
            .collect();
        body.statements.remove(0);
        Some(names)
    }

    let mut saw_stateful = false;
    let mut signal_deps: Vec<String> = Vec::new();
    let mut fsms: Vec<String> = Vec::new();

    let mut process = |body: &mut Option<zyntax_typed_ast::typed_ast::TypedBlock>| {
        let Some(body) = body else {
            return;
        };
        // Decorators can stack either way; strip until no more match.
        loop {
            if let Some(names) = strip_leading_marker(body, "__stateful_view__") {
                saw_stateful = true;
                for n in names {
                    if !signal_deps.contains(&n) {
                        signal_deps.push(n);
                    }
                }
                continue;
            }
            if let Some(names) = strip_leading_marker(body, "__fsm_view__") {
                saw_stateful = true;
                for n in names {
                    if !fsms.contains(&n) {
                        fsms.push(n);
                    }
                }
                continue;
            }
            break;
        }
    };

    for decl in program.declarations.iter_mut() {
        match &mut decl.node {
            TypedDeclaration::Function(func) => process(&mut func.body),
            TypedDeclaration::Impl(imp) => {
                for method in imp.methods.iter_mut() {
                    process(&mut method.body);
                }
            }
            _ => {}
        }
    }

    (saw_stateful, signal_deps, fsms)
}

/// Snapshot `signal <name>: <T>` and `fsm <Name> { … }` decls. MUST run BEFORE
/// the signal-rewrite / fsm-meta-strip passes — they erase the originating decls.
fn collect_declared(program: &TypedProgram) -> (Vec<(String, Type)>, Vec<String>) {
    use zyntax_typed_ast::typed_ast::TypedDeclaration;
    let mut signals = Vec::new();
    let mut fsms = Vec::new();
    for decl in &program.declarations {
        match &decl.node {
            TypedDeclaration::Function(func) if is_signal_decl(func) => {
                if let Some(name) = func.name.resolve_global() {
                    signals.push((name.to_string(), func.return_type.clone()));
                }
            }
            TypedDeclaration::Impl(imp) => {
                let is_fsm = imp
                    .methods
                    .iter()
                    .any(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"));
                if is_fsm {
                    if let Some(name) = imp.trait_name.resolve_global() {
                        fsms.push(name.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    (signals, fsms)
}

/// Extract CSS from `__blinc_stylesheet__` marker fns and remove them from the
/// program. The CSS text is run through [`auto_inject_semicolons`] so `;`-free
/// declarations work.
fn extract_and_strip_stylesheets(program: &mut TypedProgram, out: &mut Vec<String>) {
    use zyntax_typed_ast::typed_ast::{
        TypedDeclaration, TypedExpression, TypedLiteral, TypedStatement,
    };
    program.declarations.retain(|decl| {
        let TypedDeclaration::Function(func) = &decl.node else {
            return true;
        };
        if func.name.resolve_global().as_deref() != Some("__blinc_stylesheet__") {
            return true;
        }
        let Some(body) = &func.body else {
            return false;
        };
        for stmt in &body.statements {
            let TypedStatement::Expression(expr) = &stmt.node else {
                continue;
            };
            if let TypedExpression::Literal(TypedLiteral::String(s)) = &expr.node {
                if let Some(text) = s.resolve_global() {
                    out.push(auto_inject_semicolons(&text));
                }
            }
        }
        false
    });
}

/// Append `;` inside `{ ... }` blocks where the line's last char doesn't already
/// terminate. Brace depth tracking is naïve — string/comment braces will skew it.
fn auto_inject_semicolons(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + raw.len() / 8);
    let mut depth: i32 = 0;
    for line in raw.split_inclusive('\n') {
        // Separate body from trailing whitespace so we inject `;` before the newline.
        let line_end_idx = line
            .rfind(|c: char| !c.is_whitespace())
            .map(|i| i + line[i..].chars().next().map(char::len_utf8).unwrap_or(0))
            .unwrap_or(line.len());
        let body = &line[..line_end_idx];
        let tail = &line[line_end_idx..];

        let depth_before = depth;
        depth += body.matches('{').count() as i32;
        depth -= body.matches('}').count() as i32;

        out.push_str(body);

        let trimmed = body.trim_start();
        let last_char = body.chars().rev().find(|c| !c.is_whitespace());
        let is_comment_line =
            trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with("*");
        let inside_block = depth_before > 0;
        let needs_semi = inside_block
            && !is_comment_line
            && match last_char {
                None => false,
                Some(c) => !matches!(c, ';' | '{' | '}' | ',' | '/' | '*'),
            };
        if needs_semi {
            out.push(';');
        }
        out.push_str(tail);
    }
    out
}

/// Rewrite `<sig>.get()` / `<sig>.set(v)` / `<sig> = v` into `__signal_<get|set>_<T>` calls.
fn resolve_signal_calls(program: &mut TypedProgram) {
    use std::collections::HashMap;
    use zyntax_typed_ast::typed_ast::{TypedCall, TypedDeclaration, TypedExpression, TypedLiteral};
    use zyntax_typed_ast::InternedString;

    // Phase 1: collect signal name → return type.
    let mut signals: HashMap<InternedString, Type> = HashMap::new();
    for decl in &program.declarations {
        let TypedDeclaration::Function(func) = &decl.node else {
            continue;
        };
        if !is_signal_decl(func) {
            continue;
        }
        signals.insert(func.name, func.return_type.clone());
    }

    if signals.is_empty() {
        return;
    }

    // Phase 2: rewrite `<sig>.get()` → `__signal_get_<T>("<name>")`.
    fn typed_signal_extern_name(ty: &Type) -> Option<&'static str> {
        match ty {
            Type::Primitive(PrimitiveType::I32) => Some("__signal_get_i32"),
            Type::Primitive(PrimitiveType::F64) => Some("__signal_get_f64"),
            Type::Primitive(PrimitiveType::String) => Some("__signal_get_string"),
            _ => None,
        }
    }

    fn typed_signal_setter_extern_name(ty: &Type) -> Option<&'static str> {
        match ty {
            Type::Primitive(PrimitiveType::I32) => Some("__signal_set_i32"),
            Type::Primitive(PrimitiveType::F64) => Some("__signal_set_f64"),
            Type::Primitive(PrimitiveType::String) => Some("__signal_set_string"),
            _ => None,
        }
    }

    fn rewrite_expr(
        expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>,
        signals: &HashMap<InternedString, Type>,
    ) {
        // MUST intercept `<signal> = <expr>` BEFORE the recursive walk — the
        // LHS `Variable` doesn't otherwise trigger a rewrite.
        if let TypedExpression::Binary(b) = &expr.node {
            if b.op == zyntax_typed_ast::typed_ast::BinaryOp::Assign {
                if let TypedExpression::Variable(name) = &b.left.node {
                    if let Some(sig_ty) = signals.get(name).cloned() {
                        if let Some(setter) = typed_signal_setter_extern_name(&sig_ty) {
                            // Rewrite RHS first so nested signal reads route through getters.
                            let mut rhs = (*b.right).clone();
                            rewrite_expr(&mut rhs, signals);

                            let name_arg = zyntax_typed_ast::TypedNode::new(
                                TypedExpression::Literal(TypedLiteral::String(*name)),
                                Type::Primitive(PrimitiveType::String),
                                expr.span,
                            );
                            let callee = zyntax_typed_ast::TypedNode::new(
                                TypedExpression::Variable(InternedString::new_global(setter)),
                                Type::Unknown,
                                expr.span,
                            );
                            expr.node = TypedExpression::Call(TypedCall {
                                callee: Box::new(callee),
                                positional_args: vec![name_arg, rhs],
                                named_args: vec![],
                                type_args: vec![],
                            });
                            expr.ty = Type::Primitive(PrimitiveType::Unit);
                            return;
                        }
                    }
                }
            }
        }

        // Children first so nested signal calls (e.g. `text(count.get())`) are rewritten.
        match &mut expr.node {
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, signals);
                rewrite_expr(&mut b.right, signals);
            }
            TypedExpression::Unary(u) => {
                rewrite_expr(&mut u.operand, signals);
            }
            TypedExpression::Call(c) => {
                rewrite_expr(&mut c.callee, signals);
                for a in &mut c.positional_args {
                    rewrite_expr(a, signals);
                }
            }
            TypedExpression::Field(f) => {
                rewrite_expr(&mut f.object, signals);
            }
            TypedExpression::Index(idx) => {
                rewrite_expr(&mut idx.object, signals);
                rewrite_expr(&mut idx.index, signals);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for item in items {
                    rewrite_expr(item, signals);
                }
            }
            TypedExpression::MethodCall(mc) => {
                rewrite_expr(&mut mc.receiver, signals);
                for a in &mut mc.positional_args {
                    rewrite_expr(a, signals);
                }
            }
            TypedExpression::Block(block) => {
                rewrite_block(block, signals);
            }
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition, signals);
                rewrite_expr(&mut if_expr.then_branch, signals);
                rewrite_expr(&mut if_expr.else_branch, signals);
            }
            TypedExpression::Lambda(lam) => match &mut lam.body {
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Expression(e) => {
                    rewrite_expr(e, signals);
                }
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Block(block) => {
                    rewrite_block(block, signals);
                }
            },
            _ => {}
        }

        // `.get()` / `.set(x)` lands in two AST shapes:
        //   1. `MethodCall` — expression position (postfix-expr).
        //   2. `Call { callee: Field { ... }, ... }` — statement position.
        // Recognise both.
        let method_call = match &expr.node {
            TypedExpression::MethodCall(mc) => {
                if let TypedExpression::Variable(receiver_name) = &mc.receiver.node {
                    Some((
                        *receiver_name,
                        mc.method,
                        mc.positional_args.clone(),
                        expr.span,
                    ))
                } else {
                    None
                }
            }
            TypedExpression::Call(c) => {
                if let TypedExpression::Field(f) = &c.callee.node {
                    if let TypedExpression::Variable(receiver_name) = &f.object.node {
                        Some((
                            *receiver_name,
                            f.field,
                            c.positional_args.clone(),
                            expr.span,
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        let Some((receiver_name, method, args, span)) = method_call else {
            return;
        };
        let Some(sig_ty) = signals.get(&receiver_name).cloned() else {
            return;
        };
        let method_name = method.resolve_global().map(|s| s.to_string());
        match method_name.as_deref() {
            // `count.get()` — read. Zero args, returns the
            // signal's value type.
            Some("get") if args.is_empty() => {
                let Some(extern_name) = typed_signal_extern_name(&sig_ty) else {
                    return;
                };
                expr.node = TypedExpression::Call(TypedCall {
                    callee: Box::new(zyntax_typed_ast::TypedNode::new(
                        TypedExpression::Variable(InternedString::new_global(extern_name)),
                        Type::Unknown,
                        span,
                    )),
                    positional_args: vec![zyntax_typed_ast::TypedNode::new(
                        TypedExpression::Literal(TypedLiteral::String(receiver_name)),
                        Type::Primitive(PrimitiveType::String),
                        span,
                    )],
                    named_args: vec![],
                    type_args: vec![],
                });
                expr.ty = sig_ty;
            }
            // `count.set(value)` — write. Arg already child-rewritten.
            Some("set") if args.len() == 1 => {
                let Some(setter) = typed_signal_setter_extern_name(&sig_ty) else {
                    return;
                };
                let value = args.into_iter().next().expect("len == 1 just checked");
                expr.node = TypedExpression::Call(TypedCall {
                    callee: Box::new(zyntax_typed_ast::TypedNode::new(
                        TypedExpression::Variable(InternedString::new_global(setter)),
                        Type::Unknown,
                        span,
                    )),
                    positional_args: vec![
                        zyntax_typed_ast::TypedNode::new(
                            TypedExpression::Literal(TypedLiteral::String(receiver_name)),
                            Type::Primitive(PrimitiveType::String),
                            span,
                        ),
                        value,
                    ],
                    named_args: vec![],
                    type_args: vec![],
                });
                expr.ty = Type::Primitive(PrimitiveType::Unit);
            }
            _ => {}
        }
    }

    fn rewrite_block(
        block: &mut zyntax_typed_ast::typed_ast::TypedBlock,
        signals: &HashMap<InternedString, Type>,
    ) {
        for stmt in &mut block.statements {
            rewrite_stmt(stmt, signals);
        }
    }

    fn rewrite_stmt(
        stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>,
        signals: &HashMap<InternedString, Type>,
    ) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, signals),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, signals);
                }
            }
            TypedStatement::Return(Some(e)) => rewrite_expr(e, signals),
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, signals);
                rewrite_block(&mut if_stmt.then_block, signals);
                if let Some(else_block) = &mut if_stmt.else_block {
                    rewrite_block(else_block, signals);
                }
            }
            TypedStatement::While(w) => {
                rewrite_expr(&mut w.condition, signals);
                rewrite_block(&mut w.body, signals);
            }
            TypedStatement::Block(b) => rewrite_block(b, signals),
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        let TypedDeclaration::Function(func) = &mut decl.node else {
            continue;
        };
        if let Some(body) = &mut func.body {
            rewrite_block(body, &signals);
        }
    }
    for decl in &mut program.declarations {
        let TypedDeclaration::Impl(imp) = &mut decl.node else {
            continue;
        };
        for method in &mut imp.methods {
            if let Some(body) = &mut method.body {
                rewrite_block(body, &signals);
            }
        }
    }

    // Phase 3: strip signal-marker decls (metadata only; usage was rewritten above).
    program.declarations.retain(|decl| {
        let TypedDeclaration::Function(func) = &decl.node else {
            return true;
        };
        !is_signal_decl(func)
    });
}

/// Rewrite `<FsmName>.trigger(<path>)` → `__fsm_runtime_trigger__("<FsmName>", <path>)`.
fn resolve_fsm_trigger_calls(program: &mut TypedProgram) {
    use std::collections::HashSet;
    use zyntax_typed_ast::typed_ast::{TypedCall, TypedDeclaration, TypedExpression, TypedLiteral};
    use zyntax_typed_ast::InternedString;

    // Phase 1: collect declared FSM names from `__fsm_meta__`-bearing impls.
    let mut fsm_names: HashSet<InternedString> = HashSet::new();
    for decl in &program.declarations {
        if let TypedDeclaration::Impl(imp) = &decl.node {
            if imp.trait_name.resolve_global().is_some()
                && imp
                    .methods
                    .iter()
                    .any(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
            {
                fsm_names.insert(imp.trait_name);
            }
        }
    }
    if fsm_names.is_empty() {
        return;
    }

    fn rewrite_expr(
        expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>,
        fsm_names: &HashSet<InternedString>,
    ) {
        // Recurse children first.
        match &mut expr.node {
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, fsm_names);
                rewrite_expr(&mut b.right, fsm_names);
            }
            TypedExpression::Unary(u) => rewrite_expr(&mut u.operand, fsm_names),
            TypedExpression::Call(c) => {
                rewrite_expr(&mut c.callee, fsm_names);
                for a in &mut c.positional_args {
                    rewrite_expr(a, fsm_names);
                }
            }
            TypedExpression::Field(f) => rewrite_expr(&mut f.object, fsm_names),
            TypedExpression::Index(idx) => {
                rewrite_expr(&mut idx.object, fsm_names);
                rewrite_expr(&mut idx.index, fsm_names);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for it in items {
                    rewrite_expr(it, fsm_names);
                }
            }
            TypedExpression::MethodCall(mc) => {
                rewrite_expr(&mut mc.receiver, fsm_names);
                for a in &mut mc.positional_args {
                    rewrite_expr(a, fsm_names);
                }
            }
            TypedExpression::Block(b) => rewrite_block(b, fsm_names),
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition, fsm_names);
                rewrite_expr(&mut if_expr.then_branch, fsm_names);
                rewrite_expr(&mut if_expr.else_branch, fsm_names);
            }
            TypedExpression::Lambda(lam) => match &mut lam.body {
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Expression(e) => {
                    rewrite_expr(e, fsm_names);
                }
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Block(block) => {
                    rewrite_block(block, fsm_names);
                }
            },
            _ => {}
        }

        // Match `<FsmName>.trigger(<arg>)` in both AST shapes (MethodCall / Call+Field).
        let trigger_call = match &expr.node {
            TypedExpression::MethodCall(mc) if mc.positional_args.len() == 1 => {
                if let TypedExpression::Variable(receiver_name) = &mc.receiver.node {
                    Some((
                        *receiver_name,
                        mc.method,
                        mc.positional_args[0].clone(),
                        expr.span,
                    ))
                } else {
                    None
                }
            }
            TypedExpression::Call(c) if c.positional_args.len() == 1 => {
                if let TypedExpression::Field(f) = &c.callee.node {
                    if let TypedExpression::Variable(receiver_name) = &f.object.node {
                        Some((
                            *receiver_name,
                            f.field,
                            c.positional_args[0].clone(),
                            expr.span,
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        let Some((receiver_name, method, path_arg, span)) = trigger_call else {
            return;
        };
        if !fsm_names.contains(&receiver_name) {
            return;
        }
        if method.resolve_global().as_deref() != Some("trigger") {
            return;
        }

        let fsm_name_arg = zyntax_typed_ast::TypedNode::new(
            TypedExpression::Literal(TypedLiteral::String(receiver_name)),
            Type::Primitive(PrimitiveType::String),
            span,
        );
        let callee = zyntax_typed_ast::TypedNode::new(
            TypedExpression::Variable(InternedString::new_global("__fsm_runtime_trigger__")),
            Type::Unknown,
            span,
        );
        expr.node = TypedExpression::Call(TypedCall {
            callee: Box::new(callee),
            positional_args: vec![fsm_name_arg, path_arg],
            named_args: vec![],
            type_args: vec![],
        });
        expr.ty = Type::Primitive(PrimitiveType::Unit);
    }

    fn rewrite_block(
        block: &mut zyntax_typed_ast::typed_ast::TypedBlock,
        fsm_names: &HashSet<InternedString>,
    ) {
        for stmt in &mut block.statements {
            rewrite_stmt(stmt, fsm_names);
        }
    }

    fn rewrite_stmt(
        stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>,
        fsm_names: &HashSet<InternedString>,
    ) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, fsm_names),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, fsm_names);
                }
            }
            TypedStatement::Return(Some(e)) => rewrite_expr(e, fsm_names),
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, fsm_names);
                rewrite_block(&mut if_stmt.then_block, fsm_names);
                if let Some(else_block) = &mut if_stmt.else_block {
                    rewrite_block(else_block, fsm_names);
                }
            }
            TypedStatement::While(w) => {
                rewrite_expr(&mut w.condition, fsm_names);
                rewrite_block(&mut w.body, fsm_names);
            }
            TypedStatement::Block(b) => rewrite_block(b, fsm_names),
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &mut func.body {
                    rewrite_block(body, &fsm_names);
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = &mut method.body {
                        rewrite_block(body, &fsm_names);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Rewrite `<FsmName>.subscribe(<path>, <closure>)` →
/// `__fsm_subscribe__("<FsmName>", <path>, <closure>)`. Path filtering happens
/// host-side in `blinc_runtime::fsm::register_subscriber`.
fn resolve_fsm_subscribe_calls(program: &mut TypedProgram) {
    use std::collections::HashSet;
    use zyntax_typed_ast::typed_ast::{TypedCall, TypedDeclaration, TypedExpression, TypedLiteral};
    use zyntax_typed_ast::InternedString;

    let mut fsm_names: HashSet<InternedString> = HashSet::new();
    for decl in &program.declarations {
        if let TypedDeclaration::Impl(imp) = &decl.node {
            if imp.trait_name.resolve_global().is_some()
                && imp
                    .methods
                    .iter()
                    .any(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
            {
                fsm_names.insert(imp.trait_name);
            }
        }
    }
    if fsm_names.is_empty() {
        return;
    }

    fn rewrite_expr(
        expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>,
        fsm_names: &HashSet<InternedString>,
    ) {
        match &mut expr.node {
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, fsm_names);
                rewrite_expr(&mut b.right, fsm_names);
            }
            TypedExpression::Unary(u) => rewrite_expr(&mut u.operand, fsm_names),
            TypedExpression::Call(c) => {
                rewrite_expr(&mut c.callee, fsm_names);
                for a in &mut c.positional_args {
                    rewrite_expr(a, fsm_names);
                }
            }
            TypedExpression::Field(f) => rewrite_expr(&mut f.object, fsm_names),
            TypedExpression::Index(idx) => {
                rewrite_expr(&mut idx.object, fsm_names);
                rewrite_expr(&mut idx.index, fsm_names);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for it in items {
                    rewrite_expr(it, fsm_names);
                }
            }
            TypedExpression::MethodCall(mc) => {
                rewrite_expr(&mut mc.receiver, fsm_names);
                for a in &mut mc.positional_args {
                    rewrite_expr(a, fsm_names);
                }
            }
            TypedExpression::Block(b) => rewrite_block(b, fsm_names),
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition, fsm_names);
                rewrite_expr(&mut if_expr.then_branch, fsm_names);
                rewrite_expr(&mut if_expr.else_branch, fsm_names);
            }
            TypedExpression::Lambda(lam) => match &mut lam.body {
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Expression(e) => {
                    rewrite_expr(e, fsm_names);
                }
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Block(block) => {
                    rewrite_block(block, fsm_names);
                }
            },
            _ => {}
        }

        // Match in both AST shapes (MethodCall / Call+Field).
        let subscribe_call = match &expr.node {
            TypedExpression::MethodCall(mc) if mc.positional_args.len() == 2 => {
                if let TypedExpression::Variable(receiver_name) = &mc.receiver.node {
                    Some((
                        *receiver_name,
                        mc.method,
                        mc.positional_args[0].clone(),
                        mc.positional_args[1].clone(),
                        expr.span,
                    ))
                } else {
                    None
                }
            }
            TypedExpression::Call(c) if c.positional_args.len() == 2 => {
                if let TypedExpression::Field(f) = &c.callee.node {
                    if let TypedExpression::Variable(receiver_name) = &f.object.node {
                        Some((
                            *receiver_name,
                            f.field,
                            c.positional_args[0].clone(),
                            c.positional_args[1].clone(),
                            expr.span,
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        let Some((receiver_name, method, path_arg, closure_arg, span)) = subscribe_call else {
            return;
        };
        if !fsm_names.contains(&receiver_name) {
            return;
        }
        if method.resolve_global().as_deref() != Some("subscribe") {
            return;
        }

        let fsm_name_arg = zyntax_typed_ast::TypedNode::new(
            TypedExpression::Literal(TypedLiteral::String(receiver_name)),
            Type::Primitive(PrimitiveType::String),
            span,
        );
        let callee = zyntax_typed_ast::TypedNode::new(
            TypedExpression::Variable(InternedString::new_global("__fsm_subscribe__")),
            Type::Unknown,
            span,
        );
        expr.node = TypedExpression::Call(TypedCall {
            callee: Box::new(callee),
            positional_args: vec![fsm_name_arg, path_arg, closure_arg],
            named_args: vec![],
            type_args: vec![],
        });
        expr.ty = Type::Primitive(PrimitiveType::Unit);
    }

    fn rewrite_block(
        block: &mut zyntax_typed_ast::typed_ast::TypedBlock,
        fsm_names: &HashSet<InternedString>,
    ) {
        for stmt in &mut block.statements {
            rewrite_stmt(stmt, fsm_names);
        }
    }

    fn rewrite_stmt(
        stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>,
        fsm_names: &HashSet<InternedString>,
    ) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, fsm_names),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, fsm_names);
                }
            }
            TypedStatement::Return(Some(e)) => rewrite_expr(e, fsm_names),
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, fsm_names);
                rewrite_block(&mut if_stmt.then_block, fsm_names);
                if let Some(else_block) = &mut if_stmt.else_block {
                    rewrite_block(else_block, fsm_names);
                }
            }
            TypedStatement::While(w) => {
                rewrite_expr(&mut w.condition, fsm_names);
                rewrite_block(&mut w.body, fsm_names);
            }
            TypedStatement::Block(b) => rewrite_block(b, fsm_names),
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &mut func.body {
                    rewrite_block(body, &fsm_names);
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = &mut method.body {
                        rewrite_block(body, &fsm_names);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Validate every `__component_call__("Name", ...)` marker references a known
/// component. Catches typos before Zyntax's less-helpful unresolved-symbol error.
/// Does NOT rewrite markers — that contract is consumed by `lower_component_calls`.
fn validate_component_calls(program: &TypedProgram) -> Result<(), Vec<String>> {
    use std::collections::HashSet;
    use zyntax_typed_ast::typed_ast::{TypedDeclaration, TypedExpression, TypedLiteral};

    let mut known: HashSet<String> = HashSet::new();
    for decl in &program.declarations {
        if let TypedDeclaration::Class(c) = &decl.node {
            if let Some(name) = c.name.resolve_global() {
                known.insert(name.to_string());
            }
        }
        // Named imports — whitelist so the validator (pre import-resolution) doesn't flag them.
        if let TypedDeclaration::Import(import) = &decl.node {
            for item in &import.items {
                if let zyntax_typed_ast::TypedImportItem::Named { name, .. } = item {
                    if let Some(s) = name.resolve_global() {
                        known.insert(s.to_string());
                    }
                }
            }
        }
    }
    // Pull pre-registered primitives (`Div`, `Text`, …) from the substrate registry.
    blinc_runtime::component::with_component_registry(|r| {
        for (_, def) in r.iter() {
            known.insert(def.name.as_ref().to_string());
        }
    });

    let mut errors: Vec<String> = Vec::new();

    fn check_expr(
        expr: &zyntax_typed_ast::TypedNode<TypedExpression>,
        known: &HashSet<String>,
        errors: &mut Vec<String>,
    ) {
        match &expr.node {
            TypedExpression::Binary(b) => {
                check_expr(&b.left, known, errors);
                check_expr(&b.right, known, errors);
            }
            TypedExpression::Unary(u) => check_expr(&u.operand, known, errors),
            TypedExpression::Call(c) => {
                check_expr(&c.callee, known, errors);
                for a in &c.positional_args {
                    check_expr(a, known, errors);
                }

                // Check `__component_call__("Name", ...)` against known set.
                if let TypedExpression::Variable(callee_name) = &c.callee.node {
                    if callee_name.resolve_global().as_deref() == Some("__component_call__") {
                        if let Some(name_node) = c.positional_args.first() {
                            if let TypedExpression::Literal(TypedLiteral::String(name)) =
                                &name_node.node
                            {
                                let name_str = name.resolve_global().unwrap_or_default();
                                if !known.contains::<str>(name_str.as_ref()) {
                                    errors.push(format!(
                                        "unknown component `{}` — declare it with \
                                         `component {} {{ ... }}` before use",
                                        name_str, name_str
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            TypedExpression::Field(f) => check_expr(&f.object, known, errors),
            TypedExpression::Index(idx) => {
                check_expr(&idx.object, known, errors);
                check_expr(&idx.index, known, errors);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for it in items {
                    check_expr(it, known, errors);
                }
            }
            TypedExpression::MethodCall(mc) => {
                check_expr(&mc.receiver, known, errors);
                for a in &mc.positional_args {
                    check_expr(a, known, errors);
                }
            }
            TypedExpression::Block(b) => check_block(b, known, errors),
            TypedExpression::If(if_expr) => {
                check_expr(&if_expr.condition, known, errors);
                check_expr(&if_expr.then_branch, known, errors);
                check_expr(&if_expr.else_branch, known, errors);
            }
            _ => {}
        }
    }

    fn check_block(
        block: &zyntax_typed_ast::typed_ast::TypedBlock,
        known: &HashSet<String>,
        errors: &mut Vec<String>,
    ) {
        for stmt in &block.statements {
            check_stmt(stmt, known, errors);
        }
    }

    fn check_stmt(
        stmt: &zyntax_typed_ast::TypedNode<TypedStatement>,
        known: &HashSet<String>,
        errors: &mut Vec<String>,
    ) {
        match &stmt.node {
            TypedStatement::Expression(e) => check_expr(e, known, errors),
            TypedStatement::Let(l) => {
                if let Some(init) = &l.initializer {
                    check_expr(init, known, errors);
                }
            }
            TypedStatement::Return(Some(e)) => check_expr(e, known, errors),
            TypedStatement::If(if_stmt) => {
                check_expr(&if_stmt.condition, known, errors);
                check_block(&if_stmt.then_block, known, errors);
                if let Some(else_block) = &if_stmt.else_block {
                    check_block(else_block, known, errors);
                }
            }
            TypedStatement::While(w) => {
                check_expr(&w.condition, known, errors);
                check_block(&w.body, known, errors);
            }
            TypedStatement::Block(b) => check_block(b, known, errors),
            _ => {}
        }
    }

    for decl in &program.declarations {
        match &decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &func.body {
                    check_block(body, &known, &mut errors);
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &imp.methods {
                    if let Some(body) = &method.body {
                        check_block(body, &known, &mut errors);
                    }
                }
            }
            _ => {}
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Rewrite `__component_call__("Name", positionals, __named__(...), body)` markers
/// into `Call(Variable("Name"), positionals, named_args, body)`. MUST run after
/// `validate_component_calls`. Slot markers inside body Blocks are left alone.
fn lower_component_calls(program: &mut TypedProgram) {
    use zyntax_typed_ast::typed_ast::{
        TypedCall, TypedDeclaration, TypedExpression, TypedLiteral, TypedNamedArg,
    };

    fn rewrite_expr(expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>) {
        // Recurse bottom-up so nested marker calls also lower.
        match &mut expr.node {
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left);
                rewrite_expr(&mut b.right);
            }
            TypedExpression::Unary(u) => rewrite_expr(&mut u.operand),
            TypedExpression::Call(c) => {
                rewrite_expr(&mut c.callee);
                for a in &mut c.positional_args {
                    rewrite_expr(a);
                }
                for n in &mut c.named_args {
                    rewrite_expr(&mut n.value);
                }
            }
            TypedExpression::Field(f) => rewrite_expr(&mut f.object),
            TypedExpression::Index(idx) => {
                rewrite_expr(&mut idx.object);
                rewrite_expr(&mut idx.index);
            }
            TypedExpression::Array(items) | TypedExpression::Tuple(items) => {
                for it in items {
                    rewrite_expr(it);
                }
            }
            TypedExpression::MethodCall(mc) => {
                rewrite_expr(&mut mc.receiver);
                for a in &mut mc.positional_args {
                    rewrite_expr(a);
                }
            }
            TypedExpression::Block(b) => rewrite_block(b),
            TypedExpression::If(if_expr) => {
                rewrite_expr(&mut if_expr.condition);
                rewrite_expr(&mut if_expr.then_branch);
                rewrite_expr(&mut if_expr.else_branch);
            }
            _ => {}
        }

        // Only act on `__component_call__` markers.
        let TypedExpression::Call(call) = &expr.node else {
            return;
        };
        let TypedExpression::Variable(callee_name) = &call.callee.node else {
            return;
        };
        if callee_name.resolve_global().as_deref() != Some("__component_call__") {
            return;
        }

        // args[0] is the component name as a StringLiteral. If
        // it's missing or shaped wrong the parser would have caught
        // it; this is a defensive bail.
        let Some(name_arg) = call.positional_args.first() else {
            return;
        };
        let TypedExpression::Literal(TypedLiteral::String(component_name)) = &name_arg.node else {
            return;
        };
        let component_name = *component_name;
        let span = expr.span;

        // Split the remaining args into positional + named. A
        // `__named__("name", value)` marker call lifts into a
        // `TypedNamedArg`. Everything else stays positional.
        let mut new_positional: Vec<zyntax_typed_ast::TypedNode<TypedExpression>> = Vec::new();
        let mut new_named: Vec<TypedNamedArg> = Vec::new();

        for arg in call.positional_args.iter().skip(1) {
            // Is this arg a `__named__("name", value)` marker call?
            if let TypedExpression::Call(inner) = &arg.node {
                if let TypedExpression::Variable(inner_callee) = &inner.callee.node {
                    if inner_callee.resolve_global().as_deref() == Some("__named__") {
                        let name_node = &inner.positional_args[0];
                        let value_node = &inner.positional_args[1];
                        let TypedExpression::Literal(TypedLiteral::String(arg_name)) =
                            &name_node.node
                        else {
                            // Ill-formed marker — fall through as positional.
                            new_positional.push(arg.clone());
                            continue;
                        };
                        new_named.push(TypedNamedArg {
                            name: *arg_name,
                            value: Box::new(value_node.clone()),
                            span: arg.span,
                        });
                        continue;
                    }
                }
            }
            new_positional.push(arg.clone());
        }

        // Carry pre-existing named_args through (defensive — grammar doesn't emit them).
        new_named.extend(call.named_args.iter().cloned());

        // Resolve callee to the registry's `view_symbol` (substrate primitives use
        // `$Blinc$<Name>$view`; user components use `<Name>$view`).
        let component_name_str = component_name.resolve_global().unwrap_or_default();
        let component_name_str: &str = component_name_str.as_ref();
        let view_symbol = blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name(component_name_str)
                .map(|def| def.view_symbol.as_ref().to_string())
        })
        .unwrap_or_else(|| format!("{component_name_str}$view"));
        let new_callee = zyntax_typed_ast::TypedNode::new(
            TypedExpression::Variable(zyntax_typed_ast::InternedString::new_global(&view_symbol)),
            Type::Any,
            span,
        );

        expr.node = TypedExpression::Call(TypedCall {
            callee: Box::new(new_callee),
            positional_args: new_positional,
            named_args: new_named,
            type_args: vec![],
        });
    }

    fn rewrite_block(block: &mut zyntax_typed_ast::typed_ast::TypedBlock) {
        let old_stmts = std::mem::take(&mut block.statements);
        let mut new_stmts: Vec<zyntax_typed_ast::TypedNode<TypedStatement>> =
            Vec::with_capacity(old_stmts.len());
        for mut stmt in old_stmts {
            rewrite_stmt(&mut stmt);
            collect_children_into(&mut new_stmts, stmt);
        }
        block.statements = new_stmts;
    }

    /// Handle body-bearing component calls. Substrate primitives: body block
    /// becomes `children: [Widget]` (plus `slot_<Name>` per slot pair).
    /// User components: flatten body statements after the call. MUST keep slot
    /// markers in place for the primitive-partition path.
    fn collect_children_into(
        out: &mut Vec<zyntax_typed_ast::TypedNode<TypedStatement>>,
        mut stmt: zyntax_typed_ast::TypedNode<TypedStatement>,
    ) {
        if let TypedStatement::Expression(expr_node) = &mut stmt.node {
            if let TypedExpression::Call(call) = &mut expr_node.node {
                let has_body_block = matches!(
                    call.positional_args.last().map(|a| &a.node),
                    Some(TypedExpression::Block(_))
                );
                if has_body_block {
                    if callee_is_substrate_primitive(call) {
                        let block_arg = call.positional_args.pop().unwrap();
                        let block_span = block_arg.span;
                        let TypedExpression::Block(body_block) = block_arg.node else {
                            unreachable!("just confirmed Block via the matches! above");
                        };

                        // Partition body statements: unnamed body
                        // entries → default `children`; entries
                        // inside `__slot_open__("X") … __slot_close__`
                        // marker pairs → `slot_X` named arg.
                        let mut default_children: Vec<
                            zyntax_typed_ast::TypedNode<TypedExpression>,
                        > = Vec::new();
                        let mut slot_buckets: Vec<(
                            String,
                            Vec<zyntax_typed_ast::TypedNode<TypedExpression>>,
                        )> = Vec::new();
                        let mut current_slot: Option<String> = None;

                        for s in body_block.statements {
                            if let Some(name) = slot_open_name(&s) {
                                current_slot = Some(name);
                                continue;
                            }
                            if is_slot_close_stmt(&s) {
                                current_slot = None;
                                continue;
                            }
                            let TypedStatement::Expression(e) = s.node else {
                                continue;
                            };
                            match &current_slot {
                                None => default_children.push(*e),
                                Some(name) => {
                                    if let Some(bucket) =
                                        slot_buckets.iter_mut().find(|(n, _)| n == name)
                                    {
                                        bucket.1.push(*e);
                                    } else {
                                        slot_buckets.push((name.clone(), vec![*e]));
                                    }
                                }
                            }
                        }

                        if !default_children.is_empty() {
                            call.named_args.push(zyntax_typed_ast::TypedNamedArg {
                                name: zyntax_typed_ast::InternedString::new_global("children"),
                                value: Box::new(zyntax_typed_ast::TypedNode::new(
                                    TypedExpression::Array(default_children),
                                    Type::Any,
                                    block_span,
                                )),
                                span: block_span,
                            });
                        }
                        for (name, exprs) in slot_buckets {
                            let arg_name = format!("slot_{name}");
                            call.named_args.push(zyntax_typed_ast::TypedNamedArg {
                                name: zyntax_typed_ast::InternedString::new_global(&arg_name),
                                value: Box::new(zyntax_typed_ast::TypedNode::new(
                                    TypedExpression::Array(exprs),
                                    Type::Any,
                                    block_span,
                                )),
                                span: block_span,
                            });
                        }

                        out.push(stmt);
                        return;
                    }

                    // User-declared component with a body — fall
                    // back to flatten: push the body-less call,
                    // then inline each child statement at the
                    // outer level. Slot markers are dropped here
                    // (user-component view methods don't accept
                    // named slots yet).
                    let block_arg = call.positional_args.pop().unwrap();
                    let TypedExpression::Block(body_block) = block_arg.node else {
                        unreachable!("just confirmed Block via the matches! above");
                    };
                    out.push(stmt);
                    for inner in body_block.statements {
                        if is_slot_marker_stmt(&inner) {
                            continue;
                        }
                        collect_children_into(out, inner);
                    }
                    return;
                }
            }
        }

        out.push(stmt);
    }

    /// Match `__slot_open__("name")` and return `"name"`.
    fn slot_open_name(stmt: &zyntax_typed_ast::TypedNode<TypedStatement>) -> Option<String> {
        let TypedStatement::Expression(e) = &stmt.node else {
            return None;
        };
        let TypedExpression::Call(c) = &e.node else {
            return None;
        };
        let TypedExpression::Variable(callee) = &c.callee.node else {
            return None;
        };
        if callee.resolve_global().as_deref() != Some("__slot_open__") {
            return None;
        }
        let arg = c.positional_args.first()?;
        let TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::String(name)) = &arg.node
        else {
            return None;
        };
        name.resolve_global().map(|s| s.to_string())
    }

    /// Match `__slot_close__()` — ends the active slot bucket.
    fn is_slot_close_stmt(stmt: &zyntax_typed_ast::TypedNode<TypedStatement>) -> bool {
        let TypedStatement::Expression(e) = &stmt.node else {
            return false;
        };
        let TypedExpression::Call(c) = &e.node else {
            return false;
        };
        let TypedExpression::Variable(callee) = &c.callee.node else {
            return false;
        };
        callee.resolve_global().as_deref() == Some("__slot_close__")
    }

    /// Callee is a substrate primitive (mangled name begins with `$Blinc$`).
    fn callee_is_substrate_primitive(call: &TypedCall) -> bool {
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return false;
        };
        callee
            .resolve_global()
            .as_deref()
            .is_some_and(|s| s.starts_with("$Blinc$"))
    }

    /// `Expression(Call(Variable("__slot_open__" | "__slot_close__"), _))`.
    fn is_slot_marker_stmt(stmt: &zyntax_typed_ast::TypedNode<TypedStatement>) -> bool {
        let TypedStatement::Expression(expr_node) = &stmt.node else {
            return false;
        };
        let TypedExpression::Call(call) = &expr_node.node else {
            return false;
        };
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return false;
        };
        matches!(
            callee.resolve_global().as_deref(),
            Some("__slot_open__") | Some("__slot_close__")
        )
    }

    fn rewrite_stmt(stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init);
                }
            }
            TypedStatement::Return(Some(e)) => rewrite_expr(e),
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition);
                rewrite_block(&mut if_stmt.then_block);
                if let Some(else_block) = &mut if_stmt.else_block {
                    rewrite_block(else_block);
                }
            }
            TypedStatement::While(w) => {
                rewrite_expr(&mut w.condition);
                rewrite_block(&mut w.body);
            }
            TypedStatement::Block(b) => rewrite_block(b),
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &mut func.body {
                    rewrite_block(body);
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = &mut method.body {
                        rewrite_block(body);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Lift `__component_props__` marker params onto every other method in the impl,
/// then strip the marker. Idempotent.
fn bind_component_props(program: &mut TypedProgram) {
    use zyntax_typed_ast::typed_ast::TypedDeclaration;

    for decl in program.declarations.iter_mut() {
        let TypedDeclaration::Impl(imp) = &mut decl.node else {
            continue;
        };

        let prop_params = imp
            .methods
            .iter_mut()
            .find(|m| m.name.resolve_global().as_deref() == Some("__component_props__"))
            .map(|m| std::mem::take(&mut m.params));

        let Some(prop_params) = prop_params else {
            continue;
        };

        // Props MUST come first — call site lowers `Counter(1, 2)` to `Counter$view(1, 2)`.
        for method in imp.methods.iter_mut() {
            if method.name.resolve_global().as_deref() == Some("__component_props__") {
                continue;
            }
            let mut new_params = prop_params.clone();
            new_params.extend(std::mem::take(&mut method.params));
            method.params = new_params;
        }

        // Strip the marker so compile doesn't expose a `Counter$__component_props__`.
        imp.methods
            .retain(|m| m.name.resolve_global().as_deref() != Some("__component_props__"));
    }
}

/// Wrap each `__fsm_meta__` body with `__fsm_begin__("Name")` / `__fsm_end__()`
/// so inner marker calls know which fsm they're configuring. Idempotent.
fn inject_fsm_context_markers(program: &mut TypedProgram) {
    use zyntax_typed_ast::typed_ast::{
        TypedCall, TypedDeclaration, TypedExpression, TypedLiteral, TypedStatement,
    };
    use zyntax_typed_ast::{InternedString, TypedNode};

    fn make_marker_call(callee: &str, str_args: &[&str]) -> TypedNode<TypedStatement> {
        let args: Vec<TypedNode<TypedExpression>> = str_args
            .iter()
            .map(|s| {
                TypedNode::new(
                    TypedExpression::Literal(TypedLiteral::String(InternedString::new_global(s))),
                    Type::Primitive(PrimitiveType::String),
                    Span::default(),
                )
            })
            .collect();

        let call = TypedExpression::Call(TypedCall {
            callee: Box::new(TypedNode::new(
                TypedExpression::Variable(InternedString::new_global(callee)),
                Type::Unknown,
                Span::default(),
            )),
            positional_args: args,
            named_args: vec![],
            type_args: vec![],
        });

        TypedNode::new(
            TypedStatement::Expression(Box::new(TypedNode::new(
                call,
                Type::Primitive(PrimitiveType::Unit),
                Span::default(),
            ))),
            Type::Primitive(PrimitiveType::Unit),
            Span::default(),
        )
    }

    for decl in &mut program.declarations {
        let TypedDeclaration::Impl(imp) = &mut decl.node else {
            continue;
        };
        let Some(fsm_name) = imp.trait_name.resolve_global() else {
            continue;
        };

        for method in &mut imp.methods {
            if method.name.resolve_global().as_deref() != Some("__fsm_meta__") {
                continue;
            }
            let Some(body) = method.body.as_mut() else {
                continue;
            };

            // Skip if already wrapped (defensive against double-application).
            let already_wrapped = body
                .statements
                .first()
                .map(|s| {
                    let TypedStatement::Expression(e) = &s.node else {
                        return false;
                    };
                    let TypedExpression::Call(c) = &e.node else {
                        return false;
                    };
                    let TypedExpression::Variable(callee) = &c.callee.node else {
                        return false;
                    };
                    callee.resolve_global().as_deref() == Some("__fsm_begin__")
                })
                .unwrap_or(false);
            if already_wrapped {
                continue;
            }

            let begin = make_marker_call("__fsm_begin__", &[&fsm_name]);
            let end = make_marker_call("__fsm_end__", &[]);
            body.statements.insert(0, begin);
            body.statements.push(end);
        }
    }
}

/// Populate the global `FsmRegistry` from each fsm's `__fsm_meta__` body and
/// strip the meta method. Three phases: scan, pin TypeIds, strip markers.
fn populate_fsm_registry_pass(
    program: &mut TypedProgram,
    module: zyntax_typed_ast::InternedString,
) {
    use zyntax_typed_ast::type_registry::{
        TypeDefinition, TypeId, TypeKind, VariantDef, VariantFields, Visibility,
    };
    use zyntax_typed_ast::typed_ast::{
        TypedDeclaration, TypedExpression, TypedLiteral, TypedVariantFields,
    };
    use zyntax_typed_ast::InternedString;

    // Phase 1: scan. Collect (fsm_name, FsmDefinition) tuples.
    let mut found: Vec<(InternedString, FsmDefinition)> = Vec::new();
    let mut guards_to_lift: Vec<(
        InternedString,
        zyntax_typed_ast::TypedNode<zyntax_typed_ast::TypedExpression>,
    )> = Vec::new();

    for decl in &program.declarations {
        let TypedDeclaration::Impl(imp) = &decl.node else {
            continue;
        };
        let Some(meta) = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
        else {
            continue;
        };
        let Some(body) = meta.body.as_ref() else {
            continue;
        };

        let fsm_name = imp.trait_name;
        let mut def = FsmDefinition {
            name: Some(fsm_name),
            ..Default::default()
        };

        for stmt_node in &body.statements {
            let TypedStatement::Expression(expr_node) = &stmt_node.node else {
                continue;
            };
            let TypedExpression::Call(call) = &expr_node.node else {
                continue;
            };
            let TypedExpression::Variable(callee_id) = &call.callee.node else {
                continue;
            };
            let callee = callee_id.resolve_global().unwrap_or_default();

            // Helper: pull a string-literal arg at index `idx`.
            let str_arg = |idx: usize| -> Option<InternedString> {
                call.positional_args.get(idx).and_then(|a| {
                    if let TypedExpression::Literal(TypedLiteral::String(s)) = &a.node {
                        Some(*s)
                    } else {
                        None
                    }
                })
            };

            match callee.as_str() {
                "__fsm_initial__" => {
                    if let Some(state) = str_arg(0) {
                        def.initial = Some(state);
                    }
                }
                "__fsm_transition__" => {
                    if let (Some(from), Some(event), Some(to)) =
                        (str_arg(0), str_arg(1), str_arg(2))
                    {
                        def.transitions.push(EventTransition {
                            from,
                            event,
                            to,
                            actions: vec![],
                        });
                    }
                }
                "__fsm_tick__" => {
                    // args: 0=from, 1=guard expr, 2=to. Lift guard into a top-level fn
                    // so it survives `__fsm_meta__` stripping.
                    if let (Some(from), Some(to)) = (str_arg(0), str_arg(2)) {
                        let idx = def.tick_guards.len();
                        let fsm_name_str = fsm_name.resolve_global().unwrap_or_default();
                        let guard_fn_name = format!("__fsm_tick_guard_{fsm_name_str}_{idx}__");
                        let guard_fn = InternedString::new_global(&guard_fn_name);

                        // Clone the guard expression to escape the read borrow on `program`.
                        if let Some(expr_node) = call.positional_args.get(1) {
                            guards_to_lift.push((guard_fn, expr_node.clone()));
                        }

                        def.tick_guards.push(TickGuard {
                            from,
                            to,
                            guard_fn: Some(guard_fn),
                        });
                    }
                }
                _ => {}
            }
        }

        found.push((fsm_name, def));
    }

    // Phase 2: pin TypeIds + populate the registry. Pre-register so Zyntax's
    // compile path short-circuits and respects our id.
    for (fsm_name, def) in &found {
        let type_id = TypeId::next();

        // Pin `decl.ty` so Zyntax's enum-registration check respects our id.
        let named_ty = program.type_registry.make_type(type_id, Vec::new());
        for decl in &mut program.declarations {
            let TypedDeclaration::Enum(enum_decl) = &decl.node else {
                continue;
            };
            if enum_decl.name == *fsm_name {
                decl.ty = named_ty.clone();
                break;
            }
        }

        // Pre-register so Zyntax skips double-registration with a fresh TypeId.
        if let Some(enum_decl) = program.declarations.iter().find_map(|d| match &d.node {
            TypedDeclaration::Enum(e) if e.name == *fsm_name => Some(e),
            _ => None,
        }) {
            let variants: Vec<VariantDef> = enum_decl
                .variants
                .iter()
                .enumerate()
                .map(|(i, v)| VariantDef {
                    name: v.name,
                    fields: match &v.fields {
                        TypedVariantFields::Unit => VariantFields::Unit,
                        TypedVariantFields::Tuple(types) => VariantFields::Tuple(types.clone()),
                        TypedVariantFields::Named(_) => VariantFields::Unit,
                    },
                    discriminant: Some(i as i64),
                    span: v.span,
                })
                .collect();

            let type_def = TypeDefinition {
                id: type_id,
                name: enum_decl.name,
                kind: TypeKind::Enum { variants },
                type_params: Vec::new(),
                constraints: Vec::new(),
                fields: Vec::new(),
                methods: Vec::new(),
                constructors: Vec::new(),
                metadata: Default::default(),
                span: enum_decl.span,
            };
            let _: TypeId = program.type_registry.register_type(type_def);
            let _ = Visibility::Public; // silence unused-import in case the path changes upstream
        }

        let id = FsmId { module, type_id };
        with_fsm_registry_mut(|r| r.upsert(id, def.clone()));
    }

    // Phase 2.5: lift each captured tick-guard expression into a top-level fn
    // returning i32 (1 if guard fires, 0 otherwise). i32 chosen because bool-return
    // ABI marshaling through `runtime.call::<bool>` is untested upstream.
    use zyntax_typed_ast::typed_ast::{TypedFunction, TypedIf};
    for (fn_name, guard_expr) in guards_to_lift {
        let i32_ty = Type::Primitive(PrimitiveType::I32);

        // `return 1`
        let return_one = zyntax_typed_ast::TypedNode::new(
            TypedStatement::Return(Some(Box::new(zyntax_typed_ast::TypedNode::new(
                TypedExpression::Literal(zyntax_typed_ast::typed_ast::TypedLiteral::Integer(1)),
                i32_ty.clone(),
                Span::default(),
            )))),
            i32_ty.clone(),
            Span::default(),
        );

        let then_block = zyntax_typed_ast::typed_ast::TypedBlock {
            statements: vec![return_one],
            span: Span::default(),
        };

        // `if <guard> { return 1 }`
        let if_stmt = zyntax_typed_ast::TypedNode::new(
            TypedStatement::If(TypedIf {
                condition: Box::new(guard_expr),
                then_block,
                else_block: None,
                span: Span::default(),
            }),
            Type::Primitive(PrimitiveType::Unit),
            Span::default(),
        );

        // `return 0`
        let return_zero = zyntax_typed_ast::TypedNode::new(
            TypedStatement::Return(Some(Box::new(zyntax_typed_ast::TypedNode::new(
                TypedExpression::Literal(zyntax_typed_ast::typed_ast::TypedLiteral::Integer(0)),
                i32_ty.clone(),
                Span::default(),
            )))),
            i32_ty.clone(),
            Span::default(),
        );

        let body = zyntax_typed_ast::typed_ast::TypedBlock {
            statements: vec![if_stmt, return_zero],
            span: Span::default(),
        };

        let func = TypedFunction {
            name: fn_name,
            return_type: i32_ty.clone(),
            body: Some(body),
            ..Default::default()
        };
        let decl_node = zyntax_typed_ast::TypedNode::new(
            TypedDeclaration::Function(func),
            Type::Unknown,
            Span::default(),
        );
        program.declarations.push(decl_node);
    }

    // Phase 3: strip `__fsm_meta__` so compile doesn't try to resolve markers.
    for decl in &mut program.declarations {
        let TypedDeclaration::Impl(imp) = &mut decl.node else {
            continue;
        };
        imp.methods
            .retain(|m| m.name.resolve_global().as_deref() != Some("__fsm_meta__"));
    }
}

/// Synthesise a sibling `<FSM>Event` enum for every fsm with transitions.
/// Variants are the unique event names in declaration order. Tick transitions
/// don't have user-facing event names and never appear here.
fn synthesize_fsm_event_enums(program: &mut TypedProgram) {
    use std::collections::HashSet;
    use zyntax_typed_ast::type_registry::Visibility;
    use zyntax_typed_ast::typed_ast::{
        TypedDeclaration, TypedEnum, TypedExpression, TypedLiteral, TypedVariant,
        TypedVariantFields,
    };
    use zyntax_typed_ast::{InternedString, TypedNode};

    let mut event_enums: Vec<TypedNode<TypedDeclaration>> = Vec::new();

    for decl in &program.declarations {
        let TypedDeclaration::Impl(imp) = &decl.node else {
            continue;
        };

        // Find the synthesised `__fsm_meta__` method.
        let Some(meta) = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
        else {
            continue;
        };
        let Some(body) = meta.body.as_ref() else {
            continue;
        };

        // Collect unique event names from `__fsm_transition__(_, event,
        // _)` markers, preserving declaration order so the runtime
        // discriminant assignment is stable.
        let mut events: Vec<InternedString> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        for stmt_node in &body.statements {
            let TypedStatement::Expression(expr_node) = &stmt_node.node else {
                continue;
            };
            let TypedExpression::Call(call) = &expr_node.node else {
                continue;
            };
            let TypedExpression::Variable(callee) = &call.callee.node else {
                continue;
            };
            if callee.resolve_global().as_deref() != Some("__fsm_transition__") {
                continue;
            }
            let Some(event_arg) = call.positional_args.get(1) else {
                continue;
            };
            let TypedExpression::Literal(TypedLiteral::String(name)) = &event_arg.node else {
                continue;
            };
            let key = name.resolve_global().unwrap_or_default();
            if !key.is_empty() && seen.insert(key) {
                events.push(*name);
            }
        }

        if events.is_empty() {
            // Tick-only fsm — nothing to synthesise.
            continue;
        }

        // Use `trait_name` (bare ident) rather than `for_type` (Type::Named).
        let fsm_name = imp.trait_name.resolve_global().unwrap_or_default();
        let event_enum_name = InternedString::new_global(&format!("{fsm_name}Event"));

        let variants: Vec<TypedVariant> = events
            .into_iter()
            .map(|name| TypedVariant {
                name,
                fields: TypedVariantFields::Unit,
                discriminant: None,
                span: Span::default(),
            })
            .collect();

        let event_enum = TypedDeclaration::Enum(TypedEnum {
            name: event_enum_name,
            type_params: vec![],
            variants,
            visibility: Visibility::Public,
            span: Span::default(),
        });

        event_enums.push(TypedNode::new(event_enum, Type::Unknown, Span::default()));
    }

    // Append at the end so `find_map` lookups still return user-declared decls first.
    program.declarations.extend(event_enums);
}

/// Desugar `match` marker-statement quads into `if/else if/.../else` chains
/// over string equality. Wildcard arm becomes the trailing `else`.
fn lower_match_blocks(program: &mut TypedProgram) {
    use zyntax_typed_ast::typed_ast::{
        BinaryOp, TypedBinary, TypedBlock, TypedDeclaration, TypedExpression, TypedIf, TypedLiteral,
    };
    use zyntax_typed_ast::TypedNode;

    fn is_call_to(stmt: &TypedNode<TypedStatement>, name: &str) -> bool {
        let TypedStatement::Expression(expr) = &stmt.node else {
            return false;
        };
        let TypedExpression::Call(call) = &expr.node else {
            return false;
        };
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return false;
        };
        callee.resolve_global().as_deref() == Some(name)
    }

    fn call_first_arg(stmt: &TypedNode<TypedStatement>) -> Option<&TypedNode<TypedExpression>> {
        let TypedStatement::Expression(expr) = &stmt.node else {
            return None;
        };
        let TypedExpression::Call(call) = &expr.node else {
            return None;
        };
        call.positional_args.first()
    }

    /// Lower every `__match_begin__ … __match_end__` span in `stmts`.
    /// MUST recurse into nested blocks first so inner matches lower before outers see them.
    fn rewrite_stmts(stmts: &mut Vec<TypedNode<TypedStatement>>) {
        for stmt in stmts.iter_mut() {
            recurse_into_stmt(stmt);
        }

        let mut i = 0;
        while i < stmts.len() {
            if !is_call_to(&stmts[i], "__match_begin__") {
                i += 1;
                continue;
            }
            let Some(scrutinee_expr) = call_first_arg(&stmts[i]).cloned() else {
                i += 1;
                continue;
            };

            let mut end_idx = i + 1;
            while end_idx < stmts.len() && !is_call_to(&stmts[end_idx], "__match_end__") {
                end_idx += 1;
            }
            if end_idx >= stmts.len() {
                // Malformed — no end marker.
                i += 1;
                continue;
            }

            // Each arm at i+1..end_idx is a Block whose first stmt is `__match_arm__(pat)`.
            let mut arms: Vec<(Option<String>, TypedBlock)> = Vec::new();
            for arm in stmts[(i + 1)..end_idx].iter() {
                let TypedStatement::Block(arm_block) = &arm.node else {
                    continue;
                };
                if arm_block.statements.is_empty() {
                    continue;
                }
                if !is_call_to(&arm_block.statements[0], "__match_arm__") {
                    continue;
                }
                let pat_str = call_first_arg(&arm_block.statements[0]).and_then(|expr| {
                    if let TypedExpression::Literal(TypedLiteral::String(s)) = &expr.node {
                        s.resolve_global().map(|s| s.to_string())
                    } else {
                        None
                    }
                });
                let body = TypedBlock {
                    statements: arm_block.statements[1..].to_vec(),
                    span: arm_block.span,
                };
                arms.push((pat_str, body));
            }

            // Build the if/else-if/else chain. First `_` arm becomes trailing `else`.
            let mut else_block: Option<TypedBlock> = None;
            let mut chain_arms: Vec<(String, TypedBlock)> = Vec::new();
            for (pat, body) in arms {
                match pat.as_deref() {
                    Some("__wildcard__") if else_block.is_none() => {
                        else_block = Some(body);
                    }
                    Some("__wildcard__") => {}
                    Some(p) => {
                        chain_arms.push((p.to_string(), body));
                    }
                    None => {}
                }
            }

            // Fold from last to first so the FIRST arm wraps everything else.
            let mut tail_else = else_block;
            for (pat, body) in chain_arms.into_iter().rev() {
                let span = body.span;
                let pat_literal = TypedNode::new(
                    TypedExpression::Literal(TypedLiteral::String(
                        zyntax_typed_ast::InternedString::new_global(&pat),
                    )),
                    Type::Primitive(PrimitiveType::String),
                    span,
                );
                let condition = TypedNode::new(
                    TypedExpression::Binary(TypedBinary {
                        op: BinaryOp::Eq,
                        left: Box::new(scrutinee_expr.clone()),
                        right: Box::new(pat_literal),
                    }),
                    Type::Primitive(PrimitiveType::Bool),
                    span,
                );
                let if_stmt = TypedStatement::If(TypedIf {
                    condition: Box::new(condition),
                    then_block: body,
                    else_block: tail_else.take(),
                    span,
                });
                tail_else = Some(TypedBlock {
                    statements: vec![TypedNode::new(
                        if_stmt,
                        Type::Primitive(PrimitiveType::Unit),
                        span,
                    )],
                    span,
                });
            }

            // Splice the chain in place of the marker span.
            let chain_stmts = tail_else.map(|b| b.statements).unwrap_or_default();
            stmts.splice(i..=end_idx, chain_stmts);
            i += 1;
        }
    }

    fn recurse_into_stmt(stmt: &mut TypedNode<TypedStatement>) {
        match &mut stmt.node {
            TypedStatement::Block(b) => {
                rewrite_stmts(&mut b.statements);
            }
            TypedStatement::If(if_stmt) => {
                rewrite_stmts(&mut if_stmt.then_block.statements);
                if let Some(else_block) = &mut if_stmt.else_block {
                    rewrite_stmts(&mut else_block.statements);
                }
            }
            TypedStatement::While(w) => {
                rewrite_stmts(&mut w.body.statements);
            }
            TypedStatement::Expression(expr) => {
                recurse_into_expr(expr);
            }
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    recurse_into_expr(init);
                }
            }
            _ => {}
        }
    }

    fn recurse_into_expr(expr: &mut TypedNode<TypedExpression>) {
        // Lambda bodies need this: `<Fsm>.subscribe(..., || { match … })` must
        // lower before any downstream pass walks the lambda HIR.
        match &mut expr.node {
            TypedExpression::Lambda(lam) => match &mut lam.body {
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Expression(e) => {
                    recurse_into_expr(e);
                }
                zyntax_typed_ast::typed_ast::TypedLambdaBody::Block(block) => {
                    rewrite_stmts(&mut block.statements);
                }
            },
            TypedExpression::Block(block) => {
                rewrite_stmts(&mut block.statements);
            }
            TypedExpression::Call(call) => {
                recurse_into_expr(&mut call.callee);
                for arg in &mut call.positional_args {
                    recurse_into_expr(arg);
                }
            }
            TypedExpression::Binary(b) => {
                recurse_into_expr(&mut b.left);
                recurse_into_expr(&mut b.right);
            }
            TypedExpression::If(if_expr) => {
                recurse_into_expr(&mut if_expr.condition);
                recurse_into_expr(&mut if_expr.then_branch);
                recurse_into_expr(&mut if_expr.else_branch);
            }
            _ => {}
        }
    }

    for decl in &mut program.declarations {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = &mut func.body {
                    rewrite_stmts(&mut body.statements);
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = &mut method.body {
                        rewrite_stmts(&mut body.statements);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Mint a placeholder `Interface { name: <FsmName> }` for each FSM impl so
/// Zyntax's compiler doesn't log "Trait not found" and drop the impl's methods.
fn synthesize_fsm_trait_interfaces(program: &mut TypedProgram) {
    use std::collections::HashSet;
    use zyntax_typed_ast::type_registry::Visibility;
    use zyntax_typed_ast::typed_ast::{TypedDeclaration, TypedInterface};
    use zyntax_typed_ast::{InternedString, TypedNode};

    let mut fsm_names: HashSet<InternedString> = HashSet::new();
    for decl in &program.declarations {
        let TypedDeclaration::Impl(imp) = &decl.node else {
            continue;
        };
        if imp
            .methods
            .iter()
            .any(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
        {
            fsm_names.insert(imp.trait_name);
        }
    }

    let interfaces: Vec<TypedNode<TypedDeclaration>> = fsm_names
        .into_iter()
        .map(|name| {
            let iface = TypedInterface {
                name,
                type_params: vec![],
                extends: vec![],
                methods: vec![],
                associated_types: vec![],
                visibility: Visibility::Public,
                span: Span::default(),
            };
            TypedNode::new(
                TypedDeclaration::Interface(iface),
                Type::Unknown,
                Span::default(),
            )
        })
        .collect();

    program.declarations.extend(interfaces);
}

// =====================================================================
// Runtime-substrate bridge (blinc_runtime::fsm)
// =====================================================================
//
// JIT-side impls of the substrate traits. Both publishers (JIT here, future
// LLVM AOT) write to the same `FsmRegistry` and install their own dispatcher.

/// JIT `GuardDispatcher` — routes tick-guard calls through `ZyntaxRuntime`.
/// Lifted guards return `i32` (1 = fires, 0 = doesn't).
struct JitGuardDispatcher {
    runtime: Arc<Mutex<ZyntaxRuntime>>,
}

// SAFETY: `ZyntaxRuntime` is `!Send + !Sync` (Cranelift `JITModule`). The
// surrounding `Mutex` serialises access; UI threads run single-threaded anyway.
// The unsafe impl is what lets `Arc<dyn GuardDispatcher>` hold a JIT dispatcher.
unsafe impl Send for JitGuardDispatcher {}
unsafe impl Sync for JitGuardDispatcher {}

impl blinc_runtime::fsm::GuardDispatcher for JitGuardDispatcher {
    fn call_guard(&self, symbol: &str) -> Option<bool> {
        let runtime = self.runtime.lock().ok()?;
        // Direct JIT dispatch: `call_function` routes through the
        // new BC interp tier which fails with `Host("missing block
        // …")` on lifted-guard HIR shapes; `call_raw` routes
        // through `call_dynamic_function` → zrtl TypeMeta lookup
        // which null-derefs for user-compiled fns. Transmute the
        // JIT pointer to `extern "C" fn() -> i32` instead —
        // exactly the shape `populate_fsm_registry_pass` lifts
        // guards to.
        let ptr = runtime.get_function_ptr(symbol)?;
        let guard: extern "C" fn() -> i32 = unsafe { std::mem::transmute(ptr) };
        Some(guard() != 0)
    }
}

/// JIT `ViewRenderer` — value-returning views call as `() -> i64` (handle);
/// legacy Unit-returning views call as `() -> ()` and drain the scene-op buffer.
struct JitViewRenderer {
    runtime: Arc<Mutex<ZyntaxRuntime>>,
    value_returning_views: Arc<Mutex<std::collections::HashSet<String>>>,
}

// SAFETY: same as `JitGuardDispatcher` — Mutex serialises access to `!Send` runtime.
unsafe impl Send for JitViewRenderer {}
unsafe impl Sync for JitViewRenderer {}

impl blinc_runtime::view::ViewRenderer for JitViewRenderer {
    fn render_named(
        &self,
        symbol: &str,
    ) -> Result<ZyntaxValue, blinc_runtime::view::ViewRenderError> {
        let is_value_returning = self
            .value_returning_views
            .lock()
            .map(|set| set.contains(symbol))
            .unwrap_or(false);

        let runtime = self.runtime.lock().map_err(|_| {
            blinc_runtime::view::ViewRenderError::Backend(
                "BlincDsl runtime mutex poisoned".to_string(),
            )
        })?;
        if is_value_returning {
            // Direct JIT dispatch — see [`JitGuardDispatcher::call_guard`].
            let ptr = runtime.get_function_ptr(symbol).ok_or_else(|| {
                blinc_runtime::view::ViewRenderError::Backend(format!(
                    "view symbol '{symbol}' not registered in runtime"
                ))
            })?;
            let view: extern "C" fn() -> i64 = unsafe { std::mem::transmute(ptr) };
            Ok(ZyntaxValue::Int(view()))
        } else {
            runtime
                .call::<()>(symbol, &[])
                .map_err(|e| blinc_runtime::view::ViewRenderError::Backend(e.to_string()))?;
            Ok(ZyntaxValue::Void)
        }
    }
}

/// Pre-register `blinc_layout` widget primitives (`Div`, `Text`, …) in the
/// substrate's `ComponentRegistry`. Idempotent; called once at `BlincDsl::new()`.
fn register_blinc_layout_primitives() {
    use blinc_runtime::component::{ComponentDefinition, PropDef, Type};
    use zyntax_typed_ast::type_registry::PrimitiveType;
    use zyntax_typed_ast::InternedString;

    let string_ty = Type::Primitive(PrimitiveType::String);

    // `Div { ..children }` — container. `children` and `__style` cross as `i64` payloads.
    let div = ComponentDefinition {
        name: std::sync::Arc::from("Div"),
        view_symbol: std::sync::Arc::from("$Blinc$Div$view"),
        props: vec![
            PropDef {
                name: std::sync::Arc::from("children"),
                ty: Type::Primitive(PrimitiveType::I64),
            },
            PropDef {
                name: std::sync::Arc::from("__style"),
                ty: Type::Primitive(PrimitiveType::I64),
            },
            PropDef {
                name: std::sync::Arc::from("class"),
                ty: Type::Primitive(PrimitiveType::String),
            },
            PropDef {
                // `on_click = || { … }` — Zyntax closure value as `i64`.
                name: std::sync::Arc::from("on_click"),
                ty: Type::Primitive(PrimitiveType::I64),
            },
        ],
    };

    // `Text("hi")` — text leaf. Styling props land later.
    let text_widget = ComponentDefinition {
        name: std::sync::Arc::from("Text"),
        view_symbol: std::sync::Arc::from("$Blinc$Text$view"),
        props: vec![PropDef {
            name: std::sync::Arc::from("content"),
            ty: string_ty.clone(),
        }],
    };

    blinc_runtime::component::with_component_registry_mut(|r| {
        r.register(div);
        r.register(text_widget);
    });

    let _ = InternedString::new_global("__blinc_layout_primitives_marker__");
}

/// Mirror DSL component decls (impl + matching Class) into the runtime's
/// `ComponentRegistry`. View symbol is `<Name>$view`.
fn publish_components_to_runtime_registry(program: &TypedProgram) {
    use zyntax_typed_ast::typed_ast::TypedDeclaration;

    for decl in &program.declarations {
        let TypedDeclaration::Impl(imp) = &decl.node else {
            continue;
        };

        // `for_type` is usually `Type::Unresolved(name)` mid-pipeline;
        // post-resolution it can be `Type::Named { id, ... }`.
        let component_name_intern = match &imp.for_type {
            Type::Unresolved(name) => *name,
            Type::Named { id, .. } => {
                if let Some(type_def) = program.type_registry.get_type_by_id(*id) {
                    type_def.name
                } else {
                    continue;
                }
            }
            _ => continue,
        };
        let component_name_string = match component_name_intern.resolve_global() {
            Some(s) => s,
            None => continue,
        };
        let component_name: &str = component_name_string.as_ref();

        // Only register impls with a sibling Class — skips FSM impls and orphan impls.
        let class_match = program.declarations.iter().any(|d| match &d.node {
            TypedDeclaration::Class(c) => c.name == component_name_intern,
            _ => false,
        });
        if !class_match {
            continue;
        }

        // Find the view method. Bodyless components are skipped defensively.
        let Some(view_method) = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
        else {
            continue;
        };

        // Each view param becomes a `PropDef`; `ty` passes through unchanged.
        let props: Vec<blinc_runtime::component::PropDef> = view_method
            .params
            .iter()
            .filter(|p| !p.is_self)
            .filter_map(|p| {
                let name_str = p.name.resolve_global()?;
                Some(blinc_runtime::component::PropDef {
                    name: std::sync::Arc::from(name_str.as_ref()),
                    ty: p.ty.clone(),
                })
            })
            .collect();

        let runtime_def = blinc_runtime::component::ComponentDefinition {
            name: std::sync::Arc::from(component_name),
            view_symbol: std::sync::Arc::from(format!("{component_name}$view").as_str()),
            props,
        };

        blinc_runtime::component::with_component_registry_mut(|r| {
            r.register(runtime_def);
        });
    }
}

/// Publish the local FSM registry into `blinc_runtime::fsm::FsmRegistry`.
/// State codes = enum decl order. Event codes = first-appearance order,
/// offset by `FSM_EVENT_CODE_OFFSET` to avoid colliding with pointer event codes.
fn publish_fsms_to_runtime_registry(program: &TypedProgram) {
    use zyntax_typed_ast::typed_ast::TypedDeclaration;

    for decl in &program.declarations {
        let TypedDeclaration::Impl(imp) = &decl.node else {
            continue;
        };
        let fsm_name_intern = imp.trait_name;
        let Some(fsm_name) = fsm_name_intern.resolve_global() else {
            continue;
        };

        // Find the matching state-enum. Mid-pipeline FSMs always have one.
        let state_enum = program.declarations.iter().find_map(|d| match &d.node {
            TypedDeclaration::Enum(e) if e.name == fsm_name_intern => Some(e),
            _ => None,
        });
        let Some(state_enum) = state_enum else {
            continue;
        };

        // Read local registry. Missing entry → bail (idempotent).
        let local_def = with_fsm_registry(|r| {
            r.iter()
                .map(|(_, d)| d)
                .find(|d| d.name.map(|n| n == fsm_name_intern).unwrap_or(false))
                .cloned()
        });
        let Some(local_def) = local_def else {
            continue;
        };

        // State codes = indices into declaration order.
        let state_names: Vec<std::sync::Arc<str>> = state_enum
            .variants
            .iter()
            .map(|v| std::sync::Arc::from(v.name.resolve_global().unwrap_or_default().as_ref()))
            .collect();
        let state_code = |name: zyntax_typed_ast::InternedString| -> Option<u32> {
            let s = name.resolve_global()?;
            let needle: &str = s.as_ref();
            state_names
                .iter()
                .position(|n| {
                    let n_ref: &str = n.as_ref();
                    n_ref == needle
                })
                .map(|i| i as u32)
        };

        // Event codes — first-appearance order, offset to avoid POINTER_* collisions.
        let mut event_names: Vec<std::sync::Arc<str>> = Vec::new();
        let mut event_code_of = |name: zyntax_typed_ast::InternedString| -> u32 {
            let resolved = name.resolve_global().unwrap_or_default();
            let needle: &str = resolved.as_ref();
            if let Some(i) = event_names.iter().position(|n| {
                let n_ref: &str = n.as_ref();
                n_ref == needle
            }) {
                return i as u32 + blinc_runtime::fsm::FSM_EVENT_CODE_OFFSET;
            }
            event_names.push(std::sync::Arc::from(needle));
            (event_names.len() - 1) as u32 + blinc_runtime::fsm::FSM_EVENT_CODE_OFFSET
        };

        let transitions: Vec<blinc_runtime::fsm::EventTransition> = local_def
            .transitions
            .iter()
            .filter_map(|t| {
                Some(blinc_runtime::fsm::EventTransition {
                    from_code: state_code(t.from)?,
                    event_code: event_code_of(t.event),
                    to_code: state_code(t.to)?,
                    actions: t.actions.clone(),
                })
            })
            .collect();

        let tick_guards: Vec<blinc_runtime::fsm::TickGuard> = local_def
            .tick_guards
            .iter()
            .filter_map(|g| {
                let symbol_intern = g.guard_fn?;
                let symbol = symbol_intern.resolve_global()?;
                Some(blinc_runtime::fsm::TickGuard {
                    from_code: state_code(g.from)?,
                    to_code: state_code(g.to)?,
                    guard_symbol: std::sync::Arc::from(symbol.as_ref()),
                })
            })
            .collect();

        let initial_code = local_def.initial.and_then(state_code).unwrap_or(0);

        let runtime_def = blinc_runtime::fsm::FsmDefinition {
            name: std::sync::Arc::from(fsm_name.as_ref()),
            initial_code,
            state_names,
            event_names,
            transitions,
            tick_guards,
        };

        blinc_runtime::fsm::with_fsm_registry_mut(|r| {
            r.register(runtime_def);
        });
    }
}

// =====================================================================
// Runtime engine
// =====================================================================

/// The Blinc DSL runtime. Owns the compiled grammar, the Zyntax runtime,
/// and the loaded module table.
pub struct BlincDsl {
    grammar: Grammar2,
    runtime: Arc<Mutex<ZyntaxRuntime>>,
    /// JIT symbols `lower_view_to_value_returning` rewrote to the
    /// `i64` widget-handle ABI; consulted to choose `call_function`
    /// vs `call::<()>` at render time.
    value_returning_views: Arc<Mutex<std::collections::HashSet<String>>>,
    /// JIT function names per source path; used by `recompile_file`.
    compiled_modules: Arc<Mutex<std::collections::HashMap<std::path::PathBuf, Vec<String>>>>,
    /// CSS from `style { … }` blocks, compile-order.
    compiled_stylesheets: Arc<Mutex<Vec<String>>>,
    /// Cursor into `compiled_stylesheets` of how far the
    /// `BlincContextState` queue flush has reached.
    stylesheets_queued_up_to: Arc<Mutex<usize>>,
    /// Every declared `signal <name>: <T>`, accumulated across compiles.
    declared_signals: Arc<Mutex<Vec<(String, Type)>>>,
    /// Every declared `fsm <Name>`, accumulated across compiles.
    declared_fsms: Arc<Mutex<Vec<String>>>,
    /// Set when any view carries `@stateful`. `view_widget` then
    /// wraps the tree in a reactive `Stateful`.
    has_stateful_view: Arc<Mutex<bool>>,
    /// Signal names listed in `@stateful([…])`. Empty = subscribe
    /// to every declared signal.
    stateful_view_deps: Arc<Mutex<Vec<String>>>,
    /// FSM names listed in `@fsm([…])`. Empty = first declared FSM.
    stateful_view_fsms: Arc<Mutex<Vec<String>>>,
}

impl BlincDsl {
    /// Build a fresh runtime with the embedded Blinc grammar and all
    /// host builtins pre-registered.
    pub fn new() -> BlincDslResult<Self> {
        // Parse grammar first so embedded-grammar bugs fail fast.
        let grammar = Grammar2::from_source(BLINC_GRAMMAR)?;

        let mut runtime = ZyntaxRuntime::new()
            .map_err(|e| BlincDslError::Compile(format!("runtime init: {e}")))?;

        // MUST register builtins BEFORE any module load so the JIT linker can resolve them.
        register_builtins(&mut runtime);

        // MUST finalize after register_function — Cranelift JIT only sees symbols after rebuild.
        runtime
            .finalize_runtime_symbols()
            .map_err(|e| BlincDslError::Compile(format!("finalize symbols: {e}")))?;

        // `ZyntaxRuntime` isn't Send+Sync; the Arc<Mutex<_>> wrapper is the production shape.
        #[allow(clippy::arc_with_non_send_sync)]
        let runtime = Arc::new(Mutex::new(runtime));

        register_blinc_layout_primitives();

        let value_returning_views = Arc::new(Mutex::new(std::collections::HashSet::new()));
        let compiled_modules = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let compiled_stylesheets = Arc::new(Mutex::new(Vec::new()));
        let stylesheets_queued_up_to = Arc::new(Mutex::new(0));
        let declared_signals = Arc::new(Mutex::new(Vec::new()));
        let declared_fsms = Arc::new(Mutex::new(Vec::new()));
        let has_stateful_view = Arc::new(Mutex::new(false));
        let stateful_view_deps = Arc::new(Mutex::new(Vec::new()));
        let stateful_view_fsms = Arc::new(Mutex::new(Vec::new()));

        Ok(Self {
            grammar,
            runtime,
            value_returning_views,
            compiled_modules,
            compiled_stylesheets,
            stylesheets_queued_up_to,
            declared_signals,
            declared_fsms,
            has_stateful_view,
            stateful_view_deps,
            stateful_view_fsms,
        })
    }

    /// Install this `BlincDsl`'s JIT guard dispatcher as the process-wide
    /// `blinc_runtime::fsm::GuardDispatcher`. Opt-in to avoid races between
    /// multiple `BlincDsl` instances in the same process. Last-write-wins.
    pub fn install_runtime_bridge(&self) {
        blinc_runtime::fsm::set_guard_dispatcher(std::sync::Arc::new(JitGuardDispatcher {
            runtime: self.runtime.clone(),
        }));
    }

    /// Register a Rust widget that implements [`ExternWidget`]. Primary Rust→DSL surface.
    pub fn register_extern_widget<W: ExternWidget>(&self) -> BlincDslResult<()> {
        self.register_extern_widget_spec(W::extern_widget_spec())
    }

    /// Lower-level than [`Self::register_extern_widget`] — register an explicit
    /// [`ExternWidgetSpec`]. MUST be called before `compile_source` for any
    /// source that uses the widget.
    pub fn register_extern_widget_spec(&self, spec: ExternWidgetSpec) -> BlincDslResult<()> {
        if spec.param_types.len() > ZRTL_MAX_PARAMS {
            return Err(BlincDslError::Compile(format!(
                "register_extern_widget({}): parameter count {} exceeds ZRTL_MAX_PARAMS ({})",
                spec.name,
                spec.param_types.len(),
                ZRTL_MAX_PARAMS
            )));
        }
        let mut params = [TypeTag::VOID; ZRTL_MAX_PARAMS];
        for (i, ty) in spec.param_types.iter().enumerate() {
            params[i] = type_to_tag(ty);
        }
        let sig = ZrtlSymbolSig {
            param_count: spec.param_types.len() as u8,
            flags: ZrtlSigFlags::NONE,
            return_type: type_to_tag(&spec.return_type),
            params,
        };

        // Leak the symbol — `register_function_typed` requires `&'static str`.
        // Bounded by widget-type count, not instance count.
        let view_symbol_static: &'static str = Box::leak(spec.view_symbol.into_boxed_str());

        {
            let mut runtime = self
                .runtime
                .lock()
                .expect("BlincDsl runtime mutex poisoned");
            // MUST finalize after register — Cranelift only sees new symbols after rebuild.
            runtime.register_function_typed(view_symbol_static, spec.extern_ptr, sig);
            runtime
                .finalize_runtime_symbols()
                .map_err(|e| BlincDslError::Compile(format!("finalize symbols: {e}")))?;
        }

        blinc_runtime::component::with_component_registry_mut(|r| {
            r.register(blinc_runtime::component::ComponentDefinition {
                name: std::sync::Arc::from(spec.name.as_str()),
                view_symbol: std::sync::Arc::from(view_symbol_static),
                props: spec.props,
            });
        });

        // Widget-handle externs are value-returning — pick the `i64`-return ABI at render time.
        self.value_returning_views
            .lock()
            .expect("value_returning_views mutex poisoned")
            .insert(view_symbol_static.to_string());

        Ok(())
    }

    /// Build a DSL-defined component as a `Box<dyn ElementBuilder>`. DSL→Rust
    /// half of the interop. Props pass through as positional Zyntax values.
    pub fn query(
        &self,
        name: &str,
        props: &[ZyntaxValue],
    ) -> BlincDslResult<Box<dyn blinc_layout::div::ElementBuilder>> {
        let view_symbol = blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name(name).map(|def| def.view_symbol.clone())
        })
        .ok_or_else(|| {
            BlincDslError::Compile(format!(
                "query({name}): no component named `{name}` is registered — compile DSL source \
                 that declares it, or call `register_extern_widget` first"
            ))
        })?;

        let is_value_returning = self
            .value_returning_views
            .lock()
            .map(|set| set.contains(view_symbol.as_ref()))
            .unwrap_or(false);
        if !is_value_returning {
            return Err(BlincDslError::Compile(format!(
                "query({name}): component's view symbol `{}` isn't value-returning — only \
                 widget-primitive-rooted views can be queried (legacy `text(...)` bodies \
                 produce Unit)",
                view_symbol
            )));
        }

        // Param-count check happens inside `call_function`. We
        // build a signature whose return is I64 (widget handle)
        // and whose params are derived from the component def's
        // prop list, which (after `publish_components_to_runtime_registry`)
        // mirrors the view method's actual ABI.
        let param_types: Vec<Type> = blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name(name)
                .map(|def| def.props.iter().map(|p| p.ty.clone()).collect())
                .unwrap_or_default()
        });

        if param_types.len() > ZRTL_MAX_PARAMS {
            return Err(BlincDslError::Compile(format!(
                "query({name}): component declares {} props, exceeds ZRTL_MAX_PARAMS ({})",
                param_types.len(),
                ZRTL_MAX_PARAMS
            )));
        }

        // Build a `NativeSignature` for `call_function`. The
        // ZRTL `Type` → native conversion mirrors `type_to_tag`
        // but lives in this caller because `call_function`
        // takes the broader `NativeType` shape, not a `TypeTag`.
        let native_params: Vec<NativeType> = param_types
            .iter()
            .map(|ty| type_to_native(ty))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|ty| {
                BlincDslError::Compile(format!(
                    "query({name}): no NativeType mapping for prop type {ty:?}"
                ))
            })?;
        let sig = NativeSignature::new(&native_params, NativeType::I64);

        let runtime = self
            .runtime
            .lock()
            .expect("BlincDsl runtime mutex poisoned");
        let result = runtime
            .call_function(view_symbol.as_ref(), props, &sig)
            .map_err(BlincDslError::from)?;
        drop(runtime);

        let ZyntaxValue::Int(handle) = result else {
            return Err(BlincDslError::Compile(format!(
                "query({name}): expected ZyntaxValue::Int(handle) from view call, got {result:?}"
            )));
        };

        // SAFETY: handle came from a registered widget-handle extern.
        let widget = unsafe { materialize_widget(handle) }.ok_or_else(|| {
            BlincDslError::Compile(format!(
                "query({name}): view returned the null handle (extern build failed)"
            ))
        })?;
        Ok(widget.into_element_builder())
    }

    /// Compile a `.blinc` source. Returns JIT function names (keyed by name
    /// for hot-reload). Runs the full post-parse pipeline: marker injection,
    /// signal/fsm resolution, component lowering, value-returning view rewrite,
    /// styling overlay collection, extern named-arg resolution, init dispatch.
    pub fn compile_source(&self, source: &str, filename: &str) -> BlincDslResult<Vec<String>> {
        let mut runtime = self
            .runtime
            .lock()
            .expect("BlincDsl runtime mutex poisoned");

        // `parse_with_signatures` runs Zyntax's `inject_builtin_externs`. We
        // populate signatures via `register_function_typed` in `register_builtins`.
        let mut typed_program = self
            .grammar
            .parse_with_signatures(source, filename, runtime.plugin_signatures())
            .map_err(|e| BlincDslError::Compile(e.to_string()))?;

        // Pre-Zyntax-212dba3 a call to an imported `<Comp>$view` lowered
        // to `Indirect(Undef)` and slid through; post-bump it surfaces a
        // clean `Lowering("Call to undefined function ...")`. Inject
        // extern decls for already-compiled imports so the entry's
        // lowering sees them as known symbols.
        self.inject_imported_view_externs(&mut typed_program, filename);

        // Snapshot signal/fsm decls BEFORE rewrite/strip passes destroy them.
        {
            let (signals, fsms) = collect_declared(&typed_program);
            self.declared_signals
                .lock()
                .expect("declared_signals mutex poisoned")
                .extend(signals);
            self.declared_fsms
                .lock()
                .expect("declared_fsms mutex poisoned")
                .extend(fsms);
        }

        // Detect and strip `@stateful` / `@fsm` markers. Accumulate explicit deps.
        {
            let (saw_stateful, explicit_deps, explicit_fsms) =
                detect_and_strip_stateful_views(&mut typed_program);
            if saw_stateful {
                *self
                    .has_stateful_view
                    .lock()
                    .expect("has_stateful_view mutex poisoned") = true;
            }
            if !explicit_deps.is_empty() {
                let mut acc = self
                    .stateful_view_deps
                    .lock()
                    .expect("stateful_view_deps mutex poisoned");
                for name in explicit_deps {
                    if !acc.contains(&name) {
                        acc.push(name);
                    }
                }
            }
            if !explicit_fsms.is_empty() {
                let mut acc = self
                    .stateful_view_fsms
                    .lock()
                    .expect("stateful_view_fsms mutex poisoned");
                for name in explicit_fsms {
                    if !acc.contains(&name) {
                        acc.push(name);
                    }
                }
            }
        }

        inject_fsm_context_markers(&mut typed_program);
        synthesize_fsm_event_enums(&mut typed_program);
        synthesize_fsm_trait_interfaces(&mut typed_program);
        lower_match_blocks(&mut typed_program);
        resolve_signal_calls(&mut typed_program);
        resolve_fsm_trigger_calls(&mut typed_program);
        resolve_fsm_subscribe_calls(&mut typed_program);

        // Extract `style { … }` blocks and queue through `BlincContextState` for next frame.
        {
            let mut sheets = self
                .compiled_stylesheets
                .lock()
                .expect("compiled_stylesheets mutex poisoned");
            let before = sheets.len();
            extract_and_strip_stylesheets(&mut typed_program, &mut sheets);
            if blinc_core::context_state::BlincContextState::is_initialized() {
                let ctx = blinc_core::context_state::BlincContextState::get();
                for css in &sheets[before..] {
                    ctx.queue_stylesheet(css.clone());
                }
            }
        }

        // MUST validate BEFORE lower_component_calls — validator reads the marker shape.
        validate_component_calls(&typed_program)
            .map_err(|errors| BlincDslError::Compile(errors.join("\n")))?;
        lower_component_calls(&mut typed_program);
        bind_component_props(&mut typed_program);

        // Module hardcoded to "main" — Zyntax compiles each source into one module.
        let module = zyntax_typed_ast::InternedString::new_global("main");
        populate_fsm_registry_pass(&mut typed_program, module);

        publish_fsms_to_runtime_registry(&typed_program);

        // MUST run after `bind_component_props` so view params reflect the prop list.
        publish_components_to_runtime_registry(&typed_program);

        // MUST run BEFORE `ensure_unit_return` so its defensive `Return(None)`
        // doesn't override the value-bearing one.
        {
            let mut vrv = self
                .value_returning_views
                .lock()
                .expect("value_returning_views mutex poisoned");
            lower_view_to_value_returning(&mut typed_program, &mut vrv);
        }

        // MUST run AFTER `lower_view_to_value_returning` and BEFORE `ensure_unit_return`.
        lower_children_arrays_to_blocks(&mut typed_program);

        // MUST run BEFORE `resolve_extern_widget_named_args` so it sees uniform `__style`.
        lower_styling_args_to_overlays(&mut typed_program);

        // Resolve named args against our component registry — Zyntax's auto-injected
        // extern decls carry synthetic `p0`, `p1`, … param names that can't bind by name.
        resolve_extern_widget_named_args(&mut typed_program);

        // Defensive `Return(None)` so the body classifier can't infer a value-bearing return.
        ensure_unit_return(&mut typed_program);

        let function_names = runtime
            .compile_typed_program(typed_program)
            .map_err(|e| BlincDslError::Compile(e.to_string()))?;

        // Export `<Component>$view` symbols so the next `compile_source`
        // (e.g. the entry in `compile_project`) can link against imports
        // that resolve to them.
        for name in &function_names {
            if name.ends_with("$view") {
                let _ = runtime.export_function(name);
            }
        }

        // Eagerly run each component's `<Component>$__init__` exactly once at compile.
        // Running at compile (not on_mount) avoids accumulating subscribers across rebuilds.
        for name in &function_names {
            if !name.ends_with("$__init__") {
                continue;
            }
            // Best-effort: a typo'd signal/FSM in `init { … }` shouldn't sink the whole compile.
            let _ = runtime.call::<()>(name, &[]);
        }

        Ok(function_names)
    }

    /// For each `import { X } from "./path"` in `program`, resolve the
    /// imported file relative to `entry_filename`'s parent, look it up
    /// in `compiled_modules`, and synthesize `extern fn <X>$view(): i64`
    /// decls so the entry program's lowering recognises imported view
    /// calls as known symbols.
    ///
    /// Today assumes zero-prop view signatures — prop-bearing imports
    /// need their param list mirrored too. Sufficient for the
    /// `compile_project` test surface.
    fn inject_imported_view_externs(&self, program: &mut TypedProgram, entry_filename: &str) {
        use zyntax_typed_ast::type_registry::{CallingConvention, Visibility};
        use zyntax_typed_ast::typed_ast::{TypedDeclaration, TypedFunction};
        use zyntax_typed_ast::{typed_node, InternedString};

        let entry_path = Path::new(entry_filename);
        let parent = entry_path.parent().unwrap_or_else(|| Path::new("."));
        let arch = zyntax_typed_ast::import_resolver::ModuleArchitecture::NodeStyle {
            extensions: vec![".blinc".to_string()],
            index_name: "index".to_string(),
        };

        let modules = match self.compiled_modules.lock() {
            Ok(m) => m.clone(),
            Err(_) => return,
        };

        let mut wanted: Vec<String> = Vec::new();
        for decl in &program.declarations {
            let TypedDeclaration::Import(import) = &decl.node else {
                continue;
            };
            let segments: Vec<String> = import
                .module_path
                .iter()
                .filter_map(|s| s.resolve_global().map(|s| s.to_string()))
                .filter(|s| !s.is_empty())
                .map(|s| s.trim_start_matches('/').to_string())
                .collect();
            if segments.is_empty() {
                continue;
            }
            let candidates = arch.module_to_paths(&segments, &parent.to_path_buf());
            let Some(imported_path) = candidates.into_iter().find(|p| p.exists()) else {
                continue;
            };
            let Some(compiled) = modules.get(&imported_path) else {
                continue;
            };
            for item in &import.items {
                let zyntax_typed_ast::TypedImportItem::Named { name, .. } = item else {
                    continue;
                };
                let Some(import_name) = name.resolve_global() else {
                    continue;
                };
                let view_sym = format!("{import_name}$view");
                if compiled.iter().any(|s| s == &view_sym) && !wanted.contains(&view_sym) {
                    wanted.push(view_sym);
                }
            }
        }

        for sym in wanted {
            // Sanity guard against double-injection if the entry
            // source already declared `<X>$view` for some reason.
            let already_declared = program.declarations.iter().any(|d| {
                if let TypedDeclaration::Function(f) = &d.node {
                    f.name.resolve_global().as_deref() == Some(sym.as_str())
                } else {
                    false
                }
            });
            if already_declared {
                continue;
            }
            let interned = InternedString::new_global(&sym);
            let func = TypedFunction {
                name: interned,
                annotations: vec![],
                effects: vec![],
                with_handlers: vec![],
                type_params: vec![],
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::I64),
                body: None,
                visibility: Visibility::Public,
                is_async: false,
                is_pure: false,
                is_external: true,
                calling_convention: CallingConvention::Default,
                link_name: Some(interned),
            };
            program.declarations.push(typed_node(
                TypedDeclaration::Function(func),
                Type::Primitive(PrimitiveType::Unit),
                Span::default(),
            ));
        }
    }

    /// Compile a `.blinc` file off disk. Records JIT names per-path for hot reload.
    pub fn compile_file(&self, path: &Path) -> BlincDslResult<Vec<String>> {
        let source = std::fs::read_to_string(path)?;
        let filename = path.to_string_lossy();
        let names = self.compile_source(&source, &filename)?;
        self.compiled_modules
            .lock()
            .expect("compiled_modules mutex poisoned")
            .insert(path.to_path_buf(), names.clone());
        Ok(names)
    }

    /// Compile every `*.blinc` file directly inside `path` (non-recursive).
    /// Names must be unique across the directory (shared global substrate registry).
    pub fn compile_directory(
        &self,
        path: &Path,
    ) -> BlincDslResult<std::collections::HashMap<std::path::PathBuf, Vec<String>>> {
        let mut out = std::collections::HashMap::new();
        let entries = std::fs::read_dir(path)?;
        // Sort by file name for deterministic compile order.
        let mut files: Vec<std::path::PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("blinc"))
            .collect();
        files.sort();
        for file in files {
            let names = self.compile_file(&file)?;
            out.insert(file, names);
        }
        Ok(out)
    }

    /// Re-run compilation for a single file. Replaces the per-
    /// path entry in the compiled-modules map; the JIT-side
    /// symbol table picks up the new function pointers via
    /// beadie's atomic `swap_compiled` (FSM / signal / component
    /// registry state survives — see `blinc_runtime::reload`).
    ///
    /// Returns the file's freshly-emitted function names.
    pub fn recompile_file(&self, path: &Path) -> BlincDslResult<Vec<String>> {
        self.compile_file(path)
    }

    /// Compile an entry `.blinc` file with cross-module
    /// resolution rooted at `source_root`. ES6-style imports
    /// (`import { X } from "./widgets"`) in the entry source
    /// pull dependent files via a registered callback resolver;
    /// Zyntax's import chain parses each, flat-merges its
    /// declarations into the entry program, and JIT-compiles
    /// the whole thing as one unit.
    ///
    /// Imports are de-duplicated per process (thread-local in
    /// Zyntax's `process_imports_for_traits`), so the same
    /// module pulled by multiple files only compiles once.
    pub fn compile_project(&self, entry: &Path, source_root: &Path) -> BlincDslResult<Vec<String>> {
        let mut aggregated: Vec<String> = Vec::new();
        self.compile_project_inner(entry, source_root, &mut aggregated)?;
        Ok(aggregated)
    }

    fn compile_project_inner(
        &self,
        entry: &Path,
        source_root: &Path,
        out: &mut Vec<String>,
    ) -> BlincDslResult<()> {
        // ES6 / Node-style resolution: tries `<root>/<segs>.blinc`
        // then `<root>/<segs>/index.blinc`. Drives the dotted
        // `["", "/widgets"]` shape Zyntax's auto-split produces
        // for `./widgets` AND plain `["widgets"]` for bare names.
        let arch = zyntax_typed_ast::import_resolver::ModuleArchitecture::NodeStyle {
            extensions: vec![".blinc".to_string()],
            index_name: "index".to_string(),
        };

        let source = std::fs::read_to_string(entry)?;
        let program = self.parse_to_typed_ast(&source, entry.to_string_lossy().as_ref())?;

        for decl in &program.declarations {
            let zyntax_typed_ast::TypedDeclaration::Import(import) = &decl.node else {
                continue;
            };
            let segments: Vec<String> = import
                .module_path
                .iter()
                .filter_map(|s| s.resolve_global().map(|s| s.to_string()))
                // Action language's `.`-split on `./widgets` → `["", "/widgets"]`.
                .filter(|s| !s.is_empty())
                .map(|s| s.trim_start_matches('/').to_string())
                .collect();
            if segments.is_empty() {
                continue;
            }
            let candidates = arch.module_to_paths(&segments, &source_root.to_path_buf());
            let Some(imported) = candidates.into_iter().find(|p| p.exists()) else {
                continue;
            };
            let already = self
                .compiled_modules
                .lock()
                .map(|m| m.contains_key(&imported))
                .unwrap_or(false);
            if already {
                continue;
            }
            self.compile_project_inner(&imported, source_root, out)?;
        }

        let names = self.compile_file(entry)?;
        out.extend(names);
        Ok(())
    }

    /// JIT function names last emitted for `path`, or `None` if never compiled.
    pub fn compiled_function_names(&self, path: &Path) -> Option<Vec<String>> {
        self.compiled_modules
            .lock()
            .ok()
            .and_then(|m| m.get(path).cloned())
    }

    /// CSS strings from every `style { ... }` block, in compile order.
    pub fn compiled_stylesheets(&self) -> Vec<String> {
        self.compiled_stylesheets
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// Parse `.blinc` source to TypedAST without compiling. Runs the same
    /// post-parse pipeline as `compile_source` so AST tests see the lowered shape.
    pub fn parse_to_typed_ast(&self, source: &str, filename: &str) -> BlincDslResult<TypedProgram> {
        let runtime = self
            .runtime
            .lock()
            .expect("BlincDsl runtime mutex poisoned");

        let mut program = self
            .grammar
            .parse_with_signatures(source, filename, runtime.plugin_signatures())
            .map_err(|e| BlincDslError::Compile(e.to_string()))?;

        // Post-parse passes — no-op on programs without the matching shapes.
        inject_fsm_context_markers(&mut program);
        synthesize_fsm_event_enums(&mut program);
        synthesize_fsm_trait_interfaces(&mut program);
        lower_match_blocks(&mut program);
        resolve_signal_calls(&mut program);
        resolve_fsm_trigger_calls(&mut program);
        resolve_fsm_subscribe_calls(&mut program);
        let _ = detect_and_strip_stateful_views(&mut program);

        validate_component_calls(&program)
            .map_err(|errors| BlincDslError::Compile(errors.join("\n")))?;

        // MUST run after validation — validator reads the marker shape.
        lower_component_calls(&mut program);

        bind_component_props(&mut program);

        // Local set; `parse_to_typed_ast` doesn't touch the JIT renderer.
        let mut local_vrv = std::collections::HashSet::new();
        lower_view_to_value_returning(&mut program, &mut local_vrv);

        lower_children_arrays_to_blocks(&mut program);
        lower_styling_args_to_overlays(&mut program);
        resolve_extern_widget_named_args(&mut program);

        Ok(program)
    }

    /// Invoke bare-form `render_view` and drain the scene buffer.
    /// For `view { ... }` programs (no enclosing `component`).
    pub fn render_view(&self) -> BlincDslResult<Vec<DslOp>> {
        self.render_named("render_view")
    }

    /// Invoke `<Name>$view` (inherent-impl mangling) and drain the scene buffer.
    pub fn render_component(&self, name: &str) -> BlincDslResult<Vec<DslOp>> {
        let symbol = format!("{name}$view");
        self.render_named(&symbol)
    }

    fn render_named(&self, fn_name: &str) -> BlincDslResult<Vec<DslOp>> {
        let is_value_returning = self
            .value_returning_views
            .lock()
            .map(|set| set.contains(fn_name))
            .unwrap_or(false);

        let runtime = self
            .runtime
            .lock()
            .expect("BlincDsl runtime mutex poisoned");

        if is_value_returning {
            // Handle discarded — substrate `ViewRenderer` flows it through to consumers.
            // Direct JIT dispatch — see [`JitGuardDispatcher::call_guard`].
            let ptr = runtime.get_function_ptr(fn_name).ok_or_else(|| {
                BlincDslError::Compile(format!("view symbol '{fn_name}' not registered in runtime"))
            })?;
            let view: extern "C" fn() -> i64 = unsafe { std::mem::transmute(ptr) };
            let _ = view();
        } else {
            runtime.call::<()>(fn_name, &[])?;
        }
        Ok(take_scene_ops())
    }

    /// Backend-agnostic view renderer backed by this `BlincDsl`'s Cranelift runtime.
    pub fn view_renderer(&self) -> std::sync::Arc<dyn blinc_runtime::view::ViewRenderer> {
        std::sync::Arc::new(JitViewRenderer {
            runtime: self.runtime.clone(),
            value_returning_views: self.value_returning_views.clone(),
        })
    }

    /// Every declared `signal <name>: <T>` across all compiled sources.
    pub fn declared_signals(&self) -> Vec<(String, Type)> {
        self.declared_signals
            .lock()
            .expect("declared_signals mutex poisoned")
            .clone()
    }

    /// Every declared `fsm <Name> { ... }` across all compiled sources.
    pub fn declared_fsms(&self) -> Vec<String> {
        self.declared_fsms
            .lock()
            .expect("declared_fsms mutex poisoned")
            .clone()
    }

    /// Materialise the compiled `view { ... }` as a top-level `ElementBuilder`.
    ///
    /// Opt-in reactivity: returns a bare widget by default. If any view carries
    /// `@stateful`, the result is wrapped in a `Stateful<FsmStateId>` that
    /// re-renders on signal/FSM changes.
    ///
    /// ```text
    /// @stateful view {
    ///     Div { Text(f"Count: {count.get()}") }
    /// }
    /// ```
    pub fn view_widget(&self) -> Box<dyn blinc_layout::div::ElementBuilder> {
        use blinc_core::reactive::SignalId;
        use blinc_runtime::fsm::FsmStateId;
        use zyntax_embed::ZyntaxValue;

        // Flush pending stylesheets into `BlincContextState` — `compile_source`
        // typically runs before the context is live. Cursor keeps it idempotent.
        if blinc_core::context_state::BlincContextState::is_initialized() {
            let sheets = self
                .compiled_stylesheets
                .lock()
                .expect("compiled_stylesheets mutex poisoned");
            let mut cursor = self
                .stylesheets_queued_up_to
                .lock()
                .expect("stylesheets_queued_up_to mutex poisoned");
            if *cursor < sheets.len() {
                let ctx = blinc_core::context_state::BlincContextState::get();
                for css in &sheets[*cursor..] {
                    ctx.queue_stylesheet(css.clone());
                }
                *cursor = sheets.len();
            }
        }

        let renderer = self.view_renderer();
        let stateful = *self
            .has_stateful_view
            .lock()
            .expect("has_stateful_view mutex poisoned");

        // No `@stateful` → render once, return bare tree.
        if !stateful {
            return materialize_view(&renderer);
        }

        let signals = self.declared_signals();
        let fsms = self.declared_fsms();

        // Empty explicit lists → bare `@stateful` / `@fsm`: use all declared / first.
        let explicit_deps = self
            .stateful_view_deps
            .lock()
            .expect("stateful_view_deps mutex poisoned")
            .clone();
        let explicit_fsms = self
            .stateful_view_fsms
            .lock()
            .expect("stateful_view_fsms mutex poisoned")
            .clone();

        // Skip signals whose type isn't bridged (only i32/f64/string today).
        let dep_pool: Vec<(String, Type)> = if explicit_deps.is_empty() {
            signals.clone()
        } else {
            explicit_deps
                .iter()
                .filter_map(|name| {
                    signals.iter().find_map(|(n, ty)| {
                        if n == name {
                            Some((n.clone(), ty.clone()))
                        } else {
                            None
                        }
                    })
                })
                .collect()
        };

        let mut signal_ids: Vec<SignalId> = Vec::new();
        for (name, ty) in &dep_pool {
            let id = match ty {
                Type::Primitive(PrimitiveType::I32) => {
                    blinc_runtime::signal::state_i32(name).map(|s| s.signal_id())
                }
                Type::Primitive(PrimitiveType::F64) => {
                    blinc_runtime::signal::state_f64(name).map(|s| s.signal_id())
                }
                Type::Primitive(PrimitiveType::String) => {
                    blinc_runtime::signal::state_str(name).map(|s| s.signal_id())
                }
                _ => None,
            };
            if let Some(id) = id {
                signal_ids.push(id);
            }
        }

        let mut builder = blinc_layout::stateful::stateful::<FsmStateId>();
        let fsm_for_binding = if let Some(name) = explicit_fsms.first() {
            // First-listed FSM wins; substrate exposes a single `SharedState` per stateful.
            Some(name.as_str())
        } else {
            fsms.first().map(|s| s.as_str())
        };
        if let Some(fsm_name) = fsm_for_binding {
            if let Some(shared) = blinc_runtime::fsm::default_state(fsm_name) {
                builder = builder.with_shared_state(shared);
            }
        }
        builder = builder.deps(signal_ids);

        let renderer_for_callback = renderer.clone();
        Box::new(builder.on_state(move |_sctx| {
            let value =
                blinc_runtime::view::render_main(&renderer_for_callback).expect("render_main");
            let ZyntaxValue::Int(handle) = value else {
                return blinc_layout::div::Div::new();
            };
            let inner = unsafe { materialize_widget(handle) }
                .map(|w| w.into_element_builder())
                .unwrap_or_else(|| Box::new(blinc_layout::div::Div::new()));
            blinc_layout::div::Div::new().child_box(inner)
        }))
    }

    /// Resolve a tick-driven transition. First-matching guard wins. Returns
    /// `None` when no guard fires.
    pub fn step_tick(
        &self,
        id: &FsmId,
        current: &str,
    ) -> BlincDslResult<Option<zyntax_typed_ast::InternedString>> {
        // Snapshot candidates and drop the registry lock before taking the runtime lock.
        let candidates: Vec<(
            zyntax_typed_ast::InternedString,
            zyntax_typed_ast::InternedString,
        )> = with_fsm_registry(|r| {
            r.get(id)
                .map(|def| {
                    def.tick_guards
                        .iter()
                        .filter(|g| g.from.resolve_global().as_deref() == Some(current))
                        .filter_map(|g| g.guard_fn.map(|fn_name| (fn_name, g.to)))
                        .collect()
                })
                .unwrap_or_default()
        });

        if candidates.is_empty() {
            return Ok(None);
        }

        let runtime = self
            .runtime
            .lock()
            .expect("BlincDsl runtime mutex poisoned");

        for (guard_fn, to) in candidates {
            let Some(name) = guard_fn.resolve_global() else {
                continue;
            };
            // Direct JIT dispatch — see [`JitGuardDispatcher::call_guard`]
            // for why we transmute the pointer rather than going through
            // `call_function` or `call_raw`.
            let Some(ptr) = runtime.get_function_ptr(&name) else {
                continue;
            };
            let guard: extern "C" fn() -> i32 = unsafe { std::mem::transmute(ptr) };
            if guard() != 0 {
                return Ok(Some(to));
            }
        }

        Ok(None)
    }

    /// Set an i32-typed signal in the per-thread signal table.
    pub fn set_signal_i32(&self, name: &str, value: i32) {
        blinc_runtime::signal::set_i32(name, value);
    }

    /// Read an i32-typed signal. `None` if unset (distinct from `Some(0)`).
    pub fn get_signal_i32(&self, name: &str) -> Option<i32> {
        blinc_runtime::signal::get_i32(name)
    }

    /// Set an f64-typed signal.
    pub fn set_signal_f64(&self, name: &str, value: f64) {
        blinc_runtime::signal::set_f64(name, value);
    }

    /// Read an f64-typed signal. `None` if unset.
    pub fn get_signal_f64(&self, name: &str) -> Option<f64> {
        blinc_runtime::signal::get_f64(name)
    }

    /// Set a string-typed signal.
    pub fn set_signal_string(&self, name: &str, value: impl Into<String>) {
        blinc_runtime::signal::set_str(name, value);
    }

    /// Read a string-typed signal.
    pub fn get_signal_string(&self, name: &str) -> Option<String> {
        blinc_runtime::signal::get_str(name)
    }
}

/// Pairs a DSL-visible symbol name with an `extern "C"` fn pointer and signature.
/// Used for runtime registration AND type-system injection (spliced as an extern
/// fn decl into each parsed `TypedProgram` before `compile_typed_program`).
struct BuiltinDescriptor {
    /// Mangled symbol the grammar lowers to (no `@builtin` alias indirection).
    name: &'static str,
    param_types: &'static [Type],
    return_type: Type,
    /// `extern "C"` fn cast to `*const u8` for `register_function`.
    ptr: *const u8,
}

// SAFETY: Only fn pointers and `'static` references inside.
unsafe impl Sync for BuiltinDescriptor {}

/// All host builtins. Ordering irrelevant — registration walks the full table.
fn builtins() -> Vec<BuiltinDescriptor> {
    vec![
        BuiltinDescriptor {
            name: "$Blinc$text",
            param_types: &[Type::Primitive(PrimitiveType::String)],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_text as *const u8,
        },
        BuiltinDescriptor {
            name: "$Blinc$text_int",
            param_types: &[Type::Primitive(PrimitiveType::I32)],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_text_int as *const u8,
        },
        BuiltinDescriptor {
            // `<name>.get()` lowered by `resolve_signal_calls`.
            name: "__signal_get_i32",
            param_types: &[Type::Primitive(PrimitiveType::String)],
            return_type: Type::Primitive(PrimitiveType::I32),
            ptr: blinc_signal_get_i32 as *const u8,
        },
        BuiltinDescriptor {
            name: "__signal_get_f64",
            param_types: &[Type::Primitive(PrimitiveType::String)],
            return_type: Type::Primitive(PrimitiveType::F64),
            ptr: blinc_signal_get_f64 as *const u8,
        },
        BuiltinDescriptor {
            name: "__signal_get_string",
            param_types: &[Type::Primitive(PrimitiveType::String)],
            return_type: Type::Primitive(PrimitiveType::String),
            ptr: blinc_signal_get_string as *const u8,
        },
        BuiltinDescriptor {
            // `<sig> = <expr>` inside a function / closure body
            // lowers via `resolve_signal_calls` to a call here
            // with the LHS's interned name and the (already
            // signal-rewritten) RHS value.
            name: "__signal_set_i32",
            param_types: &[
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::I32),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_signal_set_i32 as *const u8,
        },
        BuiltinDescriptor {
            name: "__signal_set_f64",
            param_types: &[
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::F64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_signal_set_f64 as *const u8,
        },
        BuiltinDescriptor {
            name: "__signal_set_string",
            param_types: &[
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::String),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_signal_set_string as *const u8,
        },
        BuiltinDescriptor {
            // `<FsmName>.trigger("State.Event")` lowered by `resolve_fsm_trigger_calls`.
            name: "__fsm_runtime_trigger__",
            param_types: &[
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::String),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_fsm_runtime_trigger as *const u8,
        },
        BuiltinDescriptor {
            // `<FsmName>.subscribe("From.Event", closure)` — third arg is the
            // closure's raw fn ptr smuggled as i64.
            name: "__fsm_subscribe__",
            param_types: &[
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_fsm_subscribe as *const u8,
        },
        BuiltinDescriptor {
            // `__fstring_format__` (i32 only — f64 needs a separate `__fstring_format_f64__`).
            name: "$Blinc$format_int",
            param_types: &[Type::Primitive(PrimitiveType::I32)],
            return_type: Type::Primitive(PrimitiveType::String),
            ptr: blinc_format_int as *const u8,
        },
        BuiltinDescriptor {
            // `string_concat` — chains f-string parts.
            name: "$Blinc$string_concat",
            param_types: &[
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::String),
            ],
            return_type: Type::Primitive(PrimitiveType::String),
            ptr: blinc_string_concat as *const u8,
        },
        BuiltinDescriptor {
            // `Text("hi")` → leaked `WidgetBox::Text(...)` as i64.
            name: "$Blinc$Text$view",
            param_types: &[Type::Primitive(PrimitiveType::String)],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_text_view as *const u8,
        },
        BuiltinDescriptor {
            // `Div(children, style, class, on_click)`. `class` = whitespace-sep names,
            // `on_click` = raw fn ptr as i64 (0 = none).
            name: "$Blinc$Div$view",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_div_view as *const u8,
        },
        BuiltinDescriptor {
            // `__new_child_list__()` — mint `Vec<WidgetHandle>`, populated by `__push_child__`.
            name: "__new_child_list__",
            param_types: &[],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_new_child_list as *const u8,
        },
        BuiltinDescriptor {
            // `__push_child__(list, child)` — append. List pointer stays live for container.
            name: "__push_child__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_push_child as *const u8,
        },
        // Style-overlay builders — mirror the child-list pattern.
        BuiltinDescriptor {
            name: "__new_style_overlay__",
            param_types: &[],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_new_style_overlay as *const u8,
        },
        BuiltinDescriptor {
            name: "__set_overlay_bg__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_set_overlay_bg as *const u8,
        },
        BuiltinDescriptor {
            name: "__set_overlay_opacity__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::F64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_set_overlay_opacity as *const u8,
        },
        BuiltinDescriptor {
            name: "__set_overlay_corner_radius__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::F64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_set_overlay_corner_radius as *const u8,
        },
        BuiltinDescriptor {
            name: "__set_overlay_border_width__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::F64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_set_overlay_border_width as *const u8,
        },
        BuiltinDescriptor {
            name: "__set_overlay_border_color__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_set_overlay_border_color as *const u8,
        },
    ]
}

/// Map a typed-AST `Type` to the wire-format `TypeTag` for `ZrtlSymbolSig`.
fn type_to_tag(ty: &Type) -> TypeTag {
    match ty {
        Type::Primitive(PrimitiveType::Unit) => TypeTag::VOID,
        Type::Primitive(PrimitiveType::String) => TypeTag::STRING,
        Type::Primitive(PrimitiveType::I32) => TypeTag::I32,
        Type::Primitive(PrimitiveType::I64) => TypeTag::I64,
        Type::Primitive(PrimitiveType::F64) => TypeTag::F64,
        // Panic loudly — silent VOID would break codegen.
        _ => panic!(
            "blinc_dsl_core: no TypeTag mapping for {ty:?} \
             — extend `type_to_tag` when adding new builtin types"
        ),
    }
}

/// Map a typed-AST `Type` to the runtime `NativeType` for `call_function`.
/// Strings cross the FFI as `NativeType::Ptr` (length-prefixed buffer).
fn type_to_native(ty: &Type) -> Result<NativeType, &Type> {
    match ty {
        Type::Primitive(PrimitiveType::Unit) => Ok(NativeType::Void),
        Type::Primitive(PrimitiveType::String) => Ok(NativeType::Ptr),
        Type::Primitive(PrimitiveType::I32) => Ok(NativeType::I32),
        Type::Primitive(PrimitiveType::I64) => Ok(NativeType::I64),
        Type::Primitive(PrimitiveType::F64) => Ok(NativeType::F64),
        other => Err(other),
    }
}

/// Build the ZRTL signature for a builtin (stored in `backend.symbol_signatures`).
fn descriptor_to_sig(b: &BuiltinDescriptor) -> ZrtlSymbolSig {
    assert!(
        b.param_types.len() <= ZRTL_MAX_PARAMS,
        "{}: parameter count {} exceeds ZRTL_MAX_PARAMS ({})",
        b.name,
        b.param_types.len(),
        ZRTL_MAX_PARAMS
    );

    let mut params = [TypeTag::VOID; ZRTL_MAX_PARAMS];
    for (i, ty) in b.param_types.iter().enumerate() {
        params[i] = type_to_tag(ty);
    }

    ZrtlSymbolSig {
        param_count: b.param_types.len() as u8,
        flags: ZrtlSigFlags::NONE,
        return_type: type_to_tag(&b.return_type),
        params,
    }
}

/// Register all `$Blinc$*` builtins on the runtime with full signatures.
fn register_builtins(runtime: &mut ZyntaxRuntime) {
    for b in builtins() {
        let sig = descriptor_to_sig(&b);
        runtime.register_function_typed(b.name, b.ptr, sig);
    }
}

/// Rewrite view fns to value-returning (`I64` widget handle) when their body
/// ends in a `$Blinc$<X>$view` call. MUST run before [`ensure_unit_return`].
/// Pinning return to a concrete `I64` (not `Any`) keeps Zyntax's body
/// classifier on the well-trodden specialised-call path.
fn lower_view_to_value_returning(
    program: &mut TypedProgram,
    value_returning_symbols: &mut std::collections::HashSet<String>,
) {
    use zyntax_typed_ast::{TypedDeclaration, TypedExpression};

    fn is_view_name(name: zyntax_typed_ast::InternedString) -> bool {
        matches!(
            name.resolve_global().as_deref(),
            Some("render_view") | Some("view")
        )
    }

    /// Match `Expression(Call(Variable("<X>$view"), ...))` where `<X>` is a
    /// substrate primitive or a user component whose own view is value-returning.
    fn is_primitive_view_call_stmt(
        stmt: &TypedStatement,
        value_returning_symbols: &std::collections::HashSet<String>,
    ) -> bool {
        let TypedStatement::Expression(expr) = stmt else {
            return false;
        };
        let TypedExpression::Call(call) = &expr.node else {
            return false;
        };
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return false;
        };
        let Some(name) = callee.resolve_global() else {
            return false;
        };
        let s: &str = name.as_ref();
        // Substrate primitives are always i64-returning.
        if s.starts_with("$Blinc$") && s.ends_with("$view") {
            return true;
        }
        // User components: only if promoted by an earlier pass.
        value_returning_symbols.contains(s)
    }

    /// Rewrite trailing primitive-call to `Return(Some(call))` and return whether converted.
    fn try_convert_trailing(
        body: &mut zyntax_typed_ast::typed_ast::TypedBlock,
        value_returning_symbols: &std::collections::HashSet<String>,
    ) -> bool {
        let Some(last) = body.statements.last() else {
            return false;
        };
        if !is_primitive_view_call_stmt(&last.node, value_returning_symbols) {
            return false;
        }
        let last = body
            .statements
            .last_mut()
            .expect("just confirmed last exists above");
        let placeholder = TypedStatement::Continue;
        let original = std::mem::replace(&mut last.node, placeholder);
        let TypedStatement::Expression(expr) = original else {
            unreachable!("just confirmed Expression shape above");
        };
        last.node = TypedStatement::Return(Some(expr));
        true
    }

    let widget_handle_type = Type::Primitive(PrimitiveType::I64);

    // Pass 1: impl `view` methods. Pass 2: free-standing view fns referencing them.
    for decl in program.declarations.iter_mut() {
        let TypedDeclaration::Impl(imp) = &mut decl.node else {
            continue;
        };
        // `<TypeName>$<method>` mangling — pull the type name once per impl.
        let type_name: Option<String> = match &imp.for_type {
            Type::Unresolved(name) => name.resolve_global().map(|s| s.to_string()),
            Type::Named { id, .. } => program
                .type_registry
                .get_type_by_id(*id)
                .and_then(|t| t.name.resolve_global())
                .map(|s| s.to_string()),
            _ => None,
        };
        for method in &mut imp.methods {
            if !is_view_name(method.name) {
                continue;
            }
            let Some(body) = method.body.as_mut() else {
                continue;
            };
            if try_convert_trailing(body, value_returning_symbols) {
                method.return_type = widget_handle_type.clone();
                if let (Some(t), Some(m)) = (type_name.as_ref(), method.name.resolve_global()) {
                    value_returning_symbols.insert(format!("{t}${m}"));
                }
            }
        }
    }

    for decl in program.declarations.iter_mut() {
        let TypedDeclaration::Function(func) = &mut decl.node else {
            continue;
        };
        if func.is_external {
            continue;
        }
        if !is_view_name(func.name) {
            continue;
        }
        let Some(body) = func.body.as_mut() else {
            continue;
        };
        if try_convert_trailing(body, value_returning_symbols) {
            func.return_type = widget_handle_type.clone();
            if let Some(name) = func.name.resolve_global() {
                value_returning_symbols.insert(name.to_string());
            }
        }
    }
}

/// Expand substrate primitive calls carrying `children = Array([...])` (and slot
/// arrays) into Block expansions backed by `__new_child_list__` / `__push_child__`.
/// Post-order recursive. MUST run after `lower_view_to_value_returning` and
/// before `ensure_unit_return`.
fn lower_children_arrays_to_blocks(program: &mut TypedProgram) {
    use zyntax_typed_ast::{TypedCall, TypedDeclaration, TypedExpression, TypedNamedArg};

    /// Counter for unique `__blinc_children_<N>` idents.
    fn next_id(counter: &mut u32) -> u32 {
        let id = *counter;
        *counter += 1;
        id
    }

    /// Walk a statement and rewrite any nested primitive calls.
    fn walk_stmt(stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>, counter: &mut u32) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, counter),
            TypedStatement::Return(Some(e)) => rewrite_expr(e, counter),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, counter);
                }
            }
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, counter);
                for s in &mut if_stmt.then_block.statements {
                    walk_stmt(s, counter);
                }
                if let Some(else_block) = &mut if_stmt.else_block {
                    for s in &mut else_block.statements {
                        walk_stmt(s, counter);
                    }
                }
            }
            _ => {}
        }
    }

    /// Post-order — recurse before rewriting `expr`.
    fn rewrite_expr(expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>, counter: &mut u32) {
        match &mut expr.node {
            TypedExpression::Call(call) => {
                rewrite_expr(&mut call.callee, counter);
                for arg in &mut call.positional_args {
                    rewrite_expr(arg, counter);
                }
                for named in &mut call.named_args {
                    rewrite_expr(&mut named.value, counter);
                }
            }
            TypedExpression::Array(items) => {
                for item in items {
                    rewrite_expr(item, counter);
                }
            }
            TypedExpression::Block(block) => {
                for stmt in &mut block.statements {
                    walk_stmt(stmt, counter);
                }
            }
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, counter);
                rewrite_expr(&mut b.right, counter);
            }
            _ => {}
        }

        // For primitives with child-slot props (`children`, `slot_<Name>`), gather each
        // Array into a `__new_child_list__` Block. The final call carries the lists as
        // named args; `resolve_extern_widget_named_args` later positionalises them.
        let span = expr.span;
        let i64_ty = Type::Primitive(PrimitiveType::I64);
        let unit_ty = Type::Primitive(PrimitiveType::Unit);

        let TypedExpression::Call(call) = &mut expr.node else {
            return;
        };
        let Some(slot_prop_names) = callee_slot_prop_names(call) else {
            return;
        };

        let mut prelude: Vec<zyntax_typed_ast::TypedNode<TypedStatement>> = Vec::new();
        let mut had_real_slot = false;

        for slot_name in &slot_prop_names {
            let na_idx = call
                .named_args
                .iter()
                .position(|na| na.name.resolve_global().as_deref() == Some(slot_name.as_str()));
            let Some(idx) = na_idx else {
                // Slot not supplied — inject a `0` literal so
                // the registry-driven resolution finds something
                // at this slot's named position.
                call.named_args.push(TypedNamedArg {
                    name: zyntax_typed_ast::InternedString::new_global(slot_name),
                    value: Box::new(typed_node(
                        TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(0)),
                        i64_ty.clone(),
                        span,
                    )),
                    span,
                });
                continue;
            };
            let mut na = call.named_args.remove(idx);
            let TypedExpression::Array(child_exprs) = std::mem::replace(
                &mut na.value.node,
                TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(0)),
            ) else {
                // Already a non-Array value (e.g., user passed
                // a raw i64 list pointer). Leave it alone.
                call.named_args.push(na);
                continue;
            };
            had_real_slot = true;

            let id = next_id(counter);
            let list_ident =
                zyntax_typed_ast::InternedString::new_global(&format!("__blinc_children_{id}"));

            // let __blinc_children_<id> = __new_child_list__()
            prelude.push(typed_node(
                TypedStatement::Let(zyntax_typed_ast::typed_ast::TypedLet {
                    name: list_ident,
                    ty: i64_ty.clone(),
                    mutability: zyntax_typed_ast::Mutability::Immutable,
                    initializer: Some(Box::new(typed_node(
                        TypedExpression::Call(TypedCall {
                            callee: Box::new(typed_node(
                                TypedExpression::Variable(
                                    zyntax_typed_ast::InternedString::new_global(
                                        "__new_child_list__",
                                    ),
                                ),
                                Type::Any,
                                span,
                            )),
                            positional_args: vec![],
                            named_args: vec![],
                            type_args: vec![],
                        }),
                        i64_ty.clone(),
                        span,
                    ))),
                    span,
                }),
                unit_ty.clone(),
                span,
            ));

            // for each child: __push_child__(__list, child)
            for child_expr in child_exprs {
                let push_call = TypedExpression::Call(TypedCall {
                    callee: Box::new(typed_node(
                        TypedExpression::Variable(zyntax_typed_ast::InternedString::new_global(
                            "__push_child__",
                        )),
                        Type::Any,
                        span,
                    )),
                    positional_args: vec![
                        typed_node(TypedExpression::Variable(list_ident), i64_ty.clone(), span),
                        child_expr,
                    ],
                    named_args: vec![],
                    type_args: vec![],
                });
                prelude.push(typed_node(
                    TypedStatement::Expression(Box::new(typed_node(
                        push_call,
                        unit_ty.clone(),
                        span,
                    ))),
                    unit_ty.clone(),
                    span,
                ));
            }

            // Re-attach the slot as a named arg pointing at the ident.
            call.named_args.push(TypedNamedArg {
                name: zyntax_typed_ast::InternedString::new_global(slot_name),
                value: Box::new(typed_node(
                    TypedExpression::Variable(list_ident),
                    i64_ty.clone(),
                    span,
                )),
                span,
            });
        }

        if !had_real_slot {
            // No body-supplied slots — `0`-literal fills already on call.
            return;
        }

        // Wrap call in a trailing-expression Block.
        let final_call = TypedExpression::Call(TypedCall {
            callee: call.callee.clone(),
            positional_args: std::mem::take(&mut call.positional_args),
            named_args: std::mem::take(&mut call.named_args),
            type_args: std::mem::take(&mut call.type_args),
        });
        prelude.push(typed_node(
            TypedStatement::Expression(Box::new(typed_node(final_call, i64_ty.clone(), span))),
            i64_ty.clone(),
            span,
        ));

        expr.node = TypedExpression::Block(zyntax_typed_ast::typed_ast::TypedBlock {
            statements: prelude,
            span,
        });
    }

    /// Return child-slot prop names (`children`, `slot_<Name>`) in registry order.
    /// `None` for leaf primitives or non-primitives.
    fn callee_slot_prop_names(call: &TypedCall) -> Option<Vec<String>> {
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return None;
        };
        let sym = callee.resolve_global()?;
        let sym: &str = &sym;
        let name = sym
            .strip_prefix("$Blinc$")
            .and_then(|s| s.strip_suffix("$view"))?;
        let slots: Vec<String> = blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name(name)
                .map(|def| {
                    def.props
                        .iter()
                        .filter_map(|p| {
                            let n = p.name.as_ref();
                            (n == "children" || n.starts_with("slot_")).then(|| n.to_string())
                        })
                        .collect()
                })
                .unwrap_or_default()
        });
        if slots.is_empty() {
            None
        } else {
            Some(slots)
        }
    }

    /// Legacy — unused after refactor.
    #[allow(dead_code)]
    fn callee_takes_children(call: &TypedCall) -> bool {
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return false;
        };
        let Some(sym) = callee.resolve_global() else {
            return false;
        };
        let sym: &str = &sym;
        let Some(name) = sym
            .strip_prefix("$Blinc$")
            .and_then(|s| s.strip_suffix("$view"))
        else {
            return false;
        };
        blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name(name)
                .map(|def| def.props.iter().any(|p| p.name.as_ref() == "children"))
                .unwrap_or(false)
        })
    }

    let mut counter: u32 = 0;
    for decl in program.declarations.iter_mut() {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = func.body.as_mut() {
                    for stmt in &mut body.statements {
                        walk_stmt(stmt, &mut counter);
                    }
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = method.body.as_mut() {
                        for stmt in &mut body.statements {
                            walk_stmt(stmt, &mut counter);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Inline styling props recognised on DSL primitive call sites. Each maps to
/// an overlay-setter extern (`__set_overlay_*__`).
const STYLING_PROP_NAMES: &[(&str, &str, StylingValueKind)] = &[
    ("bg", "__set_overlay_bg__", StylingValueKind::IntColor),
    (
        "opacity",
        "__set_overlay_opacity__",
        StylingValueKind::Float,
    ),
    (
        "corner_radius",
        "__set_overlay_corner_radius__",
        StylingValueKind::Float,
    ),
    (
        "border_width",
        "__set_overlay_border_width__",
        StylingValueKind::Float,
    ),
    (
        "border_color",
        "__set_overlay_border_color__",
        StylingValueKind::IntColor,
    ),
];

#[derive(Clone, Copy)]
enum StylingValueKind {
    IntColor,
    Float,
}

/// Gather inline styling args (`bg`, `opacity`, …) into a `__new_style_overlay__`
/// Block and attach overlay pointer as `__style` named arg. MUST run after
/// `lower_children_arrays_to_blocks` and before `resolve_extern_widget_named_args`.
fn lower_styling_args_to_overlays(program: &mut TypedProgram) {
    use zyntax_typed_ast::{TypedCall, TypedDeclaration, TypedExpression, TypedNamedArg};

    fn callee_is_styled_primitive(call: &TypedCall) -> bool {
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return false;
        };
        let Some(sym) = callee.resolve_global() else {
            return false;
        };
        let sym: &str = &sym;
        let Some(name) = sym
            .strip_prefix("$Blinc$")
            .and_then(|s| s.strip_suffix("$view"))
        else {
            return false;
        };
        blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name(name)
                .map(|def| def.props.iter().any(|p| p.name.as_ref() == "__style"))
                .unwrap_or(false)
        })
    }

    fn walk_stmt(stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>, counter: &mut u32) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e, counter),
            TypedStatement::Return(Some(e)) => rewrite_expr(e, counter),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init, counter);
                }
            }
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition, counter);
                for s in &mut if_stmt.then_block.statements {
                    walk_stmt(s, counter);
                }
                if let Some(else_block) = &mut if_stmt.else_block {
                    for s in &mut else_block.statements {
                        walk_stmt(s, counter);
                    }
                }
            }
            _ => {}
        }
    }

    fn rewrite_expr(expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>, counter: &mut u32) {
        match &mut expr.node {
            TypedExpression::Call(call) => {
                rewrite_expr(&mut call.callee, counter);
                for arg in &mut call.positional_args {
                    rewrite_expr(arg, counter);
                }
                for na in &mut call.named_args {
                    rewrite_expr(&mut na.value, counter);
                }
            }
            TypedExpression::Array(items) => {
                for item in items {
                    rewrite_expr(item, counter);
                }
            }
            TypedExpression::Block(block) => {
                for stmt in &mut block.statements {
                    walk_stmt(stmt, counter);
                }
            }
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left, counter);
                rewrite_expr(&mut b.right, counter);
            }
            _ => {}
        }

        let TypedExpression::Call(call) = &mut expr.node else {
            return;
        };
        if !callee_is_styled_primitive(call) {
            return;
        }

        // Partition named args into styling args (consumed by
        // overlay setters) vs other args (left in place).
        let mut styling_args: Vec<(&'static str, TypedNamedArg)> = Vec::new();
        let mut remaining_named: Vec<TypedNamedArg> = Vec::new();
        let existing_named = std::mem::take(&mut call.named_args);
        for na in existing_named {
            let resolved = na.name.resolve_global();
            let name_str: Option<&str> = resolved.as_deref();
            if let Some(name) = name_str {
                if let Some(entry) = STYLING_PROP_NAMES.iter().find(|(n, _, _)| *n == name) {
                    styling_args.push((entry.1, na));
                    continue;
                }
            }
            remaining_named.push(na);
        }

        let span = expr.span;
        let i64_ty = Type::Primitive(PrimitiveType::I64);
        let unit_ty = Type::Primitive(PrimitiveType::Unit);

        if styling_args.is_empty() {
            // Restore other named args and inject a null overlay
            // pointer so the call's `__style` slot is filled.
            call.named_args = remaining_named;
            call.named_args.push(TypedNamedArg {
                name: zyntax_typed_ast::InternedString::new_global("__style"),
                value: Box::new(typed_node(
                    TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(0)),
                    i64_ty.clone(),
                    span,
                )),
                span,
            });
            return;
        }

        // Allocate a unique ident for the overlay let-binding.
        let id = {
            let i = *counter;
            *counter += 1;
            i
        };
        let overlay_ident =
            zyntax_typed_ast::InternedString::new_global(&format!("__blinc_style_{id}"));

        let mut stmts: Vec<zyntax_typed_ast::TypedNode<TypedStatement>> = Vec::new();

        // let __blinc_style_N = __new_style_overlay__()
        stmts.push(typed_node(
            TypedStatement::Let(zyntax_typed_ast::typed_ast::TypedLet {
                name: overlay_ident,
                ty: i64_ty.clone(),
                mutability: zyntax_typed_ast::Mutability::Immutable,
                initializer: Some(Box::new(typed_node(
                    TypedExpression::Call(TypedCall {
                        callee: Box::new(typed_node(
                            TypedExpression::Variable(
                                zyntax_typed_ast::InternedString::new_global(
                                    "__new_style_overlay__",
                                ),
                            ),
                            Type::Any,
                            span,
                        )),
                        positional_args: vec![],
                        named_args: vec![],
                        type_args: vec![],
                    }),
                    i64_ty.clone(),
                    span,
                ))),
                span,
            }),
            unit_ty.clone(),
            span,
        ));

        // One setter call per styling arg.
        for (setter_name, na) in styling_args {
            let setter_call = TypedExpression::Call(TypedCall {
                callee: Box::new(typed_node(
                    TypedExpression::Variable(zyntax_typed_ast::InternedString::new_global(
                        setter_name,
                    )),
                    Type::Any,
                    span,
                )),
                positional_args: vec![
                    typed_node(
                        TypedExpression::Variable(overlay_ident),
                        i64_ty.clone(),
                        span,
                    ),
                    *na.value,
                ],
                named_args: vec![],
                type_args: vec![],
            });
            stmts.push(typed_node(
                TypedStatement::Expression(Box::new(typed_node(
                    setter_call,
                    unit_ty.clone(),
                    span,
                ))),
                unit_ty.clone(),
                span,
            ));
        }

        // Trailing call: keep the original shape but attach
        // `__style = Var(__blinc_style_N)` so the named-args
        // resolution pass routes it to the right slot.
        call.named_args = remaining_named;
        call.named_args.push(TypedNamedArg {
            name: zyntax_typed_ast::InternedString::new_global("__style"),
            value: Box::new(typed_node(
                TypedExpression::Variable(overlay_ident),
                i64_ty.clone(),
                span,
            )),
            span,
        });

        // The Call expression itself is what closes the Block;
        // we extract a clone of the (now-modified) call to push
        // as the trailing Expression statement, then replace
        // `expr` with the Block.
        let final_call = TypedExpression::Call(TypedCall {
            callee: call.callee.clone(),
            positional_args: std::mem::take(&mut call.positional_args),
            named_args: std::mem::take(&mut call.named_args),
            type_args: std::mem::take(&mut call.type_args),
        });
        stmts.push(typed_node(
            TypedStatement::Expression(Box::new(typed_node(final_call, i64_ty.clone(), span))),
            i64_ty.clone(),
            span,
        ));

        expr.node = TypedExpression::Block(zyntax_typed_ast::typed_ast::TypedBlock {
            statements: stmts,
            span,
        });
    }

    let mut counter: u32 = 0;
    for decl in program.declarations.iter_mut() {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = func.body.as_mut() {
                    for stmt in &mut body.statements {
                        walk_stmt(stmt, &mut counter);
                    }
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = method.body.as_mut() {
                        for stmt in &mut body.statements {
                            walk_stmt(stmt, &mut counter);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Positionalise named args on `$Blinc$<X>$view` calls using the substrate
/// `ComponentRegistry` prop order. Zyntax's auto-injected extern decls carry
/// synthetic param names (`p0`, `p1`, …) that can't bind by name. Skipped
/// for user-declared components (handled by `bind_component_props`).
fn resolve_extern_widget_named_args(program: &mut TypedProgram) {
    use zyntax_typed_ast::{TypedCall, TypedDeclaration, TypedExpression};

    fn walk_stmt(stmt: &mut zyntax_typed_ast::TypedNode<TypedStatement>) {
        match &mut stmt.node {
            TypedStatement::Expression(e) => rewrite_expr(e),
            TypedStatement::Return(Some(e)) => rewrite_expr(e),
            TypedStatement::Let(l) => {
                if let Some(init) = &mut l.initializer {
                    rewrite_expr(init);
                }
            }
            TypedStatement::If(if_stmt) => {
                rewrite_expr(&mut if_stmt.condition);
                for s in &mut if_stmt.then_block.statements {
                    walk_stmt(s);
                }
                if let Some(else_block) = &mut if_stmt.else_block {
                    for s in &mut else_block.statements {
                        walk_stmt(s);
                    }
                }
            }
            _ => {}
        }
    }

    fn rewrite_expr(expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>) {
        // Recurse first — nested calls resolved before the outer.
        match &mut expr.node {
            TypedExpression::Call(call) => {
                rewrite_expr(&mut call.callee);
                for arg in &mut call.positional_args {
                    rewrite_expr(arg);
                }
                for na in &mut call.named_args {
                    rewrite_expr(&mut na.value);
                }
            }
            TypedExpression::Array(items) => {
                for item in items {
                    rewrite_expr(item);
                }
            }
            TypedExpression::Block(block) => {
                for stmt in &mut block.statements {
                    walk_stmt(stmt);
                }
            }
            TypedExpression::Binary(b) => {
                rewrite_expr(&mut b.left);
                rewrite_expr(&mut b.right);
            }
            _ => {}
        }

        let span = expr.span;
        let TypedExpression::Call(call) = &mut expr.node else {
            return;
        };
        let Some(props) = primitive_callee_props(call) else {
            return;
        };
        // Don't early-out on empty named_args — trailing slots still need defaults
        // so call arity matches the extern signature.

        let mut slots: Vec<Option<zyntax_typed_ast::TypedNode<TypedExpression>>> =
            (0..props.len()).map(|_| None).collect();
        let existing_positional = std::mem::take(&mut call.positional_args);
        let mut overflow: Vec<zyntax_typed_ast::TypedNode<TypedExpression>> = Vec::new();
        for (i, arg) in existing_positional.into_iter().enumerate() {
            if i < slots.len() {
                slots[i] = Some(arg);
            } else {
                overflow.push(arg);
            }
        }

        let existing_named = std::mem::take(&mut call.named_args);
        let mut unresolved_named: Vec<zyntax_typed_ast::TypedNamedArg> = Vec::new();
        for na in existing_named {
            let Some(name) = na.name.resolve_global() else {
                unresolved_named.push(na);
                continue;
            };
            let name_str: &str = &name;
            if let Some(pos) = props.iter().position(|(n, _)| n == name_str) {
                if slots[pos].is_some() {
                    unresolved_named.push(na);
                } else {
                    slots[pos] = Some(*na.value);
                }
            } else {
                unresolved_named.push(na);
            }
        }

        // Fill unfilled slots with type-appropriate defaults so call arity matches.
        let mut new_positional: Vec<zyntax_typed_ast::TypedNode<TypedExpression>> =
            Vec::with_capacity(slots.len());
        for (slot, (_, ty)) in slots.into_iter().zip(props.iter()) {
            if let Some(arg) = slot {
                new_positional.push(arg);
            } else {
                new_positional.push(default_literal_for(ty, span));
            }
        }
        new_positional.extend(overflow);

        call.positional_args = new_positional;
        call.named_args = unresolved_named;
    }

    /// Substrate primitive's prop (name, type) pairs in declaration order.
    fn primitive_callee_props(call: &TypedCall) -> Option<Vec<(String, Type)>> {
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return None;
        };
        let sym = callee.resolve_global()?;
        let sym: &str = &sym;
        let name = sym
            .strip_prefix("$Blinc$")
            .and_then(|s| s.strip_suffix("$view"))?;
        blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name(name).map(|def| {
                def.props
                    .iter()
                    .map(|p| (p.name.to_string(), p.ty.clone()))
                    .collect()
            })
        })
    }

    /// Default literal for an unsupplied prop slot (`0` / `0.0` / `""`).
    fn default_literal_for(
        ty: &Type,
        span: zyntax_typed_ast::Span,
    ) -> zyntax_typed_ast::TypedNode<TypedExpression> {
        match ty {
            Type::Primitive(PrimitiveType::F64) => typed_node(
                TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Float(0.0)),
                ty.clone(),
                span,
            ),
            Type::Primitive(PrimitiveType::String) => typed_node(
                TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::String(
                    zyntax_typed_ast::InternedString::new_global(""),
                )),
                ty.clone(),
                span,
            ),
            _ => typed_node(
                TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(0)),
                ty.clone(),
                span,
            ),
        }
    }

    for decl in program.declarations.iter_mut() {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if let Some(body) = func.body.as_mut() {
                    for stmt in &mut body.statements {
                        walk_stmt(stmt);
                    }
                }
            }
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = method.body.as_mut() {
                        for stmt in &mut body.statements {
                            walk_stmt(stmt);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Append `Return(None)` to user fns so the body classifier can't promote a
/// trailing Expression into a value-bearing return.
fn ensure_unit_return(program: &mut TypedProgram) {
    use zyntax_typed_ast::TypedDeclaration;

    fn add_trailing_return_if_missing(body: &mut zyntax_typed_ast::typed_ast::TypedBlock) {
        let trailing_is_return = matches!(
            body.statements.last().map(|s| &s.node),
            Some(TypedStatement::Return(_))
        );
        if !trailing_is_return {
            body.statements.push(typed_node(
                TypedStatement::Return(None),
                Type::Primitive(PrimitiveType::Unit),
                Span::default(),
            ));
        }
    }

    for decl in program.declarations.iter_mut() {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if func.is_external {
                    continue;
                }
                if let Some(body) = func.body.as_mut() {
                    add_trailing_return_if_missing(body);
                }
            }
            // Impl methods compile to `<TypeName>$<method>` free fns — need the
            // same `Return(None)` so `call::<()>` doesn't hit the value-return path.
            TypedDeclaration::Impl(imp) => {
                for method in &mut imp.methods {
                    if let Some(body) = method.body.as_mut() {
                        add_trailing_return_if_missing(body);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile a `.blinc` source and stringify errors. Fresh DSL each call.
    fn try_compile(source: &str, filename: &str) -> Result<Vec<String>, String> {
        let _ = tracing_subscriber::fmt::try_init();
        let dsl = BlincDsl::new().map_err(|e| e.to_string())?;
        dsl.compile_source(source, filename)
            .map_err(|e| e.to_string())
    }

    /// Regression: `component Foo { view { ... } }` must register `Foo$view`.
    /// Empty `trait_name` is the inherent-impl marker — without it Zyntax's
    /// `lower_impl_block` silently drops the methods.
    #[test]
    fn compile_component_registers_view_symbol() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let symbols = dsl
            .compile_source(
                r#"component Greeting { view { text("hi from Greeting") } }"#,
                "compile_component.blinc",
            )
            .expect("compile");

        assert!(
            symbols.iter().any(|s| s == "Greeting$view"),
            "expected `Greeting$view` in compiled symbols, got {:?}",
            symbols
        );
    }

    /// Unused prop arg flows through silently.
    #[test]
    fn render_component_with_unused_prop() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            component Counter (initial: i32) {
                view { text("static") }
            }
            view { Counter(42) }
            "#,
            "unused_prop.blinc",
        )
        .expect("compile");

        let ops = dsl.render_view().expect("render_view");
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            DslOp::Text(s) => assert_eq!(s, "static"),
            other => panic!("expected DslOp::Text(\"static\"), got {other:?}"),
        }
    }

    /// Bare-view literal-only f-string interpolation.
    #[test]
    fn render_bare_view_with_fstring_literal() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(r#"view { text(f"hi {42}") }"#, "bare_fstring_literal.blinc")
            .expect("compile");

        let ops = dsl.render_view().expect("render_view");
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            DslOp::Text(s) => assert_eq!(s, "hi 42"),
            other => panic!("expected DslOp::Text(\"hi 42\"), got {other:?}"),
        }
    }

    /// End-to-end string-signal pipeline: host writes, DSL reads, render reflects.
    #[test]
    fn render_view_reads_string_signal() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            signal title: string
            view { text(f"hi {title.get()}") }
            "#,
            "string_signal.blinc",
        )
        .expect("compile");

        dsl.set_signal_string("title", "Welcome");

        let ops = dsl.render_view().expect("render_view");
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            DslOp::Text(s) => assert_eq!(s, "hi Welcome"),
            other => panic!("expected DslOp::Text(\"hi Welcome\"), got {other:?}"),
        }

        // Update and re-render — view sees the new value.
        dsl.set_signal_string("title", "Updated");
        let ops = dsl.render_view().expect("render_view");
        match &ops[0] {
            DslOp::Text(s) => assert_eq!(s, "hi Updated"),
            other => panic!("expected DslOp::Text(\"hi Updated\"), got {other:?}"),
        }
    }

    /// Unset string signal → empty string (matches `get_str_or_default`).
    #[test]
    fn render_view_string_signal_unset_defaults_to_empty() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            signal greeting: string
            view { text(f"prefix:{greeting.get()}") }
            "#,
            "string_signal_unset.blinc",
        )
        .expect("compile");

        let ops = dsl.render_view().expect("render_view");
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            DslOp::Text(s) => assert_eq!(s, "prefix:"),
            other => panic!("expected `\"prefix:\"` DslOp::Text, got {other:?}"),
        }
    }

    /// f-string interpolation inside an impl-method view body.
    #[test]
    fn render_component_view_with_literal_fstring() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            component Greeting {
                view { text(f"hi {42}") }
            }
            view { Greeting() }
            "#,
            "literal_fstring_in_impl.blinc",
        )
        .expect("compile");

        let ops = dsl.render_view().expect("render_view");
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            DslOp::Text(s) => assert_eq!(s, "hi 42"),
            other => panic!("expected DslOp::Text(\"hi 42\"), got {other:?}"),
        }
    }

    /// `view { Outer() { Inner() Inner() } }` flattens into parent+child ops in source order.
    #[test]
    fn render_view_with_component_children() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            component Outer { view { text("outer") } }
            component Inner { view { text("inner") } }
            view {
                Outer() {
                    Inner()
                    Inner()
                }
            }
            "#,
            "children_e2e.blinc",
        )
        .expect("compile");

        let ops = dsl.render_view().expect("render_view");
        assert_eq!(ops.len(), 3, "expected outer + 2 inners, got {ops:?}");
        match (&ops[0], &ops[1], &ops[2]) {
            (DslOp::Text(a), DslOp::Text(b), DslOp::Text(c)) => {
                assert_eq!(a, "outer");
                assert_eq!(b, "inner");
                assert_eq!(c, "inner");
            }
            other => panic!("expected 3 text ops in order, got {other:?}"),
        }
    }

    /// `slot Name { ... }` body items flatten alongside default children.
    #[test]
    fn render_view_with_slots_flattens() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            component Tabs { view { text("tabs") } }
            component Tab { view { text("tab") } }
            view {
                Tabs() {
                    slot Header { Tab() }
                    Tab()
                }
            }
            "#,
            "slots_e2e.blinc",
        )
        .expect("compile");

        let ops = dsl.render_view().expect("render_view");
        // Tabs + Header's Tab + default-children Tab = 3 ops.
        assert_eq!(ops.len(), 3, "expected tabs + 2 tabs, got {ops:?}");
    }

    /// `Outer() { Mid() { Inner() } }` — flatten is recursive.
    #[test]
    fn render_view_with_nested_component_children() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            component Outer { view { text("outer") } }
            component Mid { view { text("mid") } }
            component Inner { view { text("inner") } }
            view {
                Outer() {
                    Mid() {
                        Inner()
                    }
                }
            }
            "#,
            "nested_children.blinc",
        )
        .expect("compile");

        let ops = dsl.render_view().expect("render_view");
        assert_eq!(ops.len(), 3, "expected outer + mid + inner, got {ops:?}");
        match (&ops[0], &ops[1], &ops[2]) {
            (DslOp::Text(a), DslOp::Text(b), DslOp::Text(c)) => {
                assert_eq!(a, "outer");
                assert_eq!(b, "mid");
                assert_eq!(c, "inner");
            }
            other => panic!("expected outer/mid/inner in order, got {other:?}"),
        }
    }

    /// Prop value bound to param, interpolated in an f-string, rendered.
    #[test]
    fn render_component_with_prop_in_fstring() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            component Counter (initial: i32) {
                view { text(f"value: {initial}") }
            }
            view { Counter(42) }
            "#,
            "render_prop.blinc",
        )
        .expect("compile");

        let ops = dsl.render_view().expect("render_view");
        match &ops[0] {
            DslOp::Text(s) => assert_eq!(s, "value: 42"),
            other => panic!("expected DslOp::Text(\"value: 42\"), got {other:?}"),
        }
    }

    /// `bind_component_props` writes the prop list onto each method's params (source order).
    #[test]
    fn bind_component_props_writes_view_params() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Counter (initial: i32, step: i32) {
                    view { text("static") }
                    fn on_click() { }
                }
                "#,
                "bind_props.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .expect("expected an Impl decl");

        // Props bound on both methods; marker stripped.
        for method_name in ["view", "on_click"] {
            let method = impl_block
                .methods
                .iter()
                .find(|m| m.name.resolve_global().as_deref() == Some(method_name))
                .unwrap_or_else(|| panic!("expected method `{method_name}`"));
            assert_eq!(
                method.params.len(),
                2,
                "{method_name} should receive 2 prop params, got {:?}",
                method
                    .params
                    .iter()
                    .map(|p| p.name.resolve_global())
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                method.params[0].name.resolve_global().as_deref(),
                Some("initial")
            );
            assert_eq!(
                method.params[1].name.resolve_global().as_deref(),
                Some("step")
            );
        }

        assert!(
            !impl_block
                .methods
                .iter()
                .any(|m| { m.name.resolve_global().as_deref() == Some("__component_props__") }),
            "marker should be stripped after binding"
        );
    }

    /// Bare `view { Inner() }` composes via the mangled `Inner$view` symbol.
    #[test]
    fn render_view_invoking_component() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            component Inner { view { text("from inner") } }
            view { Inner() }
            "#,
            "view_invokes_component.blinc",
        )
        .expect("compile");

        let ops = dsl.render_view().expect("render_view");
        assert_eq!(
            ops.len(),
            1,
            "expected 1 op from the nested view, got {ops:?}"
        );
        match &ops[0] {
            DslOp::Text(s) => assert_eq!(s, "from inner"),
            other => panic!("expected DslOp::Text, got {other:?}"),
        }
    }

    /// `render_component(name)` invokes the mangled `<name>$view`.
    #[test]
    fn render_component_emits_view_ops() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"component Hello { view { text("hello world") } }"#,
            "render_component.blinc",
        )
        .expect("compile");

        let ops = dsl
            .render_component("Hello")
            .expect("render_component should invoke Hello$view");

        assert_eq!(ops.len(), 1, "expected 1 op, got {ops:?}");
        match &ops[0] {
            DslOp::Text(s) => assert_eq!(s, "hello world"),
            other => panic!("expected DslOp::Text, got {other:?}"),
        }
    }

    /// Round-trip: `view { text("...") }` through the full pipeline.
    #[test]
    fn round_trip_text_view() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(r#"view { text("Hello, Blinc DSL!") }"#, "smoke.blinc")
            .expect("compile");
        let ops = dsl.render_view().expect("render_view");

        assert_eq!(ops.len(), 1, "expected 1 op, got {ops:?}");
        match &ops[0] {
            DslOp::Text(s) => assert_eq!(s, "Hello, Blinc DSL!"),
            other => panic!("expected DslOp::Text, got {other:?}"),
        }
    }

    // Component (Class + Impl) parsing tests.

    /// `component Counter { ... }` parses to a `Class` decl with the fields.
    #[test]
    fn parse_component_struct_only() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"component Counter { count: i32, width: i32 }"#,
                "struct_only.blinc",
            )
            .expect("parse");

        let class = program
            .declarations
            .iter()
            .find_map(|d| {
                if let zyntax_typed_ast::TypedDeclaration::Class(c) = &d.node {
                    Some(c)
                } else {
                    None
                }
            })
            .expect("expected at least one Class decl");

        assert_eq!(class.name.resolve_global().as_deref(), Some("Counter"));
        assert_eq!(class.fields.len(), 2, "expected 2 fields");
        assert_eq!(
            class.fields[0].name.resolve_global().as_deref(),
            Some("count")
        );
        assert_eq!(
            class.fields[1].name.resolve_global().as_deref(),
            Some("width")
        );
    }

    /// `impl Counter { fn view() { ... } }` parses to an `Impl` decl with a method.
    #[test]
    fn parse_impl_with_view() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(r#"impl Counter { fn view() { text("hi") } }"#, "impl.blinc")
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| {
                if let zyntax_typed_ast::TypedDeclaration::Impl(i) = &d.node {
                    Some(i)
                } else {
                    None
                }
            })
            .expect("expected an Impl decl");

        // Empty `trait_name` = inherent-impl marker.
        assert_eq!(impl_block.trait_name.resolve_global().as_deref(), Some(""));
        assert_eq!(impl_block.methods.len(), 1, "expected 1 method (view)");
        assert_eq!(
            impl_block.methods[0].name.resolve_global().as_deref(),
            Some("view")
        );
    }

    // Reactivity tests — `state` wraps the field type in `Type::Named { State, [T] }`.

    /// `state count: i32` → `TypedField` with `Type::Named { State, [i32] }`.
    #[test]
    fn parse_state_field_wraps_type() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"component Counter { state count: i32 }"#,
                "state_field.blinc",
            )
            .expect("parse");

        let class = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Class(c) => Some(c),
                _ => None,
            })
            .expect("expected a Class");

        assert_eq!(class.fields.len(), 1);
        let count_field = &class.fields[0];
        assert_eq!(count_field.name.resolve_global().as_deref(), Some("count"));

        // `state` wraps the type in `Type::Named { State, [i32] }`.
        match &count_field.ty {
            zyntax_typed_ast::Type::Named { id, type_args, .. } => {
                let _ = dsl.runtime.lock().ok().map(|_| ());
                assert_eq!(
                    type_args.len(),
                    1,
                    "expected one type arg, got {type_args:?}"
                );
                match &type_args[0] {
                    zyntax_typed_ast::Type::Primitive(prim) => {
                        assert!(
                            matches!(prim, zyntax_typed_ast::PrimitiveType::I32),
                            "expected i32 inner, got {prim:?}"
                        );
                    }
                    other => panic!("expected primitive inner, got {other:?}"),
                }
                let _ = id;
            }
            other => panic!("state field should be Type::Named, got {other:?}"),
        }
    }

    /// Mixed reactive + plain fields with mixed types:
    /// `state count: i32, name: string`. State field wrapped in
    /// `Type::Named { State, [i32] }`; plain field stays as the
    /// bare primitive.
    ///
    /// Earlier this test mis-attributed a parse failure to a PEG
    /// ambiguity in `struct_field`'s alternates. The actual cause
    /// was that the grammar was emitting `Type::Primitive { name:
    /// intern("string") }` while Zyntax's
    /// `primitive_type_from_name` (interpreter.rs:2017) recognises
    /// only `str` / `String` for the string primitive (matching
    /// ml.zyn's `prim_str` at ml.zyn:1444). Construct-type fell
    /// through to "unknown primitive type" and the rule failed —
    /// looked like an alternate-ordering issue from the outside,
    /// but was a tag-name mismatch. Fixed by emitting
    /// `intern("str")` internally while keeping `string` as the
    /// user-facing DSL keyword.
    #[test]
    fn parse_mixed_state_and_plain_fields() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"component Profile { state count: i32, name: string }"#,
                "mixed_fields.blinc",
            )
            .expect("parse");

        let class = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Class(c) => Some(c),
                _ => None,
            })
            .expect("expected a Class");

        assert_eq!(class.fields.len(), 2);

        let count_field = &class.fields[0];
        assert_eq!(count_field.name.resolve_global().as_deref(), Some("count"));
        assert!(
            matches!(&count_field.ty, zyntax_typed_ast::Type::Named { .. }),
            "state field should be Type::Named (State<...>), got {:?}",
            count_field.ty
        );

        let name_field = &class.fields[1];
        assert_eq!(name_field.name.resolve_global().as_deref(), Some("name"));
        assert!(
            matches!(
                &name_field.ty,
                zyntax_typed_ast::Type::Primitive(zyntax_typed_ast::PrimitiveType::String)
            ),
            "plain field should be Primitive(String), got {:?}",
            name_field.ty
        );
    }

    /// Two `state` fields in the same field list parse cleanly.
    #[test]
    fn parse_two_state_fields_same_list() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"component Counter { state count: i32, state width: i32 }"#,
                "two_states.blinc",
            )
            .expect("parse");

        let class = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Class(c) => Some(c),
                _ => None,
            })
            .expect("expected a Class");

        assert_eq!(class.fields.len(), 2);
        for f in &class.fields {
            assert!(
                matches!(&f.ty, zyntax_typed_ast::Type::Named { .. }),
                "every field is `state`, so each ty should be Type::Named, got {:?}",
                f.ty
            );
        }
    }

    /// Split form: separate `component Name { ... }` + `impl Name { ... }` blocks.
    #[test]
    fn parse_component_split_form() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Counter { count: i32 }
                impl Counter {
                    fn view() { text("count") }
                }
                "#,
                "counter_split.blinc",
            )
            .expect("parse");

        let mut class_count = 0;
        let mut impl_count = 0;
        for decl in &program.declarations {
            match &decl.node {
                zyntax_typed_ast::TypedDeclaration::Class(_) => class_count += 1,
                zyntax_typed_ast::TypedDeclaration::Impl(_) => impl_count += 1,
                _ => {}
            }
        }
        assert_eq!(class_count, 1, "expected 1 Class decl");
        assert_eq!(impl_count, 1, "expected 1 Impl decl");
    }

    /// Folded `component { ... }` emits both Class and Impl from one block.
    #[test]
    fn parse_component_folded() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                // Empty `on_click` body — just validates the handler is recognised as a method.
                r#"
                component Counter {
                    count: i32
                    view { text("count") }
                    fn on_click() {}
                }
                "#,
                "counter_folded.blinc",
            )
            .expect("parse");

        let class = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Class(c) => Some(c),
                _ => None,
            })
            .expect("expected a Class decl from the folded component");
        assert_eq!(class.name.resolve_global().as_deref(), Some("Counter"));
        assert_eq!(
            class.fields.len(),
            1,
            "expected one field (count) in folded component"
        );

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .expect("expected an Impl decl from the folded component");
        assert_eq!(impl_block.trait_name.resolve_global().as_deref(), Some(""));
        assert_eq!(
            impl_block.methods.len(),
            2,
            "expected view + on_click methods, got {:?}",
            impl_block
                .methods
                .iter()
                .map(|m| m.name.resolve_global())
                .collect::<Vec<_>>()
        );

        // view first (prepended), on_click second.
        assert_eq!(
            impl_block.methods[0].name.resolve_global().as_deref(),
            Some("view")
        );
        assert_eq!(
            impl_block.methods[1].name.resolve_global().as_deref(),
            Some("on_click")
        );
    }

    /// Folded component with props binds props as leading method params.
    #[test]
    fn parse_component_with_props_folded() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Counter (initial: i32, step: i32) {
                    state count: i32
                    view { text("count") }
                }
                "#,
                "counter_with_props.blinc",
            )
            .expect("parse");

        // Class.fields = body fields only (props bind to methods, not Class).
        let class = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Class(c) => Some(c),
                _ => None,
            })
            .expect("expected a Class decl from the folded component");
        assert_eq!(class.fields.len(), 1, "only the body's state field");
        assert_eq!(
            class.fields[0].name.resolve_global().as_deref(),
            Some("count")
        );

        // After bind_component_props: view has props as leading params (source order).
        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .expect("expected an Impl decl");

        assert!(
            !impl_block
                .methods
                .iter()
                .any(|m| { m.name.resolve_global().as_deref() == Some("__component_props__") }),
            "__component_props__ marker should be stripped after binding"
        );

        let view = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .expect("expected a view method");

        let param_names: Vec<_> = view
            .params
            .iter()
            .map(|p| p.name.resolve_global())
            .collect();
        assert_eq!(
            view.params.len(),
            2,
            "view should receive 2 props, got params: {:?}",
            param_names
        );
        assert_eq!(
            view.params[0].name.resolve_global().as_deref(),
            Some("initial")
        );
        assert_eq!(
            view.params[1].name.resolve_global().as_deref(),
            Some("step")
        );
    }

    /// Struct-only form with props: parsed but props silently dropped (no methods to bind to).
    #[test]
    fn parse_component_with_props_struct_only() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"component Pair (left: i32, right: i32) { sum: i32 }"#,
                "pair_with_props.blinc",
            )
            .expect("parse");

        let class = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Class(c) => Some(c),
                _ => None,
            })
            .expect("expected a Class decl");

        // Only body's `sum` field — props dropped (no method to bind to).
        assert_eq!(class.fields.len(), 1);
        assert_eq!(
            class.fields[0].name.resolve_global().as_deref(),
            Some("sum")
        );
    }

    /// Empty props parens (`Foo () { ... }`) is legal.
    #[test]
    fn parse_component_with_empty_props() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Empty () {
                    state x: i32
                    view { text("x") }
                }
                "#,
                "empty_props.blinc",
            )
            .expect("parse");

        let class = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Class(c) => Some(c),
                _ => None,
            })
            .expect("expected a Class");

        assert_eq!(class.fields.len(), 1, "only the state field is present");
        assert_eq!(class.fields[0].name.resolve_global().as_deref(), Some("x"));
    }

    /// `Counter()` lowers to `Call(Counter$view, [])` with marker folded away.
    #[test]
    fn parse_component_call_no_args() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Counter { }
                view { Counter() }
                "#,
                "call_no_args.blinc",
            )
            .expect("parse");

        let stmts = first_user_function_body(&program);
        assert_eq!(stmts.len(), 1, "expected 1 stmt, got {stmts:?}");

        let expr_node = unwrap_trailing_call(&stmts[0]);
        let TypedExpression::Call(call) = &expr_node.node else {
            panic!("expected Call expression");
        };

        let TypedExpression::Variable(callee_name) = &call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            callee_name.resolve_global().as_deref(),
            Some("Counter$view"),
            "callee should be the component's view symbol after lowering"
        );

        assert_eq!(call.positional_args.len(), 0, "no args");
        assert_eq!(call.named_args.len(), 0, "no named args");
    }

    /// `Counter(1, 2)` lowers to positional args in source order.
    #[test]
    fn parse_component_call_positional_args() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Counter { }
                view { Counter(1, 2) }
                "#,
                "call_positional.blinc",
            )
            .expect("parse");

        let stmts = first_user_function_body(&program);
        let expr_node = unwrap_trailing_call(&stmts[0]);
        let TypedExpression::Call(call) = &expr_node.node else {
            panic!("expected Call");
        };

        let TypedExpression::Variable(callee_name) = &call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            callee_name.resolve_global().as_deref(),
            Some("Counter$view")
        );

        assert_eq!(call.positional_args.len(), 2);

        let TypedExpression::Literal(TypedLiteral::Integer(one)) = &call.positional_args[0].node
        else {
            panic!("arg 0 should be Integer(1)");
        };
        let TypedExpression::Literal(TypedLiteral::Integer(two)) = &call.positional_args[1].node
        else {
            panic!("arg 1 should be Integer(2)");
        };
        assert_eq!(*one, 1);
        assert_eq!(*two, 2);
    }

    /// `Counter(1, step = 2)` — named arg lifted into `named_args`.
    #[test]
    fn parse_component_call_named_arg() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Counter { }
                view { Counter(1, step = 2) }
                "#,
                "call_named.blinc",
            )
            .expect("parse");

        let stmts = first_user_function_body(&program);
        let expr_node = unwrap_trailing_call(&stmts[0]);
        let TypedExpression::Call(call) = &expr_node.node else {
            panic!("expected Call");
        };

        let TypedExpression::Variable(callee_name) = &call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            callee_name.resolve_global().as_deref(),
            Some("Counter$view")
        );

        assert_eq!(call.positional_args.len(), 1);
        let TypedExpression::Literal(TypedLiteral::Integer(one)) = &call.positional_args[0].node
        else {
            panic!("positional arg 0 should be Integer(1)");
        };
        assert_eq!(*one, 1);

        assert_eq!(call.named_args.len(), 1);
        assert_eq!(
            call.named_args[0].name.resolve_global().as_deref(),
            Some("step")
        );
        let TypedExpression::Literal(TypedLiteral::Integer(named_value)) =
            &call.named_args[0].value.node
        else {
            panic!("named arg value should be Integer(2)");
        };
        assert_eq!(*named_value, 2);
    }

    /// `let widget = Counter(0)` — component call in expression position.
    #[test]
    fn parse_component_call_in_expr_position() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Counter { }
                view { let widget = Counter(0) }
                "#,
                "call_expr_position.blinc",
            )
            .expect("parse");

        let stmts = first_user_function_body(&program);
        assert_eq!(stmts.len(), 1);

        let TypedStatement::Let(let_stmt) = &stmts[0].node else {
            panic!("expected Let statement");
        };
        let init = let_stmt
            .initializer
            .as_ref()
            .expect("let must have initializer");
        let TypedExpression::Call(call) = &init.node else {
            panic!("expected Call initializer, got {:?}", init.node);
        };
        let TypedExpression::Variable(callee_name) = &call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            callee_name.resolve_global().as_deref(),
            Some("Counter$view"),
            "callee should be mangled view symbol after lowering"
        );
    }

    /// `Div` and `Text` are pre-registered at `BlincDsl::new()` time.
    #[test]
    fn blinc_layout_primitives_registered() {
        let _ = tracing_subscriber::fmt::try_init();

        let _dsl = BlincDsl::new().expect("runtime init");

        let div =
            blinc_runtime::component::with_component_registry(|r| r.get_by_name("Div").cloned())
                .expect("Div should be pre-registered");
        assert_eq!(div.view_symbol.as_ref(), "$Blinc$Div$view");
        assert!(
            div.prop("children").is_some(),
            "Div should advertise its `children` slot"
        );

        let text =
            blinc_runtime::component::with_component_registry(|r| r.get_by_name("Text").cloned())
                .expect("Text should be pre-registered");
        assert_eq!(text.view_symbol.as_ref(), "$Blinc$Text$view");
        assert!(text.prop("content").is_some());
    }

    /// Smallest value-returning view: `view { Text("hello") }` compiles and runs.
    #[test]
    fn render_text_widget_compiles_and_runs() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let result = dsl.compile_source(r#"view { Text("hello") }"#, "text_widget_smoke.blinc");
        assert!(
            result.is_ok(),
            "Text widget primitive should compile: {:?}",
            result.err()
        );

        // The legacy `render_view` returns `Vec<DslOp>` — the
        // value-returning extern doesn't push to the scene
        // buffer, so we expect an empty op vec. The widget
        // handle returned by `$Blinc$Text$view` gets discarded
        // today (Phase 2d wires the return value through the
        // view function's signature; Phase 2e materialises it
        // back into a `blinc_layout::Text`).
        let ops = dsl.render_view().expect("render_view");
        assert!(
            ops.is_empty(),
            "value-returning Text extern shouldn't push DslOps, got: {ops:?}"
        );
    }

    /// DSL source that calls `Div { Text("hi") }` validates
    /// cleanly because both primitives are pre-registered.
    /// Compile / render is NOT exercised here — the extern
    /// bodies aren't wired yet, so a full compile would fail
    /// at JIT link time. This pins the front-end half of the
    /// pivot: the parser + validation pass already accept the
    /// widget-tree DSL shape.
    #[test]
    fn validate_accepts_blinc_layout_primitives() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let result = dsl.parse_to_typed_ast(
            r#"
            view {
                Div() {
                    Text("hello")
                }
            }
            "#,
            "widget_primitives_parse.blinc",
        );
        assert!(
            result.is_ok(),
            "DSL using pre-registered Div/Text should validate, got: {:?}",
            result.err()
        );
    }

    /// Validator rejects undeclared components and names them in the diagnostic.
    #[test]
    fn validate_rejects_unknown_component() {
        let _ = tracing_subscriber::fmt::try_init();

        // Unique name to avoid cross-test pollution in the global component registry.
        let dsl = BlincDsl::new().expect("runtime init");
        let result = dsl.parse_to_typed_ast(
            r#"view { UnknownComponentValidateTest(1) }"#,
            "unknown_component.blinc",
        );
        let err = result.expect_err("unknown component should fail validation");
        let msg = format!("{err}");
        assert!(
            msg.contains("unknown component `UnknownComponentValidateTest`"),
            "diagnostic should name the failing component, got: {msg}"
        );
    }

    /// Validator accepts forward references (declared after first use).
    #[test]
    fn validate_accepts_forward_reference() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let result = dsl.parse_to_typed_ast(
            r#"
            view { Inner(0) }
            component Inner (x: i32) { }
            "#,
            "forward_ref.blinc",
        );
        assert!(
            result.is_ok(),
            "forward reference should validate, got: {:?}",
            result.err()
        );
    }

    /// Validator collects ALL unknown-component errors (not just the first).
    #[test]
    fn validate_collects_multiple_unknown_calls() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let result = dsl.parse_to_typed_ast(
            r#"
            view {
                FooBar(0)
                BazQux(1)
            }
            "#,
            "many_unknown.blinc",
        );
        let err = result.expect_err("two unknown components should fail");
        let msg = format!("{err}");
        assert!(
            msg.contains("FooBar"),
            "missing FooBar in diagnostic: {msg}"
        );
        assert!(
            msg.contains("BazQux"),
            "missing BazQux in diagnostic: {msg}"
        );
    }

    /// `Counter() { Inner(1) Inner(2) }` flattens to parent + child stmts; no body-Block arg.
    #[test]
    fn parse_component_call_with_body_children() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Counter { }
                component Inner { }
                view {
                    Counter() {
                        Inner(1)
                        Inner(2)
                    }
                }
                "#,
                "body_children.blinc",
            )
            .expect("parse");

        let stmts = first_user_function_body(&program);
        assert_eq!(
            stmts.len(),
            3,
            "expected flat [Counter$view, Inner$view, Inner$view], got {} stmts",
            stmts.len()
        );

        fn callee_name(stmt: &zyntax_typed_ast::TypedNode<TypedStatement>) -> Option<String> {
            // `unwrap_trailing_call` peels `Return(Some(...))` wrappers.
            let TypedExpression::Call(c) = &unwrap_trailing_call(stmt).node else {
                return None;
            };
            let TypedExpression::Variable(v) = &c.callee.node else {
                return None;
            };
            v.resolve_global().map(|s| s.to_string())
        }
        assert_eq!(callee_name(&stmts[0]).as_deref(), Some("Counter$view"));
        assert_eq!(callee_name(&stmts[1]).as_deref(), Some("Inner$view"));
        assert_eq!(callee_name(&stmts[2]).as_deref(), Some("Inner$view"));

        // Parent has no body-Block arg — children inlined.
        let TypedExpression::Call(c) = &unwrap_trailing_call(&stmts[0]).node else {
            unreachable!()
        };
        assert_eq!(
            c.positional_args.len(),
            0,
            "Counter$view should have no args after body inlining, got: {:?}",
            c.positional_args
        );
    }

    /// Bare-form `view { Text("hi") }` lowers to `Return(Some(...))` with `I64` return.
    #[test]
    fn lower_view_to_value_returning_wraps_primitive_call() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                view { Text("hi") }
                "#,
                "value_returning_view.blinc",
            )
            .expect("parse");

        let render_view = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Function(f)
                    if f.name.resolve_global().as_deref() == Some("render_view") =>
                {
                    Some(f)
                }
                _ => None,
            })
            .expect("expected a render_view function");

        assert_eq!(
            render_view.return_type,
            Type::Primitive(PrimitiveType::I64),
            "render_view should return I64 (widget handle) after value-returning rewrite"
        );

        let body = render_view
            .body
            .as_ref()
            .expect("render_view should have a body");
        let last = body
            .statements
            .last()
            .expect("body should have at least one stmt");
        let TypedStatement::Return(Some(expr)) = &last.node else {
            panic!("expected trailing `Return(Some(_))`, got: {:?}", last.node);
        };
        let TypedExpression::Call(call) = &expr.node else {
            panic!("returned expr should be a Call");
        };
        let TypedExpression::Variable(callee) = &call.callee.node else {
            panic!("callee should be a Variable");
        };
        assert_eq!(
            callee.resolve_global().as_deref(),
            Some("$Blinc$Text$view"),
            "callee should be the primitive Text view symbol"
        );
    }

    /// Trailing legacy `text(...)` (Unit-returning) does NOT get value-return rewrite.
    #[test]
    fn lower_view_to_value_returning_skips_legacy_text_extern() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                view { text("hi") }
                "#,
                "legacy_text_view.blinc",
            )
            .expect("parse");

        let render_view = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Function(f)
                    if f.name.resolve_global().as_deref() == Some("render_view") =>
                {
                    Some(f)
                }
                _ => None,
            })
            .expect("expected a render_view function");

        assert_eq!(
            render_view.return_type,
            Type::Primitive(PrimitiveType::Unit),
            "legacy text(...) view should stay Unit-returning"
        );

        let body = render_view
            .body
            .as_ref()
            .expect("render_view should have a body");
        let last = body
            .statements
            .last()
            .expect("body should have at least one stmt");
        assert!(
            matches!(last.node, TypedStatement::Expression(_)),
            "trailing stmt should stay as Expression(_), got: {:?}",
            last.node
        );
    }

    /// Slot bodies partition into `slot_<Name>` + default `children` named args.
    #[test]
    fn slot_bodies_partition_into_named_args() {
        let _ = tracing_subscriber::fmt::try_init();

        // Synthetic widget so the partition has somewhere to route slot bodies.
        blinc_runtime::component::with_component_registry_mut(|r| {
            r.register(blinc_runtime::component::ComponentDefinition {
                name: std::sync::Arc::from("SlotProbe"),
                view_symbol: std::sync::Arc::from("$Blinc$SlotProbe$view"),
                props: vec![
                    blinc_runtime::component::PropDef {
                        name: std::sync::Arc::from("children"),
                        ty: Type::Primitive(PrimitiveType::I64),
                    },
                    blinc_runtime::component::PropDef {
                        name: std::sync::Arc::from("slot_Header"),
                        ty: Type::Primitive(PrimitiveType::I64),
                    },
                ],
            });
        });

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                view {
                    SlotProbe() {
                        slot Header { Text("h") }
                        Text("body")
                    }
                }
                "#,
                "slot_probe.blinc",
            )
            .expect("parse");

        // Assert TWO `__new_child_list__` let bindings (default children + Header).
        let stmts = first_user_function_body(&program);
        let TypedStatement::Return(Some(e)) = &stmts[0].node else {
            panic!("expected Return(Some(...))");
        };
        let TypedExpression::Block(block) = &e.node else {
            panic!("expected Block expansion, got: {:?}", e.node);
        };
        let new_list_count = block
            .statements
            .iter()
            .filter(|s| {
                let TypedStatement::Let(l) = &s.node else {
                    return false;
                };
                let Some(init) = &l.initializer else {
                    return false;
                };
                let TypedExpression::Call(c) = &init.node else {
                    return false;
                };
                let TypedExpression::Variable(v) = &c.callee.node else {
                    return false;
                };
                v.resolve_global().as_deref() == Some("__new_child_list__")
            })
            .count();
        assert_eq!(
            new_list_count, 2,
            "should mint one child-list per slot (default + Header)"
        );
    }

    /// `Text(content = "hi")` lowers to a positional call.
    #[test]
    fn named_args_on_primitive_call_resolve_to_positional() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(r#"view { Text(content = "hi") }"#, "named_args.blinc")
            .expect("parse");

        let stmts = first_user_function_body(&program);
        assert_eq!(stmts.len(), 1);
        let TypedStatement::Return(Some(e)) = &stmts[0].node else {
            panic!("expected Return(Some(...)), got: {:?}", stmts[0].node);
        };
        let TypedExpression::Call(c) = &e.node else {
            panic!("returned expr should be a Call, got: {:?}", e.node);
        };
        let TypedExpression::Variable(callee) = &c.callee.node else {
            panic!("callee should be Variable");
        };
        assert_eq!(callee.resolve_global().as_deref(), Some("$Blinc$Text$view"));
        assert!(
            c.named_args.is_empty(),
            "named args should have been resolved to positional, got: {:?}",
            c.named_args
        );
        assert_eq!(c.positional_args.len(), 1, "Text takes one (content) prop");
        let TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::String(s)) =
            &c.positional_args[0].node
        else {
            panic!(
                "positional[0] should be the string literal, got: {:?}",
                c.positional_args[0].node
            );
        };
        assert_eq!(
            s.resolve_global().as_deref(),
            Some("hi"),
            "the named-arg value should land at position 0"
        );
    }

    /// Container primitive body lowers to `let __list__ = __new_child_list__()` +
    /// `__push_child__`s + trailing container call.
    #[test]
    fn parse_primitive_call_with_body_lowers_to_children_block_expansion() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                view {
                    Div() {
                        Text("a")
                        Text("b")
                    }
                }
                "#,
                "primitive_body.blinc",
            )
            .expect("parse");

        let stmts = first_user_function_body(&program);
        assert_eq!(stmts.len(), 1, "body should stay as one stmt");

        let TypedStatement::Return(Some(e)) = &stmts[0].node else {
            panic!(
                "stmts[0] should be Return(Some(Block)), got: {:?}",
                stmts[0].node
            );
        };
        let TypedExpression::Block(block) = &e.node else {
            panic!(
                "returned expr should be a Block expansion, got: {:?}",
                e.node
            );
        };

        // 0: let __blinc_children_N = __new_child_list__()
        // 1-2: __push_child__(list, Text$view("a"|"b"))
        // 3: $Blinc$Div$view(list)
        assert_eq!(
            block.statements.len(),
            4,
            "expected 4 stmts (let + 2 pushes + final call), got: {block:?}"
        );

        let TypedStatement::Let(let_stmt) = &block.statements[0].node else {
            panic!("stmt[0] should be Let, got: {:?}", block.statements[0].node);
        };
        let list_ident = let_stmt
            .name
            .resolve_global()
            .expect("let name should resolve");
        assert!(
            list_ident.starts_with("__blinc_children_"),
            "let name should follow the __blinc_children_<N> convention, got `{list_ident}`"
        );
        let init = let_stmt
            .initializer
            .as_ref()
            .expect("let should carry an initializer");
        let TypedExpression::Call(init_call) = &init.node else {
            panic!("let initializer should be a Call");
        };
        let TypedExpression::Variable(init_callee) = &init_call.callee.node else {
            panic!("init callee should be a Variable");
        };
        assert_eq!(
            init_callee.resolve_global().as_deref(),
            Some("__new_child_list__"),
            "let initialiser should call __new_child_list__"
        );

        for (i, stmt) in block.statements.iter().enumerate().skip(1).take(2) {
            let TypedStatement::Expression(expr) = &stmt.node else {
                panic!("stmt[{i}] should be Expression(Call)");
            };
            let TypedExpression::Call(push_call) = &expr.node else {
                panic!("stmt[{i}] should be Call");
            };
            let TypedExpression::Variable(push_callee) = &push_call.callee.node else {
                panic!("stmt[{i}] callee should be Variable");
            };
            assert_eq!(
                push_callee.resolve_global().as_deref(),
                Some("__push_child__"),
                "stmt[{i}] should call __push_child__"
            );
            assert_eq!(
                push_call.positional_args.len(),
                2,
                "__push_child__ takes (list, child)"
            );
            let TypedExpression::Variable(list_ref) = &push_call.positional_args[0].node else {
                panic!("__push_child__ arg 0 should be the list ident");
            };
            assert_eq!(
                list_ref.resolve_global().as_deref(),
                Some(list_ident.as_ref())
            );
            let TypedExpression::Call(child_call) = &push_call.positional_args[1].node else {
                panic!("__push_child__ arg 1 should be a child Call");
            };
            let TypedExpression::Variable(child_callee) = &child_call.callee.node else {
                panic!("child callee should be a Variable");
            };
            assert_eq!(
                child_callee.resolve_global().as_deref(),
                Some("$Blinc$Text$view")
            );
        }

        let TypedStatement::Expression(final_expr) = &block.statements[3].node else {
            panic!("stmt[3] should be Expression(Call)");
        };
        let TypedExpression::Call(div_call) = &final_expr.node else {
            panic!("stmt[3] should be Call");
        };
        let TypedExpression::Variable(div_callee) = &div_call.callee.node else {
            panic!("div callee should be a Variable");
        };
        assert_eq!(
            div_callee.resolve_global().as_deref(),
            Some("$Blinc$Div$view")
        );
        assert_eq!(
            div_call.positional_args.len(),
            4,
            "Div takes (children, __style, class, on_click)"
        );
        let TypedExpression::Variable(div_list_arg) = &div_call.positional_args[0].node else {
            panic!("Div arg 0 should be the list ident Variable");
        };
        assert_eq!(
            div_list_arg.resolve_global().as_deref(),
            Some(list_ident.as_ref())
        );
        assert!(
            matches!(
                &div_call.positional_args[1].node,
                TypedExpression::Literal(zyntax_typed_ast::TypedLiteral::Integer(0))
            ),
            "Div arg 1 should be the null overlay literal"
        );
    }

    /// Body Block with a `let` flattens; `let` rides between parent and child calls.
    #[test]
    fn parse_component_call_with_let_in_body() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Wrapper { }
                component Inner { }
                view {
                    Wrapper() {
                        let count = 1
                        Inner(count)
                    }
                }
                "#,
                "body_let.blinc",
            )
            .expect("parse");

        let stmts = first_user_function_body(&program);
        assert_eq!(
            stmts.len(),
            3,
            "expected [Wrapper$view, let, Inner$view] after flatten"
        );
        assert!(
            matches!(stmts[0].node, TypedStatement::Expression(_)),
            "stmts[0] should be the Wrapper call"
        );
        assert!(
            matches!(stmts[1].node, TypedStatement::Let(_)),
            "stmts[1] should be the let binding"
        );
        // Empty component bodies → no view methods → no value-return promotion.
        assert!(
            matches!(stmts[2].node, TypedStatement::Expression(_)),
            "stmts[2] should be the Inner call"
        );
    }

    /// `slot Header { ... }` inside a component body — the
    /// `__slot_open__` / `__slot_close__` markers are stripped at
    /// flatten time (host-side runtime would route named slots,
    /// but the prototype's flat DslOp scene buffer has no slot
    /// concept). Slot bodies fold into the parent as if they were
    /// plain children.
    #[test]
    fn parse_component_call_with_slot() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Tabs { }
                component Tab { }
                view {
                    Tabs() {
                        slot Header {
                            Tab(1)
                        }
                        Tab(2)
                    }
                }
                "#,
                "body_slot.blinc",
            )
            .expect("parse");

        let stmts = first_user_function_body(&program);

        // Flatten output: `Tabs$view(); Tab$view(1); Tab$view(2)`.
        // Slot markers `__slot_open__` / `__slot_close__` get dropped.
        assert_eq!(
            stmts.len(),
            3,
            "expected flat [Tabs$view, Tab$view, Tab$view] after stripping slot markers"
        );

        fn callee_name(stmt: &zyntax_typed_ast::TypedNode<TypedStatement>) -> Option<String> {
            let TypedExpression::Call(c) = &unwrap_trailing_call(stmt).node else {
                return None;
            };
            let TypedExpression::Variable(v) = &c.callee.node else {
                return None;
            };
            v.resolve_global().map(|s| s.to_string())
        }
        assert_eq!(callee_name(&stmts[0]).as_deref(), Some("Tabs$view"));
        assert_eq!(callee_name(&stmts[1]).as_deref(), Some("Tab$view"));
        assert_eq!(callee_name(&stmts[2]).as_deref(), Some("Tab$view"));

        // No slot markers remaining.
        for s in stmts {
            let name = callee_name(s).unwrap_or_default();
            assert!(
                !name.starts_with("__slot_"),
                "found leftover slot marker `{name}` in flattened output"
            );
        }
    }

    /// `lower_component_calls` strips all `__component_call__` callee refs.
    #[test]
    fn lower_component_calls_strips_marker_callee() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Foo { }
                view {
                    Foo(1, name = 2)
                    let x = Foo(3)
                }
                "#,
                "strip_marker.blinc",
            )
            .expect("parse");

        // Count any remaining `__component_call__` refs.
        fn count_marker_refs(expr: &TypedExpression) -> usize {
            let mut n = 0;
            match expr {
                TypedExpression::Variable(name)
                    if name.resolve_global().as_deref() == Some("__component_call__") =>
                {
                    n += 1;
                }
                TypedExpression::Call(c) => {
                    n += count_marker_refs(&c.callee.node);
                    for a in &c.positional_args {
                        n += count_marker_refs(&a.node);
                    }
                    for na in &c.named_args {
                        n += count_marker_refs(&na.value.node);
                    }
                }
                TypedExpression::Block(b) => {
                    for s in &b.statements {
                        if let TypedStatement::Expression(e) = &s.node {
                            n += count_marker_refs(&e.node);
                        }
                        if let TypedStatement::Let(l) = &s.node {
                            if let Some(init) = &l.initializer {
                                n += count_marker_refs(&init.node);
                            }
                        }
                    }
                }
                _ => {}
            }
            n
        }

        let mut total = 0;
        for decl in &program.declarations {
            if let zyntax_typed_ast::TypedDeclaration::Function(func) = &decl.node {
                if let Some(body) = &func.body {
                    for stmt in &body.statements {
                        match &stmt.node {
                            TypedStatement::Expression(e) => {
                                total += count_marker_refs(&e.node);
                            }
                            TypedStatement::Let(l) => {
                                if let Some(init) = &l.initializer {
                                    total += count_marker_refs(&init.node);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        assert_eq!(
            total, 0,
            "expected no __component_call__ refs after lowering, found {}",
            total
        );
    }

    /// Lowercase `counter(0)` does not parse as a component call.
    #[test]
    fn parse_lowercase_call_is_not_component_call() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        // Lowercase calls only land via typed `text(...)` rules.
        let result = dsl.parse_to_typed_ast(r#"view { counter(0) }"#, "lowercase_call.blinc");
        assert!(
            result.is_err(),
            "lowercase `counter(0)` should not parse as a component call"
        );
    }

    /// Single-prop bare component (no body methods) — props silently dropped.
    #[test]
    fn parse_component_with_single_prop() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(r#"component Only (just_one: i32) { }"#, "single_prop.blinc")
            .expect("parse");

        let class = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Class(c) => Some(c),
                _ => None,
            })
            .expect("expected a Class");

        assert_eq!(class.fields.len(), 0);
    }

    /// `text(N)` round-trip — probes the i32 ABI through Cranelift.
    #[test]
    fn round_trip_text_int() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(r#"view { text(42) }"#, "int_smoke.blinc")
            .expect("compile");
        let ops = dsl.render_view().expect("render_view");

        assert_eq!(ops.len(), 1, "expected 1 op, got {ops:?}");
        match &ops[0] {
            DslOp::IntText(n) => assert_eq!(*n, 42),
            other => panic!("expected DslOp::IntText, got {other:?}"),
        }
    }

    // F-string parsing tests — TypedAST shape only.

    use zyntax_typed_ast::typed_ast::{TypedExpression, TypedLiteral};
    use zyntax_typed_ast::TypedDeclaration;

    /// Body statements of the program's first non-extern function (test-only).
    fn first_user_function_body(
        program: &TypedProgram,
    ) -> &[zyntax_typed_ast::TypedNode<TypedStatement>] {
        for decl in program.declarations.iter() {
            if let TypedDeclaration::Function(func) = &decl.node {
                if !func.is_external {
                    return func
                        .body
                        .as_ref()
                        .map(|b| b.statements.as_slice())
                        .unwrap_or(&[]);
                }
            }
        }
        panic!("no user function found in program")
    }

    /// Peel `Return(Some(expr))` to expose the inner expression (test-only).
    fn unwrap_trailing_call(
        stmt: &zyntax_typed_ast::TypedNode<TypedStatement>,
    ) -> &zyntax_typed_ast::TypedNode<TypedExpression> {
        match &stmt.node {
            TypedStatement::Expression(e) => e,
            TypedStatement::Return(Some(e)) => e,
            other => panic!("expected Expression or Return(Some(...)), got: {other:?}"),
        }
    }

    /// `text(f"hello")` — single-part no-interp f-string parses as plain `text("hello")`.
    #[test]
    fn parse_text_fstring_single_part_text() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(r#"view { text(f"hello") }"#, "fstr_single.blinc")
            .expect("parse");

        let stmts = first_user_function_body(&program);
        assert_eq!(stmts.len(), 1, "expected 1 stmt in body, got {stmts:?}");
        let TypedStatement::Expression(call_node) = &stmts[0].node else {
            panic!("expected Expression statement");
        };
        let TypedExpression::Call(call) = &call_node.node else {
            panic!("expected Call");
        };
        assert_eq!(call.positional_args.len(), 1);
        let TypedExpression::Literal(TypedLiteral::String(_)) = &call.positional_args[0].node
        else {
            panic!(
                "expected single string-literal arg, got {:?}",
                call.positional_args[0].node
            );
        };
    }

    /// `text(f"{42}")` — single interp part → bare `__fstring_format__(42)`.
    #[test]
    fn parse_text_fstring_single_part_interp() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(r#"view { text(f"{42}") }"#, "fstr_interp.blinc")
            .expect("parse");

        let stmts = first_user_function_body(&program);
        let TypedStatement::Expression(call_node) = &stmts[0].node else {
            panic!("expected Expression statement");
        };
        let TypedExpression::Call(text_call) = &call_node.node else {
            panic!("expected Call");
        };
        assert_eq!(text_call.positional_args.len(), 1);
        let TypedExpression::Call(fmt_call) = &text_call.positional_args[0].node else {
            panic!(
                "expected nested __fstring_format__ call, got {:?}",
                text_call.positional_args[0].node
            );
        };
        let TypedExpression::Variable(name) = &fmt_call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            name.resolve_global().as_deref(),
            Some("__fstring_format__"),
            "expected __fstring_format__ wrapping the int arg"
        );
    }

    /// `text(f"answer: {42}!")` → `__fstring__(text, fmt, text)` with 3 args.
    #[test]
    fn parse_text_fstring_multi_part() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(r#"view { text(f"answer: {42}!") }"#, "fstr_multi.blinc")
            .expect("parse");

        let stmts = first_user_function_body(&program);
        let TypedStatement::Expression(call_node) = &stmts[0].node else {
            panic!("expected Expression statement");
        };
        let TypedExpression::Call(text_call) = &call_node.node else {
            panic!("expected Call");
        };
        assert_eq!(text_call.positional_args.len(), 1);
        let TypedExpression::Call(fstring_call) = &text_call.positional_args[0].node else {
            panic!(
                "expected nested __fstring__ call, got {:?}",
                text_call.positional_args[0].node
            );
        };
        let TypedExpression::Variable(name) = &fstring_call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            name.resolve_global().as_deref(),
            Some("__fstring__"),
            "expected fold_concat-emitted __fstring__ marker"
        );
        assert_eq!(
            fstring_call.positional_args.len(),
            3,
            "expected three parts (text, int, text), got {:?}",
            fstring_call.positional_args
        );
    }

    // Expression-layer parsing tests.

    /// `text(f"{count}")` — variable interpolation reaches `primary_expr`.
    #[test]
    fn parse_fstring_variable_ref() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(r#"view { text(f"{count}") }"#, "fstr_var.blinc")
            .expect("parse");

        let stmts = first_user_function_body(&program);
        let TypedStatement::Expression(call_node) = &stmts[0].node else {
            panic!("expected Expression statement");
        };
        let TypedExpression::Call(text_call) = &call_node.node else {
            panic!("expected Call");
        };
        let TypedExpression::Call(fmt_call) = &text_call.positional_args[0].node else {
            panic!("expected nested __fstring_format__ call");
        };
        let TypedExpression::Variable(name) = &fmt_call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(name.resolve_global().as_deref(), Some("__fstring_format__"));
        // Arg is Variable("count"), not an int literal.
        assert_eq!(fmt_call.positional_args.len(), 1);
        let TypedExpression::Variable(arg_name) = &fmt_call.positional_args[0].node else {
            panic!(
                "expected Variable arg, got {:?}",
                fmt_call.positional_args[0].node
            );
        };
        assert_eq!(arg_name.resolve_global().as_deref(), Some("count"));
    }

    /// `count = count + 1` parses as `Binary(Var, Assign, Binary(Var, Add, Int))`.
    #[test]
    fn parse_assignment_state_mutation() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Counter {
                    state count: i32
                    view { text(f"{count}") }
                    fn on_click() { count = count + 1 }
                }
                "#,
                "counter_assign.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .expect("expected an Impl decl");

        let on_click = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("on_click"))
            .expect("expected on_click method");

        let body = on_click.body.as_ref().expect("on_click should have a body");
        assert_eq!(
            body.statements.len(),
            1,
            "expected one assignment stmt, got {:?}",
            body.statements
        );

        let TypedStatement::Expression(expr_node) = &body.statements[0].node else {
            panic!("expected Expression stmt");
        };
        let TypedExpression::Binary(outer) = &expr_node.node else {
            panic!("expected outer Binary, got {:?}", expr_node.node);
        };
        assert!(
            matches!(outer.op, zyntax_typed_ast::BinaryOp::Assign),
            "outer op should be Assign, got {:?}",
            outer.op
        );

        let TypedExpression::Variable(target) = &outer.left.node else {
            panic!("expected Variable target, got {:?}", outer.left.node);
        };
        assert_eq!(target.resolve_global().as_deref(), Some("count"));

        let TypedExpression::Binary(rhs) = &outer.right.node else {
            panic!("expected RHS Binary, got {:?}", outer.right.node);
        };
        assert!(
            matches!(rhs.op, zyntax_typed_ast::BinaryOp::Add),
            "RHS op should be Add, got {:?}",
            rhs.op
        );
        let TypedExpression::Variable(lhs_var) = &rhs.left.node else {
            panic!("expected Variable on RHS LHS");
        };
        assert_eq!(lhs_var.resolve_global().as_deref(), Some("count"));
        let TypedExpression::Literal(TypedLiteral::Integer(n)) = &rhs.right.node else {
            panic!("expected IntLiteral on RHS RHS, got {:?}", rhs.right.node);
        };
        assert_eq!(*n, 1);
    }

    /// `1 + 2 * 3` parses with Mul binding tighter than Add.
    #[test]
    fn parse_arithmetic_precedence() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state x: i32
                    view {}
                    fn step() { x = 1 + 2 * 3 }
                }
                "#,
                "precedence.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .expect("expected Impl");

        let step = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("step"))
            .expect("expected step method");
        let body = step.body.as_ref().expect("body");
        let TypedStatement::Expression(node) = &body.statements[0].node else {
            panic!("expected Expression stmt");
        };
        let TypedExpression::Binary(assign) = &node.node else {
            panic!("expected Binary");
        };
        let TypedExpression::Binary(add) = &assign.right.node else {
            panic!("RHS should be Binary(Add)");
        };
        assert!(
            matches!(add.op, zyntax_typed_ast::BinaryOp::Add),
            "top RHS should be Add, got {:?}",
            add.op
        );
        let TypedExpression::Literal(TypedLiteral::Integer(left_n)) = &add.left.node else {
            panic!("Add LHS should be IntLiteral, got {:?}", add.left.node);
        };
        assert_eq!(*left_n, 1);
        let TypedExpression::Binary(mul) = &add.right.node else {
            panic!("Add RHS should be Binary(Mul), got {:?}", add.right.node);
        };
        assert!(
            matches!(mul.op, zyntax_typed_ast::BinaryOp::Mul),
            "nested op should be Mul, got {:?}",
            mul.op
        );
    }

    /// `(1 + 2) * 3` — parens override precedence; `paren_expr` is a pass-through.
    #[test]
    fn parse_paren_grouping() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state x: i32
                    view {}
                    fn step() { x = (1 + 2) * 3 }
                }
                "#,
                "parens.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("step"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        let TypedStatement::Expression(node) = &body.statements[0].node else {
            panic!("expected Expression stmt");
        };
        let TypedExpression::Binary(assign) = &node.node else {
            panic!("expected assign");
        };
        let TypedExpression::Binary(mul) = &assign.right.node else {
            panic!("RHS should be Binary, got {:?}", assign.right.node);
        };
        assert!(
            matches!(mul.op, zyntax_typed_ast::BinaryOp::Mul),
            "top RHS should be Mul, got {:?}",
            mul.op
        );
        let TypedExpression::Binary(add) = &mul.left.node else {
            panic!("Mul LHS should be Add subtree, got {:?}", mul.left.node);
        };
        assert!(matches!(add.op, zyntax_typed_ast::BinaryOp::Add));
    }

    /// `let derived = count + 1` lowers to immutable `TypedStatement::Let`.
    #[test]
    fn parse_let_binding() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view {}
                    fn step() {
                        let derived = count + 1
                    }
                }
                "#,
                "let_binding.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .expect("expected Impl");
        let body = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("step"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();

        assert_eq!(body.statements.len(), 1, "expected one let stmt");
        let TypedStatement::Let(let_node) = &body.statements[0].node else {
            panic!("expected Let, got {:?}", body.statements[0].node);
        };
        assert_eq!(let_node.name.resolve_global().as_deref(), Some("derived"));
        assert!(
            matches!(let_node.mutability, zyntax_typed_ast::Mutability::Immutable),
            "phase-2 let is immutable, got {:?}",
            let_node.mutability
        );
        let init = let_node
            .initializer
            .as_ref()
            .expect("let must have initializer");
        let TypedExpression::Binary(add) = &init.node else {
            panic!("expected Binary initializer, got {:?}", init.node);
        };
        assert!(matches!(add.op, zyntax_typed_ast::BinaryOp::Add));
    }

    /// `if count > 0 { … } else { … }` parses with comparison condition + both branches.
    #[test]
    fn parse_if_else_with_comparison() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view {
                        if count > 0 {
                            text("positive")
                        } else {
                            text("zero")
                        }
                    }
                }
                "#,
                "if_else.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let view = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();

        assert_eq!(view.statements.len(), 1);
        let TypedStatement::If(if_stmt) = &view.statements[0].node else {
            panic!("expected If, got {:?}", view.statements[0].node);
        };

        let TypedExpression::Binary(cond) = &if_stmt.condition.node else {
            panic!("expected Binary condition");
        };
        assert!(
            matches!(cond.op, zyntax_typed_ast::BinaryOp::Gt),
            "expected Gt, got {:?}",
            cond.op
        );

        assert_eq!(if_stmt.then_block.statements.len(), 1);
        let else_block = if_stmt.else_block.as_ref().expect("expected else branch");
        assert_eq!(else_block.statements.len(), 1);
    }

    /// Field separators are optional. Authors should be able to
    /// write fields one-per-line, comma-separated, or mixed —
    /// without the parser caring. Pinning all three shapes
    /// because the easiest way to regress this is to "tighten"
    /// `struct_field_tail` back to a required comma during a
    /// future refactor.
    #[test]
    fn parse_field_separators_optional() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        for (label, src) in [
            (
                "newline",
                "component A { state count: i32 state width: i32 view {} }",
            ),
            (
                "comma",
                "component A { state count: i32, state width: i32 view {} }",
            ),
            (
                "mixed",
                "component A { state count: i32, state width: i32\nstate height: i32 view {} }",
            ),
        ] {
            let program = dsl
                .parse_to_typed_ast(src, &format!("sep_{label}.blinc"))
                .unwrap_or_else(|e| panic!("parse failure for {label}: {e:?}"));
            let class = program
                .declarations
                .iter()
                .find_map(|d| match &d.node {
                    zyntax_typed_ast::TypedDeclaration::Class(c) => Some(c),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no Class for {label}"));
            let expected = if label == "mixed" { 3 } else { 2 };
            assert_eq!(
                class.fields.len(),
                expected,
                "{label}: expected {expected} fields"
            );
        }
    }

    /// `LoaderState.Loading` parses as `Field { object: Variable, field }`.
    #[test]
    fn parse_dot_namespacing_via_field_access() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state x: i32
                    view {}
                    fn step() { let v = LoaderState.Loading }
                }
                "#,
                "dot_namespacing.blinc",
            )
            .expect("parse");

        let imp = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("step"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        let TypedStatement::Let(let_node) = &body.statements[0].node else {
            panic!("expected Let");
        };
        let init = let_node.initializer.as_ref().unwrap();
        let TypedExpression::Field(field_access) = &init.node else {
            panic!(
                "expected Field access for `LoaderState.Loading`, got {:?}",
                init.node
            );
        };
        assert_eq!(
            field_access.field.resolve_global().as_deref(),
            Some("Loading")
        );
        let TypedExpression::Variable(obj_name) = &field_access.object.node else {
            panic!("expected Variable as object");
        };
        assert_eq!(obj_name.resolve_global().as_deref(), Some("LoaderState"));
    }

    /// `a && b` lowers to `Binary(_, And, _)`.
    #[test]
    fn parse_logical_and() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state x: i32
                    view {
                        if x > 0 && x < 100 { text("in range") }
                    }
                }
                "#,
                "logical_and.blinc",
            )
            .expect("parse");

        let imp = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        let TypedStatement::If(if_stmt) = &body.statements[0].node else {
            panic!("expected If");
        };
        let TypedExpression::Binary(top) = &if_stmt.condition.node else {
            panic!("expected Binary top condition");
        };
        assert!(
            matches!(top.op, zyntax_typed_ast::BinaryOp::And),
            "top op should be And, got {:?}",
            top.op
        );
        let TypedExpression::Binary(lhs) = &top.left.node else {
            panic!("LHS of And should be a comparison");
        };
        assert!(matches!(lhs.op, zyntax_typed_ast::BinaryOp::Gt));
        let TypedExpression::Binary(rhs) = &top.right.node else {
            panic!("RHS of And should be a comparison");
        };
        assert!(matches!(rhs.op, zyntax_typed_ast::BinaryOp::Lt));
    }

    /// `a || b && c` — AND binds tighter than OR.
    #[test]
    fn parse_logical_or_and_precedence() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state x: i32
                    view {
                        if x < 0 || x > 100 && x < 200 { text("either") }
                    }
                }
                "#,
                "logical_precedence.blinc",
            )
            .expect("parse");

        let imp = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        let TypedStatement::If(if_stmt) = &body.statements[0].node else {
            panic!("expected If");
        };

        let TypedExpression::Binary(top) = &if_stmt.condition.node else {
            panic!("expected Binary at top");
        };
        assert!(
            matches!(top.op, zyntax_typed_ast::BinaryOp::Or),
            "top op should be Or, got {:?}",
            top.op
        );
        let TypedExpression::Binary(rhs_and) = &top.right.node else {
            panic!("RHS of Or should be Binary(And), got {:?}", top.right.node);
        };
        assert!(
            matches!(rhs_and.op, zyntax_typed_ast::BinaryOp::And),
            "RHS top op should be And, got {:?}",
            rhs_and.op
        );
    }

    /// `count.get()` parses as `MethodCall { receiver, method, [] }`.
    #[test]
    fn parse_method_call_no_args() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view {}
                    fn step() { let v = count.get() }
                }
                "#,
                "method_call.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("step"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        let TypedStatement::Let(let_node) = &body.statements[0].node else {
            panic!("expected Let");
        };
        let init = let_node.initializer.as_ref().unwrap();
        let TypedExpression::MethodCall(call) = &init.node else {
            panic!("expected MethodCall, got {:?}", init.node);
        };
        assert_eq!(call.method.resolve_global().as_deref(), Some("get"));
        assert_eq!(call.positional_args.len(), 0);
        let TypedExpression::Variable(receiver) = &call.receiver.node else {
            panic!("expected Variable receiver");
        };
        assert_eq!(receiver.resolve_global().as_deref(), Some("count"));
    }

    /// `ctx.get(0)` — method call with one positional arg.
    #[test]
    fn parse_method_call_with_arg() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view {}
                    fn step() { let s = ctx.get(0) }
                }
                "#,
                "method_call_arg.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("step"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        let TypedStatement::Let(let_node) = &body.statements[0].node else {
            panic!("expected Let");
        };
        let init = let_node.initializer.as_ref().unwrap();
        let TypedExpression::MethodCall(call) = &init.node else {
            panic!("expected MethodCall, got {:?}", init.node);
        };
        assert_eq!(call.method.resolve_global().as_deref(), Some("get"));
        assert_eq!(call.positional_args.len(), 1);
        let TypedExpression::Literal(TypedLiteral::Integer(n)) = &call.positional_args[0].node
        else {
            panic!("expected IntLiteral arg");
        };
        assert_eq!(*n, 0);
    }

    /// `count.get() > 0` parses as `Binary(MethodCall, Gt, Int)` — postfix > binary.
    #[test]
    fn parse_method_call_in_condition() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view { if count.get() > 0 { text("pos") } }
                }
                "#,
                "mcall_in_cond.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let view = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        let TypedStatement::If(if_stmt) = &view.statements[0].node else {
            panic!("expected If");
        };
        let TypedExpression::Binary(cmp) = &if_stmt.condition.node else {
            panic!("expected Binary condition");
        };
        assert!(matches!(cmp.op, zyntax_typed_ast::BinaryOp::Gt));
        let TypedExpression::MethodCall(_) = &cmp.left.node else {
            panic!("expected MethodCall on LHS, got {:?}", cmp.left.node);
        };
    }

    /// `view([deps]) {|ctx| ...}` → `view(ctx)` with leading `__view_deps__(...)` marker.
    #[test]
    fn parse_view_with_deps() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Counter {
                    state count: i32
                    state width: i32
                    view([count, width]) {|ctx|
                        let c = ctx.get(0)
                        text("rendered")
                    }
                }
                "#,
                "view_deps.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .expect("expected Impl");
        let view = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .expect("expected view method");

        // (a) one parameter named "ctx".
        assert_eq!(
            view.params.len(),
            1,
            "expected one param, got {:?}",
            view.params
        );
        assert_eq!(view.params[0].name.resolve_global().as_deref(), Some("ctx"));

        let body = view.body.as_ref().expect("view body");
        assert!(
            body.statements.len() >= 2,
            "expected >=2 stmts (marker + user code), got {}",
            body.statements.len()
        );
        let TypedStatement::Expression(marker_node) = &body.statements[0].node else {
            panic!("expected marker stmt to be Expression");
        };
        let TypedExpression::Call(marker) = &marker_node.node else {
            panic!("expected marker to be Call, got {:?}", marker_node.node);
        };
        let TypedExpression::Variable(callee_name) = &marker.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            callee_name.resolve_global().as_deref(),
            Some("__view_deps__"),
            "marker callee should be __view_deps__"
        );
        assert_eq!(
            marker.positional_args.len(),
            2,
            "expected two deps, got {:?}",
            marker.positional_args
        );

        for (i, expected) in ["count", "width"].iter().enumerate() {
            let TypedExpression::Variable(name) = &marker.positional_args[i].node else {
                panic!("expected Variable arg at {}", i);
            };
            assert_eq!(name.resolve_global().as_deref(), Some(*expected));
        }

        let TypedStatement::Let(_) = &body.statements[1].node else {
            panic!(
                "expected user `let` stmt after marker, got {:?}",
                body.statements[1].node
            );
        };
    }

    /// Plain `view { ... }` parses as a no-param fn with no `__view_deps__` marker.
    #[test]
    fn parse_view_simple_still_works() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component Counter {
                    state count: i32
                    view { text("hi") }
                }
                "#,
                "view_simple.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let view = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap();
        assert_eq!(view.params.len(), 0);
        let body = view.body.as_ref().unwrap();
        let TypedStatement::Expression(first) = &body.statements[0].node else {
            panic!("expected Expression stmt");
        };
        let TypedExpression::Call(call) = &first.node else {
            panic!("expected Call");
        };
        let TypedExpression::Variable(callee) = &call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_ne!(
            callee.resolve_global().as_deref(),
            Some("__view_deps__"),
            "simple view shouldn't carry the deps marker"
        );
    }

    /// `if/else if/else` lowers to recursive nested-If shape.
    #[test]
    fn parse_else_if_chain() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view {
                        if count > 100 { text("big") }
                        else if count > 10 { text("medium") }
                        else { text("small") }
                    }
                }
                "#,
                "else_if_chain.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let view = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        assert_eq!(
            view.statements.len(),
            1,
            "view body holds the single outer If"
        );

        let TypedStatement::If(outer) = &view.statements[0].node else {
            panic!("expected outer If");
        };
        let outer_else = outer.else_block.as_ref().expect("outer else");
        assert_eq!(
            outer_else.statements.len(),
            1,
            "else block should hold one statement (the chained If)"
        );

        let TypedStatement::If(chained) = &outer_else.statements[0].node else {
            panic!("expected chained If as the only stmt in outer else");
        };
        let TypedExpression::Binary(cmp) = &chained.condition.node else {
            panic!("expected chained condition to be Binary");
        };
        assert!(matches!(cmp.op, zyntax_typed_ast::BinaryOp::Gt));
        let TypedExpression::Literal(TypedLiteral::Integer(n)) = &cmp.right.node else {
            panic!("expected IntLit on RHS of chained condition");
        };
        assert_eq!(*n, 10);

        let tail_else = chained.else_block.as_ref().expect("chained else (tail)");
        assert_eq!(
            tail_else.statements.len(),
            1,
            "tail else holds text(\"small\")"
        );
    }

    /// 4-arm `if/else if`-chain walks to nested depth 4.
    #[test]
    fn parse_else_if_chain_deep() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view {
                        if count > 1000 { text("a") }
                        else if count > 100 { text("b") }
                        else if count > 10 { text("c") }
                        else if count > 1 { text("d") }
                        else { text("e") }
                    }
                }
                "#,
                "else_if_deep.blinc",
            )
            .expect("parse");

        let imp = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let view_body = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();

        let mut depth = 0;
        let mut current = match &view_body.statements[0].node {
            TypedStatement::If(i) => i,
            _ => panic!("top of view body should be If"),
        };
        loop {
            depth += 1;
            let else_block = current
                .else_block
                .as_ref()
                .unwrap_or_else(|| panic!("level {depth}: expected an else"));
            if else_block.statements.len() == 1 {
                if let TypedStatement::If(next) = &else_block.statements[0].node {
                    current = next;
                    continue;
                }
            }
            break;
        }
        assert_eq!(depth, 4, "expected 4 chained Ifs before the tail else");
    }

    /// `if A { } else if B { }` — chained inner If has `else_block: None`.
    #[test]
    fn parse_else_if_no_trailing_else() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view {
                        if count > 100 { text("a") }
                        else if count > 10 { text("b") }
                    }
                }
                "#,
                "else_if_no_tail.blinc",
            )
            .expect("parse");

        let imp = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();

        let TypedStatement::If(outer) = &body.statements[0].node else {
            panic!("expected outer If");
        };
        let outer_else = outer.else_block.as_ref().expect("outer else");
        let TypedStatement::If(chained) = &outer_else.statements[0].node else {
            panic!("expected chained If");
        };
        assert!(
            chained.else_block.is_none(),
            "chained If should have no else when source omits it"
        );
    }

    /// `if … { … }` with no else has `else_block: None`.
    #[test]
    fn parse_if_no_else() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state count: i32
                    view { if count > 0 { text("yes") } }
                }
                "#,
                "if_no_else.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let view = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        let TypedStatement::If(if_stmt) = &view.statements[0].node else {
            panic!("expected If");
        };
        assert!(
            if_stmt.else_block.is_none(),
            "no-else form should leave else_block None"
        );
    }

    // FSM declaration tests.

    /// `fsm Name { … }` emits both Enum (states) and Impl (carrying `__fsm_meta__`).
    #[test]
    fn parse_fsm_emits_enum_and_impl() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Loader {
                    state Idle
                    state Loading
                    state Done
                    initial Idle
                    on Idle.Load -> Loading
                    on Loading.Finish -> Done
                }
                "#,
                "fsm_loader.blinc",
            )
            .expect("parse");

        let enum_decl = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Enum(e) => Some(e),
                _ => None,
            })
            .expect("expected Enum decl from fsm");
        assert_eq!(enum_decl.name.resolve_global().as_deref(), Some("Loader"));
        assert_eq!(
            enum_decl.variants.len(),
            3,
            "expected 3 variants (Idle, Loading, Done), got {:?}",
            enum_decl
                .variants
                .iter()
                .map(|v| v.name.resolve_global())
                .collect::<Vec<_>>()
        );
        for (i, expected) in ["Idle", "Loading", "Done"].iter().enumerate() {
            assert_eq!(
                enum_decl.variants[i].name.resolve_global().as_deref(),
                Some(*expected),
                "variant {i}"
            );
        }

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .expect("expected Impl decl from fsm");
        assert_eq!(
            impl_block.trait_name.resolve_global().as_deref(),
            Some("Loader")
        );
        assert_eq!(impl_block.methods.len(), 1, "expected one method");
        assert_eq!(
            impl_block.methods[0].name.resolve_global().as_deref(),
            Some("__fsm_meta__")
        );
    }

    /// `__fsm_meta__` body: `__fsm_begin__("FsmName")` first, `__fsm_initial__("State")`
    /// next, then transitions, `__fsm_end__()` last.
    #[test]
    fn parse_fsm_initial_marker() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Toggle {
                    state On
                    state Off
                    initial Off
                    on Off.Click -> On
                    on On.Click -> Off
                }
                "#,
                "fsm_toggle.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let meta = &impl_block.methods[0];
        let body = meta.body.as_ref().expect("__fsm_meta__ body");

        let extract = |idx: usize| -> (String, Vec<String>) {
            let TypedStatement::Expression(node) = &body.statements[idx].node else {
                panic!("expected Expression stmt at [{idx}]");
            };
            let TypedExpression::Call(call) = &node.node else {
                panic!("expected Call at [{idx}]");
            };
            let TypedExpression::Variable(callee) = &call.callee.node else {
                panic!("expected Variable callee at [{idx}]");
            };
            let str_args = call
                .positional_args
                .iter()
                .filter_map(|a| {
                    if let TypedExpression::Literal(TypedLiteral::String(s)) = &a.node {
                        s.resolve_global()
                    } else {
                        None
                    }
                })
                .collect();
            (callee.resolve_global().unwrap_or_default(), str_args)
        };

        let (begin_callee, begin_args) = extract(0);
        assert_eq!(begin_callee, "__fsm_begin__");
        assert_eq!(begin_args, vec!["Toggle".to_string()]);

        let (initial_callee, initial_args) = extract(1);
        assert_eq!(initial_callee, "__fsm_initial__");
        assert_eq!(initial_args, vec!["Off".to_string()]);

        let last = body.statements.len() - 1;
        let (end_callee, end_args) = extract(last);
        assert_eq!(end_callee, "__fsm_end__");
        assert!(end_args.is_empty(), "__fsm_end__ takes no args");
    }

    /// `on State.Event -> Next` lowers to `__fsm_transition__("State", "Event", "Next")`.
    #[test]
    fn parse_fsm_transitions() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Loader {
                    state Idle
                    state Loading
                    state Done
                    initial Idle
                    on Idle.Load -> Loading
                    on Loading.Finish -> Done
                    on Done.Reset -> Idle
                }
                "#,
                "fsm_three_transitions.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block.methods[0].body.as_ref().unwrap();

        // begin + initial + 3 transitions + end.
        assert_eq!(
            body.statements.len(),
            6,
            "expected begin + initial + 3 transitions + end, got {}",
            body.statements.len()
        );

        let expected_transitions = [
            ("Idle", "Load", "Loading"),
            ("Loading", "Finish", "Done"),
            ("Done", "Reset", "Idle"),
        ];

        for (i, (from, event, to)) in expected_transitions.iter().enumerate() {
            // [0]=begin, [1]=initial, [2..]=transitions, [last]=end.
            let stmt = &body.statements[i + 2].node;
            let TypedStatement::Expression(node) = stmt else {
                panic!("expected Expression stmt at index {}", i + 1);
            };
            let TypedExpression::Call(call) = &node.node else {
                panic!("expected Call at index {}", i + 1);
            };
            let TypedExpression::Variable(callee) = &call.callee.node else {
                panic!("expected Variable callee");
            };
            assert_eq!(
                callee.resolve_global().as_deref(),
                Some("__fsm_transition__"),
                "marker at {} should be __fsm_transition__",
                i + 1
            );
            assert_eq!(call.positional_args.len(), 3);
            for (j, expected) in [from, event, to].iter().enumerate() {
                let TypedExpression::Literal(TypedLiteral::String(s)) =
                    &call.positional_args[j].node
                else {
                    panic!(
                        "expected String arg at transition {} arg {}, got {:?}",
                        i, j, call.positional_args[j].node
                    );
                };
                assert_eq!(
                    s.resolve_global().as_deref(),
                    Some(**expected),
                    "transition {} arg {}: expected {}, got differently",
                    i,
                    j,
                    expected
                );
            }
        }
    }

    /// `tick From -> To when <expr>` lowers to `__fsm_tick__("From", <expr>, "To")`.
    #[test]
    fn parse_fsm_tick_transition() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Loader {
                    state Loading
                    state Done
                    initial Loading
                    tick Loading -> Done when progress.get() > 100
                }
                "#,
                "fsm_tick.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block.methods[0].body.as_ref().unwrap();

        // begin + initial + tick + end. Tick at body[2].
        assert_eq!(body.statements.len(), 4);
        let TypedStatement::Expression(node) = &body.statements[2].node else {
            panic!("expected Expression stmt at body[2]");
        };
        let TypedExpression::Call(call) = &node.node else {
            panic!("expected Call");
        };
        let TypedExpression::Variable(callee) = &call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            callee.resolve_global().as_deref(),
            Some("__fsm_tick__"),
            "tick marker callee"
        );
        assert_eq!(call.positional_args.len(), 3, "expected (from, guard, to)");

        let TypedExpression::Literal(TypedLiteral::String(from)) = &call.positional_args[0].node
        else {
            panic!("expected string literal arg 0");
        };
        assert_eq!(from.resolve_global().as_deref(), Some("Loading"));

        // arg 1: guard = `Binary(MethodCall, Gt, IntLiteral(100))`.
        let TypedExpression::Binary(bin) = &call.positional_args[1].node else {
            panic!(
                "expected Binary guard expression, got {:?}",
                call.positional_args[1].node
            );
        };
        assert!(
            matches!(bin.op, zyntax_typed_ast::BinaryOp::Gt),
            "guard top op should be Gt"
        );
        let TypedExpression::MethodCall(mc) = &bin.left.node else {
            panic!("guard LHS should be MethodCall");
        };
        assert_eq!(mc.method.resolve_global().as_deref(), Some("get"));
        let TypedExpression::Literal(TypedLiteral::Integer(n)) = &bin.right.node else {
            panic!("guard RHS should be IntLiteral");
        };
        assert_eq!(*n, 100);

        let TypedExpression::Literal(TypedLiteral::String(to)) = &call.positional_args[2].node
        else {
            panic!("expected string literal arg 2");
        };
        assert_eq!(to.resolve_global().as_deref(), Some("Done"));
    }

    /// Event + tick transitions coexist in `__fsm_meta__` in declaration order.
    #[test]
    fn parse_fsm_mixed_event_and_tick() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Loader {
                    state Idle
                    state Loading
                    state Done
                    initial Idle
                    on Idle.Start -> Loading
                    tick Loading -> Done when progress.get() > 100
                    on Done.Reset -> Idle
                }
                "#,
                "fsm_mixed.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block.methods[0].body.as_ref().unwrap();

        assert_eq!(body.statements.len(), 6);

        let callee_at = |idx: usize| -> String {
            let TypedStatement::Expression(node) = &body.statements[idx].node else {
                panic!("expected Expression at {idx}");
            };
            let TypedExpression::Call(call) = &node.node else {
                panic!("expected Call at {idx}");
            };
            let TypedExpression::Variable(callee) = &call.callee.node else {
                panic!("expected Variable callee at {idx}");
            };
            callee.resolve_global().unwrap_or_default()
        };

        assert_eq!(callee_at(0), "__fsm_begin__");
        assert_eq!(callee_at(1), "__fsm_initial__");
        assert_eq!(callee_at(2), "__fsm_transition__");
        assert_eq!(callee_at(3), "__fsm_tick__");
        assert_eq!(callee_at(4), "__fsm_transition__");
        assert_eq!(callee_at(5), "__fsm_end__");
    }

    /// `__fsm_meta__` body is wrapped with `__fsm_begin__("FsmName")`
    /// at the front and `__fsm_end__()` at the back so the host's
    /// stateful marker runtime knows which fsm scopes the markers
    /// in between. Pins the wrapping behaviour against future
    /// refactors that might split the `inject_fsm_context_markers`
    /// pass.
    #[test]
    fn parse_fsm_begin_end_wrapping() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Loader {
                    state Idle
                    state Loading
                    initial Idle
                    on Idle.Start -> Loading
                }
                "#,
                "fsm_begin_end.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block.methods[0].body.as_ref().unwrap();

        let TypedStatement::Expression(begin_node) = &body.statements[0].node else {
            panic!("expected Expression at body[0]");
        };
        let TypedExpression::Call(begin_call) = &begin_node.node else {
            panic!("expected Call at body[0]");
        };
        let TypedExpression::Variable(begin_callee) = &begin_call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            begin_callee.resolve_global().as_deref(),
            Some("__fsm_begin__")
        );
        let TypedExpression::Literal(TypedLiteral::String(name)) =
            &begin_call.positional_args[0].node
        else {
            panic!("expected string arg to __fsm_begin__");
        };
        assert_eq!(
            name.resolve_global().as_deref(),
            Some("Loader"),
            "__fsm_begin__ should carry the fsm's own name"
        );

        let last = body.statements.len() - 1;
        let TypedStatement::Expression(end_node) = &body.statements[last].node else {
            panic!("expected Expression at last");
        };
        let TypedExpression::Call(end_call) = &end_node.node else {
            panic!("expected Call");
        };
        let TypedExpression::Variable(end_callee) = &end_call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(end_callee.resolve_global().as_deref(), Some("__fsm_end__"));
        assert!(
            end_call.positional_args.is_empty(),
            "__fsm_end__ takes no args"
        );
    }

    /// FSM with transitions gets sibling `<FSM>Event` enum synthesised.
    #[test]
    fn synthesize_event_enum_basic() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Loader {
                    state Idle
                    state Loading
                    state Done
                    initial Idle
                    on Idle.Start -> Loading
                    on Loading.Finish -> Done
                    on Done.Reset -> Idle
                }
                "#,
                "fsm_event_enum.blinc",
            )
            .expect("parse");

        // State enum (Loader) + synthesised event enum (LoaderEvent).
        let enums: Vec<_> = program
            .declarations
            .iter()
            .filter_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Enum(e) => Some(e),
                _ => None,
            })
            .collect();
        assert_eq!(
            enums.len(),
            2,
            "expected state enum + event enum, got {}",
            enums.len()
        );

        let state_enum = enums[0];
        assert_eq!(state_enum.name.resolve_global().as_deref(), Some("Loader"));

        let event_enum = enums[1];
        assert_eq!(
            event_enum.name.resolve_global().as_deref(),
            Some("LoaderEvent"),
            "synthesised enum should be named <FSM>Event"
        );
        assert_eq!(
            event_enum.variants.len(),
            3,
            "expected 3 unique events (Start, Finish, Reset)"
        );
        for (i, expected) in ["Start", "Finish", "Reset"].iter().enumerate() {
            assert_eq!(
                event_enum.variants[i].name.resolve_global().as_deref(),
                Some(*expected),
                "variant {i} should be {expected} (declaration order preserved)"
            );
        }
    }

    /// Duplicate event names dedup to one variant (first-seen order).
    #[test]
    fn synthesize_event_enum_dedup() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Toggle {
                    state On
                    state Off
                    initial Off
                    on Off.Click -> On
                    on On.Click -> Off
                }
                "#,
                "fsm_event_dedup.blinc",
            )
            .expect("parse");

        let event_enum = program
            .declarations
            .iter()
            .filter_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Enum(e) => Some(e),
                _ => None,
            })
            .find(|e| e.name.resolve_global().as_deref() == Some("ToggleEvent"))
            .expect("expected ToggleEvent enum");

        assert_eq!(
            event_enum.variants.len(),
            1,
            "duplicate `Click` events should dedup to one variant"
        );
        assert_eq!(
            event_enum.variants[0].name.resolve_global().as_deref(),
            Some("Click")
        );
    }

    /// Tick-only FSM gets no event enum synthesised.
    #[test]
    fn synthesize_no_event_enum_for_tick_only_fsm() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                fsm Progress {
                    state Loading
                    state Done
                    initial Loading
                    tick Loading -> Done when count.get() > 100
                }
                "#,
                "fsm_tick_only.blinc",
            )
            .expect("parse");

        let enums: Vec<_> = program
            .declarations
            .iter()
            .filter_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Enum(e) => Some(e),
                _ => None,
            })
            .collect();
        assert_eq!(
            enums.len(),
            1,
            "tick-only fsm should have only the state enum, got {} enums",
            enums.len()
        );
        assert_eq!(enums[0].name.resolve_global().as_deref(), Some("Progress"));
    }

    /// FSM with no transitions parses — body has only begin/initial/end.
    #[test]
    fn parse_fsm_no_transitions() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                "fsm Status { state Open state Closed initial Open }",
                "fsm_stub.blinc",
            )
            .expect("parse");

        let enum_decl = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Enum(e) => Some(e),
                _ => None,
            })
            .unwrap();
        assert_eq!(enum_decl.variants.len(), 2);

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block.methods[0].body.as_ref().unwrap();
        assert_eq!(
            body.statements.len(),
            3,
            "stub fsm body should be begin + initial + end"
        );
    }

    // FsmRegistry data-structure tests.

    use zyntax_typed_ast::type_registry::TypeId;
    use zyntax_typed_ast::InternedString;

    fn fid(module: &str, raw_id: u32) -> FsmId {
        FsmId {
            module: InternedString::new_global(module),
            type_id: TypeId::new(raw_id),
        }
    }

    fn intern(s: &str) -> InternedString {
        InternedString::new_global(s)
    }

    /// `FsmId` distinguishes by module (different modules, same TypeId → different ids).
    #[test]
    fn fsm_id_disambiguates_by_module() {
        let a = fid("foo", 7);
        let b = fid("bar", 7);
        let c = fid("foo", 7);
        assert_ne!(a, b, "different modules → different ids");
        assert_eq!(a, c, "same (module, type_id) → equal");
    }

    /// Upsert + get round-trips initial state, transitions, tick guards.
    #[test]
    fn fsm_registry_upsert_get() {
        let mut registry = FsmRegistry::new();
        let id = fid("main", 42);

        let def = FsmDefinition {
            initial: Some(intern("Idle")),
            transitions: vec![
                EventTransition {
                    from: intern("Idle"),
                    event: intern("Start"),
                    to: intern("Loading"),
                    actions: vec![],
                },
                EventTransition {
                    from: intern("Loading"),
                    event: intern("Done"),
                    to: intern("Success"),
                    actions: vec![],
                },
            ],
            tick_guards: vec![TickGuard {
                from: intern("Loading"),
                to: intern("TimedOut"),
                guard_fn: Some(intern("__fsm_tick_guard_Loader_0__")),
            }],
            name: Some(intern("Loader")),
        };

        registry.upsert(id, def.clone());
        let got = registry.get(&id).expect("inserted entry should exist");
        assert_eq!(got.initial, Some(intern("Idle")));
        assert_eq!(got.transitions.len(), 2);
        assert_eq!(got.transitions[1].event, intern("Done"));
        assert_eq!(got.tick_guards.len(), 1);
        assert_eq!(got.name, Some(intern("Loader")));
    }

    /// Re-inserting the same id replaces the entry (second upsert wins).
    #[test]
    fn fsm_registry_upsert_replaces() {
        let mut registry = FsmRegistry::new();
        let id = fid("main", 1);

        registry.upsert(
            id,
            FsmDefinition {
                initial: Some(intern("V1")),
                ..FsmDefinition::default()
            },
        );
        registry.upsert(
            id,
            FsmDefinition {
                initial: Some(intern("V2")),
                ..FsmDefinition::default()
            },
        );

        let got = registry.get(&id).unwrap();
        assert_eq!(got.initial, Some(intern("V2")), "second upsert should win");
    }

    /// `remove` returns the prior value and `get` returns None after.
    #[test]
    fn fsm_registry_remove() {
        let mut registry = FsmRegistry::new();
        let id = fid("main", 5);
        registry.upsert(
            id,
            FsmDefinition {
                initial: Some(intern("S")),
                ..FsmDefinition::default()
            },
        );

        let removed = registry.remove(&id).expect("entry should exist");
        assert_eq!(removed.initial, Some(intern("S")));
        assert!(registry.get(&id).is_none(), "remove should drop the entry");
    }

    /// `compile_source` populates the global FsmRegistry (module + TypeId wiring).
    #[test]
    fn compile_source_populates_fsm_registry() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        // Distinct fsm name per test to avoid global-registry collisions.
        dsl.compile_source(
            r#"
            fsm RegistryProbeA {
                state Idle
                state Running
                state Done
                initial Idle
                on Idle.Begin -> Running
                on Running.Finish -> Done
            }
            "#,
            "registry_probe_a.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let probe = with_fsm_registry(|r| {
            r.iter()
                .find(|(id, def)| {
                    id.module == module
                        && def.name.and_then(|n| n.resolve_global()).as_deref()
                            == Some("RegistryProbeA")
                })
                .map(|(id, def)| (*id, def.clone()))
        });

        let (_id, def) = probe.expect("RegistryProbeA should be in the registry after compile");
        assert_eq!(
            def.initial.and_then(|n| n.resolve_global()).as_deref(),
            Some("Idle"),
            "initial state survived registry round-trip"
        );
        assert_eq!(
            def.transitions.len(),
            2,
            "expected two event-driven transitions"
        );
        assert_eq!(
            def.transitions[0].event.resolve_global().as_deref(),
            Some("Begin")
        );
        assert_eq!(
            def.transitions[1].event.resolve_global().as_deref(),
            Some("Finish")
        );
    }

    /// Tick guards lift to top-level fns named `__fsm_tick_guard_<Fsm>_<idx>__`.
    #[test]
    fn compile_source_lifts_tick_guards_to_functions() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        // Two guards on same fsm to exercise the index suffix.
        let function_names = dsl
            .compile_source(
                r#"
                fsm GuardLiftProbe {
                    state Loading
                    state Done
                    state Failed
                    initial Loading
                    tick Loading -> Done when 1 > 0
                    tick Loading -> Failed when 1 < 0
                }
                "#,
                "guard_lift.blinc",
            )
            .expect("compile");

        assert!(
            function_names
                .iter()
                .any(|n| n == "__fsm_tick_guard_GuardLiftProbe_0__"),
            "expected guard 0 function in compiled symbols, got {:?}",
            function_names
        );
        assert!(
            function_names
                .iter()
                .any(|n| n == "__fsm_tick_guard_GuardLiftProbe_1__"),
            "expected guard 1 function in compiled symbols, got {:?}",
            function_names
        );

        let module = InternedString::new_global("main");
        let def = with_fsm_registry(|r| {
            r.iter()
                .find(|(id, def)| {
                    id.module == module
                        && def.name.and_then(|n| n.resolve_global()).as_deref()
                            == Some("GuardLiftProbe")
                })
                .map(|(_, def)| def.clone())
        })
        .expect("GuardLiftProbe should be registered");

        assert_eq!(def.tick_guards.len(), 2);
        assert_eq!(
            def.tick_guards[0]
                .guard_fn
                .and_then(|n| n.resolve_global())
                .as_deref(),
            Some("__fsm_tick_guard_GuardLiftProbe_0__")
        );
        assert_eq!(
            def.tick_guards[1]
                .guard_fn
                .and_then(|n| n.resolve_global())
                .as_deref(),
            Some("__fsm_tick_guard_GuardLiftProbe_1__")
        );
    }

    /// Tick guards land in the registry alongside event transitions.
    #[test]
    fn compile_source_records_tick_guards_in_registry() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm RegistryProbeB {
                state Loading
                state Done
                initial Loading
                tick Loading -> Done when count.get() > 100
            }
            "#,
            "registry_probe_b.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let probe = with_fsm_registry(|r| {
            r.iter()
                .find(|(id, def)| {
                    id.module == module
                        && def.name.and_then(|n| n.resolve_global()).as_deref()
                            == Some("RegistryProbeB")
                })
                .map(|(_, def)| def.clone())
        });

        let def = probe.expect("RegistryProbeB should be in the registry");
        assert_eq!(def.tick_guards.len(), 1);
        assert_eq!(
            def.tick_guards[0].from.resolve_global().as_deref(),
            Some("Loading")
        );
        assert_eq!(
            def.tick_guards[0].to.resolve_global().as_deref(),
            Some("Done")
        );
        assert_eq!(
            def.transitions.len(),
            0,
            "tick-only fsm has no event transitions"
        );
    }

    /// Dispatch round-trip: compile fsm, find by name, walk full transition cycle.
    #[test]
    fn dispatch_round_trip() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm DispatchProbe {
                state Idle
                state Loading
                state Done
                initial Idle
                on Idle.Start -> Loading
                on Loading.Finish -> Done
                on Done.Reset -> Idle
            }
            "#,
            "dispatch_probe.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");

        let id = with_fsm_registry(|r| r.find_by_name(module, "DispatchProbe"))
            .expect("DispatchProbe should be in the registry");

        // Idle --Start--> Loading
        let next = with_fsm_registry(|r| r.step_event(&id, "Idle", "Start"));
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Loading")
        );

        // Loading --Finish--> Done
        let next = with_fsm_registry(|r| r.step_event(&id, "Loading", "Finish"));
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Done")
        );

        // Done --Reset--> Idle (full cycle)
        let next = with_fsm_registry(|r| r.step_event(&id, "Done", "Reset"));
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Idle")
        );
    }

    /// Non-matching dispatches return `None` (unknown event, wrong from, phantom id).
    #[test]
    fn dispatch_misses_return_none() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm DispatchMissProbe {
                state On
                state Off
                initial Off
                on Off.Click -> On
                on On.Click -> Off
            }
            "#,
            "dispatch_miss.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "DispatchMissProbe"))
            .expect("DispatchMissProbe should be in the registry");

        let miss = with_fsm_registry(|r| r.step_event(&id, "Off", "DoesNotExist"));
        assert!(miss.is_none(), "unknown event should miss");

        let miss = with_fsm_registry(|r| r.step_event(&id, "Nowhere", "Click"));
        assert!(miss.is_none(), "wrong from-state should miss");

        let phantom = FsmId {
            module,
            type_id: TypeId::new(u32::MAX),
        };
        let miss = with_fsm_registry(|r| r.step_event(&phantom, "Off", "Click"));
        assert!(miss.is_none(), "phantom fsm id should miss");
    }

    /// Serialiser for tests that share the process-wide `GuardDispatcher` slot.
    static BRIDGE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// DSL components publish into the runtime-agnostic `component` registry.
    #[test]
    fn publish_components_to_runtime_registry_round_trip() {
        let _ = tracing_subscriber::fmt::try_init();
        // Serialise against parallel global-registry writes from other tests.
        let _guard = BRIDGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            component RegistryRoundTripCounter (initial: i32, step: i32) {
                view { text("counter") }
            }
            component RegistryRoundTripGreeting {
                view { text("hi") }
            }
            view { RegistryRoundTripCounter(1, 2) }
            "#,
            "component_registry_round_trip.blinc",
        )
        .expect("compile");

        // Unique names avoid parallel-test races on the global registry.
        let counter = blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name("RegistryRoundTripCounter").cloned()
        })
        .expect("RegistryRoundTripCounter should be published");
        assert_eq!(
            counter.view_symbol.as_ref(),
            "RegistryRoundTripCounter$view"
        );
        assert_eq!(counter.prop_count(), 2);
        assert_eq!(counter.props[0].name.as_ref(), "initial");
        assert_eq!(
            counter.props[0].ty,
            blinc_runtime::component::Type::Primitive(PrimitiveType::I32)
        );
        assert_eq!(counter.props[1].name.as_ref(), "step");
        assert_eq!(
            counter.props[1].ty,
            blinc_runtime::component::Type::Primitive(PrimitiveType::I32)
        );

        let greeting = blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name("RegistryRoundTripGreeting").cloned()
        })
        .expect("RegistryRoundTripGreeting should be published");
        assert_eq!(
            greeting.view_symbol.as_ref(),
            "RegistryRoundTripGreeting$view"
        );
        assert_eq!(greeting.prop_count(), 0);
    }

    /// Widget consumer renders via `Arc<dyn ViewRenderer>` without a `BlincDsl` ref.
    #[test]
    fn jit_view_renderer_round_trip() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            component Greeting { view { text("hello via renderer") } }
            view { Greeting() }
            "#,
            "renderer_round_trip.blinc",
        )
        .expect("compile");

        let renderer: std::sync::Arc<dyn blinc_runtime::view::ViewRenderer> = dsl.view_renderer();

        let main_value = blinc_runtime::view::render_main(&renderer).expect("render_main");
        assert_eq!(main_value, ZyntaxValue::Void);

        let comp_value =
            blinc_runtime::view::render_component(&renderer, "Greeting").expect("render_component");
        assert_eq!(comp_value, ZyntaxValue::Void);

        // Unknown component surfaces as `Backend` (Cranelift symbol resolution).
        let err = blinc_runtime::view::render_component(&renderer, "DoesNotExist")
            .expect_err("unknown component should error");
        assert!(
            matches!(err, blinc_runtime::view::ViewRenderError::Backend(_)),
            "JIT path surfaces missing symbols as Backend errors, got {err:?}"
        );
    }

    /// `view { Text("hi") }` compiles to the value-returning
    /// shape: the substrate ViewRenderer returns a non-zero
    /// `ZyntaxValue::Int(handle)`, which decodes via
    /// [`materialize_widget`] back to a `WidgetBox::Text` whose
    /// `content` matches the source. Pins the full Phase 2
    /// round-trip: AST rewrite → JIT-i64-return ABI → host-side
    /// box reclamation.
    #[test]
    fn jit_view_renderer_round_trip_value_returning_text() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(r#"view { Text("hello") }"#, "value_returning_text.blinc")
            .expect("compile");

        let renderer: std::sync::Arc<dyn blinc_runtime::view::ViewRenderer> = dsl.view_renderer();
        let value = blinc_runtime::view::render_main(&renderer).expect("render_main");

        let ZyntaxValue::Int(handle) = value else {
            panic!("expected ZyntaxValue::Int(handle), got: {value:?}");
        };
        assert_ne!(handle, 0, "Text view should not return the null handle");

        // SAFETY: handle came straight out of `$Blinc$Text$view`,
        // which uses `Box::into_raw(Box::new(WidgetBox::Text(...)))`
        // and hands the pointer back unchanged through `i64`.
        let widget =
            unsafe { materialize_widget(handle) }.expect("non-null handle should decode to Some");
        let WidgetBox::Text(text) = *widget else {
            panic!("expected WidgetBox::Text, got Div");
        };
        // `Text::new` stores the content via its public surface;
        // pull it back out through the `content()` getter.
        assert_eq!(
            text.content(),
            "hello",
            "Text widget should carry the source string"
        );
    }

    /// Rust→DSL: `register_extern_widget` makes a Rust widget callable from DSL.
    #[test]
    fn register_extern_widget_rust_to_dsl_round_trip() {
        let _ = tracing_subscriber::fmt::try_init();

        // Mirrors what `#[extern_widget]` would generate.
        extern "C" fn fancy_text_view(content_ptr: *const i32) -> i64 {
            if content_ptr.is_null() {
                return 0;
            }
            // SAFETY: length-prefixed String buffer per param type.
            let content = unsafe { blinc_string_decode(content_ptr) };
            let widget = blinc_layout::text::Text::new(content);
            Box::into_raw(Box::new(WidgetBox::Custom(Box::new(widget)))) as i64
        }

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.register_extern_widget_spec(ExternWidgetSpec {
            name: "FancyText".into(),
            view_symbol: "$Blinc$FancyText$view".into(),
            props: vec![blinc_runtime::component::PropDef {
                name: std::sync::Arc::from("content"),
                ty: Type::Primitive(PrimitiveType::String),
            }],
            param_types: vec![Type::Primitive(PrimitiveType::String)],
            return_type: Type::Primitive(PrimitiveType::I64),
            extern_ptr: fancy_text_view as *const u8,
        })
        .expect("register_extern_widget_spec");

        dsl.compile_source(
            r#"view { FancyText("registered widget") }"#,
            "fancy_text.blinc",
        )
        .expect("compile");

        let renderer: std::sync::Arc<dyn blinc_runtime::view::ViewRenderer> = dsl.view_renderer();
        let value = blinc_runtime::view::render_main(&renderer).expect("render_main");

        let ZyntaxValue::Int(handle) = value else {
            panic!("expected ZyntaxValue::Int(handle), got: {value:?}");
        };
        assert_ne!(handle, 0, "FancyText extern should return a real handle");

        // SAFETY: handle from fancy_text_view → `WidgetBox::Custom(Box::new(Text))`.
        let widget =
            unsafe { materialize_widget(handle) }.expect("non-null handle should decode to Some");
        assert!(
            matches!(*widget, WidgetBox::Custom(_)),
            "expected WidgetBox::Custom"
        );
    }

    /// DSL→Rust: `dsl.query(...)` returns a `Box<dyn ElementBuilder>` for a DSL component.
    #[test]
    fn query_dsl_component_returns_element_builder() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            component MyContainer {
                view { Div() }
            }
            "#,
            "query_container.blinc",
        )
        .expect("compile");

        let widget = dsl.query("MyContainer", &[]).expect("query");
        assert_eq!(
            widget.element_type_id(),
            blinc_layout::div::ElementTypeId::Div,
            "queried widget should report as a Div"
        );
    }

    /// `query()` errors on Unit-returning views (only value-returning are queryable).
    #[test]
    fn query_legacy_unit_returning_component_errors() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            component LegacyGreeting {
                view { text("hi") }
            }
            "#,
            "legacy_greeting.blinc",
        )
        .expect("compile");

        // `Box<dyn ElementBuilder>` isn't `Debug` — use `.err()`, not `expect_err`.
        let err = dsl
            .query("LegacyGreeting", &[])
            .err()
            .expect("Unit-returning component should error");
        let msg = format!("{err}");
        assert!(
            msg.contains("isn't value-returning"),
            "diagnostic should explain the contract, got: {msg}"
        );
    }

    /// `query()` on an unknown name returns a clear diagnostic.
    #[test]
    fn query_unknown_component_errors_with_helpful_message() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let err = dsl
            .query("DoesNotExist", &[])
            .err()
            .expect("unknown component should error");
        let msg = format!("{err}");
        assert!(
            msg.contains("no component named"),
            "diagnostic should name the missing component, got: {msg}"
        );
    }

    /// End-to-end: `Div { Text() Text() }` produces a Div with two Text children.
    #[test]
    fn jit_view_renderer_div_with_text_children_composes() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            view {
                Div() {
                    Text("first")
                    Text("second")
                }
            }
            "#,
            "div_with_children.blinc",
        )
        .expect("compile");

        let renderer: std::sync::Arc<dyn blinc_runtime::view::ViewRenderer> = dsl.view_renderer();
        let value = blinc_runtime::view::render_main(&renderer).expect("render_main");

        let ZyntaxValue::Int(handle) = value else {
            panic!("expected widget handle, got: {value:?}");
        };
        assert_ne!(handle, 0, "Div view should return a real handle");

        // Div lands in `Custom(Styled<Div>)`; Styled forwards `children_builders`.
        let widget = unsafe { materialize_widget(handle) }.expect("non-null handle");
        let WidgetBox::Custom(builder) = *widget else {
            panic!("expected WidgetBox::Custom (Styled<Div>)");
        };
        assert_eq!(builder.children_builders().len(), 2);
    }

    /// `Div { Div { Text } }` round-trips — each nesting level mints its own child-list.
    #[test]
    fn jit_view_renderer_div_nested_div_composes() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            view {
                Div() {
                    Div() {
                        Text("inner")
                    }
                }
            }
            "#,
            "div_nested.blinc",
        )
        .expect("compile");

        let renderer: std::sync::Arc<dyn blinc_runtime::view::ViewRenderer> = dsl.view_renderer();
        let value = blinc_runtime::view::render_main(&renderer).expect("render_main");
        let ZyntaxValue::Int(handle) = value else {
            panic!("expected widget handle, got: {value:?}");
        };
        assert_ne!(handle, 0);

        let widget = unsafe { materialize_widget(handle) }.expect("non-null handle");
        let WidgetBox::Custom(outer) = *widget else {
            panic!("outer should be a Custom(Styled<Div>)");
        };
        let outer_children = outer.children_builders();
        assert_eq!(outer_children.len(), 1, "outer Div should have 1 child");

        // Inner is a Styled<Div>; `element_type_id` delegates to the inner Div.
        let inner = &outer_children[0];
        assert_eq!(
            inner.element_type_id(),
            blinc_layout::div::ElementTypeId::Div,
            "inner child should report itself as a Div"
        );
        assert_eq!(inner.children_builders().len(), 1);
    }

    #[test]
    fn div_with_inline_styling_args_applies_overlay() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"view { Div(bg = 16711680, opacity = 0.5) }"#,
            "div_styled.blinc",
        )
        .expect("compile");

        let renderer: std::sync::Arc<dyn blinc_runtime::view::ViewRenderer> = dsl.view_renderer();
        let value = blinc_runtime::view::render_main(&renderer).expect("render_main");
        let ZyntaxValue::Int(handle) = value else {
            panic!("expected widget handle, got: {value:?}");
        };
        assert_ne!(handle, 0);

        let widget = unsafe { materialize_widget(handle) }.expect("non-null handle");
        let WidgetBox::Custom(builder) = *widget else {
            panic!("expected Custom(Styled<Div>)");
        };
        let props = builder.render_props();
        assert_eq!(props.opacity, 0.5);
        assert!(props.background.is_some(), "background should be set");
        if let Some(blinc_core::layer::Brush::Solid(c)) = props.background {
            // 16711680 = 0xFF0000 = red. Color has no PartialEq, so compare channels.
            assert!((c.r - 1.0).abs() < 0.01);
            assert!(c.g.abs() < 0.01);
            assert!(c.b.abs() < 0.01);
        } else {
            panic!("background should be a solid brush");
        }
    }

    #[test]
    fn styled_wrapper_overlays_specified_fields_only() {
        use blinc_layout::ElementBuilder;
        let text = blinc_layout::text::Text::new("hi");
        let base_props = text.render_props();

        let overlay = RenderPropsOverlay {
            opacity: Some(0.5),
            corner_radius: Some(8.0),
            ..Default::default()
        };
        let merged = Styled::new(text, overlay).render_props();

        assert_eq!(merged.opacity, 0.5);
        assert_eq!(
            merged.border_radius,
            blinc_core::layer::CornerRadius::new(8.0, 8.0, 8.0, 8.0)
        );
        assert!(merged.border_radius_explicit);
        // Brush has no PartialEq — compare is_some().
        assert_eq!(merged.background.is_some(), base_props.background.is_some());
        assert_eq!(merged.border_width, base_props.border_width);
        assert_eq!(merged.border_color, base_props.border_color);
    }

    #[test]
    fn styled_wrapper_default_overlay_is_noop() {
        use blinc_layout::ElementBuilder;
        let text = blinc_layout::text::Text::new("hi");
        let base_props = text.render_props();
        let merged = Styled::new(text, RenderPropsOverlay::default()).render_props();

        assert_eq!(merged.opacity, base_props.opacity);
        assert_eq!(merged.background.is_some(), base_props.background.is_some());
        assert_eq!(merged.border_radius, base_props.border_radius);
        assert_eq!(merged.border_width, base_props.border_width);
        assert_eq!(merged.border_color, base_props.border_color);
    }

    /// `view { Div() }` returns a non-zero handle decoding to `Custom(Styled<Div>)`.
    #[test]
    fn jit_view_renderer_round_trip_value_returning_div() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(r#"view { Div() }"#, "value_returning_div.blinc")
            .expect("compile");

        let renderer: std::sync::Arc<dyn blinc_runtime::view::ViewRenderer> = dsl.view_renderer();
        let value = blinc_runtime::view::render_main(&renderer).expect("render_main");

        let ZyntaxValue::Int(handle) = value else {
            panic!("expected ZyntaxValue::Int(handle), got: {value:?}");
        };
        assert_ne!(handle, 0, "Div view should not return the null handle");

        // Div wraps itself in `Styled<Div>` → Custom variant.
        let widget =
            unsafe { materialize_widget(handle) }.expect("non-null handle should decode to Some");
        assert!(
            matches!(*widget, WidgetBox::Custom(_)),
            "expected WidgetBox::Custom (Styled<Div>)"
        );
    }

    /// DSL FSM round-trips through the runtime-agnostic `blinc_runtime::fsm`.
    #[test]
    fn publish_to_runtime_registry_round_trip() {
        let _ = tracing_subscriber::fmt::try_init();
        // Serialise against other bridge-dispatching tests.
        let _guard = BRIDGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm Loader {
                state Idle
                state Loading
                state Done
                initial Idle
                on Idle.Start -> Loading
                on Loading.Finish -> Done
                tick Loading -> Done when 1 > 0
            }
            "#,
            "loader_runtime_bridge.blinc",
        )
        .expect("compile");

        // Install the JIT dispatcher AFTER compile so the guard
        // symbol is registered in this dsl's runtime before any
        // bridge dispatch hits it. Under the BRIDGE_TEST_LOCK,
        // no other bridge test can overwrite the slot while we
        // dispatch.
        dsl.install_runtime_bridge();

        // The FSM should be registered in the runtime substrate
        // under its DSL name.
        let state = blinc_runtime::fsm::FsmStateId::from_fsm_name("Loader")
            .expect("Loader should be published to blinc_runtime substrate");

        // Codes should reflect declaration order: Idle = 0,
        // Loading = 1, Done = 2.
        assert_eq!(state.variant, 0, "initial state should be Idle (code 0)");
        assert_eq!(state.state_name().as_deref(), Some("Idle"));

        // Event dispatch: Idle + Start → Loading. Event codes
        // are first-appearance order (Start = 0, Finish = 1),
        // offset by `FSM_EVENT_CODE_OFFSET` so they can't collide
        // with widget pointer-event codes.
        use blinc_runtime::blinc_layout::stateful::StateTransitions;
        let start_code = blinc_runtime::fsm::FSM_EVENT_CODE_OFFSET;
        let loading = state
            .on_event(start_code)
            .expect("Idle + Start should transition");
        assert_eq!(loading.variant, 1);
        assert_eq!(loading.state_name().as_deref(), Some("Loading"));

        // Tick dispatch: Loading + (1 > 0 always fires) → Done.
        // Routes through the JitGuardDispatcher installed by
        // BlincDsl::new(), which JIT-calls the lifted guard fn.
        let done = loading.on_tick().expect("guard `1 > 0` should fire");
        assert_eq!(done.variant, 2);
        assert_eq!(done.state_name().as_deref(), Some("Done"));
    }

    /// Tick dispatch fires when guard returns true.
    #[test]
    fn step_tick_fires_when_guard_true() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm StepTickTrue {
                state Loading
                state Done
                initial Loading
                tick Loading -> Done when 1 > 0
            }
            "#,
            "step_tick_true.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "StepTickTrue"))
            .expect("StepTickTrue should be registered");

        let next = dsl.step_tick(&id, "Loading").expect("step_tick call");
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Done"),
            "guard `1 > 0` should fire and transition Loading → Done"
        );
    }

    /// Tick dispatch returns `None` when guard returns false.
    #[test]
    fn step_tick_no_transition_when_guard_false() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm StepTickFalse {
                state Loading
                state Done
                initial Loading
                tick Loading -> Done when 1 < 0
            }
            "#,
            "step_tick_false.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "StepTickFalse")).unwrap();

        let next = dsl.step_tick(&id, "Loading").expect("step_tick call");
        assert!(
            next.is_none(),
            "guard `1 < 0` should not fire — got {next:?}"
        );
    }

    /// First true guard wins (declaration order, short-circuit).
    #[test]
    fn step_tick_first_true_guard_wins() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        // First guard always true so we can verify the second never fires.
        dsl.compile_source(
            r#"
            fsm StepTickPriority {
                state Loading
                state Failed
                state Done
                initial Loading
                tick Loading -> Failed when 1 > 0
                tick Loading -> Done when 1 > 0
            }
            "#,
            "step_tick_priority.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "StepTickPriority")).unwrap();

        let next = dsl.step_tick(&id, "Loading").expect("step_tick call");
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Failed"),
            "first declared guard should fire (Loading → Failed), not Loading → Done"
        );
    }

    /// No matching from-state → `None` (covers both "no rules" and "phantom state").
    #[test]
    fn step_tick_no_matching_from_state() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm StepTickNoMatch {
                state Loading
                state Done
                initial Loading
                tick Loading -> Done when 1 > 0
            }
            "#,
            "step_tick_no_match.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "StepTickNoMatch")).unwrap();

        let from_done = dsl.step_tick(&id, "Done").expect("step_tick");
        assert!(from_done.is_none(), "Done has no tick rules");

        let from_phantom = dsl.step_tick(&id, "DoesNotExist").expect("step_tick");
        assert!(from_phantom.is_none(), "phantom from-state should miss");
    }

    // Signal-resolved guard tests.

    /// `count.get()` (i32) lowers to `__signal_get_i32("count")`.
    #[test]
    fn signal_get_rewrites_to_typed_extern_i32() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                signal count: i32
                fsm SignalProbeI32 {
                    state Idle
                    state Hot
                    initial Idle
                    tick Idle -> Hot when count.get() > 100
                }
                "#,
                "signal_i32.blinc",
            )
            .expect("parse");

        // Signal decl stripped — no top-level fn `count`.
        let has_signal_decl = program.declarations.iter().any(|d| {
            let zyntax_typed_ast::TypedDeclaration::Function(f) = &d.node else {
                return false;
            };
            f.name.resolve_global().as_deref() == Some("count")
        });
        assert!(
            !has_signal_decl,
            "signal-marker decl should be stripped before compile"
        );

        // Rewrite lands inside `__fsm_meta__`'s `__fsm_tick__` marker (arg[1] = guard).
        // Function lifting only runs in compile_source, not parse_to_typed_ast.
        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i)
                    if i.trait_name.resolve_global().as_deref() == Some("SignalProbeI32") =>
                {
                    Some(i)
                }
                _ => None,
            })
            .expect("SignalProbeI32 Impl");
        let meta = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
            .expect("__fsm_meta__ method");
        let body = meta.body.as_ref().expect("__fsm_meta__ body");

        let tick_call = body
            .statements
            .iter()
            .find_map(|s| {
                let TypedStatement::Expression(e) = &s.node else {
                    return None;
                };
                let TypedExpression::Call(c) = &e.node else {
                    return None;
                };
                let TypedExpression::Variable(callee) = &c.callee.node else {
                    return None;
                };
                if callee.resolve_global().as_deref() == Some("__fsm_tick__") {
                    Some(c)
                } else {
                    None
                }
            })
            .expect("__fsm_tick__ marker should exist");
        let guard = &tick_call.positional_args[1];
        let TypedExpression::Binary(cmp) = &guard.node else {
            panic!("guard should be Binary, got {:?}", guard.node);
        };
        let TypedExpression::Call(call) = &cmp.left.node else {
            panic!(
                "guard LHS should be Call after rewrite, got {:?}",
                cmp.left.node
            );
        };
        let TypedExpression::Variable(callee) = &call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            callee.resolve_global().as_deref(),
            Some("__signal_get_i32"),
            "signal call should rewrite to __signal_get_i32"
        );
        assert_eq!(call.positional_args.len(), 1);
        let TypedExpression::Literal(TypedLiteral::String(name)) = &call.positional_args[0].node
        else {
            panic!("expected string-literal name arg");
        };
        assert_eq!(name.resolve_global().as_deref(), Some("count"));
    }

    /// `name.get()` (string) lowers to `__signal_get_string("name")`.
    #[test]
    fn signal_get_rewrites_to_typed_extern_string() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                signal username: string
                component C {
                    state x: i32
                    view {}
                    fn step() { let s = username.get() }
                }
                "#,
                "signal_string.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .expect("Impl block expected");
        let step = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("step"))
            .unwrap();
        let body = step.body.as_ref().unwrap();
        let TypedStatement::Let(let_node) = &body.statements[0].node else {
            panic!("expected Let stmt");
        };
        let init = let_node.initializer.as_ref().expect("let init");
        let TypedExpression::Call(sig_call) = &init.node else {
            panic!("let init should be Call after rewrite, got {:?}", init.node);
        };
        let TypedExpression::Variable(callee) = &sig_call.callee.node else {
            panic!("signal callee should be Variable");
        };
        assert_eq!(
            callee.resolve_global().as_deref(),
            Some("__signal_get_string"),
            "string-typed signal should rewrite to __signal_get_string"
        );
    }

    /// Multiple signals — each rewrites based on its declared type.
    #[test]
    fn multiple_signals_rewrite_independently() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                signal count: i32
                signal label: string
                fsm MultiSignalProbe {
                    state Idle
                    state Hot
                    initial Idle
                    tick Idle -> Hot when count.get() > 0
                }
                component C {
                    state x: i32
                    view {}
                    fn step() { let s = label.get() }
                }
                "#,
                "multi_signals.blinc",
            )
            .expect("parse");

        // Both signal markers stripped.
        let strays: Vec<_> = program
            .declarations
            .iter()
            .filter_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Function(f) => {
                    let name = f.name.resolve_global();
                    if matches!(name.as_deref(), Some("count") | Some("label")) {
                        Some(name)
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .collect();
        assert!(
            strays.is_empty(),
            "signal markers should all be stripped, got strays: {strays:?}"
        );

        // Each signal has its expected extern in some call somewhere.
        fn callee_exists(program: &TypedProgram, callee: &str) -> bool {
            fn walk_expr(e: &zyntax_typed_ast::TypedNode<TypedExpression>, callee: &str) -> bool {
                match &e.node {
                    TypedExpression::Call(c) => {
                        if let TypedExpression::Variable(name) = &c.callee.node {
                            if name.resolve_global().as_deref() == Some(callee) {
                                return true;
                            }
                        }
                        c.positional_args.iter().any(|a| walk_expr(a, callee))
                            || walk_expr(&c.callee, callee)
                    }
                    TypedExpression::Binary(b) => {
                        walk_expr(&b.left, callee) || walk_expr(&b.right, callee)
                    }
                    _ => false,
                }
            }
            fn walk_stmt(s: &zyntax_typed_ast::TypedNode<TypedStatement>, callee: &str) -> bool {
                match &s.node {
                    TypedStatement::Expression(e) => walk_expr(e, callee),
                    TypedStatement::Let(l) => l
                        .initializer
                        .as_ref()
                        .map(|init| walk_expr(init, callee))
                        .unwrap_or(false),
                    TypedStatement::If(i) => {
                        walk_expr(&i.condition, callee)
                            || i.then_block.statements.iter().any(|s| walk_stmt(s, callee))
                            || i.else_block
                                .as_ref()
                                .is_some_and(|b| b.statements.iter().any(|s| walk_stmt(s, callee)))
                    }
                    TypedStatement::Return(Some(e)) => walk_expr(e, callee),
                    _ => false,
                }
            }
            program.declarations.iter().any(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Function(f) => f
                    .body
                    .as_ref()
                    .map(|b| b.statements.iter().any(|s| walk_stmt(s, callee)))
                    .unwrap_or(false),
                zyntax_typed_ast::TypedDeclaration::Impl(i) => i.methods.iter().any(|m| {
                    m.body
                        .as_ref()
                        .map(|b| b.statements.iter().any(|s| walk_stmt(s, callee)))
                        .unwrap_or(false)
                }),
                _ => false,
            })
        }

        assert!(
            callee_exists(&program, "__signal_get_i32"),
            "i32 signal should produce __signal_get_i32 call"
        );
        assert!(
            callee_exists(&program, "__signal_get_string"),
            "string signal should produce __signal_get_string call"
        );
    }

    // Host-machinery + end-to-end signal-guard tests.

    /// `signal=200, guard >100` → tick fires.
    #[test]
    fn signal_guard_fires_when_above_threshold() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.set_signal_i32("e2e_above", 200);
        dsl.compile_source(
            r#"
            signal e2e_above: i32
            fsm SignalGuardAbove {
                state Idle
                state Hot
                initial Idle
                tick Idle -> Hot when e2e_above.get() > 100
            }
            "#,
            "signal_e2e_above.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "SignalGuardAbove"))
            .expect("SignalGuardAbove should be registered");

        let next = dsl.step_tick(&id, "Idle").expect("step_tick");
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Hot"),
            "guard `e2e_above.get() > 100` (200) should fire"
        );
    }

    /// `signal=50, guard >100` → tick doesn't fire.
    #[test]
    fn signal_guard_no_fire_when_below_threshold() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.set_signal_i32("e2e_below", 50);
        dsl.compile_source(
            r#"
            signal e2e_below: i32
            fsm SignalGuardBelow {
                state Idle
                state Hot
                initial Idle
                tick Idle -> Hot when e2e_below.get() > 100
            }
            "#,
            "signal_e2e_below.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "SignalGuardBelow")).unwrap();

        let next = dsl.step_tick(&id, "Idle").expect("step_tick");
        assert!(
            next.is_none(),
            "guard `e2e_below.get() > 100` (50) should not fire — got {next:?}"
        );
    }

    /// Signal table is read at JIT time, not snapshot at compile — mutations are visible.
    #[test]
    fn signal_guard_reflects_updated_value() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.set_signal_i32("e2e_mut", 0);
        dsl.compile_source(
            r#"
            signal e2e_mut: i32
            fsm SignalGuardMut {
                state Idle
                state Hot
                initial Idle
                tick Idle -> Hot when e2e_mut.get() > 100
            }
            "#,
            "signal_e2e_mut.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "SignalGuardMut")).unwrap();

        assert!(dsl.step_tick(&id, "Idle").unwrap().is_none());

        dsl.set_signal_i32("e2e_mut", 999);

        let next = dsl.step_tick(&id, "Idle").expect("step_tick");
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Hot"),
            "after raising the signal, the guard should fire"
        );
    }

    // Float-literal + f64-signal tests.

    /// `1.5` parses as `TypedLiteral::Float(f64)`.
    #[test]
    fn parse_float_literal() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                component C {
                    state x: f64
                    view {}
                    fn step() { let p = 1.5 }
                }
                "#,
                "float_literal.blinc",
            )
            .expect("parse");

        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .unwrap();
        let body = impl_block
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("step"))
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        let TypedStatement::Let(let_node) = &body.statements[0].node else {
            panic!("expected Let");
        };
        let init = let_node.initializer.as_ref().unwrap();
        let TypedExpression::Literal(TypedLiteral::Float(v)) = &init.node else {
            panic!("expected Float literal, got {:?}", init.node);
        };
        assert!((*v - 1.5_f64).abs() < f64::EPSILON, "expected 1.5, got {v}");
    }

    /// `-0.25` and `1e3` both parse via the same `float` rule.
    #[test]
    fn parse_float_literal_signed_and_scientific() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        for (label, src, expected) in [
            ("negative", "-0.25", -0.25_f64),
            ("scientific", "1e3", 1000.0_f64),
        ] {
            let program = dsl
                .parse_to_typed_ast(
                    &format!(
                        "component C {{ state x: f64 view {{}} fn step() {{ let p = {src} }} }}"
                    ),
                    &format!("float_{label}.blinc"),
                )
                .unwrap_or_else(|e| panic!("{label}: {e:?}"));
            let imp = program
                .declarations
                .iter()
                .find_map(|d| match &d.node {
                    zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                    _ => None,
                })
                .unwrap();
            let body = imp
                .methods
                .iter()
                .find(|m| m.name.resolve_global().as_deref() == Some("step"))
                .unwrap()
                .body
                .as_ref()
                .unwrap();
            let TypedStatement::Let(let_node) = &body.statements[0].node else {
                panic!("{label}: expected Let");
            };
            let init = let_node.initializer.as_ref().unwrap();
            let TypedExpression::Literal(TypedLiteral::Float(v)) = &init.node else {
                panic!("{label}: expected Float, got {:?}", init.node);
            };
            assert!(
                (*v - expected).abs() < 1e-9,
                "{label}: expected {expected}, got {v}"
            );
        }
    }

    /// `signal progress: f64` + `progress.get()` → `__signal_get_f64("progress")`.
    #[test]
    fn signal_get_rewrites_to_typed_extern_f64() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                r#"
                signal progress: f64
                fsm SignalProbeF64 {
                    state Loading
                    state Done
                    initial Loading
                    tick Loading -> Done when progress.get() >= 1.0
                }
                "#,
                "signal_f64.blinc",
            )
            .expect("parse");

        let imp = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i)
                    if i.trait_name.resolve_global().as_deref() == Some("SignalProbeF64") =>
                {
                    Some(i)
                }
                _ => None,
            })
            .expect("SignalProbeF64 Impl");
        let meta = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("__fsm_meta__"))
            .unwrap();
        let body = meta.body.as_ref().unwrap();

        let tick_call = body
            .statements
            .iter()
            .find_map(|s| {
                let TypedStatement::Expression(e) = &s.node else {
                    return None;
                };
                let TypedExpression::Call(c) = &e.node else {
                    return None;
                };
                let TypedExpression::Variable(callee) = &c.callee.node else {
                    return None;
                };
                (callee.resolve_global().as_deref() == Some("__fsm_tick__")).then_some(c)
            })
            .expect("__fsm_tick__ marker");

        let guard = &tick_call.positional_args[1];
        let TypedExpression::Binary(cmp) = &guard.node else {
            panic!("guard should be Binary, got {:?}", guard.node);
        };
        assert!(matches!(cmp.op, zyntax_typed_ast::BinaryOp::Ge));
        let TypedExpression::Call(sig_call) = &cmp.left.node else {
            panic!("LHS should be Call after rewrite");
        };
        let TypedExpression::Variable(callee) = &sig_call.callee.node else {
            panic!("expected Variable callee");
        };
        assert_eq!(
            callee.resolve_global().as_deref(),
            Some("__signal_get_f64"),
            "f64 signal should rewrite to __signal_get_f64"
        );
        let TypedExpression::Literal(TypedLiteral::Float(v)) = &cmp.right.node else {
            panic!("RHS should be FloatLit, got {:?}", cmp.right.node);
        };
        assert!((*v - 1.0_f64).abs() < f64::EPSILON);
    }

    /// End-to-end: float-signal guard threshold crossing fires tick.
    #[test]
    fn float_signal_guard_fires_on_threshold() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.set_signal_f64("e2e_progress", 0.0);
        dsl.compile_source(
            r#"
            signal e2e_progress: f64
            fsm FloatGuardProbe {
                state Loading
                state Done
                initial Loading
                tick Loading -> Done when e2e_progress.get() >= 1.0
            }
            "#,
            "float_e2e.blinc",
        )
        .expect("compile");

        let module = InternedString::new_global("main");
        let id = with_fsm_registry(|r| r.find_by_name(module, "FloatGuardProbe"))
            .expect("FloatGuardProbe registered");

        let next = dsl.step_tick(&id, "Loading").expect("step_tick");
        assert!(next.is_none(), "0.0 < 1.0, should not fire");

        dsl.set_signal_f64("e2e_progress", 1.0);
        let next = dsl.step_tick(&id, "Loading").expect("step_tick");
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Done"),
            "1.0 >= 1.0, guard fires"
        );
    }

    /// `FsmInstance` lifecycle: construct, dispatch sequence, follow `current()`.
    #[test]
    fn fsm_instance_event_round_trip() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm InstanceProbeA {
                state Idle
                state Loading
                state Done
                initial Idle
                on Idle.Start -> Loading
                on Loading.Finish -> Done
                on Done.Reset -> Idle
            }
            "#,
            "instance_probe_a.blinc",
        )
        .expect("compile");

        let mut instance = FsmInstance::new(&dsl, "main", "InstanceProbeA")
            .expect("InstanceProbeA should construct");
        assert_eq!(instance.current(), "Idle", "starts in declared initial");

        // Idle --Start--> Loading
        let fired = instance.dispatch_event(&dsl, "Start");
        assert!(fired, "Idle.Start should transition");
        assert_eq!(instance.current(), "Loading");

        // Loading --Finish--> Done
        let fired = instance.dispatch_event(&dsl, "Finish");
        assert!(fired);
        assert_eq!(instance.current(), "Done");

        // Done --Reset--> Idle (full cycle)
        let fired = instance.dispatch_event(&dsl, "Reset");
        assert!(fired);
        assert_eq!(instance.current(), "Idle");
    }

    /// Misses: dispatch on an event that doesn't match the
    /// Unknown event leaves `current()` unchanged and returns false.
    #[test]
    fn fsm_instance_event_miss_keeps_current() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm InstanceProbeMiss {
                state Off
                state On
                initial Off
                on Off.Click -> On
                on On.Click -> Off
            }
            "#,
            "instance_probe_miss.blinc",
        )
        .expect("compile");

        let mut instance = FsmInstance::new(&dsl, "main", "InstanceProbeMiss").unwrap();
        assert_eq!(instance.current(), "Off");

        let fired = instance.dispatch_event(&dsl, "DoesNotExist");
        assert!(!fired);
        assert_eq!(instance.current(), "Off", "miss should leave state alone");
    }

    /// Signal-guarded tick through `FsmInstance::tick`.
    #[test]
    fn fsm_instance_tick_with_signal_guard() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.set_signal_i32("instance_tick_count", 5);
        dsl.compile_source(
            r#"
            signal instance_tick_count: i32
            fsm InstanceProbeTick {
                state Cold
                state Hot
                initial Cold
                tick Cold -> Hot when instance_tick_count.get() > 100
            }
            "#,
            "instance_probe_tick.blinc",
        )
        .expect("compile");

        let mut instance = FsmInstance::new(&dsl, "main", "InstanceProbeTick").unwrap();

        let fired = instance.tick(&dsl).expect("tick");
        assert!(!fired);
        assert_eq!(instance.current(), "Cold");

        dsl.set_signal_i32("instance_tick_count", 200);

        let fired = instance.tick(&dsl).expect("tick");
        assert!(fired);
        assert_eq!(instance.current(), "Hot");
    }

    /// `reset()` returns to the declared initial state from any current state.
    #[test]
    fn fsm_instance_reset() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(
            r#"
            fsm InstanceProbeReset {
                state Idle
                state Working
                initial Idle
                on Idle.Go -> Working
            }
            "#,
            "instance_probe_reset.blinc",
        )
        .expect("compile");

        let mut instance = FsmInstance::new(&dsl, "main", "InstanceProbeReset").unwrap();

        instance.dispatch_event(&dsl, "Go");
        assert_eq!(instance.current(), "Working");

        instance.reset();
        assert_eq!(
            instance.current(),
            "Idle",
            "reset should return to declared initial state"
        );
    }

    /// `FsmInstance::new` returns `None` for unknown fsm names (no panic).
    #[test]
    fn fsm_instance_unknown_name_returns_none() {
        let dsl = BlincDsl::new().expect("runtime init");
        let attempt = FsmInstance::new(&dsl, "main", "DoesNotExistFsm");
        assert!(
            attempt.is_none(),
            "missing fsm should return None, not panic"
        );
    }

    /// `FsmDefinition::step_event` works directly without the registry.
    #[test]
    fn fsm_definition_step_event_direct() {
        let def = FsmDefinition {
            initial: Some(intern("Idle")),
            transitions: vec![
                EventTransition {
                    from: intern("Idle"),
                    event: intern("Go"),
                    to: intern("Running"),
                    actions: vec![],
                },
                EventTransition {
                    from: intern("Running"),
                    event: intern("Stop"),
                    to: intern("Idle"),
                    actions: vec![],
                },
            ],
            ..FsmDefinition::default()
        };

        assert_eq!(
            def.step_event("Idle", "Go")
                .and_then(|n| n.resolve_global())
                .as_deref(),
            Some("Running")
        );
        assert_eq!(
            def.step_event("Running", "Stop")
                .and_then(|n| n.resolve_global())
                .as_deref(),
            Some("Idle")
        );
        assert!(def.step_event("Idle", "Stop").is_none());
        assert!(def.step_event("Done", "Go").is_none());
    }

    /// `with_fsm_registry` / `with_fsm_registry_mut` round-trip.
    #[test]
    fn fsm_registry_global_accessors() {
        // High TypeId to avoid collisions with parallel tests.
        let id = fid("global_test_module", 9_999);

        with_fsm_registry_mut(|r| {
            r.upsert(
                id,
                FsmDefinition {
                    initial: Some(intern("Begin")),
                    ..FsmDefinition::default()
                },
            );
        });

        let initial = with_fsm_registry(|r| {
            r.get(&id)
                .and_then(|d| d.initial.and_then(|n| n.resolve_global()))
        });
        assert_eq!(initial.as_deref(), Some("Begin"));

        with_fsm_registry_mut(|r| {
            r.remove(&id);
        });
    }

    /// Mixed `text("…")` + `text(N)` route to distinct builtins via PEG alternates.
    #[test]
    fn round_trip_text_mixed_args() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        dsl.compile_source(r#"view { text("answer:") text(42) }"#, "mixed_smoke.blinc")
            .expect("compile");
        let ops = dsl.render_view().expect("render_view");

        assert_eq!(ops.len(), 2, "expected 2 ops, got {ops:?}");
        match &ops[0] {
            DslOp::Text(s) => assert_eq!(s, "answer:"),
            other => panic!("expected DslOp::Text, got {other:?}"),
        }
        match &ops[1] {
            DslOp::IntText(n) => assert_eq!(*n, 42),
            other => panic!("expected DslOp::IntText, got {other:?}"),
        }
    }

    // Diagnostic-channel probes — failure modes return `BlincDslError`, not panic.

    /// Stray closing brace → `BlincDslError::Compile`.
    #[test]
    fn diag_parse_error_unmatched_brace() {
        let err = try_compile("view { text(\"hi\") } }", "parse_err.blinc")
            .expect_err("expected compile to fail on stray closing brace");
        let lower = err.to_lowercase();
        assert!(
            lower.contains("parse") || lower.contains("error") || lower.contains("expected"),
            "expected diagnostic to mention parse / error / expected; got: {err}"
        );
    }

    /// `text()` with no args violates the grammar rule — actionable diagnostic.
    #[test]
    fn diag_arity_error_text_no_args() {
        let err = try_compile("view { text() }", "arity_err.blinc")
            .expect_err("expected compile to fail on text() with no args");
        let lower = err.to_lowercase();
        assert!(
            lower.contains("error") || lower.contains("expected") || lower.contains("parse"),
            "expected diagnostic to mention error / expected / parse; got: {err}"
        );
    }

    /// Type-mismatch on `text(...)` — ignored until grammar supports non-string exprs.
    #[test]
    #[ignore = "needs phase-2 expression args for text()"]
    fn diag_type_error_text_with_int_literal() {
        let err = try_compile("view { text(42) }", "type_err.blinc")
            .expect_err("expected compile to fail on text(42)");
        let lower = err.to_lowercase();
        assert!(
            lower.contains("type") || lower.contains("expected") || lower.contains("string"),
            "expected diagnostic to mention type / expected / string; got: {err}"
        );
    }
}
