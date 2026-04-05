# Virtualized List

The `virtual_list` widget efficiently renders large datasets by only creating elements for visible items. Use it for lists with thousands of items.

## Basic Usage

```rust
use blinc_layout::widgets::virtual_list::virtual_list;

let items: Vec<String> = (0..10_000)
    .map(|i| format!("Item {}", i))
    .collect();

virtual_list(items.len(), 32.0, move |index| {
    div()
        .h(32.0)
        .w_full()
        .padding_x_px(12.0)
        .flex_row()
        .items_center()
        .child(
            text(&items[index])
                .size(14.0)
                .color(Color::WHITE)
        )
})
.w_full()
.h(400.0)
.into_div()
```

## Parameters

| Parameter | Description |
|-----------|-------------|
| `item_count` | Total number of items in the list |
| `item_height` | Fixed height per item in pixels |
| `builder` | Closure that creates a `Div` for each visible index |

## Configuration

```rust
virtual_list(count, 32.0, builder)
    .w_full()              // Width
    .h(400.0)              // Viewport height
    .bg(Color::BLACK)      // Background color
    .rounded(8.0)          // Corner radius
    .overscan(5)           // Extra items above/below viewport (default: 3)
    .into_div()            // Convert to Div for use as child
```

## How It Works

1. Only items visible in the viewport (plus overscan buffer) are created as elements
2. A bottom spacer maintains the correct scroll content height
3. The list wraps in a `scroll()` container with momentum physics
4. Items are re-created when the scroll position changes

## Limitations

- **Fixed item height**: All items must have the same height
- **Initial render only**: The current implementation renders the initial visible set; scroll-based re-virtualization requires Stateful integration (future work)
