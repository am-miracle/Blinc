//! System tray icon support
//!
//! Create tray icons with menus for desktop applications. The tray icon
//! persists even when all windows are closed, enabling background apps.
//!
//! # Example
//!
//! ```ignore
//! use blinc_app::tray::{TrayIconBuilder, TrayMenuItem};
//!
//! let tray = TrayIconBuilder::new()
//!     .tooltip("My App")
//!     .menu(vec![
//!         TrayMenuItem::item("Show Window", || {
//!             // show main window
//!         }),
//!         TrayMenuItem::separator(),
//!         TrayMenuItem::item("Quit", || {
//!             std::process::exit(0);
//!         }),
//!     ])
//!     .build();
//! ```

use std::sync::Arc;

/// A menu item for the system tray context menu
pub enum TrayMenuItem {
    /// A clickable menu item with label and callback
    Item {
        label: String,
        enabled: bool,
        callback: Arc<dyn Fn() + Send + Sync>,
    },
    /// A visual separator between items
    Separator,
    /// A submenu with nested items
    Submenu {
        label: String,
        items: Vec<TrayMenuItem>,
    },
}

impl TrayMenuItem {
    /// Create a clickable menu item
    pub fn item(label: impl Into<String>, callback: impl Fn() + Send + Sync + 'static) -> Self {
        Self::Item {
            label: label.into(),
            enabled: true,
            callback: Arc::new(callback),
        }
    }

    /// Create a disabled menu item
    pub fn item_disabled(label: impl Into<String>) -> Self {
        Self::Item {
            label: label.into(),
            enabled: false,
            callback: Arc::new(|| {}),
        }
    }

    /// Create a separator
    pub fn separator() -> Self {
        Self::Separator
    }

    /// Create a submenu
    pub fn submenu(label: impl Into<String>, items: Vec<TrayMenuItem>) -> Self {
        Self::Submenu {
            label: label.into(),
            items,
        }
    }
}

/// Builder for creating a system tray icon
pub struct TrayIconBuilder {
    tooltip: Option<String>,
    icon_rgba: Option<(Vec<u8>, u32, u32)>,
    menu_items: Vec<TrayMenuItem>,
}

impl TrayIconBuilder {
    /// Create a new tray icon builder
    pub fn new() -> Self {
        Self {
            tooltip: None,
            icon_rgba: None,
            menu_items: Vec::new(),
        }
    }

    /// Set the tooltip text shown on hover
    pub fn tooltip(mut self, tooltip: impl Into<String>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    /// Set the icon from RGBA pixel data
    pub fn icon_rgba(mut self, rgba: Vec<u8>, width: u32, height: u32) -> Self {
        self.icon_rgba = Some((rgba, width, height));
        self
    }

    /// Set the context menu items
    pub fn menu(mut self, items: Vec<TrayMenuItem>) -> Self {
        self.menu_items = items;
        self
    }

    /// Build and show the tray icon. Returns a handle that keeps the icon alive.
    pub fn build(self) -> Option<TrayHandle> {
        #[cfg(feature = "tray-icon")]
        {
            use tray_icon::TrayIconBuilder as NativeTrayBuilder;

            // Build the menu
            let menu = muda::Menu::new();
            let mut callbacks: Vec<(muda::MenuId, Arc<dyn Fn() + Send + Sync>)> = Vec::new();

            type MenuCallback = (muda::MenuId, Arc<dyn Fn() + Send + Sync>);

            fn build_menu_items(
                menu: &muda::Menu,
                items: &[TrayMenuItem],
                callbacks: &mut Vec<MenuCallback>,
            ) {
                for item in items {
                    match item {
                        TrayMenuItem::Item {
                            label,
                            enabled,
                            callback,
                        } => {
                            let menu_item = muda::MenuItem::new(label, *enabled, None);
                            callbacks.push((menu_item.id().clone(), Arc::clone(callback)));
                            let _ = menu.append(&menu_item);
                        }
                        TrayMenuItem::Separator => {
                            let _ = menu.append(&muda::PredefinedMenuItem::separator());
                        }
                        TrayMenuItem::Submenu { label, items } => {
                            let submenu = muda::Submenu::new(label, true);
                            for sub_item in items {
                                match sub_item {
                                    TrayMenuItem::Item {
                                        label,
                                        enabled,
                                        callback,
                                    } => {
                                        let menu_item = muda::MenuItem::new(label, *enabled, None);
                                        callbacks
                                            .push((menu_item.id().clone(), Arc::clone(callback)));
                                        let _ = submenu.append(&menu_item);
                                    }
                                    TrayMenuItem::Separator => {
                                        let _ =
                                            submenu.append(&muda::PredefinedMenuItem::separator());
                                    }
                                    _ => {}
                                }
                            }
                            let _ = menu.append(&submenu);
                        }
                    }
                }
            }

            build_menu_items(&menu, &self.menu_items, &mut callbacks);

            // Create the icon
            let icon = if let Some((rgba, w, h)) = self.icon_rgba {
                tray_icon::Icon::from_rgba(rgba, w, h).ok()
            } else {
                // Default: small colored square
                let size = 32u32;
                let mut rgba = vec![0u8; (size * size * 4) as usize];
                for pixel in rgba.chunks_exact_mut(4) {
                    pixel[0] = 100; // R
                    pixel[1] = 150; // G
                    pixel[2] = 255; // B
                    pixel[3] = 255; // A
                }
                tray_icon::Icon::from_rgba(rgba, size, size).ok()
            };

            let icon = match icon {
                Some(i) => i,
                None => {
                    tracing::error!("Failed to create tray icon");
                    return None;
                }
            };

            // Build the tray
            let mut builder = NativeTrayBuilder::new().with_icon(icon);

            if let Some(ref tooltip) = self.tooltip {
                builder = builder.with_tooltip(tooltip);
            }

            if !self.menu_items.is_empty() {
                builder = builder.with_menu(Box::new(menu));
            }

            match builder.build() {
                Ok(tray) => {
                    // Set up menu event handler
                    let callbacks = Arc::new(callbacks);
                    std::thread::spawn(move || {
                        let receiver = muda::MenuEvent::receiver();
                        while let Ok(event) = receiver.recv() {
                            for (id, cb) in callbacks.iter() {
                                if *id == event.id {
                                    cb();
                                    break;
                                }
                            }
                        }
                    });

                    Some(TrayHandle { _tray: tray })
                }
                Err(e) => {
                    tracing::error!("Failed to build tray icon: {}", e);
                    None
                }
            }
        }

        #[cfg(not(feature = "tray-icon"))]
        {
            tracing::warn!("System tray not available (tray-icon feature not enabled)");
            None
        }
    }
}

impl Default for TrayIconBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle that keeps the tray icon alive. Drop to remove the icon.
pub struct TrayHandle {
    #[cfg(feature = "tray-icon")]
    _tray: tray_icon::TrayIcon,
}
