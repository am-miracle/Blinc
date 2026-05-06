//! CSS Debug Example
//!
//! Tests three known CSS issues:
//! 1. var() not picking up values from :root
//! 2. width/height percentage not working
//! 3. Text not inheriting color from parent div
//!
//! Run with: cargo run -p blinc_app_examples --example css_debug

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;
use blinc_app::windowed::WindowedContext;
use blinc_core::State;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let config = WindowConfig {
        title: "CSS Debug — var(), %, color inheritance".to_string(),
        width: 900,
        height: 700,
        resizable: true,
        ..Default::default()
    };

    WindowedApp::run(config, build_ui)
}

const STYLESHEET: &str = r#"
        :root {
            --brand-color: #3b82f6;
            --accent-color: #f59e0b;
            --text-color: #ffffff;
            --card-radius: 16px;
            --card-padding: 24px;
        }

        /* === TEST 1: var() from :root === */
        #var-test {
            background: var(--brand-color);
            border-radius: var(--card-radius);
            padding: var(--card-padding);
        }

        #var-test-accent {
            background: var(--accent-color);
            border-radius: 12px;
            padding: 16px;
        }

        #var-test-fallback {
            background: var(--nonexistent, #ef4444);
            border-radius: 12px;
            padding: 16px;
        }

        /* === TEST 2: Percentage width/height === */
        #percent-container {
            background: #1e293b;
            border-radius: 12px;
            padding: 16px;
        }

        #percent-half {
            width: 50%;
            background: #3b82f6;
            border-radius: 8px;
            padding: 12px;
        }

        #percent-full {
            width: 100%;
            background: #8b5cf6;
            border-radius: 8px;
            padding: 12px;
        }

        #percent-third {
            width: 33.3%;
            background: #10b981;
            border-radius: 8px;
            padding: 12px;
        }

        /* === TEST 3: Text color inheritance === */
        #inherit-parent {
            color: #3b82f6;
            background: #1e293b;
            border-radius: 12px;
            padding: 16px;
        }

        #inherit-nested {
            color: #f59e0b;
        }

        .white-text {
            color: #ffffff;
        }

        /* Section styles */
        #root {
            background: #0f172a;
        }

        .section {
            background: #1e293b;
            border-radius: 16px;
            padding: 24px;
        }

        /* === TEST 4: CSS inside stateful containers === */
        #sf-var-test {
            background: var(--brand-color);
            border-radius: 12px;
            padding: 16px;
        }

        #sf-percent-container {
            background: #1e293b;
            border-radius: 8px;
            padding: 12px;
        }

        #sf-percent-half {
            width: 50%;
            background: #3b82f6;
            border-radius: 8px;
            padding: 12px;
        }

        #sf-percent-third {
            width: 33.3%;
            background: #10b981;
            border-radius: 8px;
            padding: 12px;
        }

        #sf-color-parent {
            color: #10b981;
            background: #1e293b;
            border-radius: 8px;
            padding: 12px;
        }

        .sf-card {
            background: #7c3aed;
            border-radius: 12px;
            padding: 16px;
        }
"#;

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    if ctx.rebuild_count == 0 {
        ctx.add_css(STYLESHEET);
    }

    let count = ctx.use_state_keyed("rebuild-trigger", || 0i32);

    div()
        .w(ctx.width)
        .h(ctx.height)
        .id("root")
        .flex_col()
        .gap(5.0)
        .p(5.0)
        .overflow_y_scroll()
        .child(
            div()
                .flex_row()
                .items_center()
                .gap(4.0)
                .child(
                    text("CSS Debug Tests (inside stateful containers)")
                        .size(28.0)
                        .weight(FontWeight::Bold)
                        .color(Color::WHITE),
                )
                // Button to trigger rebuild
                .child(rebuild_button(count.clone())),
        )
        // All tests wrapped in stateful containers
        .child(test_var_from_root_stateful(count.clone()))
        .child(test_percentage_stateful(count.clone()))
        .child(test_color_inheritance_stateful(count.clone()))
        .child(test_layout_stateful())
        .child(test_inner_click_persistence(ctx, count.clone()))
}

/// Rebuild trigger button — click to force stateful containers to re-run on_state
fn rebuild_button(count: State<i32>) -> impl ElementBuilder {
    let count_click = count.clone();
    stateful::<NoState>()
        .deps([count.signal_id()])
        .on_state(move |_ctx| {
            let n = count.get();
            div()
                .bg(Color::rgba(0.23, 0.51, 0.96, 1.0))
                .rounded(8.0)
                .p(2.0)
                .px(4.0)
                .child(
                    text(format!("Rebuild ({})", n))
                        .size(16.0)
                        .weight(FontWeight::Bold)
                        .color(Color::WHITE),
                )
        })
        .on_click(move |_| {
            count_click.set(count_click.get() + 1);
        })
}

/// TEST 1: var() from :root inside stateful
fn test_var_from_root_stateful(count: State<i32>) -> impl ElementBuilder {
    stateful::<NoState>()
        .deps([count.signal_id()])
        .on_state(move |_ctx| {
            div()
                .class("section")
                .flex_col()
                .gap(3.0)
                .child(
                    div().child(
                        text("Test 1: var() from :root (stateful)")
                            .size(20.0)
                            .weight(FontWeight::Bold)
                            .color(Color::rgba(0.58, 0.64, 0.72, 1.0)),
                    ),
                )
                .child(
                    div()
                        .flex_row()
                        .gap(3.0)
                        .child(
                            div().id("var-test").child(
                                text("var(--brand-color)\nExpect: Blue bg")
                                    .size(14.0)
                                    .color(Color::WHITE),
                            ),
                        )
                        .child(
                            div().id("var-test-accent").child(
                                text("var(--accent-color)\nExpect: Amber bg")
                                    .size(14.0)
                                    .color(Color::WHITE),
                            ),
                        )
                        .child(
                            div().id("var-test-fallback").child(
                                text("var(--nonexistent, #ef4444)\nExpect: Red bg (fallback)")
                                    .size(14.0)
                                    .color(Color::WHITE),
                            ),
                        ),
                )
        })
}

/// TEST 2: Percentage width inside stateful
fn test_percentage_stateful(count: State<i32>) -> impl ElementBuilder {
    stateful::<NoState>()
        .deps([count.signal_id()])
        .on_state(move |_ctx| {
            div()
                .class("section")
                .flex_col()
                .gap(3.0)
                .child(
                    div().child(
                        text("Test 2: Percentage Width (stateful)")
                            .size(20.0)
                            .weight(FontWeight::Bold)
                            .color(Color::rgba(0.58, 0.64, 0.72, 1.0)),
                    ),
                )
                .child(
                    div()
                        .id("percent-container")
                        .w_full()
                        .flex_col()
                        .gap(2.0)
                        .child(
                            div().id("percent-half").child(
                                text("width: 50% — should be half")
                                    .size(14.0)
                                    .color(Color::WHITE),
                            ),
                        )
                        .child(
                            div().id("percent-full").child(
                                text("width: 100% — should be full")
                                    .size(14.0)
                                    .color(Color::WHITE),
                            ),
                        )
                        .child(
                            div().id("percent-third").child(
                                text("width: 33.3% — should be one-third")
                                    .size(14.0)
                                    .color(Color::WHITE),
                            ),
                        ),
                )
        })
}

/// TEST 3: Text color inheritance inside stateful
fn test_color_inheritance_stateful(count: State<i32>) -> impl ElementBuilder {
    stateful::<NoState>()
        .deps([count.signal_id()])
        .on_state(move |_ctx| {
            div()
                .class("section")
                .flex_col()
                .gap(3.0)
                .child(
                    div().child(
                        text("Test 3: Color Inheritance (stateful)")
                            .size(20.0)
                            .weight(FontWeight::Bold)
                            .color(Color::rgba(0.58, 0.64, 0.72, 1.0)),
                    ),
                )
                .child(
                    div()
                        .flex_col()
                        .gap(2.0)
                        .child(
                            div()
                                .id("inherit-parent")
                                .flex_col()
                                .gap(2.0)
                                .child(
                                    text("Should be BLUE (inherited from #inherit-parent { color: #3b82f6 })")
                                        .size(14.0),
                                )
                                .child(
                                    div()
                                        .id("inherit-nested")
                                        .child(
                                            text("Should be AMBER (inherited from #inherit-nested { color: #f59e0b })")
                                                .size(14.0),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .class("white-text")
                                .child(
                                    text("Should be WHITE (inherited from .white-text { color: #fff })")
                                        .size(14.0),
                                ),
                        )
                        .child(
                            text("Control: direct .color(Color::GREEN) — should be green")
                                .size(14.0)
                                .color(Color::GREEN),
                        ),
                )
        })
}

/// TEST 4: CSS layout properties inside stateful (no hover — pure layout test)
fn test_layout_stateful() -> impl ElementBuilder {
    stateful::<NoState>().on_state(|_ctx| {
        div()
            .class("section")
            .flex_col()
            .gap(3.0)
            .child(
                div().child(
                    text("Test 4: CSS Layout in stateful (class, var, %, color)")
                        .size(20.0)
                        .weight(FontWeight::Bold)
                        .color(Color::rgba(0.58, 0.64, 0.72, 1.0)),
                ),
            )
            // var() test
            .child(
                div().id("sf-var-test").child(
                    text("var(--brand-color) bg inside stateful")
                        .size(14.0)
                        .color(Color::WHITE),
                ),
            )
            // Percentage width test
            .child(
                div()
                    .id("sf-percent-container")
                    .flex_col()
                    .gap(2.0)
                    .child(
                        div().id("sf-percent-half").child(
                            text("width: 50% inside stateful")
                                .size(14.0)
                                .color(Color::WHITE),
                        ),
                    )
                    .child(
                        div().id("sf-percent-third").child(
                            text("width: 33.3% inside stateful")
                                .size(14.0)
                                .color(Color::WHITE),
                        ),
                    ),
            )
            // Color inheritance test
            .child(
                div().id("sf-color-parent").child(
                    text("Should be GREEN (inherited from #sf-color-parent { color: #10b981 })")
                        .size(14.0),
                ),
            )
            // Class-based styling test
            .child(
                div().class("sf-card").child(
                    text(".sf-card class styling inside stateful")
                        .size(14.0)
                        .color(Color::WHITE),
                ),
            )
    })
}

/// TEST 5: Inner child click handlers persist across stateful rebuilds
///
/// This tests that on_click handlers attached to children INSIDE on_state
/// callbacks survive when the parent stateful container rebuilds due to
/// signal changes. The inner buttons should remain clickable after pressing
/// the main "Rebuild" button.
fn test_inner_click_persistence(ctx: &WindowedContext, count: State<i32>) -> impl ElementBuilder {
    let inner_count = ctx.use_state_keyed("inner-click-count", || 0i32);
    let inner_count_for_state = inner_count.clone();
    let inner_count_click = inner_count.clone();

    stateful::<NoState>()
        .deps([count.signal_id(), inner_count.signal_id()])
        .on_state(move |_ctx| {
            let n = count.get();
            let clicks = inner_count_for_state.get();
            div()
                .class("section")
                .flex_col()
                .gap(3.0)
                .child(
                    div().child(
                        text("Test 5: Inner Child Click Persistence")
                            .size(20.0)
                            .weight(FontWeight::Bold)
                            .color(Color::rgba(0.58, 0.64, 0.72, 1.0)),
                    ),
                )
                .child(
                    div().child(
                        text(format!(
                            "Parent rebuild count: {} | Inner click count: {}",
                            n, clicks
                        ))
                        .size(14.0)
                        .color(Color::WHITE),
                    ),
                )
                // Inner clickable button — this handler should persist across rebuilds
                .child(
                    div()
                        .bg(Color::rgba(0.08, 0.65, 0.51, 1.0))
                        .rounded(8.0)
                        .p(3.0)
                        .px(4.0)
                        .child(
                            text("Click me (inner child handler)")
                                .size(16.0)
                                .weight(FontWeight::Bold)
                                .color(Color::WHITE),
                        )
                        .on_click({
                            let ic = inner_count_click.clone();
                            move |_| {
                                ic.set(ic.get() + 1);
                            }
                        }),
                )
                .child(
                    div().child(
                        text("After pressing 'Rebuild', this inner button should still work")
                            .size(12.0)
                            .color(Color::rgba(0.58, 0.64, 0.72, 1.0)),
                    ),
                )
        })
}
