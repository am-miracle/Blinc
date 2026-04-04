//! Desktop event loop implementation using winit
//!
//! Supports multiple windows via `AppCommand::CreateWindow`.

use std::collections::HashMap;

use crate::input;
use crate::window::DesktopWindow;
use blinc_platform::{
    ControlFlow, Event, EventLoop, LifecycleEvent, PlatformError, Window, WindowConfig,
    WindowEvent, WindowId,
};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent as WinitWindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop as WinitEventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::WindowId as WinitWindowId;

/// Commands sent to the event loop via `EventLoopProxy`.
///
/// Used for cross-thread requests that require access to `ActiveEventLoop`
/// (e.g., creating new windows, which can only happen inside the event handler).
#[derive(Debug)]
pub enum AppCommand {
    /// Wake the event loop (request redraw on all windows)
    Wake,
    /// Create a new window with the given configuration
    CreateWindow(WindowConfig),
    /// Close a specific window
    CloseWindow(WindowId),
}

/// Proxy for waking up the event loop from another thread
///
/// Use this to request a redraw from a background animation thread,
/// or to send commands for window creation/destruction.
#[derive(Clone)]
pub struct WakeProxy {
    proxy: EventLoopProxy<AppCommand>,
}

impl WakeProxy {
    /// Wake up the event loop, causing it to process events and potentially redraw
    pub fn wake(&self) {
        let _ = self.proxy.send_event(AppCommand::Wake);
    }

    /// Request creation of a new window on the next event loop tick
    pub fn create_window(&self, config: WindowConfig) {
        let _ = self.proxy.send_event(AppCommand::CreateWindow(config));
    }

    /// Request closing a specific window
    pub fn close_window(&self, id: WindowId) {
        let _ = self.proxy.send_event(AppCommand::CloseWindow(id));
    }
}

/// Desktop event loop wrapping winit's event loop
pub struct DesktopEventLoop {
    event_loop: WinitEventLoop<AppCommand>,
    window_config: WindowConfig,
    wake_proxy: WakeProxy,
}

impl DesktopEventLoop {
    /// Create a new desktop event loop
    pub fn new(config: WindowConfig) -> Result<Self, PlatformError> {
        let event_loop = WinitEventLoop::with_user_event()
            .build()
            .map_err(|e| PlatformError::EventLoop(e.to_string()))?;

        let wake_proxy = WakeProxy {
            proxy: event_loop.create_proxy(),
        };

        Ok(Self {
            event_loop,
            window_config: config,
            wake_proxy,
        })
    }

    /// Get a wake proxy that can be used to wake up the event loop from another thread
    ///
    /// This is useful for animation threads that need to request redraws,
    /// or for creating new windows from background tasks.
    pub fn wake_proxy(&self) -> WakeProxy {
        self.wake_proxy.clone()
    }
}

impl EventLoop for DesktopEventLoop {
    type Window = DesktopWindow;

    fn run<F>(self, handler: F) -> Result<(), PlatformError>
    where
        F: FnMut(Event, &Self::Window) -> ControlFlow + 'static,
    {
        let mut app = DesktopApp::new(self.window_config, handler);
        self.event_loop
            .run_app(&mut app)
            .map_err(|e| PlatformError::EventLoop(e.to_string()))
    }
}

/// Convert a winit WindowId to our platform-agnostic WindowId
fn to_window_id(winit_id: WinitWindowId) -> WindowId {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    winit_id.hash(&mut hasher);
    WindowId(hasher.finish())
}

/// Internal winit application handler supporting multiple windows
struct DesktopApp<F>
where
    F: FnMut(Event, &DesktopWindow) -> ControlFlow,
{
    /// Config for the primary (initial) window
    window_config: WindowConfig,
    /// All open windows keyed by winit's WindowId
    windows: HashMap<WinitWindowId, DesktopWindow>,
    /// The primary window's winit ID (first window created)
    primary_winit_id: Option<WinitWindowId>,
    /// Event handler
    handler: F,
    /// Current keyboard modifiers
    modifiers: ModifiersState,
    /// Current mouse position (per-window tracking could be added later)
    mouse_position: (f32, f32),
    /// Whether the app should exit
    should_exit: bool,
    /// Currently active modal window (blocks input to other windows)
    modal_window: Option<WinitWindowId>,
}

impl<F> DesktopApp<F>
where
    F: FnMut(Event, &DesktopWindow) -> ControlFlow,
{
    fn new(window_config: WindowConfig, handler: F) -> Self {
        Self {
            window_config,
            windows: HashMap::new(),
            primary_winit_id: None,
            handler,
            modifiers: ModifiersState::empty(),
            mouse_position: (0.0, 0.0),
            should_exit: false,
            modal_window: None,
        }
    }

    /// Dispatch an event using the window identified by winit_id
    fn handle_event_for(&mut self, winit_id: WinitWindowId, event: Event) {
        if let Some(window) = self.windows.get(&winit_id) {
            let flow = (self.handler)(event, window);
            if flow == ControlFlow::Exit {
                self.should_exit = true;
            }
        }
    }

    /// Dispatch an event using the primary window (for global events)
    fn handle_event(&mut self, event: Event) {
        if let Some(primary_id) = self.primary_winit_id {
            self.handle_event_for(primary_id, event);
        }
    }

    /// Create a new window and register it
    fn create_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        config: &WindowConfig,
    ) -> Option<WinitWindowId> {
        match DesktopWindow::new(event_loop, config) {
            Ok(window) => {
                let winit_id = window.winit_window().id();
                self.windows.insert(winit_id, window);
                Some(winit_id)
            }
            Err(e) => {
                tracing::error!("Failed to create window: {}", e);
                None
            }
        }
    }
}

impl<F> ApplicationHandler<AppCommand> for DesktopApp<F>
where
    F: FnMut(Event, &DesktopWindow) -> ControlFlow,
{
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Create the primary window if we don't have one
        if self.primary_winit_id.is_none() {
            let config = self.window_config.clone();
            if let Some(winit_id) = self.create_window(event_loop, &config) {
                self.primary_winit_id = Some(winit_id);
                self.handle_event_for(winit_id, Event::Lifecycle(LifecycleEvent::Resumed));
            } else {
                event_loop.exit();
            }
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        self.handle_event(Event::Lifecycle(LifecycleEvent::Suspended));
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        // Request redraw on wait timeout (frame tick) for all windows
        if matches!(cause, StartCause::WaitCancelled { .. } | StartCause::Poll) {
            for window in self.windows.values() {
                window.request_redraw();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        winit_id: WinitWindowId,
        event: WinitWindowEvent,
    ) {
        // Block input to non-modal windows when a modal is active
        if let Some(modal_id) = self.modal_window {
            if winit_id != modal_id {
                // Allow close/resize/redraw but block input events
                match &event {
                    WinitWindowEvent::KeyboardInput { .. }
                    | WinitWindowEvent::MouseInput { .. }
                    | WinitWindowEvent::CursorMoved { .. }
                    | WinitWindowEvent::MouseWheel { .. }
                    | WinitWindowEvent::Touch(_) => return,
                    _ => {}
                }
            }
        }

        let wid = to_window_id(winit_id);

        match event {
            WinitWindowEvent::CloseRequested => {
                self.handle_event_for(winit_id, Event::Window(wid, WindowEvent::CloseRequested));

                // Remove the window
                self.windows.remove(&winit_id);

                // Clear modal if the closed window was the modal
                if self.modal_window == Some(winit_id) {
                    self.modal_window = None;
                }

                // If no windows remain, exit
                if self.windows.is_empty() {
                    self.should_exit = true;
                    event_loop.exit();
                }
            }

            WinitWindowEvent::Resized(size) => {
                self.handle_event_for(
                    winit_id,
                    Event::Window(
                        wid,
                        WindowEvent::Resized {
                            width: size.width,
                            height: size.height,
                        },
                    ),
                );
            }

            WinitWindowEvent::Moved(pos) => {
                self.handle_event_for(
                    winit_id,
                    Event::Window(wid, WindowEvent::Moved { x: pos.x, y: pos.y }),
                );
            }

            WinitWindowEvent::Focused(focused) => {
                if let Some(window) = self.windows.get(&winit_id) {
                    window.set_focused(focused);
                }
                self.handle_event_for(winit_id, Event::Window(wid, WindowEvent::Focused(focused)));
            }

            WinitWindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.handle_event_for(
                    winit_id,
                    Event::Window(wid, WindowEvent::ScaleFactorChanged { scale_factor }),
                );
            }

            WinitWindowEvent::RedrawRequested => {
                self.handle_event_for(winit_id, Event::Frame(wid));
                if self.should_exit {
                    event_loop.exit();
                }
            }

            WinitWindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }

            WinitWindowEvent::KeyboardInput { event, .. } => {
                let input_event =
                    input::convert_keyboard_event(&event.logical_key, event.state, self.modifiers);
                self.handle_event_for(winit_id, Event::Input(wid, input_event));
                if let Some(window) = self.windows.get(&winit_id) {
                    window.request_redraw();
                }
            }

            WinitWindowEvent::CursorMoved { position, .. } => {
                self.mouse_position = (position.x as f32, position.y as f32);
                let input_event = input::mouse_moved(self.mouse_position.0, self.mouse_position.1);
                self.handle_event_for(winit_id, Event::Input(wid, input_event));
            }

            WinitWindowEvent::MouseInput { state, button, .. } => {
                let (x, y) = self.mouse_position;
                let input_event = match state {
                    winit::event::ElementState::Pressed => input::mouse_pressed(button, x, y),
                    winit::event::ElementState::Released => input::mouse_released(button, x, y),
                };
                self.handle_event_for(winit_id, Event::Input(wid, input_event));
            }

            WinitWindowEvent::MouseWheel { delta, phase, .. } => {
                let (dx, dy) = match delta {
                    winit::event::MouseScrollDelta::LineDelta(x, y) => (x, y),
                    winit::event::MouseScrollDelta::PixelDelta(pos) => {
                        (pos.x as f32 / 10.0, pos.y as f32 / 10.0)
                    }
                };
                let input_event = input::scroll_event(dx, dy, phase);
                self.handle_event_for(winit_id, Event::Input(wid, input_event));

                if matches!(
                    phase,
                    winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled
                ) {
                    self.handle_event_for(winit_id, Event::Input(wid, input::scroll_end_event()));
                }
            }

            WinitWindowEvent::Touch(touch) => {
                let input_event = input::convert_touch_event(&touch);
                self.handle_event_for(winit_id, Event::Input(wid, input_event));
            }

            WinitWindowEvent::CursorEntered { .. } => {
                self.handle_event_for(
                    winit_id,
                    Event::Input(
                        wid,
                        blinc_platform::InputEvent::Mouse(blinc_platform::MouseEvent::Entered),
                    ),
                );
            }

            WinitWindowEvent::CursorLeft { .. } => {
                self.handle_event_for(
                    winit_id,
                    Event::Input(
                        wid,
                        blinc_platform::InputEvent::Mouse(blinc_platform::MouseEvent::Left),
                    ),
                );
            }

            WinitWindowEvent::Ime(ime_event) => {
                match ime_event {
                    winit::event::Ime::Commit(text) => {
                        // IME committed text — deliver each character as a Char key event
                        for c in text.chars() {
                            let input_event = blinc_platform::InputEvent::Keyboard(
                                blinc_platform::KeyboardEvent {
                                    key: blinc_platform::Key::Char(c),
                                    state: blinc_platform::KeyState::Pressed,
                                    modifiers: blinc_platform::Modifiers::default(),
                                },
                            );
                            self.handle_event_for(winit_id, Event::Input(wid, input_event));
                        }
                        if let Some(window) = self.windows.get(&winit_id) {
                            window.request_redraw();
                        }
                    }
                    winit::event::Ime::Preedit(text, cursor) => {
                        // IME pre-edit (composition in progress)
                        // TODO: render pre-edit text with underline at cursor position
                        let _ = (text, cursor);
                    }
                    winit::event::Ime::Enabled => {
                        tracing::debug!("IME enabled for window {:?}", winit_id);
                    }
                    winit::event::Ime::Disabled => {
                        tracing::debug!("IME disabled for window {:?}", winit_id);
                    }
                }
            }

            _ => {}
        }

        if self.should_exit {
            event_loop.exit();
        }
    }

    fn memory_warning(&mut self, _event_loop: &ActiveEventLoop) {
        self.handle_event(Event::Lifecycle(LifecycleEvent::LowMemory));
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, command: AppCommand) {
        match command {
            AppCommand::Wake => {
                // Wake from animation thread — request redraw on all windows
                for window in self.windows.values() {
                    window.request_redraw();
                }
            }
            AppCommand::CreateWindow(config) => {
                let is_modal = config.modal;
                if let Some(winit_id) = self.create_window(event_loop, &config) {
                    if is_modal {
                        self.modal_window = Some(winit_id);
                    }
                    let wid = to_window_id(winit_id);
                    self.handle_event_for(winit_id, Event::Lifecycle(LifecycleEvent::Resumed));
                    tracing::info!(
                        "Created new window {:?} (wid={:?}, modal={})",
                        winit_id,
                        wid,
                        is_modal
                    );
                }
            }
            AppCommand::CloseWindow(wid) => {
                // Find the winit ID for this WindowId
                let winit_id = self
                    .windows
                    .iter()
                    .find(|(_, w)| w.id() == wid)
                    .map(|(id, _)| *id);
                if let Some(winit_id) = winit_id {
                    self.handle_event_for(
                        winit_id,
                        Event::Window(wid, WindowEvent::CloseRequested),
                    );
                    self.windows.remove(&winit_id);

                    // Clear modal if the closed window was the modal
                    if self.modal_window == Some(winit_id) {
                        self.modal_window = None;
                    }

                    if self.windows.is_empty() {
                        self.should_exit = true;
                        event_loop.exit();
                    }
                }
            }
        }
    }
}
