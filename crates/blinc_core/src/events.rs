//! Event dispatch system
//!
//! Unified event handling across all platforms.

use rustc_hash::FxHashMap;

/// Event type identifier
pub type EventType = u32;

/// Common event types
pub mod event_types {
    use super::EventType;

    pub const POINTER_DOWN: EventType = 1;
    pub const POINTER_UP: EventType = 2;
    pub const POINTER_MOVE: EventType = 3;
    pub const POINTER_ENTER: EventType = 4;
    pub const POINTER_LEAVE: EventType = 5;
    /// Drag event (mouse down + move)
    pub const DRAG: EventType = 6;
    /// Drag ended (mouse up after drag)
    pub const DRAG_END: EventType = 7;
    pub const FOCUS: EventType = 10;
    pub const BLUR: EventType = 11;
    pub const KEY_DOWN: EventType = 20;
    pub const KEY_UP: EventType = 21;
    /// Text input event (for character input, IME composition)
    pub const TEXT_INPUT: EventType = 22;
    pub const SCROLL: EventType = 30;
    /// Scroll gesture ended (for deceleration/momentum)
    pub const SCROLL_END: EventType = 31;
    /// Pinch zoom gesture update
    pub const PINCH: EventType = 32;
    /// Rotation gesture update (two-finger rotate on trackpad)
    pub const ROTATE: EventType = 33;
    /// Double-tap gesture (two-finger double tap on trackpad)
    pub const DOUBLE_TAP: EventType = 34;
    pub const RESIZE: EventType = 40;

    // File drop events
    /// File(s) dragged over the element
    pub const FILE_DRAG_OVER: EventType = 35;
    /// File(s) dropped onto the element
    pub const FILE_DROP: EventType = 36;
    /// File drag left the element
    pub const FILE_DRAG_LEAVE: EventType = 37;

    // Window lifecycle events
    pub const WINDOW_FOCUS: EventType = 50;
    pub const WINDOW_BLUR: EventType = 51;

    // Element lifecycle events
    pub const MOUNT: EventType = 60;
    pub const UNMOUNT: EventType = 61;

    // Clipboard events
    pub const CUT: EventType = 70;
    pub const COPY: EventType = 71;
    pub const PASTE: EventType = 72;

    // Selection events
    pub const SELECT_ALL: EventType = 80;
}

/// A UI event with associated data
#[derive(Clone, Debug)]
pub struct Event {
    pub event_type: EventType,
    pub target: u64, // Widget ID
    pub data: EventData,
    pub timestamp: u64,
    pub propagation_stopped: bool,
}

/// Event-specific data
#[derive(Clone, Debug)]
pub enum EventData {
    Pointer {
        x: f32,
        y: f32,
        button: u8,
        pressure: f32,
    },
    Key {
        /// Virtual key code (platform-specific, use KeyCode constants)
        key: KeyCode,
        /// Keyboard modifier flags
        modifiers: Modifiers,
        /// Whether this is a repeat event
        repeat: bool,
    },
    /// Text input from keyboard or IME
    TextInput {
        /// The input text (may be multiple characters for IME)
        text: String,
    },
    /// Clipboard paste data
    Clipboard {
        /// The pasted text content
        text: String,
    },
    Scroll {
        delta_x: f32,
        delta_y: f32,
    },
    Resize {
        width: u32,
        height: u32,
    },
    None,
}

/// Virtual key codes (platform-agnostic)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct KeyCode(pub u32);

impl KeyCode {
    // Alphanumeric keys
    pub const A: KeyCode = KeyCode(0x41);
    pub const B: KeyCode = KeyCode(0x42);
    pub const C: KeyCode = KeyCode(0x43);
    pub const D: KeyCode = KeyCode(0x44);
    pub const E: KeyCode = KeyCode(0x45);
    pub const F: KeyCode = KeyCode(0x46);
    pub const G: KeyCode = KeyCode(0x47);
    pub const H: KeyCode = KeyCode(0x48);
    pub const I: KeyCode = KeyCode(0x49);
    pub const J: KeyCode = KeyCode(0x4A);
    pub const K: KeyCode = KeyCode(0x4B);
    pub const L: KeyCode = KeyCode(0x4C);
    pub const M: KeyCode = KeyCode(0x4D);
    pub const N: KeyCode = KeyCode(0x4E);
    pub const O: KeyCode = KeyCode(0x4F);
    pub const P: KeyCode = KeyCode(0x50);
    pub const Q: KeyCode = KeyCode(0x51);
    pub const R: KeyCode = KeyCode(0x52);
    pub const S: KeyCode = KeyCode(0x53);
    pub const T: KeyCode = KeyCode(0x54);
    pub const U: KeyCode = KeyCode(0x55);
    pub const V: KeyCode = KeyCode(0x56);
    pub const W: KeyCode = KeyCode(0x57);
    pub const X: KeyCode = KeyCode(0x58);
    pub const Y: KeyCode = KeyCode(0x59);
    pub const Z: KeyCode = KeyCode(0x5A);

    // Number keys
    pub const KEY_0: KeyCode = KeyCode(0x30);
    pub const KEY_1: KeyCode = KeyCode(0x31);
    pub const KEY_2: KeyCode = KeyCode(0x32);
    pub const KEY_3: KeyCode = KeyCode(0x33);
    pub const KEY_4: KeyCode = KeyCode(0x34);
    pub const KEY_5: KeyCode = KeyCode(0x35);
    pub const KEY_6: KeyCode = KeyCode(0x36);
    pub const KEY_7: KeyCode = KeyCode(0x37);
    pub const KEY_8: KeyCode = KeyCode(0x38);
    pub const KEY_9: KeyCode = KeyCode(0x39);

    // Special keys
    pub const BACKSPACE: KeyCode = KeyCode(0x08);
    pub const TAB: KeyCode = KeyCode(0x09);
    pub const ENTER: KeyCode = KeyCode(0x0D);
    pub const ESCAPE: KeyCode = KeyCode(0x1B);
    pub const SPACE: KeyCode = KeyCode(0x20);
    pub const DELETE: KeyCode = KeyCode(0x7F);

    // Arrow keys
    pub const LEFT: KeyCode = KeyCode(0x25);
    pub const UP: KeyCode = KeyCode(0x26);
    pub const RIGHT: KeyCode = KeyCode(0x27);
    pub const DOWN: KeyCode = KeyCode(0x28);

    // Navigation keys
    pub const HOME: KeyCode = KeyCode(0x24);
    pub const END: KeyCode = KeyCode(0x23);
    pub const PAGE_UP: KeyCode = KeyCode(0x21);
    pub const PAGE_DOWN: KeyCode = KeyCode(0x22);

    // Unknown/unmapped key
    pub const UNKNOWN: KeyCode = KeyCode(0);
}

/// Keyboard modifier flags
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    bits: u8,
}

impl Modifiers {
    pub const NONE: Modifiers = Modifiers { bits: 0 };
    pub const SHIFT: u8 = 0b0001;
    pub const CTRL: u8 = 0b0010;
    pub const ALT: u8 = 0b0100;
    pub const META: u8 = 0b1000; // Cmd on macOS, Win on Windows

    /// Create new modifiers from flags
    pub const fn new(shift: bool, ctrl: bool, alt: bool, meta: bool) -> Self {
        let mut bits = 0;
        if shift {
            bits |= Self::SHIFT;
        }
        if ctrl {
            bits |= Self::CTRL;
        }
        if alt {
            bits |= Self::ALT;
        }
        if meta {
            bits |= Self::META;
        }
        Self { bits }
    }

    /// Create from raw bits
    pub const fn from_bits(bits: u8) -> Self {
        Self { bits }
    }

    /// Check if shift is pressed
    pub const fn shift(&self) -> bool {
        self.bits & Self::SHIFT != 0
    }

    /// Check if ctrl is pressed
    pub const fn ctrl(&self) -> bool {
        self.bits & Self::CTRL != 0
    }

    /// Check if alt is pressed
    pub const fn alt(&self) -> bool {
        self.bits & Self::ALT != 0
    }

    /// Check if meta (Cmd/Win) is pressed
    pub const fn meta(&self) -> bool {
        self.bits & Self::META != 0
    }

    /// Check if any modifier is pressed
    pub const fn any(&self) -> bool {
        self.bits != 0
    }

    /// Check if command key is pressed (Ctrl on non-macOS, Meta on macOS)
    #[cfg(target_os = "macos")]
    pub const fn command(&self) -> bool {
        self.meta()
    }

    /// Check if command key is pressed (Ctrl on non-macOS, Meta on macOS)
    #[cfg(not(target_os = "macos"))]
    pub const fn command(&self) -> bool {
        self.ctrl()
    }
}

impl Event {
    pub fn stop_propagation(&mut self) {
        self.propagation_stopped = true;
    }
}

/// Event handler function type
pub type EventHandler = Box<dyn Fn(&Event) + Send + Sync>;

/// Dispatches events to registered handlers
pub struct EventDispatcher {
    handlers: FxHashMap<(u64, EventType), Vec<EventHandler>>,
}

impl EventDispatcher {
    pub fn new() -> Self {
        Self {
            handlers: FxHashMap::default(),
        }
    }

    /// Register an event handler for a widget and event type
    pub fn register<F>(&mut self, widget_id: u64, event_type: EventType, handler: F)
    where
        F: Fn(&Event) + Send + Sync + 'static,
    {
        self.handlers
            .entry((widget_id, event_type))
            .or_default()
            .push(Box::new(handler));
    }

    /// Dispatch an event to all registered handlers
    pub fn dispatch(&self, event: &mut Event) {
        if let Some(handlers) = self.handlers.get(&(event.target, event.event_type)) {
            for handler in handlers {
                if event.propagation_stopped {
                    break;
                }
                handler(event);
            }
        }
    }
}

impl Default for EventDispatcher {
    fn default() -> Self {
        Self::new()
    }
}
