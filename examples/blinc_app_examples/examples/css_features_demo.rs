//! CSS Visual Features Demo
//!
//! Showcases newly added CSS visual features:
//! - mix-blend-mode: Blend overlapping elements (multiply, screen, overlay, etc.)
//! - pointer-events: Control click-through behavior
//! - cursor: CSS cursor style on hover
//! - text-decoration: Underline, line-through with color and thickness
//! - text-overflow: Ellipsis truncation with white-space: nowrap
//!
//! Run with: cargo run -p blinc_app_examples --example css_features_demo

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;
use blinc_app::windowed::WindowedContext;
use blinc_core::Color;
use blinc_theme::{ColorToken, ThemeState};

const STYLESHEET: &str = r#"
    /* ─── mix-blend-mode (simple class selectors) ── */
    .blend-multiply    { mix-blend-mode: multiply; }
    .blend-screen      { mix-blend-mode: screen; }
    .blend-overlay     { mix-blend-mode: overlay; }
    .blend-darken      { mix-blend-mode: darken; }
    .blend-lighten     { mix-blend-mode: lighten; }
    .blend-difference  { mix-blend-mode: difference; }
    .blend-exclusion   { mix-blend-mode: exclusion; }
    .blend-color-dodge { mix-blend-mode: color-dodge; }
    .blend-hard-light  { mix-blend-mode: hard-light; }

    /* ─── pointer-events ─────────────────────────── */
    .pe-overlay {
        pointer-events: none;
        opacity: 0.5;
    }
    .pe-button {
        cursor: pointer;
    }
    .pe-button:hover {
        background: #3b82f6;
    }

    /* ─── cursor ─────────────────────────────────── */
    .cursor-item {
        width: 100px;
        height: 60px;
        border-radius: 8px;
        display: flex;
        align-items: center;
        justify-content: center;
    }
    .cursor-default      { cursor: default; background: #374151; }
    .cursor-pointer      { cursor: pointer; background: #1d4ed8; }
    .cursor-text         { cursor: text; background: #047857; }
    .cursor-move         { cursor: move; background: #7c3aed; }
    .cursor-grab         { cursor: grab; background: #b45309; }
    .cursor-not-allowed  { cursor: not-allowed; background: #dc2626; }
    .cursor-crosshair    { cursor: crosshair; background: #0891b2; }
    .cursor-wait         { cursor: wait; background: #4338ca; }

    /* ─── text-decoration ────────────────────────── */
    .td-underline    { text-decoration: underline; }
    .td-line-through { text-decoration: line-through; }
    .td-color        { text-decoration: underline; text-decoration-color: #ef4444; }
    .td-thick        { text-decoration: underline; text-decoration-thickness: 3px; }
    .td-both         { text-decoration: underline; text-decoration-color: #8b5cf6; text-decoration-thickness: 2px; }

    /* ─── text-overflow ──────────────────────────── */
    .truncated {
        width: 250px;
        overflow: hidden;
        white-space: nowrap;
        text-overflow: ellipsis;
        padding: 8px 12px;
        border-radius: 8px;
        background: #1f2937;
    }
    .normal-wrap {
        width: 250px;
        padding: 8px 12px;
        border-radius: 8px;
        background: #1f2937;
    }
"#;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    let config = WindowConfig {
        title: "CSS Visual Features Demo".to_string(),
        width: 1200,
        height: 900,
        resizable: true,
        fullscreen: false,
        ..Default::default()
    };

    WindowedApp::run(config, build_ui)
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder + use<> {
    if ctx.rebuild_count == 0 {
        ctx.add_css(STYLESHEET);
    }

    let theme = ThemeState::get();
    let bg = theme.color(ColorToken::Background);

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(bg)
        .flex_col()
        .child(header())
        .child(
            scroll().w_full().h(ctx.height - 60.0).child(
                div()
                    .w_full()
                    .p(24.0)
                    .flex_col()
                    .gap(32.0)
                    .child(blend_mode_section())
                    .child(pointer_events_section())
                    .child(cursor_section())
                    .child(text_decoration_section())
                    .child(text_overflow_section()),
            ),
        )
}

fn header() -> Div {
    let theme = ThemeState::get();

    div()
        .w_full()
        .h(60.0)
        .bg(theme.color(ColorToken::Surface))
        .flex_row()
        .items_center()
        .px(24.0)
        .child(
            text("CSS Visual Features Demo")
                .size(24.0)
                .weight(FontWeight::Bold)
                .color(theme.color(ColorToken::TextPrimary)),
        )
}

fn section_title(title: &str) -> Div {
    let theme = ThemeState::get();
    div().w_full().mb(8.0).child(
        text(title)
            .size(20.0)
            .weight(FontWeight::Bold)
            .color(theme.color(ColorToken::TextPrimary)),
    )
}

fn label(s: &str) -> Div {
    let theme = ThemeState::get();
    div().child(
        text(s)
            .size(13.0)
            .color(theme.color(ColorToken::TextSecondary)),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// mix-blend-mode
// ─────────────────────────────────────────────────────────────────────────────

fn blend_mode_section() -> Div {
    let modes = [
        "multiply",
        "screen",
        "overlay",
        "darken",
        "lighten",
        "difference",
        "exclusion",
        "color-dodge",
        "hard-light",
    ];

    let mut row = div().w_full().flex_row().flex_wrap().gap(16.0);

    for mode in modes {
        let blend_class = format!("blend-{}", mode);
        row = row.child(
            div()
                .flex_col()
                .items_center()
                .gap(4.0)
                .child(
                    div()
                        .w(120.0)
                        .h(90.0)
                        // Background rect (red)
                        .child(
                            div()
                                .absolute()
                                .w(80.0)
                                .h(80.0)
                                .rounded(12.0)
                                .bg(Color::rgb(0.9, 0.2, 0.3))
                                .top(5.0)
                                .left(0.0),
                        )
                        // Foreground rect (blue) with blend mode, overlapping
                        .child(
                            div()
                                .absolute()
                                .w(80.0)
                                .h(80.0)
                                .rounded(12.0)
                                .bg(Color::rgb(0.2, 0.5, 0.9))
                                .class(&blend_class)
                                .top(0.0)
                                .left(35.0),
                        ),
                )
                .child(label(mode)),
        );
    }

    div()
        .w_full()
        .flex_col()
        .gap(12.0)
        .child(section_title("mix-blend-mode"))
        .child(row)
}

// ─────────────────────────────────────────────────────────────────────────────
// pointer-events
// ─────────────────────────────────────────────────────────────────────────────

fn pointer_events_section() -> Div {
    div()
        .w_full()
        .flex_col()
        .gap(12.0)
        .child(section_title("pointer-events"))
        .child(
            stack()
                .w(400.0)
                .h(120.0)
                // Bottom layer: the interactive button
                .child(
                    div()
                        .w_full()
                        .h_full()
                        .bg(Color::rgb(0.15, 0.15, 0.2))
                        .rounded(12.0)
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .top(30.0)
                        .child(
                            div()
                                .class("pe-button")
                                .w(200.0)
                                .h(40.0)
                                .bg(Color::rgb(0.3, 0.3, 0.4))
                                .rounded(8.0)
                                .flex_row()
                                .items_center()
                                .justify_center()
                                .child(
                                    text("Hover me (through overlay)")
                                        .size(14.0)
                                        .color(Color::WHITE),
                                ),
                        ),
                )
                // Top layer: semi-transparent overlay with pointer-events: none
                .child(
                    div()
                        .class("pe-overlay")
                        .w_full()
                        .h_full()
                        .bg(Color::rgb(0.9, 0.2, 0.2))
                        .rounded(12.0)
                        .flex_row()
                        .items_center()
                        .justify_center()
                        .child(
                            text("This overlay has pointer-events: none")
                                .size(14.0)
                                .color(Color::WHITE),
                        ),
                ),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// cursor
// ─────────────────────────────────────────────────────────────────────────────

fn cursor_section() -> Div {
    let cursors = [
        ("default", "cursor-default"),
        ("pointer", "cursor-pointer"),
        ("text", "cursor-text"),
        ("move", "cursor-move"),
        ("grab", "cursor-grab"),
        ("no-drop", "cursor-not-allowed"),
        ("crosshair", "cursor-crosshair"),
        ("wait", "cursor-wait"),
    ];

    let mut row = div().w_full().flex_row().flex_wrap().gap(12.0);

    for (name, class) in cursors {
        row = row.child(
            div()
                .flex_col()
                .items_center()
                .gap(4.0)
                .child(
                    div()
                        .class("cursor-item")
                        .class(class)
                        .child(text(name).size(12.0).color(Color::WHITE)),
                )
                .child(label(name)),
        );
    }

    div()
        .w_full()
        .flex_col()
        .gap(12.0)
        .child(section_title("cursor"))
        .child(row)
}

// ─────────────────────────────────────────────────────────────────────────────
// text-decoration
// ─────────────────────────────────────────────────────────────────────────────

fn text_decoration_section() -> Div {
    let theme = ThemeState::get();
    let text_color = theme.color(ColorToken::TextPrimary);

    div()
        .w_full()
        .flex_col()
        .gap(12.0)
        .child(section_title("text-decoration"))
        .child(
            div()
                .w_full()
                .flex_col()
                .gap(16.0)
                .child(
                    div()
                        .class("td-underline")
                        .child(text("Underlined text").size(16.0).color(text_color)),
                )
                .child(
                    div()
                        .class("td-line-through")
                        .child(text("Strikethrough text").size(16.0).color(text_color)),
                )
                .child(
                    div()
                        .class("td-color")
                        .child(text("Red underline color").size(16.0).color(text_color)),
                )
                .child(
                    div()
                        .class("td-thick")
                        .child(text("Thick underline (3px)").size(16.0).color(text_color)),
                )
                .child(
                    div().class("td-both").child(
                        text("Purple underline, 2px thick")
                            .size(16.0)
                            .color(text_color),
                    ),
                ),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// text-overflow + white-space
// ─────────────────────────────────────────────────────────────────────────────

fn text_overflow_section() -> Div {
    div()
        .w_full()
        .flex_col()
        .gap(12.0)
        .child(section_title("text-overflow + white-space"))
        .child(
            div()
                .w_full()
                .flex_col()
                .gap(12.0)
                .child(label("text-overflow: ellipsis + white-space: nowrap"))
                .child(
                    div().class("truncated").child(
                        text("This is a very long text that should be truncated with an ellipsis at the end of the container")
                            .size(14.0)
                            .color(Color::WHITE),
                    ),
                )
                .child(label("Normal wrapping (default)"))
                .child(
                    div().class("normal-wrap").child(
                        text("This is the same long text but with normal wrapping behavior enabled by default")
                            .size(14.0)
                            .color(Color::WHITE),
                    ),
                ),
        )
}
