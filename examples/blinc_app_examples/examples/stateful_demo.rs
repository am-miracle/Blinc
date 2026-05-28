//! Stateful API + Signal-bound modifiers demo.
//!
//! Two complementary examples:
//!
//! 1. **Stateful counter / event display** — the original demo of
//!    `stateful::<S>()`, `ctx.use_signal()`, `ctx.use_animated_value()`
//!    (declarative spring animations + scoped signals).
//!
//! 2. **Signal-bound modifiers** (reactive-architecture-v2 Phase 2) —
//!    `.bg(&state)` / `.opacity(&state)` / `.rounded(&state)` /
//!    `.border_color(&state)` / `.w(&state)` patch a single
//!    `RenderProps` (or taffy `Style`) cell on `state.set(...)` without
//!    a `Stateful` wrap or closure re-run.
//!
//! Run with: cargo run -p blinc_app_examples --example stateful_demo

use blinc_animation::SpringConfig;
use blinc_app::prelude::*;
use blinc_app::windowed::{WindowedApp, WindowedContext};
use blinc_cn::prelude::*;
use blinc_core::Color;
use blinc_core::Transform;
use blinc_core::events::event_types;
use blinc_core::reactive::{computed, effect, signal};
use blinc_layout::stateful::ButtonState;
use blinc_theme::{ColorToken, HybridTheme, RadiusToken, ThemeBundle, ThemeState};

/// Theme bundle shared by the desktop entry point and the wasm
/// wrapper. Pulls in `blinc_cn::cn_styles::CN_STYLES` so the
/// signal-bound section's `cn::button` widgets pick up the same
/// `.cn-*` hover / active rules as the cn demo.
pub fn theme_bundle() -> ThemeBundle {
    HybridTheme::bundle().with_css(blinc_cn::cn_styles::CN_STYLES)
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let config = WindowConfig {
        title: "Stateful + Signal-bound Demo".to_string(),
        width: 900,
        height: 800,
        resizable: true,
        ..Default::default()
    };

    WindowedApp::run_with_theme(
        config,
        theme_bundle(),
        blinc_theme::detect_system_color_scheme(),
        build_ui,
    )
}

/// See [`scroll::build_ui`](../scroll/fn.build_ui.html) for the
/// cross-target example convention this signature follows.
pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder + use<> {
    let theme = ThemeState::get();
    let bg = theme.color(ColorToken::Background);
    let text_primary = theme.color(ColorToken::TextPrimary);
    let text_secondary = theme.color(ColorToken::TextSecondary);

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(bg)
        .flex_col()
        .child(
            // Sticky title bar so the heading stays visible while the
            // body scrolls past it.
            div()
                .w_full()
                .p(theme.spacing().space_3)
                .flex_col()
                .gap(theme.spacing().space_1)
                .child(
                    text("Stateful + Signal-bound Demo")
                        .size(theme.typography().text_2xl)
                        .weight(FontWeight::Bold)
                        .color(text_primary),
                )
                .child(
                    text(
                        "Two demos: legacy stateful::<S>() patterns on top, \
                         reactive-architecture-v2 signal-bound modifiers below.",
                    )
                    .size(theme.typography().text_sm)
                    .color(text_secondary),
                ),
        )
        .child(
            scroll().w_full().h(ctx.height - 90.0).child(
                div()
                    .w_full()
                    .p(theme.spacing().space_3)
                    .flex_col()
                    .gap(theme.spacing().space_6)
                    .items_center()
                    .child(counter_button())
                    .child(event_info_display())
                    .child(signal_bound_modifier_section(ctx))
                    .child(free_function_signal_section()),
            ),
        )
}

// ============================================================================
// SECTION HELPERS
// ============================================================================

fn section_title(title: &str) -> impl ElementBuilder + use<> {
    let theme = ThemeState::get();
    text(title)
        .size(theme.typography().text_xl)
        .weight(FontWeight::Bold)
        .color(theme.color(ColorToken::TextPrimary))
}

fn section_container() -> Div {
    let theme = ThemeState::get();
    div()
        .w_full()
        .h_fit()
        .bg(theme.color(ColorToken::Surface))
        .rounded(theme.radius(RadiusToken::Default))
        .border(1.0, theme.color(ColorToken::Border))
        .p(theme.spacing().space_3)
        .flex_col()
        .gap(theme.spacing().space_3)
}

// ============================================================================
// LEGACY STATEFUL DEMO
// ============================================================================

/// A counter button demonstrating `ctx.use_signal()` and `ctx.use_spring()`.
fn counter_button() -> impl ElementBuilder + use<> {
    stateful::<ButtonState>()
        .initial(ButtonState::Idle)
        .on_state(|ctx| {
            // Scoped signal - persists across rebuilds, keyed to this stateful
            // Automatically registered as dependency - on_state re-runs when count changes
            let count = ctx.use_signal("count", || 0i32);

            // Declarative spring animation - specify target and bg, get current value
            let (target_scale, bg) = match ctx.state() {
                ButtonState::Idle => (1.0, Color::rgba(0.3, 0.5, 0.9, 1.0)),
                ButtonState::Hovered => (1.08, Color::rgba(0.4, 0.6, 1.0, 1.0)),
                ButtonState::Pressed => (0.95, Color::rgba(0.25, 0.4, 0.8, 1.0)),
                ButtonState::Disabled => (1.0, Color::GRAY),
            };

            let current_scale = ctx.use_spring("scale", target_scale, SpringConfig::snappy());

            // Handle click via ctx.event()
            if let Some(event) = ctx.event()
                && event.event_type == event_types::POINTER_UP
            {
                count.update(|n| n + 1);
                tracing::info!("Counter incremented to {}", count.get());
            }

            div()
                .w(200.0)
                .h(80.0)
                .bg(bg)
                .rounded(16.0)
                .flex_col()
                .items_center()
                .justify_center()
                .gap_px(4.0)
                .cursor_pointer()
                .transform(Transform::scale(current_scale, current_scale))
                .child(
                    text(format!("{}", count.get()))
                        .size(36.0)
                        .weight(FontWeight::Bold)
                        .color(Color::WHITE)
                        .pointer_events_none(),
                )
                .child(
                    text("Click me!")
                        .size(14.0)
                        .color(Color::rgba(1.0, 1.0, 1.0, 0.8))
                        .pointer_events_none(),
                )
        })
}

/// Display showing event information via `ctx.event()`.
fn event_info_display() -> impl ElementBuilder + use<> {
    stateful::<ButtonState>().on_state(|ctx| {
        // Track last event info using scoped signal
        // Automatically registered as dependency - on_state re-runs when it changes
        let last_event = ctx.use_signal("last_event", || "None".to_string());

        // Update event info when we receive an event
        if let Some(event) = ctx.event() {
            let event_name = match event.event_type {
                event_types::POINTER_ENTER => "POINTER_ENTER",
                event_types::POINTER_LEAVE => "POINTER_LEAVE",
                event_types::POINTER_DOWN => "POINTER_DOWN",
                event_types::POINTER_UP => "POINTER_UP",
                event_types::POINTER_MOVE => "POINTER_MOVE",
                _ => "Unknown",
            };
            last_event.set(format!(
                "{} at ({:.0}, {:.0})",
                event_name, event.local_x, event.local_y
            ));
        }

        let state_name = match ctx.state() {
            ButtonState::Idle => "Idle",
            ButtonState::Hovered => "Hovered",
            ButtonState::Pressed => "Pressed",
            ButtonState::Disabled => "Disabled",
        };

        let bg = match ctx.state() {
            ButtonState::Idle => Color::rgba(0.15, 0.15, 0.2, 1.0),
            ButtonState::Hovered => Color::rgba(0.2, 0.2, 0.28, 1.0),
            ButtonState::Pressed => Color::rgba(0.12, 0.12, 0.16, 1.0),
            ButtonState::Disabled => Color::rgba(0.1, 0.1, 0.12, 0.5),
        };

        div()
            .w(400.0)
            .p(20.0)
            .bg(bg)
            .rounded(12.0)
            .flex_col()
            .gap(12.0)
            .cursor_pointer()
            .child(
                div()
                    .flex_row()
                    .justify_between()
                    .child(
                        text("State:")
                            .size(16.0)
                            .color(Color::rgba(1.0, 1.0, 1.0, 0.6)),
                    )
                    .child(
                        text(state_name)
                            .size(16.0)
                            .weight(FontWeight::SemiBold)
                            .color(Color::WHITE),
                    ),
            )
            .child(
                div()
                    .flex_row()
                    .justify_between()
                    .child(
                        text("Last Event:")
                            .size(16.0)
                            .color(Color::rgba(1.0, 1.0, 1.0, 0.6)),
                    )
                    .child(
                        text(last_event.get())
                            .size(16.0)
                            .weight(FontWeight::SemiBold)
                            .color(Color::rgba(0.4, 0.8, 1.0, 1.0)),
                    ),
            )
            .child(
                text("Hover over this panel to see events")
                    .size(14.0)
                    .color(Color::rgba(1.0, 1.0, 1.0, 0.4))
                    .text_center(),
            )
    })
}

// ============================================================================
// SIGNAL-BOUND MODIFIER SECTION (reactive-architecture-v2 P2)
// ============================================================================

/// Demonstrates `.bg(&state)` / `.opacity(&state)` / `.rounded(&state)` /
/// `.border_color(&state)` / `.w(&state)` — signal-bound modifiers that
/// patch a single `RenderProps` (or taffy `Style`) cell on
/// `state.set(...)` without a `Stateful` wrap or closure re-run.
///
/// Moved out of `cn_demo` to keep its showcase focused on cn widgets;
/// the signal-bound substrate is a `blinc_layout` primitive that
/// stands on its own.
fn signal_bound_modifier_section(ctx: &WindowedContext) -> impl ElementBuilder + use<> {
    let theme = ThemeState::get();
    let text_secondary = theme.color(ColorToken::TextSecondary);

    // The swatch's color + opacity + corner radius + border live as plain
    // State<T>. No Stateful wraps the swatch. The button on_click handlers
    // call .set(...) and the framework patches the swatch directly via
    // the property channel.
    let bg = ctx.use_state_keyed("p2_demo_bg", || Color::from_hex(0x7a2bff));
    let op = ctx.use_state_keyed("p2_demo_op", || 1.0_f32);
    let radius = ctx.use_state_keyed("p2_demo_radius", || 16.0_f32);
    let bc = ctx.use_state_keyed("p2_demo_border", || Color::from_hex(0x1a1320));
    // P2.4 layout-bound state: signal-bound .w() goes through the
    // taffy-write path → relayout next frame, no Stateful wrap.
    let width = ctx.use_state_keyed("p2_demo_width", || 120.0_f32);

    section_container()
        .child(section_title("Signal-bound .bg() / .opacity() (Phase 2)"))
        .child(
            text(
                "The swatch below binds `.bg`, `.opacity`, `.rounded`, and \
                 `.border_color` directly to signals. Each button calls \
                 `state.set(...)` — the swatch updates without a Stateful \
                 subtree rebuild; only the affected RenderProps cell is \
                 patched and the next frame paints.",
            )
            .size(theme.typography().text_sm)
            .color(text_secondary),
        )
        .child(
            div()
                .flex_row()
                .gap(16.0)
                .items_center()
                .child(
                    // The reactive swatch — no Stateful wrapper.
                    // .w(&width) goes through the layout-bound path: signal
                    // updates patch the taffy Style + trigger relayout.
                    div()
                        .w(&width)
                        .h(120.0)
                        .bg(&bg)
                        .opacity(&op)
                        .rounded(&radius)
                        .border(2.0, Color::TRANSPARENT) // border-width is Tier-2
                        .border_color(&bc),
                )
                .child(
                    div()
                        .flex_col()
                        .gap(8.0)
                        .child(
                            div()
                                .flex_row()
                                .gap(8.0)
                                .child(cn::button("Purple").on_click({
                                    let bg = bg.clone();
                                    move |_| bg.set(Color::from_hex(0x7a2bff))
                                }))
                                .child(cn::button("Cyan").on_click({
                                    let bg = bg.clone();
                                    move |_| bg.set(Color::from_hex(0x00e5ff))
                                }))
                                .child(cn::button("Magenta").on_click({
                                    let bg = bg.clone();
                                    move |_| bg.set(Color::from_hex(0xff2d9b))
                                })),
                        )
                        .child(
                            div()
                                .flex_row()
                                .gap(8.0)
                                .child(cn::button("Opacity 1.0").on_click({
                                    let op = op.clone();
                                    move |_| op.set(1.0)
                                }))
                                .child(cn::button("Opacity 0.5").on_click({
                                    let op = op.clone();
                                    move |_| op.set(0.5)
                                }))
                                .child(cn::button("Opacity 0.25").on_click({
                                    let op = op.clone();
                                    move |_| op.set(0.25)
                                })),
                        )
                        .child(
                            div()
                                .flex_row()
                                .gap(8.0)
                                .child(cn::button("Radius 4").on_click({
                                    let radius = radius.clone();
                                    move |_| radius.set(4.0)
                                }))
                                .child(cn::button("Radius 16").on_click({
                                    let radius = radius.clone();
                                    move |_| radius.set(16.0)
                                }))
                                .child(cn::button("Radius 60 (pill)").on_click({
                                    let radius = radius.clone();
                                    move |_| radius.set(60.0)
                                })),
                        )
                        .child(
                            div()
                                .flex_row()
                                .gap(8.0)
                                .child(cn::button("Border dark").on_click({
                                    let bc = bc.clone();
                                    move |_| bc.set(Color::from_hex(0x1a1320))
                                }))
                                .child(cn::button("Border cyan").on_click({
                                    let bc = bc.clone();
                                    move |_| bc.set(Color::from_hex(0x00e5ff))
                                }))
                                .child(cn::button("Border magenta").on_click({
                                    let bc = bc.clone();
                                    move |_| bc.set(Color::from_hex(0xff2d9b))
                                })),
                        )
                        // Layout-bound row: signal updates trigger relayout
                        // via the taffy-write path (no Stateful, no rebuild).
                        .child(
                            div()
                                .flex_row()
                                .gap(8.0)
                                .child(cn::button("Width 80").on_click({
                                    let width = width.clone();
                                    move |_| width.set(80.0)
                                }))
                                .child(cn::button("Width 120").on_click({
                                    let width = width.clone();
                                    move |_| width.set(120.0)
                                }))
                                .child(cn::button("Width 240").on_click({
                                    let width = width.clone();
                                    move |_| width.set(240.0)
                                })),
                        ),
                ),
        )
}

/// Demonstrates the bare reactive primitive surface:
///
/// - `signal(initial)` — bare reactive primitive (Copy, no `.clone()`
///   ceremony in closures).
/// - `Signal<T>::set` / `.update` / `.get` directly on the handle.
/// - `computed(|g| ...)` — derived value that auto-tracks every signal
///   read inside the closure, plugged into the same `IntoReactive`
///   channel as `.bg(&computed)`.
/// - `effect(|g| ...)` — side-effect that re-runs when any tracked
///   dependency changes (here: logs to the terminal).
///
/// All three operate on the process-global reactive graph, so they
/// interop with the `State<T>` / `use_state*` surface above without
/// any plumbing.
fn free_function_signal_section() -> impl ElementBuilder + use<> {
    let theme = ThemeState::get();
    let text_secondary = theme.color(ColorToken::TextSecondary);

    // Bare signals — Copy, no Arc clone needed in closures.
    let r = signal(0.5_f32);
    let g = signal(0.5_f32);
    let b = signal(0.5_f32);

    // Computed: derived from r/g/b. The closure auto-tracks the three
    // signal reads via the graph; any `.set` on r/g/b refires this.
    let mixed = computed(move |graph| {
        Color::rgba(
            graph.get(r).unwrap_or(0.0),
            graph.get(g).unwrap_or(0.0),
            graph.get(b).unwrap_or(0.0),
            1.0,
        )
    });

    // Effect: side-effect demo. Logs each change to the terminal —
    // useful for debugging, telemetry, or any "do something when X
    // changes" pattern. The handle is intentionally leaked: an effect
    // dropped while the graph is still alive would unsubscribe.
    let _e = effect(move |graph| {
        let rv = graph.get(r).unwrap_or(0.0);
        let gv = graph.get(g).unwrap_or(0.0);
        let bv = graph.get(b).unwrap_or(0.0);
        tracing::info!("free-fn effect: rgb = ({rv:.2}, {gv:.2}, {bv:.2})");
    });

    let slider_row = |channel: &'static str, s: blinc_core::Signal<f32>| {
        div()
            .flex_row()
            .gap(8.0)
            .items_center()
            .child(div().w(20.0).child(text(channel).color(text_secondary)))
            .child(cn::button("0").on_click(move |_| s.set(0.0)))
            .child(cn::button("0.25").on_click(move |_| s.set(0.25)))
            .child(cn::button("0.5").on_click(move |_| s.set(0.5)))
            .child(cn::button("0.75").on_click(move |_| s.set(0.75)))
            .child(cn::button("1").on_click(move |_| s.set(1.0)))
            .child(cn::button("+0.1").on_click(move |_| s.update(|v| (v + 0.1).min(1.0))))
            .child(cn::button("-0.1").on_click(move |_| s.update(|v| (v - 0.1).max(0.0))))
    };

    section_container()
        .child(section_title(
            "Free functions: signal() / computed() / effect()",
        ))
        .child(
            text(
                "The swatch's colour is a `computed(|g| Color::rgba(...))` \
                 derived from three bare `signal(0.5)` primitives. Each \
                 slider button calls `.set(...)` directly on a Copy \
                 `Signal<f32>` — no `.clone()` ceremony, no `Stateful` \
                 wrap, no `ctx`. An `effect(...)` also subscribes and \
                 logs each change to the terminal.",
            )
            .size(theme.typography().text_sm)
            .color(text_secondary),
        )
        .child(
            div()
                .flex_row()
                .gap(16.0)
                .items_center()
                .child(div().w(120.0).h(120.0).rounded(16.0).bg(&mixed))
                .child(
                    div()
                        .flex_col()
                        .gap(8.0)
                        .child(slider_row("R", r))
                        .child(slider_row("G", g))
                        .child(slider_row("B", b)),
                ),
        )
}
