# Backlog

## Known Issues

### TextInput cursor positioning after window resize
- **Description**: Cursor doesn't properly set position when clicked on the text_input after window resize
- **Location**: `crates/blinc_layout/src/widgets/text_input.rs`
- **Likely cause**: Layout bounds stored for scroll calculation become stale after resize, affecting click-to-cursor position mapping

### Mobile browser rendering issues
- **iPhone 12 Pro Max (Safari/iOS 18+)**: Web demos render and run — WebGPU works. Slight jitter on stateful element subtree updates (accordion open, checkbox toggle, etc.). Likely caused by the full subtree rebuild + relayout path being too expensive for a single 16ms frame on mobile GPU. Potential fix: batch visual-only prop updates separately from structural rebuilds, or defer relayout to the next frame.
- **Google Pixel (Chrome Android)**: Renders text only; scrolling produces infinite artifacts everywhere. Likely a Vulkan/WebGPU driver issue on Pixel's GPU (Adreno or Mali depending on model). Could also be a surface configuration mismatch (sRGB vs non-sRGB texture format, or present mode incompatibility). Needs investigation with Chrome's `chrome://gpu` diagnostics and the wgpu validation layer enabled.
- **Priority**: P2 — mobile web is a bonus, not a primary target yet. Desktop Chrome/Edge/Firefox/Safari are the primary web targets.

### Web: text alignment and decoration width mismatch — RESOLVED
- **Resolution**: Fixed in `990937c3`. Root cause was the font registry's negative cache: `preload_generic_styles` ran before font bytes were loaded, cached "not found" for every generic family+weight combo, and subsequent preloads hit the stale cache. Fix: `invalidate_generic_cache()` clears negative entries before re-running preload after font loads. Also added JetBrains Mono + Fira Code to the monospace fallback name list in the font registry. Once the correct fonts resolve, the measurer and renderer use identical metrics and centering/decorations align.
