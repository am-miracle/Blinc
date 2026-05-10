# Form Components

Components for building forms: inputs, checkboxes, selects, and more.

## Input

Text input field:

```rust
use blinc_cn::prelude::*;

input()
    .placeholder("Enter your name...")
    .value(name)
    .on_change(|value| set_name(value))
```

### Input Types

```rust
// Text (default)
input().placeholder("Name")

// Email
input().input_type("email").placeholder("Email")

// Password
input().input_type("password").placeholder("Password")

// Number
input().input_type("number").placeholder("Age")

// Search
input().input_type("search").placeholder("Search...")
```

### Input States

```rust
// Disabled
input().disabled(true)

// Read-only
input().readonly(true)

// With error
input().error(true)
```

## Textarea

Multi-line text input:

```rust
textarea()
    .placeholder("Enter description...")
    .rows(4)
    .value(description)
    .on_change(|value| set_description(value))
```

## Checkbox

```rust
checkbox()
    .checked(is_checked)
    .on_change(|checked| set_checked(checked))
    .child(label("Accept terms and conditions"))
```

### Indeterminate State

```rust
checkbox()
    .checked(some_checked)
    .indeterminate(some_checked && !all_checked)
    .on_change(|checked| toggle_all(checked))
    .child(label("Select all"))
```

## Switch

Toggle switch:

```rust
switch_()
    .checked(is_enabled)
    .on_change(|enabled| set_enabled(enabled))
```

### With Label

```rust
div()
    .flex_row()
    .items_center()
    .gap(8.0)
    .child(switch_().checked(dark_mode).on_change(|v| set_dark_mode(v)))
    .child(label("Dark mode"))
```

## Radio Group

```rust
radio_group()
    .value(selected)
    .on_change(|value| set_selected(value))
    .child(
        div().flex_col().gap(8.0)
            .child(radio_item("small").child(label("Small")))
            .child(radio_item("medium").child(label("Medium")))
            .child(radio_item("large").child(label("Large")))
    )
```

## Select

Dropdown selection:

```rust
select()
    .value(selected)
    .on_change(|value| set_selected(value))
    .child(select_trigger()
        .child(select_value().placeholder("Select option...")))
    .child(select_content()
        .child(select_item("opt1").child(text("Option 1")))
        .child(select_item("opt2").child(text("Option 2")))
        .child(select_item("opt3").child(text("Option 3"))))
```

### Grouped Options

```rust
select()
    .child(select_trigger().child(select_value()))
    .child(select_content()
        .child(select_group()
            .child(select_label("Fruits"))
            .child(select_item("apple").child(text("Apple")))
            .child(select_item("banana").child(text("Banana"))))
        .child(select_separator())
        .child(select_group()
            .child(select_label("Vegetables"))
            .child(select_item("carrot").child(text("Carrot")))
            .child(select_item("broccoli").child(text("Broccoli")))))
```

## Combobox

Searchable select with autocomplete:

```rust
combobox()
    .value(selected)
    .on_change(|value| set_selected(value))
    .child(combobox_trigger()
        .child(combobox_input().placeholder("Search...")))
    .child(combobox_content()
        .child(combobox_empty().child(text("No results found")))
        .child(combobox_item("react").child(text("React")))
        .child(combobox_item("vue").child(text("Vue")))
        .child(combobox_item("svelte").child(text("Svelte"))))
```

## Slider

Range slider:

```rust
slider()
    .value(volume)
    .min(0.0)
    .max(100.0)
    .step(1.0)
    .on_change(|value| set_volume(value))
```

### Range Slider

```rust
slider()
    .value_range(min_price, max_price)
    .min(0.0)
    .max(1000.0)
    .on_change_range(|min, max| {
        set_min_price(min);
        set_max_price(max);
    })
```

## Label

```rust
// Associated with input via for
label("Email").for_id("email-input")

// Direct child of input
checkbox()
    .child(label("Remember me"))
```

## Form Layout Example

```rust
div()
    .flex_col()
    .gap(24.0)
    .max_w(400.0)
    // Name field
    .child(
        div().flex_col().gap(4.0)
            .child(label("Name"))
            .child(input()
                .placeholder("John Doe")
                .value(&name)
                .on_change(|v| set_name(v)))
    )
    // Email field
    .child(
        div().flex_col().gap(4.0)
            .child(label("Email"))
            .child(input()
                .input_type("email")
                .placeholder("john@example.com")
                .value(&email)
                .on_change(|v| set_email(v)))
    )
    // Country select
    .child(
        div().flex_col().gap(4.0)
            .child(label("Country"))
            .child(select()
                .value(&country)
                .on_change(|v| set_country(v))
                .child(select_trigger().child(select_value()))
                .child(select_content()
                    .child(select_item("us").child(text("United States")))
                    .child(select_item("uk").child(text("United Kingdom")))
                    .child(select_item("ca").child(text("Canada")))))
    )
    // Terms checkbox
    .child(
        checkbox()
            .checked(accepted_terms)
            .on_change(|v| set_accepted_terms(v))
            .child(label("I accept the terms and conditions"))
    )
    // Submit button
    .child(
        button("Submit")
            .full_width(true)
            .disabled(!accepted_terms)
            .on_click(|| submit_form())
    )
```

## Validation

```rust
let email = use_state_keyed("form_email", || String::new());
let email_error = use_derived(|| {
    let value = email.get();
    if value.is_empty() {
        None
    } else if !value.contains('@') {
        Some("Invalid email address")
    } else {
        None
    }
});

div().flex_col().gap(4.0)
    .child(label("Email"))
    .child(input()
        .value(&email)
        .error(email_error.is_some())
        .on_change({
            let email = email.clone();
            move |v| email.set(v)
        }))
    .child(
        email_error.map(|err|
            text(err).size(12.0).color(Color::RED)
        )
    )
```
