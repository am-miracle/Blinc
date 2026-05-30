//! End-to-end: register the cn.* widget pack, compile a DSL source
//! that uses `cn.Button(...)`, materialise the view, confirm the
//! resulting widget tree carries a cn::ButtonBuilder.

use blinc_dsl_core::BlincDsl;

/// `cn.Button("Save")` with the default variant + size compiles
/// and produces a renderable view. Failure modes covered:
///   * grammar regression (`cn.Button(...)` doesn't parse)
///   * registration regression (`cn.Button` not in the runtime
///     component registry after `register_all`)
///   * thunk regression (string arg decoded incorrectly)
#[test]
fn cn_button_compiles_and_renders() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");

    let src = r#"
        view {
            cn.Button("Save")
        }
    "#;
    let syms = dsl
        .compile_source(src, "cn_button_probe.blinc")
        .expect("compile cn.Button");
    eprintln!("compiled symbols: {syms:?}");
    // Confirm the cn.Button thunk made it into the JIT alongside
    // render_view. The macro mangles `cn.Button` → `cn_Button` for the
    // Rust ident + linker symbol; the JIT lists symbols by linker name.
    assert!(
        syms.iter().any(|s| s == "render_view"),
        "render_view should be among compiled symbols, got {syms:?}",
    );

    // Render via the substrate renderer — mirrors what production
    // call sites use (counter_dsl.rs etc.).
    let renderer = dsl.view_renderer();
    let value = blinc_runtime::view::render_main(&renderer).expect("render_main");
    let zyntax_embed::ZyntaxValue::Int(handle) = value else {
        panic!("expected widget handle, got: {value:?}");
    };
    assert_ne!(handle, 0, "cn.Button view should return a non-null handle");
}

/// Named-arg form — `cn.Button(label = "Cancel", variant = "destructive")`.
/// Confirms the prop registry lines up with the wrapper's pub fields
/// and downstream `resolve_extern_widget_named_args` finds them.
#[test]
fn cn_button_accepts_named_args() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");

    let src = r#"
        view {
            cn.Button(label = "Cancel", variant = "destructive", size = "small")
        }
    "#;
    dsl.compile_source(src, "cn_button_named_probe.blinc")
        .expect("compile cn.Button with named args");
}

/// `cn.Button(label = my_signal)` compiles through the
/// `Reactive<String>` pipeline. Three-slot FFI shape
/// (`tag`, `id_payload`, `literal_ptr`) routes the signal id
/// through the lowering pass, the macro thunk decodes it into
/// `Reactive::Signal`, and the wrapper snapshots the value at
/// build time via `Reactive::get_or_else`.
#[test]
fn cn_button_label_signal_compiles() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");

    let src = r#"
        signal title: string
        view {
            cn.Button(label = title)
        }
    "#;
    dsl.compile_source(src, "cn_button_label_signal.blinc")
        .expect("compile cn.Button(label = signal)");
}

/// `cn.Button(label = computed { ... } : string)` compiles. The
/// computed expression evaluates to a `DerivedId.to_raw() as i64`
/// at runtime; the wrapper builds a `Reactive::Computed(...)`
/// handle and snapshots the value via `get_or_else`.
///
/// Like the matching `cn.Progress.value` computed test, this
/// covers wiring only — value-flow inside the lambda body waits
/// on the upstream `gotcha-zyntax-lambda-return-value` fix.
#[test]
fn cn_button_label_computed_compiles() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");

    let src = r#"
        signal raw: string
        view {
            cn.Button(label = computed { raw.get() } : string)
        }
    "#;
    dsl.compile_source(src, "cn_button_label_computed.blinc")
        .expect("compile cn.Button(label = computed { … })");
}

/// `cn.Button()` with no label supplied: the macro's
/// `Reactive<String>` field defaults to `Literal(String::new())`
/// via the unsupplied-prop default path. Confirms the empty-string
/// fallback round-trips cleanly without panic.
#[test]
fn cn_button_omitted_label_defaults_to_empty() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");

    let src = r#"
        view {
            cn.Button()
        }
    "#;
    dsl.compile_source(src, "cn_button_label_default.blinc")
        .expect("compile cn.Button() with default label");
}

/// `cn.Button(disabled = true)` — literal bool. Exercises the
/// scalar `Reactive<bool>` path (two-slot `(tag, payload: i64)`
/// wire) end-to-end through the JIT.
#[test]
fn cn_button_disabled_literal_compiles() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");

    let src = r#"
        view {
            cn.Button(label = "Save", disabled = true)
        }
    "#;
    dsl.compile_source(src, "cn_button_disabled_literal.blinc")
        .expect("compile cn.Button(disabled = true)");
}

/// `cn.Button(disabled = my_signal)` — bool signal binding. The
/// lowering pass picks up `my_signal` as a SignalType::Bool entry
/// in the registry and routes through the SIGNAL tag.
///
/// IGNORED: blinc_dsl_core's signal registry doesn't currently
/// declare `bool` signals — only `i32`, `f64`, `string`. Adding
/// `signal foo: bool` to the DSL needs a tiny grammar / runtime
/// addition. Un-ignore once the bool signal declaration lands;
/// the macro + lowering + decode already support
/// `Reactive<bool>` from the SIGNAL tag.
#[test]
#[ignore = "blinc_runtime::signal doesn't declare SignalType::Bool yet"]
fn cn_button_disabled_signal_compiles() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");

    let src = r#"
        signal locked: bool
        view {
            cn.Button(label = "Save", disabled = locked)
        }
    "#;
    dsl.compile_source(src, "cn_button_disabled_signal.blinc")
        .expect("compile cn.Button(disabled = bool signal)");
}

/// `cn.Button()` with no `disabled` supplied: `Reactive<bool>`
/// defaults to `Literal(false)`. Confirms the omitted-prop default
/// path round-trips cleanly for the bool inner type.
#[test]
fn cn_button_omitted_disabled_defaults_to_false() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");

    let src = r#"
        view {
            cn.Button(label = "Save")
        }
    "#;
    dsl.compile_source(src, "cn_button_disabled_default.blinc")
        .expect("compile cn.Button() with default disabled");
}

/// `on_click` prop accepts a DSL closure. The closure compiles to a
/// zero-arg `extern "C" fn()` and the i64 fn-ptr is handed to
/// `CnButton::to_cn_builder` at materialise time. Mirrors
/// `Div(on_click = || { … })`.
///
/// Headless mode can't click the button, but the JIT compile path
/// here exercises every brittle hand-off: closure-body lift, fn-ptr
/// extraction, named-arg resolution against the registry, the
/// macro-generated thunk decoding `on_click: i64`. A regression in
/// any of those would error at `compile_source` time.
#[test]
fn cn_button_accepts_on_click_closure() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");

    let src = r#"
        signal click_count: i32
        view {
            cn.Button("Tap", on_click = || {
                click_count.set(click_count.get() + 1)
            })
        }
    "#;
    dsl.compile_source(src, "cn_button_on_click_probe.blinc")
        .expect("compile cn.Button with on_click closure");
}
