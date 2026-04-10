# Backlog

## Known Issues

### TextInput cursor positioning after window resize
- **Description**: Cursor doesn't properly set position when clicked on the text_input after window resize
- **Location**: `crates/blinc_layout/src/widgets/text_input.rs`
- **Likely cause**: Layout bounds stored for scroll calculation become stale after resize, affecting click-to-cursor position mapping

### Web: text alignment and decoration width mismatch — RESOLVED
- **Resolution**: Fixed in `990937c3`. Root cause was the font registry's negative cache: `preload_generic_styles` ran before font bytes were loaded, cached "not found" for every generic family+weight combo, and subsequent preloads hit the stale cache. Fix: `invalidate_generic_cache()` clears negative entries before re-running preload after font loads. Also added JetBrains Mono + Fira Code to the monospace fallback name list in the font registry. Once the correct fonts resolve, the measurer and renderer use identical metrics and centering/decorations align.
