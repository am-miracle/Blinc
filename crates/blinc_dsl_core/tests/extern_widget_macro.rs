//! Integration test for `#[extern_widget]`.
//!
//! Lives outside the lib because the macro expands to absolute
//! `::blinc_dsl_core::...` paths — that resolution only works
//! from a *dependent* crate where `blinc_dsl_core` shows up by
//! name in the crate graph. The lib's own `#[cfg(test)]` module
//! resolves itself as `crate::...` and so can't host this test.

use blinc_dsl_core::{
    extern_widget, materialize_widget, BlincDsl, BlincStructValue, ExternWidget, WidgetBox,
    ZyntaxValue,
};
use blinc_layout::div::ElementBuilder;
use std::sync::{Mutex, OnceLock};

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacroBadgeConfig {
    pub title: String,
    pub count: i32,
    pub featured: bool,
}

impl TryFrom<BlincStructValue> for MacroBadgeConfig {
    type Error = &'static str;

    fn try_from(value: BlincStructValue) -> Result<Self, Self::Error> {
        Ok(Self {
            title: value
                .get_string("title")
                .ok_or("missing title")?
                .to_string(),
            count: value.get_i32("count").ok_or("missing count")?,
            featured: value.get_bool("featured").ok_or("missing featured")?,
        })
    }
}

fn last_badge_config() -> &'static Mutex<Option<MacroBadgeConfig>> {
    static LAST: OnceLock<Mutex<Option<MacroBadgeConfig>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

#[extern_widget(name = "MacroBadge")]
pub struct MacroBadge {
    pub config: MacroBadgeConfig,
}

impl ElementBuilder for MacroBadge {
    fn build(&self, tree: &mut blinc_layout::LayoutTree) -> blinc_layout::LayoutNodeId {
        *last_badge_config().lock().expect("last badge mutex") = Some(self.config.clone());
        blinc_layout::text::Text::new(&self.config.title).build(tree)
    }

    fn render_props(&self) -> blinc_layout::RenderProps {
        blinc_layout::text::Text::new(&self.config.title).render_props()
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

#[test]
fn extern_widget_macro_decodes_struct_prop() {
    let _ = tracing_subscriber::fmt::try_init();
    *last_badge_config().lock().expect("last badge mutex") = None;

    let dsl = BlincDsl::new().expect("runtime init");
    dsl.register_extern_widget::<MacroBadge>()
        .expect("register MacroBadge");

    dsl.compile_source(
        r#"
        struct MacroBadgeConfig {
            title: string
            count: i32
            featured: bool
        }

        view {
            MacroBadge(config = MacroBadgeConfig(count = 7, featured = true, title = "seven"))
        }
        "#,
        "macro_badge_struct.blinc",
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
        panic!("expected Custom(MacroBadge)");
    };
    let mut tree = blinc_layout::LayoutTree::new();
    builder.build(&mut tree);

    assert_eq!(
        *last_badge_config().lock().expect("last badge mutex"),
        Some(MacroBadgeConfig {
            title: "seven".to_string(),
            count: 7,
            featured: true,
        })
    );
}

fn last_switch_enabled() -> &'static Mutex<Option<bool>> {
    static LAST: OnceLock<Mutex<Option<bool>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

#[extern_widget(name = "MacroSwitch")]
pub struct MacroSwitch {
    pub enabled: bool,
}

impl ElementBuilder for MacroSwitch {
    fn build(&self, tree: &mut blinc_layout::LayoutTree) -> blinc_layout::LayoutNodeId {
        *last_switch_enabled().lock().expect("last switch mutex") = Some(self.enabled);
        blinc_layout::div::Div::new().build(tree)
    }

    fn render_props(&self) -> blinc_layout::RenderProps {
        blinc_layout::div::Div::new().render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        &[]
    }
}

#[test]
fn extern_widget_macro_decodes_bool_prop() {
    let _ = tracing_subscriber::fmt::try_init();
    *last_switch_enabled().lock().expect("last switch mutex") = None;

    let dsl = BlincDsl::new().expect("runtime init");
    dsl.register_extern_widget::<MacroSwitch>()
        .expect("register MacroSwitch");

    dsl.compile_source(
        r#"
        view {
            let enabled = true
            MacroSwitch(enabled = enabled)
        }
        "#,
        "macro_switch_bool.blinc",
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
        panic!("expected Custom(MacroSwitch)");
    };
    let mut tree = blinc_layout::LayoutTree::new();
    builder.build(&mut tree);

    assert_eq!(
        *last_switch_enabled().lock().expect("last switch mutex"),
        Some(true)
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
// Named slots
// =====================================================================

#[extern_widget(name = "MacroPanel")]
pub struct MacroPanel {
    #[children]
    pub children: Vec<Box<dyn ElementBuilder>>,
    #[slot(name = "Header")]
    pub header: Vec<Box<dyn ElementBuilder>>,
    #[slot(name = "Footer")]
    pub footer: Vec<Box<dyn ElementBuilder>>,
}

impl ElementBuilder for MacroPanel {
    fn build(&self, tree: &mut blinc_layout::LayoutTree) -> blinc_layout::LayoutNodeId {
        blinc_layout::div::Div::new().build(tree)
    }
    fn render_props(&self) -> blinc_layout::RenderProps {
        blinc_layout::div::Div::new().render_props()
    }
    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        &self.children
    }
}

/// Body block with both default children and two named slots:
/// each slot's contents land in the matching `Vec` field on the
/// reconstructed widget.
#[test]
fn extern_widget_named_slots_route_correctly() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("runtime init");
    dsl.register_extern_widget::<MacroPanel>()
        .expect("register MacroPanel");

    dsl.compile_source(
        r#"
        view {
            MacroPanel() {
                slot Header { Text("h1") Text("h2") }
                Text("body")
                slot Footer { Text("f1") }
            }
        }
        "#,
        "macro_panel.blinc",
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
        panic!("expected Custom(MacroPanel)");
    };
    // The macro re-emits the user struct unchanged, so the
    // builder IS a MacroPanel by construction. We can't downcast
    // through `dyn ElementBuilder`, but `children_builders()`
    // returns `&self.children`, which the impl forwards.
    assert_eq!(
        builder.children_builders().len(),
        1,
        "default children: just `Text(\"body\")`"
    );
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
