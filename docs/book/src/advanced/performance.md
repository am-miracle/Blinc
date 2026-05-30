# Performance Tips

Blinc is designed for high performance, but a few guidelines keep your UI on
the **compositor fast path**: the codepath where a frame patches GPU
primitives in place and re-dispatches only what changed, instead of
re-walking and re-rasterising the tree.

For the architecture behind these tips, see [GPU
Rendering](../architecture/gpu-rendering.md).

## Stay on the fast path

The frame loop tries to short-circuit Phase 4 (paint) when nothing
expensive changed since the last frame. The gates that have to hold:

- No structural rebuild this frame.
- No layout-affecting change (width, padding, gap, flex direction, …).
- No scroll physics, bounds animation, or new overlay.
- The render cache is valid.

If all gates hold, the compositor patches motion-bound and CSS-animated
primitives in place and the GPU pass scissors a small damage rect. Tree
size becomes irrelevant; work scales with what *changed*.

Anything that invalidates a gate forces a full walker rerun.

## Use Stateful for Visual States

**Do:** Use `stateful::<S>()` for hover, press, and focus effects:

```rust
use blinc_layout::stateful::stateful;

fn hover_button() -> impl ElementBuilder {
    stateful::<ButtonState>()
        .px(16.0)
        .py(8.0)
        .rounded(8.0)
        .on_state(|ctx| {
            let bg = match ctx.state() {
                ButtonState::Idle => Color::RED,
                ButtonState::Hovered => Color::BLUE,
                _ => Color::RED,
            };
            div().bg(bg)
        })
        .child(text("Hover me").color(Color::WHITE))
}
```

**Don't:** Reach for a top-level `use_signal` to switch a visual property
when CSS, `Stateful`, or signal *binding* (`bg(my_signal)`) would do the
job:

```rust
// AVOID: flipping this signal triggers a subtree rebuild because the
// branched expression rebuilds the element. Even when the compositor can
// patch the colour, the rebuild itself isn't free.
let is_hovered = ctx.use_signal(false);
div()
    .on_hover_enter(move |_| ctx.set(is_hovered, true))
    .on_hover_leave(move |_| ctx.set(is_hovered, false))
    .bg(if ctx.get(is_hovered).unwrap_or(false) {
        Color::BLUE
    } else {
        Color::RED
    })
```

Order of preference for visual state:

1. **CSS `:hover` / `:focus` / `:active`.** Patched in place by
   `apply_css_deltas` with zero Rust overhead.
2. **`stateful::<S>()`.** Element-scoped FSM, only re-renders the affected
   subtree on transition.
3. **Direct signal binding** (`.bg(color_signal)`). Fast-path patch via
   `apply_binding_deltas`; no rebuild at all.
4. **Branched `if/else` driven by a signal read.** Last resort; this
   rebuilds the reading component's subtree.

## Minimize Signal Updates

A signal update re-runs the component that reads it, rebuilding *that
component's subtree* (not the whole UI). Batch related updates so several
mutations share a single rebuild:

```rust
// Good: single subtree rebuild
ctx.batch(|g| {
    g.set(x, 10);
    g.set(y, 20);
    g.set(z, 30);
});

// Avoid: three subtree rebuilds in succession
ctx.set(x, 10);
ctx.set(y, 20);
ctx.set(z, 30);
```

Note that signal *binding* (`.bg(my_signal)`, `.w(my_signal)`, etc.) is
cheaper still: the compositor patches the GPU primitive directly without
any subtree rebuild.

## Use Keyed State Appropriately

Keyed state persists across rebuilds. Use it for:

- Form input values
- Toggle states
- Selected items

Don't overuse; each key adds memory overhead.

## Efficient List Rendering

For large lists, consider:

1. **Virtualization.** Only render visible items.
2. **Stable keys.** Use consistent identifiers for list items.
3. **Memoization.** Cache expensive computations.

```rust
// For very long lists, wrap in scroll and limit rendered items
scroll()
    .h(500.0)
    .child(
        div()
            .flex_col()
            .child(
                visible_items.iter().map(|item| render_item(item))
            )
    )
```

## Canvas Optimization

For custom drawing:

1. **Minimize state reads.** Read animated values once, not per-shape.
2. **Use transforms.** Push/pop transforms instead of recalculating positions.
3. **Batch similar draws.** Group shapes by color/brush.

```rust
canvas(move |ctx, bounds| {
    // Read once
    let angle = timeline.lock().unwrap().get(entry_id).unwrap_or(0.0);

    // Use transform for rotation
    ctx.push_transform(Transform::rotate(angle));
    // ... draw ...
    ctx.pop_transform();
})
```

## Animation Performance

1. **Use appropriate spring stiffness.** Stiffer springs settle faster.
2. **Limit simultaneous animations.** Too many can cause jank.
3. **Use timelines for loops.** More efficient than many spring values.

```rust
// Good: single timeline with multiple entries
let timeline = ctx.use_animated_timeline();
let (x, y, scale) = timeline.lock().unwrap().configure(|t| {
    (t.add(0, 1000, 0.0, 100.0),
     t.add(0, 1000, 0.0, 100.0),
     t.add(0, 500, 1.0, 1.5))
});
```

## Memory Management

1. **Clone Arc, not data.** Use `Arc::clone()` for shared state.
2. **Drop unused state.** Clean up keyed state when no longer needed.
3. **Avoid closures capturing large data.** Clone only what's needed.

```rust
// Good: clone the Arc, not the data
let data = Arc::clone(&shared_data);

// Avoid: captures entire struct
let large_struct = expensive_struct.clone();
div().on_click(move |_| use_struct(&large_struct))
```

## Lazy Loading for Images

For applications with many images (galleries, feeds, chat), use lazy
loading to defer loading until images are visible:

```rust
// Images in a scrollable gallery
scroll()
    .h(600.0)
    .child(
        div()
            .flex_row()
            .flex_wrap()
            .gap(8.0)
            .child(
                image_urls.iter().map(|url| {
                    img(*url)
                        .lazy()  // Only loads when scrolled into view
                        .placeholder_color(Color::rgba(0.2, 0.2, 0.2, 1.0))
                        .w(150.0)
                        .h(150.0)
                        .cover()
                })
            )
    )
```

Benefits:

- **Reduced initial memory.** Only visible images are loaded.
- **Faster startup.** No waiting for off-screen images.
- **Automatic cleanup.** LRU cache evicts old images.

Emoji images (`emoji()` and `emoji_sized()`) are automatically lazy-loaded.
The ~180MB system emoji font is only loaded when emoji characters actually
appear on screen.

## Cull off-screen content in long scrolls

For scroll containers with hundreds or thousands of children, opt into
viewport culling:

```rust
scroll()
    .h(600.0)
    .viewport_cull(true)
    .child(
        items.iter().map(|item| render_item(item))
    )
```

With `viewport_cull(true)`, children outside the container's bounds (plus
a 200 px overscan band) emit **zero primitives**. They don't enter the
static batch, the dynamic batch, or any damage rect. Animations on
off-screen children still tick on the background thread but don't request
the next frame.

Fixed and sticky children opt out automatically.

## Avoid the slow path

These changes trip the compositor fast path and require a full walker run:

| Trigger                                                 | Why                                       |
|---------------------------------------------------------|-------------------------------------------|
| Animating `width`, `height`, `padding`, `gap`, `margin` | Triggers Taffy re-layout                  |
| Animating `clip-path` inset / circle radius             | Damage-rect path can't patch in place     |
| Animating `filter: blur(…)` or `backdrop-filter`        | Out of scope for `apply_css_deltas`       |
| Scroll physics actively moving                          | Cache invalidated by scroll offset        |
| Overlay / dialog / sheet open or close                  | Layer composition changes                 |
| Stateful flips that change child structure              | Subtree rebuild                           |

Prefer animating `transform: translate / scale / rotate`, `opacity`,
`background-color`, `border-*`, `corner-radius`, `box-shadow`, and 3D
rotations. These all patch in place.

## Watch for canvas closures

Canvas draw closures are called every frame by design; that's the
contract. A canvas inside a tree means that tree can't take the
cache-blit-only fast path because the closure has to run to produce its
primitives.

If your canvas's draw output doesn't actually change every frame, gate the
expensive work inside the closure on a signal you control rather than
recomputing geometry on every paint.

## Debugging Performance

Enable tracing to identify bottlenecks:

```rust
tracing_subscriber::fmt()
    .with_env_filter("blinc_layout=debug")
    .init();
```

Look for:

- `did_rebuild=true` or `needs_relayout=true` in `frame_timing` traces.
  These mean the fast path was skipped.
- Frequent subtree rebuilds.
- Long frame times.
- Excessive state updates.

## Summary

| Do | Don't |
|----|-------|
| Prefer CSS `:hover` / `:focus` → `Stateful` → signal binding | Branch on a signal read to flip a visual property |
| Animate `transform`, `opacity`, colours, radii, shadow | Animate `width` / `height` / `padding` / `gap` |
| Opt long scrolls into `viewport_cull(true)` | Render thousands of children unconditionally |
| Batch signal updates | Update signals one at a time |
| Use `Arc::clone()` | Clone large data into closures |
| Use timelines for loops | Create many spring values |
| Read animated values once | Read repeatedly in draw loops |
| Gate expensive canvas work on a signal | Recompute geometry every frame inside a canvas closure |
