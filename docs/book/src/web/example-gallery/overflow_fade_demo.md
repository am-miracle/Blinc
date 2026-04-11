# Overflow Fade

Demonstrates the `overflow-fade` CSS property which applies smooth alpha
fading at overflow clip edges instead of hard clipping.

Supports:
- Uniform fade: `overflow-fade: 24px` (all edges)
- Vertical/horizontal: `overflow-fade: 24px 0px` (top/bottom only)
- Per-edge: `overflow-fade: 24px 0px 24px 0px`
- CSS transitions and @keyframes animation
- Works with scroll containers

<iframe
  src="../../examples/overflow_fade_demo/index.html"
  width="100%"
  height="560"
  loading="lazy"
  style="border:1px solid #45475a;border-radius:8px;background:#181825;"
  title="Blinc overflow_fade_demo example"
></iframe>

> **Tip:** Some demos are best viewed in a full browser window. Click "Open in a new tab" below for the full experience.

[Open in a new tab](../../examples/overflow_fade_demo/index.html) · [View source on GitHub](https://github.com/project-blinc/Blinc/blob/main/crates/blinc_app/examples/overflow_fade_demo.rs)
