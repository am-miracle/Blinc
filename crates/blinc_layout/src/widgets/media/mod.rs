//! Audio and video player widgets
//!
//! Requires the `media` feature flag.
//!
//! Both widgets use `MediaControls` which is generic over
//! the `Player` trait — shared play/pause, seek, time, volume.
//!
//! All elements have CSS classes (`.blinc-audio-*`, `.blinc-video-*`,
//! `.blinc-media-*`) for stylesheet targeting.

mod controls;
mod format_time;
mod waveform;

pub use controls::MediaControls;
pub use format_time::format_time_ms;
pub use waveform::waveform;

use std::rc::Rc;
use std::sync::Arc;

use blinc_core::Color;
use blinc_media::Player;

use crate::div::{div, Div};
use crate::text::text;

// ============================================================================
// Audio Player Widget
// ============================================================================

/// Audio player widget with optional waveform visualization
pub struct AudioPlayerWidget {
    player: Rc<blinc_media::AudioPlayer>,
    samples: Option<Arc<Vec<f32>>>,
    inner: Div,
}

/// Create an audio player widget
pub fn audio_player(player: Rc<blinc_media::AudioPlayer>) -> AudioPlayerWidget {
    AudioPlayerWidget {
        player,
        samples: None,
        inner: div().class("blinc-audio-player"),
    }
}

impl AudioPlayerWidget {
    /// Add waveform visualization from audio samples
    pub fn waveform_data(mut self, samples: &blinc_media::AudioSamples) -> Self {
        let f32_samples = samples.as_f32();
        let channels = samples.channels as usize;
        let mono: Vec<f32> = if channels > 1 {
            f32_samples
                .chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                .collect()
        } else {
            f32_samples
        };
        let bucket_count = 200;
        let bucket_size = (mono.len() / bucket_count).max(1);
        let buckets: Vec<f32> = mono
            .chunks(bucket_size)
            .map(|chunk| chunk.iter().map(|s| s.abs()).fold(0.0f32, f32::max))
            .collect();
        self.samples = Some(Arc::new(buckets));
        self
    }

    pub fn w(mut self, v: f32) -> Self {
        self.inner = self.inner.w(v);
        self
    }
    pub fn h(mut self, v: f32) -> Self {
        self.inner = self.inner.h(v);
        self
    }
    pub fn w_full(mut self) -> Self {
        self.inner = self.inner.w_full();
        self
    }
    pub fn bg(mut self, color: Color) -> Self {
        self.inner = self.inner.bg(color);
        self
    }
    pub fn rounded(mut self, r: f32) -> Self {
        self.inner = self.inner.rounded(r);
        self
    }
    pub fn class(mut self, class: &str) -> Self {
        self.inner = self.inner.class(class);
        self
    }
    pub fn id(mut self, id: &str) -> Self {
        self.inner = self.inner.id(id);
        self
    }

    pub fn into_div(self) -> Div {
        let mut container = self.inner.flex_col().gap_px(4.0);

        // Waveform
        if let Some(ref buckets) = self.samples {
            container = container.child(
                waveform(Arc::clone(buckets))
                    .w_full()
                    .h(60.0)
                    .class("blinc-audio-waveform")
                    .into_div(),
            );
        }

        // Controls (via Player trait)
        let controls = MediaControls::new(self.player).class("blinc-audio-controls");
        container = container.child(controls.into_div());

        container
    }
}

// ============================================================================
// Video Player Widget
// ============================================================================

/// Video player widget with frame display and controls
pub struct VideoPlayerWidget {
    player: Rc<blinc_media::VideoPlayer>,
    inner: Div,
    show_dimensions: bool,
}

/// Create a video player widget
pub fn video_player(player: Rc<blinc_media::VideoPlayer>) -> VideoPlayerWidget {
    VideoPlayerWidget {
        player,
        inner: div().class("blinc-video-player"),
        show_dimensions: false,
    }
}

impl VideoPlayerWidget {
    pub fn show_dimensions(mut self) -> Self {
        self.show_dimensions = true;
        self
    }
    pub fn w(mut self, v: f32) -> Self {
        self.inner = self.inner.w(v);
        self
    }
    pub fn h(mut self, v: f32) -> Self {
        self.inner = self.inner.h(v);
        self
    }
    pub fn w_full(mut self) -> Self {
        self.inner = self.inner.w_full();
        self
    }
    pub fn bg(mut self, color: Color) -> Self {
        self.inner = self.inner.bg(color);
        self
    }
    pub fn rounded(mut self, r: f32) -> Self {
        self.inner = self.inner.rounded(r);
        self
    }
    pub fn class(mut self, class: &str) -> Self {
        self.inner = self.inner.class(class);
        self
    }
    pub fn id(mut self, id: &str) -> Self {
        self.inner = self.inner.id(id);
        self
    }

    pub fn into_div(self) -> Div {
        let player = Rc::clone(&self.player);
        let mut container = self.inner.flex_col();

        // Video surface (canvas)
        let player_for_canvas = Rc::clone(&player);
        let surface = crate::canvas::canvas(
            move |ctx: &mut dyn blinc_core::DrawContext, bounds: crate::canvas::CanvasBounds| {
                let frame = player_for_canvas.current_frame();
                if frame.is_some() {
                    // Frame available — dark surface (real rendering needs DrawContext RGBA upload)
                    ctx.fill_rect(
                        blinc_core::Rect::new(0.0, 0.0, bounds.width, bounds.height),
                        blinc_core::CornerRadius::default(),
                        blinc_core::Brush::Solid(Color::rgba(0.08, 0.08, 0.1, 1.0)),
                    );
                } else {
                    ctx.fill_rect(
                        blinc_core::Rect::new(0.0, 0.0, bounds.width, bounds.height),
                        blinc_core::CornerRadius::default(),
                        blinc_core::Brush::Solid(Color::rgba(0.05, 0.05, 0.07, 1.0)),
                    );
                }
            },
        )
        .w_full()
        .flex_grow();

        container = container.child(surface);

        // Controls (via Player trait — VideoPlayer implements Player)
        let controls = MediaControls::new(player).class("blinc-video-controls");
        container = container.child(controls.into_div());

        // Dimensions
        if self.show_dimensions {
            if let Some(frame) = self.player.current_frame() {
                container = container.child(
                    text(format!("{}x{}", frame.width, frame.height))
                        .size(10.0)
                        .color(Color::rgba(0.4, 0.4, 0.5, 1.0))
                        .class("blinc-video-dimensions"),
                );
            }
        }

        container
    }
}
