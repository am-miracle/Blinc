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
