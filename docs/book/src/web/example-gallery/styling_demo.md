# Unified Styling API

Demonstrates all styling approaches in Blinc:
- `css!` macro: CSS-like syntax with hyphenated property names
- `style!` macro: Rust-friendly syntax with underscored names
- `ElementStyle` builder: Programmatic style construction
- CSS Parser: Runtime CSS string parsing

All approaches produce `ElementStyle` - a unified schema for visual properties.

<iframe
  src="../../examples/styling_demo/index.html"
  width="100%"
  height="560"
  loading="lazy"
  style="border:1px solid #45475a;border-radius:8px;background:#181825;"
  title="Blinc styling_demo example"
></iframe>

[Open in a new tab](../../examples/styling_demo/index.html) · [View source on GitHub](https://github.com/project-blinc/Blinc/blob/main/crates/blinc_app/examples/styling_demo.rs)
