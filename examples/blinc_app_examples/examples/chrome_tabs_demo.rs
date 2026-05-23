//! Chrome-Style Tabs Demo
//!
//! Demonstrates the notch element in "reverse" mode: instead of a dropdown
//! hanging BELOW a bar with concave top corners, Chrome-style tabs sit
//! ABOVE a toolbar with concave BOTTOM corners. The concave curves flare
//! outward past the tab's box and visually merge with the toolbar beneath,
//! giving the active tab its signature smooth connection to the toolbar
//! edge.
//!
//! Run with: cargo run -p blinc_app_examples --example chrome_tabs_demo

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_core::Color;
use blinc_layout::stateful::NoState;

const TOOLBAR_H: f32 = 44.0;

const BOOK_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 19.5v-15A2.5 2.5 0 0 1 6.5 2H20v20H6.5a2.5 2.5 0 0 1 0-5H20"/></svg>"#;

const GLOBE_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="2" y1="12" x2="22" y2="12"/><path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"/></svg>"#;

const CODE_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/></svg>"#;

const PLUS_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/></svg>"#;

const X_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>"#;

const LOCK_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="11" width="18" height="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>"#;

const BACK_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="19" y1="12" x2="5" y2="12"/><polyline points="12 19 5 12 12 5"/></svg>"#;

const FORWARD_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="5" y1="12" x2="19" y2="12"/><polyline points="12 5 19 12 12 19"/></svg>"#;

const REFRESH_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="1 4 1 10 7 10"/><polyline points="23 20 23 14 17 14"/><path d="M20.49 9A9 9 0 0 0 5.64 5.64L1 10m22 4-4.64 4.36A9 9 0 0 1 3.51 15"/></svg>"#;

const STAR_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2"/></svg>"#;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    let config = WindowConfig {
        title: "Chrome Tabs Demo".to_string(),
        width: 900,
        height: 600,
        resizable: true,
        fullscreen: false,
        ..Default::default()
    };

    blinc_app::windowed::WindowedApp::run(config, build_ui)
}

pub fn build_ui(_ctx: &mut WindowedContext) -> impl ElementBuilder + use<> {
    stateful::<NoState>().on_state(move |_ctx| {
        let page_bg = Color::from_hex(0xf3f4f6);
        let window_frame_bg = Color::from_hex(0x1f3333); // deep teal — Chrome dark-mode-ish
        let inactive_tab_fg = Color::WHITE.with_alpha(0.65);
        let active_tab_fg = Color::WHITE;
        let toolbar_bg = Color::from_hex(0x243c3c);
        let url_bar_bg = Color::rgba(0.0, 0.0, 0.0, 0.35);

        // Chrome-tab geometry. The tab's BOTTOM corners are concave, so
        // they flare OUTWARD past the tab's layout box by `tab_concave`
        // pixels. The toolbar immediately below the tab row picks up that
        // flare visually — when the active tab and the toolbar share a
        // fill colour, the concave curve reads as a seamless merge.
        let tab_width = 200.0;
        let tab_height = 36.0;
        let tab_top_radius = 10.0;
        let tab_concave = 12.0;

        div()
            .w_full()
            .h_full()
            .bg(page_bg)
            .flex_col()
            .items_center()
            .justify_center()
            .gap(4.0) // units ×4 px = 16 px
            .child(
                text("Chrome-Style Tabs — concave bottom corners merge each tab into the toolbar")
                    .size(13.0)
                    .color(Color::rgba(0.3, 0.3, 0.35, 1.0)),
            )
            .child(browser_window(
                window_frame_bg,
                inactive_tab_fg,
                active_tab_fg,
                toolbar_bg,
                url_bar_bg,
                tab_width,
                tab_height,
                tab_top_radius,
                tab_concave,
            ))
    })
}

#[allow(clippy::too_many_arguments)]
fn browser_window(
    frame_bg: Color,
    inactive_tab_fg: Color,
    active_tab_fg: Color,
    toolbar_bg: Color,
    url_bar_bg: Color,
    tab_w: f32,
    tab_h: f32,
    tab_top_r: f32,
    tab_concave: f32,
) -> Div {
    // The window body width. Tab row + toolbar + content all share it.
    let window_w = 760.0;

    div()
        .w(window_w)
        .bg(frame_bg)
        .rounded(12.0)
        .flex_col()
        .child(title_bar(frame_bg, inactive_tab_fg))
        .child(
            // The tab row and toolbar are stacked so the active tab's
            // concave flare — which extends `tab_concave` px BELOW the
            // tab's layout box — can visibly overlap the toolbar. In a
            // plain flex_col the toolbar would be drawn AFTER the tab
            // row and paint over the flare; using a stack with explicit
            // top positioning lets the tab row render on top of the
            // toolbar while still consuming the full layout area.
            stack()
                .w_full()
                .h(tab_h + TOOLBAR_H)
                .child(
                    // Toolbar — absolute positioned at the bottom of
                    // the stack, drawn first (bottom of z-order).
                    div()
                        .absolute()
                        .top(tab_h)
                        .left(0.0)
                        .right(0.0)
                        .h(TOOLBAR_H)
                        .bg(toolbar_bg)
                        .flex_row()
                        .items_center()
                        .px(3.0) // 12 px
                        .gap(2.0) // 8 px
                        .child(toolbar_icon(BACK_SVG, inactive_tab_fg))
                        .child(toolbar_icon(FORWARD_SVG, inactive_tab_fg))
                        .child(toolbar_icon(REFRESH_SVG, inactive_tab_fg))
                        .child(
                            // URL bar
                            div()
                                .flex_grow()
                                .h(28.0)
                                .rounded(14.0)
                                .bg(url_bar_bg)
                                .flex_row()
                                .items_center()
                                .px(3.0)
                                .gap(2.0)
                                .child(svg(LOCK_SVG).square(12.0).color(inactive_tab_fg))
                                .child(
                                    text("blinc.rs/docs/notch-element")
                                        .size(12.0)
                                        .no_wrap()
                                        .pointer_events_none()
                                        .color(Color::WHITE.with_alpha(0.85)),
                                ),
                        )
                        .child(toolbar_icon(STAR_SVG, inactive_tab_fg)),
                )
                .child(
                    // Tab row — absolute positioned at the top, drawn
                    // AFTER the toolbar so the active tab's concave
                    // flare paints over the toolbar's top edge. Since
                    // both share `toolbar_bg`, the flare reads as a
                    // seamless merge between tab and toolbar.
                    //
                    // The row's height is `tab_h + tab_concave` so the
                    // active tab's BL/BR flares (which extend
                    // `tab_concave` px below the tab's box) stay inside
                    // the row's layout bounds and don't get clipped.
                    // `items_start` aligns tops so the active tab's box
                    // bottom sits at y = tab_h (the toolbar's top edge),
                    // and the flare renders from y = tab_h to y = tab_h
                    // + tab_concave, overlapping the toolbar below.
                    div()
                        .absolute()
                        .top(0.0)
                        .left(0.0)
                        .right(0.0)
                        .h(tab_h + tab_concave)
                        .flex_row()
                        .items_start()
                        .pl(1.0) // 4 px left inset
                        .gap_px(8.0)
                        .child(active_tab(
                            "Notch Menu Bar - Blinc UI Framework",
                            BOOK_SVG,
                            toolbar_bg, // match toolbar so the flare merges seamlessly
                            active_tab_fg,
                            tab_w,
                            tab_h,
                            tab_top_r,
                            tab_concave,
                        ))
                        .child(
                            // First inactive tab sits to the right of
                            // the active tab. The active tab's layout
                            // box has `concave_r` px of empty space on
                            // its right (the inner body ends at
                            // `w − concave_r` at content-y, even though
                            // the layout box runs to `w`). Pulling this
                            // tab left by `concave_r` px overlaps the
                            // dead zone so the visible gap between the
                            // active tab's close button and this tab
                            // matches the inter-inactive gap.
                            inactive_tab(
                                "Chrome Tabs — Wikipedia",
                                GLOBE_SVG,
                                Color::rgba(0.0, 0.0, 0.0, 0.25),
                                inactive_tab_fg,
                                tab_w,
                                tab_h - 6.0,
                                tab_top_r,
                            )
                            .ml(-(tab_concave / 4.0)),
                        )
                        .child(inactive_tab(
                            "SDF Rendering Notes",
                            CODE_SVG,
                            Color::rgba(0.0, 0.0, 0.0, 0.25),
                            inactive_tab_fg,
                            tab_w,
                            tab_h - 6.0,
                            tab_top_r,
                        ))
                        .child(
                            // New-tab button. Height matches the
                            // inactive tabs (tab_h − 6) so with
                            // `items_start` on the row the plus icon's
                            // vertical center aligns with the inactive
                            // tab centers — otherwise a shorter box
                            // would leave the icon riding too high.
                            div()
                                .w(32.0)
                                .h(tab_h - 6.0)
                                .ml(1.0) // 4 px
                                .rounded(6.0)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(svg(PLUS_SVG).square(16.0).color(inactive_tab_fg)),
                        ),
                ),
        )
        .child(
            // Content area — plain white page below the chrome.
            div()
                .w_full()
                .h(340.0)
                .bg(Color::WHITE)
                .flex_col()
                .items_center()
                .justify_center()
                .gap(3.0) // 12 px
                .child(
                    text("Active tab merges into the toolbar")
                        .size(20.0)
                        .color(Color::from_hex(0x1f3333)),
                )
                .child(
                    text("via concave bottom corners in the notch element")
                        .size(14.0)
                        .color(Color::rgba(0.4, 0.4, 0.45, 1.0)),
                ),
        )
}

/// Traffic-light / title bar row above the tabs.
fn title_bar(_frame_bg: Color, fg: Color) -> Div {
    div()
        .w_full()
        .h(28.0)
        .flex_row()
        .items_center()
        .px(3.0) // 12 px
        .gap(1.5) // 6 px
        .child(
            div()
                .w(12.0)
                .h(12.0)
                .rounded_full()
                .bg(Color::from_hex(0xff5f57)),
        )
        .child(
            div()
                .w(12.0)
                .h(12.0)
                .rounded_full()
                .bg(Color::from_hex(0xfebc2e)),
        )
        .child(
            div()
                .w(12.0)
                .h(12.0)
                .rounded_full()
                .bg(Color::from_hex(0x28c840)),
        )
        .child(
            div()
                .flex_grow()
                .flex_row()
                .justify_center()
                .child(text("Blinc").size(11.0).color(fg)),
        )
        .child(div().w(42.0).h(12.0)) // spacer to balance traffic lights
}

/// Active tab — the only tab that uses the notch element.
///
/// The notch's `concave_bottom(r)` insets the inner body UPWARD from
/// the outer layout bottom by `r` px (see `sd_notch` in
/// `crates/blinc_gpu/src/shaders.rs`). That means the VISIBLE shape of
/// a notch with layout height `H` is only `H − r` tall. To get a tab
/// whose visible body matches `h`, we set the layout height to
/// `h + concave_r`. The visible bottom then lands at `y = h`, which
/// aligns exactly with the toolbar's top edge, and since both share
/// `toolbar_bg` the flares merge into the toolbar with no seam.
///
/// Children also need to respect the inner body on every axis: the
/// shader insets the inner body by `concave_r` px on left/right and
/// `concave_r` px on the bottom. `Notch::build` does NOT auto-add that
/// padding (existing callers like the notch_demo dropdown already
/// handle their own inset manually), so we set it explicitly here.
#[allow(clippy::too_many_arguments)]
fn active_tab(
    title: &str,
    icon_svg: &'static str,
    bg: Color,
    fg: Color,
    w: f32,
    h: f32,
    top_r: f32,
    concave_r: f32,
) -> impl ElementBuilder + use<> {
    notch()
        .rounded_top(top_r)
        .concave_bottom(concave_r)
        .bg(bg)
        .w(w)
        .h(h + concave_r)
        .flex_row()
        .items_center()
        // Explicit inset for the concave-bottom inner body. Notch
        // padding methods take raw px (unlike Div's 4-px units).
        .pl(concave_r + 6.0)
        .pr(concave_r + 6.0)
        .pb(concave_r)
        .child(
            // Content wrapper: the icon+label group `flex_grow`s so it
            // claims all space left after the fixed-width close button,
            // while the X stays right-anchored. `flex_shrink_0` on the
            // leaf icon boxes keeps them from being squeezed down when
            // the label is long.
            div()
                .w_full()
                .flex_row()
                .items_center()
                .gap_px(8.0)
                .child(
                    div()
                        .flex_grow()
                        .min_w(0.0)
                        .flex_row()
                        .items_center()
                        .gap(2.0)
                        .child(icon_box(icon_svg, 14.0, fg))
                        .child(tab_label(title, fg)),
                )
                .child(icon_box(X_SVG, 12.0, fg.with_alpha(0.7))),
        )
}

/// Wrap an SVG icon in a flex-shrink-0 box so it keeps its nominal
/// size inside a flex row even when a sibling label is shrinking.
fn icon_box(svg_markup: &'static str, size: f32, color: Color) -> Div {
    div()
        .flex_shrink_0()
        .square(size)
        .flex()
        .items_center()
        .justify_center()
        .child(svg(svg_markup).square(size).color(color))
}

/// Inactive tab — in Chrome, only the active tab has the notch shape
/// and the close icon. Inactive tabs are plain rounded rectangles (all
/// four corners rounded) with a fixed max width and a visible gap
/// between their bottom and the toolbar below.
fn inactive_tab(
    title: &str,
    icon_svg: &'static str,
    bg: Color,
    fg: Color,
    w: f32,
    h: f32,
    top_r: f32,
) -> Div {
    div()
        .bg(bg)
        .rounded(top_r)
        .max_w(w)
        .h(h)
        .flex_row()
        .items_center()
        .px(3.0)
        .gap(2.0)
        .child(icon_box(icon_svg, 14.0, fg))
        .child(tab_label(title, fg))
}

/// A shared tab-label wrapper. `flex_grow` lets it claim the remaining
/// row space, `min_w(0.0)` overrides flexbox's default min-content width
/// (which otherwise lets the text push the tab wider to fit), and
/// `overflow_clip` trims long titles cleanly. `no_wrap` +
/// `pointer_events_none` on the text itself keep the title on one line
/// and let mouse events fall through to the tab.
fn tab_label(title: &str, fg: Color) -> Div {
    div().min_w(0.0).overflow_clip().child(
        text(title)
            .size(12.0)
            .no_wrap()
            .pointer_events_none()
            .color(fg),
    )
}

fn toolbar_icon(icon_svg: &'static str, color: Color) -> Div {
    div()
        .w(28.0)
        .h(28.0)
        .flex()
        .items_center()
        .justify_center()
        .child(svg(icon_svg).square(16.0).color(color))
}

#[cfg(target_arch = "wasm32")]
fn main() {}
