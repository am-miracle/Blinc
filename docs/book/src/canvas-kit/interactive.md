# CanvasKit: Interactive Canvases

When a sketch needs hit-testing, pointer / keyboard events, pan, or zoom, pair it with `CanvasKit`. Register hit regions by ID inside `draw`, install callbacks once on the kit itself, and wire pan/zoom automatically.

## Hit regions

```rust
use blinc_canvas_kit::prelude::*;

struct Scene {
    kit: CanvasKit,
    hovered: Option<String>,
}

impl Sketch for Scene {
    fn setup(&mut self, _ctx: &mut SketchContext<'_>) {
        self.kit.on_element_click(|ev| {
            println!("clicked region: {}", ev.id);
        });

        // Note: hover callbacks run on pointer enter / leave. Persist the
        // hovered id into sketch state to drive per-frame highlight logic.
        let hovered = /* reference-counted handle back into sketch state */;
        self.kit.on_element_hover(move |ev| {
            // update `hovered` here
        });
    }

    fn draw(&mut self, ctx: &mut SketchContext<'_>, _t: f32, _dt: f32) {
        // Register hit regions each frame (IDs flow into the callbacks).
        self.kit.hit_rect("box-a", Rect::new(50.0, 50.0, 100.0, 100.0));
        self.kit.hit_rect("box-b", Rect::new(200.0, 50.0, 100.0, 100.0));

        // Draw — pick color based on whatever the hover callback stashed.
    }
}
```

## Callbacks

All installed on the `CanvasKit` once (typically in `setup` or at construction):

| Callback | Fires on |
|----------|----------|
| `on_element_click(cb)` | Click on a hit region |
| `on_element_hover(cb)` | Enter / leave a hit region |
| `on_element_drag(cb)` | Drag a hit region |
| `on_element_drag_end(cb)` | Drag release |
| `on_selection_change(cb)` | Multi-select / marquee changes |

Each callback receives a `CanvasEvent` carrying the region `id`, the content-space pointer position, and the triggering `EventContext`.

## Built-in gestures

`CanvasKit` wires the following automatically once it's in a sketch:

- **Pan** — drag on empty background
- **Zoom** — scroll wheel (content-space)
- **Marquee select** — drag from empty background with shift / modifier
- **Grid snap** — configurable via `kit.snap_rect(rect)` / `kit.snap_point(p)`

Tune sensitivity via the builder methods before mounting:

```rust
let kit = CanvasKit::new("scene")
    .with_drag_sensitivity(1.0)
    .with_zoom_sensitivity(0.1)
    .with_momentum_decay(0.92);
```

## Content-space vs screen-space

`hit_rect` and `hit_test` operate on **content-space** coordinates (pre-pan, pre-zoom). The kit transforms pointer events and render bounds into content space before dispatching — you author as if the canvas were infinite and at 1:1 zoom. Use `kit.is_visible(rect)` to cull content-space rects against the current viewport before drawing expensive primitives.

## Full example

See the [Canvas Kit Interactive example](../web/example-gallery/canvas_kit_demo.md) for a complete walkthrough with pan, zoom, hover feedback, drag, marquee select, and a HUD overlay.

## Bundled input routing

To pipe every event the kit receives (pointer, scroll, key) into a single callback — useful for bridging into `blinc_input::InputState::record` or custom routing — attach `.on_canvas_events(|e| ...)` to the `Div` returned by `sketch()`:

```rust
use blinc_canvas_kit::sketch::SketchEvents;

sketch("scene", Scene { kit: CanvasKit::new("scene") })
    .on_canvas_events(|ev| input.record(ev))
```
