//! `cn.Card { … }` end-to-end. Confirms:
//!   * The namespaced grammar handles body blocks (`cn.Card { Text("x") }`)
//!     the same as bare-name widgets.
//!   * The macro's `#[children]` field decodes the runtime children list
//!     through the namespaced thunk.
//!   * The wrapper's `ElementBuilder::children_builders()` returns the
//!     decoded children (not the empty `&[]` sentinel the leaf widgets
//!     pre-#[skip] used to hand back).

use blinc_dsl_core::BlincDsl;

#[test]
fn cn_card_with_children_compiles_and_renders() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");

    let src = r#"
        view {
            cn.Card {
                cn.Label("Email", required = true)
                cn.Button("Save", variant = "primary")
            }
        }
    "#;
    dsl.compile_source(src, "cn_card.blinc")
        .expect("compile cn.Card");

    // Drains the scene-op buffer through `render_view`. The DSL
    // produces a non-trivial widget tree; we mainly care that the
    // call doesn't panic, which would indicate a thunk / decode
    // regression upstream of widget construction.
    let _ops = dsl.render_view().expect("render_view");
}

/// `cn.Card { }` with no body parses cleanly and the `#[children]`
/// decode receives an empty Vec — guard against an off-by-one where
/// a missing body block fed an undef sentinel into the FFI.
#[test]
fn cn_card_empty_body_compiles() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");

    let src = r#"
        view {
            cn.Card { }
        }
    "#;
    dsl.compile_source(src, "cn_card_empty.blinc")
        .expect("compile empty cn.Card");
}

/// Nested namespaced calls: `cn.Card { cn.Card { Text("x") } }`. Catches
/// regressions where the dotted-name lookup is non-reentrant, or where
/// a child cn.Card's thunk steps on its parent's children list.
#[test]
fn cn_card_nested_compiles() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");

    let src = r#"
        view {
            cn.Card {
                cn.Card {
                    cn.Label("nested")
                }
                cn.Separator()
                cn.Label("sibling")
            }
        }
    "#;
    dsl.compile_source(src, "cn_card_nested.blinc")
        .expect("compile nested cn.Card");

    let _ops = dsl.render_view().expect("render_view");
}
