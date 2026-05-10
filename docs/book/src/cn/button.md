# Button

Buttons trigger actions or events.

## Basic Usage

```rust
use blinc_cn::prelude::*;

button("Click me")
    .on_click(|| println!("Clicked!"))
```

## Variants

Buttons come in several visual variants:

```rust
// Primary (default) - Main actions
button("Save").variant(ButtonVariant::Primary)

// Secondary - Alternative actions
button("Cancel").variant(ButtonVariant::Secondary)

// Destructive - Dangerous actions
button("Delete").variant(ButtonVariant::Destructive)

// Outline - Bordered style
button("Edit").variant(ButtonVariant::Outline)

// Ghost - Minimal style
button("More").variant(ButtonVariant::Ghost)

// Link - Looks like a link
button("Learn more").variant(ButtonVariant::Link)
```

## Sizes

```rust
// Small
button("Small").size(ButtonSize::Sm)

// Default
button("Default").size(ButtonSize::Default)

// Large
button("Large").size(ButtonSize::Lg)

// Icon only (square)
button("").size(ButtonSize::Icon).icon(icons::SETTINGS)
```

## With Icons

```rust
use blinc_icons::icons;

// Icon before text
button("Settings")
    .icon(icons::SETTINGS)

// Icon after text
button("Next")
    .icon_right(icons::ARROW_RIGHT)

// Icon only
button("")
    .size(ButtonSize::Icon)
    .icon(icons::PLUS)
```

## States

```rust
// Disabled
button("Disabled")
    .disabled(true)

// Loading
button("Saving...")
    .loading(true)

// Full width
button("Submit")
    .full_width(true)
```

## Event Handling

```rust
button("Submit")
    .on_click(|| {
        // Handle click
        submit_form();
    })
    .on_hover(|hovering| {
        // Handle hover state
        if hovering {
            show_tooltip();
        }
    })
```

## Button Groups

```rust
div()
    .flex_row()
    .gap(8.0)
    .child(button("Save").variant(ButtonVariant::Primary))
    .child(button("Cancel").variant(ButtonVariant::Outline))
```

## Examples

### Form Submit Button

```rust
button("Create Account")
    .variant(ButtonVariant::Primary)
    .size(ButtonSize::Lg)
    .full_width(true)
    .on_click(|| handle_submit())
```

### Icon Button

```rust
button("")
    .size(ButtonSize::Icon)
    .variant(ButtonVariant::Ghost)
    .icon(icons::X)
    .on_click(|| close_dialog())
```

### Loading Button

```rust
let is_loading = use_state_keyed("button_loading", || false);

button(if is_loading.get() { "Saving..." } else { "Save" })
    .loading(is_loading.get())
    .disabled(is_loading.get())
    .on_click({
        let is_loading = is_loading.clone();
        move |_| {
            is_loading.set(true);
            save_data().then({
                let is_loading = is_loading.clone();
                move || is_loading.set(false)
            });
        }
    })
```

## API Reference

### Props

| Prop | Type | Default | Description |
|------|------|---------|-------------|
| `variant` | `ButtonVariant` | `Primary` | Visual style |
| `size` | `ButtonSize` | `Default` | Button size |
| `disabled` | `bool` | `false` | Disable interaction |
| `loading` | `bool` | `false` | Show loading state |
| `full_width` | `bool` | `false` | Expand to full width |
| `icon` | `&str` | `None` | Icon before text |
| `icon_right` | `&str` | `None` | Icon after text |

### Events

| Event | Type | Description |
|-------|------|-------------|
| `on_click` | `Fn()` | Called when clicked |
| `on_hover` | `Fn(bool)` | Called on hover change |
