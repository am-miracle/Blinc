//! Shared text editing utilities for code and text_area widgets
//!
//! Contains word boundary detection, clipboard integration, and
//! other helpers shared between multi-line text editing widgets.

// =============================================================================
// Mobile haptic feedback + native edit menu helpers
// =============================================================================
//
// These wrap `blinc_core::native_bridge::native_call` and are no-ops on
// platforms / apps that don't have a native bridge installed (desktop /
// web). The actual implementations live in `BlincNativeBridge.swift`
// (`UIImpactFeedbackGenerator` / `UIMenuController` / `UIEditMenu`) and
// `BlincNativeBridge.kt` (`Vibrator` / `ActionMode`). Both bridge
// templates declare two namespaces backing this module: `haptics`
// for the per-character feedback and `edit_menu` for the
// double-tap context menu.

/// Trigger a light haptic "selection changed" feedback on mobile.
///
/// Maps to `UISelectionFeedbackGenerator.selectionChanged()` on iOS and
/// a short low-amplitude vibration on Android. Used when the user drags
/// the cursor with their finger so each character boundary crossed
/// gives a subtle tactile click — matches the native iOS UITextField /
/// Android EditText cursor-drag UX.
///
/// No-op on desktop / web and on mobile builds where the
/// `BlincNativeBridge` isn't initialized.
pub fn haptic_selection() {
    let _ = blinc_core::native_bridge::native_call::<(), _>("haptics", "selection", ());
}

/// Trigger a single short impact haptic — heavier than
/// `haptic_selection`. Used for "I just selected the word under your
/// finger" feedback on double-tap.
pub fn haptic_impact_light() {
    use blinc_core::native_bridge::NativeValue;
    // The bridge templates use `impact` with a `style` arg:
    // 0 = light, 1 = medium, 2 = heavy.
    let _ = blinc_core::native_bridge::native_call::<(), _>(
        "haptics",
        "impact",
        vec![NativeValue::Int32(0)],
    );
}

/// Available actions in a text-input edit menu, encoded as a bitmask
/// the native side decides how to render.
///
///   - bit 0 (0x01): Cut
///   - bit 1 (0x02): Copy
///   - bit 2 (0x04): Paste
///   - bit 3 (0x08): Select All
///
/// On iOS this becomes a `UIEditMenuInteraction` (iOS 16+) or a
/// `UIMenuController` with `UIMenuItem`s for the listed commands. On
/// Android it becomes an `ActionMode.Callback2` with the matching
/// menu items.
pub mod edit_menu_actions {
    pub const CUT: u32 = 0x01;
    pub const COPY: u32 = 0x02;
    pub const PASTE: u32 = 0x04;
    pub const SELECT_ALL: u32 = 0x08;
}

/// Show the native text-edit context menu (iOS UIEditMenuInteraction
/// / Android ActionMode) at the given screen-space position, with the
/// given selection bounds and supported actions.
///
/// The menu's actions route back to Rust through the same
/// `BlincNativeBridge` callback path: when the user picks Copy, the
/// native side calls `native_call("edit_menu", "on_action", (action,))`
/// which the Blinc app dispatches to whichever editable widget owns
/// the focus.
///
/// `actions` is a bitmask of `edit_menu_actions::*` constants —
/// callers should OR together the actions appropriate for the current
/// state (e.g. omit CUT/COPY when there's no selection, omit PASTE
/// when the clipboard is empty).
///
/// Returns `Ok(())` if the bridge is wired up. No-op on desktop / web
/// and on mobile builds without an initialized native bridge.
pub fn show_edit_menu(
    anchor_x: f32,
    anchor_y: f32,
    selection_x: f32,
    selection_y: f32,
    selection_width: f32,
    selection_height: f32,
    actions: u32,
) {
    use blinc_core::native_bridge::NativeValue;
    let _ = blinc_core::native_bridge::native_call::<(), _>(
        "edit_menu",
        "show",
        vec![
            NativeValue::Float32(anchor_x),
            NativeValue::Float32(anchor_y),
            NativeValue::Float32(selection_x),
            NativeValue::Float32(selection_y),
            NativeValue::Float32(selection_width),
            NativeValue::Float32(selection_height),
            NativeValue::Int32(actions as i32),
        ],
    );
}

/// Hide the native text-edit context menu, if any is currently
/// showing. Called when focus changes, the user taps elsewhere, or
/// the editor's content shifts so the anchor would be wrong.
pub fn hide_edit_menu() {
    let _ = blinc_core::native_bridge::native_call::<(), _>("edit_menu", "hide", ());
}

/// Find the start of the previous word from a character position in a line.
/// Words are separated by whitespace and punctuation boundaries.
pub fn word_boundary_left(text: &str, char_pos: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    if char_pos == 0 || chars.is_empty() {
        return 0;
    }

    let mut pos = char_pos.min(chars.len());

    // Skip whitespace to the left
    while pos > 0 && chars[pos - 1].is_whitespace() {
        pos -= 1;
    }

    if pos == 0 {
        return 0;
    }

    // Determine the category of the character we landed on
    let is_word = chars[pos - 1].is_alphanumeric() || chars[pos - 1] == '_';

    // Skip characters of the same category
    if is_word {
        while pos > 0 && (chars[pos - 1].is_alphanumeric() || chars[pos - 1] == '_') {
            pos -= 1;
        }
    } else {
        // Punctuation group
        while pos > 0
            && !chars[pos - 1].is_alphanumeric()
            && chars[pos - 1] != '_'
            && !chars[pos - 1].is_whitespace()
        {
            pos -= 1;
        }
    }

    pos
}

/// Find the end of the next word from a character position in a line.
/// Words are separated by whitespace and punctuation boundaries.
pub fn word_boundary_right(text: &str, char_pos: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    if char_pos >= len || chars.is_empty() {
        return len;
    }

    let mut pos = char_pos;

    // Determine the category of the character at pos
    let is_word = chars[pos].is_alphanumeric() || chars[pos] == '_';
    let is_ws = chars[pos].is_whitespace();

    if is_ws {
        // Skip whitespace
        while pos < len && chars[pos].is_whitespace() {
            pos += 1;
        }
    } else if is_word {
        // Skip word characters
        while pos < len && (chars[pos].is_alphanumeric() || chars[pos] == '_') {
            pos += 1;
        }
    } else {
        // Skip punctuation
        while pos < len
            && !chars[pos].is_alphanumeric()
            && chars[pos] != '_'
            && !chars[pos].is_whitespace()
        {
            pos += 1;
        }
    }

    // Also skip trailing whitespace after a word/punct group
    while pos < len && chars[pos].is_whitespace() {
        pos += 1;
    }

    pos
}

/// Find word boundaries around a position (for double-click word selection).
/// Returns (start, end) character positions of the word at `char_pos`.
pub fn word_at_position(text: &str, char_pos: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    if len == 0 || char_pos >= len {
        return (char_pos, char_pos);
    }

    let ch = chars[char_pos];
    let is_word = ch.is_alphanumeric() || ch == '_';

    if ch.is_whitespace() {
        // Select the whitespace run
        let mut start = char_pos;
        let mut end = char_pos;
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while end < len && chars[end].is_whitespace() {
            end += 1;
        }
        (start, end)
    } else if is_word {
        let mut start = char_pos;
        let mut end = char_pos;
        while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
            start -= 1;
        }
        while end < len && (chars[end].is_alphanumeric() || chars[end] == '_') {
            end += 1;
        }
        (start, end)
    } else {
        // Punctuation — select the punctuation run
        let mut start = char_pos;
        let mut end = char_pos;
        while start > 0
            && !chars[start - 1].is_alphanumeric()
            && chars[start - 1] != '_'
            && !chars[start - 1].is_whitespace()
        {
            start -= 1;
        }
        while end < len
            && !chars[end].is_alphanumeric()
            && chars[end] != '_'
            && !chars[end].is_whitespace()
        {
            end += 1;
        }
        (start, end)
    }
}

// =============================================================================
// Clipboard adapters
// =============================================================================
//
// `arboard` covers macOS, Windows, Linux, and iOS — but NOT
// `wasm32-unknown-unknown` (no platform backend) and NOT
// `target_os = "android"` (no X11/Wayland or UIKit clipboard
// layer that arboard knows how to talk to). Both excluded targets
// fall back to no-op stubs here. The Android runner can still
// reach the system clipboard through the
// `clipboard.copy` / `clipboard.paste` namespace handlers in
// `BlincNativeBridge.kt`, which apps wire up via the native
// bridge — that path is intentionally not surfaced through these
// sync helpers because the rich-text editor's Cmd+C / Cmd+V
// keybinds expect a synchronous return.
//
// `cfg(any(target_arch = "wasm32", target_os = "android"))` is
// the no-op path; everything else uses arboard.

/// Read text from the system clipboard.
/// Cross-platform via arboard (macOS, Windows, Linux, iOS).
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub fn clipboard_read() -> Option<String> {
    arboard::Clipboard::new()
        .ok()
        .and_then(|mut cb| cb.get_text().ok())
        .filter(|t| !t.is_empty())
}

/// Stub for wasm32 + Android. The browser clipboard API is
/// async-only and the Android system clipboard is reached via the
/// native bridge (`clipboard.paste` namespace) instead of a
/// synchronous helper. Returns `None` so Cmd+V keybinds in the
/// rich text editor no-op without crashing.
#[cfg(any(target_arch = "wasm32", target_os = "android"))]
pub fn clipboard_read() -> Option<String> {
    None
}

/// Write text to the system clipboard.
/// Cross-platform via arboard (macOS, Windows, Linux, iOS).
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub fn clipboard_write(text: &str) -> bool {
    arboard::Clipboard::new()
        .ok()
        .and_then(|mut cb| cb.set_text(text.to_string()).ok())
        .is_some()
}

/// Stub for wasm32 + Android. See [`clipboard_read`] for rationale.
#[cfg(any(target_arch = "wasm32", target_os = "android"))]
pub fn clipboard_write(_text: &str) -> bool {
    false
}

/// Read image from the system clipboard as RGBA pixels.
/// Returns (rgba_data, width, height) or None.
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub fn clipboard_read_image() -> Option<(Vec<u8>, u32, u32)> {
    let mut cb = arboard::Clipboard::new().ok()?;
    let img = cb.get_image().ok()?;
    Some((img.bytes.into_owned(), img.width as u32, img.height as u32))
}

/// Stub for wasm32 + Android. See [`clipboard_read`] for rationale.
#[cfg(any(target_arch = "wasm32", target_os = "android"))]
pub fn clipboard_read_image() -> Option<(Vec<u8>, u32, u32)> {
    None
}

/// Write image to the system clipboard from RGBA pixels.
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub fn clipboard_write_image(rgba: &[u8], width: u32, height: u32) -> bool {
    let img = arboard::ImageData {
        width: width as usize,
        height: height as usize,
        bytes: std::borrow::Cow::Borrowed(rgba),
    };
    arboard::Clipboard::new()
        .ok()
        .and_then(|mut cb| cb.set_image(img).ok())
        .is_some()
}

/// Stub for wasm32 + Android. See [`clipboard_read`] for rationale.
#[cfg(any(target_arch = "wasm32", target_os = "android"))]
pub fn clipboard_write_image(_rgba: &[u8], _width: u32, _height: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_word_boundary_left() {
        assert_eq!(word_boundary_left("hello world", 5), 0);
        assert_eq!(word_boundary_left("hello world", 6), 0);
        assert_eq!(word_boundary_left("hello world", 11), 6);
        assert_eq!(word_boundary_left("fn main() {", 3), 0);
        assert_eq!(word_boundary_left("fn main() {", 8), 7); // at ')', skips '(' punct
    }

    #[test]
    fn test_word_boundary_right() {
        assert_eq!(word_boundary_right("hello world", 0), 6);
        assert_eq!(word_boundary_right("hello world", 6), 11);
        assert_eq!(word_boundary_right("fn main() {", 0), 3);
    }

    #[test]
    fn test_word_at_position() {
        assert_eq!(word_at_position("hello world", 2), (0, 5));
        assert_eq!(word_at_position("hello world", 7), (6, 11));
        assert_eq!(word_at_position("hello world", 5), (5, 6)); // space
    }
}
