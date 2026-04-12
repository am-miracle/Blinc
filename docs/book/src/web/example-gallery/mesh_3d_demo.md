# 3D Mesh Demo — renders the Khronos glTF `DamagedHelmet` sample model

Demonstrates:
- `blinc_canvas_kit::SceneKit3D` — orbit camera + light rig wrapped
  around a `canvas` element, with drag/scroll input wired for free.
- `DrawContext::draw_mesh_data` — the direct-render mesh path. The
  canvas closure just calls `ctx.draw_mesh_data(&mesh, transform)`;
  everything behind that (camera capture, pending-mesh queue,
  GpuPaintContext → GpuRenderer dispatch, PBR shading) is plumbing.
- Inline glTF loading — no external `gltf` crate dep. The sample
  model has a fixed layout (single mesh, single primitive, packed
  f32 attributes at known bufferView offsets, u16 indices), so
  parsing is a handful of offset reads plus a `blinc_image::ImageData`
  call for the albedo texture.

<iframe
  src="../../examples/mesh_3d_demo/index.html"
  width="100%"
  height="560"
  loading="lazy"
  style="border:1px solid #45475a;border-radius:8px;background:#181825;"
  title="Blinc mesh_3d_demo example"
></iframe>

> **Tip:** Some demos are best viewed in a full browser window. Click "Open in a new tab" below for the full experience.

[Open in a new tab](../../examples/mesh_3d_demo/index.html) · [View source on GitHub](https://github.com/project-blinc/Blinc/blob/main/crates/blinc_app/examples/mesh_3d_demo.rs)
