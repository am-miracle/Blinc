# Smoke test for the downstream-packages canvas stack.

Exercises, end-to-end:

- `blinc_gltf`: load a real glTF asset (buster_drone) — 39 meshes,
  92 nodes, 1 animation with 100 transform channels.
- `blinc_skeleton`: sample the animation clip into node transforms
  each frame (no skins in this asset — pure transform animation).
- `blinc_canvas_kit::SceneKit3D`: orbit camera, HDRI lighting, PBR
  rendering via the immediate-mode `draw_mesh_data` path.
- `blinc_canvas_kit::SketchEvents::on_canvas_events`: one-call event
  forwarding from the scene's `Div` into…
- `blinc_input::InputState`: polling keys inside the render closure.

Controls:
- **Drag**: orbit camera (wired by `SceneKit3D`)
- **Scroll**: zoom in / out
- **Space**: pause / resume the animation
- **R**: reset clip time to 0
- **Left / Right**: scrub ±1 frame while held

<iframe
  src="../../examples/gltf_animation_demo/index.html"
  width="100%"
  height="560"
  loading="lazy"
  style="border:1px solid #45475a;border-radius:8px;background:#181825;"
  title="Blinc gltf_animation_demo example"
></iframe>

> **Tip:** Some demos are best viewed in a full browser window. Click "Open in a new tab" below for the full experience.

[Open in a new tab](../../examples/gltf_animation_demo/index.html) · [View source on GitHub](https://github.com/project-blinc/Blinc/blob/main/examples/blinc_app_examples/examples/gltf_animation_demo.rs)
