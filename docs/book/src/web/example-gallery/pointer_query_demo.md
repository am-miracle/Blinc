# Pointer Query

Demonstrates the CSS-driven continuous pointer query system.
All pointer-reactive effects are defined purely in CSS using
`calc(env(pointer-*))` expressions — no Rust pointer reads needed.

The pointer query system binds cursor position to ANY numerical CSS property:
  opacity, corner-radius, border-width, rotate, and more.

CSS properties used:
  pointer-space: self;        — enables pointer tracking
  pointer-origin: center;     — coordinate origin
  pointer-range: -1.0 1.0;   — output range
  pointer-smoothing: 0.08;    — exponential smoothing
  opacity: calc(env(pointer-*));               — hover fade
  border-radius: calc(env(pointer-*));         — dynamic corners
  border-width: calc(env(pointer-*));          — dynamic borders
  rotate: calc(env(pointer-*));                — subtle rotation

<iframe
  src="../../examples/pointer_query_demo/index.html"
  width="100%"
  height="560"
  loading="lazy"
  style="border:1px solid #45475a;border-radius:8px;background:#181825;"
  title="Blinc pointer_query_demo example"
></iframe>

> **Tip:** Some demos are best viewed in a full browser window. Click "Open in a new tab" below for the full experience.

[Open in a new tab](../../examples/pointer_query_demo/index.html) · [View source on GitHub](https://github.com/project-blinc/Blinc/blob/main/crates/blinc_app/examples/pointer_query_demo.rs)
