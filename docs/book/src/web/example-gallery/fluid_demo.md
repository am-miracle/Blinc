# Fluid Surface

Combines `@flow` GPU shaders with `pointer-query` CSS-driven interaction.
A central card renders a pointer-reactive fluid shader, while surrounding
labels respond to cursor proximity via `calc(env(pointer-distance))`.

<iframe
  src="../../examples/fluid_demo/index.html"
  width="100%"
  height="560"
  loading="lazy"
  style="border:1px solid #45475a;border-radius:8px;background:#181825;"
  title="Blinc fluid_demo example"
></iframe>

[Open in a new tab](../../examples/fluid_demo/index.html) · [View source on GitHub](https://github.com/project-blinc/Blinc/blob/main/crates/blinc_app/examples/fluid_demo.rs)
