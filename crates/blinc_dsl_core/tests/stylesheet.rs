//! Phase 1 of styling: top-level `stylesheet "..."` blocks.

use blinc_dsl_core::BlincDsl;

/// A single-line stylesheet block lands in `compiled_stylesheets()`
/// verbatim (no quotes, no escapes).
#[test]
fn stylesheet_block_collected_into_compiled_stylesheets() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("runtime init");
    dsl.compile_source(
        r##"
        stylesheet "#header { background: red; }"
        view { Text("hi") }
        "##,
        "stylesheet_basic.blinc",
    )
    .expect("compile");

    let sheets = dsl.compiled_stylesheets();
    assert_eq!(sheets.len(), 1, "expected one sheet, got: {sheets:?}");
    assert!(
        sheets[0].contains("#header { background: red; }"),
        "stylesheet content should pass through verbatim, got: {:?}",
        sheets[0]
    );
}

/// Multi-line CSS + `@flow` block survive end-to-end. The host
/// passes the result to `ctx.add_css(...)`, which knows how to
/// parse `@flow` (no Blinc-side work needed — `css_parser`
/// handles it).
#[test]
fn stylesheet_block_preserves_multiline_and_flow() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("runtime init");
    let css = r#"
        .card { background: #3b82f6; border-radius: 12px; }
        @flow ripple {
            target: fragment;
            input uv: builtin(uv);
            input time: builtin(time);
            node wave = sin(uv.x * 10.0 + time);
            output color = vec4(wave, wave, wave, 1.0);
        }
    "#;
    let source = format!(
        r##"
        stylesheet "{}"
        view {{ Text("hi") }}
        "##,
        css.replace('"', "\\\"")
    );

    dsl.compile_source(&source, "stylesheet_flow.blinc")
        .expect("compile");

    let sheets = dsl.compiled_stylesheets();
    assert_eq!(sheets.len(), 1);
    assert!(sheets[0].contains(".card"), "missing .card rule");
    assert!(sheets[0].contains("@flow ripple"), "missing @flow block");
    assert!(
        sheets[0].contains("output color"),
        "missing flow body — multi-line content didn't pass through"
    );
}

/// Inline `class = "..."` on Div lands on the constructed
/// widget's class list. Pairs with a stylesheet that targets
/// `.btn` — together they form the standard CSS+class flow
/// that mirrors the Rust-side `.class("btn")` + `ctx.add_css`
/// pattern in `styling_demo.rs`.
#[test]
fn div_inline_class_attribute_applies_to_widget() {
    use blinc_dsl_core::{materialize_widget, WidgetBox, ZyntaxValue};

    let _ = tracing_subscriber::fmt::try_init();

    let dsl = blinc_dsl_core::BlincDsl::new().expect("runtime init");
    dsl.compile_source(
        r##"
        stylesheet ".btn { background: red; }"
        view { Div(class = "btn primary") }
        "##,
        "class_attr.blinc",
    )
    .expect("compile");

    let renderer: std::sync::Arc<dyn blinc_runtime::view::ViewRenderer> = dsl.view_renderer();
    let value = blinc_runtime::view::render_main(&renderer).expect("render_main");
    let ZyntaxValue::Int(handle) = value else {
        panic!("expected widget handle");
    };
    assert_ne!(handle, 0);

    let widget = unsafe { materialize_widget(handle) }.expect("non-null handle");
    let WidgetBox::Custom(builder) = *widget else {
        panic!("expected Custom(Styled<Div>)");
    };
    // The Styled wrapper delegates element_classes() to the
    // inner Div, which stores the class list.
    let classes = builder.element_classes();
    assert!(
        classes.iter().any(|c| c.as_ref() == "btn"),
        "missing 'btn' class, got: {classes:?}"
    );
    assert!(
        classes.iter().any(|c| c.as_ref() == "primary"),
        "missing 'primary' class, got: {classes:?}"
    );
}

/// Multiple stylesheet blocks in one source land in order.
#[test]
fn multiple_stylesheet_blocks_collect_in_order() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("runtime init");
    dsl.compile_source(
        r#"
        stylesheet ".first { color: red; }"
        stylesheet ".second { color: blue; }"
        view { Text("hi") }
        "#,
        "stylesheet_multi.blinc",
    )
    .expect("compile");

    let sheets = dsl.compiled_stylesheets();
    assert_eq!(sheets.len(), 2);
    assert!(sheets[0].contains(".first"));
    assert!(sheets[1].contains(".second"));
}
