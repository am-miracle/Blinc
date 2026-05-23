//! Image CSS Styling Demo
//!
//! Demonstrates CSS properties that work on images via stylesheets:
//! - opacity, border-radius, border, box-shadow
//! - transform (rotate, scale, translate) via parent divs
//! - CSS transitions and hover effects on image containers
//! - CSS filters (grayscale, sepia, invert, brightness, contrast, saturate, hue-rotate)
//!
//! Images are wrapped in divs with IDs for CSS targeting, since Image elements
//! inherit CSS properties from their parent container's RenderProps.
//!
//! Run with: cargo run -p blinc_app_examples --example image_css_demo

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;
use blinc_app::windowed::WindowedContext;

const IMG: &str = "examples/blinc_app_examples/examples/assets/avatar.jpg";

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    let config = WindowConfig {
        title: "Image CSS Styling Demo".to_string(),
        width: 1100,
        height: 800,
        resizable: true,
        fullscreen: false,
        ..Default::default()
    };

    WindowedApp::run(config, build_ui)
}

const STYLESHEET: &str = r#"
    /* ================================================ */
    /* Base Layout                                       */
    /* ================================================ */
    #root {
        background: #ffffff80;
        padding: 32px;
        gap: 28px;
        width: 100%;
    }

    /* ================================================ */
    /* 1. Opacity                                        */
    /* ================================================ */
    #img-opacity-100 {
        opacity: 1.0;
        border-radius: 8px;
    }
    #img-opacity-75 {
        opacity: 0.75;
        border-radius: 8px;
    }
    #img-opacity-50 {
        opacity: 0.5;
        border-radius: 8px;
    }
    #img-opacity-25 {
        opacity: 0.25;
        border-radius: 8px;
    }

    /* ================================================ */
    /* 2. Border Radius                                  */
    /* ================================================ */
    #img-radius-0 {
        border-radius: 0px;
    }
    #img-radius-12 {
        border-radius: 12px;
    }
    #img-radius-24 {
        border-radius: 24px;
    }
    #img-radius-circle {
        border-radius: 50px;
    }

    /* ================================================ */
    /* 3. Border                                         */
    /* ================================================ */
    #img-border-thin {
        border-radius: 12px;
        border-width: 2px;
        border-color: rgba(255, 255, 255, 0.6);
    }
    #img-border-thick {
        border-radius: 12px;
        border-width: 4px;
        border-color: #3b82f6;
    }
    #img-border-accent {
        border-radius: 50px;
        border-width: 3px;
        border-color: #f59e0b;
    }

    /* ================================================ */
    /* 4. Box Shadow                                     */
    /* ================================================ */
    #img-shadow-sm {
        border-radius: 12px;
        box-shadow: 0 2px 8px rgba(0, 0, 0, 0.4);
    }
    #img-shadow-md {
        border-radius: 12px;
        box-shadow: 0 4px 16px rgba(59, 130, 246, 0.5);
    }
    #img-shadow-lg {
        border-radius: 12px;
        box-shadow: 0 8px 32px rgba(245, 158, 11, 0.6);
    }

    /* ================================================ */
    /* 5. CSS Transform on parent (affects child image)  */
    /* ================================================ */
    #img-rotate {
        border-radius: 12px;
        transform: rotate(12deg);
    }
    #img-scale {
        border-radius: 12px;
        transform: scale(1.15);
    }
    #img-skew {
        border-radius: 12px;
        transform: skewX(-8deg);
    }

    /* ================================================ */
    /* 6. Hover Transitions                              */
    /* ================================================ */
    #img-hover-scale {
        border-radius: 12px;
        transition: transform 0.25s ease, box-shadow 0.25s ease;
        box-shadow: 0 2px 8px rgba(0, 0, 0, 0.3);
    }
    #img-hover-scale:hover {
        transform: scale(1.08);
        box-shadow: 0 8px 24px rgba(59, 130, 246, 0.5);
    }

    #img-hover-opacity {
        border-radius: 12px;
        opacity: 0.6;
        transition: opacity 0.3s ease, border-color 0.3s ease;
        border-width: 2px;
        border-color: rgba(255, 255, 255, 0.2);
    }
    #img-hover-opacity:hover {
        opacity: 1.0;
        border-color: rgba(255, 255, 255, 0.8);
    }

    #img-hover-rotate {
        border-radius: 50px;
        transition: transform 0.4s ease, box-shadow 0.3s ease;
        border-width: 2px;
        border-color: rgba(245, 158, 11, 0.6);
    }
    #img-hover-rotate:hover {
        transform: rotate(15deg) scale(1.15);
        box-shadow: 0 8px 24px rgba(245, 158, 11, 0.5);
    }

    #img-hover-shadow {
        border-radius: 12px;
        transition: box-shadow 0.3s ease;
        box-shadow: 0 0px 0px rgba(0, 0, 0, 0.0);
    }
    #img-hover-shadow:hover {
        box-shadow: 0 0px 40px rgba(139, 92, 246, 0.7);
    }

    /* ================================================ */
    /* 7. Background Image (CSS url())                   */
    /* ================================================ */
    #bg-image-card {
        width: 100px;
        height: 100px;
        border-radius: 16px;
        background: url("examples/blinc_app_examples/examples/assets/avatar.jpg");
        box-shadow: 0 4px 16px rgba(0, 0, 0, 0.4);
        border-width: 2px;
        border-color: rgba(255, 255, 255, 0.3);
        transition: transform 0.25s ease, box-shadow 0.25s ease;
    }
    #bg-image-card:hover {
        transform: scale(1.05);
        box-shadow: 0 8px 24px rgba(59, 130, 246, 0.5);
    }

    /* ================================================ */
    /* 8. CSS Filters                                    */
    /* ================================================ */
    #img-grayscale {
        border-radius: 12px;
        filter: grayscale(100%);
    }
    #img-sepia {
        border-radius: 12px;
        filter: sepia(100%);
    }
    #img-invert {
        border-radius: 12px;
        filter: invert(100%);
    }
    #img-brightness {
        border-radius: 12px;
        filter: brightness(150%);
    }
    #img-contrast {
        border-radius: 12px;
        filter: contrast(200%);
    }
    #img-saturate {
        border-radius: 12px;
        filter: saturate(300%);
    }
    #img-hue-rotate {
        border-radius: 12px;
        filter: hue-rotate(90deg);
    }
    #img-filter-combo {
        border-radius: 12px;
        filter: grayscale(50%) brightness(120%) contrast(110%);
    }

    /* ================================================ */
    /* 9. Mask Image (CSS Gradients)                     */
    /* ================================================ */
    #img-mask-fade-bottom {
        border-radius: 12px;
        mask-image: linear-gradient(to bottom, black, transparent);
    }
    #img-mask-fade-right {
        border-radius: 12px;
        mask-image: linear-gradient(to right, black, transparent);
    }
    #img-mask-radial {
        border-radius: 12px;
        mask-image: radial-gradient(circle, black, transparent);
    }
    #img-mask-diagonal {
        border-radius: 12px;
        mask-image: linear-gradient(135deg, black 0%, transparent 100%);
    }

    /* ================================================ */
    /* 10. Mask Image Transitions (Hover)               */
    /* ================================================ */
    #img-mask-hover-reveal {
        border-radius: 12px;
        mask-image: linear-gradient(to bottom, black, transparent);
        transition: mask-image 0.6s ease;
    }
    #img-mask-hover-reveal:hover {
        mask-image: linear-gradient(to bottom, black, black);
    }
    #img-mask-hover-radial {
        border-radius: 12px;
        mask-image: radial-gradient(circle, black, transparent);
        transition: mask-image 0.5s ease;
    }
    #img-mask-hover-radial:hover {
        mask-image: radial-gradient(circle, black, black);
    }
    #img-mask-hover-fade {
        border-radius: 12px;
        mask-image: linear-gradient(to right, black, black);
        transition: mask-image 0.5s ease;
    }
    #img-mask-hover-fade:hover {
        mask-image: linear-gradient(to right, black, transparent);
    }
"#;

// ============================================================================
// UI STRUCTURE
// ============================================================================

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder + use<> {
    if ctx.rebuild_count == 0 {
        ctx.add_css(STYLESHEET);
    }

    div()
        .id("root")
        .flex_col()
        .overflow_y_scroll()
        // Header
        .child(
            div()
                .flex_col()
                .gap_px(4.0)
                .child(
                    text("Image CSS Styling Demo")
                        .size(28.0)
                        .weight(FontWeight::Bold)
                        .color(Color::WHITE),
                )
                .child(
                    text(
                        "All visual styles applied via ctx.add_css() — Rust only defines structure",
                    )
                    .size(15.0)
                    .color(Color::rgba(1.0, 1.0, 1.0, 0.5)),
                ),
        )
        // Sections
        .child(opacity_section())
        .child(border_radius_section())
        .child(border_section())
        .child(shadow_section())
        .child(transform_section())
        .child(hover_section())
        .child(bg_image_section())
        .child(filter_section())
        .child(mask_section())
        .child(mask_transition_section())
}

// ============================================================================
// HELPER: image card with label
// ============================================================================

fn img_card(id: &str, label: &str) -> impl ElementBuilder + use<> {
    div()
        .flex_col()
        .gap_px(6.0)
        .items_center()
        .child(
            div()
                .id(id)
                .w(100.0)
                .h(100.0)
                .overflow_clip()
                .child(image(IMG).cover().h(100.0)),
        )
        .child(
            text(label)
                .size(12.0)
                .color(Color::rgba(1.0, 1.0, 1.0, 0.5))
                .weight(FontWeight::SemiBold),
        )
}

fn section(title: &str) -> Div {
    div().flex_col().gap_px(12.0).child(
        text(title)
            .size(18.0)
            .weight(FontWeight::SemiBold)
            .color(Color::rgba(1.0, 1.0, 1.0, 0.85)),
    )
}

// ============================================================================
// SECTIONS
// ============================================================================

fn opacity_section() -> impl ElementBuilder + use<> {
    section("1. Opacity").child(
        div()
            .flex_row()
            .gap_px(20.0)
            .items_end()
            .child(img_card("img-opacity-100", "opacity: 1.0"))
            .child(img_card("img-opacity-75", "opacity: 0.75"))
            .child(img_card("img-opacity-50", "opacity: 0.50"))
            .child(img_card("img-opacity-25", "opacity: 0.25")),
    )
}

fn border_radius_section() -> impl ElementBuilder + use<> {
    section("2. Border Radius").child(
        div()
            .flex_row()
            .gap_px(20.0)
            .items_end()
            .child(img_card("img-radius-0", "0px"))
            .child(img_card("img-radius-12", "12px"))
            .child(img_card("img-radius-24", "24px"))
            .child(img_card("img-radius-circle", "50px (circle)")),
    )
}

fn border_section() -> impl ElementBuilder + use<> {
    section("3. Border").child(
        div()
            .flex_row()
            .gap_px(20.0)
            .items_end()
            .child(img_card("img-border-thin", "2px white"))
            .child(img_card("img-border-thick", "4px blue"))
            .child(img_card("img-border-accent", "3px amber circle")),
    )
}

fn shadow_section() -> impl ElementBuilder + use<> {
    section("4. Box Shadow").child(
        div()
            .flex_row()
            .gap_px(20.0)
            .items_end()
            .child(img_card("img-shadow-sm", "small"))
            .child(img_card("img-shadow-md", "medium (blue)"))
            .child(img_card("img-shadow-lg", "large (amber)")),
    )
}

fn transform_section() -> impl ElementBuilder + use<> {
    section("5. CSS Transform (via parent)").child(
        div()
            .flex_row()
            .gap_px(40.0)
            .items_end()
            .child(img_card("img-rotate", "rotate(12deg)"))
            .child(img_card("img-scale", "scale(1.15)"))
            .child(img_card("img-skew", "skewX(-8deg)")),
    )
}

fn hover_section() -> impl ElementBuilder + use<> {
    section("6. Hover Transitions (hover over images)").child(
        div()
            .flex_row()
            .gap_px(20.0)
            .items_end()
            .child(img_card("img-hover-scale", "scale + shadow"))
            .child(img_card("img-hover-opacity", "opacity + border"))
            .child(img_card("img-hover-rotate", "rotate + scale"))
            .child(img_card("img-hover-shadow", "glow shadow")),
    )
}

fn filter_section() -> impl ElementBuilder + use<> {
    section("8. CSS Filters").child(
        div()
            .flex_row()
            .flex_wrap()
            .gap_px(20.0)
            .items_end()
            .child(img_card("img-grayscale", "grayscale(100%)"))
            .child(img_card("img-sepia", "sepia(100%)"))
            .child(img_card("img-invert", "invert(100%)"))
            .child(img_card("img-brightness", "brightness(150%)"))
            .child(img_card("img-contrast", "contrast(200%)"))
            .child(img_card("img-saturate", "saturate(300%)"))
            .child(img_card("img-hue-rotate", "hue-rotate(90deg)"))
            .child(img_card("img-filter-combo", "combo")),
    )
}

fn bg_image_section() -> impl ElementBuilder + use<> {
    section("7. Background Image (CSS url())").child(
        div().flex_row().gap_px(20.0).items_end().child(
            div()
                .flex_col()
                .gap_px(6.0)
                .items_center()
                .child(div().id("bg-image-card"))
                .child(
                    text("background: url(...)")
                        .size(12.0)
                        .color(Color::rgba(1.0, 1.0, 1.0, 0.5))
                        .weight(FontWeight::SemiBold),
                ),
        ),
    )
}

fn mask_section() -> impl ElementBuilder + use<> {
    section("9. Mask Image (CSS Gradients)").child(
        div()
            .flex_row()
            .gap_px(20.0)
            .items_end()
            .child(img_card("img-mask-fade-bottom", "fade bottom"))
            .child(img_card("img-mask-fade-right", "fade right"))
            .child(img_card("img-mask-radial", "radial reveal"))
            .child(img_card("img-mask-diagonal", "diagonal")),
    )
}

fn mask_transition_section() -> impl ElementBuilder + use<> {
    section("10. Mask Transitions (Hover)").child(
        div()
            .flex_row()
            .gap_px(20.0)
            .items_end()
            .child(img_card("img-mask-hover-reveal", "reveal down"))
            .child(img_card("img-mask-hover-radial", "radial in"))
            .child(img_card("img-mask-hover-fade", "fade right")),
    )
}
