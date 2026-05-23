//! Emoji and HTML Entities Demo
//!
//! This example demonstrates:
//! - HTML entity decoding in text() elements
//! - Emoji rendering with system fonts
//! - ASCII special characters
//! - Unicode symbols
//!
//! Run with: cargo run -p blinc_app_examples --example emoji_demo

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_core::Color;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    let config = WindowConfig {
        title: "Emoji & HTML Entities Demo".to_string(),
        width: 900,
        height: 800,
        resizable: true,
        ..Default::default()
    };

    blinc_app::windowed::WindowedApp::run(config, build_ui)
}

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder + use<> {
    scroll()
        .w(ctx.width)
        .h(ctx.height)
        .direction(ScrollDirection::Vertical)
        .child(
            div()
                .w_full()
                .bg(Color::rgba(0.08, 0.08, 0.1, 1.0))
                .flex_col()
                .items_center()
                .gap(5.0)
                .p(24.0)
                // Title
                .child(
                    h1("Emoji & HTML Entities Demo")
                        .color(Color::WHITE)
                        .text_center(),
                )
                .child(muted("Testing HTML entity decoding and emoji rendering").text_center())
                // Sections
                .child(html_entities_section())
                .child(emoji_section())
                .child(math_symbols_section())
                .child(arrows_section())
                .child(currency_section())
                .child(punctuation_section())
                .child(greek_letters_section())
                .child(mixed_content_section()),
        )
}

/// Section showing HTML named entities
fn html_entities_section() -> Div {
    section_card(
        "HTML Named Entities",
        "Automatic decoding of &amp;name; format",
    )
    .child(entity_row("&amp;", "Ampersand"))
    .child(entity_row("&lt;", "Less than"))
    .child(entity_row("&gt;", "Greater than"))
    .child(entity_row("&quot;", "Quote"))
    .child(entity_row("&apos;", "Apostrophe"))
    .child(entity_row("&nbsp;", "Non-breaking space (between these)"))
    .child(entity_row("&copy;", "Copyright"))
    .child(entity_row("&reg;", "Registered"))
    .child(entity_row("&trade;", "Trademark"))
    .child(entity_row("&deg;", "Degree"))
    .child(entity_row("&plusmn;", "Plus-minus"))
    .child(entity_row("&frac12;", "One half"))
    .child(entity_row("&frac14;", "One quarter"))
    .child(entity_row("&times;", "Multiplication"))
    .child(entity_row("&divide;", "Division"))
}

/// Section showing emoji as images
fn emoji_section() -> Div {
    section_card("Emoji (as Images)", "Rendered using system emoji font")
        // Faces - using emoji_sized() helper
        .child(
            div()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(8.0)
                .child(text("Faces:").color(Color::rgba(0.6, 0.6, 0.6, 1.0)))
                .child(emoji_sized("😀", 32.0))
                .child(emoji_sized("😃", 32.0))
                .child(emoji_sized("😄", 32.0))
                .child(emoji_sized("😁", 32.0))
                .child(emoji_sized("😆", 32.0))
                .child(emoji_sized("😅", 32.0))
                .child(emoji_sized("🤣", 32.0))
                .child(emoji_sized("😂", 32.0)),
        )
        // Hearts
        .child(
            div()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(8.0)
                .child(text("Hearts:").color(Color::rgba(0.6, 0.6, 0.6, 1.0)))
                .child(emoji_sized("❤️", 32.0))
                .child(emoji_sized("🧡", 32.0))
                .child(emoji_sized("💛", 32.0))
                .child(emoji_sized("💚", 32.0))
                .child(emoji_sized("💙", 32.0))
                .child(emoji_sized("💜", 32.0))
                .child(emoji_sized("🖤", 32.0))
                .child(emoji_sized("🤍", 32.0)),
        )
        // Hands
        .child(
            div()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(8.0)
                .child(text("Hands:").color(Color::rgba(0.6, 0.6, 0.6, 1.0)))
                .child(emoji_sized("👍", 32.0))
                .child(emoji_sized("👎", 32.0))
                .child(emoji_sized("👏", 32.0))
                .child(emoji_sized("🙌", 32.0))
                .child(emoji_sized("🤝", 32.0))
                .child(emoji_sized("✊", 32.0))
                .child(emoji_sized("✌️", 32.0))
                .child(emoji_sized("🤞", 32.0)),
        )
        // Objects
        .child(
            div()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(8.0)
                .child(text("Objects:").color(Color::rgba(0.6, 0.6, 0.6, 1.0)))
                .child(emoji_sized("🎉", 32.0))
                .child(emoji_sized("🎊", 32.0))
                .child(emoji_sized("🎁", 32.0))
                .child(emoji_sized("🎈", 32.0))
                .child(emoji_sized("🔥", 32.0))
                .child(emoji_sized("⭐", 32.0))
                .child(emoji_sized("💡", 32.0))
                .child(emoji_sized("📱", 32.0)),
        )
        // Animals
        .child(
            div()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(8.0)
                .child(text("Animals:").color(Color::rgba(0.6, 0.6, 0.6, 1.0)))
                .child(emoji_sized("🐶", 32.0))
                .child(emoji_sized("🐱", 32.0))
                .child(emoji_sized("🐭", 32.0))
                .child(emoji_sized("🐹", 32.0))
                .child(emoji_sized("🐰", 32.0))
                .child(emoji_sized("🦊", 32.0))
                .child(emoji_sized("🐻", 32.0))
                .child(emoji_sized("🐼", 32.0)),
        )
        // Food
        .child(
            div()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(8.0)
                .child(text("Food:").color(Color::rgba(0.6, 0.6, 0.6, 1.0)))
                .child(emoji_sized("🍎", 32.0))
                .child(emoji_sized("🍕", 32.0))
                .child(emoji_sized("🍔", 32.0))
                .child(emoji_sized("🌮", 32.0))
                .child(emoji_sized("🍜", 32.0))
                .child(emoji_sized("🍣", 32.0))
                .child(emoji_sized("🍩", 32.0))
                .child(emoji_sized("☕", 32.0)),
        )
        // Weather
        .child(
            div()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(8.0)
                .child(text("Weather:").color(Color::rgba(0.6, 0.6, 0.6, 1.0)))
                .child(emoji_sized("☀️", 32.0))
                .child(emoji_sized("🌤", 32.0))
                .child(emoji_sized("⛅", 32.0))
                .child(emoji_sized("🌧", 32.0))
                .child(emoji_sized("⛈", 32.0))
                .child(emoji_sized("🌩", 32.0))
                .child(emoji_sized("❄️", 32.0))
                .child(emoji_sized("🌈", 32.0)),
        )
        // Big emoji showcase
        .child(
            div()
                .flex_row()
                .flex_wrap()
                .items_center()
                .justify_center()
                .gap(16.0)
                .pt(16.0)
                .child(text("Large:").color(Color::rgba(0.6, 0.6, 0.6, 1.0)))
                .child(emoji_sized("🚀", 64.0))
                .child(emoji_sized("🎨", 64.0))
                .child(emoji_sized("🎮", 64.0))
                .child(emoji_sized("🏆", 64.0)),
        )
}

/// Section showing mathematical symbols
fn math_symbols_section() -> Div {
    section_card("Mathematical Symbols", "Math entities and operators")
        .child(entity_row("&infin;", "Infinity"))
        .child(entity_row("&ne;", "Not equal"))
        .child(entity_row("&le;", "Less than or equal"))
        .child(entity_row("&ge;", "Greater than or equal"))
        .child(entity_row("&sum;", "Summation"))
        .child(entity_row("&prod;", "Product"))
        .child(entity_row("&radic;", "Square root"))
        .child(entity_row("&int;", "Integral"))
        .child(entity_row("&part;", "Partial differential"))
        .child(entity_row("&nabla;", "Nabla/Del"))
        .child(entity_row("&prop;", "Proportional to"))
        .child(entity_row("&asymp;", "Approximately equal"))
}

/// Section showing arrows
fn arrows_section() -> Div {
    section_card("Arrows", "Directional arrow entities")
        .child(entity_row("&larr;", "Left arrow"))
        .child(entity_row("&rarr;", "Right arrow"))
        .child(entity_row("&uarr;", "Up arrow"))
        .child(entity_row("&darr;", "Down arrow"))
        .child(entity_row("&harr;", "Left-right arrow"))
        .child(entity_row("&lArr;", "Double left arrow"))
        .child(entity_row("&rArr;", "Double right arrow"))
        .child(entity_row("&hArr;", "Double left-right arrow"))
        .child(entity_row("&crarr;", "Carriage return arrow"))
}

/// Section showing currency symbols
fn currency_section() -> Div {
    section_card("Currency Symbols", "Currency entities")
        .child(entity_row("&euro;", "Euro"))
        .child(entity_row("&pound;", "British Pound"))
        .child(entity_row("&yen;", "Japanese Yen"))
        .child(entity_row("&cent;", "Cent"))
        .child(entity_row("&#36;", "Dollar (numeric)"))
        .child(entity_row("&#8377;", "Indian Rupee (numeric)"))
        .child(entity_row("&#8369;", "Philippine Peso (numeric)"))
        .child(entity_row("&#8361;", "Korean Won (numeric)"))
}

/// Section showing punctuation and quotes
fn punctuation_section() -> Div {
    section_card("Punctuation & Quotes", "Quotation marks and dashes")
        .child(entity_row(
            "&ldquo;Hello&rdquo;",
            "Left/right double quotes",
        ))
        .child(entity_row("&lsquo;Hi&rsquo;", "Left/right single quotes"))
        .child(entity_row("&laquo;Bonjour&raquo;", "Guillemets"))
        .child(entity_row("&ndash;", "En dash"))
        .child(entity_row("&mdash;", "Em dash"))
        .child(entity_row("&hellip;", "Horizontal ellipsis"))
        .child(entity_row("&bull;", "Bullet"))
        .child(entity_row("&middot;", "Middle dot"))
}

/// Section showing Greek letters
fn greek_letters_section() -> Div {
    section_card("Greek Letters", "Greek alphabet entities")
        .child(
            div()
                .flex_row()
                .flex_wrap()
                .gap(4.0)
                .child(
                    text("Uppercase:")
                        .size(14.0)
                        .color(Color::rgba(0.6, 0.6, 0.6, 1.0)),
                )
                .child(
                    text("&Alpha; &Beta; &Gamma; &Delta; &Epsilon; &Zeta; &Eta; &Theta;")
                        .size(18.0)
                        .color(Color::WHITE),
                ),
        )
        .child(
            div()
                .flex_row()
                .flex_wrap()
                .gap(4.0)
                .child(
                    text("Lowercase:")
                        .size(14.0)
                        .color(Color::rgba(0.6, 0.6, 0.6, 1.0)),
                )
                .child(
                    text("&alpha; &beta; &gamma; &delta; &epsilon; &zeta; &eta; &theta;")
                        .size(18.0)
                        .color(Color::WHITE),
                ),
        )
        .child(
            div()
                .flex_row()
                .flex_wrap()
                .gap(4.0)
                .child(
                    text("More:")
                        .size(14.0)
                        .color(Color::rgba(0.6, 0.6, 0.6, 1.0)),
                )
                .child(
                    text("&pi; &sigma; &omega; &lambda; &mu; &phi; &psi; &chi;")
                        .size(18.0)
                        .color(Color::WHITE),
                ),
        )
}

/// Section showing mixed emoji and entities
fn mixed_content_section() -> Div {
    section_card("Mixed Content", "Combining emoji, entities, and text")
        .child(
            text("I &hearts; coding! 💻 It's &gt; everything else ✨")
                .size(20.0)
                .color(Color::WHITE),
        )
        .child(
            text("Temperature: 72&deg;F &mdash; Perfect weather! ☀️")
                .size(20.0)
                .color(Color::WHITE),
        )
        .child(
            text("Price: &euro;99.99 (was &pound;120) 💰 Save &frac14;!")
                .size(20.0)
                .color(Color::WHITE),
        )
        .child(
            text("Math: &pi; &asymp; 3.14159&hellip; 🔢")
                .size(20.0)
                .color(Color::WHITE),
        )
        .child(
            text("&copy; 2025 Blinc &mdash; Built with &hearts; and ☕")
                .size(20.0)
                .color(Color::WHITE),
        )
        .child(
            text("Status: ✅ Complete &check; | ❌ Failed &cross; | ⏳ Pending")
                .size(20.0)
                .color(Color::WHITE),
        )
        .child(
            text("Numeric: &#65;&#66;&#67; = ABC | &#x1F600; = 😀")
                .size(20.0)
                .color(Color::WHITE),
        )
}

/// Helper to create a section card with title and subtitle
fn section_card(title: &str, subtitle: &str) -> Div {
    div()
        .w_full()
        .bg(Color::rgba(0.12, 0.12, 0.15, 1.0))
        .rounded(12.0)
        .p(16.0)
        .flex_col()
        .gap(12.0)
        .child(
            div()
                .flex_col()
                .gap(2.0)
                .child(h3(title).color(Color::rgba(0.4, 0.8, 1.0, 1.0)))
                .child(muted(subtitle)),
        )
}

/// Helper to create a row showing an entity and its rendered form
fn entity_row(entity: &str, description: &str) -> Div {
    div()
        .flex_row()
        .items_center()
        .gap(16.0)
        .child(
            // Show the entity code
            div()
                .min_w(120.0)
                .bg(Color::rgba(0.08, 0.08, 0.1, 1.0))
                .rounded(4.0)
                .px(8.0)
                .py(4.0)
                .child(
                    text(entity)
                        .size(14.0)
                        .monospace()
                        .color(Color::rgba(0.9, 0.6, 0.3, 1.0)),
                ),
        )
        .child(
            // Show the rendered result
            div()
                .min_w(60.0)
                .child(text(entity).size(20.0).color(Color::WHITE)),
        )
        .child(
            // Description
            text(description)
                .size(14.0)
                .color(Color::rgba(0.6, 0.6, 0.6, 1.0)),
        )
}
