//! Integration test for `#[extern_widget]`.
//!
//! Lives outside the lib because the macro expands to absolute
//! `::blinc_dsl_core::...` paths — that resolution only works
//! from a *dependent* crate where `blinc_dsl_core` shows up by
//! name in the crate graph. The lib's own `#[cfg(test)]` module
//! resolves itself as `crate::...` and so can't host this test.

use blinc_dsl_core::{
    extern_widget, materialize_widget, BlincDsl, ExternWidget, WidgetBox, ZyntaxValue,
};
use blinc_layout::div::ElementBuilder;

/// Declare a Rust widget the same way an app would: a plain
/// struct decorated with `#[extern_widget]`. The struct's fields
/// become the DSL-visible props; the user provides the
/// `ElementBuilder` impl that drives rendering.
#[extern_widget(name = "MacroText")]
pub struct MacroText {
    pub content: String,
}

impl ElementBuilder for MacroText {
    fn build(&self, tree: &mut blinc_layout::LayoutTree) -> blinc_layout::LayoutNodeId {
        // The wrapped `Text` already knows how to build itself;
        // delegate so this integration test doesn't have to
        // reimplement layout-tree construction.
        blinc_layout::text::Text::new(&self.content).build(tree)
    }

    fn render_props(&self) -> blinc_layout::RenderProps {
        blinc_layout::text::Text::new(&self.content).render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        &[]
    }
}

/// End-to-end: a Rust widget declared via `#[extern_widget]` is
/// callable from DSL source like any built-in primitive, returns
/// a non-zero handle, and decodes back through the `Custom`
/// variant. Registration flows through the trait-based generic
/// `dsl.register_extern_widget::<W>()`.
#[test]
fn extern_widget_macro_round_trip() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("runtime init");
    dsl.register_extern_widget::<MacroText>()
        .expect("register MacroText via trait-based generic");

    // Verify the macro-generated trait impl carries the
    // attribute-supplied DSL name.
    assert_eq!(MacroText::DSL_NAME, "MacroText");

    dsl.compile_source(r#"view { MacroText("via macro") }"#, "macro_widget.blinc")
        .expect("compile");

    let renderer: std::sync::Arc<dyn blinc_runtime::view::ViewRenderer> = dsl.view_renderer();
    let value = blinc_runtime::view::render_main(&renderer).expect("render_main");

    let ZyntaxValue::Int(handle) = value else {
        panic!("expected widget handle, got: {value:?}");
    };
    assert_ne!(handle, 0, "macro-emitted thunk should return a real handle");

    // SAFETY: handle came out of the macro-generated thunk,
    // which uses `into_handle` to wrap the builder result in
    // `WidgetBox::Custom`.
    let widget = unsafe { materialize_widget(handle) }.expect("non-null handle");
    assert!(
        matches!(*widget, WidgetBox::Custom(_)),
        "macro-emitted thunk should land in WidgetBox::Custom"
    );
}

// =====================================================================
// `#[children]` field — body-block plumbing through the macro
// =====================================================================

/// A user widget that accepts children. `#[children]` marks the
/// receiver for the body block in DSL source; the field type is
/// `Vec<Box<dyn ElementBuilder>>`, which the macro-generated
/// thunk fills from the runtime children-list.
#[extern_widget(name = "MacroCard")]
pub struct MacroCard {
    pub title: String,
    #[children]
    pub children: Vec<Box<dyn ElementBuilder>>,
}

impl ElementBuilder for MacroCard {
    fn build(&self, tree: &mut blinc_layout::LayoutTree) -> blinc_layout::LayoutNodeId {
        // For the test we don't actually render — just satisfy
        // the trait. Real widgets would compose `self.title` and
        // `self.children` into a real layout subtree.
        blinc_layout::div::Div::new().build(tree)
    }

    fn render_props(&self) -> blinc_layout::RenderProps {
        blinc_layout::div::Div::new().render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        &self.children
    }
}

// =====================================================================
// `styled` flag — inline visual styling props through the macro
// =====================================================================

#[extern_widget(name = "StyledBox", styled)]
pub struct StyledBox {
    pub label: String,
}

impl ElementBuilder for StyledBox {
    fn build(&self, tree: &mut blinc_layout::LayoutTree) -> blinc_layout::LayoutNodeId {
        blinc_layout::text::Text::new(&self.label).build(tree)
    }
    fn render_props(&self) -> blinc_layout::RenderProps {
        blinc_layout::text::Text::new(&self.label).render_props()
    }
    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        &[]
    }
}

#[test]
fn extern_widget_styled_flag_applies_overlay() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("runtime init");
    dsl.register_extern_widget::<StyledBox>()
        .expect("register StyledBox");

    dsl.compile_source(
        r#"view { StyledBox("hello", opacity = 0.25, bg = 65280) }"#,
        "styled_box.blinc",
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
        panic!("expected Custom(Styled<StyledBox>)");
    };
    let props = builder.render_props();
    assert_eq!(props.opacity, 0.25);
    // 65280 = 0x00FF00 → green.
    if let Some(blinc_core::layer::Brush::Solid(c)) = props.background {
        assert!(c.r.abs() < 0.01);
        assert!((c.g - 1.0).abs() < 0.01);
        assert!(c.b.abs() < 0.01);
    } else {
        panic!("background should be a solid brush");
    }
}

/// End-to-end: `MacroCard(title = "hi") { Text("a") Text("b") }`
/// compiles, calls the macro-generated thunk with the right
/// args, and decodes back to a `Custom`-variant widget whose
/// `children_builders()` returns the two `Text`s.
#[test]
fn extern_widget_macro_with_children_round_trip() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("runtime init");
    dsl.register_extern_widget::<MacroCard>()
        .expect("register MacroCard");

    dsl.compile_source(
        r#"
        view {
            MacroCard(title = "hello") {
                Text("first")
                Text("second")
            }
        }
        "#,
        "macro_card.blinc",
    )
    .expect("compile");

    let renderer: std::sync::Arc<dyn blinc_runtime::view::ViewRenderer> = dsl.view_renderer();
    let value = blinc_runtime::view::render_main(&renderer).expect("render_main");

    let ZyntaxValue::Int(handle) = value else {
        panic!("expected widget handle, got: {value:?}");
    };
    assert_ne!(handle, 0);

    // SAFETY: handle came out of the macro-generated thunk for
    // MacroCard, which wraps the user struct in `WidgetBox::Custom`.
    let widget = unsafe { materialize_widget(handle) }.expect("non-null handle");
    let WidgetBox::Custom(boxed) = *widget else {
        panic!("expected WidgetBox::Custom");
    };
    assert_eq!(
        boxed.children_builders().len(),
        2,
        "user widget should report 2 child builders, got {}",
        boxed.children_builders().len()
    );
}
