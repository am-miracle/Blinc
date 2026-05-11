//! Blinc DSL core — Zyntax-embedded grammar, runtime engine, and host
//! glue.
//!
//! # Status
//!
//! Risk-reduction prototype. Covers the `view { text("...") }` slice
//! end-to-end so we can validate Zyntax integration before scaling up
//! to the full grammar. See ROADMAP §3.9 for the phasing.
//!
//! # Pipeline
//!
//! ```text
//! .blinc source
//!     |
//!     v
//! [Grammar2::from_source(BLINC_GRAMMAR)] -> TypedProgram
//!     |
//!     v
//! [ZyntaxRuntime::compile_typed_program]  -> HirModule (cached)
//!     |
//!     v
//! [runtime.call::<()>("render_view", &[])]
//!     |
//!     v
//! [host scene buffer drained via take_scene_ops()]
//!     |
//!     v
//! [ElementBuilder tree handed to Blinc renderer]
//! ```
//!
//! Builtins are registered statically (`runtime.register_function`) —
//! no `.zrtl` plugin discovery from disk. Blinc's set of builtins is
//! fixed at link time, which matches the zynml "patterns to NOT copy"
//! recommendation in our research notes.

use std::path::Path;
use std::sync::{Arc, Mutex};

use thiserror::Error;
use zyntax_embed::{
    Grammar2, Grammar2Error, NativeSignature, NativeType, RuntimeError, TypeTag, ZrtlSigFlags,
    ZrtlSymbolSig, ZyntaxRuntime,
};

/// Mirror of `zyntax_compiler::zrtl::MAX_PARAMS` (16). Not re-exported
/// from `zyntax_embed`, so we redeclare locally; the value is part of
/// the wire ABI for `ZrtlSymbolSig` and won't change without a major
/// version bump on the embed crate.
const ZRTL_MAX_PARAMS: usize = 16;
use zyntax_typed_ast::type_registry::{PrimitiveType, Type};
use zyntax_typed_ast::{typed_node, Span, TypedProgram, TypedStatement};

/// The embedded Blinc DSL grammar source.
///
/// Baked at compile time so apps don't ship a `.zyn` file separately.
/// Mirrors the zynml `ZYNML_GRAMMAR` pattern (zynml/src/lib.rs:77 in
/// the Zyntax sibling repo).
pub const BLINC_GRAMMAR: &str = include_str!("../grammar/blinc.zyn");

// =====================================================================
// Scene buffer — TRANSITIONAL legacy op stream
// =====================================================================
//
// This module's `$Blinc$text` / `$Blinc$text_int` builtins still
// push onto a per-thread scene buffer; `BlincDsl::render_view` /
// `render_component` drain it. That op-stream design is being
// replaced by value-returning view functions that construct real
// `blinc_layout` widget trees via registered primitives — the
// substrate's `ViewRenderer::render_named` already returns
// `ZyntaxValue` for that path. This buffer plumbing stays around
// only until `Div`, `Text`, etc. are registered as widget
// primitives (Phase 2 of the pivot); at that point the externs
// become widget-handle constructors, the buffer goes away, and
// `render_view` / `render_component` return widget trees directly.
//
// Substrate-public code path: `blinc_runtime::view::ViewRenderer`
// (returns `ZyntaxValue`).
// Legacy DSL-only path: `BlincDsl::render_view()` (returns
// `Vec<DslOp>`, drains this buffer).

use std::cell::RefCell;

/// One declarative draw op emitted by the DSL during a
/// `render_view` call. Transitional — once widget primitives
/// are registered, view functions return real
/// `blinc_layout::Div` / `Text` handles wrapped in `ZyntaxValue`
/// and this enum + buffer disappear.
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

/// Drain and return everything pushed onto the scene buffer
/// since the last call. Legacy path — see the module-level
/// note above.
pub fn take_scene_ops() -> Vec<DslOp> {
    SCENE_BUFFER.with(|b| std::mem::take(&mut *b.borrow_mut()))
}

// =====================================================================
// Builtins
// =====================================================================

/// `$Blinc$text` — the only builtin in the prototype slice.
///
/// Cranelift passes Zyntax `string` arguments as a pointer into the
/// length-prefixed `ZyntaxString` layout: `[i32 length][utf8 bytes...]`
/// (see `zyntax_embed::string` and the `ZyntaxString::HEADER_SIZE`
/// constant — 4 bytes). We pull the length, read the bytes, and strip
/// the surrounding quotes the grammar preserved (`string_literal`'s
/// `text()` capture returns `"hello"` whole, not `hello`).
///
/// # Safety
///
/// Called by Zyntax's JIT through a function pointer registered via
/// [`ZyntaxRuntime::register_function`]. The runtime guarantees the
/// argument shape matches the registered signature (one
/// `NativeType::Ptr`); any deviation is a Zyntax bug, not ours.
extern "C" fn blinc_text(s_ptr: *const i32) {
    if s_ptr.is_null() {
        tracing::warn!("$Blinc$text called with null pointer");
        return;
    }

    // SAFETY: the runtime guarantees `s_ptr` points at a Zyntax
    // length-prefixed UTF-8 buffer when the signature is `Ptr` for a
    // string parameter.
    let raw = unsafe {
        let len = std::ptr::read_unaligned(s_ptr) as usize;
        let body = (s_ptr as *const u8).add(std::mem::size_of::<i32>());
        let bytes = std::slice::from_raw_parts(body, len);
        std::str::from_utf8(bytes).unwrap_or("<invalid utf-8>")
    };

    // The grammar's `string_literal` capture preserves the surrounding
    // quotes (`text()` returns `"hello"` not `hello`). Strip them
    // before the host sees the value.
    let stripped = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw);

    push_op(DslOp::Text(stripped.to_string()));
}

/// `__signal_get_i32` — host implementation of the i32 signal
/// accessor synthesised by the `resolve_signal_calls` pass.
///
/// The pass rewrites every `<name>.get()` (where `<name>` is an
/// i32 signal declared via `signal <name>: i32`) into
/// `__signal_get_i32("<name>")`. At JIT time this Rust function
/// runs, looks the name up in the per-thread `SIGNAL_TABLE_I32`,
/// and returns the current value (or `0` when the signal hasn't
/// been set, which mirrors Default::default for i32 — embedders
/// that want a different fallback can call `set_signal_i32`
/// during `BlincDsl::new` to seed defaults).
///
/// # Safety
///
/// Same contract as [`blinc_text`]. The runtime guarantees
/// `name_ptr` points at a Zyntax length-prefixed UTF-8 buffer
/// when the registered signature has `String` for the parameter.
extern "C" fn blinc_signal_get_i32(name_ptr: *const i32) -> i32 {
    if name_ptr.is_null() {
        tracing::warn!("__signal_get_i32 called with null name pointer");
        return 0;
    }

    // SAFETY: Zyntax guarantees the length-prefixed Zyntax-string
    // layout when the registered parameter type is String.
    let name = unsafe {
        let len = std::ptr::read_unaligned(name_ptr) as usize;
        let body = (name_ptr as *const u8).add(std::mem::size_of::<i32>());
        let bytes = std::slice::from_raw_parts(body, len);
        std::str::from_utf8(bytes).unwrap_or("<invalid utf-8>")
    };

    // The signal-name string literal generated by the rewrite is
    // already unquoted (StringLiteral construction strips the
    // quotes — interpreter.rs:553). Defensive trim for parity with
    // blinc_text in case the synthesis changes upstream.
    let stripped = name
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(name);

    blinc_runtime::signal::get_i32_or_default(stripped)
}

/// `__signal_get_f64` — host implementation of the f64 signal
/// accessor. Mirrors `blinc_signal_get_i32` in shape; the only
/// differences are the lookup table and the return type.
/// Default fallback is `0.0` for unset signals.
///
/// # Safety
///
/// Same contract as [`blinc_signal_get_i32`]. The runtime
/// guarantees `name_ptr` points at a Zyntax length-prefixed
/// UTF-8 buffer.
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

/// `__signal_get_string` — host implementation of the string
/// signal accessor.
///
/// Same shape as the i32 / f64 mirrors but returns a Zyntax
/// length-prefixed string pointer (the same layout
/// `blinc_string_alloc` produces — what `$Blinc$text` and the
/// f-string concat chain consume). The value is cloned from the
/// per-thread signal table and the resulting buffer leaks via
/// the prototype's `blinc_string_alloc` (see the module comment
/// on the f-string helpers for the per-render arena fix path).
///
/// # Safety
///
/// Same contract as [`blinc_signal_get_i32`]. The runtime
/// guarantees `name_ptr` points at a Zyntax length-prefixed
/// UTF-8 buffer.
extern "C" fn blinc_signal_get_string(name_ptr: *const i32) -> *const i32 {
    if name_ptr.is_null() {
        tracing::warn!("__signal_get_string called with null name pointer");
        return blinc_string_alloc("");
    }

    // SAFETY: Zyntax guarantees the length-prefixed string
    // layout when the registered parameter type is String.
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

/// `$Blinc$text_int` — integer arm of `text(...)`. Pushes an integer
/// onto the scene buffer.
///
/// Probes the i32 ABI through Cranelift's JIT: the host receives an
/// actual `i32` register, not a length-prefixed pointer. Matches what
/// the grammar's `integer` terminal lowers to via `parse_int(text())`.
///
/// # Safety
///
/// Same contract as [`blinc_text`]. The runtime guarantees the
/// argument shape matches the registered `NativeType::I32`.
extern "C" fn blinc_text_int(n: i32) {
    push_op(DslOp::IntText(n));
}

// =====================================================================
// F-string desugaring builtins
// =====================================================================
//
// `f"hi {n}"` lowers (via the normalization-pass `fstring_to_concat`
// rewrite at zyntax/crates/passes/normalization/src/lib.rs:137) into
// `string_concat("hi ", __fstring_format__(n))`. Both names need to
// resolve to host externs at JIT time — without them the SSA emits
// undefined-variable reads that the runtime later derefs as a null
// pointer, surfacing as a SIGSEGV instead of a compile error.
//
// Upstream ZynML registers these via the ZRTL `io` plugin
// (`$IO$string_concat`, `$IO$format_dynamic`). Blinc doesn't load
// ZRTL plugins — we use the static `register_function_typed` path
// for everything — so we implement minimal Blinc-side versions
// here.
//
// **Memory layout.** Zyntax-side strings are length-prefixed:
//
// ```text
// +-----+----- ... -----+
// | u32 |   UTF-8 bytes |
// +-----+----- ... -----+
//   len      content
// ```
//
// The `*const i32` pointer the JIT passes around points at the
// length word; the bytes follow immediately after. `blinc_text`
// already reads from this layout — these formatters produce it.
//
// **Memory ownership.** Strings produced here LEAK — the host
// doesn't free them. Acceptable for the prototype (test runs are
// short-lived) but tracked as a known limitation: a single render
// pass that materialises N f-strings keeps N buffers resident
// until process exit. Fix path is to switch to a per-render
// arena bump-allocator and reset between calls.

/// Encode a Rust `&str` as a Zyntax length-prefixed string and
/// leak it. Returns a pointer to the length word.
///
/// The buffer layout matches what `blinc_text` (and any
/// future string-typed builtin) reads.
fn blinc_string_alloc(s: &str) -> *const i32 {
    let len = s.len() as u32;
    let total = 4 + s.len();
    let mut buf: Vec<u8> = Vec::with_capacity(total);
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
    let ptr = buf.as_ptr() as *const i32;
    // Leak — see module comment above. The ZRTL plugin path uses
    // a managed allocator with explicit free; the static-builtin
    // path here doesn't have that machinery wired up yet.
    std::mem::forget(buf);
    ptr
}

/// Decode a Zyntax length-prefixed string back into a Rust
/// `&str`. The returned reference is valid as long as the
/// underlying buffer remains live — for our leak-on-allocate
/// strategy that's "forever", so 'static.
///
/// # Safety
///
/// Caller must guarantee `ptr` came from `blinc_string_alloc`
/// (or another producer that emits the same length-prefix +
/// UTF-8 layout). The JIT guarantees this when the registered
/// signature has `String` for the parameter.
unsafe fn blinc_string_decode<'a>(ptr: *const i32) -> &'a str {
    if ptr.is_null() {
        return "";
    }
    let len = std::ptr::read_unaligned(ptr) as usize;
    let body = (ptr as *const u8).add(4);
    let bytes = std::slice::from_raw_parts(body, len);
    std::str::from_utf8(bytes).unwrap_or("<invalid utf-8>")
}

/// `__fstring_format__` for i32 — formats an integer as its
/// decimal-string representation in a fresh Zyntax string.
///
/// The DSL's `@builtin` map routes `__fstring_format__` to
/// `$Blinc$format_int`. Today this only handles i32 inputs —
/// f64 props would need a separate format builtin (i.e. a
/// generic dispatch path or a per-type DSL-visible alias).
extern "C" fn blinc_format_int(n: i32) -> *const i32 {
    let s = n.to_string();
    blinc_string_alloc(&s)
}

/// `string_concat` — joins two Zyntax-formatted strings into a
/// fresh one. Same memory-ownership caveat as the rest of this
/// section.
extern "C" fn blinc_string_concat(a: *const i32, b: *const i32) -> *const i32 {
    // SAFETY: the runtime guarantees both args are well-formed
    // length-prefixed string pointers when the registered
    // signature has `String` for both params.
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
// These are the runtime-side of the value-returning view
// architecture. Each pre-registered widget primitive (see
// `register_blinc_layout_primitives`) has a matching extern
// here that builds a real `blinc_layout` value and returns
// a pointer-sized handle. The handle crosses the JIT boundary
// as `i64` (the platform's `usize` width); the host-side
// walker that lands in Phase 2e knows how to dereference it
// back into the real Rust value.
//
// **Memory ownership.** Each call boxes a fresh widget
// (`Box::into_raw` → leak-on-construct, like the f-string
// helpers above) and returns the raw pointer. A consumer that
// receives the handle becomes responsible for reclaiming it
// (the walker takes ownership via `Box::from_raw`). For the
// prototype's short-lived test runs the leak is acceptable;
// production paths flow the handle into the actual UI tree
// where it gets owned for the widget's lifetime.

/// Tagged box carrying a concrete `blinc_layout` widget across
/// the JIT boundary. Each widget-primitive extern (`blinc_text_view`,
/// `blinc_div_view`, …) builds the appropriate variant, wraps it in
/// `Box<WidgetBox>`, and returns the raw pointer cast to `i64`.
///
/// The variant tag is what lets the host-side walker
/// ([`materialize_widget`]) dispatch back to a concrete
/// `blinc_layout` value — an untagged raw pointer would lose the
/// type identity at the JIT boundary.
///
/// Each payload is boxed because `blinc_layout::div::Div` carries
/// the full property suite (~1 KB) and Rust enums size to their
/// largest variant; without the inner box every `Text` handle
/// would balloon to the same width as a `Div`.
///
/// **`Custom` carries arbitrary `ElementBuilder` implementations**
/// — the variant Rust-side widget extensions land in. It's the
/// shared payload type for both interop directions:
///
///   - Rust → DSL: a Rust-side widget registered with
///     [`BlincDsl::register_extern_widget`] wraps the user
///     struct in `WidgetBox::Custom` so it round-trips back to
///     Rust as a typed `Box<dyn ElementBuilder>`.
///   - DSL → Rust: [`BlincDsl::query`] decodes a value-returning
///     DSL component's handle through this variant when the
///     component eventually produces user types.
pub enum WidgetBox {
    Text(Box<blinc_layout::text::Text>),
    Div(Box<blinc_layout::div::Div>),
    Custom(Box<dyn blinc_layout::div::ElementBuilder>),
}

impl WidgetBox {
    /// Coerce into a `Box<dyn ElementBuilder>` so callers can
    /// slot the widget into a Rust-side tree without caring which
    /// variant it came in as. Used by the DSL→Rust query path
    /// and any consumer that just wants "build me into a layout
    /// tree" semantics.
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
}

/// Re-exports the `#[extern_widget]` macro lives at the crate
/// root so users only need one import to declare a DSL-exposed
/// Rust widget. The expansion targets [`__extern_widget_internals`]
/// — see that module for the surface the macro consumes.
pub use blinc_macros::extern_widget;

/// The canonical Zyntax runtime value enum, re-exported so
/// consumers of `blinc_dsl_core` (notably [`BlincDsl::query`]
/// callers and `ViewRenderer` consumers) can match on view
/// return values without separately depending on
/// `zyntax_embed`.
pub use zyntax_embed::ZyntaxValue;

/// Internals exposed for the [`extern_widget!`](crate::extern_widget)
/// proc-macro's code generation. Not part of the stable public API
/// surface — the items here exist so generated code can find them
/// at fully-qualified paths. Manual callers should use the typed
/// surfaces in the crate root ([`WidgetBox`],
/// [`ExternWidgetSpec`], [`ExternWidget`],
/// [`BlincDsl::register_extern_widget`]).
///
/// All re-exports here mirror existing public items; the module
/// is `doc(hidden)` only because the *path* is an implementation
/// detail of the macro, not the items themselves.
#[doc(hidden)]
pub mod __extern_widget_internals {
    pub use crate::{
        BlincDsl, BlincDslError, BlincDslResult, ExternWidget, ExternWidgetSpec,
        RenderPropsOverlay, Styled, WidgetBox,
    };
    pub use blinc_runtime::component::PropDef;
    pub use zyntax_typed_ast::type_registry::{PrimitiveType, Type};

    /// Reclaim a `RenderPropsOverlay` from a `__new_style_overlay__`
    /// pointer for use in macro-generated thunks. Wraps
    /// `materialize_overlay` so the macro doesn't reach into a
    /// public-but-unsafe surface name directly.
    ///
    /// # Safety
    ///
    /// `ptr` MUST come from `__new_style_overlay__`.
    pub unsafe fn decode_overlay(ptr: i64) -> crate::RenderPropsOverlay {
        unsafe { crate::materialize_overlay(ptr) }
    }

    /// Construct a `WidgetHandle` from any
    /// `Box<dyn ElementBuilder>`. Wraps it in
    /// [`WidgetBox::Custom`] and leaks the box; the host-side
    /// walker reclaims via `materialize_widget` at view-render
    /// time.
    pub fn into_handle(widget: Box<dyn blinc_layout::div::ElementBuilder>) -> i64 {
        Box::into_raw(Box::new(WidgetBox::Custom(widget))) as i64
    }

    /// Decode a Zyntax-FFI string argument (length-prefixed
    /// UTF-8 buffer) to an owned `String`. Returns the empty
    /// string for null pointers — same fast-fail the in-tree
    /// `$Blinc$Text$view` extern uses.
    ///
    /// # Safety
    ///
    /// `ptr` MUST come from the Zyntax JIT's String FFI lowering
    /// — i.e., the registered signature has `String` for the
    /// corresponding parameter. Any other source is undefined
    /// behaviour.
    pub unsafe fn decode_string(ptr: *const i32) -> String {
        if ptr.is_null() {
            return String::new();
        }
        // SAFETY: forwarded from caller per fn-level doc.
        let s = unsafe { super::blinc_string_decode(ptr) };
        s.to_string()
    }

    /// Decode a children-list pointer (minted by
    /// `__new_child_list__` and populated via `__push_child__`)
    /// into an owned `Vec<Box<dyn ElementBuilder>>` ready to
    /// land in a `#[children]`-annotated struct field.
    ///
    /// A null/zero pointer means the DSL caller didn't provide a
    /// body block — the widget gets an empty children list, same
    /// shape as the in-tree `Div() { }` path.
    ///
    /// # Safety
    ///
    /// `ptr` MUST be the `i64`-encoded payload of a
    /// `Box<Vec<WidgetHandle>>` minted by
    /// `__new_child_list__`. Every JIT call site that produces
    /// children pointers routes through
    /// `lower_children_arrays_to_blocks`, which constructs lists
    /// exclusively via that extern — call sites can't forge a
    /// pointer.
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

/// Trait implemented by Rust types that want to be callable from
/// Blinc DSL source. The [`extern_widget!`](crate::extern_widget)
/// proc-macro generates the impl on a user struct, pulling the
/// DSL name out of the macro attribute and the prop list out of
/// the struct's fields.
///
/// Users register a widget through the type:
///
/// ```ignore
/// dsl.register_extern_widget::<FancyText>()?;
/// ```
///
/// The generic dispatch reads `Self::DSL_NAME` and
/// `Self::extern_widget_spec()` and forwards to the lower-level
/// [`BlincDsl::register_extern_widget_spec`] — see that method
/// for the underlying registration semantics.
///
/// **Hand-rolling without the macro:** implement this trait
/// manually if you need a shape the proc-macro doesn't yet
/// support (callbacks, custom marshalling, generic widgets).
/// The trait surface is intentionally narrow so hand impls and
/// macro impls stay symmetric.
pub trait ExternWidget {
    /// User-facing identifier — what `.blinc` source types to
    /// call the widget. Matches the `name = "<DslName>"` value
    /// the macro attribute carries.
    const DSL_NAME: &'static str;

    /// Build the full registration spec. The macro generates this
    /// from the struct's fields; hand impls construct one directly.
    fn extern_widget_spec() -> ExternWidgetSpec;
}

/// Description of a Rust-side widget being exposed to the DSL.
///
/// Hand-rolled today; the planned `#[extern_widget]` proc-macro
/// generates one of these from a Rust struct + `ElementBuilder`
/// impl. Passed to [`BlincDsl::register_extern_widget`] which:
///
///   1. Registers the JIT-side `extern "C"` thunk under
///      `view_symbol` with the matching ZRTL signature.
///   2. Records a `ComponentDefinition` in the substrate
///      `ComponentRegistry` so `validate_component_calls` /
///      `lower_component_calls` see the widget as a DSL-callable
///      component.
///   3. Marks `view_symbol` as value-returning so
///      [`JitViewRenderer`] / [`BlincDsl::render_named`] pick
///      the `i64`-return ABI when invoking it.
///
/// **Naming convention:** Pick a `$Blinc$<Name>$view`-shaped
/// symbol to match the in-tree primitive externs (`$Blinc$Text$view`,
/// `$Blinc$Div$view`). The DSL-facing name (`name` field) is what
/// users type in `.blinc` source; the `view_symbol` is what the
/// JIT links to.
///
/// **Extern function contract:** `extern_ptr` MUST point at a
/// real `extern "C" fn(...)` whose argument types correspond
/// to `param_types` (Zyntax FFI uses
/// `*const i32` for `String`, `i32` / `i64` / `f64` directly for
/// primitives) and which returns a `WidgetHandle`
/// (`Box::into_raw(Box::new(WidgetBox::Custom(...)))` cast to `i64`).
/// A return value of `0` signals null / build-failed.
pub struct ExternWidgetSpec {
    /// User-facing DSL name. Whatever a `.blinc` author types
    /// (e.g., `"Button"` matches `Button(...)` call sites).
    pub name: String,
    /// JIT-linker-visible symbol the extern is registered under.
    /// Convention: `$Blinc$<Name>$view`.
    pub view_symbol: String,
    /// Substrate metadata about the widget's props. Drives
    /// validation diagnostics and (eventually) IDE / reflection
    /// surfaces.
    pub props: Vec<blinc_runtime::component::PropDef>,
    /// FFI parameter types in declaration order. Must match the
    /// extern fn's actual signature exactly — mismatches manifest
    /// as register-level garbage reads at call time.
    pub param_types: Vec<Type>,
    /// FFI return type. Typically
    /// `Type::Primitive(PrimitiveType::I64)` (widget handle).
    pub return_type: Type,
    /// `extern "C" fn(...)` cast to `*const u8`. The extern must
    /// uphold the [`ExternWidgetSpec`] contract documented above.
    pub extern_ptr: *const u8,
}

/// Opaque widget handle as exchanged across the JIT boundary.
/// Carries the address of a `Box<WidgetBox>` cast to the
/// signature-advertised `i64`. Zero is the null-equivalent
/// sentinel (extern fast-fail return).
type WidgetHandle = i64;

/// Take ownership of a `WidgetHandle` returned by a value-bearing
/// view function. Reconstructs the `Box<WidgetBox>` whose pointer
/// the extern stored in the handle, returning `None` for the
/// null-sentinel (`0`) the externs use to flag early-out.
///
/// # Safety
///
/// `handle` MUST be a handle previously produced by one of this
/// crate's `$Blinc$<X>$view` externs (which all use
/// `Box::into_raw(Box::new(WidgetBox::...))` to mint the pointer)
/// — or be zero. Calling with any other pointer is undefined
/// behaviour. Calling twice with the same non-zero handle is a
/// double-free; the JIT side hands out each handle exactly once
/// per call.
pub unsafe fn materialize_widget(handle: WidgetHandle) -> Option<Box<WidgetBox>> {
    if handle == 0 {
        return None;
    }
    // SAFETY: see fn-level doc.
    Some(unsafe { Box::from_raw(handle as *mut WidgetBox) })
}

/// `$Blinc$Text$view(content: string) -> WidgetHandle`
///
/// Constructs a `blinc_layout::Text` from the Zyntax string
/// argument, wraps it in a [`WidgetBox::Text`], leaks the box,
/// and returns the raw pointer cast to `i64`. The host-side
/// walker reclaims the box via [`materialize_widget`].
///
/// # Safety
///
/// `content_ptr` must point at a Zyntax length-prefixed UTF-8
/// buffer when the registered signature has `String` for the
/// parameter — the JIT guarantees this.
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

/// `$Blinc$Div$view(children: i64) -> WidgetHandle`
///
/// Constructs a `blinc_layout::Div` populated with the children
/// in the supplied child-list. `children` is an `i64`-encoded
/// pointer to a `Vec<WidgetHandle>` minted by
/// [`blinc_new_child_list`]; each handle in the vec was produced
/// by some other widget extern and gets reclaimed here via
/// [`materialize_widget`] + [`WidgetBox::into_element_builder`].
///
/// A null/zero list pointer produces an empty `Div` — the same
/// thing `Div()` with no body would have produced under the
/// previous zero-arg signature.
///
/// **Memory ownership.** The child-list `Vec` is consumed
/// (`Box::from_raw`) — callers MUST NOT use the pointer after
/// this call. Each child handle is also consumed once: the
/// reclaimed `Box<WidgetBox>` flows into the Div as a
/// `Box<dyn ElementBuilder>` child, owned for the Div's
/// lifetime.
extern "C" fn blinc_div_view(children: WidgetHandle, style: i64) -> WidgetHandle {
    let mut widget = blinc_layout::div::Div::new();
    if children != 0 {
        // SAFETY: `children` is the raw-pointer payload of an
        // `i64` minted by `blinc_new_child_list`. The JIT
        // guarantees provenance by routing all child-list
        // construction through that extern.
        let list: Box<Vec<WidgetHandle>> =
            unsafe { Box::from_raw(children as *mut Vec<WidgetHandle>) };
        for handle in *list {
            if let Some(child_box) = unsafe { materialize_widget(handle) } {
                widget = widget.child_box(child_box.into_element_builder());
            }
        }
    }
    // SAFETY: `style` came from `__new_style_overlay__` (or is
    // `0`).
    let overlay = unsafe { materialize_overlay(style) };
    Box::into_raw(Box::new(WidgetBox::Custom(Box::new(Styled::new(
        widget, overlay,
    ))))) as WidgetHandle
}

/// `__new_child_list__() -> i64`
///
/// Allocate a fresh empty `Vec<WidgetHandle>` on the heap and
/// hand back its raw-pointer payload as `i64`. The lower pass
/// `lower_children_arrays` calls this once per primitive
/// container with a body block — `__push_child__` then appends
/// each evaluated child handle, and the container's extern
/// (`$Blinc$Div$view`, …) consumes the list.
extern "C" fn blinc_new_child_list() -> i64 {
    Box::into_raw(Box::new(Vec::<WidgetHandle>::new())) as i64
}

/// `__push_child__(list: i64, child: i64)`
///
/// Append a child handle to the `Vec<WidgetHandle>` referenced
/// by `list`. The list pointer must still be live (no
/// `Box::from_raw` since allocation); the container extern
/// reclaims it later.
///
/// # Safety
///
/// Both args MUST come from `__new_child_list__` (for `list`)
/// and a widget-handle extern (for `child`). The JIT-side
/// rewrite emits these pairings explicitly so this is enforced
/// at lowering time, not at the call site.
extern "C" fn blinc_push_child(list: i64, child: WidgetHandle) {
    if list == 0 {
        return;
    }
    // SAFETY: see fn-level doc. We deliberately use
    // `&mut *(raw as *mut Vec<...>)` instead of `Box::from_raw`
    // so the allocation stays live — the container extern is
    // what reclaims it.
    let vec: &mut Vec<WidgetHandle> = unsafe { &mut *(list as *mut Vec<WidgetHandle>) };
    vec.push(child);
}

// Overlay-builder externs. The lowering pass synthesises calls
// to these for styled-primitive call sites that supply inline
// visual props (`bg`, `opacity`, …). The overlay pointer is
// consumed by the container/widget extern, which wraps the
// constructed widget in `Styled<W>`.

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

/// Reclaim a `Box<RenderPropsOverlay>` minted by
/// `__new_style_overlay__`. Returns `Default::default()` for a
/// null pointer (the no-styling-args call-site shape).
///
/// # Safety
///
/// `ptr` MUST come from `__new_style_overlay__`. The lowering
/// pass is the only producer.
pub unsafe fn materialize_overlay(ptr: i64) -> RenderPropsOverlay {
    if ptr == 0 {
        return RenderPropsOverlay::default();
    }
    *unsafe { Box::from_raw(ptr as *mut RenderPropsOverlay) }
}

// =====================================================================
// Errors
// =====================================================================

/// Top-level error type for the embed API. Wraps Zyntax's own error
/// types so callers see one taxonomy. Each variant carries the
/// underlying Zyntax error so `Display` / `source()` still surfaces
/// the original diagnostic with file:line:col spans where Zyntax
/// produced them.
#[derive(Debug, Error)]
pub enum BlincDslError {
    /// `Grammar2::from_source(BLINC_GRAMMAR)` failed. This is a Blinc
    /// bug — the grammar is baked at compile time, so any compile-
    /// failure is on us, not the user.
    #[error("blinc grammar compile failed: {0}")]
    Grammar(#[from] Grammar2Error),

    /// `runtime.compile_typed_program(...)` failed — the user's
    /// `.blinc` source has a parse / type / lowering error. The
    /// inner string carries the diagnostic with a file:line span.
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
// Module-aware identity for fsms compiled by the DSL. Keys combine
// the Zyntax module name (currently always `"main"` — Zyntax compiles
// every source into a single module today, see
// `zyntax_embed/src/runtime.rs:1373`) with the FSM's `TypeId` from
// the program's `type_registry`. When Zyntax surfaces real per-source
// modules later, the same key shape extends without breaking changes:
// `("foo", TypeId)` and `("bar", TypeId)` for same-named fsms in
// different modules can coexist.
//
// Why not bare-string keys: two fsms named `Loader` in different
// modules would collide on a string key. `InternedString` doesn't help
// either — `InternedString::new_global("Loader")` returns the same
// handle process-wide regardless of source.
//
// Why not `HirId`: `HirId` is generated during HIR lowering after
// compilation, isn't accessible at parse time, and is opaque
// (`Uuid`-backed) — it works for runtime symbol lookup but not as a
// stable identity exposed at the DSL surface.

/// Identity of an fsm in the global registry: the Zyntax module the
/// fsm is compiled in plus its `TypeId` within that module's type
/// registry. Stable within a process run (TypeIds come from a
/// process-global atomic counter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FsmId {
    /// The Zyntax module name the fsm lives in. Today this is always
    /// `"main"` because Zyntax compiles each source into a single
    /// module. Future per-source / per-package modules will surface
    /// distinct values here without changing the rest of the registry
    /// API.
    pub module: zyntax_typed_ast::InternedString,
    /// The fsm's enum `TypeId`, looked up from the program's
    /// `type_registry` after `compile_source`. Used as the registry
    /// key for fast lookup; user-facing `dispatch::<Loader>(event)`
    /// resolves to this id via Zyntax's type machinery.
    pub type_id: zyntax_typed_ast::type_registry::TypeId,
}

/// Tick-driven guard transition record. The original DSL guard
/// expression (the `<expr>` after `when`) is lifted into a
/// stand-alone top-level function during the post-parse pass so
/// it survives compile and is callable at dispatch time as an
/// ordinary Zyntax-compiled symbol. `guard_fn` carries the
/// generated function's symbol name; resolving it through
/// `runtime.call::<bool>(name, &[])` evaluates the guard with
/// current signal values.
///
/// Why a generated function rather than carrying the AST node
/// directly: Zyntax's runtime exposes `call(symbol, args)` for
/// invocation, but no general-purpose "evaluate this AST" entry
/// point. Wrapping the expression in a function gives us a
/// callable symbol with no extra runtime infrastructure. The
/// generated name is `__fsm_tick_guard_<FsmName>_<idx>__`, where
/// `<idx>` is the guard's position in the fsm's declaration order
/// (deterministic, stable across runs of the same source).
#[derive(Debug, Clone)]
pub struct TickGuard {
    pub from: zyntax_typed_ast::InternedString,
    pub to: zyntax_typed_ast::InternedString,
    /// Synthesised guard-function symbol name. `None` only when
    /// the parse-time pass couldn't extract an expression (a
    /// malformed `__fsm_tick__` marker). In normal flow this is
    /// always `Some`.
    pub guard_fn: Option<zyntax_typed_ast::InternedString>,
}

/// One event-driven transition. Names match the DSL surface:
/// `on <from>.<event> -> <to>`.
#[derive(Debug, Clone)]
pub struct EventTransition {
    pub from: zyntax_typed_ast::InternedString,
    pub event: zyntax_typed_ast::InternedString,
    pub to: zyntax_typed_ast::InternedString,
}

/// The runtime definition of an fsm — populated by the host when
/// the fsm's `__fsm_meta__` body executes (each marker call inside
/// mutates the entry). Owned by the `FsmRegistry` keyed by `FsmId`.
#[derive(Debug, Clone, Default)]
pub struct FsmDefinition {
    /// Initial state name (variant of the fsm's state enum).
    pub initial: Option<zyntax_typed_ast::InternedString>,
    /// Event-driven transitions in declaration order. Same order as
    /// the source so dispatch can iterate match-arm-style.
    pub transitions: Vec<EventTransition>,
    /// Tick-driven guards in declaration order. Currently the guard
    /// expression isn't carried — see `TickGuard` doc.
    pub tick_guards: Vec<TickGuard>,
    /// Bare fsm name (from the begin marker), useful for diagnostic
    /// messages. The authoritative identity is `FsmId`.
    pub name: Option<zyntax_typed_ast::InternedString>,
}

impl FsmDefinition {
    /// Resolve an event-driven transition. Returns the target
    /// state's name (variant of the fsm's state enum) when there's
    /// a transition matching `(from = current, event = event)`, or
    /// `None` if no rule applies.
    ///
    /// Linear scan in declaration order — match-arm-style. The
    /// first matching rule wins. Authors who want priority semantics
    /// rely on declaration order in the source; the post-parse pass
    /// preserves it (`populate_fsm_registry_pass` walks the AST in
    /// statement order).
    ///
    /// `&str` arguments rather than `InternedString` because the
    /// dispatch caller is typically holding either bare runtime
    /// strings (from a host event channel) or compile-time literals
    /// — converting at the boundary keeps the call site terse. The
    /// implementation interns once for comparison.
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

/// Process-wide registry of fsm definitions populated as the host
/// runs each fsm's `__fsm_meta__` method. Lookup is by `FsmId`;
/// dispatch sites resolve the id from the user-facing fsm enum
/// type.
#[derive(Debug, Default)]
pub struct FsmRegistry {
    fsms: std::collections::HashMap<FsmId, FsmDefinition>,
}

impl FsmRegistry {
    /// Create an empty registry. Most callers should prefer the
    /// process-wide `global_fsm_registry()` accessor — the registry
    /// is host-managed singleton state, not something an embedder
    /// typically wants to instantiate per-instance.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert / update an fsm definition. Replaces any existing
    /// entry with the same `FsmId` — used when a hot-reload re-runs
    /// `__fsm_meta__` for an already-known fsm.
    pub fn upsert(&mut self, id: FsmId, def: FsmDefinition) {
        self.fsms.insert(id, def);
    }

    /// Look up an fsm by id. Returns `None` if no `__fsm_meta__`
    /// has been run for this `(module, type_id)` pair.
    pub fn get(&self, id: &FsmId) -> Option<&FsmDefinition> {
        self.fsms.get(id)
    }

    /// Mutable lookup. Used internally by the marker builtins to
    /// append transitions to the top-of-stack fsm's definition.
    pub fn get_mut(&mut self, id: &FsmId) -> Option<&mut FsmDefinition> {
        self.fsms.get_mut(id)
    }

    /// Number of registered fsms. Useful for tests and diagnostics.
    pub fn len(&self) -> usize {
        self.fsms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fsms.is_empty()
    }

    /// Iterate over all registered (id, definition) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&FsmId, &FsmDefinition)> {
        self.fsms.iter()
    }

    /// Remove an fsm from the registry. Used during hot-reload when
    /// a fsm decl gets removed from a source file — the host must
    /// drop the stale entry so dispatch fails loudly rather than
    /// silently using a definition for a type that no longer exists.
    pub fn remove(&mut self, id: &FsmId) -> Option<FsmDefinition> {
        self.fsms.remove(id)
    }

    /// Find an fsm by its source-level name within a given module.
    /// Returns `None` if no fsm of that name has been registered for
    /// the module. Useful as the entry point for callers that don't
    /// have an `FsmId` in hand — e.g. user code in Rust that holds
    /// the fsm name as a string after parsing the DSL source.
    ///
    /// Linear scan; for typical app sizes (handful of fsms per
    /// module) this is fine. If the registry grows past dozens of
    /// fsms per module a name → FsmId secondary index is the
    /// natural next step.
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

    /// Convenience: look up an fsm by id and resolve a transition
    /// in one call. Returns the target state's name when
    /// `(current, event)` matches a registered transition, `None`
    /// otherwise (no fsm registered, or no matching rule).
    ///
    /// Equivalent to `self.get(id).and_then(|d| d.step_event(...))`
    /// but lets callers avoid the explicit `get` lookup at the
    /// dispatch site.
    pub fn step_event(
        &self,
        id: &FsmId,
        current: &str,
        event: &str,
    ) -> Option<zyntax_typed_ast::InternedString> {
        self.get(id).and_then(|d| d.step_event(current, event))
    }
}

/// A live instance of a DSL-defined fsm — pairs an `FsmId` with the
/// current state name. Holds enough state for an embedder to drive
/// transitions without committing to a particular widget integration:
/// dispatch events / ticks, read the current state, reset to initial.
///
/// This is the dependency-free bridge to Blinc's Stateful pattern.
/// Wrapping an `FsmInstance` inside a `Stateful<S>` impl is one
/// integration shape, but it's not the only one — `FsmInstance`
/// works equally well as a field in any reactive container or
/// widget closure that needs string-keyed state.
///
/// # State representation
///
/// Current state is held as `InternedString` of the variant name
/// (e.g. `"Idle"`, `"Loading"`). This keeps the bridge dynamic —
/// no compile-time mapping between the DSL fsm's variants and a
/// user-defined Rust enum is required. Embedders that want a
/// strongly-typed enum on top can wrap with their own conversion
/// shim; for prototype use cases, the string-keyed shape is the
/// short path to "wire UI → DSL fsm".
///
/// # Example
///
/// ```ignore
/// let dsl = BlincDsl::new()?;
/// dsl.compile_source(/* fsm Loader { ... } */, "loader.blinc")?;
///
/// let mut loader = FsmInstance::new(&dsl, "main", "Loader")?;
/// assert_eq!(loader.current(), "Idle");
///
/// // Wire to a button click:
/// button("Start").on_click(move |_| {
///     loader.dispatch_event(&dsl, "Start");
///     // loader.current() → "Loading"
/// });
/// ```
#[derive(Debug, Clone)]
pub struct FsmInstance {
    /// Identity of the fsm definition this instance follows.
    /// Resolved via `FsmRegistry::find_by_name` at construction
    /// time so subsequent dispatches don't pay the lookup cost.
    pub id: FsmId,
    /// Current state name. Mutated in place by `dispatch_event` /
    /// `tick` when a transition fires.
    pub current: zyntax_typed_ast::InternedString,
}

impl FsmInstance {
    /// Create a new instance pinned to a fsm registered in the
    /// global registry. The instance starts in the fsm's declared
    /// initial state. Returns `None` if no fsm of the given name
    /// is registered for the module, or if the fsm was registered
    /// without an initial state.
    pub fn new(_dsl: &BlincDsl, module: &str, fsm_name: &str) -> Option<Self> {
        let module_i = zyntax_typed_ast::InternedString::new_global(module);
        let id = with_fsm_registry(|r| r.find_by_name(module_i, fsm_name))?;
        let initial = with_fsm_registry(|r| r.get(&id).and_then(|d| d.initial))?;
        Some(Self {
            id,
            current: initial,
        })
    }

    /// Current state name as a borrowed `&str`. Resolves from the
    /// instance's stored `InternedString`.
    pub fn current(&self) -> String {
        self.current.resolve_global().unwrap_or_default()
    }

    /// Dispatch an event by name. Returns `true` if a transition
    /// fired (and `current` has been updated to the new state),
    /// `false` if no rule matched. Mirrors the
    /// `StateTransitions::on_event` shape Blinc widgets expect:
    /// "did anything change?" maps to "should the UI rebuild?".
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

    /// Tick. Walks the registered tick-guards via `BlincDsl::step_tick`
    /// (which JIT-evaluates each guard), updates `current` if any
    /// fires, returns whether a transition happened. Errors from
    /// the JIT call propagate.
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

    /// Reset to the fsm's initial state. Useful when a higher-level
    /// flow restarts a sub-state-machine.
    pub fn reset(&mut self) {
        if let Some(initial) = with_fsm_registry(|r| r.get(&self.id).and_then(|d| d.initial)) {
            self.current = initial;
        }
    }
}

/// Process-wide fsm registry. Host marker builtins (registered in a
/// follow-up commit) read and mutate this through the `with_*`
/// accessors below. Stored as `OnceLock<Mutex<FsmRegistry>>` so
/// embedders that build multiple `BlincDsl` instances in the same
/// process share one consistent view.
static GLOBAL_FSM_REGISTRY: std::sync::OnceLock<std::sync::Mutex<FsmRegistry>> =
    std::sync::OnceLock::new();

fn fsm_registry_lock() -> std::sync::MutexGuard<'static, FsmRegistry> {
    GLOBAL_FSM_REGISTRY
        .get_or_init(|| std::sync::Mutex::new(FsmRegistry::new()))
        .lock()
        .expect("BlincDsl global FsmRegistry mutex poisoned")
}

/// Run a closure with shared access to the global fsm registry.
/// Use this from dispatch sites that need to resolve a fsm
/// definition by `FsmId`.
pub fn with_fsm_registry<R>(f: impl FnOnce(&FsmRegistry) -> R) -> R {
    let guard = fsm_registry_lock();
    f(&guard)
}

/// Run a closure with mutable access. Used internally by the
/// marker builtins; embedders shouldn't normally need this — the
/// registry is populated automatically when source compiles.
pub fn with_fsm_registry_mut<R>(f: impl FnOnce(&mut FsmRegistry) -> R) -> R {
    let mut guard = fsm_registry_lock();
    f(&mut guard)
}

// =====================================================================
// FSM dispatch synthesis (post-parse)
// =====================================================================

/// Recognition heuristic for `signal <name>: <T>` decls. The
/// grammar action's `Function` constructor in
/// `zyn_peg/src/runtime2/interpreter.rs:898` hardcodes
/// `link_name: None`, so we can't tag signal decls with a marker
/// link_name from the action — instead we identify them by shape:
/// extern function, no body, no parameters, primitive return,
/// and `link_name: None`. The latter discriminates against host
/// builtins auto-injected by `inject_builtin_externs`
/// (zyntax_embed/src/grammar2.rs:280) which always set
/// `link_name: Some(<target_symbol>)`.
fn is_signal_decl(func: &zyntax_typed_ast::typed_ast::TypedFunction) -> bool {
    func.is_external
        && func.body.is_none()
        && func.params.is_empty()
        && func.link_name.is_none()
        && matches!(func.return_type, Type::Primitive(_))
}

/// Resolve DSL signal references. Walks the program for
/// `signal <name>: <T>` declarations (recognised by `is_signal_decl`
/// above), records (name, type), then rewrites every
/// `<name>.get()` method call into a host-extern call
/// `__signal_get_<T>("<name>")`. The signal extern decls are
/// stripped before the program reaches Zyntax's compile path so
/// the bare-name extern doesn't collide with anything at link
/// time.
///
/// Why this shape:
///
///   * One host extern per primitive type (`__signal_get_i32`,
///     `__signal_get_string`, etc.) keeps the host registration
///     cost O(types), not O(signals). Adding a new signal is
///     pure DSL — no host code.
///   * `<name>.get()` syntax matches how DSL authors think about
///     signals (`State<T>::get()` is the standard accessor).
///     Internally we route through name + type discrimination at
///     the boundary; the DSL surface stays uniform.
///   * Stripping the signal decls before compile avoids two
///     problems: (a) the marker `link_name` would fail to link,
///     and (b) the bare-named extern (e.g. `count`) might shadow
///     other top-level callables.
///
/// Currently only primitive return types (`i32`, `string`) are
/// handled — adding more is one match arm here plus a host extern.
/// Struct returns route through Zyntax's Dyn Boxed machinery and
/// don't need this pass at all (the DSL author writes
/// `user.get().age` and Zyntax codegens the field load directly).
fn resolve_signal_calls(program: &mut TypedProgram) {
    use std::collections::HashMap;
    use zyntax_typed_ast::typed_ast::{TypedCall, TypedDeclaration, TypedExpression, TypedLiteral};
    use zyntax_typed_ast::InternedString;

    // -----------------------------------------------------------------
    // Phase 1: collect signal name → return type. Walk extern fns
    // tagged with the magic link_name marker. The pass doesn't
    // require signals be declared before use (PEG already orders
    // top_level_item alternates), but we collect them all up front
    // anyway so the rewrite walker can see signals declared after
    // their first usage too.
    // -----------------------------------------------------------------
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

    // -----------------------------------------------------------------
    // Phase 2: walk every expression in the program and rewrite
    // `<sig_name>.get()` (a `TypedExpression::MethodCall` on a
    // `Variable` receiver) into a `__signal_get_<T>("<name>")`
    // host-extern call.
    // -----------------------------------------------------------------
    fn typed_signal_extern_name(ty: &Type) -> Option<&'static str> {
        match ty {
            Type::Primitive(PrimitiveType::I32) => Some("__signal_get_i32"),
            Type::Primitive(PrimitiveType::F64) => Some("__signal_get_f64"),
            Type::Primitive(PrimitiveType::String) => Some("__signal_get_string"),
            // Add a match arm + a matching host builtin to extend.
            _ => None,
        }
    }

    fn rewrite_expr(
        expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>,
        signals: &HashMap<InternedString, Type>,
    ) {
        // Recursively rewrite children FIRST so nested signal calls
        // (e.g. `text(count.get())`) get the rewrite applied to the
        // inner expression before we look at the outer.
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
            // Other variants (Literal, Variable, etc.) have no
            // rewritable children at this layer.
            _ => {}
        }

        // Now check the current node: is it a method call of the
        // shape `<sig_name>.get()` we should rewrite?
        if let TypedExpression::MethodCall(mc) = &expr.node {
            let TypedExpression::Variable(receiver_name) = &mc.receiver.node else {
                return;
            };
            let Some(sig_ty) = signals.get(receiver_name) else {
                return;
            };
            if mc.method.resolve_global().as_deref() != Some("get") {
                return;
            }
            if !mc.positional_args.is_empty() {
                return;
            }

            let Some(extern_name) = typed_signal_extern_name(sig_ty) else {
                // Unsupported signal type — leave the method call
                // alone. The compile path will surface the
                // unresolved symbol if it's actually used.
                return;
            };

            let call = TypedExpression::Call(TypedCall {
                callee: Box::new(zyntax_typed_ast::TypedNode::new(
                    TypedExpression::Variable(InternedString::new_global(extern_name)),
                    Type::Unknown,
                    expr.span,
                )),
                positional_args: vec![zyntax_typed_ast::TypedNode::new(
                    TypedExpression::Literal(TypedLiteral::String(*receiver_name)),
                    Type::Primitive(PrimitiveType::String),
                    expr.span,
                )],
                named_args: vec![],
                type_args: vec![],
            });
            expr.node = call;
            expr.ty = sig_ty.clone();
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
            // Other variants don't carry expressions we'd rewrite
            // for the prototype slice.
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
    // Also walk impl-block methods.
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

    // -----------------------------------------------------------------
    // Phase 3: strip the signal-marker decls. They were just
    // metadata-carriers; the rewrite path replaces every usage
    // with calls to host-registered builtins.
    // -----------------------------------------------------------------
    program.declarations.retain(|decl| {
        let TypedDeclaration::Function(func) = &decl.node else {
            return true;
        };
        !is_signal_decl(func)
    });
}

/// Validate that every `__component_call__("Name", ...)` marker
/// references a `Name` that's been declared as a `component` (a
/// `TypedDeclaration::Class`) in the same program. The grammar
/// emits the marker for any uppercase-leading `Foo(...)`
/// invocation; this pass is what catches typos and undeclared
/// component references at compile time, before Zyntax's
/// type-checker has a chance to surface a less-helpful unresolved-
/// symbol error.
///
/// Returns the list of `(component_name, span_hint)` pairs that
/// failed validation — empty if everything resolves. Caller folds
/// non-empty results into a `BlincDslError::Compile`.
///
/// Note: this pass intentionally does NOT rewrite the markers. The
/// `__component_call__` shape is the stable contract the
/// downstream codegen / host-runtime layer consumes — keeping the
/// marker present after parse means later passes (and any
/// debug-AST printers) can still see exactly what the user wrote.
fn validate_component_calls(program: &TypedProgram) -> Result<(), Vec<String>> {
    use std::collections::HashSet;
    use zyntax_typed_ast::typed_ast::{TypedDeclaration, TypedExpression, TypedLiteral};

    let mut known: HashSet<String> = HashSet::new();
    // User-declared `component <Name> { ... }` decls in this
    // program contribute their names directly.
    for decl in &program.declarations {
        if let TypedDeclaration::Class(c) = &decl.node {
            if let Some(name) = c.name.resolve_global() {
                known.insert(name.to_string());
            }
        }
        // Named imports — Zyntax resolves the actual module at
        // compile time; we whitelist the imported names here so
        // the validator (which runs before import resolution)
        // doesn't flag them.
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
    // Substrate's `ComponentRegistry` carries pre-registered
    // primitives (`Div`, `Text`, etc. — see
    // `register_blinc_layout_primitives`). They aren't declared
    // in the user's source but are valid call targets, so the
    // validator pulls them in alongside the user decls. This is
    // also how user-defined components compiled in OTHER programs
    // could be referenced once cross-source linking lands.
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

                // Is this a __component_call__ marker? If so, check
                // its first positional arg (the component name
                // string literal) against the known-classes set.
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

/// Rewrite `__component_call__` marker calls into regular function
/// calls keyed on the component name, lifting `__named__` markers
/// into Zyntax's native `TypedNamedArg` shape.
///
/// Before:
///   Call(__component_call__, [
///       StringLiteral("Counter"),    // component name (positional 0)
///       IntLiteral(1),               // positional prop 1
///       Call(__named__, [StringLiteral("step"), IntLiteral(2)]),
///       Block { statements: [...] }, // optional trailing children body
///   ])
///
/// After:
///   Call(Variable("Counter"), positional_args=[IntLiteral(1),
///        Block { ... }], named_args=[NamedArg { name: "step", value:
///        IntLiteral(2) }])
///
/// The transformation makes the AST type-check and JIT-compile
/// against an actual `Counter` function symbol (once component
/// runtime lands), and surfaces named args in the form
/// downstream codegen already knows how to consume. The trailing
/// `Block` (if present) rides through as a regular positional arg
/// — TypedCall has no first-class "children" slot, so the
/// component-runtime side will recognise the Block-shape arg and
/// route it as children.
///
/// Slot markers (`__slot_open__` / `__slot_close__`) are left in
/// place inside the body Block — they're statement-level markers
/// the component-runtime side unfolds when it walks the children
/// block. Rewriting them here would lose the linear pair-up
/// information the upstream stmt-list flatten produced.
///
/// Why this runs AFTER `validate_component_calls`: the validator
/// reads the marker shape directly (StringLiteral as args[0]); if
/// we rewrote first, the validator would have to also look at the
/// rewritten Variable callee. Keeping passes independent keeps the
/// pipeline easy to reason about.
///
/// The rewrite is recursive so nested component calls inside a
/// body Block (e.g. `Counter() { Inner(1) }`) also get lowered.
fn lower_component_calls(program: &mut TypedProgram) {
    use zyntax_typed_ast::typed_ast::{
        TypedCall, TypedDeclaration, TypedExpression, TypedLiteral, TypedNamedArg,
    };

    fn rewrite_expr(expr: &mut zyntax_typed_ast::TypedNode<TypedExpression>) {
        // Recurse first so nested marker calls (inside body blocks,
        // inside other call args, etc.) get rewritten bottom-up.
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

        // Now check the current node — only act on calls whose
        // callee is the `__component_call__` marker.
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
                            // Marker shape is wrong — fall through
                            // and treat as positional. The validator
                            // doesn't currently check __named__ shape;
                            // ill-formed markers will surface as
                            // unresolved-symbol errors at compile.
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

        // Existing `named_args` on the marker call (if any —
        // shouldn't happen from the grammar but defensive) carry
        // through. Source order: positional args, then explicit
        // named-marker args, then any pre-existing named_args.
        new_named.extend(call.named_args.iter().cloned());

        // Rebuild the call: callee becomes a Variable reference to
        // the component's view symbol. The substrate's component
        // registry knows the exact symbol — pre-registered
        // primitives (`Div`, `Text`) carry `$Blinc$<Name>$view`
        // so the JIT links to the Rust-side extern; user-declared
        // components carry the default Zyntax inherent-impl
        // mangling `<Name>$view`. Either way we use whatever the
        // registry holds; falling back to `<Name>$view` only when
        // the component isn't in the registry yet (e.g. an
        // unregistered user component the validator missed —
        // shouldn't happen in normal flow).
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
        // expr.ty stays whatever the original marker's type was
        // (Type::Any in practice — the resolver will refine when
        // the component symbol exists).
    }

    fn rewrite_block(block: &mut zyntax_typed_ast::typed_ast::TypedBlock) {
        // Walk + transform. For each statement, rewrite-then-
        // collect: rewrite recurses into expressions (turning
        // marker calls into typed Calls); collect handles
        // component-call-with-body shape (converts body block
        // into a `children: [Widget]` named arg on the call).
        let old_stmts = std::mem::take(&mut block.statements);
        let mut new_stmts: Vec<zyntax_typed_ast::TypedNode<TypedStatement>> =
            Vec::with_capacity(old_stmts.len());
        for mut stmt in old_stmts {
            rewrite_stmt(&mut stmt);
            collect_children_into(&mut new_stmts, stmt);
        }
        block.statements = new_stmts;
    }

    /// Append `stmt` to `out`, handling body-bearing component
    /// calls. The transformation forks on the callee:
    ///
    /// - **Substrate primitives** (callees whose mangled symbol
    ///   matches `$Blinc$<Name>$view`): the body Block becomes
    ///   a `children: [Widget]` named arg on the call. The JIT
    ///   evaluates each child expression eagerly to build the
    ///   widget-handle array, then hands it to the primitive's
    ///   extern (which knows how to build a real `blinc_layout`
    ///   container around the children). Slot markers and non-
    ///   expression statements drop on the floor — control-flow
    ///   inside primitive bodies is a later slice.
    ///
    /// - **User-declared components** (everything else): the
    ///   body Block flattens into the outer statement list — the
    ///   call comes first, then each child statement is inlined
    ///   right after. This is the transitional pre-Phase-2e
    ///   shape; once user component view methods accept an
    ///   implicit `children` param the flatten path goes away.
    ///
    /// - **Slot markers** (`__slot_open__("name")` /
    ///   `__slot_close__()`) get dropped before any of this runs.
    fn collect_children_into(
        out: &mut Vec<zyntax_typed_ast::TypedNode<TypedStatement>>,
        mut stmt: zyntax_typed_ast::TypedNode<TypedStatement>,
    ) {
        // NOTE: do NOT drop slot markers here — the partition
        // logic below relies on them to bucket primitive body
        // blocks into `slot_<Name>` named args. The user-component
        // flatten fallback below filters them out explicitly.

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

    /// If `stmt` is `Expression(Call(Variable("__slot_open__"),
    /// [StringLiteral(name)]))`, return `name`. Otherwise `None`.
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

    /// `Expression(Call(Variable("__slot_close__"), []))` — the
    /// counterpart marker that ends the active slot bucket.
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

    /// Is `call`'s callee a substrate-registered primitive — a
    /// `Variable` whose interned name starts with `$Blinc$` (the
    /// view-symbol prefix `register_blinc_layout_primitives`
    /// uses for `Div`, `Text`, etc.)?
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

/// Pull props off the synthesized `__component_props__` marker
/// method and bind them as leading parameters on every other
/// method in the same impl. Strip the marker after.
///
/// The grammar emits a component `Counter (initial: i32) { view {
/// ... }; fn on_click() { ... } }` as:
///
/// ```text
/// impl Counter {
///     fn __component_props__(initial: i32) { }   // marker
///     fn view() { ... }
///     fn on_click() { ... }
/// }
/// ```
///
/// After this pass:
///
/// ```text
/// impl Counter {
///     fn view(initial: i32) { ... }
///     fn on_click(initial: i32) { ... }
/// }
/// ```
///
/// (The marker is gone.) The call site `Counter(1)` is already
/// lowered by `lower_component_calls` to `Counter$view(1)`, so
/// the prop binds positionally — `initial` becomes `1` inside the
/// view body. The Zyntax JIT sees a normal function with a
/// typed parameter and codegens the load/store as it would for
/// any local.
///
/// Why a marker method instead of e.g. storing props on
/// `TypedTraitImpl` directly: that struct has no "params" slot,
/// and the grammar action language can't synthesize a side-table.
/// A marker method is the cheapest path through the existing AST
/// shapes — the same pattern used for `__fsm_meta__`.
///
/// The pass is idempotent: programs without a
/// `__component_props__` method pass through untouched.
fn bind_component_props(program: &mut TypedProgram) {
    use zyntax_typed_ast::typed_ast::TypedDeclaration;

    for decl in program.declarations.iter_mut() {
        let TypedDeclaration::Impl(imp) = &mut decl.node else {
            continue;
        };

        // Find the `__component_props__` marker and snapshot its
        // params. Take(params) leaves an empty Vec behind so the
        // strip step's drain-by-name is straightforward.
        let prop_params = imp
            .methods
            .iter_mut()
            .find(|m| m.name.resolve_global().as_deref() == Some("__component_props__"))
            .map(|m| std::mem::take(&mut m.params));

        let Some(prop_params) = prop_params else {
            // No marker on this impl — nothing to do. Either the
            // component has no props, or this impl came from a
            // stand-alone `impl Foo { ... }` block.
            continue;
        };

        // Prepend the prop params onto every OTHER method's
        // params list. Order matters: props must come first
        // because the call-site lowers `Counter(1, 2)` to
        // `Counter$view(1, 2)` with the user's positional args
        // matching the prop order.
        for method in imp.methods.iter_mut() {
            if method.name.resolve_global().as_deref() == Some("__component_props__") {
                continue;
            }
            // Convert TypedMethodParam → TypedMethodParam (props
            // come in as TypedMethodParam already because the
            // marker is a TypedMethod, not a TypedFunction).
            let mut new_params = prop_params.clone();
            new_params.extend(std::mem::take(&mut method.params));
            method.params = new_params;
        }

        // Strip the marker so the compile path doesn't try to
        // surface it as a callable `Counter$__component_props__`.
        imp.methods
            .retain(|m| m.name.resolve_global().as_deref() != Some("__component_props__"));
    }
}

/// Wrap every fsm's `__fsm_meta__` body with `__fsm_begin__("FsmName")`
/// at the front and `__fsm_end__()` at the back. The host registers
/// these markers as builtins that push/pop a "current FSM" name on
/// a stack, so the `__fsm_initial__` / `__fsm_transition__` calls
/// inside the body know which fsm they're configuring.
///
/// Why a post-parse pass and not grammar action: same reason the
/// `<FSM>Event` synthesis is post-parse — the action language can't
/// string-concat or reach into the surrounding rule's bindings to
/// pull the FSM name into a child rule's emit. Building the wrapper
/// stmts in Rust is simpler.
///
/// The pass is idempotent in the sense that it only fires on
/// `__fsm_meta__` methods (synthesised by the `fsm` grammar rule);
/// programs without an fsm decl pass through untouched.
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

            // Skip if begin marker already present — defensive
            // against double-application if this pass is ever run
            // twice on the same program.
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

/// Walk a parsed `TypedProgram`, populate the global `FsmRegistry`
/// from each fsm's `__fsm_meta__` body, and strip the meta method so
/// Zyntax doesn't have to compile the (now-redundant) marker calls.
///
/// Three phases:
///
///   1. **Scan**: walk `Impl` decls looking for `__fsm_meta__`,
///      collect each fsm's name + parsed metadata (initial state,
///      event transitions, tick guards) into a buffer.
///
///   2. **Pin TypeIds**: for each fsm we found, mint a `TypeId` via
///      `TypeId::next()` and set the matching Enum decl's `ty` to
///      `Type::Named { id, ... }`. Zyntax's compile path
///      (`runtime.rs:1307-1368`) checks `decl_node.ty` first when
///      registering enum types — if it's `Type::Named`, the embedded
///      id wins; otherwise Zyntax mints its own. By pinning the id
///      here we guarantee the registry's `(module, TypeId)` key
///      matches whatever Zyntax sees later. Then we insert a
///      placeholder `TypeDefinition` ourselves so the
///      `get_type_by_name(...).is_none()` guard at runtime.rs:1308
///      short-circuits — Zyntax skips re-registering and our id is
///      authoritative.
///
///   3. **Strip**: remove `__fsm_meta__` from each fsm's Impl. The
///      marker callees (`__fsm_begin__`, `__fsm_initial__`, etc.)
///      have no extern decls in the program, so leaving the body in
///      place would type-fail. We've already extracted everything
///      the registry needs from those markers — the compiled
///      program doesn't need to call them.
///
/// Why direct AST walking instead of host-builtin marker invocation:
/// the chosen design (begin/end markers + eager population at
/// compile time) maps naturally onto walking the AST in Rust. We
/// already have the marker call shapes in `TypedExpression::Call`
/// form; running them through the JIT just to mutate a host-side
/// HashMap is a long detour for the same result.
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

    // -----------------------------------------------------------------
    // Phase 1: scan. Collect (fsm_name, FsmDefinition) tuples without
    // mutating program declarations. Tick-guard expressions are
    // captured here for lifting into stand-alone functions in
    // phase 2.5 (so they survive `__fsm_meta__` stripping).
    // -----------------------------------------------------------------
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
                        def.transitions.push(EventTransition { from, event, to });
                    }
                }
                "__fsm_tick__" => {
                    // arg 0 = from, arg 1 = guard expr, arg 2 = to.
                    // We lift the guard into a stand-alone function
                    // `__fsm_tick_guard_<FsmName>_<idx>__()` so it
                    // survives the `__fsm_meta__` strip and is
                    // callable as an ordinary Zyntax symbol at
                    // dispatch time.
                    if let (Some(from), Some(to)) = (str_arg(0), str_arg(2)) {
                        let idx = def.tick_guards.len();
                        let fsm_name_str = fsm_name.resolve_global().unwrap_or_default();
                        let guard_fn_name = format!("__fsm_tick_guard_{fsm_name_str}_{idx}__");
                        let guard_fn = InternedString::new_global(&guard_fn_name);

                        // Capture the guard expression (cloned to
                        // escape the read borrow on `program`) for
                        // lifting in the next phase.
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
                _ => {} // skip __fsm_begin__, __fsm_end__, anything else.
            }
        }

        found.push((fsm_name, def));
    }

    // -----------------------------------------------------------------
    // Phase 2: pin TypeIds + populate the registry. Pre-register the
    // type so Zyntax's compile path takes the "name already known"
    // short-circuit and respects our id.
    // -----------------------------------------------------------------
    for (fsm_name, def) in &found {
        let type_id = TypeId::next();

        // Pin `decl.ty` to Type::Named { id: our_id } on the matching
        // enum decl. Zyntax's enum-registration check at
        // runtime.rs:1313 reads exactly this field.
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

        // Pre-register the type so the get_type_by_name(...).is_none()
        // check at runtime.rs:1308 short-circuits and Zyntax doesn't
        // double-register with a fresh TypeId. We synthesise a
        // TypeDefinition mirroring what Zyntax would build for an
        // Enum declaration; downstream uses of TypeRegistry consume
        // this shape directly.
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
            let _ = Visibility::Public; // silence unused-import in case the type_registry-vis path changes upstream
        }

        let id = FsmId { module, type_id };
        with_fsm_registry_mut(|r| r.upsert(id, def.clone()));
    }

    // -----------------------------------------------------------------
    // Phase 2.5: lift each captured tick-guard expression into a
    // stand-alone top-level function. The function returns `i32`
    // (1 if the guard fires, 0 otherwise); the host's `step_tick`
    // tests `!= 0` to decide whether to transition. Body shape:
    //
    //     fn __fsm_tick_guard_<Fsm>_<idx>__() -> i32 {
    //         if <guard expr> { return 1 }
    //         return 0
    //     }
    //
    // Why i32 instead of bool: bool-return ABI marshaling through
    // `runtime.call::<bool>` is untested upstream
    // (`grep -rn 'call::<bool>'` hits zero across the Zyntax tree)
    // and triggers a misaligned-pointer panic in
    // `zyntax_compiler/zrtl.rs:416` during return-value
    // type-meta lookup. Using i32 with a 1/0 convention is a
    // tested ABI and keeps the lifting logic local to this pass.
    //
    // The lifted functions are appended after phase 2 so they
    // inherit any registered TypeIds in scope.
    // -----------------------------------------------------------------
    use zyntax_typed_ast::typed_ast::{TypedFunction, TypedIf};
    for (fn_name, guard_expr) in guards_to_lift {
        let i32_ty = Type::Primitive(PrimitiveType::I32);

        // `return 1` — the then-branch's only statement.
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

        // `if <guard> { return 1 }` — no else branch.
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

        // `return 0` — the body's trailing fallthrough.
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

    // -----------------------------------------------------------------
    // Phase 3: strip `__fsm_meta__` so the compile path doesn't have
    // to resolve the marker callees. The Impl may end up empty (the
    // fsm grammar emits __fsm_meta__ as the impl's only method) —
    // an empty inherent impl is benign.
    // -----------------------------------------------------------------
    for decl in &mut program.declarations {
        let TypedDeclaration::Impl(imp) = &mut decl.node else {
            continue;
        };
        imp.methods
            .retain(|m| m.name.resolve_global().as_deref() != Some("__fsm_meta__"));
    }
}

/// Walk a parsed `TypedProgram` and synthesize a sibling `<FSM>Event`
/// enum for every fsm declaration that has at least one
/// `__fsm_transition__` marker. The synthesized enum's variants are
/// the unique event names referenced by the FSM's transitions, in
/// declaration order.
///
/// # Why a post-parse pass and not grammar action
///
/// Two reasons: (a) the grammar action language doesn't support
/// string concatenation, so we can't build the `<FSM>Event` name
/// from the FSM's own name at parse time; (b) deduplication of
/// event names across many transitions is naturally Rust code,
/// not grammar action code.
///
/// # Runtime contract
///
/// The synthesized enum is the bridge between user-facing event
/// names (`Start`, `Reset`) and `StateTransitions::on_event(event:
/// u32)`. A downstream codegen pass turns the enum into a Rust
/// `#[repr(u32)]` enum + `From<Event> for u32`, then user code
/// dispatches via `loader.dispatch(LoaderEvent::Start.into())`.
/// Tick transitions (`__fsm_tick__`) are guard-driven and don't
/// have user-facing event names, so they never appear in the
/// synthesized event enum.
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
            // Tick-only fsm or no transitions at all — nothing to
            // synthesise. The state enum + `__fsm_meta__` already
            // carry everything the runtime needs.
            continue;
        }

        // Build the variants and the `<FSM>Event` enum name. Use
        // `trait_name` (an InternedString) rather than `for_type`
        // (a Type) — the grammar sets both to the FSM identifier
        // for inherent impls, but `trait_name` gives us the bare
        // name without unwrapping a Type::Named.
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

    // Append synthesised enums after all original declarations so
    // existing `find_map` lookups (which return the first matching
    // decl) keep returning the user-declared state enum / impl.
    program.declarations.extend(event_enums);
}

// =====================================================================
// Runtime-substrate bridge (blinc_runtime::fsm)
// =====================================================================
//
// `blinc_runtime::fsm` is the pure-Rust substrate that lets DSL-
// defined FSMs plug into widget-side `Stateful<FsmStateId>` without
// the widget side knowing whether the DSL was compiled by Zyntax+
// Cranelift (this crate's JIT path) or by Zyntax+LLVM (a future AOT
// codegen crate). Both publishers write to the same
// `FsmRegistry` singleton and install their own `GuardDispatcher`.
// This section is the JIT half of that contract.

/// `GuardDispatcher` impl that routes tick-guard calls through a
/// shared `ZyntaxRuntime` handle. Wraps the same
/// `Arc<Mutex<ZyntaxRuntime>>` `BlincDsl` already owns, so
/// installing one as the process-wide dispatcher costs only the
/// `Arc::clone` it takes to keep the runtime alive while the
/// dispatcher is reachable.
///
/// The call shape (zero args, `i32` return — `1` = guard fires,
/// `0` = doesn't) mirrors what `populate_fsm_registry_pass` emits
/// when lifting guard expressions; see `BlincDsl::step_tick`
/// (~line 2440) for the matching call-site.
struct JitGuardDispatcher {
    runtime: Arc<Mutex<ZyntaxRuntime>>,
}

// SAFETY: `ZyntaxRuntime` is `!Send + !Sync` because Cranelift's
// `JITModule` is — it carries `Box<dyn Fn(LibCall) -> String>` +
// `NonNull` allocator-table pointers that the compiler can't prove
// are race-safe. We pin a `Mutex` around it (BlincDsl::runtime),
// which serialises all access. `JitGuardDispatcher` only ever
// reaches the inner runtime through that Mutex, so concurrent
// callers from different threads queue cleanly rather than racing.
// Production Blinc apps run the UI thread single-threaded anyway,
// so the cross-thread case is hypothetical — the unsafe impl is
// what lets `Arc<dyn GuardDispatcher>` (the substrate's
// `Send + Sync`-bounded trait object slot) hold a JIT dispatcher.
unsafe impl Send for JitGuardDispatcher {}
unsafe impl Sync for JitGuardDispatcher {}

impl blinc_runtime::fsm::GuardDispatcher for JitGuardDispatcher {
    fn call_guard(&self, symbol: &str) -> Option<bool> {
        let runtime = self.runtime.lock().ok()?;
        let guard_sig = NativeSignature::new(&[], NativeType::I32);
        let result = runtime.call_function(symbol, &[], &guard_sig).ok()?;
        // Lifted guards return `1` to fire, `0` not. Match the
        // exact decode `BlincDsl::step_tick` uses.
        Some(matches!(result, ZyntaxValue::Int(v) if v != 0))
    }
}

/// `ViewRenderer` impl that resolves view symbols against a
/// shared `ZyntaxRuntime` handle. Holds an
/// `Arc<Mutex<ZyntaxRuntime>>` plus the parent `BlincDsl`'s
/// `value_returning_views` set, and picks one of two call ABIs:
///
///   - **Value-returning view** (symbol is in the set): call
///     through `runtime.call_function` with a
///     `() -> i64` native signature, capture the returned widget
///     handle as `ZyntaxValue::Int(handle)`. The host-side
///     walker decodes the handle via [`materialize_widget`].
///
///   - **Legacy Unit-returning view** (symbol not in the set):
///     call as `runtime.call::<()>`, return `ZyntaxValue::Void`.
///     The legacy DSL-side `BlincDsl::render_view` drains the
///     scene-op buffer for tests still on the op-stream surface.
///
/// Cost-of-call is the JIT call plus one `Mutex` lock.
/// Renderers are cheap to construct (just two `Arc::clone`s) so
/// callers can hold one or many without worry.
struct JitViewRenderer {
    runtime: Arc<Mutex<ZyntaxRuntime>>,
    value_returning_views: Arc<Mutex<std::collections::HashSet<String>>>,
}

// SAFETY: same argument as `JitGuardDispatcher` — `ZyntaxRuntime`
// is `!Send + !Sync` because Cranelift's `JITModule` carries
// `Box<dyn Fn(LibCall) -> String>` + `NonNull` allocator pointers.
// We serialise access via the surrounding `Mutex`, and production
// Blinc apps run the UI thread single-threaded anyway. The
// unsafe impl is what lets `Arc<dyn ViewRenderer>` (the
// substrate's `Send + Sync`-bounded trait object slot) hold a
// JIT renderer.
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
            let sig = NativeSignature::new(&[], NativeType::I64);
            let result = runtime
                .call_function(symbol, &[], &sig)
                .map_err(|e| blinc_runtime::view::ViewRenderError::Backend(e.to_string()))?;
            // The widget-handle primitive externs return
            // `i64`; `call_function` decodes that as
            // `ZyntaxValue::Int(handle)`. Pass it through
            // unchanged — `materialize_widget` on the caller
            // side takes the `i64` and reclaims the box.
            Ok(result)
        } else {
            runtime
                .call::<()>(symbol, &[])
                .map_err(|e| blinc_runtime::view::ViewRenderError::Backend(e.to_string()))?;
            Ok(ZyntaxValue::Void)
        }
    }
}

/// Mirror DSL component declarations into the runtime-agnostic
/// `blinc_runtime::component::ComponentRegistry`.
///
/// Walks each `TypedDeclaration::Impl` whose `for_type` matches
/// a sibling `TypedDeclaration::Class`. For each such pair:
///
/// - The user-facing component name is the Class's name.
/// - The view-symbol mangling is `<Name>$view` (matches Zyntax's
///   inherent-impl method mangling; same name the call-site
///   lowering uses).
/// - The prop list comes from the `view` method's params,
///   which were injected by `bind_component_props` from the
///   synthesized `__component_props__` marker. Walking params
///   here (vs. re-walking the Class fields or the marker)
///   keeps the publisher robust against future changes to how
///   props get attached to methods — whichever way they end up
///   on the view's signature, this code reads them.
///
/// Prop types that don't map to one of the substrate's
/// supported [`blinc_runtime::component::PropType`] variants
/// are dropped silently (the substrate's surface is opinionated
/// about which primitives apps can introspect). New supported
/// types land in [`zyntax_prop_type`] in lockstep with the
/// substrate enum.
///
/// Runs AFTER `bind_component_props` (which is what produces
/// the param-bearing view method) and AFTER
/// `populate_fsm_registry_pass` (so FSM impls — which have an
/// empty trait_name but a matching Enum, not a Class — don't
/// get accidentally registered as components).
/// Pre-register `blinc_layout` widget primitives in the
/// substrate's `ComponentRegistry`. After this runs, DSL
/// source can call `Div(...)`, `Text(...)`, etc. and the
/// component-call validation + lowering passes treat them
/// just like user-declared components.
///
/// Called once at `BlincDsl::new()`. Idempotent — re-running
/// replaces by name (no FsmId / ComponentId churn).
///
/// **Today** the registration just establishes the names and
/// prop shapes for validation. The view-symbol slot points at
/// `$Blinc$<Name>$view` — those externs aren't wired yet;
/// trying to compile a program that uses them will fail at
/// JIT link time. Subsequent commits land the externs, the
/// grammar pivot to value-returning view bodies, and the
/// `ZyntaxValue` → `blinc_layout::Div` walker.
///
/// **Prop shapes** mirror what we want the DSL surface to
/// look like — minimal subsets of the real `blinc_layout`
/// builder methods, exposed as named args. Adding a prop is
/// one line here + one parameter on the matching extern.
/// Full coverage of `RenderProps` lands incrementally as
/// each field gets a DSL-visible name.
fn register_blinc_layout_primitives() {
    use blinc_runtime::component::{ComponentDefinition, PropDef, Type};
    use zyntax_typed_ast::type_registry::PrimitiveType;
    use zyntax_typed_ast::InternedString;

    let string_ty = Type::Primitive(PrimitiveType::String);

    // `Div { ..children }` — universal container, styled.
    // `children` and `__style` both cross the JIT as `i64`
    // pointer payloads; the lowering passes synthesise the
    // matching call-site Blocks.
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
        ],
    };

    // `Text("hi")` — text leaf. Positional `content` is the
    // only prop today; styling props (color, font_size, etc.)
    // land as later prop entries.
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

    // Suppress unused-imports when nothing further consumes
    // these (e.g., if a future refactor stops needing the
    // InternedString path here).
    let _ = InternedString::new_global("__blinc_layout_primitives_marker__");
}

fn publish_components_to_runtime_registry(program: &TypedProgram) {
    use zyntax_typed_ast::typed_ast::TypedDeclaration;

    for decl in &program.declarations {
        let TypedDeclaration::Impl(imp) = &decl.node else {
            continue;
        };

        // Extract the implementing type's name from
        // `imp.for_type`. For our grammar's component impls
        // this is `Type::Unresolved(name)` (the Impl-construct
        // path in Zyntax's interpreter at
        // runtime2/interpreter.rs:1044). For impls that have
        // been resolved past that stage it may be
        // `Type::Named { id, ... }` — look up the type's name
        // through the program's registry then.
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

        // Only register impls that match a sibling Class —
        // skips FSM impls (which match an Enum) and any stray
        // `impl Foo { ... }` block for a type that doesn't
        // exist as a component.
        let class_match = program.declarations.iter().any(|d| match &d.node {
            TypedDeclaration::Class(c) => c.name == component_name_intern,
            _ => false,
        });
        if !class_match {
            continue;
        }

        // Find the view method. Components without a view body
        // (currently impossible to declare, but defensive) get
        // skipped — there's nothing to introspect-and-render.
        let Some(view_method) = imp
            .methods
            .iter()
            .find(|m| m.name.resolve_global().as_deref() == Some("view"))
        else {
            continue;
        };

        // Each view param becomes a `PropDef`. The substrate
        // takes `zyntax_typed_ast::Type` directly, so we hand
        // the param's `ty` through unchanged — no enum
        // translation, no primitive-only filtering. Complex
        // types (structs, arrays, optionals, ...) land in the
        // substrate as-is; consumers that only understand
        // primitives pattern-match on `Type::Primitive(...)`.
        //
        // The `self` param (currently impossible — components
        // don't declare `self`) gets skipped defensively.
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

/// Translate the local `blinc_dsl_core::FsmRegistry` (Zyntax-typed,
/// uses `InternedString` everywhere) into the runtime-agnostic
/// `blinc_runtime::fsm::FsmRegistry` shape (`Arc<str>`-typed,
/// `u32`-coded state variants) and publish each entry.
///
/// Code-assignment policy:
/// - **State variant codes** come from the FSM's state enum
///   declaration order. Walking `program.declarations` for
///   `TypedDeclaration::Enum` whose name matches the FSM's
///   `trait_name` yields the variants in source order; index = code.
/// - **Event codes** are assigned in first-appearance order across
///   the FSM's transitions. Identical events get one code; new
///   events get the next free index.
///
/// Both policies are deterministic — re-running the publisher on
/// the same source produces identical codes — which is the
/// invariant the `FsmStateId::on_event` path relies on so the
/// widget side and the registry agree on which `u32` means which
/// state.
///
/// Why a separate post-parse pass instead of folding into
/// `populate_fsm_registry_pass`: the existing pass already does
/// quite a bit (FSM scan → TypeId minting → guard lifting →
/// `__fsm_meta__` strip), and the publish step needs the
/// post-lift state of the program (specifically the lifted-guard
/// symbol names) which only become stable after that pass runs.
/// Keeping them separated also makes the JIT-only nature of
/// this publish step obvious — the AOT path will reach the
/// substrate from a codegen-emitted equivalent of this function,
/// not from inside `populate_fsm_registry_pass`.
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

        // Find the matching state-enum declaration. Mid-pipeline
        // FSMs always have one (the grammar emits Class+Impl
        // pairs); skip anything else (non-FSM impls).
        let state_enum = program.declarations.iter().find_map(|d| match &d.node {
            TypedDeclaration::Enum(e) if e.name == fsm_name_intern => Some(e),
            _ => None,
        });
        let Some(state_enum) = state_enum else {
            continue;
        };

        // Read the local registry for this FSM's transitions +
        // initial + tick_guards. If the local entry is missing
        // (FSM wasn't fully processed), bail — same idempotent
        // semantics as the rest of the pipeline.
        let local_def = with_fsm_registry(|r| {
            r.iter()
                .map(|(_, d)| d)
                .find(|d| d.name.map(|n| n == fsm_name_intern).unwrap_or(false))
                .cloned()
        });
        let Some(local_def) = local_def else {
            continue;
        };

        // State names in declaration order. Codes = indices.
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

        // Event codes — first-appearance order in transitions.
        let mut event_names: Vec<std::sync::Arc<str>> = Vec::new();
        let mut event_code_of = |name: zyntax_typed_ast::InternedString| -> u32 {
            let resolved = name.resolve_global().unwrap_or_default();
            let needle: &str = resolved.as_ref();
            if let Some(i) = event_names.iter().position(|n| {
                let n_ref: &str = n.as_ref();
                n_ref == needle
            }) {
                return i as u32;
            }
            event_names.push(std::sync::Arc::from(needle));
            (event_names.len() - 1) as u32
        };

        let transitions: Vec<blinc_runtime::fsm::EventTransition> = local_def
            .transitions
            .iter()
            .filter_map(|t| {
                Some(blinc_runtime::fsm::EventTransition {
                    from_code: state_code(t.from)?,
                    event_code: event_code_of(t.event),
                    to_code: state_code(t.to)?,
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

/// The Blinc DSL runtime. Owns the compiled grammar + the Zyntax
/// runtime engine + the loaded module table.
///
/// Construction is `O(grammar parse)` — measured during the
/// risk-reduction prototype. If it climbs past 50 ms, the plan
/// (ROADMAP §3.9) is to switch to a pre-compiled `.zpeg` baked in via
/// `include_bytes!`.
pub struct BlincDsl {
    grammar: Grammar2,
    // We wrap `ZyntaxRuntime` in `Arc<Mutex<_>>` because the
    // production API (Phase 2 of ROADMAP §3.9) hands the same handle
    // to the hot-reload watcher thread for `runtime.hot_reload(...)`
    // calls. The runtime doesn't currently impl `Send + Sync`
    // upstream (its Cranelift `JITModule` is the blocker), which
    // makes clippy's `arc_with_non_send_sync` fire on construction —
    // we silence it where the `Arc::new` call lives, with the
    // expectation that upstream will eventually add the impls.
    runtime: Arc<Mutex<ZyntaxRuntime>>,
    /// JIT-linker-visible names of view symbols
    /// [`lower_view_to_value_returning`] converted to the
    /// `i64`-returning widget-handle ABI. Populated by
    /// [`Self::compile_source`] / [`Self::parse_to_typed_ast`] (both
    /// run the pass) and consulted by [`JitViewRenderer`] /
    /// [`Self::render_named`] to decide whether to call as
    /// `runtime.call::<()>` (legacy Unit-return) or as
    /// `runtime.call_function(..., NativeType::I64)` (capture a
    /// widget handle).
    ///
    /// `Arc<Mutex<...>>` mirrors the runtime field's shape — the
    /// set accumulates across compiles (monotonically grows;
    /// re-defining a symbol as Unit-returning would leave a stale
    /// entry, but our existing flow only adds value-returning
    /// symbols, never demotes).
    value_returning_views: Arc<Mutex<std::collections::HashSet<String>>>,
    /// Per-file map of JIT function names emitted by the most
    /// recent compile of each path. Populated by `compile_file`
    /// / `compile_directory` so `recompile_file` knows which
    /// symbols belong to a given source.
    compiled_modules: Arc<Mutex<std::collections::HashMap<std::path::PathBuf, Vec<String>>>>,
}

impl BlincDsl {
    /// Build a fresh runtime with the embedded Blinc grammar and all
    /// host builtins pre-registered.
    pub fn new() -> BlincDslResult<Self> {
        // Parse the grammar first so any embedded-grammar bug fails
        // fast before we touch the runtime.
        let grammar = Grammar2::from_source(BLINC_GRAMMAR)?;

        // Single-tier classic runtime for the prototype. The full
        // plan (§3.5) wraps this in a `RuntimeEngine` enum that picks
        // between `ZyntaxRuntime` and `TieredRuntime` based on
        // dev/release profile, mirroring the zynml shape.
        let mut runtime = ZyntaxRuntime::new()
            .map_err(|e| BlincDslError::Compile(format!("runtime init: {e}")))?;

        // Order matters: builtins must be registered before any
        // module load so the JIT linker can resolve `$Blinc$*`
        // symbols when `compile_typed_program` runs. Same rule zynml
        // calls out at zynml/src/lib.rs:199-200, except we do it via
        // `register_function` (statically linked symbols) rather than
        // `load_plugin` (.zrtl from disk). With Grammar2 there's no
        // separate `register_grammar` step — `compile_typed_program`
        // takes the typed AST directly.
        register_builtins(&mut runtime);

        // `register_function` only updates the backend's accumulator.
        // The Cranelift JIT module was constructed at `ZyntaxRuntime::new`
        // time and doesn't see new symbols until rebuild. Plugin loaders
        // do this implicitly; for static `register_function` callers we
        // poke the same hook explicitly. Without this, the first
        // `compile_typed_program` for any module that calls a builtin
        // panics inside Cranelift with "can't resolve symbol".
        runtime
            .finalize_runtime_symbols()
            .map_err(|e| BlincDslError::Compile(format!("finalize symbols: {e}")))?;

        // ZyntaxRuntime isn't Send+Sync today (see field-level
        // comment on `BlincDsl::runtime`). The Arc<Mutex<_>> wrapper
        // is the production-shape handle we'll need once it is.
        #[allow(clippy::arc_with_non_send_sync)]
        let runtime = Arc::new(Mutex::new(runtime));

        // Pre-register `blinc_layout` widget primitives as
        // components in the substrate. After this, DSL source
        // like `view { Div(bg: white) { Text("hi") } }` parses
        // and validates cleanly — `Div` and `Text` look like
        // any other component to `validate_component_calls` /
        // `lower_component_calls`. The runtime-side
        // (extern functions that build real `blinc_layout::Div`
        // / `Text` values, view-body-as-expression grammar,
        // widget tree walker) lands in subsequent commits.
        register_blinc_layout_primitives();

        let value_returning_views = Arc::new(Mutex::new(std::collections::HashSet::new()));
        let compiled_modules = Arc::new(Mutex::new(std::collections::HashMap::new()));

        Ok(Self {
            grammar,
            runtime,
            value_returning_views,
            compiled_modules,
        })
    }

    /// Install this `BlincDsl`'s JIT-flavoured guard dispatcher as
    /// the process-wide `blinc_runtime::fsm::GuardDispatcher`. After
    /// this call, any widget that constructs a
    /// `Stateful<FsmStateId>` can dispatch tick guards through the
    /// substrate without depending on Zyntax types — the dispatcher
    /// routes back into this `BlincDsl`'s Cranelift runtime to call
    /// the lifted guard functions.
    ///
    /// Why opt-in: the dispatcher slot is process-wide. Auto-
    /// installing in [`BlincDsl::new`] would race when multiple
    /// `BlincDsl` instances coexist in the same process (tests do
    /// this routinely; hot-reload pipelines might in production).
    /// Production apps own a single long-lived `BlincDsl` singleton
    /// and call `install_runtime_bridge()` once at startup — no
    /// race, no cost. Tests that need bridge dispatch take a
    /// serialization lock and call this explicitly.
    ///
    /// Replaces any previously-installed dispatcher (last-write-
    /// wins), so hot-reload flows that re-bootstrap the DSL stay
    /// straightforward.
    pub fn install_runtime_bridge(&self) {
        blinc_runtime::fsm::set_guard_dispatcher(std::sync::Arc::new(JitGuardDispatcher {
            runtime: self.runtime.clone(),
        }));
    }

    /// Register a Rust widget that implements [`ExternWidget`].
    ///
    /// This is the primary Rust→DSL surface — almost all callers
    /// (including the [`extern_widget!`](crate::extern_widget)
    /// proc-macro's expansion) use this form:
    ///
    /// ```ignore
    /// dsl.register_extern_widget::<FancyText>()?;
    /// ```
    ///
    /// Pulls the spec from `W::extern_widget_spec()` and forwards
    /// to [`Self::register_extern_widget_spec`]. See that method
    /// for the registration semantics.
    pub fn register_extern_widget<W: ExternWidget>(&self) -> BlincDslResult<()> {
        self.register_extern_widget_spec(W::extern_widget_spec())
    }

    /// Register a Rust-side widget by passing an explicit
    /// [`ExternWidgetSpec`]. Lower-level than
    /// [`Self::register_extern_widget`] — most callers prefer the
    /// trait-based form, which builds the spec from the
    /// [`ExternWidget`] impl.
    ///
    /// Useful when you need to register a widget shape the
    /// proc-macro doesn't yet support (callbacks, custom
    /// marshalling, multi-instance specs from a single Rust type),
    /// or when hand-rolling the integration without depending on
    /// `blinc_macros`.
    ///
    /// Side effects:
    ///
    ///   - Registers `spec.extern_ptr` on the JIT runtime under
    ///     `spec.view_symbol` with a matching ZRTL signature.
    ///   - Re-finalises the runtime's symbol table so subsequent
    ///     `compile_source` calls can JIT-link against the new
    ///     symbol.
    ///   - Adds a [`ComponentDefinition`] to the substrate
    ///     [`ComponentRegistry`] so validation and call-site
    ///     lowering recognise the widget as a callable
    ///     component.
    ///   - Records `spec.view_symbol` in the value-returning view
    ///     set so the renderers pick the `i64`-return ABI.
    ///
    /// Must be called before [`Self::compile_source`] for any
    /// source that uses the widget — otherwise the substrate
    /// validator surfaces "unknown component" and the JIT linker
    /// fails to resolve the symbol.
    ///
    /// [`ComponentRegistry`]: blinc_runtime::component::ComponentRegistry
    /// [`ComponentDefinition`]: blinc_runtime::component::ComponentDefinition
    pub fn register_extern_widget_spec(&self, spec: ExternWidgetSpec) -> BlincDslResult<()> {
        // Build the ZRTL signature for the new symbol — same
        // shape `register_builtins` constructs for the in-tree
        // primitives, just sourced from the spec's owned fields
        // instead of a `'static` descriptor.
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

        // `register_function_typed` requires a `&'static str`
        // for the symbol name (it stores the name on the JIT
        // module's symbol map). Leak the spec's `String` to
        // promote it to `'static`. Widget registrations are a
        // startup-time operation in normal flows — the leak is
        // bounded by the number of widget types in the app, not
        // the number of widget instances or render frames.
        let view_symbol_static: &'static str = Box::leak(spec.view_symbol.into_boxed_str());

        {
            let mut runtime = self
                .runtime
                .lock()
                .expect("BlincDsl runtime mutex poisoned");
            // `register_function_typed` only updates the backend's
            // accumulator; `finalize_runtime_symbols` is what
            // pokes the Cranelift JIT module so the new symbol is
            // resolvable at the next `compile_typed_program`.
            // Same two-step `register_builtins` does at startup.
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

        // Widget-handle externs are value-returning by contract.
        // Record the symbol so `JitViewRenderer::render_named`
        // (and `BlincDsl::render_named`) pick the `i64`-return
        // ABI when invoking it.
        self.value_returning_views
            .lock()
            .expect("value_returning_views mutex poisoned")
            .insert(view_symbol_static.to_string());

        Ok(())
    }

    /// Build a DSL-defined component as a Rust
    /// `Box<dyn ElementBuilder>`, ready to slot into a Rust
    /// widget tree.
    ///
    /// This is the **DSL → Rust** half of the bidirectional
    /// interop story. The companion direction (Rust widgets
    /// callable from DSL) lives at [`Self::register_extern_widget`].
    ///
    /// Prop values pass through as positional Zyntax values in
    /// the order the component's view method declares its
    /// params. Today's surface is positional-only; named-arg
    /// dispatch lands when the prop marshalling layer grows a
    /// param-name index.
    ///
    /// Returns an error if:
    ///
    ///   - `name` isn't in the substrate
    ///     [`ComponentRegistry`] (compile DSL source first, or
    ///     register via [`Self::register_extern_widget`]).
    ///   - The component's view function isn't value-returning
    ///     (only widget-primitive-rooted views are; legacy
    ///     `text(...)` views still produce Unit and can't be
    ///     queried).
    ///   - The JIT call fails (wrong arg count for the resolved
    ///     view signature, etc.).
    ///   - The returned handle is null.
    ///
    /// [`ComponentRegistry`]: blinc_runtime::component::ComponentRegistry
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

        // SAFETY: the handle came straight out of a registered
        // widget-handle extern, all of which use
        // `Box::into_raw(Box::new(WidgetBox::...))` to mint the
        // pointer. `materialize_widget`'s contract is satisfied
        // by construction here.
        let widget = unsafe { materialize_widget(handle) }.ok_or_else(|| {
            BlincDslError::Compile(format!(
                "query({name}): view returned the null handle (extern build failed)"
            ))
        })?;
        Ok(widget.into_element_builder())
    }

    /// Compile a `.blinc` source file. Returns the names of compiled
    /// functions, mirroring the zynml `load_module_file` shape so we
    /// can rely on the same hot-reload pivot later (`runtime.hot_reload`
    /// keyed by function name).
    pub fn compile_source(&self, source: &str, filename: &str) -> BlincDslResult<Vec<String>> {
        let mut runtime = self
            .runtime
            .lock()
            .expect("BlincDsl runtime mutex poisoned");

        // Parse to typed AST. We don't pass plugin signatures
        // because we don't load `.zrtl` plugins; instead we splice
        // extern declarations for our static builtins directly into
        // the parsed program below, mirroring what
        // `Grammar2::inject_builtin_externs` does for plugin
        // symbols. Any parse-time / type / call-site / arity errors
        // here come back with the span machinery Zyntax already
        // provides — we don't add a parallel check layer
        // (ROADMAP §3.6).
        // `parse_with_signatures` runs Zyntax's
        // `inject_builtin_externs` (grammar2.rs:198-315) using the
        // runtime's registered plugin signatures. We populate those
        // signatures in `register_builtins` via
        // `register_function_typed`, so every `@builtin` entry in our
        // grammar gets a properly-typed extern decl in the parsed
        // program. This is the path that makes `concat` /
        // `__fstring_format__` (used by f-string desugaring) plus
        // our direct symbols (`$Blinc$text`, `$Blinc$text_int`,
        // etc.) all type-check + JIT-link cleanly.
        //
        // We don't run our own `inject_builtin_externs` host pass
        // anymore — Zyntax's covers everything in the @builtin
        // table, and our table now lists every builtin we register.
        let mut typed_program = self
            .grammar
            .parse_with_signatures(source, filename, runtime.plugin_signatures())
            .map_err(|e| BlincDslError::Compile(e.to_string()))?;

        // Apply the same post-parse passes parse_to_typed_ast runs,
        // so fsm-bearing programs get marker injection + event-enum
        // synthesis before compilation. Mirrors parse_to_typed_ast
        // — keep these two paths in sync when adding new passes.
        inject_fsm_context_markers(&mut typed_program);
        synthesize_fsm_event_enums(&mut typed_program);
        resolve_signal_calls(&mut typed_program);

        // Validate component calls reference known components, then
        // lower the markers. Validation runs first because it reads
        // the marker shape (StringLiteral name as args[0]); lowering
        // rewrites that shape away. Same ordering as
        // `parse_to_typed_ast`.
        validate_component_calls(&typed_program)
            .map_err(|errors| BlincDslError::Compile(errors.join("\n")))?;
        lower_component_calls(&mut typed_program);
        bind_component_props(&mut typed_program);

        // Eager registry population: walk fsm impls, pin TypeIds,
        // record metadata into the global FsmRegistry, then strip
        // `__fsm_meta__` so the compile path doesn't have to handle
        // the marker callees. The module is hardcoded to "main"
        // since Zyntax compiles every source into a single module
        // today — when per-source modules surface upstream, this is
        // the place to thread the real module name through.
        let module = zyntax_typed_ast::InternedString::new_global("main");
        populate_fsm_registry_pass(&mut typed_program, module);

        // Mirror the (Zyntax-typed) local FSM registry into the
        // runtime-agnostic `blinc_runtime::fsm` substrate so
        // widget-side `Stateful<FsmStateId>` consumers can dispatch
        // transitions through `blinc_runtime::fsm::with_fsm_registry`
        // without depending on Zyntax types. The AOT path's
        // generated init function will write to the same substrate
        // from its own translation; widget code stays unchanged.
        publish_fsms_to_runtime_registry(&typed_program);

        // Mirror component declarations into the runtime-agnostic
        // `blinc_runtime::component` substrate. Runs after
        // `bind_component_props` so the view method's params
        // reflect the prop list — that's where the publisher
        // reads prop names + types from.
        publish_components_to_runtime_registry(&typed_program);

        // Wire view functions to value-returning shape: rewrite a
        // trailing primitive-view call into `Return(Some(call))`
        // and bump the function's return type to widget-handle
        // (`I64`). The pass also records each converted symbol's
        // JIT-linker name into `value_returning_views` so render
        // paths can pick the right call ABI (`call_function`
        // capturing `I64` vs legacy `call::<()>`).
        //
        // MUST run before `ensure_unit_return` so its defensive
        // `Return(None)` doesn't override our value-bearing one.
        {
            let mut vrv = self
                .value_returning_views
                .lock()
                .expect("value_returning_views mutex poisoned");
            lower_view_to_value_returning(&mut typed_program, &mut vrv);
        }

        // Rewrite primitive `children = Array([...])` named args
        // into explicit `__new_child_list__` / `__push_child__` /
        // container-call Block expansions so the JIT can ferry
        // children across the FFI boundary as a single pointer.
        // Must run AFTER `lower_view_to_value_returning` (which
        // expects to see the bare Call shape) and BEFORE
        // `ensure_unit_return` (which only inspects statement-level
        // trailing shape).
        lower_children_arrays_to_blocks(&mut typed_program);

        // Gather inline styling args (`bg`, `opacity`, …) into
        // a `__new_style_overlay__` Block and attach the overlay
        // pointer as `__style` named arg on styled primitives.
        // Must run before `resolve_extern_widget_named_args` so
        // the resolution pass sees a single uniform `__style` arg.
        lower_styling_args_to_overlays(&mut typed_program);

        // Reorder remaining named args on extern primitive calls
        // into positional positions using the substrate
        // registry's prop order. Zyntax's auto-injected extern
        // decls carry synthetic param names (`p0`, `p1`, …), so
        // named-arg → param-name binding doesn't work at the type
        // checker; this pass resolves names against our own
        // registry instead.
        resolve_extern_widget_named_args(&mut typed_program);

        // Belt-and-suspenders: terminate user functions with an
        // explicit `Return(None)` so the body classifier can't infer
        // a value-bearing return from a single trailing `Expression`
        // statement.
        ensure_unit_return(&mut typed_program);

        let function_names = runtime
            .compile_typed_program(typed_program)
            .map_err(|e| BlincDslError::Compile(e.to_string()))?;

        Ok(function_names)
    }

    /// Compile a `.blinc` file off disk. Records the resulting
    /// JIT function names against the path so [`Self::recompile_file`]
    /// can re-emit them on hot reload.
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

    /// Compile every `*.blinc` file directly inside `path`
    /// (non-recursive). Returns a per-file map of the JIT
    /// function names emitted.
    ///
    /// All files share the process-global substrate registry —
    /// component / FSM / signal names must be unique across the
    /// directory or the second compile errors on duplicate
    /// symbols. Imports + module-prefixed mangling land in a
    /// later slice.
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
                // The action language's `.`-split turns `./widgets`
                // into `["", "/widgets"]`; drop empty segments
                // and strip a leading slash on the next so the
                // path joins cleanly.
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

    /// Snapshot of which JIT function names were last emitted
    /// for `path`. `None` if the file hasn't been compiled
    /// through this `BlincDsl` instance.
    pub fn compiled_function_names(&self, path: &Path) -> Option<Vec<String>> {
        self.compiled_modules
            .lock()
            .ok()
            .and_then(|m| m.get(path).cloned())
    }

    /// Parse `.blinc` source to a TypedAST without compiling or
    /// running. Exposed for tests + tooling that want to analyse
    /// the parsed shape (e.g. an LSP, a CI lint, or — at this
    /// prototype stage — assertion tests on grammar rules that
    /// don't need full JIT round-trip).
    ///
    /// The grammar's job is to produce TypedAST; the compiler's
    /// job is to compile it. This entry point exists so we can
    /// test the former in isolation when the latter isn't the
    /// concern.
    pub fn parse_to_typed_ast(&self, source: &str, filename: &str) -> BlincDslResult<TypedProgram> {
        let runtime = self
            .runtime
            .lock()
            .expect("BlincDsl runtime mutex poisoned");

        let mut program = self
            .grammar
            .parse_with_signatures(source, filename, runtime.plugin_signatures())
            .map_err(|e| BlincDslError::Compile(e.to_string()))?;

        // Post-parse FSM passes — both run regardless of whether
        // the source contains an fsm; they no-op on programs that
        // don't have `__fsm_meta__` impls.
        //
        //   1. Inject `__fsm_begin__("FsmName")` / `__fsm_end__()`
        //      around each `__fsm_meta__` body so the host knows
        //      which FSM owns each marker call when the body is
        //      executed by Zyntax (the markers are stateful — they
        //      manipulate a host-side context stack).
        //   2. Synthesise a `<FSM>Event` enum from the unique event
        //      names referenced by `__fsm_transition__` markers.
        //   3. Resolve `signal <name>: <T>` decls into rewrites of
        //      `<name>.get()` to `__signal_get_<T>("<name>")` host
        //      extern calls. Strips the signal-marker decls so the
        //      compile path doesn't see them.
        //
        // The marker calls inside `__fsm_meta__` are then ordinary
        // function calls; Zyntax compiles them like any other call,
        // and the host registers `__fsm_begin__` / `__fsm_end__` /
        // `__fsm_initial__` / `__fsm_transition__` as builtins. No
        // codegen pass — Zyntax owns compilation.
        inject_fsm_context_markers(&mut program);
        synthesize_fsm_event_enums(&mut program);
        resolve_signal_calls(&mut program);

        // Component-call validation runs after the FSM/signal
        // rewrites because earlier passes never introduce or strip
        // `__component_call__` markers — but ordering it here keeps
        // diagnostics consistent ("unknown component" surfaces
        // *after* "unknown signal", same as field-resolution order
        // in most type-checkers).
        validate_component_calls(&program)
            .map_err(|errors| BlincDslError::Compile(errors.join("\n")))?;

        // Lower `__component_call__` markers to regular Call
        // expressions keyed on the component's Variable name.
        // Runs after validation so the validator still sees the
        // marker shape (StringLiteral name as args[0]).
        lower_component_calls(&mut program);

        // Bind component props as leading params on each impl
        // method. Independent of `lower_component_calls` — props
        // are a definition-site concern (which impl method gets
        // the params), call-site lowering is a use-site concern
        // (which symbol to call). Order between the two doesn't
        // matter functionally; running prop-binding after keeps
        // the def-site changes contiguous in the diff.
        bind_component_props(&mut program);

        // Mirror the value-returning view rewrite in
        // `compile_source` so AST-inspection tests see the same
        // shape the JIT will. Runs after `bind_component_props`
        // for the same reason `compile_source` runs it late —
        // we want it to operate on the fully-lowered view bodies.
        // Uses a local set since `parse_to_typed_ast` doesn't
        // touch the JIT-side renderer; the per-DSL set is only
        // populated by the compile path that actually emits JIT
        // symbols.
        let mut local_vrv = std::collections::HashSet::new();
        lower_view_to_value_returning(&mut program, &mut local_vrv);

        // Mirror the children-array → Block expansion so
        // AST tests see the lowered shape.
        lower_children_arrays_to_blocks(&mut program);

        // Mirror the styling-args overlay lowering.
        lower_styling_args_to_overlays(&mut program);

        // And mirror the named-arg → positional rewrite so AST
        // tests can assert the post-resolution shape.
        resolve_extern_widget_named_args(&mut program);

        Ok(program)
    }

    /// Invoke the bare-form `render_view` entry point and drain the
    /// scene buffer.
    ///
    /// For programs of the shape `view { ... }` (no enclosing
    /// `component` block). Component-form programs compile to
    /// `<Name>$render_view` instead — use [`Self::render_component`]
    /// for those.
    pub fn render_view(&self) -> BlincDslResult<Vec<DslOp>> {
        self.render_named("render_view")
    }

    /// Invoke a named component's view and drain the scene buffer.
    ///
    /// For programs of the shape `component <Name> { view { ... } }`
    /// — the component lowers to a `TypedDeclaration::Class` plus an
    /// inherent `TypedDeclaration::Impl` carrying a `view` method.
    /// Zyntax's compiler mangles inherent-impl methods as
    /// `<TypeName>$<method>`, so the view's actual symbol is
    /// `<Name>$view`. This call constructs that symbol and dispatches
    /// to it. Multi-component files work because each component gets
    /// its own distinct mangled symbol.
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
            // Value-returning view: call through the i64-return
            // ABI so the JIT side reads the widget handle out of
            // the matching return register. The legacy `render_view`
            // surface still returns `Vec<DslOp>` — the handle gets
            // discarded for now (the substrate-side ViewRenderer is
            // what flows the handle through to consumers). Test
            // callers that want the handle use `view_renderer()`.
            let sig = NativeSignature::new(&[], NativeType::I64);
            runtime.call_function(fn_name, &[], &sig)?;
        } else {
            runtime.call::<()>(fn_name, &[])?;
        }
        Ok(take_scene_ops())
    }

    /// Return a backend-agnostic view renderer that resolves view
    /// symbols against this `BlincDsl`'s Cranelift runtime.
    ///
    /// Widget code that wants to render DSL views without
    /// depending on this crate holds an
    /// `Arc<dyn blinc_runtime::view::ViewRenderer>`; this method
    /// constructs the JIT-backed implementation. The future AOT
    /// path's per-app crate will provide its own
    /// `AotViewRenderer` from the same trait — widget code
    /// switches between them by storing the right Arc.
    ///
    /// Multiple calls hand out fresh renderers, each pointing at
    /// the same shared runtime. Cheap to call as often as
    /// needed.
    pub fn view_renderer(&self) -> std::sync::Arc<dyn blinc_runtime::view::ViewRenderer> {
        std::sync::Arc::new(JitViewRenderer {
            runtime: self.runtime.clone(),
            value_returning_views: self.value_returning_views.clone(),
        })
    }

    /// Resolve a tick-driven transition. Walks the registered fsm's
    /// `tick_guards` in declaration order, evaluates each whose
    /// `from` matches `current` by JIT-calling its lifted guard
    /// function (`__fsm_tick_guard_<Fsm>_<idx>__`), and returns the
    /// `to`-state of the first guard that fires (returns `true`).
    /// Returns `None` if nothing fires — either no rules match the
    /// current state, or every matching rule's guard returns `false`.
    ///
    /// Match-arm semantics: the first true guard wins. Authors get
    /// priority by source order, the same way `step_event` resolves
    /// event-driven transitions.
    ///
    /// Why this lives on `BlincDsl` and not the registry directly:
    /// dispatch needs both the registry (to find the guard fn name)
    /// and the runtime (to JIT-call it). `BlincDsl` is the natural
    /// owner of both. Plumbing a `&ZyntaxRuntime` through the
    /// registry's API would force every caller to thread it through
    /// — pointless when the existing `BlincDsl` handle already has
    /// what we need.
    pub fn step_tick(
        &self,
        id: &FsmId,
        current: &str,
    ) -> BlincDslResult<Option<zyntax_typed_ast::InternedString>> {
        // Phase 1: snapshot matching (guard_fn, to) pairs and drop
        // the registry lock before reaching for the runtime, so
        // there's no chance of holding both locks at once.
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

        // Phase 2: evaluate in declaration order against the JIT.
        let runtime = self
            .runtime
            .lock()
            .expect("BlincDsl runtime mutex poisoned");

        // Explicit signature: zero args, i32 return. Bypasses the
        // type-meta machinery in `runtime.call`, which doesn't have
        // a registered TypeMeta for user-compiled functions and
        // panics with a misaligned-pointer dereference at
        // `zrtl.rs:416`. `call_function` takes the signature
        // directly and uses Cranelift's known ABI for i32 returns.
        let guard_sig = NativeSignature::new(&[], NativeType::I32);

        for (guard_fn, to) in candidates {
            let Some(name) = guard_fn.resolve_global() else {
                continue;
            };
            let result = runtime
                .call_function(&name, &[], &guard_sig)
                .map_err(|e| BlincDslError::Compile(e.to_string()))?;
            // Lifted guards return 1 if the guard fires, 0 otherwise.
            let fired = matches!(result, ZyntaxValue::Int(v) if v != 0);
            if fired {
                return Ok(Some(to));
            }
        }

        Ok(None)
    }

    /// Set the current value of an i32-typed signal in the
    /// per-thread signal table. Subsequent JIT calls into the
    /// program — including tick-guard evaluations via `step_tick`
    /// and view-body reads — will see the new value.
    ///
    /// Embedders typically wire this to a Blinc `State<i32>` change
    /// callback so DSL guards stay in sync with the host's reactive
    /// state without needing to flow through Zyntax: when the host
    /// signal fires, push the new value into the DSL table here,
    /// then invalidate / re-tick whatever subscribed to it.
    ///
    /// The signal name must match the DSL `signal <name>: i32`
    /// declaration. There's no validation against the FsmRegistry
    /// — unknown names just sit in the table doing nothing, the
    /// JIT lookup at `<name>.get()` time finds them, and that's
    /// the contract. Embedders can verify shape via
    /// `parse_to_typed_ast` if they want to surface typos earlier.
    pub fn set_signal_i32(&self, name: &str, value: i32) {
        blinc_runtime::signal::set_i32(name, value);
    }

    /// Read the current value of an i32-typed signal. Returns
    /// `None` when the signal hasn't been set in this thread —
    /// distinct from "set to 0", which returns `Some(0)`. Useful
    /// for diagnostics; production dispatch goes through
    /// `step_tick` / `step_event` which read the table at JIT
    /// time.
    ///
    /// Delegates to `blinc_runtime::signal::get_i32` — widget
    /// crates that want signal access without depending on the
    /// DSL compiler can call the runtime module directly.
    pub fn get_signal_i32(&self, name: &str) -> Option<i32> {
        blinc_runtime::signal::get_i32(name)
    }

    /// Set the current value of an f64-typed signal. Same shape
    /// as `set_signal_i32` but for `signal <name>: f64`
    /// declarations. Useful for floating-point guards — progress
    /// fractions, timing values, normalised positions.
    pub fn set_signal_f64(&self, name: &str, value: f64) {
        blinc_runtime::signal::set_f64(name, value);
    }

    /// Read the current value of an f64-typed signal. `None`
    /// when unset; `Some(0.0)` when explicitly seeded to zero.
    pub fn get_signal_f64(&self, name: &str) -> Option<f64> {
        blinc_runtime::signal::get_f64(name)
    }

    /// Set the current value of a string-typed signal. Same
    /// shape as the i32 / f64 mirrors but for
    /// `signal <name>: string` DSL declarations.
    pub fn set_signal_string(&self, name: &str, value: impl Into<String>) {
        blinc_runtime::signal::set_str(name, value);
    }

    /// Read the current value of a string-typed signal.
    pub fn get_signal_string(&self, name: &str) -> Option<String> {
        blinc_runtime::signal::get_str(name)
    }
}

/// Builtin descriptor — pairs a DSL-visible symbol name with the
/// `extern "C"` function pointer Cranelift will dispatch to and a
/// signature for the type checker.
///
/// Two roles in one struct:
///
/// - **Runtime registration**: `name` + `ptr` + `arg_count` are the
///   inputs to [`ZyntaxRuntime::register_function`], which makes the
///   symbol resolvable at JIT link time.
/// - **Type-system injection**: `param_types` + `return_type` are
///   used to mint a [`TypedDeclaration::Function`] with `is_external:
///   true` (via [`TypedASTBuilder::extern_function`]) that we splice
///   into every parsed `.blinc` `TypedProgram` before
///   `compile_typed_program`. Without that splice the type inferencer
///   sees `text(...)` as `Type::Any`, the body classifier rewrites
///   `render_view`'s declared `Unit` return to `I64`, and the call
///   site reads register-junk as a boxed value (misaligned-pointer
///   panic). The path `inject_builtin_externs` takes for `.zrtl`
///   plugins is the model — see
///   `zyntax/crates/zyntax_embed/src/grammar2.rs:198-315`.
struct BuiltinDescriptor {
    /// The mangled symbol the DSL emits as the call target. Grammar
    /// rules lower directly to this — no `@builtin` alias indirection
    /// (we drop the alias because we only have one consumer per
    /// symbol in the static-link world).
    name: &'static str,
    /// Argument types, in order. Drives the extern decl's parameter
    /// list and the type checker's call-site validation.
    param_types: &'static [Type],
    /// Return type. `Type::Primitive(PrimitiveType::Unit)` for
    /// builtins that perform a side-effect on the host scene buffer.
    return_type: Type,
    /// `extern "C"` function pointer — the Rust impl that gets
    /// invoked at runtime. Cast to `*const u8` for `register_function`.
    ptr: *const u8,
}

// SAFETY: `BuiltinDescriptor` only stores function pointers and
// `'static` references. Function pointers in Rust are `Send + Sync`;
// we mark this explicitly so the `[BuiltinDescriptor]` table can be
// iterated from any thread without complaints.
unsafe impl Sync for BuiltinDescriptor {}

/// The complete set of host builtins for the prototype slice. Ordering
/// is irrelevant; the registration loop walks all of them.
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
            // Bridges DSL `<name>.get()` (rewritten by
            // `resolve_signal_calls` to `__signal_get_i32("<name>")`)
            // to the per-thread `SIGNAL_TABLE_I32`. The DSL surface
            // for setting values lives on `BlincDsl::set_signal_i32`.
            name: "__signal_get_i32",
            param_types: &[Type::Primitive(PrimitiveType::String)],
            return_type: Type::Primitive(PrimitiveType::I32),
            ptr: blinc_signal_get_i32 as *const u8,
        },
        BuiltinDescriptor {
            // f64 mirror of `__signal_get_i32`. Same DSL surface
            // (`<name>.get()`) routed by `resolve_signal_calls` to
            // this extern when the signal's declared type is `f64`.
            name: "__signal_get_f64",
            param_types: &[Type::Primitive(PrimitiveType::String)],
            return_type: Type::Primitive(PrimitiveType::F64),
            ptr: blinc_signal_get_f64 as *const u8,
        },
        BuiltinDescriptor {
            // String mirror of `__signal_get_i32`. The
            // `resolve_signal_calls` pass already routes
            // string-typed signals here (see the
            // `typed_signal_extern_name` match), but the
            // builtin wasn't registered until this commit —
            // string-typed signals previously failed to JIT-
            // link. Returns a Zyntax length-prefixed string,
            // same layout `$Blinc$text` consumes (leaks the
            // backing buffer for the prototype, see the
            // f-string helpers' module comment).
            name: "__signal_get_string",
            param_types: &[Type::Primitive(PrimitiveType::String)],
            return_type: Type::Primitive(PrimitiveType::String),
            ptr: blinc_signal_get_string as *const u8,
        },
        BuiltinDescriptor {
            // `__fstring_format__` (i32 specialisation). The
            // normalization pass `fstring_to_concat` wraps every
            // non-string f-string part in a
            // `__fstring_format__(part)` call; the @builtin alias
            // routes that DSL-visible name to `$Blinc$format_int`.
            //
            // Today this handles i32 only. Mixing in f64 props
            // would route the wrong-typed value through this i32
            // formatter and produce garbage — a separate
            // `__fstring_format_f64__` builtin (or upstream
            // dispatch infra) is the fix path.
            name: "$Blinc$format_int",
            param_types: &[Type::Primitive(PrimitiveType::I32)],
            return_type: Type::Primitive(PrimitiveType::String),
            ptr: blinc_format_int as *const u8,
        },
        BuiltinDescriptor {
            // `string_concat` — joins two strings. The
            // normalization pass `fstring_to_concat` chains
            // f-string parts via this. Maps to
            // `$Blinc$string_concat` via the @builtin alias.
            name: "$Blinc$string_concat",
            param_types: &[
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::String),
            ],
            return_type: Type::Primitive(PrimitiveType::String),
            ptr: blinc_string_concat as *const u8,
        },
        BuiltinDescriptor {
            // `$Blinc$Text$view(content) -> WidgetHandle (i64)`
            // — the value-returning Text primitive. Pre-
            // registered in the substrate's `ComponentRegistry`
            // by `register_blinc_layout_primitives`; the
            // `lower_component_calls` pass routes user-facing
            // `Text("hi")` calls to this symbol.
            //
            // Returns a raw pointer to a leaked
            // `WidgetBox::Text(...)` cast to i64. The host-side
            // walker reclaims it via [`materialize_widget`].
            name: "$Blinc$Text$view",
            param_types: &[Type::Primitive(PrimitiveType::String)],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_text_view as *const u8,
        },
        BuiltinDescriptor {
            // `$Blinc$Div$view(children, style) -> WidgetHandle (i64)`
            // — value-returning Div primitive. Children consumed
            // from a `__new_child_list__` pointer; visual props
            // applied via a `__new_style_overlay__` overlay (both
            // `0` for the bare-`Div()` case).
            name: "$Blinc$Div$view",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_div_view as *const u8,
        },
        BuiltinDescriptor {
            // `__new_child_list__() -> i64` — allocate a fresh
            // `Vec<WidgetHandle>` and hand back its raw pointer
            // as `i64`. Used by `lower_children_arrays` to
            // synthesise a per-container children buffer that
            // `__push_child__` then populates before the
            // container's view extern consumes it.
            name: "__new_child_list__",
            param_types: &[],
            return_type: Type::Primitive(PrimitiveType::I64),
            ptr: blinc_new_child_list as *const u8,
        },
        BuiltinDescriptor {
            // `__push_child__(list: i64, child: i64)` — append a
            // widget handle to the `Vec` minted by
            // `__new_child_list__`. Returns nothing; the list
            // pointer stays live so the container extern can
            // reclaim it.
            name: "__push_child__",
            param_types: &[
                Type::Primitive(PrimitiveType::I64),
                Type::Primitive(PrimitiveType::I64),
            ],
            return_type: Type::Primitive(PrimitiveType::Unit),
            ptr: blinc_push_child as *const u8,
        },
        // Style-overlay builders. Mirror the child-list pattern:
        // `__new_style_overlay__` mints the overlay, `__set_*`
        // setters populate fields, the widget extern reclaims.
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

/// Project a Blinc-side typed-AST `Type` onto the wire-format
/// `TypeTag` Zyntax expects in a `ZrtlSymbolSig`.
///
/// Only the primitive types we actually use in builtins are
/// represented today. Adding a new variant here is the small,
/// localised change required when a new builtin needs a new param /
/// return type (e.g. when we add `$Blinc$div(...) -> NodeHandle`,
/// we'll mint a `TypeTag` for opaque host handles).
fn type_to_tag(ty: &Type) -> TypeTag {
    match ty {
        Type::Primitive(PrimitiveType::Unit) => TypeTag::VOID,
        Type::Primitive(PrimitiveType::String) => TypeTag::STRING,
        Type::Primitive(PrimitiveType::I32) => TypeTag::I32,
        Type::Primitive(PrimitiveType::I64) => TypeTag::I64,
        Type::Primitive(PrimitiveType::F64) => TypeTag::F64,
        // Add more as the builtin surface grows. Falling through to
        // VOID rather than guessing would silently break codegen, so
        // panic loudly to surface the gap during prototype iteration.
        _ => panic!(
            "blinc_dsl_core: no TypeTag mapping for {ty:?} \
             — extend `type_to_tag` in src/lib.rs when adding new \
             builtin parameter / return types"
        ),
    }
}

/// Project a Blinc-side typed-AST `Type` onto the
/// [`NativeType`] shape Zyntax's `runtime.call_function` expects.
///
/// Companion to [`type_to_tag`] — `type_to_tag` produces the
/// `TypeTag` used in stored ZRTL signatures (consumed by
/// call-site lowering at compile time); this produces the
/// `NativeType` used by ad-hoc `call_function` invocations at
/// runtime ([`BlincDsl::query`], guard dispatch, etc.).
///
/// Returns `Err(&Type)` when the mapping isn't known, so callers
/// can surface a helpful diagnostic naming the offender. Adding
/// new variants here lines up with [`type_to_tag`] — bump both
/// together when the prop type surface grows.
fn type_to_native(ty: &Type) -> Result<NativeType, &Type> {
    match ty {
        Type::Primitive(PrimitiveType::Unit) => Ok(NativeType::Void),
        // Zyntax marshals strings across the FFI boundary as
        // length-prefixed pointer buffers — `NativeType::Ptr` is
        // what `call_function` expects for them.
        Type::Primitive(PrimitiveType::String) => Ok(NativeType::Ptr),
        Type::Primitive(PrimitiveType::I32) => Ok(NativeType::I32),
        Type::Primitive(PrimitiveType::I64) => Ok(NativeType::I64),
        Type::Primitive(PrimitiveType::F64) => Ok(NativeType::F64),
        other => Err(other),
    }
}

/// Build the ZRTL signature for a builtin. The sig is what's stored
/// in `backend.symbol_signatures` and consulted at call-site lowering
/// (see `zyntax/crates/compiler/src/cranelift_backend.rs:2719`).
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

/// Register all `$Blinc$*` builtins on the given runtime, with full
/// signatures so call-site lowering matches the typed AST extern
/// declarations injected by [`inject_builtin_externs`]. Without the
/// signature path, the call site would default-guess `I64` returns
/// and platform-default calling conventions and collide with the
/// extern decl.
///
/// Builtins are static `extern "C"` fns — no plugin discovery, no
/// `.zrtl` files (zynml/src/lib.rs:201-219 is the pattern we
/// explicitly do NOT copy).
fn register_builtins(runtime: &mut ZyntaxRuntime) {
    for b in builtins() {
        let sig = descriptor_to_sig(&b);
        runtime.register_function_typed(b.name, b.ptr, sig);
    }
}

/// Rewrite view functions to be value-returning when their body
/// ends in a substrate widget-primitive call (`$Blinc$<X>$view`).
///
/// Blinc views are retained-mode: a `view { Div() { ... } }` body
/// builds a widget tree and the function returns the root handle
/// (an `i64` carrying a `Box::into_raw`-style pointer) so the
/// JIT-side renderer can hand it back to the embed code.
///
/// This pass walks every function named `render_view` (the
/// bare-form top-level view) and every impl method named `view`
/// (component view) and looks at the last statement:
///
///   - **Last stmt is a primitive-view call**
///     (`Expression(Call($Blinc$<X>$view, ...))`) → rewrite to
///     `Return(Some(call))` and set the declaration's
///     `return_type` to `Primitive(I64)` (the widget-handle ABI
///     type). The JIT then compiles the function with a real i64
///     return register, and [`JitViewRenderer::render_named`]
///     picks it up via [`ZyntaxRuntime::call_function`] with the
///     matching native signature.
///
///   - **Anything else** (`text(...)` op-stream call, a `let`, a
///     conditional without a trailing widget, etc.) → leave alone.
///     The function stays Unit-returning and the legacy
///     scene-buffer drain path keeps working.
///
/// Why detect `$Blinc$<X>$view` specifically rather than `Type::Any`
/// or a generic value-return: a Unit-vs-`Any` declared return forks
/// Zyntax's body classifier into a type-meta path that null-derefs
/// when invoked via the simpler `call::<()>` ABI the legacy paths
/// use. Pinning the return to a concrete primitive (`I64`) keeps
/// every code path on the well-trodden specialised-call road.
///
/// MUST run before [`ensure_unit_return`] so its defensive
/// `Return(None)` doesn't preempt the value-bearing return we
/// install here.
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

    /// `Expression(Call(Variable("$Blinc$<X>$view"), ...))` — the
    /// shape `lower_component_calls` emits for substrate-registered
    /// widget primitives whose registry entry uses the `$Blinc$`
    /// prefix.
    fn is_primitive_view_call_stmt(stmt: &TypedStatement) -> bool {
        let TypedStatement::Expression(expr) = stmt else {
            return false;
        };
        let TypedExpression::Call(call) = &expr.node else {
            return false;
        };
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return false;
        };
        callee
            .resolve_global()
            .as_deref()
            .is_some_and(|s| s.starts_with("$Blinc$") && s.ends_with("$view"))
    }

    /// If the body's last statement is a primitive view call,
    /// rewrite it to `Return(Some(call))` in place and report
    /// success so the caller can bump the function's declared
    /// return type to `Primitive(I64)`.
    fn try_convert_trailing(body: &mut zyntax_typed_ast::typed_ast::TypedBlock) -> bool {
        let Some(last) = body.statements.last() else {
            return false;
        };
        if !is_primitive_view_call_stmt(&last.node) {
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

    for decl in program.declarations.iter_mut() {
        match &mut decl.node {
            TypedDeclaration::Function(func) => {
                if func.is_external {
                    continue;
                }
                if !is_view_name(func.name) {
                    continue;
                }
                let Some(body) = func.body.as_mut() else {
                    continue;
                };
                if try_convert_trailing(body) {
                    func.return_type = widget_handle_type.clone();
                    if let Some(name) = func.name.resolve_global() {
                        value_returning_symbols.insert(name.to_string());
                    }
                }
            }
            TypedDeclaration::Impl(imp) => {
                // Inherent-impl methods get the `<TypeName>$<method>`
                // mangling. Pull the type name once and reuse it for
                // every converted method on this impl.
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
                    if try_convert_trailing(body) {
                        method.return_type = widget_handle_type.clone();
                        if let (Some(t), Some(m)) =
                            (type_name.as_ref(), method.name.resolve_global())
                        {
                            value_returning_symbols.insert(format!("{t}${m}"));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Rewrite every substrate primitive call carrying a
/// `children = Array([...])` named arg (the shape Phase 2c
/// emits for body-block-bearing primitive widgets) into an
/// explicit Block expansion that synthesises a per-container
/// child-list via `__new_child_list__` / `__push_child__`.
///
/// Before (Phase 2c output):
///
/// ```text
/// Call($Blinc$Div$view, [], named=[children = Array([
///     Call($Blinc$Text$view, ["a"]),
///     Call($Blinc$Text$view, ["b"]),
/// ])])
/// ```
///
/// After:
///
/// ```text
/// Block {
///     statements: [
///         let __blinc_children_0 = __new_child_list__();
///         __push_child__(__blinc_children_0, $Blinc$Text$view("a"));
///         __push_child__(__blinc_children_0, $Blinc$Text$view("b"));
///         $Blinc$Div$view(__blinc_children_0);  // trailing => block value
///     ]
/// }
/// ```
///
/// The container extern (`$Blinc$Div$view`) takes the list
/// pointer, walks each handle, materialises the boxed widgets,
/// and folds them into the Div's child list. See
/// [`blinc_div_view`] for the consumption side.
///
/// **Recursion:** runs post-order so nested primitive calls
/// inside a parent's child array get rewritten to Blocks first
/// — the parent's `__push_child__` then receives a Block
/// expression that evaluates the inner container at the right
/// point in source order.
///
/// **Where this runs:** after [`lower_view_to_value_returning`]
/// (so trailing-Return wrapping is settled before the Block
/// transformation alters expression shapes) and before
/// [`ensure_unit_return`] (which only cares about the
/// statement-level trailing shape, not the inner-expression
/// rewrites we do here).
///
/// **What this doesn't yet handle:**
///
///   - User-component primitives (callees not starting with
///     `$Blinc$`) — those still flatten via Phase 2c's fallback.
///   - Children inside non-trivial expressions (`if cond {
///     Div() { ... } else { Span() }`). Walked recursively so it
///     handles direct nesting; if-as-expression and similar
///     compound shapes inside children arrays land later.
fn lower_children_arrays_to_blocks(program: &mut TypedProgram) {
    use zyntax_typed_ast::{TypedCall, TypedDeclaration, TypedExpression, TypedNamedArg};

    /// Counter for unique `__blinc_children_<N>` idents within a
    /// single compile. The N values don't carry semantics; they
    /// just disambiguate so nested blocks don't shadow each
    /// other's let bindings.
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

    /// Post-order rewrite: recurse first so nested primitive
    /// calls in this expression get converted to Blocks before
    /// we look at *this* expression's shape.
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

        // For primitives whose registry advertises child slots
        // (`children` and/or `slot_<Name>` props), gather each
        // slot's Array value into a `__new_child_list__` Block.
        // Missing slots get `__style`-style `0` literal fills.
        // The final call carries each list as a NAMED arg
        // (`children = Var(__list_N)`, `slot_Header = Var(__list_M)`);
        // `resolve_extern_widget_named_args` later resolves
        // those names into positional slots per the registry.
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
            // No body-supplied slots — the `0`-literal fills are
            // already on the call; no Block expansion needed.
            return;
        }

        // Wrap the original call (now with slot named args
        // pointing at the locally-allocated lists) in a Block
        // whose trailing expression is the call.
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

    /// If the call's callee is a substrate-registered primitive
    /// with at least one child-slot prop (named `children` or
    /// `slot_<Name>`), return the slot names in registry order.
    /// Otherwise `None` — leaf primitives (`Text`) and
    /// non-primitives never lower through this pass.
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

    /// Legacy single-child gate, kept for the old call sites
    /// (none remain after this refactor, but the function name
    /// is referenced in earlier docs; remove once those are
    /// retired).
    #[allow(dead_code)]
    fn callee_takes_children(call: &TypedCall) -> bool {
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return false;
        };
        let Some(sym) = callee.resolve_global() else {
            return false;
        };
        let sym: &str = &sym;
        // Strip the JIT-linker envelope to recover the user-
        // visible DSL name. Anything that doesn't match the
        // convention is not a substrate primitive.
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

/// Resolve named args on `$Blinc$<X>$view` extern calls into
/// positional args using the substrate
/// [`ComponentRegistry`]'s prop order as the source of truth.
///
/// Zyntax's `inject_builtin_externs` synthesises extern
/// declarations with generic parameter names (`p0`, `p1`, …) —
/// so a DSL call site like `MyWidget(label = "hi", count = 5)`
/// can't bind by name through the type checker. This pass
/// closes that gap purely in our lowering layer:
///
///   - Looks up the call's substrate definition (strips the
///     `$Blinc$<Name>$view` envelope to recover the user-visible
///     name).
///   - Reads the props list (which the macro / hand-rolled
///     `ExternWidgetSpec` populated in declaration order).
///   - For each `TypedNamedArg` whose name matches a prop, slots
///     its value into the corresponding positional position.
///   - Positional args already on the call fill the *earliest*
///     unclaimed positions; named args land at their named
///     positions. Mixed positional+named call sites work as
///     long as they don't collide on a slot.
///   - Clears `named_args` once everything has landed.
///
/// **Skipped:** non-primitive callees (user-declared DSL
/// components — Zyntax's normal name-based parameter binding
/// handles those via `bind_component_props`); calls referencing
/// a prop name that isn't in the registry (left as a named arg
/// so Zyntax's own diagnostics flag it).
///
/// **Where this runs:** after [`lower_children_arrays_to_blocks`]
/// — the `children = Array(...)` named arg is already gone by
/// the time we look at remaining named args (the children pass
/// extracts it into Block expansion + a positional list-pointer).
/// Before [`ensure_unit_return`] which only cares about
/// statement-level trailing shape.
///
/// [`ComponentRegistry`]: blinc_runtime::component::ComponentRegistry
/// Names recognised as inline styling props in DSL call sites
/// (e.g., `Div(bg = 0xFF0000, opacity = 0.5)`). Each maps to an
/// overlay-setter extern with the matching FFI signature.
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

/// For styled primitives, gather inline styling named args
/// (`bg`, `opacity`, …) into a `__new_style_overlay__` Block and
/// attach the overlay pointer as `__style = <ident>` on the
/// call. Styled primitives are detected by the presence of a
/// `__style` prop in their registry definition.
///
/// Runs after `lower_children_arrays_to_blocks` and before
/// `resolve_extern_widget_named_args`.
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
        // Recurse first — nested calls inside args / blocks get
        // their own named args resolved before we look at the
        // outer call.
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

        // Now look at THIS expression.
        let TypedExpression::Call(call) = &mut expr.node else {
            return;
        };
        let Some(prop_names) = primitive_callee_prop_names(call) else {
            return;
        };
        if call.named_args.is_empty() {
            return;
        }

        // Slot vector sized to the prop count. Existing positional
        // args fill the earliest slots; named args land at their
        // resolved positions.
        let mut slots: Vec<Option<zyntax_typed_ast::TypedNode<TypedExpression>>> =
            (0..prop_names.len()).map(|_| None).collect();
        let existing_positional = std::mem::take(&mut call.positional_args);
        let mut overflow: Vec<zyntax_typed_ast::TypedNode<TypedExpression>> = Vec::new();
        for (i, arg) in existing_positional.into_iter().enumerate() {
            if i < slots.len() {
                slots[i] = Some(arg);
            } else {
                overflow.push(arg);
            }
        }

        // Place each named arg at its named position. Args
        // whose name doesn't match any prop stay in named_args
        // (Zyntax's type checker surfaces the diagnostic).
        let existing_named = std::mem::take(&mut call.named_args);
        let mut unresolved_named: Vec<zyntax_typed_ast::TypedNamedArg> = Vec::new();
        for na in existing_named {
            let Some(name) = na.name.resolve_global() else {
                unresolved_named.push(na);
                continue;
            };
            let name_str: &str = &name;
            if let Some(pos) = prop_names.iter().position(|p| p == name_str) {
                if slots[pos].is_some() {
                    // Conflict: same slot supplied positionally
                    // AND by name. Leave the named arg in place
                    // so the diagnostic surface stays honest.
                    unresolved_named.push(na);
                } else {
                    slots[pos] = Some(*na.value);
                }
            } else {
                unresolved_named.push(na);
            }
        }

        // Materialise the contiguous filled prefix. Unfilled
        // trailing slots get dropped so call arity matches what
        // the user supplied — missing required props surface as
        // a downstream type/link diagnostic, not silent garbage.
        let mut new_positional: Vec<zyntax_typed_ast::TypedNode<TypedExpression>> = Vec::new();
        for slot in slots {
            if let Some(arg) = slot {
                new_positional.push(arg);
            } else {
                break;
            }
        }
        new_positional.extend(overflow);

        call.positional_args = new_positional;
        call.named_args = unresolved_named;
    }

    /// If `call` resolves to a substrate-registered primitive
    /// (`$Blinc$<Name>$view` callee), return its prop names in
    /// declaration order. Otherwise `None`.
    fn primitive_callee_prop_names(call: &TypedCall) -> Option<Vec<String>> {
        let TypedExpression::Variable(callee) = &call.callee.node else {
            return None;
        };
        let sym = callee.resolve_global()?;
        let sym: &str = &sym;
        let name = sym
            .strip_prefix("$Blinc$")
            .and_then(|s| s.strip_suffix("$view"))?;
        blinc_runtime::component::with_component_registry(|r| {
            r.get_by_name(name)
                .map(|def| def.props.iter().map(|p| p.name.to_string()).collect())
        })
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

/// Append a `Return(None)` to the main function so the body
/// classifier can't promote a single trailing Expression into a
/// value-bearing return.
///
/// `body_returns_value` (lowering.rs:1610-1633) treats a body with a
/// single `Expression` statement as value-returning, even when the
/// declaration is `return_type: Type::Unit`. With the extern decls
/// from [`inject_builtin_externs`] in place the call's type is `Unit`
/// rather than `Any`, which avoids most of the damage, but adding an
/// explicit terminator removes any remaining ambiguity and is cheap.
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
            // Impl methods get compiled as free functions under the
            // mangled `<TypeName>$<method>` symbol — they need the
            // same trailing `Return(None)` treatment so the ABI
            // matches what the runtime's `call::<()>` expects (the
            // body classifier infers a value-bearing return from a
            // single trailing `Expression` statement otherwise, and
            // calling that as `::<()>` deref-faults on a null
            // type-meta pointer).
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

    /// Helper: try to compile a `.blinc` source. Returns the
    /// stringified error on failure so tests can assert on the
    /// diagnostic content. Tests don't share a `BlincDsl` instance
    /// because compiling a broken module pokes runtime state we
    /// don't want to leak into a follow-up assertion.
    fn try_compile(source: &str, filename: &str) -> Result<Vec<String>, String> {
        let _ = tracing_subscriber::fmt::try_init();
        let dsl = BlincDsl::new().map_err(|e| e.to_string())?;
        dsl.compile_source(source, filename)
            .map_err(|e| e.to_string())
    }

    /// Smoke test for the prototype slice — round-trips a tiny
    /// `component Foo { view { text("...") } }` should compile to a
    /// callable `Foo$view` symbol. Empty `trait_name` is the
    /// inherent-impl marker — without it, Zyntax's
    /// `lower_impl_block` looks for a `trait Foo` (which doesn't
    /// exist), silently discards the methods, and we get zero
    /// symbols back. Pin the regression.
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

    /// Prop declared but not referenced in body — the prop arg
    /// flows through silently and the unused param doesn't disturb
    /// the compile. Pins the half of `bind_component_props` that
    /// doesn't need name-resolution of the prop inside the body.
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

    /// Baseline probe: literal-only f-string interpolation in a
    /// bare view (free-function body). Validates the
    /// `fstring_to_concat` normalization + SSA path works AT ALL
    /// in our DSL setup before we dig into why impl methods fail.
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

    /// End-to-end: `signal title: string` declared, host writes
    /// via `set_signal_string`, DSL reads via `title.get()`
    /// inside a view, the rendered text reflects the host
    /// value. Proves the string-signal pipeline: extern
    /// registered, ABI matches `$Blinc$text`, signal table
    /// shared across the JIT boundary.
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

        // Update from host, re-render — view picks up the new value.
        dsl.set_signal_string("title", "Updated");
        let ops = dsl.render_view().expect("render_view");
        match &ops[0] {
            DslOp::Text(s) => assert_eq!(s, "hi Updated"),
            other => panic!("expected DslOp::Text(\"hi Updated\"), got {other:?}"),
        }
    }

    /// Unset string signal renders as empty string (matches the
    /// `get_str_or_default` substrate behaviour).
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

    /// Literal-only f-string interpolation inside an impl-method
    /// view body. Verifies the `__fstring_format__` /
    /// `string_concat` Blinc builtins resolve cleanly when the
    /// view runs as an inherent-impl method. The bare-view case
    /// is `render_bare_view_with_fstring_literal`; this is the
    /// same shape inside `component Foo { view { ... } }`.
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

    /// End-to-end: `view { Outer() { Inner() } }` actually
    /// renders both components' scene ops in source order. The
    /// flatten pass extracts the body Block from the parent
    /// call's args and inlines the children right after the
    /// parent. The flat DslOp buffer ends up with the parent's
    /// ops followed by each child's ops.
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

    /// End-to-end: `slot Name { ... }` body items flatten into
    /// the parent's render alongside default children. The slot
    /// markers themselves are dropped at runtime (no host-side
    /// named-slot routing yet for the flat DslOp buffer).
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

    /// Nested composition: `Outer() { Mid() { Inner() } }` — the
    /// flatten is recursive, so all three components' ops appear
    /// in order in the flat DslOp buffer.
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

    /// End-to-end: a prop value bound to a method param,
    /// interpolated into an f-string inside the view body, then
    /// rendered. Exercises the full parse → lower → bind →
    /// compile → JIT loop with all of: component declaration,
    /// component call site, prop binding, multi-part f-string,
    /// and the `__fstring_format__` / `string_concat` builtins.
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

    /// AST-level: after `bind_component_props`, the view method's
    /// params hold the prop list in source order. Doesn't depend
    /// on the f-string runtime wall — purely checks that the pass
    /// itself wires the params on. Pin so future grammar changes
    /// don't accidentally regress the prop → method-param flow.
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

        // Both `view` and `on_click` should have the props bound,
        // in source order. The marker `__component_props__` should
        // be stripped.
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

    /// End-to-end: a bare `view { Counter() }` that calls a defined
    /// component should compose — `lower_component_calls` rewrites
    /// `Counter()` to `Call(Variable("Counter"), ...)`, which the
    /// JIT links against the inherent-impl-mangled `Counter$view`
    /// symbol... or does it? Pin the current behaviour either way.
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
        // If lowering rewrites `Inner()` to `Inner$view()`, the
        // outer view's scene buffer would carry the inner's text.
        // If not — `Inner` is unresolved at link time — compile
        // would have failed above, so reaching here at all already
        // proves that part.
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

    /// End-to-end: compile a component, invoke its view via
    /// `render_component`, verify the scene ops show the text. The
    /// existing `render_component` API calls the bare component
    /// name; after this commit it instead has to call the mangled
    /// `<Name>$view` symbol — the prior implementation predates the
    /// grammar shape and was untested.
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

    /// `view { text("...") }` program through the full pipeline and
    /// checks the host saw the right scene op. If this fails, the
    /// integration is broken end-to-end and the rest of the plan is
    /// blocked on fixing it before phase-2 grammar expansion.
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

    // -----------------------------------------------------------------
    // Component (Class + Impl) parsing tests.
    //
    // Components lower to two TypedDeclarations: a Class for the
    // data shape, and an Impl for the methods. Same idiom as ml.zyn
    // structs + inherent impls.
    // -----------------------------------------------------------------

    /// `component Counter { count: i32, width: i32 }` parses to a
    /// `TypedDeclaration::Class` with two fields.
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

    /// `impl Counter { fn view() { text("hi") } }` parses to a
    /// `TypedDeclaration::Impl`. The interpreter's Impl-walk
    /// (interpreter.rs:1017-1080) unwraps each function into a
    /// `TypedMethod` automatically.
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

        // Empty trait_name is the inherent-impl marker. for_type
        // carries the component identity.
        assert_eq!(impl_block.trait_name.resolve_global().as_deref(), Some(""));
        assert_eq!(impl_block.methods.len(), 1, "expected 1 method (view)");
        assert_eq!(
            impl_block.methods[0].name.resolve_global().as_deref(),
            Some("view")
        );
    }

    // -----------------------------------------------------------------
    // Reactivity tests — `state` keyword wraps the field type in
    // `Type::Named { State, [T] }`. Plain fields stay as bare `T`.
    // The host walks Class.fields, treats State<...>-typed fields
    // as reactive, plain fields as data-only.
    // -----------------------------------------------------------------

    /// `state count: i32` lowers to a `TypedField` whose `ty` is
    /// `Type::Named { name: "State", type_args: [i32] }`. The
    /// State<...> wrapping is the AST-level reactivity marker.
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

        // The `state` keyword wraps the type in `Type::Named { State, [i32] }`.
        match &count_field.ty {
            zyntax_typed_ast::Type::Named { id, type_args, .. } => {
                let name_str = dsl.runtime.lock().ok().map(|_| ());
                // Type::Named's `id` references the program's type
                // registry; resolve it back to a name to verify
                // it's `State`. For now we accept any
                // Named-with-1-arg as state.
                let _ = name_str;
                assert_eq!(
                    type_args.len(),
                    1,
                    "expected one type arg (the inner type), got {type_args:?}"
                );
                // Inner arg should be the i32 primitive.
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
            other => panic!("state field should wrap type in Type::Named, got {other:?}"),
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

        // Field 0: state count → wrapped Named (State<i32>).
        let count_field = &class.fields[0];
        assert_eq!(count_field.name.resolve_global().as_deref(), Some("count"));
        assert!(
            matches!(&count_field.ty, zyntax_typed_ast::Type::Named { .. }),
            "state field should be Type::Named (State<...>), got {:?}",
            count_field.ty
        );

        // Field 1: plain name → bare Primitive(String).
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

    /// state+state in the same field list also parses cleanly.
    /// Pinning this so the false-positive "ambiguity" claim
    /// doesn't get reintroduced by anyone reading the earlier
    /// commit message.
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

    /// Optional split form: `component Name { fields }` + `impl Name
    /// { fn ... }` as separate top-level items. Supported alongside
    /// the folded form for users who want explicit decl-per-rule
    /// shape (or are programmatically generating `.blinc` source
    /// where one decl at a time is easier to emit).
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

    /// Folded `component { fields, view { ... }, fn handler() { ... } }`
    /// emits BOTH a Class and an Impl from one source-level block.
    /// Validates the inlined `concat_list([Class], [Impl])` shape
    /// in the grammar action — relies on the upstream
    /// `get_field_as_decl_list` flatten patch (#7) so the parent
    /// `top_level_items` collector unwraps the nested list.
    #[test]
    fn parse_component_folded() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        let program = dsl
            .parse_to_typed_ast(
                // `view { ... }` does the rendering. Handlers like
                // `on_click` mutate state / call other functions /
                // run general logic — they don't render. The body
                // is empty here because state-mutation syntax
                // (e.g. `count = count - 1`) isn't in the grammar
                // yet (next phase-2 slice). The test still
                // validates that the handler is recognised as a
                // method on the component's Impl.
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

        // One source `component { ... }` block -> two TypedDeclarations.
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
        // Empty trait_name is the inherent-impl marker.
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

        // view is first (prepended), on_click is second.
        assert_eq!(
            impl_block.methods[0].name.resolve_global().as_deref(),
            Some("view")
        );
        assert_eq!(
            impl_block.methods[1].name.resolve_global().as_deref(),
            Some("on_click")
        );
    }

    /// `component Counter (initial: i32, step: i32) { ... }` parses the
    /// props parenthetical and — after `bind_component_props` runs —
    /// binds the props as leading params on the view method (and any
    /// other impl methods). Class.fields holds only the body's
    /// declarations (state/data fields), not the props. Order
    /// matters: props come first in the view's param list because
    /// the call site lowers `Counter(1, 2)` to `Counter$view(1, 2)`
    /// with positional args matching the prop order.
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

        // Class.fields holds only the body's `state count` field —
        // props are bound to methods, not stored as class fields.
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

        // After bind_component_props: the view method has the
        // props as its leading params, in source order.
        let impl_block = program
            .declarations
            .iter()
            .find_map(|d| match &d.node {
                zyntax_typed_ast::TypedDeclaration::Impl(i) => Some(i),
                _ => None,
            })
            .expect("expected an Impl decl");

        // No __component_props__ method left (stripped by the pass).
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

    /// Bare struct-only form: `component Pair { sum: i32 }`. The
    /// bare shape has no methods to bind props to, so the
    /// `(props)` parenthetical is accepted syntactically but the
    /// props get silently dropped — see the grammar comment on
    /// `component_decl`. Class.fields holds only body fields.
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

        // Only the body's `sum` field — `left` / `right` props are
        // dropped because the bare form has no method to bind them
        // onto.
        assert_eq!(class.fields.len(), 1);
        assert_eq!(
            class.fields[0].name.resolve_global().as_deref(),
            Some("sum")
        );
    }

    /// Empty props parens — `component Foo () { ... }` — is legal and
    /// equivalent to no parens. Keeps the call-site / def-site
    /// grammar regular (parser doesn't need a special "must have at
    /// least one prop" rule). This pins that behaviour.
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

    /// `Counter()` inside a view body — statement-position
    /// component call. AFTER the `lower_component_calls` pass, the
    /// callee is `Variable("Counter")` and there are no positional
    /// args (the original `__component_call__` marker and its
    /// leading StringLiteral name have been folded away).
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

        // The bare `view` lowers to a free function — its body is
        // the first user-function body that doesn't belong to the
        // empty `Counter` Class.
        let stmts = first_user_function_body(&program);
        assert_eq!(stmts.len(), 1, "expected 1 stmt, got {stmts:?}");

        let TypedStatement::Expression(expr_node) = &stmts[0].node else {
            panic!("expected Expression statement");
        };
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

    /// `Counter(1, 2)` — after lowering, positional args appear
    /// in source order as `positional_args`, no leading
    /// StringLiteral name.
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
        let TypedStatement::Expression(expr_node) = &stmts[0].node else {
            panic!("expected Expression");
        };
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

        // [IntLiteral(1), IntLiteral(2)] after lowering
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

    /// `Counter(1, step = 2)` — after lowering, the named arg
    /// lifts into the Call's native `named_args` vec as a
    /// `TypedNamedArg`. Positional args stay positional.
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
        let TypedStatement::Expression(expr_node) = &stmts[0].node else {
            panic!("expected Expression");
        };
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

        // Positional: [IntLiteral(1)]
        assert_eq!(call.positional_args.len(), 1);
        let TypedExpression::Literal(TypedLiteral::Integer(one)) = &call.positional_args[0].node
        else {
            panic!("positional arg 0 should be Integer(1)");
        };
        assert_eq!(*one, 1);

        // Named: [step = 2]
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

    /// `let widget = Counter(0)` — component call in expression
    /// position (right-hand side of a `let`). Pins that
    /// `component_call_expr` is reachable through the full
    /// expression precedence chain (it sits in `primary_expr`
    /// alongside the literal / variable / paren alternates).
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
            "after lowering, callee should be the mangled view symbol \
             (not the bare marker)"
        );
    }

    /// `Div` and `Text` are pre-registered by
    /// `register_blinc_layout_primitives()` at
    /// `BlincDsl::new()` time, so DSL source can call them
    /// without the author declaring them. The validation
    /// pass accepts the references; the registry surfaces
    /// the right `view_symbol` for the upcoming lowering /
    /// extern-call wiring.
    #[test]
    fn blinc_layout_primitives_registered() {
        let _ = tracing_subscriber::fmt::try_init();

        // BlincDsl::new() runs `register_blinc_layout_primitives()`.
        let _dsl = BlincDsl::new().expect("runtime init");

        // Substrate registry should know `Div` + `Text` regardless
        // of whether any DSL source has been compiled yet.
        // Div's prop list is intentionally minimal — just
        // `children`, since universal styling (bg, padding, …)
        // is planned to come through the `Styled<W>` overlay
        // rather than per-primitive props.
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

    /// End-to-end: `view { Text("hello") }` compiles cleanly,
    /// the JIT calls `$Blinc$Text$view`, the Rust impl boxes a
    /// `blinc_layout::Text` and returns the pointer. `render_view`
    /// (legacy DslOp path) drains an empty scene buffer
    /// because the new extern doesn't push anything — the
    /// widget handle flows back through the function return
    /// instead.
    ///
    /// This is the smallest possible value-returning view —
    /// proves the architecture works end-to-end before Div +
    /// children + props compound the complexity. The host-side
    /// walker that reclaims the handle lands in Phase 2e.
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

    /// `validate_component_calls` errors when a `__component_call__`
    /// marker references a component that wasn't declared. Catches
    /// typos at parse time rather than letting them slip through to
    /// the (less-helpful) symbol-resolution stage later. The error
    /// message names the failing component and suggests the
    /// declaration form.
    #[test]
    fn validate_rejects_unknown_component() {
        let _ = tracing_subscriber::fmt::try_init();

        // Unique name so the global component registry (which
        // other tests publish into in parallel) doesn't
        // accidentally have an entry named `Counter` when the
        // validator reads it. Using
        // `UnknownComponentValidateTest` keeps this regression
        // test isolated from cross-test interference.
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

    /// Forward references — the component is declared AFTER its
    /// first use. The validation pass collects all known classes
    /// before checking calls, so source order doesn't matter. Pins
    /// that we don't accidentally re-introduce an ordering
    /// constraint via the validate pass.
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

    /// Multiple unknown calls report multiple errors — the
    /// validation pass collects ALL failures rather than bailing on
    /// the first. Helps users fix every typo in a single
    /// compile/test cycle instead of one at a time.
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

    /// Bare lowercase `counter(0)` must NOT match
    /// `component_call_expr` — the capital-first `component_name`
    /// terminal is the explicit disambiguator. The PEG should
    /// backtrack and try `assign_stmt` (which fails because there's
    /// `Counter() { Inner(1) }` — body block on a component call.
    /// After lowering, a body-bearing `Counter() { Inner(1);
    /// Inner(2) }` becomes a flat statement sequence: the parent's
    /// `Counter$view()` call followed by each child statement
    /// inlined at the parent level. The body Block is consumed —
    /// no trailing arg, no nesting. Models "children render after
    /// parent" semantics for the flat DslOp scene buffer.
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
        // 3 stmts: Counter$view(); Inner$view(1); Inner$view(2)
        assert_eq!(
            stmts.len(),
            3,
            "expected flat [Counter$view, Inner$view, Inner$view], got {} stmts",
            stmts.len()
        );

        fn callee_name(stmt: &zyntax_typed_ast::TypedNode<TypedStatement>) -> Option<String> {
            let TypedStatement::Expression(e) = &stmt.node else {
                return None;
            };
            let TypedExpression::Call(c) = &e.node else {
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

        // The parent call has NO body-Block arg anymore (children
        // were extracted and inlined). Counter takes no props, so
        // positional_args should be empty.
        let TypedStatement::Expression(e) = &stmts[0].node else {
            unreachable!()
        };
        let TypedExpression::Call(c) = &e.node else {
            unreachable!()
        };
        assert_eq!(
            c.positional_args.len(),
            0,
            "Counter$view should have no args after body inlining, got: {:?}",
            c.positional_args
        );
    }

    /// A bare-form `view { Text("hi") }` ending in a substrate
    /// widget-primitive call gets rewritten to
    /// `Return(Some(...))` and its declared return type is bumped
    /// from `Unit` to widget-handle (`I64`). Pins Phase 2d's
    /// value-returning view shape.
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

        // Return type bumped to the widget-handle ABI type.
        assert_eq!(
            render_view.return_type,
            Type::Primitive(PrimitiveType::I64),
            "render_view should return I64 (widget handle) after value-returning rewrite"
        );

        // Body's last statement is now a Return(Some(call)) — not
        // a bare Expression.
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

    /// A view whose trailing statement is `text("...")` (the
    /// legacy lowercase op-stream extern, not a substrate widget
    /// primitive) must NOT get the value-returning rewrite — that
    /// extern returns Unit and the legacy `render_view` -> scene
    /// buffer path expects a Unit-returning function.
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

        // Stays Unit-returning — the lowercase `text(...)` extern
        // is op-stream / Unit.
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

    /// Pin: slot bodies on a substrate-primitive call partition
    /// into `slot_<Name>` named args + a default `children` named
    /// arg. Inspects the AST after parse_to_typed_ast — by then
    /// children-array lowering has minted a child-list per slot.
    #[test]
    fn slot_bodies_partition_into_named_args() {
        let _ = tracing_subscriber::fmt::try_init();

        // Pre-register a synthetic widget with children + slot
        // props so the partition has somewhere to route the slot
        // bodies. Real users go through `register_extern_widget`,
        // but for an AST-shape test we just plant the
        // `ComponentDefinition` directly.
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

        // The body should have lowered through children-arrays
        // → Block expansion. Walk into the Block and look for
        // the trailing call's named-arg shape *before* the
        // resolve_extern_widget_named_args pass — wait, that
        // pass already ran in parse_to_typed_ast. So we'll just
        // assert there's a final call carrying `__blinc_children_*`
        // variables, AND that the prelude has TWO __new_child_list__
        // let bindings (one for default children, one for Header).
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

    /// `Text(content = "hi")` — a leaf primitive with one prop
    /// invoked by name — lowers to a positional call thanks to
    /// `resolve_extern_widget_named_args`. Pins the named→
    /// positional reorder explicitly so it can't regress
    /// silently into an empty positional list (which would JIT
    /// to garbage register reads).
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

    /// Substrate container primitives lower their body Block
    /// into a `__new_child_list__` + `__push_child__` + final
    /// container-call Block expansion (the
    /// `lower_children_arrays_to_blocks` shape). Asserts the
    /// post-lowering AST end-to-end: a trailing
    /// `Return(Some(Block({ let, push, push, Div(__list) })))`
    /// with each push consuming a primitive child call.
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

        // Phase 2d wrapped the trailing expression in
        // `Return(Some(...))`; the children-array pass then
        // rewrote the inner Call into a Block expansion.
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

        // Expected statement sequence (4 entries):
        //   0. let __blinc_children_N = __new_child_list__()
        //   1. __push_child__(__blinc_children_N, Text$view("a"))
        //   2. __push_child__(__blinc_children_N, Text$view("b"))
        //   3. $Blinc$Div$view(__blinc_children_N)   <-- block value
        assert_eq!(
            block.statements.len(),
            4,
            "expected 4 stmts (let + 2 pushes + final call), got: {block:?}"
        );

        // Statement 0: the let.
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

        // Statements 1 + 2: __push_child__(list, Text$view("..."))
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
            // arg 0: Variable refers to the same list ident as the let.
            let TypedExpression::Variable(list_ref) = &push_call.positional_args[0].node else {
                panic!("__push_child__ arg 0 should be the list ident Variable");
            };
            assert_eq!(
                list_ref.resolve_global().as_deref(),
                Some(list_ident.as_ref())
            );
            // arg 1: Text$view call.
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

        // Statement 3: trailing Div call carrying the list ident.
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
        // Div takes (children, __style). The styling-args pass
        // resolves `__style` to a positional `0` literal because
        // this DSL source supplied no styling args.
        assert_eq!(
            div_call.positional_args.len(),
            2,
            "Div takes (children, __style)"
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

    /// Body with a `let` binding + component child. The body
    /// Block flattens into the parent statement list — the `let`
    /// rides between the parent call and the child call. Note:
    /// this is just the AST-shape assertion; whether the `let`
    /// binding's value flows into the child call at runtime is a
    /// scope question (a flat statement sequence puts the let in
    /// the OUTER scope, not the inner component's scope).
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
        // 3 stmts: Wrapper$view(); let count = 1; Inner$view(count)
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

        // Flatten output:
        //   Tabs$view(); Tab$view(1); Tab$view(2)
        //
        // The `slot Header { Tab(1) }` lowered to
        //   `__slot_open__("Header"), Tab$view(1), __slot_close__()`
        // (linear marker pair from the slot rule's
        // `concat_list(...)`). flatten then DROPS both markers and
        // inlines `Tab$view(1)` at the parent level. `Tab(2)` was
        // already a default child; it appears next.
        assert_eq!(
            stmts.len(),
            3,
            "expected flat [Tabs$view, Tab$view, Tab$view] after stripping slot markers"
        );

        fn callee_name(stmt: &zyntax_typed_ast::TypedNode<TypedStatement>) -> Option<String> {
            let TypedStatement::Expression(e) = &stmt.node else {
                return None;
            };
            let TypedExpression::Call(c) = &e.node else {
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

        // No slot markers anywhere — strip is complete.
        for s in stmts {
            let name = callee_name(s).unwrap_or_default();
            assert!(
                !name.starts_with("__slot_"),
                "found leftover slot marker `{name}` in flattened output"
            );
        }
    }

    /// `lower_component_calls` strips the unused defensive
    /// `__component_call__` callee identifier from the AST. After
    /// the pass, there should be no `__component_call__` variable
    /// references anywhere in the program. Pin this so a future
    /// regression where the rewrite forgets to swap the callee
    /// shape is caught immediately.
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

        // Walk the program's expression tree and count any
        // remaining `__component_call__` references.
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

    /// no `=`) and then fail to parse the statement, returning a
    /// parse error. Pins that the disambiguation rule actually
    /// disambiguates.
    #[test]
    fn parse_lowercase_call_is_not_component_call() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        // `counter(0)` isn't a valid Blinc statement — lowercase
        // function calls only land via the typed `text(...)` rules
        // today. So parse should fail (not silently treat `counter`
        // as a component).
        let result = dsl.parse_to_typed_ast(r#"view { counter(0) }"#, "lowercase_call.blinc");
        assert!(
            result.is_err(),
            "lowercase `counter(0)` should not parse as a component call"
        );
    }

    /// Single-prop component on the bare form — `component Only
    /// (just_one: i32) { }` parses cleanly. With no view/method to
    /// bind the prop to, the bare form silently drops it (see the
    /// `component_decl` grammar comment). Class.fields ends up
    /// empty. This pins the regression where the
    /// `prop_decl_list` rule miscounts when there's only the head
    /// `prop_decl` and no `prop_decl_tail` repetitions.
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

        // Bare form drops props — no method to bind them onto.
        assert_eq!(class.fields.len(), 0);
    }

    /// `text(N)` round-trip — probes the i32 ABI through Cranelift.
    /// Confirms (a) the integer terminal in the grammar lowers to a
    /// real `IntLiteral`, (b) PEG backtracks from the string variant
    /// of `text(...)` and matches the int variant, (c) Zyntax
    /// type-checks the call against `$Blinc$text_int`'s `(i32) ->
    /// ()` signature, (d) Cranelift passes the value as an actual
    /// i32 register, (e) the host receives it without ABI corruption.
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

    // -----------------------------------------------------------------
    // F-string parsing tests — TypedAST shape only.
    //
    // At this prototype stage the grammar's job is to produce the
    // right TypedAST and we verify that. JIT round-trip + runtime
    // concat / format dispatch is the compiler's concern (Zyntax owns
    // the SSA backend; what shape its f-string handling takes for a
    // non-`println` caller is something we'll address when the
    // compiler integration matures, not here).
    // -----------------------------------------------------------------

    use zyntax_typed_ast::typed_ast::{TypedExpression, TypedLiteral};
    use zyntax_typed_ast::TypedDeclaration;

    /// Pull the body statements out of the parsed program's first
    /// non-extern function. Test-only helper.
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

    /// `text(f"hello")` — single-part f-string with no
    /// interpolation. Zyntax's `fold_concat` short-circuits the
    /// single-part case (interpreter.rs:2365-2370) and returns the
    /// bare expression, so the parsed AST should look identical to
    /// `text("hello")`.
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
        // text("hello") -> the only positional arg is a string literal.
        assert_eq!(call.positional_args.len(), 1);
        let TypedExpression::Literal(TypedLiteral::String(_)) = &call.positional_args[0].node
        else {
            panic!(
                "expected single string-literal arg, got {:?}",
                call.positional_args[0].node
            );
        };
    }

    /// `text(f"{42}")` — single interp part. fold_concat's
    /// short-circuit returns the bare `__fstring_format__(42)` call
    /// (interpreter.rs:2365-2370 + the wrapper from
    /// `f_string_interp`). We assert the AST has that one call as
    /// the arg of `text`.
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
        // text(__fstring_format__(42))
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

    /// `text(f"answer: {42}!")` — multi-part f-string. fold_concat
    /// builds `__fstring__(text_lit, fmt_call_stripped, text_lit)`
    /// (interpreter.rs:2372-2410). We assert the AST has that
    /// shape: one `__fstring__` call with three positional args.
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

    // -----------------------------------------------------------------
    // Expression-layer parsing tests — variable refs, binary
    // arithmetic, assignment statements. Phase-2 minimal slice.
    //
    // Same TypedAST-only verification approach as the f-string tests
    // — we assert the grammar produces the right shape and let the
    // compiler handle codegen.
    // -----------------------------------------------------------------

    /// `text(f"{count}")` — interpolating a bare variable reference.
    /// fold_concat short-circuits the single-part case and returns the
    /// `__fstring_format__(count)` call directly. We assert the call
    /// arg is a `TypedExpression::Variable`, proving variable refs
    /// flow through `f_string_expr → expr → primary_expr`.
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
        // The arg of __fstring_format__ should now be a Variable("count")
        // rather than an integer literal.
        assert_eq!(fmt_call.positional_args.len(), 1);
        let TypedExpression::Variable(arg_name) = &fmt_call.positional_args[0].node else {
            panic!(
                "expected Variable arg, got {:?}",
                fmt_call.positional_args[0].node
            );
        };
        assert_eq!(arg_name.resolve_global().as_deref(), Some("count"));
    }

    /// `count = count + 1` inside a method body. Lowers to a
    /// `TypedStatement::Expression` wrapping `Binary(Variable, Assign,
    /// Binary(Variable, Add, IntLiteral))`. The Class's reactivity
    /// marker (`State<i32>`) is irrelevant here — at parse time
    /// assignment is just an Assign Binary, regardless of whether the
    /// target is reactive. The compiler / a later pass decides whether
    /// to lower to `count.set(count.get() + 1)`.
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

        // Find the on_click method.
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

        // Statement: Expression(Binary(Variable("count"), Assign, ...)).
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

        // LHS: Variable("count").
        let TypedExpression::Variable(target) = &outer.left.node else {
            panic!("expected Variable target, got {:?}", outer.left.node);
        };
        assert_eq!(target.resolve_global().as_deref(), Some("count"));

        // RHS: Binary(Variable("count"), Add, IntLiteral(1)).
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

    /// Multiplicative binds tighter than additive: `1 + 2 * 3` should
    /// parse as `Binary(1, Add, Binary(2, Mul, 3))`, not
    /// `Binary(Binary(1, Add, 2), Mul, 3)`. Pinning this so a future
    /// well-meaning grammar refactor doesn't accidentally flatten the
    /// precedence ladder.
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
        // RHS is Add at the top with Mul nested on the right.
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

    /// Parens override precedence: `(1 + 2) * 3` should parse as
    /// `Binary(Binary(1, Add, 2), Mul, 3)`. Confirms `paren_expr`
    /// passes its inner expression straight through (no wrapper node)
    /// and feeds the multiplicative chain.
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
        // RHS top-level should be Mul; the LHS of Mul is the
        // (1 + 2) Add subtree.
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

    /// `let derived = count + 1` — immutable local binding.
    /// Lowers to `TypedStatement::Let` with `mutability ==
    /// Immutable`, `ty == Type::Any` (no annotation, compiler
    /// infers), and an initializer Binary expression.
    ///
    /// Why we test this end-to-end: the grammar action reads
    /// `is_mutable: false` / `type_annotation: None`, but the
    /// interpreter's "Let" construction
    /// (runtime2/interpreter.rs:381-403) rewrites those into the
    /// real TypedLet shape. Easy to break by changing field names.
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

    /// `if count > 0 { text("positive") } else { text("zero") }` —
    /// conditional rendering. Asserts (a) `>` parses as a
    /// comparison Binary at the condition, (b) both branches
    /// produce TypedBlocks with the right number of statements,
    /// (c) the else_block is populated.
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

        // Condition is `count > 0` — Binary with BinaryOp::Gt.
        let TypedExpression::Binary(cond) = &if_stmt.condition.node else {
            panic!("expected Binary condition");
        };
        assert!(
            matches!(cond.op, zyntax_typed_ast::BinaryOp::Gt),
            "expected Gt, got {:?}",
            cond.op
        );

        // then-branch has one text("positive") stmt.
        assert_eq!(if_stmt.then_block.statements.len(), 1);
        // else-branch present, also one stmt.
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

    /// ES6-style namespacing: `LoaderState.Loading` parses via
    /// the existing postfix field-access layer as
    /// `Field { object: Variable("LoaderState"), field: "Loading" }`.
    /// No path/`::` grammar is required — the compiler / type
    /// resolver decides at lowering time whether `LoaderState` is
    /// an enum type (variant access) or a regular value (field
    /// access). Same AST shape either way; matches the
    /// JS / TypeScript mental model the DSL targets.
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

    /// `a && b` lowers to `Binary(_, And, _)`. Single AND case;
    /// the next test covers precedence with OR.
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

    /// `a || b && c` parses with AND binding tighter than OR:
    /// top op is Or, RHS is `Binary(b, And, c)`. Pinning the
    /// precedence ladder so a future grammar refactor can't flip
    /// `||` and `&&`.
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

    /// Method-call expressions parse via the postfix layer.
    /// `count.get()` lowers to TypedExpression::MethodCall with
    /// receiver=Variable("count"), method="get", no args. This is
    /// the shape state-field reads will take once the deps-list
    /// view + ViewCtx::get(N) → State<T> chain is wired.
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
    /// Confirms `call_args_list` parses comma-separated args and
    /// the integer literal flows through.
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

    /// Method calls compose with comparisons: `count.get() > 0`
    /// parses as Binary(MethodCall, Gt, IntLiteral). Pinning the
    /// precedence — postfix should bind tighter than binary
    /// operators, so the method-call subtree is on the LHS of the
    /// comparison rather than the comparison being inside the
    /// method's args.
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
        // LHS is the method call.
        let TypedExpression::MethodCall(_) = &cmp.left.node else {
            panic!("expected MethodCall on LHS, got {:?}", cmp.left.node);
        };
    }

    /// `view([state1]) {|ctx| stmts}` — explicit-deps closure form.
    /// Lowers to a function `view(ctx)` whose body starts with a
    /// synthesised `__view_deps__(state1)` marker call, followed
    /// by the user's stmts. Asserts (a) the function has one
    /// parameter named "ctx", (b) the first statement is the
    /// marker call carrying the right deps, (c) user stmts come
    /// after the marker.
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

        // (b) first body stmt is `__view_deps__(count, width)`.
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

        // (c) marker args are Variable refs with the right names.
        for (i, expected) in ["count", "width"].iter().enumerate() {
            let TypedExpression::Variable(name) = &marker.positional_args[i].node else {
                panic!("expected Variable arg at {}", i);
            };
            assert_eq!(name.resolve_global().as_deref(), Some(*expected));
        }

        // (d) user statements follow the marker. body[1] should be
        // the `let c = ctx.get(0)` stmt.
        let TypedStatement::Let(_) = &body.statements[1].node else {
            panic!(
                "expected user `let` stmt after marker, got {:?}",
                body.statements[1].node
            );
        };
    }

    /// Plain `view { stmts }` still works alongside the deps form
    /// — `view_member` is `view_with_deps | view_simple`. The
    /// simple form should produce a parameterless function with
    /// no `__view_deps__` marker.
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
        // No params on the simple form.
        assert_eq!(view.params.len(), 0);
        // No marker — first stmt is the user's text("hi"), not a
        // __view_deps__ call.
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

    /// `if a { ... } else if b { ... } else { ... }` lowers to a
    /// recursive nested-If shape: the outer If's else_block holds a
    /// single-statement block whose only statement is another If
    /// (with its own else_block holding the final block). Pinning
    /// the shape so a future grammar refactor doesn't break the
    /// chain wrapping convention.
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

        // Outer If: condition `count > 100`, then `text("big")`,
        // else_block is a single-stmt block wrapping the next If.
        let TypedStatement::If(outer) = &view.statements[0].node else {
            panic!("expected outer If");
        };
        let outer_else = outer.else_block.as_ref().expect("outer else");
        assert_eq!(
            outer_else.statements.len(),
            1,
            "else block should hold one statement (the chained If)"
        );

        // Chained If: condition `count > 10`, then `text("medium")`,
        // else_block is a one-stmt block with `text("small")`.
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

        // Tail else.
        let tail_else = chained.else_block.as_ref().expect("chained else (tail)");
        assert_eq!(
            tail_else.statements.len(),
            1,
            "tail else holds text(\"small\")"
        );
    }

    /// 4-arm chain `if / else if / else if / else if / else` walks
    /// to nested depth 4. Pinning that PEG recursion in `if_stmt`
    /// doesn't bottom out at any chain length.
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

        // Walk down the chain — each level should hold a single
        // `If` in its else_block, with a final tail `else`.
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
            // If the only stmt in else is another If, descend.
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

    /// `else if` without trailing else: `if A { } else if B { }`.
    /// The chained inner If has `else_block: None`. Pinning the
    /// no-tail-else case so the recursion handles it correctly.
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

    /// `if count > 0 { ... }` with no else — `else_block` is None.
    /// Pinning the simple form so a future "always emit empty
    /// else" refactor doesn't sneak in.
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

    // -----------------------------------------------------------------
    // FSM declaration tests — `fsm Name { state X, initial Y, on
    // X.Event -> Z }`. Verify the grammar emits the right two-decl
    // shape (Enum + Impl) and the metadata marker calls inside
    // `__fsm_meta__`.
    // -----------------------------------------------------------------

    /// `fsm Loader { ... }` emits BOTH a TypedDeclaration::Enum
    /// (states as variants) and a TypedDeclaration::Impl (carrying
    /// the metadata via `__fsm_meta__`). Pinning that the
    /// `concat_list` lowering shape stays right — easy to break by
    /// switching list helpers during a refactor.
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
        // Variant names match.
        for (i, expected) in ["Idle", "Loading", "Done"].iter().enumerate() {
            assert_eq!(
                enum_decl.variants[i].name.resolve_global().as_deref(),
                Some(*expected),
                "variant {i}"
            );
        }

        // Inherent Impl with a single `__fsm_meta__` method.
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

    /// `__fsm_meta__` body layout after both post-parse passes:
    ///
    ///     [0] __fsm_begin__("FsmName")
    ///     [1] __fsm_initial__("InitialState")
    ///     [2..n-1] __fsm_transition__ / __fsm_tick__ markers
    ///     [n-1] __fsm_end__()
    ///
    /// Begin/end wrap the body so the host knows which fsm owns
    /// the markers in between. This test pins (a) the begin
    /// marker is at body[0] with the right FSM name, (b) the
    /// initial marker is at body[1] with the right state name,
    /// (c) the end marker is at the last index.
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

        // Helper to extract (callee, args) at a given index.
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

    /// Each `on State.Event -> Next` lowers to one
    /// `__fsm_transition__("State", "Event", "Next")` marker call.
    /// Verifies all three string args carry the right names and the
    /// markers appear after the initial-state marker (preserving
    /// declaration order so the runtime can interpret them
    /// sequentially).
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

        // begin + initial + 3 transitions + end = 6 stmts.
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
            // Body layout: [0]=begin, [1]=initial, [2..]=transitions,
            // [last]=end. Transitions start at body[2].
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

    /// `tick From -> To when <expr>` — data-guarded transition.
    /// Lowers to a `__fsm_tick__("From", <guard expr>, "To")`
    /// marker call. The middle arg is the raw expression (NOT a
    /// string literal) because the runtime/compiler reads it to
    /// lower into the body of the `StateTransitions::on_tick`
    /// impl. Pinning all three arg shapes: from is a string lit,
    /// guard is a Binary (the expression survives parsing intact),
    /// to is a string lit.
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

        // begin + initial + tick + end = 4 stmts. Tick is at body[2].
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

        // arg 0: from = "Loading" string literal.
        let TypedExpression::Literal(TypedLiteral::String(from)) = &call.positional_args[0].node
        else {
            panic!("expected string literal arg 0");
        };
        assert_eq!(from.resolve_global().as_deref(), Some("Loading"));

        // arg 1: guard = Binary(MethodCall, Gt, IntLiteral(100)).
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

        // arg 2: to = "Done" string literal.
        let TypedExpression::Literal(TypedLiteral::String(to)) = &call.positional_args[2].node
        else {
            panic!("expected string literal arg 2");
        };
        assert_eq!(to.resolve_global().as_deref(), Some("Done"));
    }

    /// Mixed event and tick transitions in the same fsm — both
    /// shapes coexist inside `__fsm_meta__`. Pinning declaration
    /// order survives so the runtime can interpret transitions
    /// sequentially (event-driven and data-guarded share priority,
    /// resolved by the order users wrote them).
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

        // begin + initial + 3 transitions + end = 6.
        assert_eq!(body.statements.len(), 6);

        // Helper to read a marker callee name from a stmt.
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

        // Body[0] = __fsm_begin__("Loader")
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

        // Last stmt = __fsm_end__()
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

    /// Post-parse synthesis: an fsm with event-driven transitions
    /// gets a sibling `<FSM>Event` enum appended to the program's
    /// declarations. Variants are the unique event names from the
    /// FSM's `__fsm_transition__` markers, in declaration order.
    /// Pinning that the bridge between user-facing event names
    /// (`Start`, `Reset`) and `StateTransitions::on_event(u32)` is
    /// emitted at parse time, ready for codegen.
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

        // Two enums in the program: the state enum (Loader) and
        // the synthesised event enum (LoaderEvent).
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

    /// Duplicate event names across transitions (e.g. `Click`
    /// reused on multiple from-states) get deduped — the event
    /// enum has at most one variant per unique name. Order is the
    /// first-seen position.
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

    /// Tick-only fsm has no `__fsm_transition__` markers and so
    /// gets no event enum synthesised. The state enum + impl are
    /// the only fsm-related decls. Confirms the pass doesn't emit
    /// an empty stub when there are no events.
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

    /// `fsm` with no transitions still parses — useful for the
    /// degenerate "states without yet-defined behaviour" stub. The
    /// body should have exactly the initial marker, no transitions.
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
        // begin + initial + end = 3.
        assert_eq!(
            body.statements.len(),
            3,
            "stub fsm body should be begin + initial + end"
        );
    }

    // -----------------------------------------------------------------
    // FsmRegistry data-structure tests. These verify the registry's
    // public API in isolation. The end-to-end "compile a fsm
    // source, see registry populate" path lands in the next commit
    // when the host marker builtins are wired up — for now we
    // exercise upsert/get/remove/iter directly so the API is pinned
    // before integration arrives.
    // -----------------------------------------------------------------

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

    /// Distinct `FsmId`s (different modules, same TypeId) hash and
    /// compare distinctly so two same-named fsms in different
    /// modules don't collide. Same TypeId across modules can happen
    /// because TypeIds come from a process-global counter — pinning
    /// the (module, type_id) tuple semantics.
    #[test]
    fn fsm_id_disambiguates_by_module() {
        let a = fid("foo", 7);
        let b = fid("bar", 7);
        let c = fid("foo", 7);
        assert_ne!(a, b, "different modules → different ids");
        assert_eq!(a, c, "same (module, type_id) → equal");
    }

    /// Upsert + get round-trip with the expected shape, including
    /// the initial state, transitions, and tick guards. Exercises
    /// the bulk of the data API.
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
                },
                EventTransition {
                    from: intern("Loading"),
                    event: intern("Done"),
                    to: intern("Success"),
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

    /// Re-inserting the same id replaces the entry (used by
    /// hot-reload paths). Pinning that the second upsert wins so
    /// stale state from prior runs doesn't leak.
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

    /// `remove` drops the entry and returns the prior value;
    /// subsequent `get` returns None. Mirrors the hot-reload-of-a-
    /// removed-fsm scenario where dispatch should fail loudly.
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

    /// End-to-end: compiling a fsm-bearing program populates the
    /// global FsmRegistry. Pinning that the (module, TypeId) →
    /// FsmDefinition wiring works through compile_source. Each test
    /// scopes by a unique fsm name so the global registry doesn't
    /// collide across tests in the same process.
    #[test]
    fn compile_source_populates_fsm_registry() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        // Distinct fsm name per test to avoid stepping on the global
        // registry from concurrent test runs.
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

        // Find the registry entry by name (we don't know the
        // assigned TypeId from outside).
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

    /// Each tick guard lifts into a stand-alone top-level
    /// function. The function name lands on the TickGuard so
    /// future dispatch can resolve it via `runtime.call::<bool>`.
    /// Pinning the naming convention
    /// (`__fsm_tick_guard_<Fsm>_<idx>__`) so a future refactor
    /// that changes the format breaks loudly here.
    #[test]
    fn compile_source_lifts_tick_guards_to_functions() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        // Two tick guards on the same fsm so we exercise the
        // index suffix — guard 0 and guard 1. Guards use bare
        // integer comparisons so the lifted bodies don't need
        // signal/symbol resolution we haven't wired up yet (the
        // shape we care about here is the lifting + naming, not
        // dispatch evaluation).
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

        // Both guard functions should appear in the compiled
        // function-name list returned from compile_source.
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

        // And the registry entry should reference both names on
        // its TickGuard records.
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
    /// Pinning that the marker walker recognises `__fsm_tick__` and
    /// extracts the (from, to) pair (the guard expression itself is
    /// stripped for now — see `TickGuard` doc).
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

    /// Dispatch round-trip: compile a fsm, find it by name, walk a
    /// full transition cycle, verify each step lands on the
    /// expected state. End-to-end coverage of the dispatch API
    /// against a registry populated by `compile_source`.
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

        // find_by_name resolves the (module, TypeId) from a bare
        // string at the boundary.
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

    /// Non-matching transitions return `None`. Three failure modes
    /// covered: unknown event, wrong from-state, and unknown fsm
    /// id. Pinning these together ensures callers can use
    /// `Option::is_some` as a "transition is legal" predicate
    /// without false positives.
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

        // (a) unknown event for the current state.
        let miss = with_fsm_registry(|r| r.step_event(&id, "Off", "DoesNotExist"));
        assert!(miss.is_none(), "unknown event should miss");

        // (b) right event but wrong from-state.
        //     Click is defined on Off and On but not on a state
        //     that doesn't exist — verify the from-match isn't
        //     loose.
        let miss = with_fsm_registry(|r| r.step_event(&id, "Nowhere", "Click"));
        assert!(miss.is_none(), "wrong from-state should miss");

        // (c) unknown fsm id (TypeId that doesn't correspond to a
        //     registered fsm).
        let phantom = FsmId {
            module,
            type_id: TypeId::new(u32::MAX),
        };
        let miss = with_fsm_registry(|r| r.step_event(&phantom, "Off", "Click"));
        assert!(miss.is_none(), "phantom fsm id should miss");
    }

    /// Serializer for tests that depend on the process-wide
    /// `blinc_runtime::fsm::GuardDispatcher` slot. `BlincDsl::new()`
    /// installs a JIT dispatcher pointing at its own runtime; if
    /// two tests construct `BlincDsl` in parallel, the slot races
    /// and a later `FsmStateId::on_tick` may dispatch into a
    /// runtime that doesn't have the expected guard symbol. Tests
    /// that route tick dispatch through the substrate take this
    /// lock at entry; tests that drive `dsl.step_tick(...)`
    /// directly don't need it (they hold the per-dsl runtime
    /// mutex instead). Cleared automatically when the
    /// `MutexGuard` drops at test exit.
    static BRIDGE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// End-to-end: DSL component declarations publish into the
    /// runtime-agnostic `blinc_runtime::component` substrate.
    /// After `compile_source`, devtools / hot-reload / prop-
    /// validator code can introspect components by name without
    /// touching `BlincDsl` — same pattern as the FSM bridge.
    #[test]
    fn publish_components_to_runtime_registry_round_trip() {
        let _ = tracing_subscriber::fmt::try_init();
        // Serialize against other tests that write to the
        // global `blinc_runtime::component::ComponentRegistry`
        // — many tests compile `component Counter { ... }` of
        // various shapes, and the publisher replaces by name
        // (hot-reload semantics), so parallel runs race over
        // which `Counter` definition is current.
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

        // Both components should be registered in the substrate
        // under their unique-to-this-test names. Using
        // distinctive names avoids the global-registry race
        // where other tests' `component Counter { ... }` /
        // `component Greeting { ... }` compilations overwrite
        // ours under parallel test execution.
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

    /// End-to-end: a widget-shaped consumer (held as
    /// `Arc<dyn ViewRenderer>` — no `BlincDsl` reference)
    /// renders a DSL view and gets back the scene ops.
    /// Mirrors how an actual app would store the renderer:
    /// construct from `BlincDsl::view_renderer()` once at
    /// startup, hand the `Arc` to widget code, never touch the
    /// DSL crate again.
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

        // Cast to the substrate trait — proves the rest of this
        // test couldn't reach for any `BlincDsl`-specific method
        // if it wanted to.
        let renderer: std::sync::Arc<dyn blinc_runtime::view::ViewRenderer> = dsl.view_renderer();

        // Bare view: `view { Greeting() }` runs Greeting's view
        // (inlined by `lower_component_calls`'s child flatten).
        // Substrate-public `render_main` returns `ZyntaxValue`
        // (today `Void` — the legacy DslOp scene-buffer drain
        // happens inside `BlincDsl::render_view` separately).
        // Phase 2 of the pivot will make views return real
        // widget trees here.
        let main_value = blinc_runtime::view::render_main(&renderer).expect("render_main");
        assert_eq!(main_value, ZyntaxValue::Void);

        // Direct component invocation: same shape.
        let comp_value =
            blinc_runtime::view::render_component(&renderer, "Greeting").expect("render_component");
        assert_eq!(comp_value, ZyntaxValue::Void);

        // Unknown component → `NotFound` (well — actually
        // routes through Cranelift's symbol resolution and
        // surfaces as `Backend` because that's how the JIT
        // returns "no such symbol"). Pin whichever shape is
        // produced so the contract is clear.
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

    /// Rust → DSL interop: a user-defined widget registered via
    /// `register_extern_widget` is callable from DSL source like
    /// any built-in primitive. The extern wraps a real
    /// `blinc_layout::Text` in `WidgetBox::Custom`; the round-trip
    /// proves both the registration plumbing (JIT thunk + substrate
    /// registry + value-returning-set entry) and the `Custom`
    /// payload decode.
    #[test]
    fn register_extern_widget_rust_to_dsl_round_trip() {
        let _ = tracing_subscriber::fmt::try_init();

        // User-defined Rust extern. Convention: matches what the
        // planned `#[extern_widget]` proc-macro would generate.
        extern "C" fn fancy_text_view(content_ptr: *const i32) -> i64 {
            if content_ptr.is_null() {
                return 0;
            }
            // SAFETY: Zyntax's String FFI hands the matching
            // length-prefixed buffer when the registered param
            // type is `String`.
            let content = unsafe { blinc_string_decode(content_ptr) };
            let widget = blinc_layout::text::Text::new(content);
            // Land in the `Custom` variant — that's the path any
            // non-`Text`/`Div` user widget takes.
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

        // DSL source uses the user widget the same way it'd use a
        // built-in primitive — no syntactic distinction.
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

        // SAFETY: handle came straight out of `fancy_text_view`
        // above, which builds `WidgetBox::Custom(Box::new(Text))`.
        let widget =
            unsafe { materialize_widget(handle) }.expect("non-null handle should decode to Some");
        assert!(
            matches!(*widget, WidgetBox::Custom(_)),
            "expected WidgetBox::Custom — the variant user widgets land in"
        );
    }

    /// DSL → Rust interop: a DSL-declared component is buildable
    /// from Rust code via `dsl.query(...)`. Compiles a
    /// `component MyContainer { view { Div() } }`, then queries it
    /// from Rust and asserts the returned
    /// `Box<dyn ElementBuilder>` reports itself as a `Div`.
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

    /// Querying a component whose view ends in a non-primitive
    /// call (so it doesn't get the value-returning rewrite) errors
    /// rather than silently returning garbage. Pins the "only
    /// value-returning views are queryable" contract.
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

        // `Box<dyn ElementBuilder>` isn't `Debug`, so use the
        // err() projection rather than `expect_err`.
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

    /// Querying a name that isn't registered surfaces a clear
    /// diagnostic rather than crashing in the JIT linker.
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

    /// End-to-end children plumbing: `view { Div() { Text("a")
    /// Text("b") } }` builds a real `blinc_layout::Div` whose
    /// `children_builders()` returns the two `Text` widgets in
    /// source order. Pins the
    /// `lower_children_arrays_to_blocks` →
    /// `__new_child_list__` / `__push_child__` →
    /// `blinc_div_view` consume-and-fold path.
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

        // Div now lands in Custom(Styled<Div>). The Styled
        // wrapper delegates `children_builders` to the inner.
        let widget = unsafe { materialize_widget(handle) }.expect("non-null handle");
        let WidgetBox::Custom(builder) = *widget else {
            panic!("expected WidgetBox::Custom (Styled<Div>)");
        };
        assert_eq!(builder.children_builders().len(), 2);
    }

    /// Nested container composition: `Div { Div { Text } }`
    /// round-trips correctly. The inner Div has 1 Text child,
    /// the outer Div has 1 Div child. Pins recursive
    /// `lower_children_arrays_to_blocks` behaviour — each
    /// nesting level mints its own `__blinc_children_<N>` list
    /// without leaking into siblings.
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

        // Outer Div lands in Custom(Styled<Div>). The Styled
        // wrapper delegates children to the inner Div.
        let widget = unsafe { materialize_widget(handle) }.expect("non-null handle");
        let WidgetBox::Custom(outer) = *widget else {
            panic!("outer should be a Custom(Styled<Div>)");
        };
        let outer_children = outer.children_builders();
        assert_eq!(outer_children.len(), 1, "outer Div should have 1 child");

        // Inner Div is also a Styled<Div> with 1 Text child;
        // `Styled<W>::build` delegates so the element_type_id
        // reflects whatever the inner widget reports — for an
        // inner Div from `blinc_layout::div::Div::new()` that's
        // `ElementTypeId::Div`.
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
            // 16711680 = 0xFF0000 → red. Compare via channels
            // since Color doesn't impl PartialEq.
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
        // Brush doesn't impl PartialEq — compare is_some() instead.
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

    /// `view { Div() }` — the value-returning Div primitive
    /// returns a non-zero handle that decodes back to a
    /// `WidgetBox::Div`. The Div is empty (no children plumbed
    /// through the JIT array-arg path yet, but the round-trip
    /// works for the container shape).
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

        // SAFETY: see `jit_view_renderer_round_trip_value_returning_text`.
        // Div now wraps itself in `Styled<Div>` and lands in
        // the Custom variant; the bare Div variant is reserved
        // for unstyled raw construction.
        let widget =
            unsafe { materialize_widget(handle) }.expect("non-null handle should decode to Some");
        assert!(
            matches!(*widget, WidgetBox::Custom(_)),
            "expected WidgetBox::Custom (Styled<Div>)"
        );
    }

    /// End-to-end: a DSL-defined FSM round-trips through the
    /// runtime-agnostic `blinc_runtime::fsm` substrate. After
    /// `compile_source` runs, the FSM should be registered in
    /// the global runtime registry, callable from widget code
    /// (or any other consumer) via `FsmStateId` without that
    /// consumer depending on Zyntax types.
    ///
    /// This pins the JIT half of the JIT/AOT contract: both
    /// publishers (this one, and a future `blinc_dsl_aot`-style
    /// build-time codegen) must produce identical substrate
    /// state for the same source — same state codes in the same
    /// declaration order, same event codes in first-appearance
    /// order, same guard symbol names.
    #[test]
    fn publish_to_runtime_registry_round_trip() {
        let _ = tracing_subscriber::fmt::try_init();
        // Serialize with any other test that takes the bridge
        // lock and dispatches through the global slot. Other
        // tests that don't touch the bridge run in parallel
        // because `BlincDsl::new()` no longer auto-installs the
        // dispatcher — that's now an explicit
        // `install_runtime_bridge()` call.
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
        // are first-appearance order: Start = 0, Finish = 1.
        use blinc_runtime::blinc_layout::stateful::StateTransitions;
        let loading = state.on_event(0).expect("Idle + Start should transition");
        assert_eq!(loading.variant, 1);
        assert_eq!(loading.state_name().as_deref(), Some("Loading"));

        // Tick dispatch: Loading + (1 > 0 always fires) → Done.
        // Routes through the JitGuardDispatcher installed by
        // BlincDsl::new(), which JIT-calls the lifted guard fn.
        let done = loading.on_tick().expect("guard `1 > 0` should fire");
        assert_eq!(done.variant, 2);
        assert_eq!(done.state_name().as_deref(), Some("Done"));
    }

    /// Tick dispatch fires when the lifted guard returns `true`.
    /// `1 > 0` always evaluates to `true`, so step_tick should
    /// return the to-state.
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

    /// Tick dispatch returns `None` when the lifted guard returns
    /// `false`. `1 < 0` is always false, so no transition.
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

    /// Multiple guards from the same from-state: declaration order
    /// wins. The first guard whose expression returns true short-
    /// circuits the rest. Pin this against future refactors that
    /// might re-order guards or evaluate them in parallel.
    #[test]
    fn step_tick_first_true_guard_wins() {
        let _ = tracing_subscriber::fmt::try_init();

        let dsl = BlincDsl::new().expect("runtime init");
        // Two guards from Loading: the first is always true (so it
        // fires), the second would also be true (but never gets a
        // chance). Verifies "first" semantics — if step_tick eval'd
        // in arbitrary order or all-at-once, this would flake.
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

    /// No tick guard matches the current from-state → `None`.
    /// Covers two related cases in one: the from-state has no
    /// guards at all (Done has none), and a state that doesn't
    /// exist as a from in any rule.
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

        // Done has no tick rules — should miss.
        let from_done = dsl.step_tick(&id, "Done").expect("step_tick");
        assert!(from_done.is_none(), "Done has no tick rules");

        // Phantom state name with no rules — should also miss.
        let from_phantom = dsl.step_tick(&id, "DoesNotExist").expect("step_tick");
        assert!(from_phantom.is_none(), "phantom from-state should miss");
    }

    // -----------------------------------------------------------------
    // Signal-resolved guard tests. The `signal <name>: <T>` decl
    // gets stripped before compile, and every `<name>.get()`
    // method call becomes a `__signal_get_<T>("<name>")` extern
    // call. Tests verify the rewrite shape on parsed AST — the
    // host-builtin side (registering `__signal_get_i32` and a
    // signal-value table) lands in a follow-up commit.
    // -----------------------------------------------------------------

    /// `count.get()` on an `i32` signal lowers to
    /// `__signal_get_i32("count")`. Verifies (a) the rewrite
    /// happened (the original MethodCall is gone), (b) the new
    /// callee is `__signal_get_i32`, (c) the signal name is
    /// preserved as a string literal arg.
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

        // The signal_decl extern should be stripped — no more
        // top-level Function named `count`.
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

        // The rewrite happens inside `__fsm_meta__`'s body — the
        // tick-guard expression is the second arg to a
        // `__fsm_tick__("from", <guard>, "to")` marker call. After
        // the rewrite, that <guard> is `Binary(Call(__signal_get_i32,
        // "count"), Gt, IntLit(100))`. Lifting into a top-level
        // function happens later, in compile_source's
        // populate_fsm_registry_pass — at parse_to_typed_ast level
        // the markers are still in place.
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

        // Walk body for the __fsm_tick__ marker; arg[1] is the
        // (now-rewritten) guard expression.
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
        // Signal name preserved as the first string-literal arg.
        assert_eq!(call.positional_args.len(), 1);
        let TypedExpression::Literal(TypedLiteral::String(name)) = &call.positional_args[0].node
        else {
            panic!("expected string-literal name arg");
        };
        assert_eq!(name.resolve_global().as_deref(), Some("count"));
    }

    /// `name.get()` on a `string` signal lowers to
    /// `__signal_get_string("name")`. Pinning the per-type extern
    /// dispatch — same DSL surface, different host extern based
    /// on the signal's declared return type. The signal usage
    /// goes through a `let` initializer (which accepts any
    /// expression) since `text(...)` doesn't yet accept method
    /// calls — the rewrite walker handles let initializers.
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
        // body: let s = __signal_get_string("username")
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

    /// Multiple signals in the same program — each gets its
    /// rewrite based on its declared type. Verifies the pass
    /// builds a signal table from ALL signal_decl markers, not
    /// just the first one.
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

        // Both signal-marker decls stripped.
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

        // Verify each signal has its expected extern in some call
        // somewhere. We just look at the program-wide presence of
        // both __signal_get_i32 and __signal_get_string callees.
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

    // -----------------------------------------------------------------
    // Host-machinery + end-to-end signal-guard tests. The DSL
    // surface (`signal <name>: i32`, `<name>.get()`) gets
    // rewritten to `__signal_get_i32("<name>")` calls; the host's
    // `blinc_signal_get_i32` reads from the per-thread
    // `SIGNAL_TABLE_I32`; `step_tick` JITs the rewritten guard
    // and dispatches based on the live signal value.
    // -----------------------------------------------------------------

    /// Round-trip: set signal → 200, compile a guard `> 100`,
    /// step_tick → fires (200 > 100). Pinning the host extern +
    /// table + JIT path stays connected. Uses a unique signal
    /// name so the per-thread table doesn't bleed across tests.
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

    /// Same shape but signal value below threshold → no fire.
    /// Together with the above test, pins both branches of the
    /// guard against the live signal table.
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

    /// Mutating the signal between step_tick calls flips the
    /// guard's outcome. Pinning that the table is read at JIT
    /// time, not snapshotted at compile time — i.e. the guard
    /// expression dispatches to the live value, the way DSL
    /// authors expect from a "reactive signal" abstraction.
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

        // First tick: signal is 0 → guard misses.
        assert!(dsl.step_tick(&id, "Idle").unwrap().is_none());

        // Update signal to a value above threshold.
        dsl.set_signal_i32("e2e_mut", 999);

        // Second tick on the same compiled program: now fires.
        let next = dsl.step_tick(&id, "Idle").expect("step_tick");
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Hot"),
            "after raising the signal, the guard should fire"
        );
    }

    // -----------------------------------------------------------------
    // FsmInstance bridge tests. Verifies the dependency-free
    // bridge between the DSL fsm registry and host code:
    // construct → current() / dispatch_event() / tick() / reset().
    // No widget integration or Stateful coupling — just the live
    // string-keyed state that embedders can wrap however they want.
    // -----------------------------------------------------------------

    // -----------------------------------------------------------------
    // Float-literal + f64-signal tests. Verify the new
    // primary_expr alternate produces a FloatLiteral, the
    // `signal <name>: f64` decl routes `.get()` to
    // `__signal_get_f64`, and a guard like
    // `progress.get() >= 1.0` fires the JIT path correctly.
    // -----------------------------------------------------------------

    /// `1.0` parses as a `TypedLiteral::Float(f64)`. Pinning the
    /// new `float` rule against the existing `integer` rule
    /// (which would otherwise consume `1` as the longer prefix
    /// without the dot, breaking ambiguity).
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

    /// Negative float `-0.25` and scientific notation `1e3` both
    /// parse via the same `float` rule. Pinning both shapes so a
    /// future rule simplification doesn't regress.
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

    /// `signal progress: f64` + `progress.get()` should rewrite
    /// to `__signal_get_f64("progress")`, mirroring the i32 path.
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

        // Locate the __fsm_meta__ body on the SignalProbeF64 impl.
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

        // Find the __fsm_tick__ marker; arg[1] is the rewritten
        // guard expression `Binary(Call(__signal_get_f64, "progress"), Ge, FloatLit(1.0))`.
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

    /// End-to-end: compile a fsm with a float-signal guard,
    /// move the signal across the threshold, watch step_tick
    /// fire. Verifies the i32-handling pattern extends cleanly
    /// to f64 — same dispatch, different table.
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

        // Below threshold → no transition.
        let next = dsl.step_tick(&id, "Loading").expect("step_tick");
        assert!(next.is_none(), "0.0 < 1.0, should not fire");

        // Cross the threshold.
        dsl.set_signal_f64("e2e_progress", 1.0);
        let next = dsl.step_tick(&id, "Loading").expect("step_tick");
        assert_eq!(
            next.and_then(|n| n.resolve_global()).as_deref(),
            Some("Done"),
            "1.0 >= 1.0, guard fires"
        );
    }

    /// Lifecycle: construct from a registered fsm, dispatch a
    /// sequence of events, watch `current()` follow each
    /// transition. End-to-end round-trip through the whole stack
    /// (parse → registry → instance → dispatch).
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
    /// current from-state should leave `current()` unchanged
    /// and return false. Pinning that the instance's state
    /// can't drift on a no-op dispatch.
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

        // Wrong event name from Off state → no transition.
        let fired = instance.dispatch_event(&dsl, "DoesNotExist");
        assert!(!fired);
        assert_eq!(instance.current(), "Off", "miss should leave state alone");
    }

    /// Tick + signal end-to-end through FsmInstance: live
    /// signal value drives the guard, instance updates when
    /// guard fires. Same plumbing as `signal_guard_*` tests but
    /// going through `FsmInstance::tick` instead of
    /// `BlincDsl::step_tick` directly — verifies the bridge
    /// composes with the JIT-evaluated guard path.
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

        // Signal below threshold → tick is a no-op.
        let fired = instance.tick(&dsl).expect("tick");
        assert!(!fired);
        assert_eq!(instance.current(), "Cold");

        // Raise the signal above threshold.
        dsl.set_signal_i32("instance_tick_count", 200);

        // Now tick fires, instance moves to Hot.
        let fired = instance.tick(&dsl).expect("tick");
        assert!(fired);
        assert_eq!(instance.current(), "Hot");
    }

    /// Reset returns the instance to its declared initial state.
    /// Pinning that reset() works from any current state, not
    /// only from the initial state.
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

        // Move off the initial state.
        instance.dispatch_event(&dsl, "Go");
        assert_eq!(instance.current(), "Working");

        // Reset.
        instance.reset();
        assert_eq!(
            instance.current(),
            "Idle",
            "reset should return to declared initial state"
        );
    }

    /// Construction fails cleanly when the fsm name doesn't
    /// match any registered fsm in the module. Pinning the
    /// "missing fsm → None" contract instead of e.g. panicking.
    #[test]
    fn fsm_instance_unknown_name_returns_none() {
        let dsl = BlincDsl::new().expect("runtime init");
        let attempt = FsmInstance::new(&dsl, "main", "DoesNotExistFsm");
        assert!(
            attempt.is_none(),
            "missing fsm should return None, not panic"
        );
    }

    /// Direct `FsmDefinition::step_event` works without going
    /// through the registry — useful for callers holding a
    /// borrowed FsmDefinition (e.g. iterating registry contents
    /// for diagnostics) and for unit-testing transition tables in
    /// isolation.
    #[test]
    fn fsm_definition_step_event_direct() {
        let def = FsmDefinition {
            initial: Some(intern("Idle")),
            transitions: vec![
                EventTransition {
                    from: intern("Idle"),
                    event: intern("Go"),
                    to: intern("Running"),
                },
                EventTransition {
                    from: intern("Running"),
                    event: intern("Stop"),
                    to: intern("Idle"),
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
    /// Verifies the global accessors give shared access in both
    /// directions; if a future refactor switches the lock to
    /// something non-mutex-shaped these tests fail loudly.
    #[test]
    fn fsm_registry_global_accessors() {
        // Use a high TypeId so this test is unlikely to collide with
        // any other test that pokes the global registry.
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

        // Cleanup so other tests see a clean global state.
        with_fsm_registry_mut(|r| {
            r.remove(&id);
        });
    }

    /// Mixed-statement view exercises both `text(...)` arg shapes
    /// (string + integer) coexisting in the same compiled function
    /// and routing to distinct host builtins via the grammar's PEG
    /// alternate dispatch.
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

    // =================================================================
    // Diagnostic-channel probes (ROADMAP §3.9 prototype goals)
    //
    // These exercise the failure modes we want to make sure surface
    // as actionable errors with file:line:col spans rather than panics
    // or generic strings. The assertions are deliberately lenient on
    // exact wording — Zyntax's diagnostic phrasing isn't stable yet —
    // but strict on (a) we got a `BlincDslError` (not a panic) and (b)
    // the string mentions the failing token / construct.
    //
    // If any of these regresses to a panic we'll catch it here before
    // it manifests as a poor user experience in `blinc dev`.
    // =================================================================

    /// **Parse error.** Source has a stray brace; the grammar can't
    /// match it. We expect a `BlincDslError::Compile` with the failing
    /// position called out.
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

    /// **Arity error.** `text()` with no argument violates the grammar
    /// rule `text_stmt = { "text" ~ "(" ~ s:string_literal ~ ")" }`.
    /// This is currently caught at parse time (not type-check time) —
    /// either is fine for the prototype, we just need a useful error.
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

    /// **Type error.** The grammar lets us emit a `text(...)` call with
    /// any expression as the arg, so we forge a typed-AST that calls
    /// `$Blinc$text` with an int literal. The injected extern decl
    /// expects `string`; Zyntax's type checker should catch the
    /// mismatch.
    ///
    /// We use `string_literal` in the grammar today, which won't
    /// produce ints — so this test exercises the type checker by
    /// wrapping the arg in something the grammar accepts but the
    /// types reject. Once the grammar grows expression nodes (phase 2)
    /// the test gets simpler. For now, keep it as a known-skip if the
    /// grammar can't produce the failing shape.
    ///
    /// **Rationale for keeping it:** when phase-2 expressions land
    /// this test will start exercising real type-mismatch paths
    /// without modification. It's pinned to the ZRTL signature
    /// boundary so as long as `$Blinc$text` declares
    /// `Type::Primitive(String)` the assertion shape stays correct.
    #[test]
    #[ignore = "phase 2 grammar will introduce expressions \
                that can be passed to text() — until then the grammar \
                only accepts string_literal so we can't construct a \
                type mismatch from source. Re-enable when grammar \
                supports e.g. integer literals as call args."]
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
