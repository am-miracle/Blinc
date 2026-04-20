# Overview

`blinc_canvas_kit` is the layer that turns Blinc's raw GPU-drawing primitives into an authoring surface. It sits on top of the [`canvas()`](../widgets/canvas.md) element and provides four building blocks:

| API | When to reach for it |
|-----|----------------------|
| [**Sketches**](./sketches.md) | Per-frame immediate-mode drawing with persistent state — particle systems, generative art, live visualisations. |
| [**Players**](./players.md) | Time-based animation sources (Lottie, Rive, custom scene formats) driven by an external `t`. |
| [**CanvasKit**](./interactive.md) | Interactive 2D canvases: pan, zoom, hit-testing, pointer / drag / selection callbacks. |
| [**SceneKit3D**](./scenekit-3d.md) | 3D scene authoring: orbit camera, lights, environment maps, mesh draw, gizmos. |

All four share a common principle: the kit owns the per-frame render loop and whatever persistent state the drawing needs, then exposes a small trait (`Sketch`, `Player`) or a handle (`CanvasKit`, `SceneKit3D`) you feed into a `Div` tree. State survives UI rebuilds via `use_state_keyed`, so layout changes, hot reload, and route transitions don't reset counters, particle systems, camera poses, or asset uploads.

Reach for raw [`canvas()`](../widgets/canvas.md) only when you want a one-shot static render with no animation loop and no persistent state — for example, a chart drawn from a one-time computation.

## Import surface

Everything in this chapter lives under the `prelude`:

```rust
use blinc_canvas_kit::prelude::*;
```

The prelude re-exports `Sketch`, `SketchContext`, `Painter2D`, `Player`, `sketch`, `CanvasKit`, `SceneKit3D`, `OrbitCamera`, and the relevant event types. Explicit imports work too — everything is in `blinc_canvas_kit` or `blinc_canvas_kit::sketch`.

## Animation cadence

Every kit runs its draw callback at the host's redraw cadence (typically vsync: 60 / 120 Hz) by requesting another frame at the end of each render. There is no opt-out from inside a `Sketch` — if you want static output, use plain `canvas()` directly. For deterministic playback (recording frames, scrubbing), drive a [`Player`](./players.md) from outside a sketch and pass synthesised `t` values.
