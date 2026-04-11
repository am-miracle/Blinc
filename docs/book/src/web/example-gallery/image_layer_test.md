# Image Layer Test

Tests the rendering order of images vs primitives (paths, backgrounds).
This helps debug z-order issues where images may render above/below other elements.

**Solution for rendering elements ON TOP of images:**
Use `.foreground()` on any element that needs to render above images.
The render order is: Background primitives → Images → Foreground primitives

<iframe
  src="../../examples/image_layer_test/index.html"
  width="100%"
  height="560"
  loading="lazy"
  style="border:1px solid #45475a;border-radius:8px;background:#181825;"
  title="Blinc image_layer_test example"
></iframe>

> **Tip:** Some demos are best viewed in a full browser window. Click "Open in a new tab" below for the full experience.

[Open in a new tab](../../examples/image_layer_test/index.html) · [View source on GitHub](https://github.com/project-blinc/Blinc/blob/main/crates/blinc_app/examples/image_layer_test.rs)
