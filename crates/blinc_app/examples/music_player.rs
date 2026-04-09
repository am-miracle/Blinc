//! Music Player Glass Card Demo
//!
//! Recreates an iOS-style "Now Playing" music player card with liquid glass
//! morphism effect (refracted bevel borders). All visual styling is driven
//! by CSS via `ctx.add_css()`.
//!
//! Run with: cargo run -p blinc_app --example music_player --features windowed

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;
use blinc_app::windowed::WindowedContext;
use blinc_icons::{icons, to_svg, to_svg_with_stroke};

const PAUSE: &str = r#"<g id="SVGRepo_bgCarrier" stroke-width="0"></g><g id="SVGRepo_tracerCarrier" stroke-linecap="round" stroke-linejoin="round"></g><g id="SVGRepo_iconCarrier"> <path d="M2 6C2 4.11438 2 3.17157 2.58579 2.58579C3.17157 2 4.11438 2 6 2C7.88562 2 8.82843 2 9.41421 2.58579C10 3.17157 10 4.11438 10 6V18C10 19.8856 10 20.8284 9.41421 21.4142C8.82843 22 7.88562 22 6 22C4.11438 22 3.17157 22 2.58579 21.4142C2 20.8284 2 19.8856 2 18V6Z"></path> <path d="M14 6C14 4.11438 14 3.17157 14.5858 2.58579C15.1716 2 16.1144 2 18 2C19.8856 2 20.8284 2 21.4142 2.58579C22 3.17157 22 4.11438 22 6V18C22 19.8856 22 20.8284 21.4142 21.4142C20.8284 22 19.8856 22 18 22C16.1144 22 15.1716 22 14.5858 21.4142C14 20.8284 14 19.8856 14 18V6Z"></path> </g>"#;
const NEXT: &str = r#"<g id="SVGRepo_bgCarrier" stroke-width="0"></g><g id="SVGRepo_tracerCarrier" stroke-linecap="round" stroke-linejoin="round"></g><g id="SVGRepo_iconCarrier"> <path d="M3.76172 7.21957V16.7896C3.76172 18.7496 5.89172 19.9796 7.59172 18.9996L11.7417 16.6096L15.8917 14.2096C17.5917 13.2296 17.5917 10.7796 15.8917 9.79957L11.7417 7.39957L7.59172 5.00957C5.89172 4.02957 3.76172 5.24957 3.76172 7.21957Z" ></path> <path d="M20.2383 18.9303C19.8283 18.9303 19.4883 18.5903 19.4883 18.1803V5.82031C19.4883 5.41031 19.8283 5.07031 20.2383 5.07031C20.6483 5.07031 20.9883 5.41031 20.9883 5.82031V18.1803C20.9883 18.5903 20.6583 18.9303 20.2383 18.9303Z" ></path> </g>"#;
const PREV: &str = r#"<g id="SVGRepo_bgCarrier" stroke-width="0"></g><g id="SVGRepo_tracerCarrier" stroke-linecap="round" stroke-linejoin="round"></g><g id="SVGRepo_iconCarrier"> <path d="M20.2409 7.21957V16.7896C20.2409 18.7496 18.1109 19.9796 16.4109 18.9996L12.2609 16.6096L8.11094 14.2096C6.41094 13.2296 6.41094 10.7796 8.11094 9.79957L12.2609 7.39957L16.4109 5.00957C18.1109 4.02957 20.2409 5.24957 20.2409 7.21957Z" ></path> <path d="M3.76172 18.9303C3.35172 18.9303 3.01172 18.5903 3.01172 18.1803V5.82031C3.01172 5.41031 3.35172 5.07031 3.76172 5.07031C4.17172 5.07031 4.51172 5.41031 4.51172 5.82031V18.1803C4.51172 18.5903 4.17172 18.9303 3.76172 18.9303Z" ></path> </g>"#;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let config = WindowConfig {
        title: "Music Player".to_string(),
        width: 600,
        height: 700,
        resizable: true,
        fullscreen: false,
        ..Default::default()
    };

    let mut css_loaded = false;

    WindowedApp::run(config, move |ctx| {
        if !css_loaded {
            ctx.add_css(
                r#"
                /* ========================================= */
                /* Background                                */
                /* ========================================= */
                #bg {
                    background: url("crates/blinc_app/examples/assets/Asake-Album-Review-1.webp");
                }

                /* ========================================= */
                /* Glass Card (liquid glass morphism)        */
                /* ========================================= */
                #card {
                    backdrop-filter: liquid-glass(blur(18px) saturate(180%) brightness(120%) border(4.0) tint(rgba(255, 255, 255, 1.0)));
                    border-radius: 24px;
                    box-shadow: 0 8px 80px rgba(0, 0, 0, 0.60);
                    padding: 24px;
                    width: 380px;
                    gap: 20px;
                }
                #progress {
                    width: 100%;
                    padding: 6px 8px;
                    border-radius: 12px;
                    backdrop-filter: blur(12px) saturate(180%) brightness(80%);
                    height: 26px;
                    overflow: clip;
                    transition: height 0.3s ease;
                }

                #progress:hover {
                    height: 52px;
                }
               
                #progress:hover #time-left, #progress:hover #time-right {
                    opacity: 1;
                }

                /* ========================================= */
                /* Album Header                              */
                /* ========================================= */
                #album-art {
                    width: 80px;
                    height: 80px;
                    border-radius: 16px;
                    backdrop-filter: blur(12px) saturate(180%) brightness(80%);
                    border-color: rgba(255, 255, 255, 0.5);
                    border-width: 1.5px;
                    border-style: solid;
                }

                #album-art img {
                    object-position: 60% 40%;
                }

                #title {
                    font-size: 20px;
                    font-weight: 700;
                    color: #ffffff;
                }

                #artist {
                    font-size: 16px;
                    color: #00000090;
                    font-weight: 600;
                }

                #badge {
                    width: 36px;
                    height: 36px;
                    border-radius: 18px;
                    border-width: 1.5px;
                    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.20);
                    border-color: rgba(255, 255, 255, 0.5);
                    backdrop-filter: blur(12px) saturate(180%) brightness(80%);
                    transition: transform 0.2s ease, backdrop-filter 0.2s ease, box-shadow 0.2s ease;
                }
                #badge:hover {
                    transform: scale(1.12);
                    backdrop-filter: blur(16px) saturate(200%) brightness(110%);
                    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.40);
                    border-color: rgba(255, 255, 255, 0.85);
                    pointer: cursor;
                }
                #badge:hover #badge-icon {
                    stroke: rgba(0, 0, 0, 0.7);
                }

                #artist-container {
                     border-width: 1.5px;
                    border-radius: 12px;
                    width: 80px;
                }

                #badge-icon {
                    stroke: rgba(255, 255, 255, 0.80);
                    stroke-width: 3.0;
                    transition: stroke 0.2s ease;
                }

                /* ========================================= */
                /* Progress Bar                              */
                /* ========================================= */
                #time-left, #time-right {
                    font-size: 13px;
                    font-weight: 600;
                    color: rgba(255, 255, 255, 1.0);
                    opacity: 0;
                    transition: opacity 0.3s ease;
                }

                #track {
                    height: 14px;
                    width: 100%;
                    border-radius: 8px;
                    background: #ffffff33;
                    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.30);
                     border-width: 1.5px;
                     border-color: rgba(255, 255, 255, 0.5);
                }

                #track-fill {
                    height: 14px;
                    border-radius: 8px;
                    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.30);
                    opacity: 0.8;
                    width: 60%;
                    animation: track-glow 2s ease-in-out infinite;
                }

                @keyframes track-glow {
                    0%   { background: linear-gradient(90deg, #d0d0d0, #e0e0e0, #ffffff); }
                    33%  { background: linear-gradient(90deg, #d0d0d0, #ffffff, #d0d0d0); }
                    66%  { background: linear-gradient(90deg, #ffffff, #e0e0e0, #d0d0d0); }
                    100% { background: linear-gradient(90deg, #d0d0d0, #e0e0e0, #ffffff); }
                }

                #thumb {
                    width: 12px;
                    height: 12px;
                    border-radius: 6px;
                    background: #ffffff;
                    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.3);
                }

                /* ========================================= */
                /* Playback Controls (Lucide stroke icons)   */
                /* ========================================= */
                svg {
                    stroke: #ffffff;
                    fill: none;
                    stroke-width: 2.5;
                }

                #pause, #next {
                    fill: #ffffff;
                    stroke: #ffffff;
                    stroke-width: 0.5;
                    transition: fill 0.2s ease, stroke 0.2s ease;
                }

                #prev {
                    fill: #ffffff;
                    stroke: #ffffff;
                    stroke-width: 0.5;
                    transition: fill 0.2s ease, stroke 0.2s ease;
                }

                .icon-wrapper {
                    width: 48px;
                    height: 48px;
                    padding: 4px;
                    border-radius: 24px;
                    border-width: 1.5px;
                    backdrop-filter: blur(12px) saturate(180%) brightness(80%);
                    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.30);
                    display: flex;
                    align-items: center;
                    justify-content: center;
                    border-color: rgba(255, 255, 255, 0.5);
                    transition: transform 0.2s ease, backdrop-filter 0.2s ease, box-shadow 0.2s ease;
                }
                .icon-wrapper:hover {
                    transform: scale(1.12);
                    backdrop-filter: blur(16px) saturate(200%) brightness(110%);
                    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.40);
                    border-color: rgba(255, 255, 255, 0.85);
                    pointer: cursor;
                }
                .icon-wrapper:hover #prev, .icon-wrapper:hover #pause, .icon-wrapper:hover #next {
                    fill: rgba(0, 0, 0, 0.7);
                    stroke: rgba(0, 0, 0, 0.7);
                }
            "#,
            );
            css_loaded = true;
        }

        build_ui(ctx)
    })
}

// ============================================================================
// UI STRUCTURE
// ============================================================================

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    div()
        .id("bg")
        .w(ctx.width)
        .h(ctx.height)
        .flex_col()
        .items_center()
        .justify_end()
        .pb(20.0)
        .child(player_card())
}

fn player_card() -> impl ElementBuilder {
    div()
        .id("card")
        .flex_col()
        .child(album_header())
        .child(progress_section())
        .child(controls_row())
}

// ============================================================================
// ALBUM HEADER: art + title/artist + badge
// ============================================================================

fn album_header() -> impl ElementBuilder {
    div()
        .flex_row()
        .gap_px(16.0)
        .items_center()
        .justify_between()
        // Album art placeholder
        .child(
            div()
                .p_px(6.0)
                .items_center()
                .justify_center()
                .overflow_clip()
                .id("album-art")
                .child(
                    image("crates/blinc_app/examples/assets/Asake-Album-Review-1.webp")
                        .fit(ObjectFit::Cover),
                ),
        )
        // Song info
        .child(
            div()
                .flex_col()
                .gap_px(8.0)
                .flex_grow()
                .child(text("Mr Money").id("title"))
                .child(
                    div()
                        .id("artist-container")
                        .w_fit()
                        .items_center()
                        .child(text("Asake").id("artist")),
                ),
        )
        // Green check badge
        .child(
            div()
                .id("badge")
                .flex()
                .items_center()
                .justify_center()
                .child(
                    svg(to_svg_with_stroke(icons::AIRPLAY, 20.0, 3.0))
                        .size(20.0, 20.0)
                        .id("badge-icon"),
                ),
        )
}

// ============================================================================
// PROGRESS: time labels + track bar
// ============================================================================

fn progress_section() -> impl ElementBuilder {
    div()
        .w_full()
        .flex_col()
        .gap_px(8.0)
        .id("progress")
        // Track bar (first so it's always visible above the clip)
        .child(
            div()
                .id("track")
                .flex_row()
                .items_center()
                .child(div().id("track-fill")),
        )
        // Time labels row (below track — clipped when collapsed, revealed on hover)
        .child(
            div()
                .flex_row()
                .items_center()
                .justify_between()
                .child(text("0:10").id("time-left"))
                .child(text("-2:32").id("time-right")),
        )
}

// ============================================================================
// PLAYBACK CONTROLS: shuffle, back, pause, forward, repeat
// ============================================================================

fn controls_row() -> impl ElementBuilder {
    div()
        .flex_row()
        .justify_between()
        .items_center()
        .child(
            svg(to_svg_with_stroke(icons::SHUFFLE, 24.0, 3.0))
                .size(28.0, 28.0)
                .id("shuffle-icon"),
        )
        .child(
            div()
                .class("icon-wrapper")
                .child(svg(to_svg(PREV, 24.0)).size(32.0, 32.0).id("prev")),
        )
        .child(
            div()
                .class("icon-wrapper")
                .child(svg(to_svg(PAUSE, 24.0)).size(32.0, 32.0).id("pause")),
        )
        .child(
            div()
                .class("icon-wrapper")
                .child(svg(to_svg(NEXT, 24.0)).size(32.0, 32.0).id("next")),
        )
        .child(
            svg(to_svg_with_stroke(icons::REPEAT, 24.0, 3.0))
                .size(28.0, 28.0)
                .id("repeat-icon"),
        )
}
