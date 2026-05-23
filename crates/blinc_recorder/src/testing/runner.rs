//! Test runner harness for Blinc UI testing.
//!
//! Provides a structured way to run UI tests with setup, teardown,
//! and assertion helpers.

use super::headless::{HeadlessConfig, HeadlessContext};
use crate::{
    RecordedEvent, RecordingConfig, SharedRecordingSession, install_recorder, record_event,
};
use std::sync::Arc;

/// Configuration for the test runner.
#[derive(Clone, Debug)]
pub struct TestConfig {
    /// Headless rendering configuration.
    pub headless: HeadlessConfig,
    /// Recording configuration for capturing test events.
    pub recording: RecordingConfig,
    /// Whether to enable event recording during tests.
    pub record_events: bool,
    /// Timeout for each test in milliseconds.
    pub timeout_ms: u64,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            headless: HeadlessConfig::default(),
            recording: RecordingConfig::testing(),
            record_events: true,
            timeout_ms: 30_000, // 30 seconds default
        }
    }
}

impl TestConfig {
    /// Create a minimal config for fast tests.
    pub fn fast() -> Self {
        Self {
            headless: HeadlessConfig::new(400, 300),
            recording: RecordingConfig::minimal(),
            record_events: false,
            timeout_ms: 5_000,
        }
    }

    /// Set custom dimensions.
    pub fn with_size(mut self, width: u32, height: u32) -> Self {
        self.headless.width = width;
        self.headless.height = height;
        self
    }

    /// Enable or disable event recording.
    pub fn with_recording(mut self, enabled: bool) -> Self {
        self.record_events = enabled;
        self
    }

    /// Set test timeout.
    pub fn with_timeout(mut self, ms: u64) -> Self {
        self.timeout_ms = ms;
        self
    }
}

/// Test runner for executing Blinc UI tests.
pub struct TestRunner {
    config: TestConfig,
    session: Option<Arc<SharedRecordingSession>>,
}

impl TestRunner {
    /// Create a new test runner with the given configuration.
    pub fn new(config: TestConfig) -> Self {
        Self {
            config,
            session: None,
        }
    }

    /// Create a test runner with default configuration.
    pub fn default_runner() -> Self {
        Self::new(TestConfig::default())
    }

    /// Create a fast test runner for quick tests.
    pub fn fast() -> Self {
        Self::new(TestConfig::fast())
    }

    /// Get the configuration.
    pub fn config(&self) -> &TestConfig {
        &self.config
    }

    /// Run a test with the provided closure.
    ///
    /// The closure receives a `TestContext` which provides methods
    /// for rendering, simulating input, and making assertions.
    pub fn run<F>(&mut self, test_fn: F)
    where
        F: FnOnce(&mut TestContext),
    {
        // Set up recording if enabled
        if self.config.record_events {
            let session = Arc::new(SharedRecordingSession::new(self.config.recording.clone()));
            install_recorder(session.clone());
            session.start();
            self.session = Some(session);
        }

        // Create headless context
        let headless = HeadlessContext::new(self.config.headless.clone());

        // Create test context
        let mut ctx = TestContext {
            headless,
            session: self.session.clone(),
        };

        // Run the test
        test_fn(&mut ctx);

        // Stop recording
        if let Some(ref session) = self.session {
            session.stop();
        }
    }

    /// Get the recording session if recording was enabled.
    pub fn session(&self) -> Option<&Arc<SharedRecordingSession>> {
        self.session.as_ref()
    }

    /// Export recorded data if recording was enabled.
    pub fn export(&self) -> Option<crate::RecordingExport> {
        self.session.as_ref().map(|s| s.export())
    }
}

/// Context provided to test functions.
pub struct TestContext {
    headless: HeadlessContext,
    session: Option<Arc<SharedRecordingSession>>,
}

impl TestContext {
    /// Get the headless context.
    pub fn headless(&self) -> &HeadlessContext {
        &self.headless
    }

    /// Get mutable headless context.
    pub fn headless_mut(&mut self) -> &mut HeadlessContext {
        &mut self.headless
    }

    /// Get the window width in logical pixels.
    pub fn width(&self) -> f32 {
        self.headless.width()
    }

    /// Get the window height in logical pixels.
    pub fn height(&self) -> f32 {
        self.headless.height()
    }

    /// Advance to the next frame.
    pub fn next_frame(&mut self) {
        self.headless.next_frame();
    }

    /// Get the current frame count.
    pub fn frame_count(&self) -> u64 {
        self.headless.frame_count()
    }

    /// Simulate a mouse click at the given position.
    pub fn click_at(&self, x: f32, y: f32) {
        use crate::{Modifiers, MouseButton, MouseEvent, Point};

        let event = RecordedEvent::Click(MouseEvent {
            position: Point::new(x, y),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            target_element: None,
        });
        record_event(event);
    }

    /// Simulate a key press.
    pub fn key_press(&self, key: crate::Key) {
        use crate::{KeyEvent, Modifiers};

        let event = RecordedEvent::KeyDown(KeyEvent {
            key,
            modifiers: Modifiers::none(),
            is_repeat: false,
            focused_element: None,
        });
        record_event(event);
    }

    /// Simulate text input.
    pub fn text_input(&self, text: &str) {
        use crate::TextInputEvent;

        let event = RecordedEvent::TextInput(TextInputEvent {
            text: text.to_string(),
            focused_element: None,
        });
        record_event(event);
    }

    /// Get the recording session stats.
    pub fn stats(&self) -> Option<crate::SessionStats> {
        self.session.as_ref().map(|s| s.stats())
    }

    /// Create an element assertion builder.
    ///
    /// Note: Full element assertions require integration with RenderTree.
    /// This is a placeholder for the assertion API.
    pub fn assert_element(&self, _id: &str) -> ElementAssertion {
        ElementAssertion { exists: false }
    }
}

/// Builder for element assertions.
///
/// This is a placeholder - full implementation requires RenderTree integration.
pub struct ElementAssertion {
    exists: bool,
}

impl ElementAssertion {
    /// Assert that the element exists.
    pub fn exists(self) -> Self {
        // TODO: Check element registry
        self
    }

    /// Assert that the element is visible.
    #[allow(clippy::wrong_self_convention)]
    pub fn is_visible(self) -> Self {
        // TODO: Check visibility
        self
    }

    /// Assert that the element is focused.
    #[allow(clippy::wrong_self_convention)]
    pub fn is_focused(self) -> Self {
        // TODO: Check focus state
        self
    }

    /// Assert that the element has the expected bounds.
    pub fn has_bounds(self, _x: f32, _y: f32, _width: f32, _height: f32) -> Self {
        // TODO: Check bounds
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runner_basic() {
        let mut runner = TestRunner::fast();

        runner.run(|ctx| {
            assert_eq!(ctx.width(), 400.0);
            assert_eq!(ctx.height(), 300.0);
            assert_eq!(ctx.frame_count(), 0);

            ctx.next_frame();
            assert_eq!(ctx.frame_count(), 1);
        });
    }

    #[test]
    fn test_runner_with_recording() {
        let mut runner = TestRunner::new(TestConfig::default().with_recording(true));

        runner.run(|ctx| {
            ctx.click_at(100.0, 100.0);
            ctx.text_input("hello");
        });

        // Check that events were recorded
        if let Some(export) = runner.export() {
            assert!(export.stats.total_events >= 2);
        }
    }

    #[test]
    fn test_config_builders() {
        let config = TestConfig::default()
            .with_size(1920, 1080)
            .with_timeout(60_000)
            .with_recording(false);

        assert_eq!(config.headless.width, 1920);
        assert_eq!(config.headless.height, 1080);
        assert_eq!(config.timeout_ms, 60_000);
        assert!(!config.record_events);
    }
}
