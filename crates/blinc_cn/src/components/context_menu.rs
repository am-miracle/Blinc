//! Context Menu component for right-click menus
//!
//! A themed context menu that appears at a specific position (usually mouse coordinates).
//! Uses the overlay system for proper positioning and dismissal.
//!
//! # Example
//!
//! ```ignore
//! use blinc_cn::prelude::*;
//!
//! fn build_ui(ctx: &WindowedContext) -> impl ElementBuilder {
//!     div()
//!         .w(400.0)
//!         .h(300.0)
//!         .bg(theme.color(ColorToken::Surface))
//!         .on_click(|event_ctx| {
//!             // Use mouse_x/mouse_y from EventContext for absolute screen position
//!             cn::context_menu()
//!                 .at(event_ctx.mouse_x, event_ctx.mouse_y)
//!                 .item("Cut", || println!("Cut"))
//!                 .item("Copy", || println!("Copy"))
//!                 .item("Paste", || println!("Paste"))
//!                 .separator()
//!                 .item("Delete", || println!("Delete"))
//!                 .show();
//!         })
//! }
//!
//! // With keyboard shortcuts displayed
//! cn::context_menu()
//!     .at(x, y)
//!     .item_with_shortcut("Cut", "Ctrl+X", || {})
//!     .item_with_shortcut("Copy", "Ctrl+C", || {})
//!     .item_with_shortcut("Paste", "Ctrl+V", || {})
//!
//! // Disabled items
//! cn::context_menu()
//!     .at(x, y)
//!     .item("Undo", || {})
//!     .item_disabled("Redo")  // No action available
//!
//! // Submenus (nested menus)
//! cn::context_menu()
//!     .at(x, y)
//!     .item("Open", || {})
//!     .submenu("Recent Files", |sub| {
//!         sub.item("file1.rs", || {})
//!            .item("file2.rs", || {})
//!     })
//! ```

use std::sync::Arc;

use blinc_core::context_state::BlincContextState;
use blinc_core::{Color, State};
use blinc_layout::click_outside;
use blinc_layout::element::CursorStyle;
use blinc_layout::overlay_state::overlay_stack;
use blinc_layout::prelude::*;
use blinc_layout::widgets::hr::hr;
use blinc_layout::widgets::overlay_stack::{OverlayBuilder, OverlayHandle};
use blinc_theme::{ColorToken, RadiusToken, ThemeState};

/// A menu item in the context menu
#[derive(Clone)]
pub struct ContextMenuItem {
    /// Display label
    label: String,
    /// Optional keyboard shortcut display
    shortcut: Option<String>,
    /// Optional icon SVG
    icon: Option<String>,
    /// Click handler
    on_click: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Whether this item is disabled
    disabled: bool,
    /// Whether this is a separator (ignores other fields)
    is_separator: bool,
    /// Submenu items (if this is a submenu trigger)
    submenu: Option<Vec<ContextMenuItem>>,
}

impl std::fmt::Debug for ContextMenuItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextMenuItem")
            .field("label", &self.label)
            .field("shortcut", &self.shortcut)
            .field("icon", &self.icon.is_some())
            .field("disabled", &self.disabled)
            .field("is_separator", &self.is_separator)
            .field("submenu", &self.submenu.as_ref().map(|s| s.len()))
            .finish()
    }
}

impl ContextMenuItem {
    /// Create a new menu item
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            shortcut: None,
            icon: None,
            on_click: None,
            disabled: false,
            is_separator: false,
            submenu: None,
        }
    }

    /// Create a separator
    pub fn separator() -> Self {
        Self {
            label: String::new(),
            shortcut: None,
            icon: None,
            on_click: None,
            disabled: false,
            is_separator: true,
            submenu: None,
        }
    }

    /// Set the click handler
    pub fn on_click<F>(mut self, f: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.on_click = Some(Arc::new(f));
        self
    }

    /// Set a keyboard shortcut hint
    pub fn shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Set an icon (SVG string)
    pub fn icon(mut self, svg: impl Into<String>) -> Self {
        self.icon = Some(svg.into());
        self
    }

    /// Mark as disabled
    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }

    /// Set submenu items
    pub fn submenu(mut self, items: Vec<ContextMenuItem>) -> Self {
        self.submenu = Some(items);
        self
    }

    // =========================================================================
    // Accessors for use by other components (like DropdownMenu)
    // =========================================================================

    /// Get the label
    pub fn get_label(&self) -> &str {
        &self.label
    }

    /// Get the shortcut if any
    pub fn get_shortcut(&self) -> Option<&str> {
        self.shortcut.as_deref()
    }

    /// Get the icon SVG if any
    pub fn get_icon(&self) -> Option<&str> {
        self.icon.as_deref()
    }

    /// Check if this item is disabled
    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    /// Check if this is a separator
    pub fn is_separator(&self) -> bool {
        self.is_separator
    }

    /// Check if this item has a submenu
    pub fn has_submenu(&self) -> bool {
        self.submenu.is_some()
    }

    /// Get the submenu items if any
    pub fn get_submenu(&self) -> Option<&Vec<ContextMenuItem>> {
        self.submenu.as_ref()
    }

    /// Get the click handler (clones the Arc)
    pub fn get_on_click(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        self.on_click.clone()
    }
}

/// Builder for creating context menus
pub struct ContextMenuBuilder {
    /// Position x coordinate
    x: f32,
    /// Position y coordinate
    y: f32,
    /// Menu items
    items: Vec<ContextMenuItem>,
    /// Minimum width
    min_width: f32,
    /// User-added CSS classes
    classes: Vec<std::sync::Arc<str>>,
    /// User-set element ID
    user_id: Option<String>,
}

impl ContextMenuBuilder {
    /// Create a new context menu builder
    pub fn new() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            items: Vec::new(),
            min_width: 180.0,
            classes: Vec::new(),
            user_id: None,
        }
    }

    /// Set the position where the menu should appear
    pub fn at(mut self, x: f32, y: f32) -> Self {
        self.x = x;
        self.y = y;
        self
    }

    /// Add a menu item
    pub fn item<F>(mut self, label: impl Into<String>, on_click: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.items
            .push(ContextMenuItem::new(label).on_click(on_click));
        self
    }

    /// Add a menu item with keyboard shortcut
    pub fn item_with_shortcut<F>(
        mut self,
        label: impl Into<String>,
        shortcut: impl Into<String>,
        on_click: F,
    ) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.items.push(
            ContextMenuItem::new(label)
                .shortcut(shortcut)
                .on_click(on_click),
        );
        self
    }

    /// Add a menu item with icon
    pub fn item_with_icon<F>(
        mut self,
        label: impl Into<String>,
        icon_svg: impl Into<String>,
        on_click: F,
    ) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.items.push(
            ContextMenuItem::new(label)
                .icon(icon_svg)
                .on_click(on_click),
        );
        self
    }

    /// Add a disabled menu item
    pub fn item_disabled(mut self, label: impl Into<String>) -> Self {
        self.items.push(ContextMenuItem::new(label).disabled());
        self
    }

    /// Add a separator line
    pub fn separator(mut self) -> Self {
        self.items.push(ContextMenuItem::separator());
        self
    }

    /// Add a submenu
    pub fn submenu<F>(mut self, label: impl Into<String>, builder: F) -> Self
    where
        F: FnOnce(SubmenuBuilder) -> SubmenuBuilder,
    {
        let sub = builder(SubmenuBuilder::new());
        self.items
            .push(ContextMenuItem::new(label).submenu(sub.items));
        self
    }

    /// Add a raw menu item
    pub fn add_item(mut self, item: ContextMenuItem) -> Self {
        self.items.push(item);
        self
    }

    /// Add a CSS class for selector matching
    pub fn class(mut self, name: impl AsRef<str>) -> Self {
        self.classes.push(blinc_core::intern::intern(name.as_ref()));
        self
    }

    /// Set the element ID for CSS selector matching
    pub fn id(mut self, id: &str) -> Self {
        self.user_id = Some(id.to_string());
        self
    }

    /// Set minimum width
    pub fn min_width(mut self, width: f32) -> Self {
        self.min_width = width;
        self
    }

    /// Show the context menu
    pub fn show(self) -> OverlayHandle {
        let theme = ThemeState::get();
        let bg = theme.color(ColorToken::Surface);
        let border = theme.color(ColorToken::Border);
        let text_color = theme.color(ColorToken::TextPrimary);
        let text_secondary = theme.color(ColorToken::TextSecondary);
        let text_tertiary = theme.color(ColorToken::TextTertiary);
        let radius = theme.radius(RadiusToken::Md);

        let font_size = 14.0;
        let padding = 12.0;

        let items = self.items;
        let width = self.min_width;
        let x = self.x;
        let y = self.y;
        let classes = self.classes;
        let user_id = self.user_id;

        // Pre-allocate handle so menu items can close us by id.
        let next_handle_id = overlay_stack()
            .lock()
            .ok()
            .map(|s| s.peek_next_handle_id())
            .unwrap_or(0);
        let menu_handle = OverlayHandle::from_raw(next_handle_id);
        let menu_id = format!("cn-context-menu-{}", next_handle_id);
        let click_outside_key = format!("ctxmenu:{}", next_handle_id);

        // Track every menu+submenu id in the open chain — used by the root's
        // click_outside registration so clicks inside any open submenu don't
        // dismiss the parent.
        let id_chain: State<Vec<String>> = BlincContextState::get()
            .use_state_keyed(&format!("ctxmenu_chain_{}", next_handle_id), || {
                Vec::<String>::new()
            });
        id_chain.set(vec![menu_id.clone()]);

        let click_outside_key_for_close = click_outside_key.clone();
        let id_chain_for_content = id_chain.clone();
        let menu_id_for_content = menu_id.clone();
        let click_outside_key_for_content = click_outside_key.clone();

        let handle = OverlayBuilder::context_menu()
            .at(x, y)
            // Defaults from DismissRules::default_for(ContextMenu):
            // on_escape=true, on_click_outside=true (handled via registry),
            // no backdrop.
            .on_close(move |_reason| {
                click_outside::unregister_click_outside(&click_outside_key_for_close);
            })
            .content(move || {
                let mut content = build_menu_content(
                    &items,
                    width,
                    menu_handle,
                    &menu_id_for_content,
                    &click_outside_key_for_content,
                    &id_chain_for_content,
                    bg,
                    border,
                    text_color,
                    text_secondary,
                    text_tertiary,
                    radius,
                    font_size,
                    padding,
                );
                for c in &classes {
                    content = content.class(c);
                }
                if let Some(ref id) = user_id {
                    content = content.id(id);
                }
                content
            })
            .show();

        debug_assert_eq!(
            handle.raw(),
            next_handle_id,
            "peek_next_handle_id was stale — concurrent push?"
        );

        // Register click_outside AFTER show() so the menu id is mounted on the
        // next tree walk. Element ids start as just the menu's; submenu hover
        // appends submenu_id via update_click_outside_ids so clicks inside the
        // submenu don't dismiss the parent.
        click_outside::register_click_outside(&click_outside_key, &menu_id, move || {
            menu_handle.close();
        });

        handle
    }
}

impl Default for ContextMenuBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for submenu items
pub struct SubmenuBuilder {
    items: Vec<ContextMenuItem>,
}

impl SubmenuBuilder {
    fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// Add a menu item
    pub fn item<F>(mut self, label: impl Into<String>, on_click: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.items
            .push(ContextMenuItem::new(label).on_click(on_click));
        self
    }

    /// Add a menu item with keyboard shortcut
    pub fn item_with_shortcut<F>(
        mut self,
        label: impl Into<String>,
        shortcut: impl Into<String>,
        on_click: F,
    ) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.items.push(
            ContextMenuItem::new(label)
                .shortcut(shortcut)
                .on_click(on_click),
        );
        self
    }

    /// Add a disabled menu item
    pub fn item_disabled(mut self, label: impl Into<String>) -> Self {
        self.items.push(ContextMenuItem::new(label).disabled());
        self
    }

    /// Add a separator
    pub fn separator(mut self) -> Self {
        self.items.push(ContextMenuItem::separator());
        self
    }

    /// Get the items from this submenu builder
    pub fn items(self) -> Vec<ContextMenuItem> {
        self.items
    }
}

impl SubmenuBuilder {
    /// Create a new submenu builder (public for use by other components)
    pub fn new_public() -> Self {
        Self::new()
    }
}

impl Default for SubmenuBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn a submenu overlay positioned to the right of its parent item.
///
/// The root context menu owns the only click_outside registration. Submenus
/// register themselves as "inside" ids on the root via `id_chain`, so a click
/// landing inside any open submenu does NOT dismiss the root chain.
#[allow(clippy::too_many_arguments)]
fn spawn_submenu(
    x: f32,
    y: f32,
    items: &[ContextMenuItem],
    min_width: f32,
    root_handle: OverlayHandle,
    root_click_outside_key: String,
    id_chain: State<Vec<String>>,
    parent_submenu_state: State<Option<u64>>,
) -> OverlayHandle {
    let theme = ThemeState::get();
    let bg = theme.color(ColorToken::Surface);
    let border = theme.color(ColorToken::Border);
    let text_color = theme.color(ColorToken::TextPrimary);
    let text_secondary = theme.color(ColorToken::TextSecondary);
    let text_tertiary = theme.color(ColorToken::TextTertiary);
    let radius = theme.radius(RadiusToken::Md);
    let font_size = 14.0;
    let padding = 12.0;

    let items = items.to_vec();

    let next_handle_id = overlay_stack()
        .lock()
        .ok()
        .map(|s| s.peek_next_handle_id())
        .unwrap_or(0);
    let submenu_handle = OverlayHandle::from_raw(next_handle_id);
    let submenu_id = format!("cn-context-menu-{}", next_handle_id);

    // Append submenu_id to the root's inside set so clicks inside the submenu
    // don't dismiss the chain.
    let mut chain = id_chain.get();
    if !chain.contains(&submenu_id) {
        chain.push(submenu_id.clone());
        id_chain.set(chain);
        click_outside::update_click_outside_ids(&root_click_outside_key, &id_chain.get());
    }

    let id_chain_for_close = id_chain.clone();
    let root_click_outside_key_for_close = root_click_outside_key.clone();
    let submenu_id_for_close = submenu_id.clone();
    let parent_submenu_state_for_close = parent_submenu_state.clone();

    let id_chain_for_content = id_chain.clone();
    let root_click_outside_key_for_content = root_click_outside_key.clone();
    let submenu_id_for_content = submenu_id.clone();

    let handle = OverlayBuilder::context_menu()
        .at(x, y)
        .on_close(move |_reason| {
            // Pop ourselves from the chain (kept tight so parent stays alive).
            let mut chain = id_chain_for_close.get();
            chain.retain(|id| id != &submenu_id_for_close);
            id_chain_for_close.set(chain);
            click_outside::update_click_outside_ids(
                &root_click_outside_key_for_close,
                &id_chain_for_close.get(),
            );
            // Clear parent's "child submenu open" tracking if we're its child.
            if parent_submenu_state_for_close.get() == Some(submenu_handle.raw()) {
                parent_submenu_state_for_close.set(None);
            }
        })
        .content(move || {
            let mut content = build_menu_content(
                &items,
                min_width,
                root_handle,
                &submenu_id_for_content,
                &root_click_outside_key_for_content,
                &id_chain_for_content,
                bg,
                border,
                text_color,
                text_secondary,
                text_tertiary,
                radius,
                font_size,
                padding,
            );
            content = content.id(&submenu_id_for_content);
            content
        })
        .show();

    debug_assert_eq!(
        handle.raw(),
        next_handle_id,
        "peek_next_handle_id was stale — concurrent push?"
    );

    handle
}

/// Build a context-menu's content (used for both the root menu and submenus).
///
/// `root_handle` always points at the top-level overlay so an item click can
/// close the entire chain (cascading down through stacked submenus). The
/// rendered div is `class("cn-context-menu")` — enter animation is delegated
/// to `@keyframes cn-context-menu-enter` in cn_styles.rs.
#[allow(clippy::too_many_arguments)]
fn build_menu_content(
    items: &[ContextMenuItem],
    width: f32,
    root_handle: OverlayHandle,
    self_id: &str,
    root_click_outside_key: &str,
    id_chain: &State<Vec<String>>,
    bg: Color,
    border: Color,
    text_color: Color,
    text_secondary: Color,
    text_tertiary: Color,
    radius: f32,
    font_size: f32,
    padding: f32,
) -> Div {
    // One slot for the submenu currently open at THIS level (LIFO closes the
    // tail). Keyed off this menu's stable id so reopening preserves identity.
    let child_submenu_state: State<Option<u64>> = BlincContextState::get()
        .use_state_keyed(&format!("ctxmenu_child_sub_{}", self_id), || None);

    let mut menu = div()
        .class("cn-context-menu")
        .id(self_id)
        .flex_col()
        .w(width)
        .bg(bg)
        .border(1.0, border)
        .rounded(radius)
        .lock_corner_shape()
        .shadow_lg()
        .overflow_clip()
        .h_fit();

    for (idx, item) in items.iter().enumerate() {
        if item.is_separator {
            // See dropdown_menu.rs for rationale — `hr()` lets the
            // panel's --surface-elevated CSS bg show through the
            // separator's padding instead of being covered by pure
            // white `Surface`.
            menu = menu.child(hr());
        } else {
            let item_label = item.label.clone();
            let item_shortcut = item.shortcut.clone();
            let item_icon = item.icon.clone();
            let item_disabled = item.disabled;
            let item_on_click = item.on_click.clone();
            let has_submenu = item.submenu.is_some();
            let submenu_items = item.submenu.clone();

            let child_submenu_for_hover_open = child_submenu_state.clone();
            let child_submenu_for_hover_close = child_submenu_state.clone();

            let id_chain_for_open = id_chain.clone();
            let root_click_outside_key_for_open = root_click_outside_key.to_string();

            let item_text_color = if item_disabled {
                text_tertiary
            } else {
                text_color
            };

            let shortcut_color = text_secondary;

            // Plain div, NO Stateful wrapper. Same reason as menubar /
            // dropdown_menu: Stateful subtree rebuilds inside an overlay
            // content closure contaminate base_styles with hover state and
            // jank the motion FSM / child z-ordering. CSS
            // `.cn-context-menu-item:hover` handles the hover background.
            let mut left_side = div()
                .w_fit()
                .h_fit()
                .flex_row()
                .items_center()
                .gap(padding / 4.0);

            if let Some(ref icon_svg) = item_icon {
                left_side = left_side.child(svg(icon_svg).size(16.0, 16.0).color(item_text_color));
            }

            left_side = left_side
                .child(
                    text(&item_label)
                        .size(font_size)
                        .color(item_text_color)
                        .no_cursor()
                        .pointer_events_none(),
                )
                .pointer_events_none();

            let right_side: Option<Div> = if let Some(ref shortcut) = item_shortcut {
                Some(
                    div().child(
                        text(shortcut)
                            .size(font_size - 2.0)
                            .color(shortcut_color)
                            .monospace()
                            .no_cursor(),
                    ),
                )
            } else if has_submenu {
                let chevron_right = r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m9 18 6-6-6-6"/></svg>"#;
                Some(
                    div()
                        .child(svg(chevron_right).size(12.0, 12.0).color(text_tertiary))
                        .pointer_events_none(),
                )
            } else {
                None
            };

            let mut row_content = div()
                .class("cn-context-menu-item")
                .w_full()
                .h_fit()
                .py(padding / 4.0)
                .px(padding / 2.0)
                .cursor(if item_disabled {
                    CursorStyle::NotAllowed
                } else {
                    CursorStyle::Pointer
                })
                .flex_row()
                .items_center()
                .justify_between()
                .child(left_side);

            if let Some(right) = right_side {
                row_content = row_content.child(right);
            }

            let mut row = row_content.on_click(move |_| {
                if !item_disabled && !has_submenu {
                    // Close the root FIRST, then run the user's callback.
                    // `close_with_reason` snapshots `entries_above_to_unwind`
                    // at call-time and cascade-closes them with
                    // `UnwindFromBelow`. If we ran `cb()` first and `cb()`
                    // synchronously pushed an overlay (e.g. via
                    // `editor.push_event` → drainer-stateful → `cn::dialog().show()`),
                    // that overlay would land ABOVE the menu in the stack
                    // and then get killed by the close cascade. Stateful
                    // subscribers fire synchronously within signal-set
                    // dispatch, so the "next frame" deferral the original
                    // ordering relied on doesn't actually exist — verified
                    // by `node_editor_demo` Delete-action timestamps
                    // showing dialog-push immediately followed by
                    // close-cascade with the dialog handle in
                    // `entries_above_to_unwind`.
                    //
                    // Reversing the order: close marks the menu (and any
                    // existing entries above it) for exit, THEN `cb()` can
                    // push fresh overlays at the top of the stack and they
                    // survive — the cascade has already been computed.
                    root_handle.close();
                    if let Some(ref cb) = item_on_click {
                        cb();
                    }
                }
            });

            // Submenu hover-to-open behaviour
            if has_submenu && !item_disabled {
                let submenu_items_for_hover = submenu_items.clone();
                let id_chain_for_hover = id_chain_for_open.clone();
                let root_key_for_hover = root_click_outside_key_for_open.clone();

                row = row.on_hover_enter(move |ctx| {
                    // Close any existing child submenu at this level.
                    if let Some(handle_id) = child_submenu_for_hover_open.get() {
                        OverlayHandle::from_raw(handle_id).close();
                    }

                    if let Some(ref items) = submenu_items_for_hover {
                        let x = ctx.bounds_x + ctx.bounds_width + 4.0;
                        let y = ctx.bounds_y;

                        let handle = spawn_submenu(
                            x,
                            y,
                            items,
                            160.0,
                            root_handle,
                            root_key_for_hover.clone(),
                            id_chain_for_hover.clone(),
                            child_submenu_for_hover_open.clone(),
                        );
                        child_submenu_for_hover_open.set(Some(handle.raw()));
                    }
                });
            } else {
                // Hovering a leaf item closes any open child submenu at this level.
                row = row.on_hover_enter(move |_| {
                    if let Some(handle_id) = child_submenu_for_hover_close.get() {
                        OverlayHandle::from_raw(handle_id).close();
                    }
                });
            }

            menu = menu.child(row);
        }
    }

    menu
}

/// Create a context menu builder
///
/// # Example
///
/// ```ignore
/// cn::context_menu()
///     .at(event.x, event.y)
///     .item("Cut", || println!("Cut"))
///     .item("Copy", || println!("Copy"))
///     .separator()
///     .item("Paste", || println!("Paste"))
///     .show();
/// ```
pub fn context_menu() -> ContextMenuBuilder {
    ContextMenuBuilder::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_menu_item_creation() {
        let item = ContextMenuItem::new("Test");
        assert_eq!(item.label, "Test");
        assert!(!item.disabled);
        assert!(!item.is_separator);
    }

    #[test]
    fn test_menu_item_with_shortcut() {
        let item = ContextMenuItem::new("Copy").shortcut("Ctrl+C");
        assert_eq!(item.shortcut, Some("Ctrl+C".to_string()));
    }

    #[test]
    fn test_separator() {
        let sep = ContextMenuItem::separator();
        assert!(sep.is_separator);
    }

    #[test]
    fn test_disabled_item() {
        let item = ContextMenuItem::new("Disabled").disabled();
        assert!(item.disabled);
    }

    #[test]
    fn test_builder_items() {
        let menu = ContextMenuBuilder::new()
            .item("Item 1", || {})
            .separator()
            .item("Item 2", || {});

        assert_eq!(menu.items.len(), 3);
        assert!(!menu.items[0].is_separator);
        assert!(menu.items[1].is_separator);
        assert!(!menu.items[2].is_separator);
    }

    #[test]
    fn test_builder_position() {
        let menu = ContextMenuBuilder::new().at(100.0, 200.0);
        assert_eq!(menu.x, 100.0);
        assert_eq!(menu.y, 200.0);
    }
}
