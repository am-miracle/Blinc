//! Notch Menu Bar Demo
//!
//! Demonstrates a macOS-style menu bar with a notched dropdown that slides
//! horizontally between icons. The dropdown maintains a seamless visual
//! connection to the menu bar via concave curves.
//!
//! Run with: cargo run -p blinc_app_examples --example notch_demo

use blinc_animation::SpringConfig;
use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_core::Color;
use blinc_layout::stateful::{ButtonState, NoState};
use blinc_theme::{ColorToken, ThemeState};

// Menu bar height
const MENU_BAR_HEIGHT: f32 = 44.0;
const ICON_SIZE: f32 = 24.0;
const ICON_GAP: f32 = 16.0;
const NOTCH_RADIUS: f32 = 32.0;
const DROPDOWN_HEIGHT: f32 = 20.0;
const DROPDOWN_WIDTH: f32 = 340.0;

/// State for tracking the active menu item and its position
#[derive(Clone, Copy, Debug, Default)]
struct DropdownState {
    item: Option<MenuItem>,
    /// The center X position of the hovered icon (absolute)
    center_x: f32,
}

// Icon SVGs (Lucide-style icons)
const CLOCK_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></svg>"#;

const BATTERY_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="7" width="18" height="10" rx="2" ry="2"/><line x1="22" y1="11" x2="22" y2="13"/></svg>"#;

const WIFI_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12.55a11 11 0 0 1 14.08 0"/><path d="M1.42 9a16 16 0 0 1 21.16 0"/><path d="M8.53 16.11a6 6 0 0 1 6.95 0"/><circle cx="12" cy="20" r="1"/></svg>"#;

const WEATHER_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9Z"/></svg>"#;

const MUSIC_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/></svg>"#;

const PLUS_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/></svg>"#;

const HOME_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m3 9 9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><polyline points="9 22 9 12 15 12 15 22"/></svg>"#;

const SEARCH_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/></svg>"#;

const USER_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2"/><circle cx="12" cy="7" r="4"/></svg>"#;

const SETTINGS_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>"#;

/// Menu item data
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MenuItem {
    Clock,
    Battery,
    Wifi,
    Weather,
    Music,
}

impl MenuItem {
    fn all() -> &'static [MenuItem] {
        &[
            MenuItem::Clock,
            MenuItem::Battery,
            MenuItem::Wifi,
            MenuItem::Weather,
            MenuItem::Music,
        ]
    }

    fn icon_svg(&self) -> &'static str {
        match self {
            MenuItem::Clock => CLOCK_SVG,
            MenuItem::Battery => BATTERY_SVG,
            MenuItem::Wifi => WIFI_SVG,
            MenuItem::Weather => WEATHER_SVG,
            MenuItem::Music => MUSIC_SVG,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    use blinc_platform::AnimationFps;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    let config = WindowConfig {
        title: "Notch Menu Bar Demo".to_string(),
        width: 800,
        height: 600,
        resizable: true,
        fullscreen: false,
        animation_fps: AnimationFps::Fixed(30),
        max_frame_latency: 2,
        ..Default::default()
    };

    blinc_app::windowed::WindowedApp::run(config, build_ui)
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    let width = ctx.width;
    let height = ctx.height;

    stateful::<NoState>().on_state(move |ctx| {
        let theme = ThemeState::get();
        let bg = Color::WHITE; //theme.color(ColorToken::Background);

        // Track dropdown state (which item + its position)
        let dropdown_state: blinc_core::State<DropdownState> =
            ctx.use_signal("dropdown_state", || DropdownState {
                item: None,
                center_x: 0.0, // Will be set on first hover
            });

        let state = dropdown_state.get();

        // Track open generation - increments each time we transition from closed to open
        // This creates a new spring on each fresh open, so position appears immediately
        let open_gen = ctx.use_signal("open_gen", || 0u32);
        let prev_item_id = ctx.use_signal("prev_item", || 0u8);

        let is_open = state.item.is_some();
        let was_open = prev_item_id.get() != 0;
        let just_opened = is_open && !was_open;

        // Only update when item actually changes (avoids infinite re-render)
        let current_item_id = state.item.map(|_| 1u8).unwrap_or(0);
        if current_item_id != prev_item_id.get() {
            prev_item_id.set(current_item_id);
            if just_opened {
                open_gen.set(open_gen.get() + 1);
            }
        }

        // Get animated values for dropdown position and size
        let target_center_x = state.center_x;
        let full_height = DROPDOWN_HEIGHT + NOTCH_RADIUS * 2.0;
        let target_height = if state.item.is_some() {
            full_height
        } else {
            0.0 // Collapse to nothing
        };

        // Use dynamic spring key for position - resets on each fresh open
        let pos_spring_key = format!("dropdown_x_{}", open_gen.get());
        let center_x = ctx.use_spring(&pos_spring_key, target_center_x, SpringConfig::gentle());

        // Width: animate between items while open, expand when closing
        let item_width = DROPDOWN_WIDTH;
        let expanded_width = DROPDOWN_WIDTH * 0.3;
        let target_width = if is_open { item_width } else { expanded_width };
        // Dynamic key resets spring on fresh open so it starts at item width, not expanded
        let width_spring_key = format!("dropdown_w_{}", open_gen.get());
        let dropdown_width =
            ctx.use_spring(&width_spring_key, target_width, SpringConfig::snappy());

        let dropdown_height = ctx.use_spring("dropdown_h", target_height, SpringConfig::snappy());

        // Fade out opacity when nearly collapsed (height < 0.5)
        let opacity = if dropdown_height < 0.5 {
            (dropdown_height / 0.5).clamp(0.0, 1.0)
        } else {
            1.0
        };

        // Keep top concave radius at full size so curves stay visible
        let top_radius = NOTCH_RADIUS;

        // Bottom radius: no animation, just use full radius when open
        let bottom_radius = NOTCH_RADIUS - 16.0;

        let menu_bar_bg = Color::BLACK;

        // Clone state for hover leave handler
        let dropdown_for_leave = dropdown_state.clone();

        // Root container
        div()
            .w(width)
            .h(height)
            .bg(bg)
            .flex_col()
            .child(
                // Menu bar + dropdown container with hover tracking.
                //
                // Shadow strategy: ONLY the menu bar carries a shadow.
                // Draw order is menu bar FIRST, dropdown SECOND (on
                // top). The menu bar's downward shadow renders beneath
                // the menu bar everywhere; where the dropdown sits
                // (centered below the active icon), the dropdown's
                // opaque fill — drawn on top — covers the shadow, so
                // the shadow flows naturally AROUND the dropdown body
                // without double-shadowing the merge region. The
                // dropdown gets its visual depth for free from the
                // menu bar's shadow wrapping around its sides.
                stack()
                    .w_full()
                    // Stack clips children to its bounds by default
                    // (see `Stack::new` in blinc_layout), so the
                    // height must include space for the dropdown's
                    // drop-shadow bleed — otherwise the shadow is
                    // cut off at the stack's bottom edge. 28 px is
                    // the shadow's `offset_y + blur` plus a small
                    // safety margin.
                    .h(MENU_BAR_HEIGHT + DROPDOWN_HEIGHT + NOTCH_RADIUS + 10.0)
                    .child(menu_bar(&dropdown_state, menu_bar_bg))
                    .when(dropdown_height > 0.6, |s| {
                        s.child(notched_dropdown(
                            state.item,
                            center_x,
                            dropdown_width,
                            opacity,
                            dropdown_height,
                            top_radius,
                            bottom_radius,
                            menu_bar_bg,
                        ))
                    })
                    // Close dropdown when mouse leaves the entire menu + dropdown area
                    .on_hover_leave(move |_| {
                        dropdown_for_leave.set(DropdownState {
                            item: None,
                            center_x: dropdown_for_leave.get().center_x,
                        });
                    }),
            )
            .child(
                // Content area below menu bar
                div()
                    .w_full()
                    .flex_grow()
                    .flex_col()
                    .items_center()
                    .justify_start()
                    .gap(6.0)
                    .child(
                        text("Hover over the icons in the menu bar above")
                            .size(16.0)
                            .color(theme.color(ColorToken::TextSecondary)),
                    )
                    // Navigation bar with active indicator bulge
                    .child(
                        div()
                            .flex_col()
                            .items_center()
                            .gap(4.0)
                            .child(
                                text("Navigation Bar with Bulge Active Indicator")
                                    .size(12.0)
                                    .color(theme.color(ColorToken::TextSecondary)),
                            )
                            .child(active_nav_bar()),
                    )
                    // Sharp angle cut/peak examples
                    .child(
                        div()
                            .flex_col()
                            .items_center()
                            .gap(4.0)
                            .child(
                                text("Sharp Angle Cuts & Peaks (V-shapes)")
                                    .size(12.0)
                                    .color(theme.color(ColorToken::TextSecondary)),
                            )
                            .child(sharp_angle_demo()),
                    ),
            )
            // Bottom dock bar with center scoop
            .child(bottom_dock_bar(width))
    })
}

/// Menu bar with icon buttons
fn menu_bar(dropdown_state: &blinc_core::State<DropdownState>, bg: Color) -> Div {
    let mut bar = div()
        .w_full()
        .h(MENU_BAR_HEIGHT)
        .bg(bg)
        .shadow(blinc_core::Shadow {
            offset_x: 0.0,
            offset_y: 4.0,
            blur: 12.0,
            spread: 0.0,
            color: Color::BLACK.with_alpha(0.35),
        })
        .flex_row()
        .items_center()
        .justify_center()
        .gap(ICON_GAP);

    for item in MenuItem::all() {
        let item = *item;
        let state = dropdown_state.clone();

        bar = bar.child(stateful_icon_button(item, state));
    }

    bar
}

/// Stateful icon button with hover state tracking
fn stateful_icon_button(
    item: MenuItem,
    dropdown_state: blinc_core::State<DropdownState>,
) -> impl ElementBuilder {
    let dropdown_for_hover = dropdown_state.clone();

    stateful::<ButtonState>()
        .initial(ButtonState::Idle)
        .on_state(move |ctx| {
            let current_state = dropdown_state.get();
            let is_active = current_state.item == Some(item);

            // Background based on state
            let icon_color = match (ctx.state(), is_active) {
                (ButtonState::Hovered, _) | (ButtonState::Pressed, _) | (_, true) => Color::WHITE,
                _ => Color::WHITE.with_alpha(0.8),
            };

            // Scale animation on press
            let scale = ctx.use_spring(
                "scale",
                if matches!(ctx.state(), ButtonState::Hovered) {
                    1.30
                } else {
                    1.0
                },
                SpringConfig::snappy(),
            );

            div()
                .w(32.0)
                .h(32.0)
                // .rounded(8.0)
                // .bg(bg)
                .flex()
                .items_center()
                .justify_center()
                .transform(blinc_core::Transform::scale(scale, scale))
                .child(
                    svg(item.icon_svg())
                        .square(ICON_SIZE)
                        .scale(scale)
                        .color(icon_color),
                )
        })
        .on_hover_enter({
            let dropdown = dropdown_for_hover.clone();
            move |event_ctx| {
                // Get the center X from event bounds
                let center_x = event_ctx.bounds_x + event_ctx.bounds_width / 2.0;
                dropdown.set(DropdownState {
                    item: Some(item),
                    center_x,
                });
            }
        })
}

/// The notched dropdown panel with collapse animation
#[allow(clippy::too_many_arguments)]
fn notched_dropdown(
    active_item: Option<MenuItem>,
    center_x: f32,
    width: f32,
    opacity: f32,
    height: f32,
    top_radius: f32,
    bottom_radius: f32,
    menu_bar_bg: Color,
) -> Notch {
    let content = dropdown_content(active_item);

    // Position dropdown so it's centered on the icon position (using animated width)
    let left = center_x - width / 2.0;

    // Calculate height ratio for padding animation (0 when collapsed, 1 when fully open)
    let full_height = DROPDOWN_HEIGHT + NOTCH_RADIUS * 2.0;
    let height_ratio = (height / full_height).clamp(0.0, 1.0);

    // Top concave radius stays full, bottom shrinks with height
    // Position at MENU_BAR_HEIGHT - top_radius so concave curves connect to menu bar.
    //
    // Shadow trick: `offset_y >= blur` makes the Gaussian shadow sit
    // ENTIRELY below the shape's top edge. The erf-based shadow alpha
    // is centered at y = shape_y + offset_y and falls off symmetrically
    // over `blur` px, so the top of the shadow lands at
    // y = shape_y + offset_y − blur ≥ shape_y. That means the
    // dropdown's top-edge shadow never bleeds upward into the menu bar
    // area at the concave merge. The trade-off is a slightly
    // offset-looking shadow (all the blur is below the shape), but
    // it's the simplest way to avoid the double-shadow at the merge.
    notch()
        .concave_top(top_radius)
        .rounded_bottom(bottom_radius)
        .bg(menu_bar_bg)
        .shadow(blinc_core::Shadow {
            offset_x: 0.0,
            offset_y: 6.0,
            blur: 8.0,
            spread: 0.0,
            color: Color::BLACK.with_alpha(0.12),
        })
        .opacity(opacity)
        // The dropdown's bbox (340 px wide, centered on the active
        // icon) overlaps neighbour menu-bar icons. Without this,
        // hit-test returns the dropdown for any cursor inside that
        // box and the underlying icons' `hover_enter` never fires —
        // hovering across the bar would only trigger every other
        // icon.
        .pointer_events_none()
        .absolute()
        .top(MENU_BAR_HEIGHT - top_radius)
        .bottom(12.0)
        .left(left)
        .w(width)
        .h(height) // Animated height for collapse effect
        .overflow_clip()
        .pt(top_radius + 12.0 * height_ratio) // Padding scales with height
        .pb(12.0 * height_ratio) // Animate to 0 when collapsed
        .px(16.0)
        // Always render content - it gets clipped by overflow_clip as height animates
        .child(
            div()
                .px(6.0)
                .w_full()
                .justify_center()
                .overflow_clip()
                .child(content),
        )
}

/// Content displayed in the dropdown based on active item
fn dropdown_content(item: Option<MenuItem>) -> Div {
    let text_primary = Color::WHITE;
    let text_secondary = Color::rgba(1.0, 1.0, 1.0, 0.6);
    let accent_orange = Color::from_hex(0xf59e0b);
    let accent_green = Color::from_hex(0x10b981);
    let accent_blue = Color::from_hex(0x3b82f6);
    let accent_cyan = Color::from_hex(0x06b6d4);
    let accent_purple = Color::from_hex(0xa855f7);

    match item {
        Some(MenuItem::Clock) => div()
            .flex_row()
            .items_center()
            .justify_center()
            .gap(8.0)
            .overflow_clip()
            .child(text("Wed Jan 8").size(14.0).color(accent_orange))
            .child(text("|").size(14.0).color(text_secondary))
            .child(text("10:42 AM").size(14.0).color(text_primary)),

        Some(MenuItem::Battery) => div()
            .flex_row()
            .items_center()
            .gap(8.0)
            .child(text("Battery").size(14.0).color(accent_green))
            .child(text("|").size(14.0).color(text_secondary))
            .child(text("87% Charged").size(14.0).color(text_primary)),

        Some(MenuItem::Wifi) => div()
            .flex_row()
            .items_center()
            .gap(8.0)
            .child(text("Network").size(14.0).color(accent_blue))
            .child(text("|").size(14.0).color(text_secondary))
            .child(text("Home WiFi").size(14.0).color(text_primary)),

        Some(MenuItem::Weather) => div()
            .flex_row()
            .gap_px(8.0)
            .w_full()
            .overflow_clip()
            .justify_center()
            .child(
                div()
                    .flex_row()
                    .items_center()
                    .gap_px(8.0)
                    .child(svg(WEATHER_SVG).square(20.0).color(accent_cyan))
                    .child(text("Cloudy").size(14.0).color(text_primary)),
            )
            .child(text("|").size(14.0).color(text_secondary))
            .child(
                div()
                    .flex_row()
                    .items_center()
                    .gap_px(8.0)
                    .child(text("80°F").size(14.0).color(text_primary))
                    .child(text("•").size(14.0).color(text_secondary))
                    .child(text("San Francisco").size(14.0).color(text_secondary)),
            ),

        Some(MenuItem::Music) => div()
            .flex_col()
            .gap(4.0)
            .child(
                div()
                    .flex_row()
                    .items_center()
                    .gap(8.0)
                    .child(
                        // Equalizer bars animation placeholder
                        div()
                            .flex_row()
                            .items_end()
                            .gap(2.0)
                            .h(16.0)
                            .child(div().w(3.0).h(8.0).bg(accent_purple).rounded(1.0))
                            .child(div().w(3.0).h(14.0).bg(accent_purple).rounded(1.0))
                            .child(div().w(3.0).h(10.0).bg(accent_purple).rounded(1.0))
                            .child(div().w(3.0).h(16.0).bg(accent_purple).rounded(1.0)),
                    )
                    .child(text("Now Playing").size(12.0).color(text_secondary)),
            )
            .child(
                text("Artist Name - Song Title")
                    .size(14.0)
                    .color(text_primary),
            ),

        None => div(),
    }
}

/// Navigation bar demonstrating the center bulge feature for active indicators
fn active_nav_bar() -> impl ElementBuilder {
    let nav_bg = Color::from_hex(0x1e293b); // Slate-800
    let active_bg = Color::from_hex(0x3b82f6); // Blue-500
    let icon_color = Color::WHITE.with_alpha(0.7);
    let active_icon_color = Color::WHITE;
    // The bulge should match the *roundness of the active button*:
    // button is 44×44 (radius 22), so we size the circular-arc cap so
    // its apex curvature radius = (half_w² + h²) / (2·h) is near 22.
    // (20² + 8.5²) / 17 ≈ 27.8 → close match. `bulge_corner_radius`
    // is the ear fillet where the arc joins the baseline.
    let bulge_height = 8.5;
    let bulge_width = 40.0;
    let bulge_corner_radius = 6.0;

    // Container for the nav bar
    div().flex_row().justify_center().child(
        // Navigation bar with bulge for active item
        notch()
            // Bulge protrudes top to highlight the active center item
            .center_bulge_top_rounded(bulge_width, bulge_height, bulge_corner_radius)
            .rounded(16.0)
            .bg(nav_bg)
            .h(56.0 + bulge_height) // Extra height for bulge
            .px(8.0)
            .flex_row()
            .items_center()
            .gap(4.0)
            // Home icon (inactive)
            .child(
                div()
                    .w(56.0)
                    .h(44.0)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(svg(HOME_SVG).square(22.0).color(icon_color)),
            )
            // Search icon (inactive)
            .child(
                div()
                    .w(56.0)
                    .h(44.0)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(svg(SEARCH_SVG).square(22.0).color(icon_color)),
            )
            // Center item (ACTIVE) - positioned in the bulge
            .child(
                div()
                    .w(44.0)
                    .h(44.0)
                    .rounded_full()
                    .bg(active_bg)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(svg(PLUS_SVG).square(24.0).color(active_icon_color)),
            )
            // User icon (inactive)
            .child(
                div()
                    .w(56.0)
                    .h(44.0)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(svg(USER_SVG).square(22.0).color(icon_color)),
            )
            // Settings icon (inactive)
            .child(
                div()
                    .w(56.0)
                    .h(44.0)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(svg(SETTINGS_SVG).square(22.0).color(icon_color)),
            ),
    )
}

/// Demo showing sharp V-shaped cuts and peaks
fn sharp_angle_demo() -> impl ElementBuilder {
    let bar_bg = Color::from_hex(0x374151); // Gray-700
    let peak_bg = Color::from_hex(0x059669); // Emerald-600
    let cut_bg = Color::from_hex(0xdc2626); // Red-600
    let icon_color = Color::WHITE.with_alpha(0.9);

    div()
        .flex_row()
        .items_end()
        .gap(24.0)
        // V-Cut example (sharp inward cut)
        .child(
            div()
                .flex_col()
                .items_center()
                .gap(4.0)
                .child(text("V-Cut (60° angle)").size(10.0).color(Color::GRAY))
                .child(
                    notch()
                        .center_cut_top(40.0, 16.0) // Width 40, depth 16 (creates ~60° angle)
                        .rounded(12.0)
                        .bg(cut_bg)
                        .w(160.0)
                        .h(44.0)
                        .flex_row()
                        .items_center()
                        .justify_center()
                        .gap(32.0)
                        .child(svg(HOME_SVG).square(20.0).color(icon_color))
                        .child(svg(SETTINGS_SVG).square(20.0).color(icon_color)),
                ),
        )
        // V-Peak example (sharp outward peak)
        .child(
            div()
                .flex_col()
                .items_center()
                .gap(4.0)
                .child(text("V-Peak (pointing up)").size(10.0).color(Color::GRAY))
                .child(
                    div().h(20.0).child(
                        // Extra height container for the peak
                        notch()
                            .center_peak_top(50.0, 18.0) // Width 50, height 18
                            .rounded(12.0)
                            .bg(peak_bg)
                            .w(180.0)
                            .h(52.0 + 18.0) // Base height + peak height
                            .flex_row()
                            .items_end()
                            .justify_center()
                            .pb(8.0)
                            .gap(16.0)
                            .child(svg(HOME_SVG).square(20.0).color(icon_color))
                            .child(
                                // Active indicator at peak position
                                div()
                                    .w(36.0)
                                    .h(36.0)
                                    .rounded_full()
                                    .bg(Color::WHITE)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(svg(PLUS_SVG).square(20.0).color(peak_bg)),
                            )
                            .child(svg(SETTINGS_SVG).square(20.0).color(icon_color)),
                    ),
                ),
        )
        // Steep cut example (narrow V)
        .child(
            div()
                .flex_col()
                .items_center()
                .gap(4.0)
                .child(text("Steep Cut (30° angle)").size(10.0).color(Color::GRAY))
                .child(
                    notch()
                        .center_cut_top(20.0, 20.0) // Width 20, depth 20 (creates ~30° angle)
                        .rounded(12.0)
                        .bg(bar_bg)
                        .w(140.0)
                        .h(44.0)
                        .flex_row()
                        .items_center()
                        .justify_center()
                        .gap(40.0)
                        .child(svg(SEARCH_SVG).square(20.0).color(icon_color))
                        .child(svg(USER_SVG).square(20.0).color(icon_color)),
                ),
        )
}

/// Bottom dock bar with Dynamic Island-style center scoop
fn bottom_dock_bar(_width: f32) -> impl ElementBuilder {
    let dock_bg = Color::rgba(0.1, 0.1, 0.1, 0.95);
    let icon_color = Color::rgba(1.0, 1.0, 1.0, 0.8);
    // Scoop geometry tuned for the 64×64 FAB below. The FAB is positioned
    // at top(-28) so its bottom edge lands at y=36 inside the dock. The
    // scoop carves a stadium pouch that leaves ~6 px of visible padding
    // between the button circle and the dock fill on all sides:
    //   width 80   →  8 px horizontal padding per side of the 64 px button
    //   depth 42   →  6 px below the button bottom (button y=36 → floor y=42)
    //   cr 8       → small ears at the top corners of the hollow
    // Bottom corner radius = depth/2 = 21, so the hollow reads as a
    // Dynamic-Island pill with a semicircular floor.
    let scoop_width = 80.0;
    let scoop_depth = 42.0;
    let scoop_corner_radius = 8.0;

    // Container with bottom margin
    div().w_full().flex_row().justify_center().child(
        notch()
            // Use rounded scoop for smoother aesthetic transitions
            .center_scoop_top_rounded(scoop_width, scoop_depth, scoop_corner_radius)
            .rounded_top(24.0)
            .bg(dock_bg)
            .w_fit()
            .h(50.0 + scoop_depth)
            // Padding for scoop is automatically applied by the notch implementation
            .child(
                div()
                    .w_full()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .gap(16.0)
                    .p(6.0)
                    .child(svg(CLOCK_SVG).square(ICON_SIZE).color(icon_color))
                    .child(svg(BATTERY_SVG).square(ICON_SIZE).color(icon_color))
                    .child(svg(WIFI_SVG).square(ICON_SIZE).color(icon_color))
                    .child(svg(WEATHER_SVG).square(ICON_SIZE).color(icon_color))
                    .child(svg(MUSIC_SVG).square(ICON_SIZE).color(icon_color)),
            )
            .child(
                // Floating button in the scoop
                div()
                    .absolute()
                    .top(-28.0)
                    .left(180.0)
                    .rounded_full()
                    .w(64.0)
                    .h(64.0)
                    .bg(Color::from_hex(0x39FF14)) // Neon green
                    .shadow_lg()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(svg(PLUS_SVG).square(28.0).color(Color::BLACK)),
            ),
    )
}
