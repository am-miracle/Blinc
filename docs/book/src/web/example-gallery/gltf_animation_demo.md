# Skeleton animation with glTF + `blinc_canvas_kit`.

Loads a rigged glTF scene (Sketchfab's buster_drone — 39 meshes,
92 nodes, one 25-second "Start_Liftoff" clip with 100 transform
channels), runs it through the `blinc_skeleton` poser each frame,
and renders the resulting transforms with `SceneKit3D`'s
immediate-mode PBR path. The clip drives node-level TRS channels
(no skins in this asset), so it exercises the pure-transform
animation pipeline end to end:

- `blinc_gltf::load_asset` — cross-platform asset loading
  (filesystem / APK / bundle / HTTP) through the
  `blinc_platform::assets` global loader, plus `KHR_materials_*`
  support and the full PBR metallic-roughness material block.
- `blinc_skeleton::densify_rotation_channels` — preprocesses the
  clip's rotation channels so fast rotors (blade rotation > 180°
  per keyframe, a frequent FBX-exporter trap) slerp smoothly
  instead of flipping direction every keyframe.
- `blinc_skeleton::animate_scene_nodes` — samples the clip at the
  current playback time and writes interpolated TRS values into
  `scene.nodes[*].transform`.
- `blinc_canvas_kit::SceneKit3D` — orbit camera, HDRI-lit
  environment, and `ctx.draw_mesh_data(...)` per primitive.
- `blinc_input::InputState` via
  `blinc_canvas_kit::SketchEvents::on_canvas_events` — polling
  keyboard + pointer state inside the render closure with a single
  `.capture_input(&state.input)` call on the scene's `Div`.

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
