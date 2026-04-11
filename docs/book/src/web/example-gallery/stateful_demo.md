# Stateful API

This example demonstrates the new stateful::<S>() API with:
- `ctx.event()` - Access triggering event in state callbacks
- `ctx.use_signal()` - Scoped signals for local state
- `ctx.use_animated_value()` - Spring-animated values

<iframe
  src="../../examples/stateful_demo/index.html"
  width="100%"
  height="560"
  loading="lazy"
  style="border:1px solid #45475a;border-radius:8px;background:#181825;"
  title="Blinc stateful_demo example"
></iframe>

> **Tip:** Some demos are best viewed in a full browser window. Click "Open in a new tab" below for the full experience.

[Open in a new tab](../../examples/stateful_demo/index.html) · [View source on GitHub](https://github.com/project-blinc/Blinc/blob/main/crates/blinc_app/examples/stateful_demo.rs)
