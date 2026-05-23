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
pub(crate) extern "C" fn blinc_text(s_ptr: *const i32) {
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
pub(crate) extern "C" fn blinc_signal_get_i32(name_ptr: *const i32) -> i32 {
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
pub(crate) extern "C" fn blinc_signal_get_f64(name_ptr: *const i32) -> f64 {
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
pub(crate) extern "C" fn blinc_signal_get_string(name_ptr: *const i32) -> *const i32 {
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
pub(crate) extern "C" fn blinc_signal_set_i32(name_ptr: *const i32, value: i32) {
    let Some(name) = decode_signal_name(name_ptr) else {
        tracing::warn!("__signal_set_i32 called with null name pointer");
        return;
    };
    blinc_runtime::signal::set_i32(name, value);
}

/// `__signal_set_f64("<name>", value)` — f64 signal write side.
pub(crate) extern "C" fn blinc_signal_set_f64(name_ptr: *const i32, value: f64) {
    let Some(name) = decode_signal_name(name_ptr) else {
        tracing::warn!("__signal_set_f64 called with null name pointer");
        return;
    };
    blinc_runtime::signal::set_f64(name, value);
}

/// `__signal_set_string("<name>", value)` — string signal write side.
pub(crate) extern "C" fn blinc_signal_set_string(name_ptr: *const i32, value_ptr: *const i32) {
    let Some(name) = decode_signal_name(name_ptr) else {
        tracing::warn!("__signal_set_string called with null name pointer");
        return;
    };
    let value = decode_signal_name(value_ptr).unwrap_or("");
    blinc_runtime::signal::set_str(name, value);
}

/// `__fsm_runtime_trigger__("<FsmName>", "<state.event>")` — dispatches `event`
/// on the default instance iff its current state matches `state`.
pub(crate) extern "C" fn blinc_fsm_runtime_trigger(fsm_ptr: *const i32, path_ptr: *const i32) {
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
pub(crate) extern "C" fn blinc_fsm_subscribe(
    fsm_ptr: *const i32,
    path_ptr: *const i32,
    closure_ptr: i64,
) {
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
pub(crate) extern "C" fn blinc_text_int(n: i32) {
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
pub(crate) fn blinc_string_alloc(s: &str) -> *const i32 {
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
pub(crate) unsafe fn blinc_string_decode<'a>(ptr: *const i32) -> &'a str {
    unsafe {
        if ptr.is_null() {
            return "";
        }
        let len = std::ptr::read_unaligned(ptr) as usize;
        let body = (ptr as *const u8).add(4);
        let bytes = std::slice::from_raw_parts(body, len);
        std::str::from_utf8(bytes).unwrap_or("<invalid utf-8>")
    }
}

/// `__fstring_format__` for i32 — decimal string of an integer.
pub(crate) extern "C" fn blinc_format_int(n: i32) -> *const i32 {
    let s = n.to_string();
    blinc_string_alloc(&s)
}

/// `string_concat` — joins two Zyntax-formatted strings into a fresh leaked one.
pub(crate) extern "C" fn blinc_string_concat(a: *const i32, b: *const i32) -> *const i32 {
    // SAFETY: length-prefixed string layout for String params.
    let a_str = unsafe { blinc_string_decode(a) };
    let b_str = unsafe { blinc_string_decode(b) };
    let mut out = String::with_capacity(a_str.len() + b_str.len());
    out.push_str(a_str);
    out.push_str(b_str);
    blinc_string_alloc(&out)
}
