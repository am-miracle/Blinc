# Backlog

## Known Issues

### TextInput cursor positioning after window resize
- **Description**: Cursor doesn't properly set position when clicked on the text_input after window resize
- **Location**: `crates/blinc_layout/src/widgets/text_input.rs`
- **Likely cause**: Layout bounds stored for scroll calculation become stale after resize, affecting click-to-cursor position mapping

### Web: text alignment and decoration width mismatch
- **Description**: Text centering (`text_center()`, `justify_center`) and text decorations (strikethrough, underline) are misaligned on the wasm32 web target. Centered text renders left-aligned; strikethrough/underline lines extend past the visible glyphs.
- **Location**: `crates/blinc_text/src/layout.rs` (alignment offset), `crates/blinc_app/src/context.rs:5114` (decoration width)
- **Likely cause**: The `FontTextMeasurer` produces different `measured_width` values on web because only Arial + FiraCode are loaded — not the full system font stack the desktop has. The text node's intrinsic size (from taffy) is computed from the measurer's width, but the GPU renderer shapes glyphs with the actual loaded font metrics. When these diverge, `(max_width - line_width) / 2.0` centering produces the wrong offset, and `decoration_width` (based on `measured_width`) overshoots the rendered glyph run. The root fix is ensuring the measurer and the GPU text shaper use identical font metrics — either by bundling the same fonts on web as the desktop expects, or by making the measurer always delegate to the same shaping engine the renderer uses.
