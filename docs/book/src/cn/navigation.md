# Navigation Components

Components for navigation: tabs, menus, breadcrumbs, and sidebars.

## Tabs

Organize content into tabbed sections:

```rust
use blinc_cn::prelude::*;

tabs()
    .value(active_tab)
    .on_change(|tab| set_active_tab(tab))
    .child(tabs_list()
        .child(tabs_trigger("account").child(text("Account")))
        .child(tabs_trigger("password").child(text("Password")))
        .child(tabs_trigger("settings").child(text("Settings"))))
    .child(tabs_content("account")
        .child(text("Account settings...")))
    .child(tabs_content("password")
        .child(text("Password settings...")))
    .child(tabs_content("settings")
        .child(text("Other settings...")))
```

## Dropdown Menu

```rust
dropdown_menu()
    .child(dropdown_menu_trigger()
        .child(button("Options").icon_right(icons::CHEVRON_DOWN)))
    .child(dropdown_menu_content()
        .child(dropdown_menu_label("Actions"))
        .child(dropdown_menu_item("edit")
            .child(icon(icons::EDIT))
            .child(text("Edit"))
            .on_click(|| edit_item()))
        .child(dropdown_menu_item("duplicate")
            .child(icon(icons::COPY))
            .child(text("Duplicate")))
        .child(dropdown_menu_separator())
        .child(dropdown_menu_item("delete")
            .child(icon(icons::TRASH))
            .child(text("Delete"))
            .variant(MenuItemVariant::Destructive)))
```

### With Keyboard Shortcuts

```rust
dropdown_menu_item("save")
    .child(icon(icons::SAVE))
    .child(text("Save"))
    .child(dropdown_menu_shortcut("⌘S"))
```

### Submenu

```rust
dropdown_menu_content()
    .child(dropdown_menu_item("new").child(text("New")))
    .child(dropdown_menu_sub()
        .child(dropdown_menu_sub_trigger()
            .child(text("Share")))
        .child(dropdown_menu_sub_content()
            .child(dropdown_menu_item("email").child(text("Email")))
            .child(dropdown_menu_item("link").child(text("Copy Link")))))
```

## Context Menu

Right-click menu:

```rust
context_menu()
    .child(context_menu_trigger()
        .child(div().w(200.0).h(150.0).bg(Color::GRAY)
            .child(text("Right-click me"))))
    .child(context_menu_content()
        .child(context_menu_item("cut").child(text("Cut")))
        .child(context_menu_item("copy").child(text("Copy")))
        .child(context_menu_item("paste").child(text("Paste")))
        .child(context_menu_separator())
        .child(context_menu_item("delete").child(text("Delete"))))
```

## Menubar

Application menu bar:

```rust
menubar()
    .child(menubar_menu()
        .child(menubar_trigger().child(text("File")))
        .child(menubar_content()
            .child(menubar_item("new").child(text("New File")))
            .child(menubar_item("open").child(text("Open...")))
            .child(menubar_separator())
            .child(menubar_item("save").child(text("Save")))
            .child(menubar_item("save-as").child(text("Save As...")))))
    .child(menubar_menu()
        .child(menubar_trigger().child(text("Edit")))
        .child(menubar_content()
            .child(menubar_item("undo").child(text("Undo")))
            .child(menubar_item("redo").child(text("Redo")))))
```

## Breadcrumb

Navigation path:

```rust
breadcrumb()
    .child(breadcrumb_list()
        .child(breadcrumb_item()
            .child(breadcrumb_link("Home").href("/")))
        .child(breadcrumb_separator())
        .child(breadcrumb_item()
            .child(breadcrumb_link("Products").href("/products")))
        .child(breadcrumb_separator())
        .child(breadcrumb_item()
            .child(breadcrumb_page("Details"))))  // Current page (not a link)
```

### With Ellipsis

```rust
breadcrumb()
    .child(breadcrumb_list()
        .child(breadcrumb_item().child(breadcrumb_link("Home")))
        .child(breadcrumb_separator())
        .child(breadcrumb_ellipsis())  // Collapsed items
        .child(breadcrumb_separator())
        .child(breadcrumb_item().child(breadcrumb_link("Category")))
        .child(breadcrumb_separator())
        .child(breadcrumb_item().child(breadcrumb_page("Current"))))
```

## Pagination

```rust
pagination()
    .total(100)
    .page_size(10)
    .current_page(current_page)
    .on_page_change(|page| set_current_page(page))
    .child(pagination_content()
        .child(pagination_previous())
        .child(pagination_items())
        .child(pagination_next()))
```

## Sidebar

Application sidebar navigation:

```rust
sidebar()
    .child(sidebar_header()
        .child(
            div().flex_row().items_center().gap(8.0)
                .child(icon(icons::BOX).size(24.0))
                .child(text("My App").weight(FontWeight::Bold))
        ))
    .child(sidebar_content()
        .child(sidebar_group()
            .child(sidebar_group_label("Main"))
            .child(sidebar_menu()
                .child(sidebar_menu_item("dashboard")
                    .icon(icons::HOME)
                    .active(current_route == "dashboard")
                    .on_click(|| navigate("/dashboard"))
                    .child(text("Dashboard")))
                .child(sidebar_menu_item("projects")
                    .icon(icons::FOLDER)
                    .on_click(|| navigate("/projects"))
                    .child(text("Projects")))
                .child(sidebar_menu_item("tasks")
                    .icon(icons::CHECK_SQUARE)
                    .on_click(|| navigate("/tasks"))
                    .child(text("Tasks")))))
        .child(sidebar_group()
            .child(sidebar_group_label("Settings"))
            .child(sidebar_menu()
                .child(sidebar_menu_item("settings")
                    .icon(icons::SETTINGS)
                    .on_click(|| navigate("/settings"))
                    .child(text("Settings")))
                .child(sidebar_menu_item("help")
                    .icon(icons::HELP_CIRCLE)
                    .on_click(|| navigate("/help"))
                    .child(text("Help"))))))
    .child(sidebar_footer()
        .child(
            div().flex_row().items_center().gap(8.0)
                .child(avatar().src("user.jpg").size(AvatarSize::Sm))
                .child(text("John Doe"))
        ))
```

### Collapsible Sidebar

```rust
let is_collapsed = use_state_keyed("sidebar_collapsed", || false);

sidebar()
    .collapsed(is_collapsed.clone())
    .child(sidebar_header()
        .child(sidebar_trigger()
            .on_click({
                let is_collapsed = is_collapsed.clone();
                move |_| is_collapsed.set(!is_collapsed.get())
            })))
    .child(/* rest of sidebar */)
```

## Navigation Menu

Horizontal navigation with dropdowns:

```rust
navigation_menu()
    .child(navigation_menu_list()
        .child(navigation_menu_item()
            .child(navigation_menu_trigger().child(text("Products")))
            .child(navigation_menu_content()
                .child(navigation_menu_link("analytics").child(text("Analytics")))
                .child(navigation_menu_link("reports").child(text("Reports")))))
        .child(navigation_menu_item()
            .child(navigation_menu_link("pricing").child(text("Pricing"))))
        .child(navigation_menu_item()
            .child(navigation_menu_link("about").child(text("About")))))
```
