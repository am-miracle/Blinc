# `DrawContext::run_gpu_pass` end-to-end demo.

Shows the same pattern a user would reach for if they wanted to
retain their own GPU buffers for instanced rendering of a large
object scene: a `CustomRenderPass` implementation owns its
`wgpu::RenderPipeline`, its base-quad `wgpu::Buffer`, and its
per-instance `wgpu::Buffer`. All three are constructed once in
`initialize` and reused on every frame. The pass is wrapped with
`blinc_gpu::GpuPass::new(...)` so it can be passed through
`DrawContext::run_gpu_pass` from inside a regular `canvas()`
closure.

<iframe
  src="../../examples/gpu_pass_demo/index.html"
  width="100%"
  height="560"
  loading="lazy"
  style="border:1px solid #45475a;border-radius:8px;background:#181825;"
  title="Blinc gpu_pass_demo example"
></iframe>

> **Tip:** Some demos are best viewed in a full browser window. Click "Open in a new tab" below for the full experience.

[Open in a new tab](../../examples/gpu_pass_demo/index.html) · [View source on GitHub](https://github.com/project-blinc/Blinc/blob/main/examples/blinc_app_examples/examples/gpu_pass_demo.rs)
