# Data Display Components

Components for displaying data: tooltips, popovers, hover cards, charts, and trees.

## Tooltip

Brief information on hover:

```rust
use blinc_cn::prelude::*;

tooltip()
    .child(tooltip_trigger()
        .child(button("Hover me")))
    .child(tooltip_content()
        .child(text("This is a tooltip")))
```

### Tooltip Position

```rust
tooltip()
    .side(TooltipSide::Top)     // Top (default)
    .side(TooltipSide::Bottom)  // Bottom
    .side(TooltipSide::Left)    // Left
    .side(TooltipSide::Right)   // Right
```

### With Arrow

```rust
tooltip()
    .child(tooltip_trigger().child(icon(icons::INFO)))
    .child(tooltip_content()
        .with_arrow(true)
        .child(text("More information")))
```

## Hover Card

Rich content on hover:

```rust
hover_card()
    .child(hover_card_trigger()
        .child(text("@username").color(Color::BLUE)))
    .child(hover_card_content()
        .child(
            div().flex_row().gap(12.0)
                .child(avatar().src("user.jpg").size(AvatarSize::Lg))
                .child(
                    div().flex_col().gap(4.0)
                        .child(text("John Doe").weight(FontWeight::Bold))
                        .child(text("@johndoe").color(Color::GRAY))
                        .child(text("Software developer at Acme Inc."))
                )
        ))
```

## Popover

Interactive content in a popup:

```rust
let is_open = use_state_keyed("popover_open", || false);

popover()
    .open(is_open.clone())
    .on_open_change({
        let is_open = is_open.clone();
        move |open| is_open.set(open)
    })
    .child(popover_trigger()
        .child(button("Open Popover")))
    .child(popover_content()
        .child(
            div().flex_col().gap(12.0)
                .child(text("Settings").weight(FontWeight::Bold))
                .child(
                    div().flex_col().gap(8.0)
                        .child(
                            div().flex_row().justify_between()
                                .child(label("Notifications"))
                                .child(switch_())
                        )
                        .child(
                            div().flex_row().justify_between()
                                .child(label("Dark Mode"))
                                .child(switch_())
                        )
                )
        ))
```

## Chart

Data visualization:

```rust
chart()
    .chart_type(ChartType::Line)
    .data(&[
        DataPoint::new("Jan", 100.0),
        DataPoint::new("Feb", 150.0),
        DataPoint::new("Mar", 120.0),
        DataPoint::new("Apr", 180.0),
    ])
    .x_label("Month")
    .y_label("Sales")
```

### Chart Types

```rust
// Line chart
chart().chart_type(ChartType::Line)

// Bar chart
chart().chart_type(ChartType::Bar)

// Area chart
chart().chart_type(ChartType::Area)

// Pie chart
chart().chart_type(ChartType::Pie)

// Histogram
chart().chart_type(ChartType::Histogram)

// Scatter plot
chart().chart_type(ChartType::Scatter)
```

### Multi-Series

```rust
chart()
    .chart_type(ChartType::Line)
    .series("Revenue", &revenue_data, Color::BLUE)
    .series("Expenses", &expense_data, Color::RED)
    .series("Profit", &profit_data, Color::GREEN)
    .legend(true)
```

### Bar Chart

```rust
chart()
    .chart_type(ChartType::Bar)
    .data(&[
        DataPoint::new("Q1", 1200.0),
        DataPoint::new("Q2", 1500.0),
        DataPoint::new("Q3", 1800.0),
        DataPoint::new("Q4", 2100.0),
    ])
    .color(Color::BLUE)
    .show_values(true)
```

### Pie Chart

```rust
chart()
    .chart_type(ChartType::Pie)
    .data(&[
        DataPoint::new("Desktop", 45.0),
        DataPoint::new("Mobile", 35.0),
        DataPoint::new("Tablet", 20.0),
    ])
    .show_labels(true)
    .show_percentages(true)
```

## Tree

Hierarchical data display:

```rust
tree()
    .child(tree_item("root")
        .child(tree_item_content()
            .child(icon(icons::FOLDER))
            .child(text("Documents")))
        .child(tree_item("doc1")
            .child(tree_item_content()
                .child(icon(icons::FILE))
                .child(text("Report.pdf"))))
        .child(tree_item("doc2")
            .child(tree_item_content()
                .child(icon(icons::FILE))
                .child(text("Notes.txt")))))
```

### Expandable Tree

```rust
tree()
    .child(tree_item("projects")
        .expandable(true)
        .expanded(true)
        .child(tree_item_trigger()
            .child(icon(icons::FOLDER))
            .child(text("Projects")))
        .child(tree_item_content()
            .child(tree_item("project1")
                .child(tree_item_trigger()
                    .child(icon(icons::FOLDER))
                    .child(text("Project A")))
                .child(tree_item_content()
                    .child(tree_item("file1")
                        .child(tree_item_content()
                            .child(icon(icons::FILE))
                            .child(text("main.rs"))))))))
```

### Selectable Tree

```rust
let selected = use_state_keyed("tree_selected", || HashSet::new());

tree()
    .selectable(true)
    .selected(&selected)
    .on_select({
        let selected = selected.clone();
        move |ids| selected.set(ids)
    })
    .child(/* tree items */)
```

## Kbd

Keyboard shortcut display:

```rust
// Single key
kbd("⌘")

// Key combination
div().flex_row().gap(4.0)
    .child(kbd("⌘"))
    .child(kbd("K"))

// In context
div().flex_row().items_center().gap(8.0)
    .child(text("Search"))
    .child(
        div().flex_row().gap(2.0)
            .child(kbd("⌘"))
            .child(kbd("K"))
    )
```

## Examples

### User Profile Card

```rust
hover_card()
    .child(hover_card_trigger()
        .child(
            div().flex_row().items_center().gap(8.0)
                .child(avatar().src(&user.avatar).size(AvatarSize::Sm))
                .child(text(&user.name))
        ))
    .child(hover_card_content()
        .w(300.0)
        .child(
            div().flex_col().gap(12.0)
                .child(
                    div().flex_row().gap(12.0)
                        .child(avatar().src(&user.avatar).size(AvatarSize::Lg))
                        .child(
                            div().flex_col()
                                .child(text(&user.name).weight(FontWeight::Bold))
                                .child(text(&user.title).color(Color::GRAY))
                        )
                )
                .child(text(&user.bio))
                .child(
                    div().flex_row().gap(16.0)
                        .child(
                            div().flex_col()
                                .child(text(&user.followers.to_string()).weight(FontWeight::Bold))
                                .child(text("Followers").size(12.0).color(Color::GRAY))
                        )
                        .child(
                            div().flex_col()
                                .child(text(&user.following.to_string()).weight(FontWeight::Bold))
                                .child(text("Following").size(12.0).color(Color::GRAY))
                        )
                )
        ))
```

### Dashboard Chart

```rust
card()
    .child(card_header()
        .child(card_title("Revenue Overview"))
        .child(card_description("Monthly revenue for 2024")))
    .child(card_content()
        .child(
            chart()
                .chart_type(ChartType::Area)
                .h(300.0)
                .data(&monthly_revenue)
                .color(Color::rgba(0.2, 0.5, 1.0, 0.5))
                .stroke_color(Color::BLUE)
                .x_label("Month")
                .y_label("Revenue ($)")
                .grid(true)
        ))
```

### File Tree

```rust
tree()
    .child(tree_item("src")
        .expandable(true)
        .expanded(true)
        .child(tree_item_trigger()
            .child(icon(icons::FOLDER_OPEN))
            .child(text("src")))
        .child(tree_item_content()
            .child(tree_item("main")
                .on_click(|| open_file("src/main.rs"))
                .child(tree_item_content()
                    .child(icon(icons::FILE_CODE))
                    .child(text("main.rs"))))
            .child(tree_item("lib")
                .on_click(|| open_file("src/lib.rs"))
                .child(tree_item_content()
                    .child(icon(icons::FILE_CODE))
                    .child(text("lib.rs"))))))
```
