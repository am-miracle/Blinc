# Semantic @flow

Demonstrates the semantic step/chain/use system for @flow shaders.
Uses `step`, `chain`, and raw `node` syntax together to create a
layered noise visualization with pointer-reactive color ramping.

The fourth card ("Plasma") uses the `flow!` macro to define a flow
shader entirely in Rust — no CSS strings needed.

<iframe
  src="../../examples/semantic_flow_demo/index.html"
  width="100%"
  height="560"
  loading="lazy"
  style="border:1px solid #45475a;border-radius:8px;background:#181825;"
  title="Blinc semantic_flow_demo example"
></iframe>

> **Tip:** Some demos are best viewed in a full browser window. Click "Open in a new tab" below for the full experience.

[Open in a new tab](../../examples/semantic_flow_demo/index.html) · [View source on GitHub](https://github.com/project-blinc/Blinc/blob/main/crates/blinc_app/examples/semantic_flow_demo.rs)
