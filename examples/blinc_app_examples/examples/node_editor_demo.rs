//! Node-editor demo — pre-wired graph with three node types, typed
//! ports, a group, and live drag-to-connect.
//!
//! Run with:
//! ```
//! cargo run -p blinc_app_examples --example node_editor_demo --features node-editor
//! ```
//!
//! What it shows:
//!
//! - **Metadata-driven**: nodes render from declarative
//!   `NodeTemplate`s. The editor never hardcodes shape — adding a new
//!   template adds a new node type.
//! - **Generic over port kind**: hosts impl `PortKind` for their own
//!   port-type enum. Here, [`DemoPort`] models `Number` / `String` /
//!   `Boolean`; the editor delegates compatibility to
//!   `DemoPort::compatible_with`.
//! - **Theme-aware**: chrome (background, header, border, badge) pulls
//!   from `ThemeState`. Switch theme bundles to recolor; squircle
//!   profile, shadows, typography, and spacing tokens flow through.
//! - **Group with badge**: two nodes are wrapped in a group with a
//!   status badge in the header.
//! - **Drag-to-connect**: drag from an output port to an input port;
//!   the validator accepts compatible kinds.
//! - **Pan + zoom + selection**: inherited from `blinc_canvas_kit`.

use blinc_app::prelude::*;
use blinc_app::windowed::WindowedContext;
use blinc_canvas_kit::prelude::*;
use blinc_core::State;
use blinc_core::layer::{Color, Point};
use blinc_node_editor::prelude::*;
use blinc_node_editor::{
    BadgeKind, ConnectionId, EditorCommand, ForceConfig, Group, GroupId, History, LayeredConfig,
    LayoutOrientation, LayoutStrategy, StatusBadge,
};
use blinc_platform::AnimationFps;
use blinc_portal_ui::{Sense, ShadowMix};
use blinc_tabler_icons::outline;
use blinc_theme::{
    ThemeState, detect_system_color_scheme, themes::universal::HybridTheme, tokens::ColorToken,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

// Resolve a theme colour at build time, falling back to a sane
// default when ThemeState isn't initialised (e.g. unit-test builds).
fn token(t: ColorToken, fallback: Color) -> Color {
    ThemeState::try_get()
        .map(|s| s.color(t))
        .unwrap_or(fallback)
}

// ─── Host-side port type ───────────────────────────────────────────

/// Sentinel encoded into the diamond-side port of a lifted external
/// connection so re-expansion can route it back to the SPECIFIC
/// internal node the user wired up, instead of falling through to
/// the demo's "entry == last inserted" heuristic and accidentally
/// landing on a sibling sink. Format:
/// `__sub_route:<canonical_internal_node_id>:<original_port>`.
const SUB_ROUTE_PREFIX: &str = "__sub_route:";

/// The host's port kind. Three semantic types, each with its own
/// accent colour the editor reads via `PortKind::accent`. Real hosts
/// would model their domain types here — reflow's `PortType`, ML
/// tensor dtypes, audio frame formats, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum DemoPort {
    Number,
    String,
    Boolean,
}

impl PortKind for DemoPort {
    fn compatible_with(&self, other: &Self) -> bool {
        // Strict: types must match exactly. Real hosts might allow
        // Number → String coercion etc.
        self == other
    }

    fn label(&self) -> String {
        match self {
            DemoPort::Number => "Number".into(),
            DemoPort::String => "String".into(),
            DemoPort::Boolean => "Bool".into(),
        }
    }

    fn accent(&self) -> Color {
        match self {
            DemoPort::Number => Color::rgb(0.40, 0.75, 1.00),
            DemoPort::String => Color::rgb(1.00, 0.70, 0.40),
            DemoPort::Boolean => Color::rgb(0.55, 0.85, 0.55),
        }
    }
}

// ─── Templates ─────────────────────────────────────────────────────

/// Build a node icon from a Tabler outline path constant.
///
/// Builds the SVG markup inline rather than going through
/// `to_svg_colored` so we can lower the `stroke-width` from
/// Tabler's default 2.0. Tabler authors paths in a 24×24 viewBox;
/// at our 16×16 display size the viewBox scales by 16/24 ≈ 0.667,
/// so a 2.0 stroke renders at ~1.33 CSS px — falls between pixel
/// boundaries and AA has to smear the edge across two rows, making
/// strokes read as thick + soft. Stroke `1.5` resolves to a clean
/// 1.0 CSS px (2 physical px on retina) which the rasterizer can
/// align to a pixel grid.
fn tabler_icon(path_data: &str) -> NodeIcon {
    NodeIcon::from_svg_str(&tabler_svg_str(path_data)).expect("valid SVG")
}

/// Wrap a Tabler-outline path-fragment constant (which is just the
/// inner `<path .../>` markup) in a full `<svg>` document. Used by
/// anything outside the node header that needs a renderable SVG
/// string — e.g. `cn::context_menu().item_with_icon(label, svg,
/// ...)` expects a complete SVG, and the raw tabler constants only
/// hold path fragments. The raster pipeline rejects the fragment
/// (`"unknown token at 1:61"`) without the wrapper.
fn tabler_svg_str(path_data: &str) -> String {
    let colour_hex = token_hex(ColorToken::TextPrimary, "#e8e8e8");
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="{colour_hex}" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">{path_data}</svg>"#
    )
}

/// Resolve a theme colour to a `#rrggbb` hex string. Tabler's
/// `to_svg_colored` only accepts strings.
fn token_hex(t: ColorToken, fallback: &str) -> String {
    let Some(c) = ThemeState::try_get().map(|s| s.color(t)) else {
        return fallback.to_string();
    };
    let to_byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        to_byte(c.r),
        to_byte(c.g),
        to_byte(c.b)
    )
}

/// Shared signals the portal widgets read + write. Same `Signal<T>`
/// reaches both the `filter` node's content slider AND the `formatter`
/// node's running toggle / label, demonstrating cross-portal reactive
/// state: setting either from outside the canvas (e.g. a sidebar
/// `cn::input`) repaints the node body immediately.
fn signals() -> &'static PortalSignals {
    use std::sync::OnceLock;
    static S: OnceLock<PortalSignals> = OnceLock::new();
    S.get_or_init(|| PortalSignals {
        threshold: blinc_core::reactive::signal::<f32>(0.5),
        running: blinc_core::reactive::signal::<bool>(false),
        sink_format: Default::default(),
        sink_clears: Default::default(),
        sink_label: Default::default(),
        sink_fill: Default::default(),
        sink_script: Default::default(),
        formatter_decimals: Default::default(),
        source_samples: Default::default(),
        histogram_buckets: Default::default(),
        pie_weights: Default::default(),
        radar_axes: Default::default(),
    })
}

/// Per-node-id signal lookup. `OnceLock`-based static map keyed by
/// `NodeId` so each instance of a template gets its OWN signals
/// instead of sharing one global value across every instance of the
/// template. Each helper lazy-inits via the supplied default
/// closure on first access.
fn per_node<T: Send + 'static>(
    map: &Mutex<HashMap<NodeId, blinc_core::reactive::Signal<T>>>,
    node_id: &NodeId,
    init: impl FnOnce() -> T,
) -> blinc_core::reactive::Signal<T> {
    let mut m = map.lock().unwrap();
    if let Some(s) = m.get(node_id) {
        return *s;
    }
    let s = blinc_core::reactive::signal(init());
    m.insert(node_id.clone(), s);
    s
}

struct PortalSignals {
    threshold: blinc_core::reactive::Signal<f32>,
    running: blinc_core::reactive::Signal<bool>,
    /// Display format for the Sink node — one of `text` / `json` /
    /// `yaml`. Per-instance so two sink nodes don't clobber each
    /// other's selection.
    sink_format: Mutex<HashMap<NodeId, blinc_core::reactive::Signal<String>>>,
    /// Click counter for the Sink node's "Clear" button.
    sink_clears: Mutex<HashMap<NodeId, blinc_core::reactive::Signal<u32>>>,
    /// User-editable label on the Sink node.
    sink_label: Mutex<HashMap<NodeId, blinc_core::reactive::Signal<String>>>,
    /// Sink node's user-pickable fill colour (hex string).
    sink_fill: Mutex<HashMap<NodeId, blinc_core::reactive::Signal<String>>>,
    /// Sink node's Lua transform script.
    sink_script: Mutex<HashMap<NodeId, blinc_core::reactive::Signal<String>>>,
    /// Formatter node's `decimals` config (per instance so two
    /// formatters can have different precisions).
    formatter_decimals: Mutex<HashMap<NodeId, blinc_core::reactive::Signal<f32>>>,
    /// Per-instance 32-sample series painted by Source nodes via
    /// `ui.chart(...)`. Seeded with a deterministic per-id phase
    /// shift so duplicate sources render visually distinct
    /// sparklines instead of sharing one global series.
    source_samples: Mutex<HashMap<NodeId, blinc_core::reactive::Signal<Vec<f32>>>>,
    /// Histogram node's bucket weights — drives `ui.chart(...).bar()`.
    histogram_buckets: Mutex<HashMap<NodeId, blinc_core::reactive::Signal<Vec<f32>>>>,
    /// Distribution node's slice weights — drives `ui.pie_chart(...)`.
    pie_weights: Mutex<HashMap<NodeId, blinc_core::reactive::Signal<Vec<f32>>>>,
    /// Radar / spider chart axes — drives `ui.radar_chart(...)`.
    radar_axes: Mutex<HashMap<NodeId, blinc_core::reactive::Signal<Vec<f32>>>>,
}

impl PortalSignals {
    fn sink_label_for(&self, id: &NodeId) -> blinc_core::reactive::Signal<String> {
        per_node(&self.sink_label, id, || "Output".to_string())
    }
    fn sink_format_for(&self, id: &NodeId) -> blinc_core::reactive::Signal<String> {
        per_node(&self.sink_format, id, || "text".to_string())
    }
    fn sink_clears_for(&self, id: &NodeId) -> blinc_core::reactive::Signal<u32> {
        per_node(&self.sink_clears, id, || 0)
    }
    fn sink_fill_for(&self, id: &NodeId) -> blinc_core::reactive::Signal<String> {
        per_node(&self.sink_fill, id, || "#3b82f6".to_string())
    }
    fn sink_script_for(&self, id: &NodeId) -> blinc_core::reactive::Signal<String> {
        per_node(&self.sink_script, id, || {
            "-- Lua sink script\nfunction on_value(v)\n  return tostring(v)\nend".to_string()
        })
    }
    fn formatter_decimals_for(&self, id: &NodeId) -> blinc_core::reactive::Signal<f32> {
        per_node(&self.formatter_decimals, id, || 2.0)
    }
    fn source_samples_for(&self, id: &NodeId) -> blinc_core::reactive::Signal<Vec<f32>> {
        per_node(&self.source_samples, id, || {
            // Phase the sinusoid by a hash of the node id so
            // duplicate sources don't share a series. Same shape,
            // different start.
            let phase = (id.as_str().bytes().fold(0u32, |a, b| a.wrapping_add(b as u32))
                % 360) as f32
                * std::f32::consts::PI
                / 180.0;
            let mut v = Vec::with_capacity(32);
            for i in 0..32 {
                let t = i as f32 / 31.0;
                let main = (t * std::f32::consts::TAU * 1.5 + phase).sin() * 0.6;
                let bump = (t * std::f32::consts::TAU * 4.0 + phase).cos() * 0.15;
                v.push(0.5 + main * 0.4 + bump);
            }
            v
        })
    }
    fn histogram_buckets_for(&self, id: &NodeId) -> blinc_core::reactive::Signal<Vec<f32>> {
        per_node(&self.histogram_buckets, id, || {
            let phase = (id.as_str().bytes().fold(0u32, |a, b| a.wrapping_add(b as u32))
                % 360) as f32
                * std::f32::consts::PI
                / 180.0;
            (0..12)
                .map(|i| {
                    let t = i as f32 / 11.0;
                    (0.2 + 0.7 * (t * std::f32::consts::TAU + phase).sin().abs()).clamp(0.05, 1.0)
                })
                .collect()
        })
    }
    fn pie_weights_for(&self, id: &NodeId) -> blinc_core::reactive::Signal<Vec<f32>> {
        per_node(&self.pie_weights, id, || {
            // Deterministic-but-varied slice mix per node id.
            let mut seed = id.as_str().bytes().fold(1u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
            (0..5)
                .map(|_| {
                    seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
                    0.1 + ((seed >> 16) as f32 / 65535.0) * 0.9
                })
                .collect()
        })
    }
    fn radar_axes_for(&self, id: &NodeId) -> blinc_core::reactive::Signal<Vec<f32>> {
        per_node(&self.radar_axes, id, || {
            // 6-axis health vector — per-id deterministic seed.
            let mut seed = id
                .as_str()
                .bytes()
                .fold(7u32, |a, b| a.wrapping_mul(17).wrapping_add(b as u32));
            (0..6)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    0.2 + ((seed >> 16) as f32 / 65535.0) * 0.8
                })
                .collect()
        })
    }
}

/// Open the colour-wheel popover anchored under the trigger chip.
/// Wraps `blinc_portal_ui::color_wheel_panel` in popover chrome
/// (bg + border + radius + padding + shadow_lg) plus a hex input
/// for typed entry and a "Done" button to confirm. Dismisses on
/// click outside; the wheel still reads/writes the same hex
/// signal so the trigger chip swatch reflects drags in real time.
fn open_color_picker_popover(
    anchor: blinc_core::layer::Rect,
    hex_signal: blinc_core::reactive::Signal<String>,
) {
    use blinc_layout::overlay_state::overlay_stack;
    use blinc_layout::widgets::overlay::AnchorDirection;
    use blinc_layout::widgets::overlay_stack::OverlayBuilder;
    use blinc_layout::widgets::text_input::text_input_data_with_placeholder;
    use blinc_layout::{click_outside, div};

    // Reserve the next handle id so the content closure can stamp
    // the same string on the wrapping div (click_outside hit-tests
    // ancestor element ids). Mirrors `cn::popover`'s build_popover_overlay.
    let next_id = overlay_stack()
        .lock()
        .ok()
        .map(|s| s.peek_next_handle_id())
        .unwrap_or(0);
    let popover_id = format!("portal-color-picker-{}", next_id);
    let click_outside_key = format!("portal-color-picker:{}", next_id);
    let popover_id_for_content = popover_id.clone();
    let key_for_on_close = click_outside_key.clone();

    // Size hint feeds `position_wrapper`'s viewport clamp — when
    // anchoring below would clip the Done button off the bottom
    // edge, the overlay system flips above automatically. Padding
    // 8 px on each side + wheel 224 + 2 * gap(8 px) + input ~32 +
    // button ~32 ≈ 320 × 240 with a few px of slack.
    const EST_POPOVER_W: f32 = 240.0;
    const EST_POPOVER_H: f32 = 320.0;
    let place_x = anchor.x();
    let place_y = anchor.y() + anchor.height() + 4.0;
    let anchor_dir = AnchorDirection::Bottom;

    // Hex input — seeded with the current hex once. on_change writes
    // the user's typed string verbatim into `hex_signal`; canonical
    // form happens implicitly when the wheel commits (drag → HSV →
    // canonical hex string back into the signal). Keeping on_change
    // pass-through avoids the mid-typing clobber where parsing
    // "#3b8" would canonicalise to "#33bb88" and the bidirectional
    // sync would overwrite the user's three-digit form.
    let hex_data = text_input_data_with_placeholder("#rrggbb");
    {
        let mut d = hex_data.lock().unwrap();
        d.value = hex_signal.get();
        d.cursor = d.value.len();
    }
    let hex_signal_on_change = hex_signal.clone();
    let hex_data_on_change = hex_data.clone();

    // Wheel → input sync. A reactive `effect` listens on the hex
    // signal and pushes the value into the SharedTextInputData
    // whenever the wheel commits a drag. Guarded by string equality
    // so the user's own typing (input → signal) doesn't loop back
    // and clobber the cursor. `refresh_text_input` requests the
    // input element re-render the new value. The effect handle is
    // disposed in `on_close` so dropping the popover stops the
    // subscription (otherwise hex updates after close would keep
    // poking a SharedTextInputData no longer mounted in the tree).
    let hex_for_effect = hex_signal.clone();
    let data_for_effect = hex_data.clone();
    let sync_effect = blinc_core::reactive::effect(move |_g| {
        let new_hex = hex_for_effect.get();
        let mut d = data_for_effect.lock().unwrap();
        if d.value != new_hex {
            d.value = new_hex;
            d.cursor = d.value.len();
            drop(d);
            blinc_layout::widgets::text_input::refresh_text_input(&data_for_effect);
        }
    });

    let handle = OverlayBuilder::popover()
        .at(place_x, place_y)
        .size(EST_POPOVER_W, EST_POPOVER_H)
        .anchor_direction(anchor_dir)
        .on_close(move |_reason| {
            click_outside::unregister_click_outside(&key_for_on_close);
            // Tear down the wheel→input sync subscription so a
            // post-close hex update doesn't poke a dead input.
            if let Ok(mut g) = blinc_core::reactive::global_graph().lock() {
                g.dispose_effect(sync_effect);
            }
        })
        .content(move || {
            let bg = blinc_theme::ThemeState::get().color(ColorToken::SurfaceElevated);
            let border = blinc_theme::ThemeState::get().color(ColorToken::Border);
            let hex_for_input = hex_data_on_change.clone();
            let hex_signal_for_change = hex_signal_on_change.clone();
            div()
                .id(&popover_id_for_content)
                .flex_col()
                // `gap(N)` is N * 4 px — `.gap(2)` → 8 px between
                // wheel, hex input, and Done button.
                .gap(2.0)
                .bg(bg)
                .border(1.0, border)
                .rounded(8.0)
                .lock_corner_shape()
                .p_px(8.0)
                .shadow_lg()
                .child(blinc_portal_ui::color_wheel_panel(hex_signal.clone()))
                .child(
                    blinc_cn::input(&hex_for_input)
                        .placeholder("#rrggbb")
                        .on_change(move |new_val: &str| {
                            // Write the user's typed string verbatim.
                            // Canonicalisation lives in the wheel
                            // commit path; canonicalising on every
                            // keystroke would expand "#3b8" → "#33bb88"
                            // mid-typing and bidirectional-sync would
                            // overwrite the user's input.
                            hex_signal_for_change.set(new_val.to_string());
                        }),
                )
                .child(
                    div().w_full().flex_row().justify_end().child(
                        blinc_cn::button("Done").on_click(move |_| {
                            // Close the overlay via its stable handle
                            // id. The popover's `on_close` callback
                            // runs once on close and unregisters the
                            // click_outside entry, so we don't double-
                            // unregister here.
                            if let Ok(mut stack) = overlay_stack().lock() {
                                stack.close(blinc_layout::widgets::overlay_stack::OverlayHandle::from_raw(next_id));
                            }
                        }),
                    ),
                )
        })
        .show();

    debug_assert_eq!(
        handle.raw(),
        next_id,
        "peek_next_handle_id was stale — concurrent push?"
    );

    // Register click_outside AFTER show() so the popover content has
    // its id mounted on the next frame's tree walk. Hit-tests against
    // ancestor element ids; any mouse-down whose ancestor chain
    // doesn't include `popover_id` triggers the dismiss.
    click_outside::register_click_outside(&click_outside_key, &popover_id, move || {
        handle.close();
    });
}

/// Open the script-editor popover anchored under the trigger chip.
/// Hosts a `blinc_layout::code_editor` bound one-way to `script_signal`
/// via `on_change` — every keystroke writes the joined-lines `String`
/// back to the signal so the chip's preview repaints live. The popover
/// is the sole writer to `script_signal` while open, so no
/// reverse-sync `effect` is needed (one-way binding sidesteps the
/// missing public `set_value` on `CodeEditorData`).
fn open_script_editor_popover(
    anchor: blinc_core::layer::Rect,
    script_signal: blinc_core::reactive::Signal<String>,
    language: Option<&'static str>,
) {
    use blinc_layout::overlay_state::overlay_stack;
    use blinc_layout::syntax::{
        JsonHighlighter, LuaHighlighter, PlainHighlighter, RustHighlighter, SyntaxConfig,
    };
    use blinc_layout::widgets::code::{code_editor, code_editor_state};
    use blinc_layout::widgets::overlay::AnchorDirection;
    use blinc_layout::widgets::overlay_stack::OverlayBuilder;
    use blinc_layout::{click_outside, div};

    let next_id = overlay_stack()
        .lock()
        .ok()
        .map(|s| s.peek_next_handle_id())
        .unwrap_or(0);
    let popover_id = format!("portal-script-editor-{}", next_id);
    let click_outside_key = format!("portal-script-editor:{}", next_id);
    let popover_id_for_content = popover_id.clone();
    let key_for_on_close = click_outside_key.clone();

    // 4 px grid throughout: editor body 540 × 280 = 135 × 70 grid.
    // Header label row + footer row each take one `ctrl_height`
    // worth of space. Outer 8 px padding + two 8 px gaps + 32 px
    // footer + 280 editor ≈ 360 — round to 368 so the viewport
    // clamp's flip-above kicks in cleanly when the trigger sits
    // low in the canvas.
    const EST_POPOVER_W: f32 = 560.0;
    const EST_POPOVER_H: f32 = 368.0;

    // Seed the editor state from the current signal value. The
    // `on_change` closure is the only write-back path; the chip's
    // preview reads `script_signal.get()` each paint so it tracks
    // typed edits without any extra wiring.
    let state = code_editor_state(script_signal.get());

    // Language is resolved to a fresh `SyntaxConfig` inside the
    // content closure on every rebuild — `SyntaxConfig` is not
    // `Clone`, and the highlighter rule sets are interned via
    // `Arc` inside `RustHighlighter::new()` / `JsonHighlighter::new()`
    // so constructing per-rebuild doesn't recompile regex tables.
    // Lua isn't shipped so it falls through to plain (every
    // keyboard affordance still works regardless).
    let lang_label = language.unwrap_or("plain").to_string();
    let lang_for_closure = language.unwrap_or("plain");

    let script_signal_for_on_change = script_signal.clone();

    let handle = OverlayBuilder::popover()
        .at(anchor.x(), anchor.y() + anchor.height() + 4.0)
        .size(EST_POPOVER_W, EST_POPOVER_H)
        .anchor_direction(AnchorDirection::Bottom)
        .on_close(move |_reason| {
            click_outside::unregister_click_outside(&key_for_on_close);
        })
        .content(move || {
            let bg = blinc_theme::ThemeState::get().color(ColorToken::SurfaceElevated);
            let border = blinc_theme::ThemeState::get().color(ColorToken::Border);
            let text_muted = blinc_theme::ThemeState::get().color(ColorToken::TextSecondary);
            let state_for_editor = state.clone();
            let sig_on_change = script_signal_for_on_change.clone();
            let lang_for_header = lang_label.clone();
            div()
                .id(&popover_id_for_content)
                .flex_col()
                .gap(2.0)
                .bg(bg)
                .border(1.0, border)
                .rounded(8.0)
                .lock_corner_shape()
                // Explicit width so `.w_full()` on the code-editor
                // wrapper resolves against a real number. Without
                // this the popover content sized to its natural
                // children only and the editor collapsed to its
                // intrinsic minimum (gutter + a few glyphs). `EST -
                // 2 * p_px(8)` = 544 leaves the inner content area
                // pinned at the size the viewport hint advertises.
                .w(EST_POPOVER_W - 16.0)
                .p_px(8.0)
                .shadow_lg()
                // Header — `{ }` script-icon glyph at the top-left
                // (matches the trigger chip's leading icon so the
                // affordance reads consistently), language label
                // muted at the top-right.
                .child(
                    div()
                        .w_full()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(blinc_layout::text("{ }").monospace().color(text_muted))
                        .child(blinc_layout::text(lang_for_header).monospace().color(text_muted)),
                )
                // Editor body — wrapped in a flex_row + overflow_clip
                // + rounded container per `code_demo.rs`'s canonical
                // pattern. The wrapper owns the chrome; the editor
                // fills it via `.w_full()`.
                .child(
                    div().flex_row().w_full().h(280.0).overflow_clip().rounded(6.0).child(
                        code_editor(&state_for_editor)
                            .syntax(match lang_for_closure {
                                "rust" => SyntaxConfig::new(RustHighlighter::new()),
                                "json" => SyntaxConfig::new(JsonHighlighter::new()),
                                "lua" => SyntaxConfig::new(LuaHighlighter::new()),
                                _ => SyntaxConfig::new(PlainHighlighter::new()),
                            })
                            .line_numbers(true)
                            // Default gutter (48 px) is sized for 3-4
                            // digit line numbers; inline scripts rarely
                            // pass 50 lines so 32 px gives clean room
                            // for two digits without the dead-space
                            // margin on a short script.
                            .gutter_width(32.0)
                            .font_size(13.0)
                            .padding(8.0)
                            .w_full()
                            .h(280.0)
                            .on_change(move |new_src: &str| {
                                // String equality guard — code_editor
                                // fires on_change even for no-op
                                // events (selection-only) on some
                                // platforms.
                                if sig_on_change.get() != new_src {
                                    sig_on_change.set(new_src.to_string());
                                }
                            }),
                    ),
                )
                // Footer — Done button right-aligned.
                .child(
                    div().w_full().flex_row().justify_end().child(
                        blinc_cn::button("Done").on_click(move |_| {
                            if let Ok(mut stack) = overlay_stack().lock() {
                                stack.close(
                                    blinc_layout::widgets::overlay_stack::OverlayHandle::from_raw(
                                        next_id,
                                    ),
                                );
                            }
                        }),
                    ),
                )
        })
        .show();

    debug_assert_eq!(
        handle.raw(),
        next_id,
        "peek_next_handle_id was stale — concurrent push?"
    );

    click_outside::register_click_outside(&click_outside_key, &popover_id, move || {
        handle.close();
    });
}

#[derive(Clone, Copy)]
enum ChartKind {
    Area,
    Bar,
    Pie,
}

/// Open an expanded chart popover anchored under the inline
/// sparkline. The host overlay re-hosts the same signal-bound
/// chart at a larger size — the source signal is shared so the
/// inline chip and the popover update in lockstep. Same
/// overlay-escape pattern the colour picker and script editor
/// use: portal_ui paints a trigger affordance; the host opens
/// the overlay anchored to the trigger's rect.
fn open_chart_pip_popover(
    anchor: blinc_core::layer::Rect,
    kind: ChartKind,
    signal: blinc_core::reactive::Signal<Vec<f32>>,
    node_id: NodeId,
) {
    use blinc_layout::overlay_state::overlay_stack;
    use blinc_layout::widgets::overlay::AnchorDirection;
    use blinc_layout::widgets::overlay_stack::OverlayBuilder;
    use blinc_layout::{click_outside, div};

    let next_id = overlay_stack()
        .lock()
        .ok()
        .map(|s| s.peek_next_handle_id())
        .unwrap_or(0);
    let popover_id = format!("portal-chart-pip-{}", next_id);
    let click_outside_key = format!("portal-chart-pip:{}", next_id);
    let popover_id_for_content = popover_id.clone();
    let key_for_on_close = click_outside_key.clone();

    // Expanded view dimensions; passed to OverlayBuilder.size so
    // the viewport-clamp picks the right placement.
    const EST_W: f32 = 460.0;
    const EST_H: f32 = 280.0;
    let title_text = match kind {
        ChartKind::Area => format!("Samples — {}", node_id.as_str()),
        ChartKind::Bar => format!("Buckets — {}", node_id.as_str()),
        ChartKind::Pie => format!("Mix — {}", node_id.as_str()),
    };

    // The popover content closure runs each rebuild; capture
    // the signal + kind by clone so the chart paints inside.
    let signal_for_content = signal;
    let title_for_content = title_text.clone();

    let handle = OverlayBuilder::popover()
        .at(anchor.x(), anchor.y() + anchor.height() + 4.0)
        .size(EST_W, EST_H)
        .anchor_direction(AnchorDirection::Bottom)
        .on_close(move |_reason| {
            click_outside::unregister_click_outside(&key_for_on_close);
        })
        .content(move || {
            let bg = blinc_theme::ThemeState::get().color(ColorToken::SurfaceElevated);
            let border = blinc_theme::ThemeState::get().color(ColorToken::Border);
            let text_muted = blinc_theme::ThemeState::get().color(ColorToken::TextSecondary);
            let title = title_for_content.clone();
            // For Area / Bar / Pie we paint a portal_ui chart in
            // the popover via a `canvas_kit` portal frame — but
            // for the demo we keep it simple and embed the chart
            // through `blinc_layout::canvas` + a closure that
            // paints the series at the expanded size.
            let sig = signal_for_content;
            let _kind = kind;
            div()
                .id(&popover_id_for_content)
                .flex_col()
                .gap(2.0)
                .bg(bg)
                .border(1.0, border)
                .rounded(8.0)
                .lock_corner_shape()
                .w(EST_W - 16.0)
                .p_px(8.0)
                .shadow_lg()
                .child(blinc_layout::text(title).color(text_muted))
                // Inline label for now — the expanded chart paint
                // is the next refinement (the chart widget can't
                // be hosted inside a popover Div directly since
                // portal_ui requires a Portal::frame context).
                .child(
                    blinc_layout::text(format!("{} samples", sig.get().len()))
                        .color(blinc_theme::ThemeState::get().color(ColorToken::TextPrimary)),
                )
        })
        .show();

    debug_assert_eq!(
        handle.raw(),
        next_id,
        "peek_next_handle_id was stale — concurrent push?"
    );

    click_outside::register_click_outside(&click_outside_key, &popover_id, move || {
        handle.close();
    });
}

fn build_templates() -> Vec<NodeTemplate<DemoPort>> {
    let source = NodeTemplate::<DemoPort>::new("source", "Source")
        .with_category("data")
        .with_subtitle("Emit values")
        .with_icon(tabler_icon(outline::DATABASE))
        .with_output(
            PortDesc::new("out_num", "value", Direction::Output, DemoPort::Number)
                .with_description("Stream of sampled numeric readings"),
        )
        // Branch on `node_id` so multiple source instances render
        // distinct visualisations off the SAME template. Each
        // source carries its own `source_samples_for` /
        // `histogram_buckets_for` / `pie_weights_for` signals
        // (per-instance lookup), so the choice of widget here is
        // purely cosmetic: the data shapes don't conflict and
        // every source can pick whichever chart fits its role.
        // - `src/1`        → area sparkline (live-telemetry look)
        // - `src/threshold`→ bar histogram (bucket frequencies)
        // - any other id   → donut pie (mix breakdown)
        .with_content(112.0, |node_id, ui| {
            let sigs = signals();
            match node_id.as_str() {
                "src/1" => {
                    ui.label("samples:");
                    let samples = sigs.source_samples_for(node_id);
                    let resp = ui
                        .chart(&samples)
                        .area()
                        .show_latest(true)
                        .show_baseline(true)
                        .pip(true)
                        .height(60.0)
                        .show();
                    if resp.pip_clicked {
                        let anchor = ui.host().rect_to_screen(resp.rect);
                        open_chart_pip_popover(
                            anchor,
                            ChartKind::Area,
                            samples,
                            node_id.clone(),
                        );
                    }
                }
                "src/threshold" => {
                    ui.label("buckets:");
                    let buckets = sigs.histogram_buckets_for(node_id);
                    let resp = ui
                        .chart(&buckets)
                        .bar()
                        .y_range(0.0..1.0)
                        .bar_gap(2.0)
                        .pip(true)
                        .height(60.0)
                        .show();
                    if resp.pip_clicked {
                        let anchor = ui.host().rect_to_screen(resp.rect);
                        open_chart_pip_popover(
                            anchor,
                            ChartKind::Bar,
                            buckets,
                            node_id.clone(),
                        );
                    }
                }
                "src/radar" => {
                    // Radar / spider chart of the node's 6-axis
                    // "telemetry" vector. Wired into the filter
                    // via the dataflow so it's an active member
                    // of the pipeline, not a standalone.
                    let axes = sigs.radar_axes_for(node_id);
                    let resp = ui
                        .radar_chart(&axes)
                        .labels(vec![
                            "cpu".to_string(),
                            "mem".to_string(),
                            "io".to_string(),
                            "net".to_string(),
                            "err".to_string(),
                            "lat".to_string(),
                        ])
                        .y_range(0.0..1.0)
                        .diameter(112.0)
                        .pip(true)
                        .show();
                    if resp.pip_clicked {
                        let anchor = ui.host().rect_to_screen(resp.rect);
                        open_chart_pip_popover(
                            anchor,
                            ChartKind::Pie,
                            axes,
                            node_id.clone(),
                        );
                    }
                }
                _ => {
                    // Default — pie / mix breakdown (subgraph
                    // inner source, any future demo instances).
                    ui.label("mix:");
                    let weights = sigs.pie_weights_for(node_id);
                    let resp = ui
                        .pie_chart(&weights)
                        .donut()
                        .diameter(72.0)
                        .pip(true)
                        .show();
                    if resp.pip_clicked {
                        let anchor = ui.host().rect_to_screen(resp.rect);
                        open_chart_pip_popover(
                            anchor,
                            ChartKind::Pie,
                            weights,
                            node_id.clone(),
                        );
                    }
                }
            }
        });

    let filter = NodeTemplate::<DemoPort>::new("filter", "Filter")
        .with_category("transform")
        .with_subtitle("Threshold gate")
        .with_icon(tabler_icon(outline::FILTER))
        .with_input(
            PortDesc::new("in_num", "input", Direction::Input, DemoPort::Number)
                .with_description("Value to test against the threshold"),
        )
        .with_input(
            PortDesc::new(
                "in_threshold",
                "threshold",
                Direction::Input,
                DemoPort::Number,
            )
            .with_description("Numeric cutoff — values above pass through"),
        )
        .with_output(
            PortDesc::new("out_pass", "pass?", Direction::Output, DemoPort::Boolean)
                .with_description("true when input >= threshold"),
        )
        // Typed config schema. Hosts walk this via
        // `blinc_node_editor::inspector::fields()` to render an
        // inspector pane; `blinc_node_editor::inspector::apply_patch`
        // merges patch requests back into `NodeInstance::config`.
        .with_property(
            NumberProperty::new("threshold", "Threshold")
                .description("Values >= this pass through")
                .default(0.5)
                .range(0.0, 1.0)
                .step(0.01),
        )
        .with_property(
            BooleanProperty::new("strict", "Strict mode")
                .description("Use > instead of >=")
                .default(false),
        )
        .with_property(
            SelectProperty::new("on_block", "On block")
                .description("Behaviour when the gate rejects a value")
                .option("drop", "Drop silently")
                .option("warn", "Log a warning")
                .option("error", "Raise an error")
                .default("warn"),
        )
        // Reactive cascade rule: switching `on_block` to "error"
        // implies strict semantics — auto-flip the `strict` toggle.
        // Observe the cascade in tracing logs (see `handle_event`'s
        // NodeConfigChanged arm).
        .with_rule(
            PropertyRule::new()
                .trigger("on_block")
                .when(Predicate::Eq {
                    key: "on_block".into(),
                    value: JsonValue::String("error".into()),
                })
                .set("strict", JsonValue::Bool(true)),
        )
        // Portal content slot — immediate-mode UI under the header.
        // The slider edits the shared `threshold` signal; any frame
        // mutating it from anywhere repaints the canvas via the
        // portal-ui notifier hook. The Reset button uses the Ghost
        // variant — appropriate for a low-emphasis toolbar action
        // that sits next to the live slider.
        .with_content(110.0, |_node_id, ui| {
            let sigs = signals();
            ui.label(&format!("threshold = {:.2}", sigs.threshold.get()));
            ui.slider(&sigs.threshold, 0.0..1.0).show();
            // Outline variant — the chevron-less Ghost fill made it
            // hard to spot as a button; Outline keeps the same low-
            // chroma palette but adds a 1px border so the affordance
            // reads clearly on the node body.
            if ui.button("Reset").outline().shadow_sm().clicked() {
                sigs.threshold.set(0.5);
            }
        });

    let formatter = NodeTemplate::<DemoPort>::new("formatter", "Formatter")
        .with_category("transform")
        .with_subtitle("Number → String")
        .with_icon(tabler_icon(outline::TYPOGRAPHY))
        .with_input(
            PortDesc::new("in_num", "value", Direction::Input, DemoPort::Number)
                .with_description("Numeric value to format"),
        )
        .with_output(
            PortDesc::new("out_str", "text", Direction::Output, DemoPort::String)
                .with_description("Stringified value ready for display"),
        )
        .with_property(
            TextProperty::new("prefix", "Prefix")
                .description("Prepended to every emitted string")
                .placeholder("value=")
                .max_length(16),
        )
        .with_property(
            NumberProperty::new("decimals", "Decimals")
                .description("Digits after the decimal point")
                .integer()
                .default(2.0)
                .range(0.0, 8.0),
        )
        .with_property(
            CodeEditorProperty::new("template", "Template")
                .description("Optional handlebars-style template")
                .language("handlebars")
                .line_numbers(false),
        )
        // Portal content slot — proves cross-portal reactive state.
        // The label_signal here re-renders whenever the FILTER
        // node's slider edits `threshold`, even though that slider
        // lives in a different portal. Same signal, two readers.
        // The switch toggles a separate `running` signal; the
        // label below tracks the toggle.
        .with_content(140.0, |node_id, ui| {
            let sigs = signals();
            ui.label(&format!("Mirrors threshold: {:.2}", sigs.threshold.get()));
            ui.horizontal(|ui| {
                ui.label("running");
                ui.switch(&sigs.running).show();
            });
            ui.horizontal(|ui| {
                ui.label("decimals");
                let decimals = sigs.formatter_decimals_for(node_id);
                ui.numeric_input(&decimals)
                    .integer()
                    .range(0.0..8.0)
                    .show();
            });
            ui.label(if sigs.running.get() {
                "● live"
            } else {
                "○ paused"
            });

            // Free-form painting — a moving sparkline driven by
            // the portal's monotonic clock. Proves
            // `allocate_painter` + per-frame animation. Sets
            // `request_animation` so the canvas keeps painting.
            let color = ui.style().accent;
            let (mut p, _) = ui.allocate_painter((180.0, 28.0), Sense::Hover);
            let t = p.time();
            let rect = p.rect();
            let n = 32;
            let mut path = blinc_core::draw::Path::new();
            for i in 0..n {
                let x = rect.x() + (i as f32 / (n - 1) as f32) * rect.width();
                let phase = t * 1.5 + i as f32 * 0.3;
                let y = rect.y() + rect.height() * (0.5 - phase.sin() * 0.35);
                if i == 0 {
                    path = path.move_to(x, y);
                } else {
                    path = path.line_to(x, y);
                }
            }
            use blinc_core::layer::Brush;
            let stroke = blinc_core::draw::Stroke::new(1.5);
            p.stroke_path(&path, &stroke, Brush::Solid(color));
            let _ = p;
            ui.request_animation();
        });

    let sink = NodeTemplate::<DemoPort>::new("sink", "Sink")
        .with_category("data")
        .with_subtitle("Display")
        .with_icon(tabler_icon(outline::DEVICE_DESKTOP))
        .with_input(
            PortDesc::new("in_pass", "gate", Direction::Input, DemoPort::Boolean)
                .with_description("When false, the message is dropped"),
        )
        .with_input(
            PortDesc::new("in_str", "label", Direction::Input, DemoPort::String)
                .with_description("Text payload to render in the sink view"),
        )
        // Portal content slot — exercises the button + select_trigger
        // widgets end-to-end. Click on "Clear" bumps `sink_clears`
        // so the label below updates (proves Response.clicked round-
        // trips). Clicking the format trigger opens a host overlay
        // (`blinc_cn::context_menu`) anchored against the trigger's
        // canvas-space rect via `ui.host().rect_to_screen`.
        //
        // Real fit-content: `with_content(...)` declares only the
        // body height. The width is measured each frame from the
        // portal's `consumed_width()` (max of every widget's right
        // edge) and fed back through `apply_portal_width_override`
        // so the chrome grows to fit the actual chips. One-frame
        // narrow flash on first paint; stable thereafter.
        .with_content(160.0, |node_id, ui| {
            let sigs = signals();
            let label_sig = sigs.sink_label_for(node_id);
            let clears_sig = sigs.sink_clears_for(node_id);
            let format_sig = sigs.sink_format_for(node_id);
            let fill_sig = sigs.sink_fill_for(node_id);
            let script_sig = sigs.sink_script_for(node_id);

            // Inline editable label — clicks set focus, typing edits.
            ui.label("label:");
            ui.text_input(&label_sig).placeholder("Label…").show();

            let count = clears_sig.get();
            ui.label(&format!("cleared: {count}"));
            if ui.button("Clear").destructive().shadow_md().clicked() {
                clears_sig.set(count + 1);
            }

            const FORMAT_OPTIONS: &[(&str, &str)] = &[
                ("text", "Plain text"),
                ("json", "JSON"),
                ("yaml", "YAML"),
            ];
            let resp = ui.select_signal(&format_sig, FORMAT_OPTIONS).show();
            if resp.clicked {
                let anchor = ui.host().rect_to_screen(resp.rect);
                let fmt = format_sig;
                let mut menu = blinc_cn::context_menu()
                    .at(anchor.x(), anchor.y() + anchor.height() + 4.0);
                for (value, label) in FORMAT_OPTIONS {
                    let s = fmt;
                    let v = value.to_string();
                    menu = menu.item(*label, move || s.set(v.clone()));
                }
                let _ = menu.show();
            }

            let color_resp = ui.color_picker(&fill_sig).show();
            if color_resp.clicked {
                let anchor = ui.host().rect_to_screen(color_resp.rect);
                open_color_picker_popover(anchor, fill_sig);
            }

            // Inline script editor — composed from the standard
            // ButtonBuilder + `.icon("{ }")` rather than a bespoke
            // widget. Preview is the first non-blank line plus a
            // "+N more" suffix; the button label snapshot rebuilds
            // every frame so the chip tracks the bound signal.
            let src = script_sig.get();
            let preview_first = src
                .lines()
                .find(|l| !l.trim().is_empty())
                .or_else(|| src.lines().next())
                .unwrap_or("")
                .to_string();
            let more = src.lines().count().saturating_sub(1);
            let preview_label = if more > 0 {
                format!("{} +{} more", preview_first, more)
            } else {
                preview_first
            };
            let script_resp = ui.button(&preview_label).icon("{ }").outline().show();
            if script_resp.clicked {
                let anchor = ui.host().rect_to_screen(script_resp.rect);
                open_script_editor_popover(anchor, script_sig, Some("lua"));
            }
        });

    // Minimal template for subgraph-reference nodes. Zero ports —
    // matches Zeal's `SubgraphNode` (the diamond is purely a
    // navigation entry-point; cross-boundary data flow would land
    // in proxy nodes registered separately). `default_shape =
    // Diamond` is moot because the renderer FORCES Diamond on any
    // instance with `subgraph_ref` set; we set it anyway so the
    // palette preview also renders as a diamond.
    let subgraph = NodeTemplate::<DemoPort>::new("subgraph", "Subgraph")
        .with_category("navigation")
        .with_subtitle("Open subgraph")
        .with_icon(tabler_icon(outline::LINK))
        .with_shape(NodeShape::Diamond);

    // Noise generator template — three variants picked by node
    // id so duplicate instances paint different patterns from
    // the same template. Emits a Number port so the node can
    // feed downstream as a noise source.
    let noise = NodeTemplate::<DemoPort>::new("noise", "Noise")
        .with_category("generate")
        .with_subtitle("Procedural pattern")
        .with_icon(tabler_icon(outline::WAVE_SINE))
        .with_output(
            PortDesc::new("out_num", "value", Direction::Output, DemoPort::Number)
                .with_description("Sample of the procedural noise field"),
        )
        .with_content(96.0, |node_id, ui| {
            let (variant, label) = match node_id.as_str() {
                "noise/worley" => (blinc_portal_ui::NoiseVariant::Worley, "worley"),
                "noise/voronoi" => (blinc_portal_ui::NoiseVariant::Voronoi, "voronoi"),
                _ => (blinc_portal_ui::NoiseVariant::Perlin, "perlin"),
            };
            ui.label(label);
            // Per-id seed so duplicate instances of the same
            // variant paint different patterns.
            let seed = node_id
                .as_str()
                .bytes()
                .fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
            ui.noise()
                .variant(variant)
                .seed(seed)
                .scale(28.0)
                .octaves(3)
                .height(64.0)
                .pip(true)
                .show();
        });

    vec![source, filter, formatter, sink, subgraph, noise]
}

// ─── Initial graph ─────────────────────────────────────────────────

type Editor = NodeEditor<DemoPort, (), (), ()>;
type DemoHistory = Arc<Mutex<History<DemoPort, (), (), ()>>>;

fn initial_nodes() -> Vec<NodeInstance<()>> {
    vec![
        // Source nodes carry an inline sparkline now, so the
        // content slot grows the node height past the legacy
        // 80 px. Hand-spaced for the demo until the editor grows
        // collision-aware layout response (queued); a real host
        // would either bake the measured size into instance.size
        // post-first-frame or auto-resolve overlaps each frame.
        NodeInstance::new("src/1", "source", Point::new(80.0, 80.0))
            .with_badge(StatusBadge::success()),
        // `with_disabled(true)` demonstrates the soft-disable flag:
        // the renderer dims the body / icon / title via
        // `theme.node_disabled_alpha()` and downgrades every
        // incident edge (only `src/threshold → filter/in_threshold`
        // here) to the faded `Pending` style. Press `D` while a
        // node is selected to toggle the flag at runtime.
        NodeInstance::new("src/threshold", "source", Point::new(80.0, 320.0))
            .with_subtitle("Threshold const")
            .with_disabled(true),
        // Third source instance — renders the new radar chart
        // variant via the source template's per-id branch. Wired
        // into the filter via the dataflow so it's not floating
        // disconnected from the pipeline.
        NodeInstance::new("src/radar", "source", Point::new(80.0, 560.0))
            .with_subtitle("Telemetry vector")
            .with_badge(StatusBadge::info(6).with_tooltip("6-axis radar")),
        NodeInstance::new("filter/1", "filter", Point::new(360.0, 180.0))
            .with_size(200.0, 100.0)
            .with_badge(StatusBadge::running()),
        NodeInstance::new("fmt/1", "formatter", Point::new(360.0, 360.0)).with_size(200.0, 80.0),
        NodeInstance::new("sink/1", "sink", Point::new(660.0, 240.0))
            // No explicit `with_size` — the sink template uses
            // `with_content_size` to declare a 320 × 160 content
            // floor; the slot taffy pass widens the chrome so this
            // instance ends up the right size without a manual
            // override.
            .with_badge(StatusBadge::info(3).with_tooltip("3 pending writes")),
        // Subgraph-reference node. Renders as a diamond with the
        // accent fill/stroke regardless of the template's shape (the
        // editor forces Diamond on any instance with `subgraph_ref`
        // set). Double-click emits `EditorEvent::SubgraphRequested`
        // — handled in `handle_event` below by opening a `cn::dialog`
        // summarising the subgraph's contents. Placed off to the
        // right of the main pipeline so the diamond chrome reads
        // clearly against the dot background, away from the existing
        // group footprint.
        NodeInstance::new("sub/sample", "subgraph", Point::new(960.0, 220.0))
            .with_size(200.0, 140.0)
            .with_subtitle("demo-workflow/sample-sub")
            .with_subgraph_ref(SubgraphId::from("sample-sub")),
        // Noise generators — one instance per variant. Each
        // template branch reads `node_id` to pick the kernel.
        // Wired into the formatter so the noise output is part
        // of the pipeline, not a disconnected showcase.
        NodeInstance::new("noise/perlin", "noise", Point::new(360.0, 540.0))
            .with_subtitle("Perlin fbm"),
        NodeInstance::new("noise/worley", "noise", Point::new(360.0, 700.0))
            .with_subtitle("Worley cells"),
        NodeInstance::new("noise/voronoi", "noise", Point::new(660.0, 540.0))
            .with_subtitle("Voronoi cells"),
    ]
}

fn initial_connections() -> Vec<Connection<()>> {
    vec![
        Connection::new(
            PortAddress::new("src/1".into(), "out_num"),
            PortAddress::new("filter/1".into(), "in_num"),
        )
        .with_state(ConnectionState::Running),
        Connection::new(
            PortAddress::new("src/threshold".into(), "out_num"),
            PortAddress::new("filter/1".into(), "in_threshold"),
        ),
        // Radar source → formatter — wires the new radar-chart
        // instance into the pipeline so it's an active member of
        // the dataflow, not a floating standalone.
        Connection::new(
            PortAddress::new("src/radar".into(), "out_num"),
            PortAddress::new("fmt/1".into(), "in_num"),
        )
        .with_state(ConnectionState::Running),
        // Noise generators → formatter — three different
        // procedural sources feeding into the same pipeline.
        Connection::new(
            PortAddress::new("noise/perlin".into(), "out_num"),
            PortAddress::new("fmt/1".into(), "in_num"),
        ),
        Connection::new(
            PortAddress::new("noise/worley".into(), "out_num"),
            PortAddress::new("fmt/1".into(), "in_num"),
        ),
        Connection::new(
            PortAddress::new("noise/voronoi".into(), "out_num"),
            PortAddress::new("fmt/1".into(), "in_num"),
        ),
        Connection::new(
            PortAddress::new("src/1".into(), "out_num"),
            PortAddress::new("fmt/1".into(), "in_num"),
        ),
        Connection::new(
            PortAddress::new("filter/1".into(), "out_pass"),
            PortAddress::new("sink/1".into(), "in_pass"),
        )
        .with_state(ConnectionState::Success),
        Connection::new(
            PortAddress::new("fmt/1".into(), "out_str"),
            PortAddress::new("sink/1".into(), "in_str"),
        ),
        // Sample connection flowing INTO the subgraph diamond. The
        // diamond has no template ports — the editor falls back to
        // `closest_point_on_rect` (same routing as collapsed groups)
        // so the line terminates on the side of the diamond that
        // faces the source endpoint. The port-id string ("entry")
        // is a host-defined namespaced reference; proxy-node
        // resolution lands in a follow-up, so for now this
        // just demonstrates the visual flow-in.
        Connection::new(
            PortAddress::new("fmt/1".into(), "out_str"),
            PortAddress::new("sub/sample".into(), "entry"),
        ),
    ]
}

fn initial_groups() -> Vec<Group<()>> {
    vec![
        Group::<()>::new(GroupId::from("transforms"), "Transforms")
            .with_description("Filter + formatter")
            .with_description_placeholder("Enter a description")
            .add_member("filter/1")
            .add_member("fmt/1")
            .with_badge(StatusBadge {
                kind: BadgeKind::Running,
                count: Some(2),
                tooltip: Some("2 active operations".into()),
            }),
    ]
}

// ─── Editor wiring ─────────────────────────────────────────────────

/// Host-side graph state. The editor stays a pure view; we hold the
/// authoritative copy of nodes / connections / groups here and
/// react to [`EditorEvent`]s by patching this state, then
/// re-syncing via granular commands.
#[derive(Clone, Default)]
struct HostGraph {
    nodes: Arc<RwLock<Vec<NodeInstance<()>>>>,
    connections: Arc<RwLock<Vec<Connection<()>>>>,
    groups: Arc<RwLock<Vec<Group<()>>>>,
    /// Subgraphs the user has "opened" — diamond replaced in-place
    /// by a colour-matched group container holding cloned internal
    /// nodes / connections. Keyed by the wrapping group's id so the
    /// minimize gesture (the host treats the group's collapse-chrome
    /// click as a "go back to diamond" signal) can find + reverse
    /// the expansion.
    expanded: Arc<RwLock<std::collections::HashMap<GroupId, ExpansionState>>>,
}

/// What the host saves when a subgraph diamond is expanded into a
/// container, so the minimize gesture can fully reverse the
/// operation. The diamond is removed from the live graph + every
/// incident external connection is also lifted (saved here); on
/// minimize they're restored verbatim.
#[derive(Clone)]
struct ExpansionState {
    diamond: NodeInstance<()>,
    external_connections: Vec<Connection<()>>,
    inserted_nodes: Vec<NodeId>,
    /// Tracked for debug / future history-integration use even though
    /// `inserted_nodes`-driven `editor.remove_node` cleanup already
    /// drops the incident edges, so we don't iterate this list during
    /// `minimize_subgraph`.
    #[allow(dead_code)]
    inserted_connections: Vec<ConnectionId>,
    group_id: GroupId,
    /// Subgraph the diamond points at. Captured at expand-time so
    /// the writeback path doesn't have to re-derive it from the
    /// diamond's `subgraph_ref` (which is preserved on the diamond
    /// for restoration but reading the diamond and stripping the
    /// option adds noise to every call site).
    subgraph_id: blinc_node_editor::SubgraphId,
    /// Id prefix used to clone subgraph node ids into the host name
    /// space (`expanded:<diamond>:…`). Storing it avoids
    /// reconstructing the format string in the writeback /
    /// dirty-detection paths.
    id_prefix: String,
    /// Snapshot of the wrapper group's contents at the moment
    /// `expand_subgraph` finished mutating. Compared against the
    /// current host state at minimize-time to decide whether the
    /// user actually edited anything (adds, removes, repositions,
    /// connection rewires) and therefore whether the
    /// save-changes confirm dialog should fire.
    baseline: ExpansionBaseline,
    /// Parent groups the diamond was a member of (if any). On expand,
    /// the diamond's id is swapped out of these groups' member lists
    /// in favour of every inserted internal node so the parent
    /// group's auto-bounds grows to enclose the expanded container.
    /// On minimize, the reverse swap restores the diamond's
    /// membership exactly. `Vec<GroupId>` because a node can in
    /// principle belong to multiple groups (rare but allowed by the
    /// current model).
    parent_groups: Vec<GroupId>,
    /// Set true the first time the wrapper group experiences any
    /// membership-changing event during this expansion (drag-in,
    /// drag-out, delete-from-wrapper, etc.). Used by
    /// `is_expansion_dirty` so a "net zero" edit sequence — add a
    /// node then remove it within the same session — still surfaces
    /// the save-changes confirmation dialog. Without this, capture
    /// diff returns zero against the at-expand baseline and the
    /// dialog never appears, leaving the user wondering whether
    /// their edits were noticed at all.
    edits_pending: bool,
}

/// Snapshot of an expanded subgraph's contents as seen through the
/// host's data: prefix-stripped node ids + topologically-meaningful
/// connection endpoints. Used purely as a diff target — by stripping
/// the `expanded:<diamond>:` prefix, the same baseline shape is
/// directly comparable to what `writeback_expansion_to_subgraph`
/// would write into `editor.subgraphs[subgraph_id]`.
///
/// Positions are included so a user drag-repositioning a node
/// inside the wrapper container counts as an edit. Node `size` is
/// included for completeness (resizable nodes drag-edit it).
#[derive(Clone, Debug, PartialEq)]
struct ExpansionBaseline {
    /// Sorted by stripped id. `(stripped_id, position_xy, size_wh)`
    nodes: Vec<(String, [f32; 2], Option<[f32; 2]>)>,
    /// Sorted lexically. `(from_node_stripped, from_port,
    /// to_node_stripped, to_port)`. Only connections where BOTH
    /// endpoints are wrapper-group members count as "internal" and
    /// are subject to the diff (external connections live in the
    /// host's domain, not the subgraph's).
    connections: Vec<(String, String, String, String)>,
}

impl ExpansionBaseline {
    /// Walk the host's current state and synthesize the baseline
    /// shape from the wrapper group's live members.
    fn capture(host: &HostGraph, group_id: &GroupId, id_prefix: &str) -> Self {
        use std::collections::HashSet;
        let member_set: HashSet<NodeId> = host
            .groups
            .read()
            .unwrap()
            .iter()
            .find(|g| g.id == *group_id)
            .map(|g| g.members.iter().cloned().collect())
            .unwrap_or_default();

        let strip =
            |raw: &str| -> String { raw.strip_prefix(id_prefix).unwrap_or(raw).to_string() };

        let mut nodes: Vec<(String, [f32; 2], Option<[f32; 2]>)> = host
            .nodes
            .read()
            .unwrap()
            .iter()
            .filter(|n| member_set.contains(&n.id))
            .map(|n| {
                (
                    strip(n.id.as_str()),
                    [n.position.x, n.position.y],
                    n.size.map(|(w, h)| [w, h]),
                )
            })
            .collect();
        nodes.sort_by(|a, b| a.0.cmp(&b.0));

        let mut connections: Vec<(String, String, String, String)> = host
            .connections
            .read()
            .unwrap()
            .iter()
            .filter(|c| member_set.contains(&c.from.node) && member_set.contains(&c.to.node))
            .map(|c| {
                (
                    strip(c.from.node.as_str()),
                    c.from.port.as_str().to_string(),
                    strip(c.to.node.as_str()),
                    c.to.port.as_str().to_string(),
                )
            })
            .collect();
        connections.sort();

        Self { nodes, connections }
    }
}

/// Build (or retrieve, on a subsequent rebuild) the editor + host
/// mirror + history. All three are cached behind
/// `ctx.use_state_keyed` so they SURVIVE window-resize / paint
/// rebuilds — without this, the build_ui closure would re-run on
/// every resize, calling NodeEditor::new() fresh + re-instantiating
/// initial_nodes(), which wipes user interaction state (drag
/// positions, opened subgraphs, undo stack, etc.).
///
/// NodeEditor, HostGraph and DemoHistory are all Arc-backed (cheap
/// to clone), so `State::get()` returns clones that share underlying
/// storage with the cached originals. Mutations made through the
/// cloned references mutate the cached state directly.
fn build_editor(ctx: &mut WindowedContext) -> (Editor, HostGraph, DemoHistory) {
    // ── Persist host + history across rebuilds ──────────────────
    // Only HOST + HISTORY survive in `use_state_keyed`. The editor
    // is freshly constructed each rebuild and re-syncs from the
    // cached host via `set_graph` — this mirrors the working pattern
    // that already preserved drag positions across rebuilds (the
    // host mirror was the authoritative state; the editor was a
    // view). Adding `host.expanded` to the persisted HostGraph
    // fixes the subgraph-expansion-vanishes-on-resize issue without
    // touching the editor's signal-registration paths.
    //
    // Caching the editor itself broke rendering (the cached
    // canvas-kit click listeners + Arc'd state didn't replay through
    // the new build's signal subscribers).
    let host_state = ctx.use_state_keyed("node-editor-demo-host", || HostGraph {
        nodes: Arc::new(RwLock::new(initial_nodes())),
        connections: Arc::new(RwLock::new(initial_connections())),
        groups: Arc::new(RwLock::new(initial_groups())),
        expanded: Arc::new(RwLock::new(std::collections::HashMap::new())),
    });
    let history_state = ctx.use_state_keyed::<DemoHistory, _>("node-editor-demo-history", || {
        Arc::new(Mutex::new(History::with_default_cap()))
    });
    let host = host_state.try_get().unwrap_or_else(|| HostGraph {
        nodes: Arc::new(RwLock::new(initial_nodes())),
        connections: Arc::new(RwLock::new(initial_connections())),
        groups: Arc::new(RwLock::new(initial_groups())),
        expanded: Arc::new(RwLock::new(std::collections::HashMap::new())),
    });
    let history = history_state
        .try_get()
        .unwrap_or_else(|| Arc::new(Mutex::new(History::with_default_cap())));

    let editor: Editor = NodeEditor::new("node-editor-demo")
        .with_templates(build_templates())
        .with_background(
            // Denser, slightly chunkier dot grid: 28 px spacing (down
            // from the default 50) + 3 px dot size (up from the
            // default 2) reads as a fine workshop surface texture
            // without competing with the node chrome. Zoom-adaptive
            // still kicks in below 0.3× to drop every 5th dot for
            // legibility when fully zoomed out.
            CanvasBackground::dots(token(ColorToken::Border, Color::rgba(0.5, 0.5, 0.55, 0.65)))
                .with_spacing(28.0)
                .with_size(3.0)
                .with_zoom_adaptive(0.3, 5),
        );

    // Initial sync from the persisted host. On first build the host
    // carries `initial_nodes / initial_connections / initial_groups`
    // (the use_state_keyed init); on subsequent rebuilds it carries
    // whatever the user's interactions have mutated it to (drag
    // positions, expansion state, etc.).
    editor.set_graph(
        host.nodes.read().unwrap().clone(),
        host.connections.read().unwrap().clone(),
        host.groups.read().unwrap().clone(),
        Vec::new(),
    );

    // Register a sample subgraph the diamond node in `initial_nodes`
    // refers to. Populate it with a few placeholder nodes so the
    // SubgraphRequested handler has something to surface in its
    // confirmation dialog. Hosts wire whatever editing flow they
    // want on the dialog's "Open" button — for this demo we just
    // display a summary.
    let sample_id = editor.create_subgraph("sample-sub", "Sample Subgraph");
    let _ = editor.set_subgraph_namespace(&sample_id, "demo-workflow/sample-sub");
    editor.with_subgraph_graph_mut(&sample_id, |sub| {
        sub.nodes.push(
            NodeInstance::new("inner/1", "source", Point::new(40.0, 40.0))
                .with_size(160.0, 70.0)
                .with_subtitle("Inner source"),
        );
        sub.nodes.push(
            NodeInstance::new("inner/2", "filter", Point::new(300.0, 80.0))
                .with_size(180.0, 90.0)
                .with_subtitle("Inner filter"),
        );
        sub.nodes.push(
            NodeInstance::new("inner/3", "sink", Point::new(560.0, 80.0))
                .with_size(180.0, 80.0)
                .with_subtitle("Inner sink"),
        );
        // Internal flow: source feeds the filter's input; the filter's
        // pass-through boolean feeds the sink's gate. Demonstrates that
        // expand_subgraph rebuilds the internal data flow inside the
        // container (otherwise the inner nodes would appear in
        // isolation with no wires between them).
        sub.connections.push(
            Connection::new(
                PortAddress::new("inner/1".into(), "out_num"),
                PortAddress::new("inner/2".into(), "in_num"),
            )
            .with_state(ConnectionState::Running),
        );
        sub.connections.push(Connection::new(
            PortAddress::new("inner/2".into(), "out_pass"),
            PortAddress::new("inner/3".into(), "in_pass"),
        ));
    });

    // Validator — accept connections whose port kinds match. This is
    // the ONLY callback surface: validators answer mid-drag questions
    // the editor needs synchronously (preview-tint colour).
    let editor = editor.on_connect_request(|req| {
        if req.from_kind.compatible_with(req.to_kind) {
            ValidationOutcome::Accept
        } else {
            ValidationOutcome::Reject {
                reason: format!(
                    "kind mismatch: {} → {}",
                    req.from_kind.label(),
                    req.to_kind.label()
                ),
            }
        }
    });

    // Synchronous context-menu callback. cn::context_menu's overlay
    // mount has to land BEFORE windowed.rs's overlay-stack dirty
    // poll on the SAME frame — otherwise the menu's @keyframes
    // enter animation misses `start_all_css_animations` and renders
    // at its `from` sample (opacity 0) until a paint invalidation
    // forces a re-bake (which mouse motion happens to trigger). The
    // matching `EditorEvent::ContextMenuRequested` event still
    // fires; hosts observing via `events_signal()` keep working.
    let editor = {
        let host_for_cb = host.clone();
        let history_for_cb = history.clone();
        let editor_for_cb = editor.clone();
        editor.on_context_menu(move |target, anchor| {
            open_context_menu(
                &editor_for_cb,
                &host_for_cb,
                &history_for_cb,
                target,
                anchor,
            );
        })
    };

    (editor, host, history)
}

/// Open a floating mini-toolbar at `anchor_screen` with Group +
/// Delete icon buttons targeting `node_ids`. Matches cn::popover's
/// surface design (SurfaceElevated bg, 1px border, theme radius,
/// shadow_lg, theme padding). Click-outside auto-dismisses via the
/// click_outside registry; clicking an action runs it then closes
/// the overlay.
fn open_multi_select_toolbar(
    editor: &Editor,
    host: &HostGraph,
    history: &DemoHistory,
    node_ids: Vec<NodeId>,
    anchor_screen: Point,
) {
    // Suppress the Group action when any selected node is already a
    // member of an existing group — re-grouping already-grouped
    // nodes would create overlapping memberships the editor doesn't
    // currently model. Delete stays available regardless.
    let can_group = {
        let groups = host.groups.read().unwrap();
        !node_ids
            .iter()
            .any(|id| groups.iter().any(|g| g.members.iter().any(|m| m == id)))
    };
    use blinc_layout::click_outside;
    use blinc_layout::overlay_state::overlay_stack;
    use blinc_layout::widgets::overlay_stack::{OverlayBuilder, OverlayHandle};
    use std::sync::Mutex;

    let theme = ThemeState::get();
    let bg = theme.color(ColorToken::SurfaceElevated);
    let border = theme.color(ColorToken::Border);
    let radius = theme.radius(blinc_theme::tokens::RadiusToken::Lg);
    let padding = theme.spacing_value(blinc_theme::tokens::SpacingToken::Space2);

    // Reserve the overlay id so the click-outside registry can
    // bind to the content element's stable id BEFORE `show()`
    // returns. Mirrors cn::popover's pattern.
    let next_handle_id = overlay_stack()
        .lock()
        .ok()
        .map(|s| s.peek_next_handle_id())
        .unwrap_or(0);
    let toolbar_id = format!("ne-multi-toolbar-{next_handle_id}");
    let click_outside_key = format!("ne-multi-toolbar:{next_handle_id}");

    // Slot shared between the overlay's content closure and the
    // outer scope so action buttons can close the overlay via the
    // captured handle after `show()` returns.
    let handle_slot: Arc<Mutex<Option<OverlayHandle>>> = Arc::new(Mutex::new(None));

    let toolbar_id_for_content = toolbar_id.clone();
    let click_outside_key_for_close = click_outside_key.clone();

    let editor_for_group = editor.clone();
    let host_for_group = host.clone();
    let history_for_group = history.clone();
    let ids_for_group = node_ids.clone();
    let editor_for_delete = editor.clone();
    let host_for_delete = host.clone();
    let ids_for_delete = node_ids.clone();
    let editor_for_align = editor.clone();
    let ids_for_align = node_ids.clone();
    let slot_for_group = handle_slot.clone();
    let slot_for_delete = handle_slot.clone();
    // Distribute is only meaningful for 3+ nodes (two anchors + at
    // least one interior). Below that we keep the buttons rendered
    // but disabled so the layout stays stable between selections.
    let can_distribute = node_ids.len() >= 3;

    // Clamp the anchor so the whole toolbar stays inside the window.
    // The overlay system positions `AtPoint` overlays as absolute
    // `(left, top)` and doesn't auto-fit to the viewport — past the
    // window edge they clip off-screen. We don't know the exact
    // toolbar size until it lays out, so we estimate from the button
    // roster (icon-only ghost ≈ 32 px wide + 6 px gap, plus three
    // separators + outer padding). 480 × 60 covers the worst case
    // (with-Group + full align + full distribute + Delete) plus a
    // small margin. The 8 px window inset keeps the toolbar from
    // hugging the screen edge.
    let (window_w, window_h) = overlay_stack()
        .lock()
        .ok()
        .map(|s| s.viewport())
        .unwrap_or((f32::INFINITY, f32::INFINITY));
    let est_w = 480.0_f32;
    let est_h = 60.0_f32;
    let inset = 8.0_f32;
    let clamped_x = anchor_screen
        .x
        .clamp(inset, (window_w - est_w - inset).max(inset));
    let clamped_y = anchor_screen
        .y
        .clamp(inset, (window_h - est_h - inset).max(inset));

    let handle = OverlayBuilder::popover()
        .at(clamped_x, clamped_y)
        .on_close(move |_reason| {
            click_outside::unregister_click_outside(&click_outside_key_for_close);
        })
        .content(move || {
            let editor_g = editor_for_group.clone();
            let host_g = host_for_group.clone();
            let history_g = history_for_group.clone();
            let group_ids = ids_for_group.clone();
            let slot_g = slot_for_group.clone();
            let editor_d = editor_for_delete.clone();
            let host_d = host_for_delete.clone();
            let delete_ids = ids_for_delete.clone();
            let slot_d = slot_for_delete.clone();
            let editor_a = editor_for_align.clone();
            let ids_a = ids_for_align.clone();
            let error_color = theme.color(ColorToken::Error);

            // Builds a tooltip-wrapped icon-only ghost button bound
            // to either an align edge or a distribute axis.
            // Align + distribute buttons leave the popover open so
            // users can chain operations (canonical workflow:
            // align → distribute on the perpendicular axis); Group
            // + Delete dismiss as before.
            //
            // Uses `InstanceKey::explicit` keyed by the unique
            // label so each tooltip's stored overlay-handle state
            // survives popover re-renders. The default
            // `track_caller` key auto-indexes calls within a frame
            // (`mk_align(...)` invoked six times from the same
            // source line gets indices 0..5), and the indices can
            // drift across re-renders inside an overlay content
            // closure — drift orphans the stored handle and leaves
            // the tooltip unable to dismiss when hover-leaves.
            let mk_align = |edge: AlignEdge, icon: &'static str, label: &'static str| {
                let editor = editor_a.clone();
                let ids = ids_a.clone();
                let key = blinc_layout::key::InstanceKey::explicit(format!(
                    "ne-multi-toolbar-tooltip:{label}"
                ));
                let trigger = move || {
                    let editor = editor.clone();
                    let ids = ids.clone();
                    div().child(
                        blinc_cn::button("")
                            .variant(blinc_cn::ButtonVariant::Ghost)
                            .icon(icon)
                            .on_click(move |_| {
                                editor.align_nodes(&ids, edge);
                                tracing::info!("align {:?} on {} nodes", edge, ids.len());
                            }),
                    )
                };
                blinc_cn::TooltipBuilder::with_key(trigger, key).text(label)
            };
            let mk_distribute = |axis: DistributeAxis, icon: &'static str, label: &'static str| {
                let editor = editor_a.clone();
                let ids = ids_a.clone();
                let key = blinc_layout::key::InstanceKey::explicit(format!(
                    "ne-multi-toolbar-tooltip:{label}"
                ));
                let trigger = move || {
                    let editor = editor.clone();
                    let ids = ids.clone();
                    div().child(
                        blinc_cn::button("")
                            .variant(blinc_cn::ButtonVariant::Ghost)
                            .icon(icon)
                            .disabled(!can_distribute)
                            .on_click(move |_| {
                                editor.distribute_nodes(&ids, axis);
                                tracing::info!("distribute {:?} on {} nodes", axis, ids.len());
                            }),
                    )
                };
                blinc_cn::TooltipBuilder::with_key(trigger, key).text(label)
            };

            let mut row = div()
                .id(&toolbar_id_for_content)
                .flex_row()
                .items_center()
                .gap(6.0)
                .p_px(padding)
                .bg(bg)
                .border(1.0, border)
                .rounded(radius)
                .shadow_lg();
            if can_group {
                let group_key =
                    blinc_layout::key::InstanceKey::explicit("ne-multi-toolbar-tooltip:group");
                let group_trigger = move || {
                    let editor_g = editor_g.clone();
                    let host_g = host_g.clone();
                    let history_g = history_g.clone();
                    let group_ids = group_ids.clone();
                    let slot_g = slot_g.clone();
                    div().child(
                        blinc_cn::button("")
                            .variant(blinc_cn::ButtonVariant::Ghost)
                            .icon(blinc_cn::prelude::icons::GROUP)
                            .on_click(move |_| {
                                let new_id = GroupId::from(format!(
                                    "g-{}",
                                    web_time::SystemTime::now()
                                        .duration_since(web_time::UNIX_EPOCH)
                                        .map(|d| d.as_millis())
                                        .unwrap_or(0)
                                ));
                                let mut group = Group::<()>::new(new_id.clone(), "New Group")
                                    .with_description("Group created from multi-select")
                                    .with_description_placeholder("Enter a description");
                                for nid in &group_ids {
                                    group = group.add_member(nid.as_str());
                                }
                                host_g.groups.write().unwrap().push(group.clone());
                                editor_g.insert_group(group.clone());
                                history_g.lock().unwrap().push(
                                    EditorCommand::InsertGroup(group),
                                    EditorCommand::RemoveGroup(new_id.clone()),
                                    "Create Group",
                                );
                                editor_g.clear_selection();
                                if let Some(h) = slot_g.lock().unwrap().as_ref() {
                                    h.close();
                                }
                                tracing::info!(
                                    "grouped {} nodes into {}",
                                    group_ids.len(),
                                    new_id.as_str()
                                );
                            }),
                    )
                };
                row = row.child(
                    blinc_cn::TooltipBuilder::with_key(group_trigger, group_key)
                        .text("Group selection"),
                );
                row = row.child(blinc_cn::separator().vertical());
            }

            // Align group — six icon-only ghost buttons, matching
            // the standard design-tool layout (horizontal axis:
            // left / centre-x / right; vertical: top / middle / bottom).
            // Tabler's `LAYOUT_ALIGN_*` glyph family reads as align
            // affordances at every size.
            row = row
                .child(mk_align(
                    AlignEdge::Left,
                    outline::LAYOUT_ALIGN_LEFT,
                    "Align left",
                ))
                .child(mk_align(
                    AlignEdge::CenterX,
                    outline::LAYOUT_ALIGN_CENTER,
                    "Align centre (horizontal)",
                ))
                .child(mk_align(
                    AlignEdge::Right,
                    outline::LAYOUT_ALIGN_RIGHT,
                    "Align right",
                ))
                .child(mk_align(
                    AlignEdge::Top,
                    outline::LAYOUT_ALIGN_TOP,
                    "Align top",
                ))
                .child(mk_align(
                    AlignEdge::CenterY,
                    outline::LAYOUT_ALIGN_MIDDLE,
                    "Align middle (vertical)",
                ))
                .child(mk_align(
                    AlignEdge::Bottom,
                    outline::LAYOUT_ALIGN_BOTTOM,
                    "Align bottom",
                ))
                .child(blinc_cn::separator().vertical())
                .child(mk_distribute(
                    DistributeAxis::Horizontal,
                    outline::LAYOUT_DISTRIBUTE_HORIZONTAL,
                    "Distribute horizontally",
                ))
                .child(mk_distribute(
                    DistributeAxis::Vertical,
                    outline::LAYOUT_DISTRIBUTE_VERTICAL,
                    "Distribute vertically",
                ))
                .child(blinc_cn::separator().vertical());

            let delete_key =
                blinc_layout::key::InstanceKey::explicit("ne-multi-toolbar-tooltip:delete");
            let delete_trigger = move || {
                let host_d = host_d.clone();
                let editor_d = editor_d.clone();
                let delete_ids = delete_ids.clone();
                let slot_d = slot_d.clone();
                div().child(
                    blinc_cn::button("")
                        .variant(blinc_cn::ButtonVariant::Ghost)
                        .icon(blinc_cn::prelude::icons::TRASH)
                        .color(error_color)
                        .on_click(move |_| {
                            let id_set: std::collections::HashSet<_> =
                                delete_ids.iter().cloned().collect();
                            host_d
                                .nodes
                                .write()
                                .unwrap()
                                .retain(|n| !id_set.contains(&n.id));
                            host_d.connections.write().unwrap().retain(|c| {
                                !id_set.contains(&c.from.node) && !id_set.contains(&c.to.node)
                            });
                            for g in host_d.groups.write().unwrap().iter_mut() {
                                g.members.retain(|m| !id_set.contains(m));
                            }
                            for id in &delete_ids {
                                editor_d.remove_node(id);
                            }
                            editor_d.clear_selection();
                            if let Some(h) = slot_d.lock().unwrap().as_ref() {
                                h.close();
                            }
                            tracing::info!(
                                "deleted {} nodes via multi-select toolbar",
                                delete_ids.len()
                            );
                        }),
                )
            };
            row.child(
                blinc_cn::TooltipBuilder::with_key(delete_trigger, delete_key)
                    .text("Delete selection"),
            )
        })
        .show();

    // Stash the handle so action callbacks can close it.
    *handle_slot.lock().unwrap() = Some(handle);

    // Register click-outside dismissal — overlay closes the moment
    // the next mouse-down lands outside any element with id
    // == toolbar_id (the chrome rect). Mirrors cn::popover's
    // pattern; without this only Escape would dismiss.
    let handle_for_outside = handle;
    click_outside::register_click_outside(&click_outside_key, &toolbar_id, move || {
        handle_for_outside.close();
    });
}

/// React to one [`EditorEvent`] by patching `host` and pushing the
/// matching granular command back at the editor. Centralises the
/// host-as-driver flow described in the roadmap. `history` records
/// each user-driven mutation with its inverse so Cmd-Z / Cmd-Shift-Z
/// can replay them.
fn handle_event(
    editor: &Editor,
    host: &HostGraph,
    history: &DemoHistory,
    evt: EditorEvent<DemoPort>,
) {
    match evt {
        EditorEvent::ConnectionAccepted(c) => {
            let conn = Connection::new(c.from.clone(), c.to.clone());
            let conn_id = conn.id;
            host.connections.write().unwrap().push(conn.clone());
            editor.insert_connection(conn.clone());
            history.lock().unwrap().push(
                EditorCommand::InsertConnection(conn),
                EditorCommand::RemoveConnection(conn_id),
                "Add Connection",
            );
            tracing::info!(
                "connected {:?} -> {:?}",
                (c.from.node.as_str(), c.from.port.as_str()),
                (c.to.node.as_str(), c.to.port.as_str()),
            );
        }
        EditorEvent::NodeDragged { id, position } => {
            // Snapshot the host's pre-drag position BEFORE writing the
            // new one so the inverse command carries the right point.
            let prev_position = host
                .nodes
                .read()
                .unwrap()
                .iter()
                .find(|n| n.id == id)
                .map(|n| n.position);
            if let Some(n) = host.nodes.write().unwrap().iter_mut().find(|n| n.id == id) {
                n.position = position;
            }
            if let Some(prev) = prev_position {
                if prev != position {
                    history.lock().unwrap().push(
                        EditorCommand::UpdateNodePosition(id.clone(), position),
                        EditorCommand::UpdateNodePosition(id, prev),
                        "Move Node",
                    );
                }
            }
            // Editor's drag handler already updated its internal copy
            // mid-drag — no need to re-push here. Host state is now in
            // sync for the next save/snapshot.
        }
        EditorEvent::DeleteConnectionRequested(id) => {
            confirm_delete_connection(editor, host, history, id);
        }
        EditorEvent::DeleteNodesRequested(ids) => {
            confirm_delete_nodes(editor, host, history, ids);
        }
        EditorEvent::AddToGroupRequested(req) => {
            // Add the node to the target group's member list +
            // mirror the change into the editor via the granular
            // `set_group_members` command.
            let prev_members = host
                .groups
                .read()
                .unwrap()
                .iter()
                .find(|g| g.id == req.group)
                .map(|g| g.members.clone());
            let updated = {
                let mut groups = host.groups.write().unwrap();
                groups.iter_mut().find(|g| g.id == req.group).map(|g| {
                    if !g.members.contains(&req.node) {
                        g.members.push(req.node.clone());
                    }
                    g.members.clone()
                })
            };
            if let (Some(members), Some(prev)) = (updated, prev_members) {
                editor.set_group_members(&req.group, members.clone());
                history.lock().unwrap().push(
                    EditorCommand::SetGroupMembers(req.group.clone(), members),
                    EditorCommand::SetGroupMembers(req.group.clone(), prev),
                    "Add to Group",
                );
                tracing::info!(
                    "added {} to group {}",
                    req.node.as_str(),
                    req.group.as_str()
                );
            }
            // If this group is a wrapping subgraph, mark the
            // expansion as edited so the save-changes dialog
            // surfaces on minimize even if the user later removes
            // what they just added.
            mark_wrapper_edits_pending(host, &req.group);
        }
        EditorEvent::RemoveFromGroupRequested(req) => {
            let prev_members = host
                .groups
                .read()
                .unwrap()
                .iter()
                .find(|g| g.id == req.group)
                .map(|g| g.members.clone());
            let updated = {
                let mut groups = host.groups.write().unwrap();
                groups.iter_mut().find(|g| g.id == req.group).map(|g| {
                    g.members.retain(|m| m != &req.node);
                    g.members.clone()
                })
            };
            if let (Some(members), Some(prev)) = (updated, prev_members) {
                editor.set_group_members(&req.group, members.clone());
                history.lock().unwrap().push(
                    EditorCommand::SetGroupMembers(req.group.clone(), members),
                    EditorCommand::SetGroupMembers(req.group.clone(), prev),
                    "Remove from Group",
                );
                tracing::info!(
                    "removed {} from group {} ({:?})",
                    req.node.as_str(),
                    req.group.as_str(),
                    req.source,
                );
            }
            // Symmetric with AddToGroupRequested above — any
            // wrapper-membership change marks the expansion edited
            // so the save-changes dialog fires on minimize, even
            // when add + remove net to zero against the baseline.
            mark_wrapper_edits_pending(host, &req.group);
            // Wrapping-subgraph special case: when a member of an
            // expanded subgraph's wrapping group is dragged OUT, three
            // things have to happen to keep it alive past the next
            // minimize and prevent stale-routing leftovers:
            //   1. Drop it from `state.inserted_nodes` so minimize
            //      doesn't sweep it away with the other expansion
            //      clones.
            //   2. Drop any lifted external connections that targeted
            //      it via the `__sub_route:<canonical>:` encoding —
            //      otherwise they'd resurrect on minimize as wires
            //      from outside pointing at a now-gone target, which
            //      re-expansion would mis-route through the
            //      entry/exit heuristic to a different inner node.
            //   3. Strip the `expanded:<diamond>:` prefix off the
            //      node's id (and its incident connections) so the
            //      node lives in the host name space as a normal
            //      node, not as an expansion artifact that would
            //      collide with a fresh clone the next time the
            //      same subgraph is opened.
            // Step 3 is best-effort: if the un-prefixed id collides
            // with an existing host node, the rename is skipped and
            // the prefixed id sticks (logged so the user can rename
            // manually if needed).
            naturalize_wrapper_member_removal(editor, host, &req.group, &req.node);
        }
        EditorEvent::ToggleCollapseRequested(req) => {
            // Special case: if this group is a subgraph-expansion
            // container (the user opened a diamond into a colour-
            // coded group), interpret the collapse gesture as
            // "minimize back to the diamond" rather than the usual
            // collapse-to-chip. Reverses the expansion: drops the
            // group + every inserted internal node / connection and
            // restores the original diamond + any external
            // connections that were lifted off it.
            if host.expanded.read().unwrap().contains_key(&req.group) {
                confirm_minimize_subgraph(editor, host, req.group.clone());
                return;
            }
            // Apply directly — collapse/expand is a benign visual
            // toggle, no destructive intent. Host mirrors via the
            // editor's granular command so the renderer's slot
            // cache invalidates.
            let prev_collapsed = host
                .groups
                .read()
                .unwrap()
                .iter()
                .find(|g| g.id == req.group)
                .map(|g| g.is_collapsed);
            if let Some(g) = host
                .groups
                .write()
                .unwrap()
                .iter_mut()
                .find(|g| g.id == req.group)
            {
                g.is_collapsed = req.collapsed;
            }
            editor.set_group_collapsed(&req.group, req.collapsed);
            if let Some(prev) = prev_collapsed {
                if prev != req.collapsed {
                    history.lock().unwrap().push(
                        EditorCommand::SetGroupCollapsed(req.group.clone(), req.collapsed),
                        EditorCommand::SetGroupCollapsed(req.group.clone(), prev),
                        if req.collapsed {
                            "Collapse Group"
                        } else {
                            "Expand Group"
                        },
                    );
                }
            }
        }
        EditorEvent::DeleteGroupRequested(req) => {
            confirm_delete_group(editor, host, history, req.group);
        }
        EditorEvent::MultiSelectionSettled {
            node_ids,
            anchor_screen,
        } => {
            open_multi_select_toolbar(editor, host, history, node_ids, anchor_screen);
        }
        EditorEvent::SelectionCleared => {
            // Demo doesn't need to do anything — click-outside on
            // the overlay already dismisses it. Real hosts might
            // close inspector panels or update breadcrumbs here.
        }
        EditorEvent::EditGroupTitleRequested {
            group,
            current,
            anchor_screen,
        } => {
            open_inline_text_editor(
                editor,
                host,
                history,
                group,
                current,
                anchor_screen,
                EditorField::Title,
            );
        }
        EditorEvent::EditGroupDescriptionRequested {
            group,
            current,
            anchor_screen,
        } => {
            open_inline_text_editor(
                editor,
                host,
                history,
                group,
                current,
                anchor_screen,
                EditorField::Description,
            );
        }
        EditorEvent::EditGroupRequested {
            group,
            current_title,
            current_description,
            anchor_screen,
        } => {
            open_inline_group_form(
                editor,
                host,
                history,
                group,
                current_title,
                current_description,
                anchor_screen,
            );
        }
        EditorEvent::ConnectionRejected { from, to, reason } => {
            // Pop a cn::toast banner with the validator's reason
            // (when supplied) so the user sees WHY the connection
            // didn't take instead of just the red preview line that
            // disappears on release. Falls back to a generic message
            // when the host's `on_connect_request` returned a
            // reason-less rejection.
            let description = if reason.trim().is_empty() {
                format!(
                    "{}:{} → {}:{}: incompatible port types",
                    from.node.as_str(),
                    from.port.as_str(),
                    to.node.as_str(),
                    to.port.as_str(),
                )
            } else {
                format!(
                    "{}:{} → {}:{}: {}",
                    from.node.as_str(),
                    from.port.as_str(),
                    to.node.as_str(),
                    to.port.as_str(),
                    reason,
                )
            };
            blinc_cn::toast_error("Connection rejected")
                .description(description)
                .duration_ms(5000)
                .show();
        }
        EditorEvent::UndoRequested => {
            if let Some(label) = history.lock().unwrap().undo(editor) {
                // Mirror the editor's resulting state back into the
                // host. Cheapest path: re-read the editor's authoritative
                // graph and overwrite the host mirror — the editor just
                // applied the inverse, so its graph is the new source of
                // truth.
                resync_host_from_editor(editor, host);
                blinc_cn::toast("Undo")
                    .description(format!("Undid {}", label))
                    .duration_ms(1500)
                    .show();
                tracing::info!("undo: {}", label);
            }
        }
        EditorEvent::RedoRequested => {
            if let Some(label) = history.lock().unwrap().redo(editor) {
                resync_host_from_editor(editor, host);
                blinc_cn::toast("Redo")
                    .description(format!("Redid {}", label))
                    .duration_ms(1500)
                    .show();
                tracing::info!("redo: {}", label);
            }
        }
        EditorEvent::DuplicateNodesRequested(ids) => {
            // Clone every selected node with a fresh id ("{old}#copy"
            // or with a numeric suffix on collision) + a small visual
            // offset. Ports on each clone are driven by the template
            // via `NodeInstance::component`, which the clone preserves
            // — so the cloned node lights up with the same port set
            // as the original automatically. Connections between
            // duplicated nodes (both endpoints in the selection) are
            // also cloned, with endpoints remapped to the clone ids.
            // Edges touching only one selected endpoint are skipped
            // intentionally — duplicating those would create implicit
            // fan-out the user didn't ask for. History records the
            // composite of node + connection inserts as ONE undo.
            if ids.is_empty() {
                return;
            }
            let offset = Point::new(24.0, 24.0);
            let mut clones: Vec<NodeInstance<()>> = Vec::new();
            let mut id_map: std::collections::HashMap<NodeId, NodeId> =
                std::collections::HashMap::new();
            let existing_ids: std::collections::HashSet<NodeId> = host
                .nodes
                .read()
                .unwrap()
                .iter()
                .map(|n| n.id.clone())
                .collect();
            let mut used_ids = existing_ids;
            for id in &ids {
                let Some(src) = host
                    .nodes
                    .read()
                    .unwrap()
                    .iter()
                    .find(|n| n.id == *id)
                    .cloned()
                else {
                    continue;
                };
                let mut clone = src.clone();
                let mut candidate = format!("{}#copy", id.as_str());
                let mut n = 2;
                while used_ids.contains(&NodeId::from(candidate.as_str())) {
                    candidate = format!("{}#copy{}", id.as_str(), n);
                    n += 1;
                }
                let clone_id = NodeId::from(candidate.as_str());
                clone.id = clone_id.clone();
                clone.position =
                    Point::new(clone.position.x + offset.x, clone.position.y + offset.y);
                used_ids.insert(clone_id.clone());
                id_map.insert(id.clone(), clone_id);
                clones.push(clone);
            }
            if clones.is_empty() {
                return;
            }
            let clone_ids: Vec<NodeId> = clones.iter().map(|c| c.id.clone()).collect();

            // Internal edges: both endpoints in `id_map`. Clone each
            // with endpoints remapped + a fresh ConnectionId (the
            // Connection::new constructor mints one).
            let cloned_connections: Vec<Connection<()>> = host
                .connections
                .read()
                .unwrap()
                .iter()
                .filter(|c| id_map.contains_key(&c.from.node) && id_map.contains_key(&c.to.node))
                .map(|c| {
                    let from = PortAddress::new(
                        id_map.get(&c.from.node).unwrap().clone(),
                        c.from.port.as_str(),
                    );
                    let to = PortAddress::new(
                        id_map.get(&c.to.node).unwrap().clone(),
                        c.to.port.as_str(),
                    );
                    let mut nc = Connection::new(from, to);
                    nc.state = c.state;
                    nc
                })
                .collect();

            // Apply to host first, then sync into the editor.
            host.nodes.write().unwrap().extend(clones.iter().cloned());
            host.connections
                .write()
                .unwrap()
                .extend(cloned_connections.iter().cloned());
            for c in &clones {
                editor.insert_node(c.clone());
            }
            for c in &cloned_connections {
                editor.insert_connection(c.clone());
            }

            // Composite forward = every InsertNode + every
            // InsertConnection. Composite inverse = every
            // RemoveConnection (first, so edges drop before their
            // endpoints) + every RemoveNode.
            let mut forward: Vec<EditorCommand<DemoPort, (), (), ()>> = Vec::new();
            forward.extend(clones.iter().cloned().map(EditorCommand::InsertNode));
            forward.extend(
                cloned_connections
                    .iter()
                    .cloned()
                    .map(EditorCommand::InsertConnection),
            );
            let mut inverse: Vec<EditorCommand<DemoPort, (), (), ()>> = Vec::new();
            inverse.extend(
                cloned_connections
                    .iter()
                    .map(|c| EditorCommand::RemoveConnection(c.id)),
            );
            inverse.extend(clone_ids.iter().cloned().map(EditorCommand::RemoveNode));
            history.lock().unwrap().push(
                EditorCommand::Composite(forward),
                EditorCommand::Composite(inverse),
                if clones.len() == 1 {
                    "Duplicate Node"
                } else {
                    "Duplicate Nodes"
                },
            );
            // Reselect the clones so the next gesture acts on them.
            // RegionId::encode produces the canvas-kit wire format
            // canvas-kit's set_selection expects — same payload the
            // editor itself emits, so this stays in sync if the wire
            // format ever changes.
            let selection: std::collections::HashSet<String> = clone_ids
                .iter()
                .map(|id| RegionId::Node(id.clone()).encode())
                .collect();
            editor.canvas_kit().set_selection(selection);
            tracing::info!(
                "duplicated {} node(s) + {} internal edge(s)",
                clones.len(),
                cloned_connections.len(),
            );
        }
        EditorEvent::SelectAllRequested => {
            // Build a selection set covering every node, edge, and
            // group the host knows about. RegionId::encode is the
            // single source of truth for the canvas-kit wire format.
            let mut selection: std::collections::HashSet<String> = std::collections::HashSet::new();
            for n in host.nodes.read().unwrap().iter() {
                selection.insert(RegionId::Node(n.id.clone()).encode());
            }
            for c in host.connections.read().unwrap().iter() {
                selection.insert(RegionId::Edge(c.id).encode());
            }
            for g in host.groups.read().unwrap().iter() {
                selection.insert(RegionId::Group(g.id.clone()).encode());
            }
            editor.canvas_kit().set_selection(selection);
        }
        EditorEvent::ContextMenuRequested { .. } => {
            // No-op: the demo opens the context menu via the
            // synchronous `on_context_menu` callback in
            // `build_editor`. The event is still fired by the editor
            // so other observers (analytics, recorder, log) can
            // subscribe via `events_signal()`.
        }
        EditorEvent::SubgraphRequested {
            subgraph_id,
            source_node,
            source_anchor: _,
        } => {
            // Host policy: confirm the open via cn::dialog, then on
            // confirm expand the diamond into a colour-matched group
            // container that owns cloned copies of the subgraph's
            // interior nodes / connections. The wrapping group's
            // collapse-chrome doubles as the "minimize" affordance
            // (handled in the ToggleCollapseRequested arm — see the
            // expansion-state guard there).
            let snapshot = editor.subgraph(&subgraph_id);
            let (title, description) = match &snapshot {
                Some(sub) => (
                    format!("Open subgraph: {}", sub.name),
                    format!(
                        "{}\n{} nodes · {} connections · {} groups",
                        sub.namespace,
                        sub.nodes.len(),
                        sub.connections.len(),
                        sub.groups.len()
                    ),
                ),
                None => (
                    format!("Subgraph not found: {}", subgraph_id),
                    "The referenced subgraph is no longer registered with the editor.".to_string(),
                ),
            };
            tracing::info!(
                target: "node_editor_demo::subgraph",
                subgraph_id = %subgraph_id.as_str(),
                "SubgraphRequested — host opening confirmation dialog"
            );
            let editor_for_open = editor.clone();
            let host_for_open = host.clone();
            let snapshot_for_open = snapshot;
            let source_node_for_open = source_node.clone();
            blinc_cn::dialog()
                .title(title)
                .description(description)
                .confirm_text("Open")
                .cancel_text("Close")
                .on_confirm(move || {
                    if let Some(sub) = snapshot_for_open.clone() {
                        expand_subgraph(
                            &editor_for_open,
                            &host_for_open,
                            source_node_for_open.clone(),
                            sub,
                        );
                    }
                })
                .show();
        }
        EditorEvent::LayoutApplied(updates) => {
            // Auto-layout produced new positions. Two jobs:
            //   1. Push an `UpdateNodePosition` history entry per
            //      node so `Cmd-Z` reverts the layout in one shot.
            //   2. Animate from the CURRENT positions to the new
            //      ones over ~500 ms so the canvas slides rather
            //      than jump-cutting — same ease-out-cubic curve
            //      the viewport tween uses.
            //
            // Step 1 fires BEFORE the animation so undo captures the
            // pre-layout positions; the redo path replays the targets.
            // History push is a single composite entry; one Cmd-Z
            // restores all positions atomically.
            let pre_positions: Vec<(NodeId, Point)> = {
                let nodes = host.nodes.read().unwrap();
                updates
                    .iter()
                    .filter_map(|(id, _)| {
                        nodes
                            .iter()
                            .find(|n| n.id == *id)
                            .map(|n| (id.clone(), n.position))
                    })
                    .collect()
            };
            if !pre_positions.is_empty() {
                let undo_cmds: Vec<EditorCommand<DemoPort, (), (), ()>> = pre_positions
                    .iter()
                    .map(|(id, p)| EditorCommand::UpdateNodePosition(id.clone(), *p))
                    .collect();
                let redo_cmds: Vec<EditorCommand<DemoPort, (), (), ()>> = updates
                    .iter()
                    .map(|(id, p)| EditorCommand::UpdateNodePosition(id.clone(), *p))
                    .collect();
                history.lock().unwrap().push(
                    EditorCommand::Composite(redo_cmds),
                    EditorCommand::Composite(undo_cmds),
                    "Auto-layout",
                );
            }
            // Kick off the animated transition. The helper schedules
            // a per-frame tick callback that lerps each node from
            // its current position to the target with ease-out-cubic
            // over LAYOUT_TWEEN_MS, then self-unregisters.
            animate_layout_transition(editor.clone(), host.clone(), updates);
        }
        EditorEvent::NodeConfigChanged {
            node,
            key,
            previous,
            value,
            from_rule,
        } => {
            // Real hosts propagate the new value into their runtime
            // model (reflow Graph.set_node_config etc.). Demo just
            // logs the trail so cascades are observable in the
            // tracing output.
            tracing::info!(
                target: "node_editor_demo::config",
                node = %node.as_str(),
                key = %key,
                ?previous,
                ?value,
                from_rule,
                "node config changed"
            );
        }
        EditorEvent::CreateGroupRequested(_)
        | EditorEvent::EdgeClicked { .. }
        | EditorEvent::NodeClicked { .. } => {
            // Unhandled in this demo; real hosts dispatch to their
            // command palette / inspector / layout code.
        }
    }
}

/// Replace the subgraph-ref diamond `diamond_id` with a colour-
/// matched group container that holds cloned copies of `sub`'s
/// internal nodes / connections / groups. External connections that
/// terminated at the diamond are LIFTED OFF — saved into the
/// expansion state so the matching `minimize_subgraph` can restore
/// them when the user collapses the container.
///
/// Cloned internal entities get id-prefixed (`expanded:<diamond>:…`)
/// so the same subgraph can be opened in multiple places without id
/// collisions across diamond instances.
fn expand_subgraph(
    editor: &Editor,
    host: &HostGraph,
    diamond_id: NodeId,
    sub: blinc_node_editor::Subgraph<DemoPort, (), (), ()>,
) {
    // Skip if this diamond is already expanded (defensive — double-
    // click + dialog flow could in theory fire twice).
    let already_expanded = host
        .expanded
        .read()
        .unwrap()
        .values()
        .any(|st| st.diamond.id == diamond_id);
    if already_expanded {
        tracing::warn!(
            target: "node_editor_demo::subgraph",
            diamond = %diamond_id.as_str(),
            "expand_subgraph called for already-expanded diamond — skipping"
        );
        return;
    }

    // Snapshot the diamond + external connections before mutation.
    let diamond = match host
        .nodes
        .read()
        .unwrap()
        .iter()
        .find(|n| n.id == diamond_id)
        .cloned()
    {
        Some(d) => d,
        None => return,
    };
    let external_connections: Vec<Connection<()>> = host
        .connections
        .read()
        .unwrap()
        .iter()
        .filter(|c| c.from.node == diamond_id || c.to.node == diamond_id)
        .cloned()
        .collect();

    // Map old internal ids → new prefixed ids so two expansions of
    // the same subgraph don't collide. Connection endpoints get
    // rewritten through the same map.
    let prefix = format!("expanded:{}:", diamond_id.as_str());
    let id_map: std::collections::HashMap<NodeId, NodeId> = sub
        .nodes
        .iter()
        .map(|n| {
            (
                n.id.clone(),
                NodeId::from(format!("{prefix}{}", n.id.as_str())),
            )
        })
        .collect();

    // Layout: drop internal nodes into a tidy row to the right of
    // the diamond's left edge so the user sees them appear "near"
    // where the diamond was. A real host would re-layout via the
    // editor's layout strategies; this is the smallest visible
    // implementation.
    let mut new_nodes: Vec<NodeInstance<()>> = Vec::with_capacity(sub.nodes.len());
    let mut x_cursor = diamond.position.x;
    let row_y = diamond.position.y;
    let h_gap = 240.0;
    for n in &sub.nodes {
        let new_id = id_map.get(&n.id).unwrap().clone();
        let mut clone = n.clone();
        clone.id = new_id;
        clone.position = Point::new(x_cursor, row_y);
        x_cursor += h_gap;
        new_nodes.push(clone);
    }

    // Re-wire internal connections with the prefixed ids. Fresh
    // ConnectionId so they don't clash with any pre-existing
    // connections in the host.
    let mut new_connections: Vec<Connection<()>> = Vec::with_capacity(sub.connections.len());
    for c in &sub.connections {
        let from_node = id_map
            .get(&c.from.node)
            .cloned()
            .unwrap_or_else(|| c.from.node.clone());
        let to_node = id_map
            .get(&c.to.node)
            .cloned()
            .unwrap_or_else(|| c.to.node.clone());
        let mut nc = Connection::new(
            PortAddress::new(from_node, c.from.port.as_str()),
            PortAddress::new(to_node, c.to.port.as_str()),
        );
        nc.state = c.state;
        new_connections.push(nc);
    }

    // Wrap in a Group whose ACCENT (border + header chrome only,
    // NOT body fill — see Group::accent) matches the diamond's
    // warning accent so the container reads as "this is the open
    // subgraph" without flooding the interior with colour. The
    // user's collapse-chrome click on this group fires
    // `ToggleCollapseRequested` — the handler upstream intercepts
    // it as a "minimize back to diamond" signal because the group's
    // id is in `host.expanded`.
    let group_id = GroupId::from(format!("{prefix}group"));
    let warning_accent =
        blinc_theme::ThemeState::get().color(blinc_theme::tokens::ColorToken::Warning);
    let group = Group::<()>::new(group_id.clone(), sub.name.clone())
        .with_description(sub.namespace.clone())
        .with_accent(warning_accent);
    // Members are the inserted nodes — auto-bounds will pull the
    // group rect tight around them.
    let group = new_nodes
        .iter()
        .fold(group, |g, n| g.add_member(n.id.clone()));

    // External connections that previously terminated at the diamond
    // get re-routed to the LAST inserted internal node (the demo's
    // designated "entry" — sinks tend to be the natural inbound
    // boundary; a real host would honour a declared graph-input /
    // proxy-input mapping). External connections that ORIGINATED
    // from the diamond re-route to come FROM the FIRST inserted
    // internal node. Originals are saved in ExpansionState so
    // minimize can restore them verbatim.
    let entry_id = new_nodes.last().map(|n| n.id.clone());
    let exit_id = new_nodes.first().map(|n| n.id.clone());
    let mut rerouted_externals: Vec<Connection<()>> = Vec::new();
    // Try to resolve a `__sub_route:<canonical_node>:<port>`-encoded
    // diamond-side port to the corresponding prefixed internal node
    // + port. Returns the prefixed `NodeId` + port name on a clean
    // parse + a matching new_nodes entry; `None` otherwise so the
    // caller falls through to the legacy entry/exit heuristic.
    //
    // The encoding is written into the diamond-side port name by
    // `promote_added_members_to_subgraph` whenever the user moves a
    // host node INTO a subgraph during expansion. It carries which
    // SPECIFIC internal node the lifted external connection should
    // re-attach to on re-expand, so two sinks with different
    // identities aren't confused for one another.
    let resolve_sub_route = |port: &str| -> Option<(NodeId, String)> {
        let rest = port.strip_prefix(SUB_ROUTE_PREFIX)?;
        let (canonical, original_port) = rest.split_once(':')?;
        let target_id = NodeId::from(format!("{prefix}{canonical}"));
        if new_nodes.iter().any(|n| n.id == target_id) {
            Some((target_id, original_port.to_string()))
        } else {
            None
        }
    };
    for ext in &external_connections {
        // Connection terminates at the diamond — re-route the to.node.
        if ext.to.node == diamond_id {
            // Prefer the encoded routing (user-added node from a
            // previous save) — it points to the specific internal
            // node the user wired up. Falls back to entry-heuristic
            // for original-subgraph externals captured at first
            // expand-time, which have plain port names.
            if let Some((target, port)) = resolve_sub_route(ext.to.port.as_str()) {
                let mut rerouted = Connection::new(
                    PortAddress::new(ext.from.node.clone(), ext.from.port.as_str()),
                    PortAddress::new(target, port.as_str()),
                );
                rerouted.state = ext.state;
                rerouted_externals.push(rerouted);
            } else if let Some(ref entry) = entry_id {
                // Demo-pragmatic port mapping: pick a real input port
                // on the entry node. The sink template's `in_str` is
                // a String input matching the demo's external source
                // (fmt/1.out_str). If the entry node is something
                // else, pick its first declared input via the host's
                // template knowledge — for the demo, hard-code by
                // name since we know the subgraph's interior.
                let port = match entry.as_str() {
                    s if s.ends_with("inner/3") => "in_str",
                    _ => "in_str",
                };
                let mut rerouted = Connection::new(
                    PortAddress::new(ext.from.node.clone(), ext.from.port.as_str()),
                    PortAddress::new(entry.clone(), port),
                );
                rerouted.state = ext.state;
                rerouted_externals.push(rerouted);
            }
        } else if ext.from.node == diamond_id {
            if let Some((target, port)) = resolve_sub_route(ext.from.port.as_str()) {
                let mut rerouted = Connection::new(
                    PortAddress::new(target, port.as_str()),
                    PortAddress::new(ext.to.node.clone(), ext.to.port.as_str()),
                );
                rerouted.state = ext.state;
                rerouted_externals.push(rerouted);
            } else if let Some(ref exit) = exit_id {
                let port = "out_num";
                let mut rerouted = Connection::new(
                    PortAddress::new(exit.clone(), port),
                    PortAddress::new(ext.to.node.clone(), ext.to.port.as_str()),
                );
                rerouted.state = ext.state;
                rerouted_externals.push(rerouted);
            }
        }
    }

    // Find any parent group(s) the diamond is a member of so we can
    // swap its id out for the inserted internal node ids. Without
    // this, the parent group's auto-bounds would still reference the
    // (now-removed) diamond and the expanded container would sit
    // outside the parent group's footprint visually.
    let parent_groups: Vec<GroupId> = host
        .groups
        .read()
        .unwrap()
        .iter()
        .filter(|g| g.members.contains(&diamond_id))
        .map(|g| g.id.clone())
        .collect();

    // ── Mutation: remove diamond + external connections, insert
    // new nodes / connections / group. Host first, then editor sync.
    {
        let mut nodes = host.nodes.write().unwrap();
        nodes.retain(|n| n.id != diamond_id);
        nodes.extend(new_nodes.iter().cloned());
    }
    {
        let mut conns = host.connections.write().unwrap();
        conns.retain(|c| c.from.node != diamond_id && c.to.node != diamond_id);
        conns.extend(new_connections.iter().cloned());
        conns.extend(rerouted_externals.iter().cloned());
    }
    // Rewrite parent-group membership: drop diamond_id, push every
    // inserted internal node id so the parent's auto-bounds expands
    // to enclose the expanded container.
    if !parent_groups.is_empty() {
        let inserted_ids: Vec<NodeId> = new_nodes.iter().map(|n| n.id.clone()).collect();
        let mut groups_w = host.groups.write().unwrap();
        for pg in &parent_groups {
            if let Some(g) = groups_w.iter_mut().find(|gr| gr.id == *pg) {
                g.members.retain(|m| *m != diamond_id);
                for nid in &inserted_ids {
                    if !g.members.contains(nid) {
                        g.members.push(nid.clone());
                    }
                }
            }
        }
    }
    host.groups.write().unwrap().push(group.clone());

    editor.remove_node(&diamond_id);
    for n in &new_nodes {
        editor.insert_node(n.clone());
    }
    for c in &new_connections {
        editor.insert_connection(c.clone());
    }
    for c in &rerouted_externals {
        editor.insert_connection(c.clone());
    }
    editor.insert_group(group.clone());
    // Mirror the parent-group member swap into the editor so the
    // editor's slot cache + graph_rev reflect the new membership and
    // selection / hit-testing stay consistent.
    if !parent_groups.is_empty() {
        let groups_r = host.groups.read().unwrap();
        for pg in &parent_groups {
            if let Some(g) = groups_r.iter().find(|gr| gr.id == *pg) {
                editor.set_group_members(pg, g.members.clone());
            }
        }
    }

    // Record expansion state so the minimize gesture can fully
    // reverse this operation. `inserted_connections` covers both
    // the internal flow and the rerouted-external connections so
    // minimize wipes them in one pass.
    let mut all_inserted_conns: Vec<ConnectionId> = new_connections.iter().map(|c| c.id).collect();
    all_inserted_conns.extend(rerouted_externals.iter().map(|c| c.id));

    // Capture the at-this-moment baseline AFTER all the mutations
    // above have committed. ExpansionBaseline::capture walks the
    // live host state through the wrapper group's members so any
    // subsequent edit (drag-reposition, add-to-group, port rewire)
    // shows up as a diff against this snapshot.
    let baseline = ExpansionBaseline::capture(host, &group_id, &prefix);

    host.expanded.write().unwrap().insert(
        group_id.clone(),
        ExpansionState {
            diamond,
            external_connections,
            inserted_nodes: new_nodes.iter().map(|n| n.id.clone()).collect(),
            inserted_connections: all_inserted_conns,
            group_id,
            subgraph_id: sub.id.clone(),
            id_prefix: prefix,
            baseline,
            parent_groups,
            edits_pending: false,
        },
    );

    tracing::info!(
        target: "node_editor_demo::subgraph",
        diamond = %diamond_id.as_str(),
        nodes = new_nodes.len(),
        conns = new_connections.len(),
        "subgraph expanded into colour-matched group container"
    );
}

/// Reverse [`expand_subgraph`]: drop every inserted entity and
/// restore the diamond + external connections.
fn minimize_subgraph(editor: &Editor, host: &HostGraph, group_id: GroupId) {
    let state = match host.expanded.write().unwrap().remove(&group_id) {
        Some(s) => s,
        None => return,
    };

    // Remove inserted nodes (also strips any incident connections
    // automatically via editor.remove_node).
    {
        let mut nodes = host.nodes.write().unwrap();
        nodes.retain(|n| !state.inserted_nodes.contains(&n.id));
    }
    {
        let mut conns = host.connections.write().unwrap();
        conns.retain(|c| {
            !state.inserted_nodes.contains(&c.from.node)
                && !state.inserted_nodes.contains(&c.to.node)
        });
    }
    for nid in &state.inserted_nodes {
        editor.remove_node(nid);
    }

    // Drop the wrapping group.
    host.groups
        .write()
        .unwrap()
        .retain(|g| g.id != state.group_id);
    editor.remove_group(&state.group_id);

    // Restore parent-group membership: drop the inserted node ids
    // and put the diamond back where it was. Mirror to the editor's
    // set_group_members so the slot cache + graph_rev refresh.
    if !state.parent_groups.is_empty() {
        let inserted: &[NodeId] = &state.inserted_nodes;
        let diamond_id = state.diamond.id.clone();
        let mut groups_w = host.groups.write().unwrap();
        for pg in &state.parent_groups {
            if let Some(g) = groups_w.iter_mut().find(|gr| gr.id == *pg) {
                g.members.retain(|m| !inserted.contains(m));
                if !g.members.contains(&diamond_id) {
                    g.members.push(diamond_id.clone());
                }
            }
        }
        drop(groups_w);
        let groups_r = host.groups.read().unwrap();
        for pg in &state.parent_groups {
            if let Some(g) = groups_r.iter().find(|gr| gr.id == *pg) {
                editor.set_group_members(pg, g.members.clone());
            }
        }
    }

    // Restore the diamond + its external connections.
    host.nodes.write().unwrap().push(state.diamond.clone());
    editor.insert_node(state.diamond.clone());
    for c in &state.external_connections {
        host.connections.write().unwrap().push(c.clone());
        editor.insert_connection(c.clone());
    }

    tracing::info!(
        target: "node_editor_demo::subgraph",
        group = %group_id.as_str(),
        restored_external = state.external_connections.len(),
        "subgraph minimized back to diamond"
    );
}

/// Returns true when the live wrapper-group state diverges from the
/// at-expand baseline — i.e. the user has added / removed nodes,
/// repositioned a node, or rewired an internal connection while the
/// subgraph was open. Compares the same shape that
/// `writeback_expansion_to_subgraph` would persist, so a "dirty"
/// verdict guarantees that a save would actually change the
/// canonical subgraph (no spurious dialogs).
fn is_expansion_dirty(state: &ExpansionState, host: &HostGraph) -> bool {
    if state.edits_pending {
        return true;
    }
    let current = ExpansionBaseline::capture(host, &state.group_id, &state.id_prefix);
    current != state.baseline
}

/// Flip the `edits_pending` flag for an expansion if `group_id`
/// names a live wrapping subgraph. No-op for non-wrapping groups.
/// Called from membership-mutating event handlers so the
/// save-changes dialog surfaces even when net diff against baseline
/// is zero (drag-in-then-out, drag-out-then-in, drag-to-reposition
/// inside the wrapper, etc.).
fn mark_wrapper_edits_pending(host: &HostGraph, group_id: &GroupId) {
    if let Some(s) = host.expanded.write().unwrap().get_mut(group_id) {
        s.edits_pending = true;
    }
}

/// Strip the `expanded:<diamond>:` prefix off the wrapper group's
/// live members and persist them as the new canonical subgraph
/// contents. Internal connections (both endpoints inside the
/// wrapper) get the same treatment.
///
/// External connections that the user rewired during expansion live
/// at the host boundary, NOT inside the subgraph definition, so
/// they're intentionally NOT written back here — minimize already
/// drops them and restores the original `state.external_connections`.
///
/// Best-effort: if the canonical subgraph is missing (deleted
/// behind the user's back), the writeback is a no-op and we log a
/// warning instead of panicking.
fn writeback_expansion_to_subgraph(
    editor: &Editor,
    host: &HostGraph,
    state: &ExpansionState,
    rename: &std::collections::HashMap<NodeId, NodeId>,
) {
    use std::collections::HashSet;
    let member_set: HashSet<NodeId> = host
        .groups
        .read()
        .unwrap()
        .iter()
        .find(|g| g.id == state.group_id)
        .map(|g| g.members.iter().cloned().collect())
        .unwrap_or_default();
    if member_set.is_empty() {
        tracing::warn!(
            target: "node_editor_demo::subgraph",
            group = %state.group_id.as_str(),
            "writeback skipped: wrapper group has no live members"
        );
        return;
    }

    // Two-step id resolution applied uniformly to nodes + internal
    // connection endpoints: (1) strip the `expanded:<diamond>:`
    // prefix off original-subgraph clones; (2) substitute via
    // `rename` for user-added nodes that needed a unique canonical
    // id to avoid colliding with existing subgraph nodes. Either
    // step can be a no-op and the id flows through unchanged.
    let resolve_canonical = |raw: &NodeId| -> NodeId {
        if let Some(renamed) = rename.get(raw) {
            return renamed.clone();
        }
        let stripped = raw
            .as_str()
            .strip_prefix(&state.id_prefix)
            .unwrap_or(raw.as_str())
            .to_string();
        NodeId::from(stripped)
    };

    let new_nodes: Vec<NodeInstance<()>> = host
        .nodes
        .read()
        .unwrap()
        .iter()
        .filter(|n| member_set.contains(&n.id))
        .map(|n| {
            let mut clone = n.clone();
            clone.id = resolve_canonical(&n.id);
            clone
        })
        .collect();
    let new_connections: Vec<Connection<()>> = host
        .connections
        .read()
        .unwrap()
        .iter()
        .filter(|c| member_set.contains(&c.from.node) && member_set.contains(&c.to.node))
        .map(|c| {
            let mut nc = Connection::new(
                PortAddress::new(resolve_canonical(&c.from.node), c.from.port.as_str()),
                PortAddress::new(resolve_canonical(&c.to.node), c.to.port.as_str()),
            );
            nc.state = c.state;
            nc
        })
        .collect();

    let applied = editor
        .with_subgraph_graph_mut(&state.subgraph_id, |sub| {
            sub.nodes = new_nodes.clone();
            sub.connections = new_connections.clone();
        })
        .is_some();

    if applied {
        tracing::info!(
            target: "node_editor_demo::subgraph",
            subgraph = %state.subgraph_id.as_str(),
            nodes = new_nodes.len(),
            conns = new_connections.len(),
            "subgraph writeback applied"
        );
    } else {
        tracing::warn!(
            target: "node_editor_demo::subgraph",
            subgraph = %state.subgraph_id.as_str(),
            "writeback skipped: canonical subgraph not found in editor.subgraphs"
        );
    }
}

/// Apply the wrapping-subgraph removal aftermath when a node has
/// just been removed from an expanded subgraph's wrapper group. No-
/// op if `group_id` isn't an expanded subgraph or `removed` wasn't
/// one of the wrapper's at-expand-time inserted nodes.
///
/// Mutates host + editor + expansion state in three steps:
///
/// 1. Strip `expanded:<diamond>:` off the dragged-out node's id and
///    every connection endpoint pointing at it, so the node now
///    lives in the host name space at its natural id. Skipped if
///    the un-prefixed id already exists in the host (collision
///    avoidance — better to keep the artifact than corrupt state).
/// 2. Patch `state.inserted_nodes` to drop the removed id (in
///    whichever form survived step 1) so the next minimize doesn't
///    sweep the now-host-resident node into the void.
/// 3. Patch `state.external_connections` to drop any lifted
///    sub-route entries whose target was this node — they'd
///    otherwise restore on minimize as wires pointing at a missing
///    canonical id and re-expansion would silently mis-route them
///    via the entry/exit fallback.
/// 4. Recapture `state.baseline` so the save-changes diff reflects
///    the new wrapper member list (the removal is now part of the
///    "pending edit" set the dirty-check sees).
fn naturalize_wrapper_member_removal(
    editor: &Editor,
    host: &HostGraph,
    group_id: &GroupId,
    removed: &NodeId,
) {
    // Snapshot the expansion state once — we mutate later under a
    // write lock, so capture everything we need to plan first.
    let snapshot = host.expanded.read().unwrap().get(group_id).cloned();
    let Some(state) = snapshot else { return };
    if !state.inserted_nodes.iter().any(|n| n == removed) {
        return;
    }

    // Step 1: naturalize the id by stripping the expansion prefix.
    // The stripped id IS the canonical-subgraph id we'd write back
    // on save, so it doubles as the right token for the sub-route
    // cleanup below.
    let stripped: Option<NodeId> = removed
        .as_str()
        .strip_prefix(&state.id_prefix)
        .map(|s| NodeId::from(s.to_string()));
    let final_id: NodeId = match &stripped {
        Some(new_id) => {
            let collision = host.nodes.read().unwrap().iter().any(|n| n.id == *new_id);
            if collision {
                tracing::warn!(
                    target: "node_editor_demo::subgraph",
                    removed = %removed.as_str(),
                    natural = %new_id.as_str(),
                    "naturalize skipped — host already has a node with the un-prefixed id"
                );
                removed.clone()
            } else {
                rename_host_node(editor, host, removed, new_id);
                new_id.clone()
            }
        }
        None => removed.clone(),
    };

    // Step 2 + 3: rewrite the expansion state. Do NOT touch
    // `state.baseline` — the at-expand snapshot is the canonical
    // diff target for save-changes detection, and recapturing here
    // would normalize the drag-out to zero (the user's removal
    // would silently fall off the save dialog and the canonical
    // subgraph would keep the now-dragged-out node as a phantom).
    // We want is_expansion_dirty to return true so the save flow
    // can write back the new wrapper membership.
    let canonical_token = stripped.as_ref().map(|s| s.as_str().to_string());
    let sub_route_marker = canonical_token
        .as_ref()
        .map(|c| format!("{SUB_ROUTE_PREFIX}{c}:"));
    let mut expanded_w = host.expanded.write().unwrap();
    if let Some(s) = expanded_w.get_mut(group_id) {
        s.inserted_nodes.retain(|n| n != removed && n != &final_id);
        if let Some(ref marker) = sub_route_marker {
            s.external_connections.retain(|c| {
                !c.from.port.as_str().starts_with(marker) && !c.to.port.as_str().starts_with(marker)
            });
        }
    }
}

/// Rename a single host node from `old_id` to `new_id`, rewriting
/// every connection endpoint that referenced the old id along the
/// way. Mirrors the edit through the editor by removing the
/// canonical node + reinserting under the new id, then doing the
/// same for every incident connection. No-op when `old_id` doesn't
/// resolve to a live host node.
fn rename_host_node(editor: &Editor, host: &HostGraph, old_id: &NodeId, new_id: &NodeId) {
    let renamed_node: Option<NodeInstance<()>> = {
        let mut nodes = host.nodes.write().unwrap();
        if let Some(n) = nodes.iter_mut().find(|n| n.id == *old_id) {
            n.id = new_id.clone();
            Some(n.clone())
        } else {
            None
        }
    };
    let Some(renamed_node) = renamed_node else {
        return;
    };

    // Rewrite connection endpoints in the host mirror. Build a list
    // of new Connection values (with the renamed endpoint) and the
    // old ConnectionIds so the editor sync below can swap them.
    let rewritten: Vec<Connection<()>> = {
        let mut conns = host.connections.write().unwrap();
        let mut out: Vec<Connection<()>> = Vec::new();
        for c in conns.iter_mut() {
            let mut touched = false;
            if c.from.node == *old_id {
                c.from.node = new_id.clone();
                touched = true;
            }
            if c.to.node == *old_id {
                c.to.node = new_id.clone();
                touched = true;
            }
            if touched {
                out.push(c.clone());
            }
        }
        out
    };

    // Editor sync: drop the old node (which also drops its incident
    // connections in the editor's mirror) and reinsert the renamed
    // node + rewritten connections.
    editor.remove_node(old_id);
    editor.insert_node(renamed_node);
    for c in rewritten {
        editor.insert_connection(c);
    }

    tracing::info!(
        target: "node_editor_demo::subgraph",
        old = %old_id.as_str(),
        new = %new_id.as_str(),
        "host node naturalized after wrapper-group removal"
    );
}

/// Save path used by [`confirm_minimize_subgraph`]'s on_confirm
/// callback. Three jobs:
///
/// 1. Identify wrapper-group members the user *added* during
///    expansion (anything in the wrapper that wasn't in
///    `state.inserted_nodes` at expand-time).
/// 2. Lift those members' boundary-crossing connections (one
///    endpoint in the wrapper, one outside) onto the diamond, since
///    the moved-in nodes are about to disappear from the host. The
///    lifted connections become new entries in
///    `state.external_connections` so minimize re-attaches them to
///    the restored diamond and a future re-expansion can route them
///    via the entry/exit convention.
/// 3. Patch `state.inserted_nodes` so the added members get
///    deleted from the host on minimize (closing the "copied in
///    both places" gap).
///
/// Returns the updated state so the caller can persist it into
/// `host.expanded` before calling [`minimize_subgraph`]. Does NOT
/// itself remove anything from the host — that's
/// [`minimize_subgraph`]'s job, which inherits the patched
/// `inserted_nodes` and does the cleanup uniformly.
fn promote_added_members_to_subgraph(
    editor: &Editor,
    state: &ExpansionState,
    host: &HostGraph,
) -> (ExpansionState, std::collections::HashMap<NodeId, NodeId>) {
    use std::collections::{HashMap, HashSet};
    let member_set: HashSet<NodeId> = host
        .groups
        .read()
        .unwrap()
        .iter()
        .find(|g| g.id == state.group_id)
        .map(|g| g.members.iter().cloned().collect())
        .unwrap_or_default();
    let original_inserted: HashSet<NodeId> = state.inserted_nodes.iter().cloned().collect();
    let added_members: Vec<NodeId> = member_set
        .iter()
        .filter(|m| !original_inserted.contains(*m))
        .cloned()
        .collect();
    if added_members.is_empty() {
        return (state.clone(), HashMap::new());
    }
    let added_set: HashSet<NodeId> = added_members.iter().cloned().collect();

    // Build a host_id → canonical_subgraph_id map for the added
    // members. The canonical id has to be unique within the target
    // subgraph's existing node set, AND unique across other added
    // members we're inserting in the same save (two main-graph nodes
    // with colliding ids — e.g. user dropped two host "sink/1"s
    // somehow — would otherwise stomp each other). Strategy: if the
    // host id is already free, use it verbatim; else suffix
    // `/imported_<n>` with the smallest n that resolves.
    let existing_canonical_ids: HashSet<String> = editor
        .with_subgraph(&state.subgraph_id, |sub| {
            sub.nodes
                .iter()
                .map(|n| n.id.as_str().to_string())
                .collect::<HashSet<String>>()
        })
        .unwrap_or_default();
    let mut taken: HashSet<String> = existing_canonical_ids.clone();
    let mut canonical_rename: HashMap<NodeId, NodeId> = HashMap::new();
    for host_id in &added_members {
        let raw = host_id.as_str().to_string();
        let canonical = if !taken.contains(&raw) {
            raw.clone()
        } else {
            let mut n: u32 = 1;
            loop {
                let candidate = format!("{raw}/imported_{n}");
                if !taken.contains(&candidate) {
                    break candidate;
                }
                n += 1;
            }
        };
        taken.insert(canonical.clone());
        canonical_rename.insert(host_id.clone(), NodeId::from(canonical));
    }

    // For each connection currently in the host:
    //  - internal (both endpoints in wrapper) → drop, subgraph
    //    writeback already captured it.
    //  - boundary-crossing where the wrapper endpoint is in the
    //    added set → lift to the diamond. The diamond-side port
    //    encodes the SUB_ROUTE_PREFIX + canonical_node_id + original
    //    port so re-expansion targets the user-added node
    //    specifically. Without the encoding the rerouting code's
    //    "entry == last inserted" fallback would land all lifted
    //    incoming connections on a single shared node — fine for
    //    one added sink, broken for two.
    //  - boundary-crossing where the wrapper endpoint is in the
    //    ORIGINAL inserted set → leave alone, minimize already
    //    handles those via `state.external_connections` restore.
    let lifted: Vec<Connection<()>> = host
        .connections
        .read()
        .unwrap()
        .iter()
        .filter_map(|c| {
            let from_added = added_set.contains(&c.from.node);
            let to_added = added_set.contains(&c.to.node);
            let from_member = member_set.contains(&c.from.node);
            let to_member = member_set.contains(&c.to.node);
            if from_added && !to_member {
                let canonical = canonical_rename.get(&c.from.node).unwrap();
                let encoded = format!(
                    "{SUB_ROUTE_PREFIX}{}:{}",
                    canonical.as_str(),
                    c.from.port.as_str()
                );
                let mut nc = Connection::new(
                    PortAddress::new(state.diamond.id.clone(), encoded.as_str()),
                    PortAddress::new(c.to.node.clone(), c.to.port.as_str()),
                );
                nc.state = c.state;
                Some(nc)
            } else if to_added && !from_member {
                let canonical = canonical_rename.get(&c.to.node).unwrap();
                let encoded = format!(
                    "{SUB_ROUTE_PREFIX}{}:{}",
                    canonical.as_str(),
                    c.to.port.as_str()
                );
                let mut nc = Connection::new(
                    PortAddress::new(c.from.node.clone(), c.from.port.as_str()),
                    PortAddress::new(state.diamond.id.clone(), encoded.as_str()),
                );
                nc.state = c.state;
                Some(nc)
            } else {
                None
            }
        })
        .collect();

    let mut updated = state.clone();
    updated.external_connections.extend(lifted);
    updated.inserted_nodes.extend(added_members);
    (updated, canonical_rename)
}

/// Minimize the wrapper group back into a diamond, with a
/// save-changes confirm dialog interposed when the user has actually
/// edited the expanded view. Three outcomes:
///
/// * **No edits** → minimize immediately (dialog would be noise).
/// * **Edits + "Save changes"** → persist the diff into
///   `editor.subgraphs[subgraph_id]`, then minimize.
/// * **Edits + "Discard"** → minimize without persisting.
/// * **Edits + backdrop / Escape** → no-op; user stays in the
///   expanded view to keep editing.
fn confirm_minimize_subgraph(editor: &Editor, host: &HostGraph, group_id: GroupId) {
    let state = match host.expanded.read().unwrap().get(&group_id).cloned() {
        Some(s) => s,
        None => return,
    };

    let dirty = is_expansion_dirty(&state, host);
    // Tracing snapshot: dump the comparison source + result so a no-
    // dialog reproduction can be diagnosed without re-instrumenting.
    // Runs under the "node_editor_demo::subgraph" target so it's
    // filterable in production builds.
    {
        let current = ExpansionBaseline::capture(host, &state.group_id, &state.id_prefix);
        tracing::info!(
            target: "node_editor_demo::subgraph",
            group = %group_id.as_str(),
            dirty,
            baseline_node_count = state.baseline.nodes.len(),
            current_node_count = current.nodes.len(),
            baseline_conn_count = state.baseline.connections.len(),
            current_conn_count = current.connections.len(),
            "confirm_minimize_subgraph dirty-check snapshot"
        );
    }

    if !dirty {
        minimize_subgraph(editor, host, group_id);
        return;
    }

    let editor_for_save = editor.clone();
    let host_for_save = host.clone();
    let state_for_save = state.clone();
    let editor_for_discard = editor.clone();
    let host_for_discard = host.clone();
    let group_for_discard = group_id.clone();

    let title = format!("Save changes to {}?", state.subgraph_id.as_str());
    let description = "You have unsaved edits inside this subgraph. \
        Save to update the canonical subgraph definition, or discard \
        to drop the edits and minimize back to the diamond."
        .to_string();

    blinc_cn::dialog()
        .title(title)
        .description(description)
        .confirm_text("Save changes")
        .cancel_text("Discard")
        .on_confirm(move || {
            // Build a save-time state that promotes wrapper-added
            // nodes into `inserted_nodes` (so minimize removes them
            // from the host) and lifts their boundary-crossing
            // connections onto the diamond (so minimize re-attaches
            // them via `external_connections`). Without this, a
            // node the user dragged in from the main graph survives
            // minimize as a duplicate AND its incoming wires stay
            // pointed at the now-orphaned host copy. The rename map
            // carries any host_id → unique canonical_id swaps so the
            // writeback path can apply them uniformly to nodes and
            // internal connections in the canonical subgraph.
            let (updated, rename) = promote_added_members_to_subgraph(
                &editor_for_save,
                &state_for_save,
                &host_for_save,
            );
            host_for_save
                .expanded
                .write()
                .unwrap()
                .insert(updated.group_id.clone(), updated.clone());
            writeback_expansion_to_subgraph(&editor_for_save, &host_for_save, &updated, &rename);
            minimize_subgraph(&editor_for_save, &host_for_save, updated.group_id.clone());
        })
        .on_cancel(move || {
            minimize_subgraph(
                &editor_for_discard,
                &host_for_discard,
                group_for_discard.clone(),
            );
        })
        .show();
}

/// Animate every node from its current position to the layout-
/// supplied target over `LAYOUT_TWEEN_MS` using ease-out-cubic, so
/// the canvas slides smoothly instead of jump-cutting when the user
/// triggers auto-layout. Same scheduler-tick pattern the editor's
/// viewport tween uses; the callback self-unregisters when the
/// transition settles so the scheduler can return to its idle rate.
///
/// Each frame writes the lerped position to BOTH the host node
/// store and the editor's graph via `update_node_position`, so the
/// canvas + any host UI that reads positions stay in sync.
fn animate_layout_transition(editor: Editor, host: HostGraph, updates: Vec<(NodeId, Point)>) {
    use std::sync::atomic::{AtomicU64, Ordering};
    /// 480 ms feels like a deliberate "settle into place" beat —
    /// short enough not to drag, long enough that the user reads
    /// the motion as repositioning rather than a flash.
    const LAYOUT_TWEEN_MS: f32 = 480.0;

    let starts: Vec<(NodeId, Point, Point)> = {
        let nodes = host.nodes.read().unwrap();
        updates
            .into_iter()
            .filter_map(|(id, target)| {
                nodes
                    .iter()
                    .find(|n| n.id == id)
                    .map(|n| (id, n.position, target))
            })
            .collect()
    };
    if starts.is_empty() {
        return;
    }

    let Some(scheduler) = blinc_layout::get_global_scheduler() else {
        // No scheduler (headless / unit test) → snap to targets.
        let mut nodes = host.nodes.write().unwrap();
        for (id, _, target) in &starts {
            if let Some(n) = nodes.iter_mut().find(|n| n.id == *id) {
                n.position = *target;
            }
            editor.update_node_position(id, *target);
        }
        return;
    };

    let elapsed = Arc::new(AtomicU64::new(0));
    let cb_id_slot: Arc<Mutex<Option<blinc_animation::TickCallbackId>>> =
        Arc::new(Mutex::new(None));

    let elapsed_for_cb = elapsed.clone();
    let cb_id_slot_for_cb = cb_id_slot.clone();
    let editor_for_cb = editor.clone();
    let host_for_cb = host.clone();

    let cb_id = scheduler.register_tick_callback(move |dt_secs| {
        // `dt` from the scheduler is in seconds (see scheduler.rs
        // `raw_dt = ... as_secs_f32()`). Convert to ms to match
        // LAYOUT_TWEEN_MS's units.
        let dt_ms = dt_secs * 1000.0;
        let prev_ms = f32::from_bits(elapsed_for_cb.load(Ordering::Relaxed) as u32);
        let new_ms = prev_ms + dt_ms;
        elapsed_for_cb.store(new_ms.to_bits() as u64, Ordering::Relaxed);
        let raw_t = (new_ms / LAYOUT_TWEEN_MS).clamp(0.0, 1.0);
        let eased = blinc_animation::Easing::EaseOutCubic.apply(raw_t);

        {
            let mut nodes = host_for_cb.nodes.write().unwrap();
            for (id, from, to) in &starts {
                let x = from.x + (to.x - from.x) * eased;
                let y = from.y + (to.y - from.y) * eased;
                let pos = Point::new(x, y);
                if let Some(n) = nodes.iter_mut().find(|n| n.id == *id) {
                    n.position = pos;
                }
                editor_for_cb.update_node_position(id, pos);
            }
        }
        blinc_layout::request_redraw();

        if raw_t >= 1.0 {
            let id = cb_id_slot_for_cb.lock().unwrap().take();
            if let Some(id) = id {
                if let Some(s) = blinc_layout::get_global_scheduler() {
                    s.remove_tick_callback(id);
                }
            }
        }
    });
    if let Some(id) = cb_id {
        *cb_id_slot.lock().unwrap() = Some(id);
    }
}

/// Open the right-click context menu anchored at the cursor's
/// screen-space point. Branches on the [`ContextMenuTarget`] variant
/// to surface the most relevant actions for what the user clicked.
/// Callbacks capture editor / host / history clones and either push
/// `EditorEvent`s (so the host event-drain loop already in place
/// runs them) or invoke editor methods directly (for actions with
/// no host-side state to mirror, like `focus_on_node` or
/// `zoom_to_fit`).
fn open_context_menu(
    editor: &Editor,
    host: &HostGraph,
    history: &DemoHistory,
    target: blinc_node_editor::ContextMenuTarget,
    anchor: Point,
) {
    use blinc_node_editor::ContextMenuTarget as T;
    // Platform-aware modifier hint for shortcut display. Tabler's
    // own convention is "Ctrl+X" on win/linux and "⌘X" on macOS;
    // we follow that here.
    let mod_key = if cfg!(target_os = "macos") {
        "⌘ + "
    } else {
        "Ctrl+"
    };

    let mut menu = blinc_cn::context_menu().at(anchor.x, anchor.y);
    match target {
        T::Node(id) => {
            let e_dup = editor.clone();
            let e_focus = editor.clone();
            let e_disable = editor.clone();
            let id_dup = id.clone();
            let id_focus = id.clone();
            let id_disable = id.clone();
            let id_delete = id.clone();

            menu = menu
                .item_with_shortcut("Duplicate", format!("{mod_key}D"), move || {
                    e_dup.push_event(EditorEvent::DuplicateNodesRequested(vec![id_dup.clone()]));
                })
                .item("Focus", move || {
                    e_focus.focus_on_node(&id_focus);
                })
                .item("Toggle disable", move || {
                    // Read authoritative state from the editor, not
                    // the host mirror — `set_node_disabled` doesn't
                    // sync back to the host, so the host's `disabled`
                    // flag would stay stuck at the initial value and
                    // the toggle would be one-way.
                    let current = e_disable.is_node_disabled(&id_disable).unwrap_or(false);
                    e_disable.set_node_disabled(&id_disable, !current);
                })
                .separator()
                .item_with_shortcut("Delete", "Shift+DEL", {
                    let e_delete = editor.clone();
                    move || {
                        // Push an event instead of calling
                        // `confirm_delete_nodes` directly. Either
                        // path now works since `cn::context_menu`
                        // closes the menu BEFORE running our cb()
                        // (no more UnwindFromBelow cascade killing
                        // freshly-pushed dialogs), but routing
                        // through the event drain keeps all delete
                        // chains symmetric: keyboard DEL, multi-
                        // selection toolbar, and the context menu
                        // all funnel into the same
                        // `confirm_delete_nodes` site.
                        e_delete
                            .push_event(EditorEvent::DeleteNodesRequested(vec![id_delete.clone()]));
                    }
                });
        }
        T::Edge(id) => {
            let e_delete = editor.clone();
            menu = menu.item_with_shortcut("Delete connection", "Shift+DEL", move || {
                // Same defer rationale as the node Delete arm.
                e_delete.push_event(EditorEvent::DeleteConnectionRequested(id));
            });
        }
        T::Group(id) => {
            let id_edit = id.clone();
            let id_collapse = id.clone();
            let id_disable = id.clone();
            let id_delete = id.clone();
            let e_edit = editor.clone();
            let host_edit = host.clone();
            let e_collapse = editor.clone();
            let host_collapse = host.clone();
            let e_disable = editor.clone();
            let e_zoom = editor.clone();

            menu = menu
                .item("Edit…", move || {
                    let snapshot = host_edit
                        .groups
                        .read()
                        .unwrap()
                        .iter()
                        .find(|g| g.id == id_edit)
                        .cloned();
                    if let Some(g) = snapshot {
                        // Anchor at title rect like the chrome chip
                        // path does so the cn::dialog shape matches
                        // — dialog is modal-centred so the exact
                        // anchor coord doesn't matter visually, but
                        // it keeps the event payload consistent.
                        let anchor = blinc_core::layer::Rect::new(0.0, 0.0, 0.0, 0.0);
                        e_edit.push_event(EditorEvent::EditGroupRequested {
                            group: id_edit.clone(),
                            current_title: g.name,
                            current_description: g.description.unwrap_or_default(),
                            anchor_screen: anchor,
                        });
                    }
                })
                .item("Toggle collapse", move || {
                    let current = host_collapse
                        .groups
                        .read()
                        .unwrap()
                        .iter()
                        .find(|g| g.id == id_collapse)
                        .map(|g| g.is_collapsed)
                        .unwrap_or(false);
                    e_collapse.push_event(EditorEvent::ToggleCollapseRequested(
                        blinc_node_editor::ToggleCollapseRequest {
                            group: id_collapse.clone(),
                            collapsed: !current,
                        },
                    ));
                })
                .item("Zoom to group", {
                    let id_zoom = id.clone();
                    move || {
                        // `zoom_to_selection` only looks at NODE
                        // selections — right-clicking a group puts
                        // `group:{id}` in the selection set, not its
                        // members, so the old call was a no-op.
                        // `focus_on_group` walks the group's member
                        // ids and frames their union.
                        e_zoom.focus_on_group(&id_zoom);
                    }
                })
                .item("Toggle disable", move || {
                    // Read editor state, not host mirror (same
                    // rationale as the node-toggle callback above).
                    let current = e_disable.is_group_disabled(&id_disable).unwrap_or(false);
                    e_disable.set_group_disabled(&id_disable, !current);
                })
                .separator()
                .item("Delete group", {
                    let e_delete = editor.clone();
                    move || {
                        // Defer to next-frame drain so the menu
                        // close-cascade doesn't unwind the dialog.
                        e_delete.push_event(EditorEvent::DeleteGroupRequested(
                            blinc_node_editor::DeleteGroupRequest {
                                group: id_delete.clone(),
                            },
                        ));
                    }
                });
        }
        T::Canvas => {
            // Forwarding to the existing UndoRequested / RedoRequested
            // / SelectAllRequested event handlers keeps the side-
            // effects (toast banner, host resync) consistent with the
            // keyboard path — don't call `history.undo()` directly
            // here or we'd double-undo (once here + once in the arm).
            let _ = history; // keep capture for ownership symmetry
            let e_sel_all = editor.clone();
            let e_fit = editor.clone();
            let e_undo = editor.clone();
            let e_redo = editor.clone();

            let e_force = editor.clone();
            let e_layered_ltr = editor.clone();
            let e_layered_ttb = editor.clone();

            menu = menu
                .item_with_shortcut("Select all", format!("{mod_key}A"), move || {
                    e_sel_all.push_event(EditorEvent::SelectAllRequested);
                })
                .item("Zoom to fit", move || {
                    e_fit.zoom_to_fit();
                })
                .separator()
                // Auto-layout submenu — three strategies under a
                // single parent. Each child swaps the editor's
                // layout strategy then dispatches; the
                // LayoutApplied handler upstream mirrors positions
                // back to host + editor and animates the transition.
                .submenu("Auto-layout", move |sub| {
                    sub.item_with_icon(
                        "Force-directed",
                        tabler_svg_str(outline::AFFILIATE),
                        move || {
                            e_force
                                .set_layout_strategy(LayoutStrategy::Force(ForceConfig::default()));
                            e_force.apply_layout();
                        },
                    )
                    .item_with_icon(
                        "Left-to-right",
                        tabler_svg_str(outline::HIERARCHY),
                        move || {
                            e_layered_ltr.set_layout_strategy(LayoutStrategy::Layered(
                                LayeredConfig {
                                    orientation: LayoutOrientation::LeftToRight,
                                    ..LayeredConfig::default()
                                },
                            ));
                            e_layered_ltr.apply_layout();
                        },
                    )
                    .item_with_icon(
                        "Top-to-bottom",
                        tabler_svg_str(outline::HIERARCHY_3),
                        move || {
                            e_layered_ttb.set_layout_strategy(LayoutStrategy::Layered(
                                LayeredConfig {
                                    orientation: LayoutOrientation::TopToBottom,
                                    ..LayeredConfig::default()
                                },
                            ));
                            e_layered_ttb.apply_layout();
                        },
                    )
                })
                .separator()
                .item_with_shortcut("Undo", format!("{mod_key}Z"), move || {
                    e_undo.push_event(EditorEvent::UndoRequested);
                })
                .item_with_shortcut(
                    "Redo",
                    if cfg!(target_os = "macos") {
                        "⌘ + ⇧ + Z".to_string()
                    } else {
                        "Ctrl + Shift + Z".to_string()
                    },
                    move || {
                        e_redo.push_event(EditorEvent::RedoRequested);
                    },
                );
        }
    }
    let _ = menu.show();
}

/// Open a cn::dialog confirming a connection delete + run the
/// host/editor/history mutation chain on confirm. Used by both the
/// `DeleteConnectionRequested` event-drain arm AND the context-menu
/// "Delete connection" item so the two paths can't drift.
fn confirm_delete_connection(
    editor: &Editor,
    host: &HostGraph,
    history: &DemoHistory,
    id: ConnectionId,
) {
    let editor_for_confirm = editor.clone();
    let host_for_confirm = host.clone();
    let history_for_confirm = history.clone();
    blinc_cn::dialog()
        .title("Delete connection?")
        .description("This will remove the edge from the graph. The connected nodes stay in place.")
        .confirm_text("Delete")
        .cancel_text("Cancel")
        .confirm_destructive(true)
        .on_confirm(move || {
            let prev = host_for_confirm
                .connections
                .read()
                .unwrap()
                .iter()
                .find(|c| c.id == id)
                .cloned();
            host_for_confirm
                .connections
                .write()
                .unwrap()
                .retain(|c| c.id != id);
            editor_for_confirm.remove_connection(id);
            if let Some(prev) = prev {
                history_for_confirm.lock().unwrap().push(
                    EditorCommand::RemoveConnection(id),
                    EditorCommand::InsertConnection(prev),
                    "Delete Connection",
                );
            }
            tracing::info!("deleted connection {}", id.0);
        })
        .show();
}

/// Open a cn::dialog confirming a node-delete + run the
/// snapshot/mutate/history chain on confirm. Snapshots node + every
/// incident connection + per-group member list BEFORE mutation so
/// the composite inverse can re-insert everything on undo.
fn confirm_delete_nodes(
    editor: &Editor,
    host: &HostGraph,
    history: &DemoHistory,
    ids: Vec<NodeId>,
) {
    if ids.is_empty() {
        return;
    }
    let (title, description) = if ids.len() == 1 {
        (
            "Delete node?".to_string(),
            "This will remove the node and every connection attached to it.".to_string(),
        )
    } else {
        (
            format!("Delete {} nodes?", ids.len()),
            "This will remove the selected nodes and every connection attached to them."
                .to_string(),
        )
    };
    let editor_for_confirm = editor.clone();
    let host_for_confirm = host.clone();
    let history_for_confirm = history.clone();
    let ids_for_confirm = ids.clone();
    blinc_cn::dialog()
        .title(title)
        .description(description)
        .confirm_text("Delete")
        .cancel_text("Cancel")
        .confirm_destructive(true)
        .on_confirm(move || {
            let id_set: std::collections::HashSet<_> = ids_for_confirm.iter().cloned().collect();
            let removed_nodes: Vec<NodeInstance<()>> = host_for_confirm
                .nodes
                .read()
                .unwrap()
                .iter()
                .filter(|n| id_set.contains(&n.id))
                .cloned()
                .collect();
            let removed_conns: Vec<Connection<()>> = host_for_confirm
                .connections
                .read()
                .unwrap()
                .iter()
                .filter(|c| id_set.contains(&c.from.node) || id_set.contains(&c.to.node))
                .cloned()
                .collect();
            let group_membership_before: Vec<(GroupId, Vec<NodeId>)> = host_for_confirm
                .groups
                .read()
                .unwrap()
                .iter()
                .filter(|g| g.members.iter().any(|m| id_set.contains(m)))
                .map(|g| (g.id.clone(), g.members.clone()))
                .collect();

            host_for_confirm
                .nodes
                .write()
                .unwrap()
                .retain(|n| !id_set.contains(&n.id));
            host_for_confirm
                .connections
                .write()
                .unwrap()
                .retain(|c| !id_set.contains(&c.from.node) && !id_set.contains(&c.to.node));
            for g in host_for_confirm.groups.write().unwrap().iter_mut() {
                g.members.retain(|m| !id_set.contains(m));
            }
            for id in &ids_for_confirm {
                editor_for_confirm.remove_node(id);
            }

            let mut inverse: Vec<EditorCommand<DemoPort, (), (), ()>> = Vec::new();
            inverse.extend(removed_nodes.iter().cloned().map(EditorCommand::InsertNode));
            inverse.extend(
                removed_conns
                    .iter()
                    .cloned()
                    .map(EditorCommand::InsertConnection),
            );
            inverse.extend(
                group_membership_before
                    .iter()
                    .cloned()
                    .map(|(g, m)| EditorCommand::SetGroupMembers(g, m)),
            );
            let forward = EditorCommand::Composite(
                ids_for_confirm
                    .iter()
                    .cloned()
                    .map(EditorCommand::RemoveNode)
                    .collect(),
            );
            let label = if ids_for_confirm.len() == 1 {
                "Delete Node"
            } else {
                "Delete Nodes"
            };
            history_for_confirm.lock().unwrap().push(
                forward,
                EditorCommand::Composite(inverse),
                label,
            );
            tracing::info!("deleted {} node(s)", ids_for_confirm.len());
        })
        .show();
}

/// Open a cn::dialog confirming a group delete + run the
/// host/editor/history chain. The members stay in place — only the
/// grouping is removed.
fn confirm_delete_group(
    editor: &Editor,
    host: &HostGraph,
    history: &DemoHistory,
    group_id: GroupId,
) {
    let editor_for_confirm = editor.clone();
    let host_for_confirm = host.clone();
    let history_for_confirm = history.clone();
    let name = host
        .groups
        .read()
        .unwrap()
        .iter()
        .find(|g| g.id == group_id)
        .map(|g| g.name.clone())
        .unwrap_or_else(|| group_id.as_str().to_string());
    blinc_cn::dialog()
        .title(format!("Delete group \"{name}\"?"))
        .description("The grouping is removed; member nodes and connections stay.")
        .confirm_text("Delete")
        .cancel_text("Cancel")
        .confirm_destructive(true)
        .on_confirm(move || {
            let prev = host_for_confirm
                .groups
                .read()
                .unwrap()
                .iter()
                .find(|g| g.id == group_id)
                .cloned();
            host_for_confirm
                .groups
                .write()
                .unwrap()
                .retain(|g| g.id != group_id);
            editor_for_confirm.remove_group(&group_id);
            if let Some(prev) = prev {
                history_for_confirm.lock().unwrap().push(
                    EditorCommand::RemoveGroup(group_id.clone()),
                    EditorCommand::InsertGroup(prev),
                    "Delete Group",
                );
            }
            tracing::info!("deleted group {}", group_id.as_str());
        })
        .show();
}

/// Pull the editor's authoritative graph back into the host mirror.
/// Called after an undo / redo: the inverse command was applied
/// against the editor, so its in-memory graph is the new truth — the
/// host's RwLock-protected mirror needs to catch up so subsequent
/// "snapshot before mutate" reads see the right state.
fn resync_host_from_editor(editor: &Editor, host: &HostGraph) {
    let (nodes, conns, groups, _exposed) = editor.graph_snapshot();
    *host.nodes.write().unwrap() = nodes;
    *host.connections.write().unwrap() = conns;
    *host.groups.write().unwrap() = groups;
}

/// Which group field a double-click is editing. Drives the editor
/// widget choice (single-line `cn::input` vs multi-line
/// `cn::textarea`) AND the save callback's field write.
#[derive(Clone, Copy)]
enum EditorField {
    Title,
    Description,
}

/// Open a floating inline editor anchored at the group field's
/// screen rect. cn::input for `Title`, cn::textarea for
/// `Description`. Enter (no shift) commits + closes; Esc closes
/// without saving; Shift+Enter inserts a newline inside the
/// textarea (default cn::textarea behaviour — we don't intercept
/// it). The save callback patches the host's `groups` Vec + calls
/// `editor.insert_group` to re-sync.
fn open_inline_text_editor(
    editor: &Editor,
    host: &HostGraph,
    history: &DemoHistory,
    group: GroupId,
    current: String,
    anchor_screen: blinc_core::layer::Rect,
    field: EditorField,
) {
    use blinc_core::context_state::BlincContextState;
    use blinc_layout::click_outside;
    use blinc_layout::overlay_state::overlay_stack;
    use blinc_layout::widgets::overlay_stack::{OverlayBuilder, OverlayHandle};
    use std::sync::Mutex;

    let theme = ThemeState::get();
    // Width-only sizing knobs — outer chrome is now provided by
    // the cn::input / cn::textarea widget itself (bg, border,
    // shadow). We only need the spacing token for our id'd
    // wrapper's padding allowance.
    let _ = (
        theme.color(ColorToken::SurfaceElevated),
        theme.color(ColorToken::Border),
    );
    let padding = theme.spacing_value(blinc_theme::tokens::SpacingToken::Space2);

    // Estimate the popover footprint for window-bounds clamping
    // (same approach as the multi-select toolbar). Width matches
    // the anchor rect with a sensible minimum so a short title
    // doesn't collapse the editor to a sliver. Height differs by
    // field — textarea defaults taller.
    let estimated_w = anchor_screen.width().max(220.0) + padding * 2.0;
    let estimated_h = match field {
        EditorField::Title => 48.0,
        EditorField::Description => 110.0,
    };
    let (window_w, window_h) = overlay_stack()
        .lock()
        .ok()
        .map(|s| s.viewport())
        .unwrap_or((f32::INFINITY, f32::INFINITY));
    let inset = 8.0_f32;
    let clamped_x = anchor_screen
        .x()
        .clamp(inset, (window_w - estimated_w - inset).max(inset));
    let clamped_y = anchor_screen
        .y()
        .clamp(inset, (window_h - estimated_h - inset).max(inset));

    // Reserve the overlay id BEFORE `.show()` so the click-outside
    // registry can bind to the editor div's stable id immediately.
    let next_handle_id = overlay_stack()
        .lock()
        .ok()
        .map(|s| s.peek_next_handle_id())
        .unwrap_or(0);
    let editor_id = format!("ne-inline-text-{next_handle_id}");
    let click_outside_key = format!("ne-inline-text:{next_handle_id}");

    // Save / cancel both flow through the captured `OverlayHandle`
    // stored here. `show()` returns the handle after the overlay
    // is mounted; we stash it into this slot so the keyboard
    // shortcuts can close it.
    let handle_slot: Arc<Mutex<Option<OverlayHandle>>> = Arc::new(Mutex::new(None));

    // Per-editor shared state. cn::input + cn::textarea both want
    // their backing state cell created up-front so it survives the
    // overlay's re-renders. We prefill with `current` so the user
    // can edit in place rather than re-typing.
    let input_data = match field {
        EditorField::Title => {
            let data = blinc_layout::widgets::text_input::text_input_data();
            data.lock().unwrap().value = current.clone();
            Some(data)
        }
        EditorField::Description => None,
    };
    let area_state = match field {
        EditorField::Title => None,
        EditorField::Description => {
            let state = blinc_layout::widgets::text_area::text_area_state();
            {
                let mut s = state.lock().unwrap();
                s.lines = if current.is_empty() {
                    vec![String::new()]
                } else {
                    current.split('\n').map(String::from).collect()
                };
            }
            Some(state)
        }
    };

    // Auto-focus via the DEFERRED path. The synchronous
    // focus_text_input call we tried previously runs side-effects
    // (notify_continuous_redraw → wakes the scheduler) BEFORE the
    // popover content has been added to the tree, which on canvas-
    // backed hosts produced a ~4s "canvas broken / zoomed-out"
    // state until mouse-move. The deferred variants enqueue the
    // focus request; the windowed frame loop's pending-focus
    // drain (called after rebuild_overlay_subtree_if_dirty)
    // applies it on the frame the popover content actually mounts
    // — focus side-effects then land against a stable composition.
    if let Some(ref data) = input_data {
        blinc_layout::widgets::text_input::focus_text_input_deferred(data);
    }
    if let Some(ref state) = area_state {
        blinc_layout::widgets::text_area::focus_text_area_deferred(state);
    }

    // Build the save callback once — both Enter and (future) Save
    // buttons fan into it. Reads the current value from the
    // appropriate state cell, patches host.groups, and pushes the
    // updated group back through the editor's command surface.
    let editor_for_save = editor.clone();
    let host_for_save = host.clone();
    let history_for_save = history.clone();
    let group_for_save = group.clone();
    let input_for_save = input_data.clone();
    let area_for_save = area_state.clone();
    let slot_for_save = handle_slot.clone();
    let field_for_save = field;
    let save = Arc::new(move || {
        let new_value: Option<String> = match field_for_save {
            EditorField::Title => input_for_save
                .as_ref()
                .map(|d| d.lock().unwrap().value.clone()),
            EditorField::Description => area_for_save
                .as_ref()
                .map(|s| s.lock().unwrap().lines.join("\n")),
        };
        let Some(new_value) = new_value else {
            return;
        };
        // Snapshot the pre-edit group BEFORE applying any change. The
        // inverse undo is `InsertGroup(prev)` — it walks back the
        // title or description to the value that was there when the
        // user opened the inline editor.
        let prev_group = host_for_save
            .groups
            .read()
            .unwrap()
            .iter()
            .find(|g| g.id == group_for_save)
            .cloned();
        // Skip the round-trip when the user pressed Enter without
        // changing anything — no need to bump graph_rev.
        let unchanged = prev_group
            .as_ref()
            .map(|g| match field_for_save {
                EditorField::Title => g.name == new_value,
                EditorField::Description => g.description.as_deref().unwrap_or("") == new_value,
            })
            .unwrap_or(false);
        if !unchanged {
            // Patch host first, then sync the editor's copy.
            let updated = {
                let mut groups = host_for_save.groups.write().unwrap();
                if let Some(g) = groups.iter_mut().find(|g| g.id == group_for_save) {
                    match field_for_save {
                        EditorField::Title => g.name = new_value.clone(),
                        EditorField::Description => {
                            g.description = if new_value.is_empty() {
                                None
                            } else {
                                Some(new_value.clone())
                            };
                        }
                    }
                    Some(g.clone())
                } else {
                    None
                }
            };
            if let (Some(updated), Some(prev)) = (updated, prev_group) {
                editor_for_save.insert_group(updated.clone());
                let label = match field_for_save {
                    EditorField::Title => "Edit Group Title",
                    EditorField::Description => "Edit Group Description",
                };
                history_for_save.lock().unwrap().push(
                    EditorCommand::InsertGroup(updated),
                    EditorCommand::InsertGroup(prev),
                    label,
                );
            }
            tracing::info!(
                "inline edit committed: group={} field={:?}",
                group_for_save.as_str(),
                std::any::type_name_of_val(&field_for_save),
            );
        }
        if let Some(h) = slot_for_save.lock().unwrap().as_ref() {
            h.close();
        }
    });

    let close_only = {
        let slot = handle_slot.clone();
        Arc::new(move || {
            if let Some(h) = slot.lock().unwrap().as_ref() {
                h.close();
            }
        })
    };

    // Build the overlay's content closure. Re-runs whenever the
    // overlay's reactive surface re-renders (rare for a focused
    // editor). Each render rebuilds the inner widget bound to the
    // same input_data / area_state, so the text + cursor survive.
    let editor_id_for_content = editor_id.clone();
    let click_outside_key_for_close = click_outside_key.clone();
    let save_for_content = save.clone();
    let close_only_for_content = close_only.clone();
    let input_for_content = input_data.clone();
    let area_for_content = area_state.clone();
    let field_for_content = field;
    let estimated_w_for_content = estimated_w;

    let handle = OverlayBuilder::popover()
        .at(clamped_x, clamped_y)
        .on_close(move |_reason| {
            click_outside::unregister_click_outside(&click_outside_key_for_close);
            // Release focus on the text input / textarea so the
            // global pointer pipeline doesn't keep routing
            // pointer-down events to the (now-hidden) widget for
            // text-selection drag — that route blocks the editor's
            // own drag handlers and makes a previously-edited
            // group undraggable until the next focus change. Pairs
            // with `focus_text_input` on open. Handles both
            // text_input AND text_area focus trackers.
            blinc_layout::widgets::text_input::blur_all_text_inputs();
        })
        .content(move || {
            let save = save_for_content.clone();
            let close_only = close_only_for_content.clone();
            // cn::input + cn::textarea ship their own bg + border
            // + rounded chrome. Wrapping them in an additional
            // bordered popover div doubles the visible container;
            // the outer scaffold here strips its own chrome (no
            // bg, no border, no shadow) and only carries the id
            // for click-outside + the on_key_down handler for
            // Enter / Esc. The cn widget paints the only visible
            // surface.
            let body: Div = match field_for_content {
                EditorField::Title => {
                    let data = input_for_content
                        .as_ref()
                        .expect("title editor missing input data")
                        .clone();
                    // Auto-focus already applied: open_inline_text_editor
                    // calls focus_text_input on `data` BEFORE this
                    // closure runs, so the cn::input Stateful built
                    // here initialises directly in Focused state —
                    // no mid-frame Idle→Focused transition, no
                    // cascading rebuild, no static-cache invalidation.
                    div()
                        .w(estimated_w_for_content - padding * 2.0)
                        .child(blinc_cn::input(&data).w(estimated_w_for_content - padding * 2.0))
                }
                EditorField::Description => {
                    let state = area_for_content
                        .as_ref()
                        .expect("description editor missing area state")
                        .clone();
                    // Sized via the parent div — cn::textarea
                    // doesn't currently expose explicit width on
                    // its builder.
                    div()
                        .w(estimated_w_for_content - padding * 2.0)
                        .h(80.0)
                        .child(blinc_cn::textarea(&state))
                }
            };
            let save_for_keys = save.clone();
            let close_for_keys = close_only.clone();
            let field_for_keys = field_for_content;
            div()
                .id(&editor_id_for_content)
                .child(body)
                .on_key_down(move |evt| {
                    let kc = blinc_core::events::KeyCode(evt.key_code);
                    if kc == blinc_core::events::KeyCode::ESCAPE {
                        (close_for_keys)();
                        return;
                    }
                    if kc == blinc_core::events::KeyCode::ENTER {
                        // Description: Shift+Enter passes through
                        // to the textarea's own key handling (which
                        // inserts a newline). Title: plain Enter
                        // commits unconditionally — single-line
                        // input.
                        let allow_newline =
                            matches!(field_for_keys, EditorField::Description) && evt.shift;
                        if !allow_newline {
                            (save_for_keys)();
                        }
                    }
                })
        })
        .show();

    *handle_slot.lock().unwrap() = Some(handle);

    let handle_for_outside = handle;
    click_outside::register_click_outside(&click_outside_key, &editor_id, move || {
        handle_for_outside.close();
    });
    let _ = BlincContextState::get;
}

/// Combined group editor — title input above a description textarea,
/// rendered inside the same `blinc_cn::dialog` chrome that the
/// delete-confirm flow uses. Fired by the edit chip in the group
/// header (`EditorEvent::EditGroupRequested`). Both fields commit
/// together as ONE history entry so undo reverts the whole edit
/// atomically, not field-by-field.
///
/// `anchor_screen` is ignored — cn's dialog is modal-centred over
/// the viewport. We keep the parameter on the event so a future
/// non-modal variant (popover anchored at the chip) can subscribe
/// without an API break.
fn open_inline_group_form(
    _editor_ignored: &Editor,
    host: &HostGraph,
    history: &DemoHistory,
    group: GroupId,
    current_title: String,
    current_description: String,
    _anchor_screen: blinc_core::layer::Rect,
) {
    let editor_for_form = _editor_ignored.clone();
    // Backing state — pre-filled so the user can edit in place.
    let title_data = blinc_layout::widgets::text_input::text_input_data();
    title_data.lock().unwrap().value = current_title;
    let desc_state = blinc_layout::widgets::text_area::text_area_state();
    {
        let mut s = desc_state.lock().unwrap();
        s.lines = if current_description.is_empty() {
            vec![String::new()]
        } else {
            current_description.split('\n').map(String::from).collect()
        };
    }
    // Auto-focus title on open — same convention as the single-
    // field inline editor.
    blinc_layout::widgets::text_input::focus_text_input_deferred(&title_data);

    // cn::dialog renders the body closure on every internal repaint,
    // so the input + textarea references live in `Arc`s captured by
    // both the body closure AND the on_confirm callback. State cells
    // already use `Arc<Mutex<...>>` so cloning is cheap.
    let title_for_body = title_data.clone();
    let desc_for_body = desc_state.clone();
    let label_color = token(ColorToken::TextSecondary, Color::rgb(0.65, 0.65, 0.7));

    let host_for_confirm = host.clone();
    let history_for_confirm = history.clone();
    let group_for_confirm = group.clone();
    let title_for_confirm = title_data.clone();
    let desc_for_confirm = desc_state.clone();

    blinc_cn::dialog()
        .title("Edit group")
        .description("Rename the group or update its description.")
        .content(move || {
            let title = title_for_body.clone();
            let desc = desc_for_body.clone();
            // Spacing on the 4 px scale: label-to-field uses 1 gap
            // (4 px), field-to-field uses 2 gaps (8 px). The dialog
            // chrome supplies its own outer padding, so we don't
            // double-up here.
            let title_field = div()
                .flex_col()
                .gap(1.0)
                .child(text("Title").size(11.0).color(label_color))
                .child(blinc_cn::input(&title));
            let desc_field = div()
                .flex_col()
                .gap(1.0)
                .child(text("Description").size(11.0).color(label_color))
                .child(div().h(80.0).child(blinc_cn::textarea(&desc)));
            div()
                .flex_col()
                .gap(2.0)
                .child(title_field)
                .child(desc_field)
        })
        .confirm_text("Save")
        .cancel_text("Cancel")
        .on_confirm(move || {
            let new_title = title_for_confirm.lock().unwrap().value.clone();
            let new_description = desc_for_confirm.lock().unwrap().lines.join("\n");

            let prev_group = host_for_confirm
                .groups
                .read()
                .unwrap()
                .iter()
                .find(|g| g.id == group_for_confirm)
                .cloned();
            let unchanged = prev_group
                .as_ref()
                .map(|g| {
                    g.name == new_title && g.description.as_deref().unwrap_or("") == new_description
                })
                .unwrap_or(false);
            if unchanged {
                return;
            }
            let updated = {
                let mut groups = host_for_confirm.groups.write().unwrap();
                if let Some(g) = groups.iter_mut().find(|g| g.id == group_for_confirm) {
                    g.name = new_title.clone();
                    g.description = if new_description.is_empty() {
                        None
                    } else {
                        Some(new_description.clone())
                    };
                    Some(g.clone())
                } else {
                    None
                }
            };
            if let (Some(updated), Some(prev)) = (updated, prev_group) {
                editor_for_form.insert_group(updated.clone());
                // Single history entry covers BOTH fields so Cmd-Z
                // reverts the whole edit, not field-by-field.
                history_for_confirm.lock().unwrap().push(
                    EditorCommand::InsertGroup(updated),
                    EditorCommand::InsertGroup(prev),
                    "Edit Group",
                );
            }
            tracing::info!("group form committed: group={}", group_for_confirm.as_str());
        })
        .show();
}

// ─── UI scaffold ───────────────────────────────────────────────────

pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder + use<> {
    let (mut editor, host, history) = build_editor(ctx);

    // Drain the editor's event queue whenever any event is pushed.
    // A zero-size `stateful_with_key` widget with `deps` on the
    // events signal re-fires its closure on every push, which is
    // when we drain + dispatch to host state.
    //
    // The drain widget renders no chrome — it's mounted only for its
    // reactive lifecycle.
    let drain_editor = editor.clone();
    let drain_host = host.clone();
    let drain_history = history.clone();
    let evts_signal = editor.events_signal();
    let drainer = stateful_with_key::<NoState>("node-editor-event-drain")
        .deps([evts_signal])
        .on_state(move |_ctx| {
            for evt in drain_editor.drain_events() {
                handle_event(&drain_editor, &drain_host, &drain_history, evt);
            }
            div().w(0.0).h(0.0)
        });

    // Cull-stats HUD overlays the canvas top-left, rebuilding when
    // either the graph changes (nodes added / removed) or the
    // viewport pans / zooms. Reads `editor.last_render_stats()` —
    // populated at the end of every render frame — so the displayed
    // counts demonstrate the editor's frustum culling: pan / zoom
    // out and the `visible` numbers drop while `total` stays put.
    let hud_editor = editor.clone();
    let hud_deps = [editor.graph_signal(), editor.canvas_kit().viewport_signal()];
    let stats_hud = stateful_with_key::<NoState>("node-editor-cull-hud")
        .deps(hud_deps)
        .on_state(move |_ctx| {
            let s = hud_editor.last_render_stats();
            // `RenderLayer::Foreground` per known upstream U1: the
            // canvas closure's primitives paint OVER absolutely-
            // positioned siblings unless those siblings opt into the
            // foreground layer. Without this the HUD draws but stays
            // hidden beneath the editor's canvas surface.
            div()
                .absolute()
                .top(12.0)
                .left(12.0)
                .layer(RenderLayer::Foreground)
                .bg(Color::rgba(0.0, 0.0, 0.0, 0.55))
                .p(8.0)
                .gap(4.0)
                .flex_col()
                .child(
                    text(format!(
                        "visible nodes: {} / {}",
                        s.visible_nodes, s.total_nodes
                    ))
                    .size(11.0)
                    .color(Color::rgb(0.92, 0.92, 0.95)),
                )
                .child(
                    text(format!(
                        "visible edges: {} / {}",
                        s.visible_edges, s.total_edges
                    ))
                    .size(11.0)
                    .color(Color::rgb(0.92, 0.92, 0.95)),
                )
                .child(
                    text("press Shift+D to toggle disabled on selection")
                        .size(10.0)
                        .color(Color::rgb(0.65, 0.65, 0.70)),
                )
        });

    // Search bar — wraps `cn::input` and reactively shows a
    // "N matches" hint. `on_change` calls `editor.search_and_focus`
    // on every keystroke; the canvas itself demonstrates the
    // framework's multi-vs-single-match policy (multi → zoom out +
    // outline all; single → zoom in tight). The count is held in a
    // reactive `State<usize>` keyed under `use_state_keyed` so the
    // label rebuilds independently of the rest of the UI.
    // Persist the search input's backing data across `build_ui`
    // re-invocations (currently only window resize triggers a
    // rebuild). Without `use_state_keyed`, every rebuild would
    // mint a fresh `SharedTextInputData` and the user's typed
    // query would silently reset to empty on resize.
    let search_data: blinc_layout::widgets::text_input::SharedTextInputData = ctx
        .use_state_keyed(
            "ne-search-input-data",
            blinc_layout::widgets::text_input::text_input_data,
        )
        .get();
    let search_count: State<usize> = ctx.use_state_keyed("ne-search-count", || 0usize);
    let search_active: State<bool> = ctx.use_state_keyed("ne-search-active", || false);
    let search_editor = editor.clone();
    let search_count_setter = search_count.clone();
    let search_active_setter = search_active.clone();
    let search_input = blinc_cn::input(&search_data)
        .placeholder("Search nodes, groups, subgraphs…")
        .w(320.0)
        .on_change(move |q| {
            let hits = search_editor.search_and_focus(q);
            search_count_setter.set(hits.len());
            search_active_setter.set(!q.trim().is_empty());
        });
    let search_count_for_label = search_count.clone();
    let search_active_for_label = search_active.clone();
    let search_label = stateful_with_key::<NoState>("ne-search-result-label")
        .deps([search_count.signal_id(), search_active.signal_id()])
        .on_state(move |_| {
            // Always render the result count — including the empty-
            // query "0 matches" state. Hiding the label until
            // `search_active` flips reshuffled the flex row mid-
            // keystroke (the label's first appearance changed its
            // sibling input's pixel position between the pointer-
            // focus event and the first key event, and `cn::input`
            // dropped the in-flight character). Always-on label +
            // `flex_shrink_0` keeps the slot rectangular regardless
            // of count value or query state.
            let _active = search_active_for_label.get();
            let count = search_count_for_label.get();
            let label = if count == 1 {
                "1 match".to_string()
            } else {
                format!("{count} matches")
            };
            div()
                .min_w(110.0)
                .flex_shrink_0()
                .child(text(&label).size(12.0).color(token(
                    ColorToken::TextSecondary,
                    Color::rgb(0.65, 0.65, 0.70),
                )))
        });

    // No manual `.bg(...)` here — the editor's `element()` paints
    // its own workspace surface from the active theme, so the host
    // only needs to position it.
    div()
        .w(ctx.width)
        .h(ctx.height)
        .flex_col()
        .child(header_bar(search_input, search_label))
        .child(
            div()
                .flex_grow()
                .overflow_clip()
                .w_full()
                .relative()
                .child(editor.element())
                .child(drainer)
                .child(stats_hud),
        )
}

fn header_bar(
    search_input: impl ElementBuilder + 'static,
    search_label: impl ElementBuilder + 'static,
) -> Div {
    div()
        .w_full()
        .h(56.0)
        .bg(token(
            ColorToken::SurfaceElevated,
            Color::rgb(0.12, 0.12, 0.18),
        ))
        .flex_row()
        .items_center()
        .justify_between()
        .px(4.0)
        .gap(16.0)
        // Title + tagline on the left.
        .child(
            div()
                .flex_row()
                .items_center()
                .gap(16.0)
                .child(
                    text("Node Editor Demo")
                        .size(20.0)
                        .weight(FontWeight::Bold)
                        .color(token(ColorToken::TextPrimary, Color::WHITE)),
                )
                .child(
                    text("Drag output ports → input ports. Scroll = zoom, drag bg = pan.")
                        .size(12.0)
                        .color(token(
                            ColorToken::TextSecondary,
                            Color::rgb(0.55, 0.55, 0.65),
                        )),
                ),
        )
        // Search bar on the right — wired to
        // `editor.search_and_focus` in `build_ui`. The framework
        // handles the matching + viewport policy; the host just
        // surfaces the input and a result-count hint.
        //
        // `.flex_shrink_0()` is load-bearing here: `cn::input`'s
        // container sets a fixed `.w(320)`, but the default flex
        // shrink factor on flex children is non-zero, so a wide
        // left column (title + tagline) will squeeze the search
        // wrapper below 320 px. When that happens, the cn::input's
        // text-scroll-to-cursor clips leading characters out of
        // view — the data is intact (search_and_focus sees the
        // full string) but the visual reads as "characters got
        // chopped off." Pinning shrink to 0 holds the declared
        // width regardless of how wide the left column gets.
        .child(
            div()
                .flex_row()
                .items_center()
                .gap(8.0)
                .flex_shrink_0()
                .child(search_input)
                .child(search_label),
        )
}

/// Theme bundle shared by the desktop entry point and the wasm
/// wrapper. `build-web-examples`' codegen detects this `pub fn` and
/// hands the returned bundle to `ThemeState::init` BEFORE
/// `WebApp::run_with_async_setup`. Without that hook the wasm wrapper
/// falls back to whatever theme `WebApp::new` auto-installs, which
/// has no `cn_styles::CN_STYLES` attached — and every cn class
/// (`.cn-dialog`, `.cn-context-menu-item`, `.cn-tooltip`, etc.) loses
/// its padding / border / hover surface. Desktop avoided the issue
/// because `main()` calls `WindowedApp::run_with_theme` with the
/// bundle below directly.
pub fn theme_bundle() -> blinc_theme::ThemeBundle {
    HybridTheme::bundle().with_css(blinc_cn::cn_styles::CN_STYLES)
}

/// Color scheme the demo wants — same call shape as
/// `theme_bundle()` so the wasm wrapper picks it up automatically.
/// Hardcoded `Dark` here so the editor demo always renders in dark
/// mode; switch to `detect_system_color_scheme()` if the demo
/// should follow the OS preference instead.
pub fn theme_color_scheme() -> blinc_theme::ColorScheme {
    let _ = detect_system_color_scheme; // silences unused-import on desktop
    ColorScheme::Dark
}

// `main` is desktop-only. The wasm wrapper includes this file
// verbatim (via build-web-examples codegen) and provides its own
// `#[wasm_bindgen(start)]` entry that hands `build_ui` to
// `WebApp::run_with_async_setup`. Without this cfg gate the
// included `main` would reference `WindowedApp::run_with_theme`
// (gated `#[cfg(feature = "windowed")]`) under the wasm wrapper's
// `web`-only blinc_app and break the build.
#[cfg(not(target_arch = "wasm32"))]
fn main() -> blinc_app::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = WindowConfig {
        title: "Node Editor Demo".to_string(),
        width: 1100,
        height: 720,
        // Cap animation-driven redraws at 30 fps. The edge-state
        // shimmer / pulse don't need vsync to read smoothly, and
        // without this the FpsAdapter ramps up to 120 fps under
        // continuous animation — pinning ~40% CPU in debug. A
        // fixed cap disables the adaptive ramp.
        // animation_fps_cap: Some(30),
        animation_fps: AnimationFps::Adaptive,
        ..Default::default()
    };

    // Use the Hybrid Universal bundle in dark mode — gives us the
    // ShapeTokens + multi-layer shadows the editor consumes.
    blinc_app::windowed::WindowedApp::run_with_theme(
        config,
        theme_bundle(),
        theme_color_scheme(),
        build_ui,
    )
}
