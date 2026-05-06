//! Tabler Icons Demo
//!
//! Showcases outline and filled icons from the blinc_tabler_icons crate.
//!
//! Run with: cargo run -p blinc_app_examples --example tabler_icons_demo --features tabler-icons

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;
use blinc_app::windowed::WindowedContext;
use blinc_core::Color;
use blinc_tabler_icons::{filled, outline};
use blinc_theme::{ColorToken, ThemeState};

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let config = WindowConfig {
        title: "Tabler Icons Demo".to_string(),
        width: 960,
        height: 800,
        resizable: true,
        ..Default::default()
    };

    WindowedApp::run(config, build_ui)
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    let theme = ThemeState::get();
    let bg = theme.color(ColorToken::Background);

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(bg)
        .flex_col()
        .child(demo_header())
        .child(
            scroll().w_full().h(ctx.height - 60.0).child(
                div()
                    .w_full()
                    .p(6.0)
                    .flex_col()
                    .gap(8.0)
                    .max_w(880.0)
                    .child(outline_icons_section())
                    .child(filled_icons_section())
                    .child(size_variants_section())
                    .child(color_variants_section())
                    .child(side_by_side_section()),
            ),
        )
}

// ============================================================================
// HEADER
// ============================================================================

fn demo_header() -> impl ElementBuilder {
    let theme = ThemeState::get();
    let surface = theme.color(ColorToken::Surface);
    let text_primary = theme.color(ColorToken::TextPrimary);
    let text_secondary = theme.color(ColorToken::TextSecondary);
    let border_color = theme.color(ColorToken::Border);

    div()
        .w_full()
        .h(60.0)
        .bg(surface)
        .border_bottom(1.0, border_color)
        .flex_row()
        .items_center()
        .px(24.0)
        .gap(12.0)
        .child(tabler_outline_icon(outline::ICONS, 28.0, text_primary))
        .child(
            text("Tabler Icons Demo")
                .size(18.0)
                .bold()
                .color(text_primary),
        )
        .child(
            text("4,983 outline + 1,053 filled")
                .size(13.0)
                .color(text_secondary),
        )
}

// ============================================================================
// OUTLINE ICONS
// ============================================================================

fn outline_icons_section() -> impl ElementBuilder {
    div()
        .flex_col()
        .gap(4.0)
        .child(section_heading(
            "Outline Icons",
            "Stroke-based, 24x24 viewBox, stroke-width 2",
        ))
        .child(
            div()
                .flex_row()
                .flex_wrap()
                .gap(1.0)
                // Navigation
                .child(outline_tile(outline::HOME, "home"))
                .child(outline_tile(outline::ARROW_LEFT, "arrow-left"))
                .child(outline_tile(outline::ARROW_RIGHT, "arrow-right"))
                .child(outline_tile(outline::ARROW_UP, "arrow-up"))
                .child(outline_tile(outline::ARROW_DOWN, "arrow-down"))
                .child(outline_tile(outline::CHEVRON_LEFT, "chevron-left"))
                .child(outline_tile(outline::CHEVRON_RIGHT, "chevron-right"))
                .child(outline_tile(outline::MENU_2, "menu-2"))
                // Actions
                .child(outline_tile(outline::CHECK, "check"))
                .child(outline_tile(outline::X, "x"))
                .child(outline_tile(outline::PLUS, "plus"))
                .child(outline_tile(outline::MINUS, "minus"))
                .child(outline_tile(outline::SEARCH, "search"))
                .child(outline_tile(outline::SETTINGS, "settings"))
                .child(outline_tile(outline::EDIT, "edit"))
                .child(outline_tile(outline::TRASH, "trash"))
                .child(outline_tile(outline::COPY, "copy"))
                .child(outline_tile(outline::CLIPBOARD, "clipboard"))
                // Communication
                .child(outline_tile(outline::MAIL, "mail"))
                .child(outline_tile(outline::MESSAGE, "message"))
                .child(outline_tile(outline::PHONE, "phone"))
                .child(outline_tile(outline::SEND, "send"))
                .child(outline_tile(outline::BELL, "bell"))
                // Files
                .child(outline_tile(outline::FILE, "file"))
                .child(outline_tile(outline::FOLDER, "folder"))
                .child(outline_tile(outline::DOWNLOAD, "download"))
                .child(outline_tile(outline::UPLOAD, "upload"))
                .child(outline_tile(outline::CLOUD, "cloud"))
                // User
                .child(outline_tile(outline::USER, "user"))
                .child(outline_tile(outline::USERS, "users"))
                .child(outline_tile(outline::LOGIN, "login"))
                .child(outline_tile(outline::LOGOUT, "logout"))
                // Media
                .child(outline_tile(outline::PHOTO, "photo"))
                .child(outline_tile(outline::VIDEO, "video"))
                .child(outline_tile(outline::MUSIC, "music"))
                .child(outline_tile(outline::PLAYER_PLAY, "player-play"))
                .child(outline_tile(outline::PLAYER_PAUSE, "player-pause"))
                .child(outline_tile(outline::VOLUME, "volume"))
                // Objects
                .child(outline_tile(outline::STAR, "star"))
                .child(outline_tile(outline::HEART, "heart"))
                .child(outline_tile(outline::BOOKMARK, "bookmark"))
                .child(outline_tile(outline::FLAG, "flag"))
                .child(outline_tile(outline::MAP_PIN, "map-pin"))
                .child(outline_tile(outline::CLOCK, "clock"))
                .child(outline_tile(outline::CALENDAR, "calendar"))
                .child(outline_tile(outline::LOCK, "lock"))
                .child(outline_tile(outline::KEY, "key"))
                // Dev
                .child(outline_tile(outline::CODE, "code"))
                .child(outline_tile(outline::TERMINAL, "terminal"))
                .child(outline_tile(outline::DATABASE, "database"))
                .child(outline_tile(outline::SERVER, "server"))
                .child(outline_tile(outline::BUG, "bug"))
                .child(outline_tile(outline::GIT_BRANCH, "git-branch"))
                .child(outline_tile(outline::GIT_MERGE, "git-merge"))
                // Brand
                .child(outline_tile(outline::BRAND_GITHUB, "brand-github"))
                .child(outline_tile(outline::BRAND_TWITTER, "brand-twitter"))
                .child(outline_tile(outline::BRAND_APPLE, "brand-apple"))
                .child(outline_tile(outline::BRAND_GOOGLE, "brand-google"))
                .child(outline_tile(outline::BRAND_RUST, "brand-rust")),
        )
}

// ============================================================================
// FILLED ICONS
// ============================================================================

fn filled_icons_section() -> impl ElementBuilder {
    div()
        .flex_col()
        .gap(4.0)
        .child(section_heading("Filled Icons", "Solid fill, no stroke"))
        .child(
            div()
                .flex_row()
                .flex_wrap()
                .gap(1.0)
                .child(filled_tile(filled::HOME, "home"))
                .child(filled_tile(filled::STAR, "star"))
                .child(filled_tile(filled::HEART, "heart"))
                .child(filled_tile(filled::BELL, "bell"))
                .child(filled_tile(filled::BOOKMARK, "bookmark"))
                .child(filled_tile(filled::FLAG, "flag"))
                .child(filled_tile(filled::MAP_PIN, "map-pin"))
                .child(filled_tile(filled::LOCK, "lock"))
                .child(filled_tile(filled::EYE, "eye"))
                .child(filled_tile(filled::CLOUD, "cloud"))
                .child(filled_tile(filled::CIRCLE_CHECK, "circle-check"))
                .child(filled_tile(filled::CIRCLE_X, "circle-x"))
                .child(filled_tile(filled::ALERT_TRIANGLE, "alert-triangle"))
                .child(filled_tile(filled::INFO_CIRCLE, "info-circle"))
                .child(filled_tile(filled::PHOTO, "photo"))
                .child(filled_tile(filled::PLAYER_PLAY, "player-play"))
                .child(filled_tile(filled::THUMB_UP, "thumb-up"))
                .child(filled_tile(filled::THUMB_DOWN, "thumb-down"))
                .child(filled_tile(filled::PIN, "pin"))
                .child(filled_tile(filled::CROWN, "crown"))
                .child(filled_tile(filled::DIAMOND, "diamond"))
                .child(filled_tile(filled::BOLT, "bolt"))
                .child(filled_tile(filled::FLAME, "flame"))
                .child(filled_tile(filled::SHIELD_CHECK, "shield-check"))
                .child(filled_tile(filled::MOOD_SMILE, "mood-smile")),
        )
}

// ============================================================================
// SIZE VARIANTS
// ============================================================================

fn size_variants_section() -> impl ElementBuilder {
    div()
        .flex_col()
        .gap(4.0)
        .child(section_heading(
            "Size Variants",
            "Same icon at different sizes",
        ))
        .child(
            div()
                .flex_row()
                .items_end()
                .gap(5.0)
                .child(size_sample(outline::ROCKET, "12px", 12.0))
                .child(size_sample(outline::ROCKET, "16px", 16.0))
                .child(size_sample(outline::ROCKET, "20px", 20.0))
                .child(size_sample(outline::ROCKET, "24px", 24.0))
                .child(size_sample(outline::ROCKET, "32px", 32.0))
                .child(size_sample(outline::ROCKET, "48px", 48.0)),
        )
}

fn size_sample(icon_data: &str, txt: &str, size: f32) -> impl ElementBuilder {
    let theme = ThemeState::get();
    let text_primary = theme.color(ColorToken::TextPrimary);
    let text_secondary = theme.color(ColorToken::TextSecondary);

    div()
        .flex_col()
        .items_center()
        .gap(2.0)
        .child(tabler_outline_icon(icon_data, size, text_primary))
        .child(text(txt).size(10.0).color(text_secondary))
}

// ============================================================================
// COLOR VARIANTS
// ============================================================================

fn color_variants_section() -> impl ElementBuilder {
    let theme = ThemeState::get();
    let text_primary = theme.color(ColorToken::TextPrimary);
    let primary = theme.color(ColorToken::Primary);
    let success = theme.color(ColorToken::Success);
    let warning = theme.color(ColorToken::Warning);
    let error = theme.color(ColorToken::Error);

    div()
        .flex_col()
        .gap(4.0)
        .child(section_heading(
            "Color Variants",
            "Theme color tokens applied to icons",
        ))
        .child(
            div()
                .flex_row()
                .items_center()
                .gap(4.0)
                .child(color_sample(outline::HEART, "Default", 24.0, text_primary))
                .child(color_sample(outline::HEART, "Primary", 24.0, primary))
                .child(color_sample(outline::HEART, "Success", 24.0, success))
                .child(color_sample(outline::HEART, "Warning", 24.0, warning))
                .child(color_sample(outline::HEART, "Error", 24.0, error)),
        )
        .child(
            div()
                .flex_row()
                .items_center()
                .gap(4.0)
                .child(filled_color_sample(
                    filled::HEART,
                    "Default",
                    24.0,
                    text_primary,
                ))
                .child(filled_color_sample(filled::HEART, "Primary", 24.0, primary))
                .child(filled_color_sample(filled::HEART, "Success", 24.0, success))
                .child(filled_color_sample(filled::HEART, "Warning", 24.0, warning))
                .child(filled_color_sample(filled::HEART, "Error", 24.0, error)),
        )
}

fn color_sample(icon_data: &str, txt: &str, size: f32, color: Color) -> impl ElementBuilder {
    let theme = ThemeState::get();
    let text_secondary = theme.color(ColorToken::TextSecondary);

    div()
        .flex_col()
        .items_center()
        .gap(2.0)
        .child(tabler_outline_icon(icon_data, size, color))
        .child(text(txt).size(10.0).color(text_secondary))
}

fn filled_color_sample(icon_data: &str, txt: &str, size: f32, color: Color) -> impl ElementBuilder {
    let theme = ThemeState::get();
    let text_secondary = theme.color(ColorToken::TextSecondary);

    div()
        .flex_col()
        .items_center()
        .gap(2.0)
        .child(tabler_filled_icon(icon_data, size, color))
        .child(text(txt).size(10.0).color(text_secondary))
}

// ============================================================================
// SIDE BY SIDE: OUTLINE vs FILLED
// ============================================================================

fn side_by_side_section() -> impl ElementBuilder {
    div()
        .flex_col()
        .gap(4.0)
        .child(section_heading(
            "Outline vs Filled",
            "Same icon in both variants side by side",
        ))
        .child(
            div()
                .flex_row()
                .flex_wrap()
                .gap(2.0)
                .child(comparison_tile(outline::HOME, filled::HOME, "home"))
                .child(comparison_tile(outline::STAR, filled::STAR, "star"))
                .child(comparison_tile(outline::HEART, filled::HEART, "heart"))
                .child(comparison_tile(outline::BELL, filled::BELL, "bell"))
                .child(comparison_tile(
                    outline::BOOKMARK,
                    filled::BOOKMARK,
                    "bookmark",
                ))
                .child(comparison_tile(outline::FLAG, filled::FLAG, "flag"))
                .child(comparison_tile(outline::LOCK, filled::LOCK, "lock"))
                .child(comparison_tile(outline::EYE, filled::EYE, "eye"))
                .child(comparison_tile(outline::CLOUD, filled::CLOUD, "cloud"))
                .child(comparison_tile(outline::BOLT, filled::BOLT, "bolt"))
                .child(comparison_tile(outline::FLAME, filled::FLAME, "flame"))
                .child(comparison_tile(outline::CROWN, filled::CROWN, "crown")),
        )
}

fn comparison_tile(outline_data: &str, filled_data: &str, name: &str) -> impl ElementBuilder {
    let theme = ThemeState::get();
    let text_primary = theme.color(ColorToken::TextPrimary);
    let text_tertiary = theme.color(ColorToken::TextTertiary);
    let border_color = theme.color(ColorToken::Border);
    let surface = theme.color(ColorToken::Surface);

    div()
        .flex_col()
        .items_center()
        .gap(2.0)
        .p(6.0)
        .bg(surface)
        .border(1.0, border_color)
        .rounded(8.0)
        .child(
            div()
                .flex_row()
                .gap(3.0)
                .child(tabler_outline_icon(outline_data, 24.0, text_primary))
                .child(tabler_filled_icon(filled_data, 24.0, text_primary)),
        )
        .child(text(name).size(9.0).color(text_tertiary))
}

// ============================================================================
// HELPERS
// ============================================================================

fn section_heading(title: &str, subtitle: &str) -> impl ElementBuilder {
    let theme = ThemeState::get();
    let text_primary = theme.color(ColorToken::TextPrimary);
    let text_secondary = theme.color(ColorToken::TextSecondary);

    div()
        .flex_col()
        .gap(1.0)
        .child(text(title).size(16.0).bold().color(text_primary))
        .child(text(subtitle).size(12.0).color(text_secondary))
}

fn outline_tile(icon_data: &str, name: &str) -> impl ElementBuilder {
    let theme = ThemeState::get();
    let text_primary = theme.color(ColorToken::TextPrimary);
    let text_tertiary = theme.color(ColorToken::TextTertiary);
    let border_color = theme.color(ColorToken::Border);

    div()
        .flex_col()
        .items_center()
        .gap(1.0)
        .p(2.0)
        .w(76.0)
        .border(1.0, border_color)
        .rounded(6.0)
        .child(tabler_outline_icon(icon_data, 24.0, text_primary))
        .child(text(name).size(8.0).color(text_tertiary))
}

fn filled_tile(icon_data: &str, name: &str) -> impl ElementBuilder {
    let theme = ThemeState::get();
    let text_primary = theme.color(ColorToken::TextPrimary);
    let text_tertiary = theme.color(ColorToken::TextTertiary);
    let border_color = theme.color(ColorToken::Border);

    div()
        .flex_col()
        .items_center()
        .gap(1.0)
        .p(2.0)
        .w(76.0)
        .border(1.0, border_color)
        .rounded(6.0)
        .child(tabler_filled_icon(icon_data, 24.0, text_primary))
        .child(text(name).size(8.0).color(text_tertiary))
}

fn tabler_outline_icon(path_data: &str, size: f32, color: Color) -> impl ElementBuilder {
    let svg_str = blinc_tabler_icons::to_svg(path_data, size);
    svg(&svg_str).size(size, size).color(color)
}

fn tabler_filled_icon(path_data: &str, size: f32, color: Color) -> impl ElementBuilder {
    let svg_str = blinc_tabler_icons::to_svg_filled(path_data, size);
    svg(&svg_str).size(size, size).color(color)
}
