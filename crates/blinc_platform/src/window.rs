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

/// Window stacking level. Maps to winit's `WindowLevel`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowLevel {
    /// Below regular windows. The window won't trap focus by default.
    AlwaysOnBottom,
    /// Normal stacking — the default for OS-managed Z order.
    #[default]
    Normal,
    /// Floats above regular windows. Useful for tool palettes, HUDs,
    /// always-visible overlays.
    AlwaysOnTop,
}

/// Where animation scheduler ticking happens.
///
/// Blinc's animation scheduler advances springs, keyframe animations,
/// timelines, and `tick_callback`s once per frame. This enum picks
/// which thread does that work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnimationThreadMode {
    /// Tick scheduler animations synchronously on the main thread,
    /// in lockstep with rendering. No background thread is spawned.
    ///
    /// **Default.** Eliminates phase jitter between animation state
    /// and rendered output, simplifies cross-thread reasoning, drops
    /// to fully zero-cost on idle (no thread to park).
    ///
    /// Right for: standard UIs, dashboards, content viewers,
    /// canvas/3D demos where the render closure is the limiting
    /// factor and animation values are read at paint time.
    #[default]
    Main,

    /// Tick scheduler animations on a dedicated background thread at
    /// the configured target FPS, independent of rendering. The main
    /// thread reads the latest computed values during render.
    ///
    /// Right for: apps with `tick_callback`s that need fixed-rate
    /// execution regardless of rendering load — game physics fixed
    /// step, audio sequencer callbacks, telemetry sampling.
    ///
    /// Most apps don't need this. CSS animations, motion
    /// containers, FLIP, theme transitions, and visual / animate_bounds
    /// are always main-thread regardless of this setting; this only
    /// controls the `blinc_animation::AnimationScheduler` family
    /// (springs / keyframes / timelines / tick_callbacks).
    Background,
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
    /// Window stacking level. Default [`WindowLevel::Normal`].
    pub window_level: WindowLevel,
    /// Whether to start in fullscreen mode
    pub fullscreen: bool,
    /// Whether to start in a maximized state.
    pub maximized: bool,
    /// Whether the window is visible at creation. `false` creates a
    /// hidden window that must be shown later. Default `true`.
    pub visible: bool,
    /// Whether the window receives keyboard focus at creation.
    /// Default `true`. Set `false` for tool palettes / panels that
    /// shouldn't steal focus from the main window.
    pub active: bool,
    /// Whether the cursor is visible inside the window. Default `true`.
    /// Toggling at runtime is also exposed via [`Window::set_cursor`]
    /// with [`Cursor::None`].
    pub cursor_visible: bool,
    /// Whether the OS Input Method Editor is allowed for international
    /// text input. Default `true`. Set `false` for game / CAD apps that
    /// need raw keyboard.
    pub ime_allowed: bool,
    /// Minimum window size in logical pixels (None = no constraint)
    pub min_size: Option<(u32, u32)>,
    /// Maximum window size in logical pixels (None = no constraint)
    pub max_size: Option<(u32, u32)>,
    /// Initial window position in logical pixels (None = OS default)
    pub position: Option<(i32, i32)>,
    /// Whether to center the window on the screen at creation
    pub center: bool,
    /// Whether this window is modal (blocks input to other windows
    /// while open). Enforced inside Blinc's event router; the OS
    /// itself isn't signaled (cross-platform modal hints don't exist
    /// in winit 0.30). For OS-level modality on a specific platform,
    /// combine with [`Self::macos_titlebar_buttons_hidden`] or the
    /// equivalent platform-specific knob.
    pub modal: bool,
    /// Parent window ID. Used by Blinc's modal input-blocking to
    /// identify which window remains interactive while `modal` is
    /// set. Not currently signaled to winit — adding an OS-level
    /// parent-child relationship requires raw window handle plumbing
    /// (winit's `with_parent_window` is `unsafe` and platform-specific).
    pub parent: Option<WindowId>,

    /// macOS: render the titlebar transparently so the window
    /// background shows through. Pair with
    /// [`Self::macos_fullsize_content_view`] for an edge-to-edge
    /// content area.
    pub macos_titlebar_transparent: bool,
    /// macOS: hide the titlebar entirely. Window keeps the traffic-
    /// light buttons unless [`Self::macos_titlebar_buttons_hidden`]
    /// is also set.
    pub macos_titlebar_hidden: bool,
    /// macOS: hide the close / minimize / zoom buttons.
    pub macos_titlebar_buttons_hidden: bool,
    /// macOS: let the content view extend under the titlebar area.
    /// Required when drawing custom title-bar chrome.
    pub macos_fullsize_content_view: bool,
    /// macOS: allow the user to drag the window by clicking and
    /// holding anywhere on the background (not just the titlebar).
    pub macos_movable_by_window_background: bool,

    /// Windows: hide this window from the taskbar.
    pub windows_skip_taskbar: bool,
    /// Windows: draw the standard window shadow under a borderless
    /// window (`decorations = false`). No effect when decorations are
    /// enabled (Windows draws the shadow automatically).
    pub windows_undecorated_shadow: bool,
    /// Windows: enable native drag-and-drop file targets. Default `true`.
    pub windows_drag_and_drop: bool,
    /// Windows: optional class-name override. Useful when an app needs
    /// to distinguish window classes for OS-level grouping or
    /// automation tools. Default `None` uses winit's class.
    pub windows_class_name: Option<String>,

    /// X11: window class / instance name. Maps to the `WM_CLASS`
    /// hint that window managers use for grouping and rules.
    pub x11_class: Option<(String, String)>,
    /// How many frames the GPU is allowed to queue ahead of the
    /// currently-presented frame. `2` is the wgpu default and gives
    /// the smoothest pacing under vsync. `1` halves the GPU memory
    /// dedicated to in-flight command buffers, vertex/uniform buffers,
    /// and bind groups — useful for memory-constrained or low-end
    /// devices, at the cost of slightly higher input latency and a
    /// greater chance of dropped frames under load. Clamped to `1..=3`.
    pub max_frame_latency: u32,
    /// Frame-rate cap applied to the redraw chain when the *only*
    /// reason a frame is being scheduled is animation progress
    /// (no input, no scroll, no drag, no Stateful state change).
    ///
    /// `None` (the default) disables the cap — animation frames are
    /// scheduled immediately, so visible animations run at the
    /// display's native vsync rate (typically 60–120 Hz). This is
    /// the right setting for games, video players, scrubbing UIs,
    /// and anything where frame-perfect motion is non-negotiable.
    ///
    /// `Some(N)` caps animation-only redraws to roughly `N` fps via
    /// a deferred wake. Halves wake-ups at `N = 30` for animations
    /// that don't visibly stair-step at sub-vsync rates — color
    /// cycles, opacity fades, blur pulses, gradient morphs.
    ///
    /// User-input frames (typing, scrolling, dragging) are NEVER
    /// throttled — they always ship at native vsync. The cap only
    /// applies when the chain would otherwise re-arm purely because
    /// of an active animation signal.
    ///
    /// **Per-property smoothness override (automatic).** Even when
    /// the cap would otherwise apply, the chain bypasses to native
    /// vsync if any visible animation is touching a property
    /// classified as `needs_vsync_for_smoothness` — transforms
    /// (translate / scale / rotate / skew), 3D rotation, layout
    /// sizing (width / height / padding / margin / gap / inset),
    /// font-size, or clip-path geometry. FLIP and `animate_bounds`
    /// also always run vsync because they animate position/size by
    /// definition. So setting `Some(30)` on a screen mixing slow
    /// fades with a rotate-y keyframe gets you 30 fps for the fade
    /// and 60+ fps for the rotation, frame-by-frame.
    ///
    /// Stateful animations are opaque to this classification — they
    /// dispatch through user callbacks and the framework can't see
    /// which property they ultimately move — so they fall on the
    /// cap-OK side. Apps with Stateful-driven transforms that need
    /// vsync should leave the cap off.
    pub animation_fps_cap: Option<u32>,
    /// Where the animation scheduler ticks (springs, keyframes,
    /// timelines, tick_callbacks).
    ///
    /// Defaults to [`AnimationThreadMode::Main`] — animations advance
    /// synchronously inside Phase 3 of each rendered frame, in
    /// lockstep with paint. Eliminates phase jitter and removes one
    /// thread from the runtime; cost on idle is zero.
    ///
    /// Set to [`AnimationThreadMode::Background`] only if your app
    /// registers `tick_callback`s that need to fire at the
    /// configured target FPS regardless of rendering — e.g. a fixed
    /// step game-physics tick, an audio sequencer, telemetry
    /// sampling.
    ///
    /// CSS animations, motion containers, FLIP, theme transitions,
    /// visual / `animate_bounds` are always ticked on the main
    /// thread regardless of this setting; this only controls the
    /// `blinc_animation::AnimationScheduler` family.
    pub animation_thread_mode: AnimationThreadMode,
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
            window_level: WindowLevel::Normal,
            fullscreen: false,
            maximized: false,
            visible: true,
            active: true,
            cursor_visible: true,
            ime_allowed: true,
            min_size: None,
            max_size: None,
            position: None,
            center: false,
            modal: false,
            parent: None,
            macos_titlebar_transparent: false,
            macos_titlebar_hidden: false,
            macos_titlebar_buttons_hidden: false,
            macos_fullsize_content_view: false,
            macos_movable_by_window_background: false,
            windows_skip_taskbar: false,
            windows_undecorated_shadow: false,
            windows_drag_and_drop: true,
            windows_class_name: None,
            x11_class: None,
            max_frame_latency: 2,
            animation_fps_cap: None,
            animation_thread_mode: AnimationThreadMode::default(),
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

    /// Set whether the window is always on top. Shortcut for
    /// `.window_level(WindowLevel::AlwaysOnTop)`.
    pub fn always_on_top(mut self, always_on_top: bool) -> Self {
        self.window_level = if always_on_top {
            WindowLevel::AlwaysOnTop
        } else {
            WindowLevel::Normal
        };
        self
    }

    /// Set the window stacking level explicitly.
    pub fn window_level(mut self, level: WindowLevel) -> Self {
        self.window_level = level;
        self
    }

    /// Set whether to start in fullscreen
    pub fn fullscreen(mut self, fullscreen: bool) -> Self {
        self.fullscreen = fullscreen;
        self
    }

    /// Set whether to start in a maximized state.
    pub fn maximized(mut self, maximized: bool) -> Self {
        self.maximized = maximized;
        self
    }

    /// Set whether the window is visible at creation. `false` creates
    /// a hidden window that must be shown later.
    pub fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Set whether the window receives keyboard focus at creation.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Set whether the cursor is visible inside the window at creation.
    pub fn cursor_visible(mut self, visible: bool) -> Self {
        self.cursor_visible = visible;
        self
    }

    /// Set whether the OS Input Method Editor is allowed.
    pub fn ime_allowed(mut self, allowed: bool) -> Self {
        self.ime_allowed = allowed;
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

    /// Cap how many frames the GPU may queue ahead. `2` is the smooth
    /// default; `1` halves in-flight GPU memory at the cost of latency
    /// and occasional frame drops. Clamped to `1..=3`.
    pub fn max_frame_latency(mut self, frames: u32) -> Self {
        self.max_frame_latency = frames.clamp(1, 3);
        self
    }

    /// Cap the animation-only redraw rate. See
    /// [`WindowConfig::animation_fps_cap`] for the full semantics —
    /// in short, `Some(30)` halves wake-ups while a CSS keyframe or
    /// transition is the only thing driving the chain, `None` (the
    /// default) keeps animation frames at native vsync. Input,
    /// scroll, and drag frames are never throttled.
    pub fn animation_fps_cap(mut self, fps: Option<u32>) -> Self {
        self.animation_fps_cap = fps.map(|f| f.max(1));
        self
    }

    /// Pick where the animation scheduler ticks. See
    /// [`AnimationThreadMode`] for the full trade-off — the short
    /// version is `Main` (default) is right for almost everything;
    /// `Background` only matters if you have `tick_callback`s that
    /// need fixed-rate execution regardless of rendering load.
    pub fn animation_thread_mode(mut self, mode: AnimationThreadMode) -> Self {
        self.animation_thread_mode = mode;
        self
    }

    /// macOS: render the titlebar transparently.
    pub fn macos_titlebar_transparent(mut self, transparent: bool) -> Self {
        self.macos_titlebar_transparent = transparent;
        self
    }

    /// macOS: hide the titlebar entirely.
    pub fn macos_titlebar_hidden(mut self, hidden: bool) -> Self {
        self.macos_titlebar_hidden = hidden;
        self
    }

    /// macOS: hide the close / minimize / zoom buttons.
    pub fn macos_titlebar_buttons_hidden(mut self, hidden: bool) -> Self {
        self.macos_titlebar_buttons_hidden = hidden;
        self
    }

    /// macOS: extend the content view under the titlebar.
    pub fn macos_fullsize_content_view(mut self, fullsize: bool) -> Self {
        self.macos_fullsize_content_view = fullsize;
        self
    }

    /// macOS: drag the window by clicking anywhere on the background.
    pub fn macos_movable_by_window_background(mut self, movable: bool) -> Self {
        self.macos_movable_by_window_background = movable;
        self
    }

    /// Windows: hide this window from the taskbar.
    pub fn windows_skip_taskbar(mut self, skip: bool) -> Self {
        self.windows_skip_taskbar = skip;
        self
    }

    /// Windows: draw the standard window shadow under a borderless
    /// window. No-op when decorations are enabled.
    pub fn windows_undecorated_shadow(mut self, shadow: bool) -> Self {
        self.windows_undecorated_shadow = shadow;
        self
    }

    /// Windows: enable native drag-and-drop file targets.
    pub fn windows_drag_and_drop(mut self, enabled: bool) -> Self {
        self.windows_drag_and_drop = enabled;
        self
    }

    /// Windows: override the OS-level class name. Useful for window
    /// grouping and automation tools.
    pub fn windows_class_name(mut self, name: impl Into<String>) -> Self {
        self.windows_class_name = Some(name.into());
        self
    }

    /// X11: set the `WM_CLASS` hint (instance, class).
    pub fn x11_class(mut self, instance: impl Into<String>, class: impl Into<String>) -> Self {
        self.x11_class = Some((instance.into(), class.into()));
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

    /// Whether the window was configured with a transparent surface.
    /// Default `false` for platforms where transparency isn't supported
    /// or relevant.
    fn is_transparent(&self) -> bool {
        false
    }

    /// Get safe area insets (top, right, bottom, left) in logical pixels.
    ///
    /// On mobile: accounts for notch, status bar, home indicator.
    /// On desktop: returns zeros (no safe area constraints).
    fn safe_area_insets(&self) -> (f32, f32, f32, f32) {
        (0.0, 0.0, 0.0, 0.0)
    }
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
