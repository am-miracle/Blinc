//! Testing infrastructure for Blinc applications.
//!
//! This module provides:
//! - `HeadlessContext` - Run UI without a window for testing
//! - `TestRunner` - Test harness for running headless tests
//! - `CapturedFrame` - Framebuffer capture for screenshots and visual testing
//! - Element assertions for verifying UI state
//!
//! # Example
//!
//! ```ignore
//! use blinc_recorder::testing::{HeadlessContext, TestRunner, TestConfig};
//!
//! #[test]
//! fn test_button_click() {
//!     let runner = TestRunner::new(TestConfig::default());
//!
//!     runner.run(|ctx| {
//!         ctx.render(|| {
//!             div().id("container").child(
//!                 button().id("submit").text("Submit")
//!             )
//!         });
//!
//!         ctx.assert_element("submit").exists().is_visible();
//!         ctx.click("submit");
//!         ctx.render_frame();
//!     });
//! }
//! ```

mod framebuffer;
mod headless;
mod runner;

pub use framebuffer::{
    CapturedFrame, FrameSequence, RegressionResult, ScreenshotExporter, compare_frames,
};
pub use headless::{HeadlessConfig, HeadlessContext};
pub use runner::{TestConfig, TestRunner};
