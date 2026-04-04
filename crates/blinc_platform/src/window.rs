//! Window abstraction and configuration

/// Platform-agnostic window identifier.
///
/// Wraps a `u64` to avoid leaking platform-specific types (e.g., winit's WindowId).
/// Each window gets a unique ID at creation time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowId(pub u64);

impl WindowId {
    /// The default/primary window ID (used for single-window apps and mobile).
    pub const PRIMARY: WindowId = WindowId(0);
}

/// Window configuration
#[derive(Clone, Debug)]
pub struct WindowConfig {
    /// Window title
    pub title: String,
    /// Initial width in logical pixels
    pub width: u32,
    /// Initial height in logical pixels
    pub height: u32,
    /// Whether the window can be resized
    pub resizable: bool,
    /// Whether to show window decorations (title bar, borders)
    pub decorations: bool,
    /// Whether the window should be transparent
    pub transparent: bool,
    /// Whether the window should always be on top
    pub always_on_top: bool,
    /// Whether to start in fullscreen mode
    pub fullscreen: bool,
    /// Minimum window size in logical pixels (None = no constraint)
    pub min_size: Option<(u32, u32)>,
    /// Maximum window size in logical pixels (None = no constraint)
    pub max_size: Option<(u32, u32)>,
    /// Initial window position in logical pixels (None = OS default)
    pub position: Option<(i32, i32)>,
    /// Whether to center the window on the screen at creation
    pub center: bool,
    /// Whether this window is modal (blocks input to other windows while open)
    pub modal: bool,
    /// Parent window ID for modal relationships (None = top-level)
    pub parent: Option<WindowId>,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "Blinc App".to_string(),
            width: 800,
            height: 600,
            resizable: true,
            decorations: true,
            transparent: false,
            always_on_top: false,
            fullscreen: false,
            min_size: None,
            max_size: None,
            position: None,
            center: false,
            modal: false,
            parent: None,
        }
    }
}

impl WindowConfig {
    /// Create a new window configuration with a title
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ..Default::default()
        }
    }

    /// Set the window title
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Set the window size
    pub fn size(mut self, width: u32, height: u32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Set whether the window is resizable
    pub fn resizable(mut self, resizable: bool) -> Self {
        self.resizable = resizable;
        self
    }

    /// Set whether to show window decorations
    pub fn decorations(mut self, decorations: bool) -> Self {
        self.decorations = decorations;
        self
    }

    /// Set whether the window is transparent
    pub fn transparent(mut self, transparent: bool) -> Self {
        self.transparent = transparent;
        self
    }

    /// Set whether the window is always on top
    pub fn always_on_top(mut self, always_on_top: bool) -> Self {
        self.always_on_top = always_on_top;
        self
    }

    /// Set whether to start in fullscreen
    pub fn fullscreen(mut self, fullscreen: bool) -> Self {
        self.fullscreen = fullscreen;
        self
    }

    /// Set minimum window size in logical pixels
    pub fn min_size(mut self, width: u32, height: u32) -> Self {
        self.min_size = Some((width, height));
        self
    }

    /// Set maximum window size in logical pixels
    pub fn max_size(mut self, width: u32, height: u32) -> Self {
        self.max_size = Some((width, height));
        self
    }

    /// Set initial window position in logical pixels
    pub fn position(mut self, x: i32, y: i32) -> Self {
        self.position = Some((x, y));
        self
    }

    /// Center the window on the screen at creation
    pub fn center(mut self) -> Self {
        self.center = true;
        self
    }

    /// Make this a modal window that blocks input to other windows while open
    pub fn modal(mut self) -> Self {
        self.modal = true;
        self
    }

    /// Set the parent window for modal relationships
    pub fn parent(mut self, parent_id: WindowId) -> Self {
        self.parent = Some(parent_id);
        self
    }
}

/// Window abstraction trait
///
/// Implemented by platform-specific window types.
pub trait Window: Send {
    /// Get the platform-agnostic window ID
    fn id(&self) -> WindowId;

    /// Get window size in physical pixels
    fn size(&self) -> (u32, u32);

    /// Get window size in logical pixels
    fn logical_size(&self) -> (f32, f32);

    /// Get the display scale factor (DPI scaling)
    fn scale_factor(&self) -> f64;

    /// Set the window title
    fn set_title(&self, title: &str);

    /// Set the cursor icon
    fn set_cursor(&self, cursor: Cursor);

    /// Request a redraw
    fn request_redraw(&self);

    /// Check if the window is focused
    fn is_focused(&self) -> bool;

    /// Check if the window is visible
    fn is_visible(&self) -> bool;

    /// Set window position in logical pixels
    fn set_position(&self, _x: i32, _y: i32) {}

    /// Center the window on its current monitor
    fn center_on_screen(&self) {}

    /// Set the window size in logical pixels
    fn set_size(&self, _width: u32, _height: u32) {}

    /// Start a window drag operation (for custom title bars).
    ///
    /// Call this from a mouse-down handler on a draggable region.
    /// The OS takes over the drag and the window follows the cursor.
    fn drag_window(&self) {}

    /// Minimize the window
    fn minimize(&self) {}

    /// Maximize or restore the window
    fn maximize(&self) {}

    /// Close the window
    fn close(&self) {}
}

/// Cursor icons
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Cursor {
    /// Default arrow cursor
    #[default]
    Default,
    /// Pointer/hand cursor (for clickable elements)
    Pointer,
    /// Text/I-beam cursor (for text input)
    Text,
    /// Crosshair cursor
    Crosshair,
    /// Move cursor (for dragging)
    Move,
    /// Not allowed cursor
    NotAllowed,
    /// North-South resize cursor
    ResizeNS,
    /// East-West resize cursor
    ResizeEW,
    /// Northeast-Southwest resize cursor
    ResizeNESW,
    /// Northwest-Southeast resize cursor
    ResizeNWSE,
    /// Grab cursor (open hand)
    Grab,
    /// Grabbing cursor (closed hand)
    Grabbing,
    /// Wait/loading cursor
    Wait,
    /// Progress cursor (arrow with spinner)
    Progress,
    /// Hidden cursor
    None,
}
