//! Floating context toolbar for the selection.
//!
//! When the editor has a non-empty selection, this widget renders a
//! small absolute-positioned toolbar just above the selection's
//! top-left corner. The toolbar is composed of:
//!
//! - **Mark buttons** — Bold, Italic, Underline, Strikethrough, Code.
//!   Clicking applies the mark to the current selection via
//!   `apply_mark_to_selection`.
//! - **Color button** — opens an inline color picker (a small palette
//!   of preset swatches) anchored next to the toolbar.
//! - **Link button** — opens an inline URL prompt that captures
//!   keystrokes until Enter (commit) or Esc (cancel).
//!
//! The pickers are intentionally *editor-only* widgets — not
//! standalone main widgets — so they live and die with the selection.
//! They're rendered as siblings of the toolbar inside the same
//! absolute-positioned overlay container.
//!
//! All buttons use direct closures (no Stateful wrappers) — clicking a
//! button mutates the editor state through the shared `RichTextState`
//! and bumps the editor's version signal.

use std::sync::Arc;

use blinc_core::{Color, State};

use crate::div::{div, Div};
use crate::widgets::rich_text_editor::cursor::ActiveFormat;

use super::format::{apply_mark_to_selection, Mark};
use super::render::RichTextTheme;
use super::state::{PickerState, RichTextState};

/// Build the floating context toolbar (and any open inline picker)
/// anchored to the current selection. Returns `None` when there's no
/// selection or it's collapsed.
pub fn selection_toolbar(
    state: &RichTextState,
    version: &State<u32>,
    theme: &RichTextTheme,
) -> Option<Div> {
    let mut data = state.lock().ok()?;
    let bounds = data.selection_bounds()?;
    let picker = data.picker.clone();

    let (x, y, w, _h) = bounds;
    // Reserve enough horizontal room for the mark row plus the open
    // picker, if any. The link prompt is the widest variant.
    let toolbar_w: f32 = match picker {
        PickerState::Link { .. } => 540.0,
        PickerState::Color => 380.0,
        PickerState::None => 232.0,
    };
    let toolbar_h = 36.0_f32;
    let center_x = x + w * 0.5;
    let mut tx = (center_x - toolbar_w * 0.5).max(0.0);
    let mut ty = y - toolbar_h - 6.0;
    if ty < 0.0 {
        // Selection is at the very top — drop the toolbar below the
        // selection instead.
        ty = y + (bounds.3) + 6.0;
    }
    let _ = &mut tx;

    // Cache the toolbar's bounding rect so the editor's mouse-down
    // handler can detect clicks that land on the toolbar and bail
    // early instead of collapsing the selection. Without this, every
    // toolbar button click would dismiss its own context.
    data.toolbar_rect = Some((tx, ty, toolbar_w, toolbar_h));
    drop(data);

    let mark_row = mark_row(state, version, theme);
    let mut toolbar = div()
        .absolute()
        .left(tx)
        .top(ty)
        .h(toolbar_h)
        .padding_x_px(8.0)
        .flex_row()
        .items_center()
        .gap_px(4.0)
        // Fully opaque dark surface so the toolbar always reads as a
        // solid panel, even when it overlaps headings or large body
        // text directly underneath it.
        .bg(Color::rgba(0.10, 0.10, 0.13, 1.0))
        .rounded(8.0)
        .border(1.0, Color::rgba(0.30, 0.30, 0.36, 1.0))
        .shadow(blinc_core::Shadow {
            offset_x: 0.0,
            offset_y: 4.0,
            blur: 12.0,
            spread: 0.0,
            color: Color::rgba(0.0, 0.0, 0.0, 0.45),
        })
        .child(mark_row);

    // Inline picker, rendered to the right of the mark row.
    match picker {
        PickerState::None => {}
        PickerState::Color => {
            toolbar = toolbar
                .child(divider())
                .child(color_picker_row(state, version));
        }
        PickerState::Link { draft } => {
            toolbar = toolbar
                .child(divider())
                .child(link_prompt_row(state, version, theme, &draft));
        }
    }

    Some(toolbar)
}

/// Build the row of mark buttons (B, I, U, S, code) plus the color
/// and link triggers.
fn mark_row(state: &RichTextState, version: &State<u32>, theme: &RichTextTheme) -> Div {
    div()
        .flex_row()
        .items_center()
        .gap_px(2.0)
        .child(mark_button("B", "Bold", true, state, version, theme, |d| {
            apply_mark_via(d, Mark::Bold)
        }))
        .child(mark_button(
            "I",
            "Italic",
            false,
            state,
            version,
            theme,
            |d| apply_mark_via(d, Mark::Italic),
        ))
        .child(mark_button(
            "U",
            "Underline",
            false,
            state,
            version,
            theme,
            |d| apply_mark_via(d, Mark::Underline),
        ))
        .child(mark_button(
            "S",
            "Strikethrough",
            false,
            state,
            version,
            theme,
            |d| apply_mark_via(d, Mark::Strikethrough),
        ))
        .child(mark_button(
            "<>",
            "Inline code",
            false,
            state,
            version,
            theme,
            |d| apply_mark_via(d, Mark::Code),
        ))
        .child(divider())
        .child(picker_button("A", "Color", state, version, theme, |d| {
            d.picker = if matches!(d.picker, PickerState::Color) {
                PickerState::None
            } else {
                PickerState::Color
            };
        }))
        .child(picker_button("@", "Link", state, version, theme, |d| {
            d.picker = if matches!(d.picker, PickerState::Link { .. }) {
                PickerState::None
            } else {
                PickerState::Link {
                    draft: existing_link(d).unwrap_or_default(),
                }
            };
        }))
}

/// One mark button. The label is rendered with the mark applied (e.g.
/// the "B" button is bold) so users can tell what each button does at
/// a glance.
fn mark_button(
    label: &str,
    _tooltip: &str,
    bold: bool,
    state: &RichTextState,
    version: &State<u32>,
    _theme: &RichTextTheme,
    on_click: impl Fn(&mut super::state::RichTextData) -> bool + Send + Sync + 'static,
) -> Div {
    let state_for_click = Arc::clone(state);
    let version_for_click = version.clone();
    // `no_cursor()` clears the text element's default I-beam so the
    // parent button's pointer cursor wins on hover. This only affects
    // the cursor style — hit-testing and rendering are unchanged.
    let mut t = crate::text::text(label)
        .size(13.0)
        .color(Color::WHITE)
        .no_cursor();
    if bold {
        t = t.weight(crate::div::FontWeight::Bold);
    }
    div()
        .w(28.0)
        .h(24.0)
        .items_center()
        .justify_center()
        .rounded(4.0)
        .cursor_pointer()
        .child(t)
        .on_mouse_down(move |_| {
            if let Ok(mut data) = state_for_click.lock() {
                if on_click(&mut data) {
                    drop(data);
                    version_for_click.set(version_for_click.get().wrapping_add(1));
                }
            }
        })
}

/// Picker trigger button — same shape as a mark button but always
/// applies the closure regardless of return value (no undo push, just
/// state-flag change).
fn picker_button(
    label: &str,
    _tooltip: &str,
    state: &RichTextState,
    version: &State<u32>,
    _theme: &RichTextTheme,
    on_click: impl Fn(&mut super::state::RichTextData) + Send + Sync + 'static,
) -> Div {
    let state_for_click = Arc::clone(state);
    let version_for_click = version.clone();
    div()
        .w(28.0)
        .h(24.0)
        .items_center()
        .justify_center()
        .rounded(4.0)
        .cursor_pointer()
        .child(
            crate::text::text(label)
                .size(13.0)
                .color(Color::WHITE)
                .no_cursor(),
        )
        .on_mouse_down(move |_| {
            if let Ok(mut data) = state_for_click.lock() {
                on_click(&mut data);
                drop(data);
                version_for_click.set(version_for_click.get().wrapping_add(1));
            }
        })
}

/// Vertical separator between groups in the toolbar.
fn divider() -> Div {
    div()
        .w(1.0)
        .h(20.0)
        .bg(Color::rgba(0.34, 0.34, 0.40, 1.0))
        .ml(1.0)
        .mr(1.0)
}

// =====================================================================
// Color picker
// =====================================================================

/// Preset palette used by the inline color picker. Order matches a
/// typical text-color popover: a default white plus a small set of
/// useful highlight colors.
fn color_palette() -> &'static [(Color, &'static str)] {
    const PALETTE: &[(Color, &str)] = &[
        (
            Color {
                r: 0.92,
                g: 0.92,
                b: 0.95,
                a: 1.0,
            },
            "Default",
        ),
        (
            Color {
                r: 0.94,
                g: 0.39,
                b: 0.39,
                a: 1.0,
            },
            "Red",
        ),
        (
            Color {
                r: 0.97,
                g: 0.59,
                b: 0.20,
                a: 1.0,
            },
            "Orange",
        ),
        (
            Color {
                r: 0.96,
                g: 0.85,
                b: 0.40,
                a: 1.0,
            },
            "Yellow",
        ),
        (
            Color {
                r: 0.50,
                g: 0.85,
                b: 0.50,
                a: 1.0,
            },
            "Green",
        ),
        (
            Color {
                r: 0.40,
                g: 0.78,
                b: 1.00,
                a: 1.0,
            },
            "Blue",
        ),
        (
            Color {
                r: 0.66,
                g: 0.55,
                b: 1.00,
                a: 1.0,
            },
            "Purple",
        ),
        (
            Color {
                r: 0.55,
                g: 0.55,
                b: 0.65,
                a: 1.0,
            },
            "Gray",
        ),
    ];
    PALETTE
}

fn color_picker_row(state: &RichTextState, version: &State<u32>) -> Div {
    let mut row = div().flex_row().items_center().gap_px(4.0);
    for (color, _label) in color_palette() {
        let state_for_click = Arc::clone(state);
        let version_for_click = version.clone();
        let c = *color;
        row = row.child(
            div()
                .w(18.0)
                .h(18.0)
                .rounded(9.0)
                .bg(c)
                .border(1.0, Color::rgba(1.0, 1.0, 1.0, 0.18))
                .cursor_pointer()
                .on_mouse_down(move |_| {
                    if let Ok(mut data) = state_for_click.lock() {
                        if let Some(sel) = data.selection {
                            if !sel.is_empty() {
                                data.push_undo();
                                apply_mark_to_selection(&mut data.document, sel, Mark::Color(c));
                            }
                        }
                        // Always set the active format so subsequent
                        // typing carries the new color.
                        data.active_format.color = Some(c);
                        data.picker = PickerState::None;
                        drop(data);
                        version_for_click.set(version_for_click.get().wrapping_add(1));
                    }
                }),
        );
    }
    row
}

// =====================================================================
// Link prompt
// =====================================================================

/// Inline URL prompt. Shows the current draft as plain text plus three
/// buttons: Apply, Remove (clears any existing link), Cancel.
///
/// Real text input is captured by the editor's `on_text_input` handler
/// when the link prompt is open — see `editor::handle_link_prompt_*` —
/// so this widget is purely visual feedback for the in-flight URL.
fn link_prompt_row(
    state: &RichTextState,
    version: &State<u32>,
    _theme: &RichTextTheme,
    draft: &str,
) -> Div {
    let placeholder = if draft.is_empty() {
        "https://…"
    } else {
        draft
    };
    let placeholder_color = if draft.is_empty() {
        Color::rgba(0.55, 0.55, 0.65, 1.0)
    } else {
        Color::WHITE
    };

    let state_for_apply = Arc::clone(state);
    let version_for_apply = version.clone();
    let state_for_remove = Arc::clone(state);
    let version_for_remove = version.clone();
    let state_for_cancel = Arc::clone(state);
    let version_for_cancel = version.clone();

    div()
        .flex_row()
        .items_center()
        .gap_px(6.0)
        .child(
            div()
                .h(22.0)
                .padding_x_px(8.0)
                .min_w(160.0)
                .items_center()
                .rounded(4.0)
                .bg(Color::rgba(0.10, 0.10, 0.13, 1.0))
                .border(1.0, Color::rgba(0.34, 0.34, 0.40, 1.0))
                .child(
                    crate::text::text(placeholder)
                        .size(12.0)
                        .color(placeholder_color),
                ),
        )
        .child(prompt_button(
            "Apply",
            state_for_apply,
            version_for_apply,
            |d| {
                if let PickerState::Link { draft } = d.picker.clone() {
                    if !draft.is_empty() {
                        if let Some(sel) = d.selection {
                            if !sel.is_empty() {
                                d.push_undo();
                                apply_mark_to_selection(
                                    &mut d.document,
                                    sel,
                                    Mark::Link(Some(draft)),
                                );
                            }
                        }
                    }
                    d.picker = PickerState::None;
                }
            },
        ))
        .child(prompt_button(
            "Remove",
            state_for_remove,
            version_for_remove,
            |d| {
                if let Some(sel) = d.selection {
                    if !sel.is_empty() {
                        d.push_undo();
                        apply_mark_to_selection(&mut d.document, sel, Mark::Link(None));
                    }
                }
                d.picker = PickerState::None;
            },
        ))
        .child(prompt_button(
            "Cancel",
            state_for_cancel,
            version_for_cancel,
            |d| {
                d.picker = PickerState::None;
            },
        ))
}

fn prompt_button(
    label: &str,
    state: RichTextState,
    version: State<u32>,
    on_click: impl Fn(&mut super::state::RichTextData) + Send + Sync + 'static,
) -> Div {
    div()
        .h(22.0)
        .padding_x_px(8.0)
        .items_center()
        .justify_center()
        .rounded(4.0)
        .bg(Color::rgba(0.20, 0.20, 0.26, 1.0))
        .cursor_pointer()
        .child(
            crate::text::text(label)
                .size(11.0)
                .color(Color::WHITE)
                .no_cursor(),
        )
        .on_mouse_down(move |_| {
            if let Ok(mut data) = state.lock() {
                on_click(&mut data);
                drop(data);
                version.set(version.get().wrapping_add(1));
            }
        })
}

// =====================================================================
// Helpers
// =====================================================================

/// Helper for mark buttons — pushes undo and applies the mark to the
/// selection. Returns `true` if anything changed so the caller knows
/// whether to bump the rebuild signal.
fn apply_mark_via(data: &mut super::state::RichTextData, mark: Mark) -> bool {
    if let Some(sel) = data.selection {
        if !sel.is_empty() {
            data.push_undo();
            let changed = apply_mark_to_selection(&mut data.document, sel, mark);
            data.active_format = ActiveFormat::from_position(&data.document, data.cursor);
            return changed;
        }
    }
    false
}

/// Find an existing link URL inside the current selection (if any). The
/// link button uses this to seed the URL prompt's draft so editing an
/// existing link is a no-retype affair.
fn existing_link(data: &super::state::RichTextData) -> Option<String> {
    let sel = data.selection?;
    let (start, _end) = sel.ordered();
    let block = data.document.blocks.get(start.block)?;
    let line = block.lines.get(start.line)?;
    let byte = super::document::char_to_byte(&line.text, start.col);
    line.spans
        .iter()
        .find(|s| s.start <= byte && byte < s.end)
        .and_then(|s| s.link_url.clone())
}
