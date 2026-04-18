//! Audio and video player widgets
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
use blinc_media::{Frame, Player, VideoState};
use blinc_theme::{ColorToken, ThemeState};

use crate::div::{div, Div, ElementBuilder, ElementTypeId};
use crate::element::RenderProps;
use crate::key::InstanceKey;
use crate::stateful::{stateful_with_key, NoState, Stateful};
use crate::text::text;
use crate::tree::{LayoutNodeId, LayoutTree};

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

/// Video player widget with frame display and signal-driven controls
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

        // Video surface — caches last frame to avoid mutex + Arc clone
        // when frame_generation hasn't changed between paints
        let player_for_canvas = Rc::clone(&player);
        let cached: std::rc::Rc<std::cell::RefCell<(u64, Option<std::sync::Arc<Frame>>)>> =
            std::rc::Rc::new(std::cell::RefCell::new((0, None)));
        let surface = crate::canvas::canvas(
            move |ctx: &mut dyn blinc_core::DrawContext, bounds: crate::canvas::CanvasBounds| {
                // Only fetch a new frame from the mutex when generation advances
                let gen = player_for_canvas.frame_generation();
                let mut cache = cached.borrow_mut();
                if gen != cache.0 {
                    cache.1 = player_for_canvas.current_frame();
                    cache.0 = gen;
                }

                ctx.fill_rect(
                    blinc_core::Rect::new(0.0, 0.0, bounds.width, bounds.height),
                    blinc_core::CornerRadius::default(),
                    blinc_core::Brush::Solid(Color::BLACK),
                );

                if let Some(ref frame) = cache.1 {
                    let rgba = frame.as_rgba();
                    let vid_w = frame.width as f32;
                    let vid_h = frame.height as f32;
                    let vid_aspect = vid_w / vid_h;
                    let box_aspect = bounds.width / bounds.height;

                    let (dest_w, dest_h) = if vid_aspect > box_aspect {
                        (bounds.width, bounds.width / vid_aspect)
                    } else {
                        (bounds.height * vid_aspect, bounds.height)
                    };
                    let dest_x = (bounds.width - dest_w) / 2.0;
                    let dest_y = (bounds.height - dest_h) / 2.0;

                    ctx.draw_rgba_pixels(
                        &rgba,
                        frame.width,
                        frame.height,
                        blinc_core::Rect::new(dest_x, dest_y, dest_w, dest_h),
                    );
                }
            },
        )
        .w_full()
        .flex_grow();

        container = container.child(surface);

        // Controls — lazy-built Stateful driven by player signals
        let controls = VideoControlsBuilder::new((*player).clone());
        container = container.child(controls);

        container
    }
}

/// Lazy-built media controls using the OnceCell pattern.
/// The inner Stateful is constructed on first `build()` access,
/// ensuring it's created within the tree-building context.
struct VideoControlsBuilder {
    player: blinc_media::VideoPlayer,
    key: InstanceKey,
    built: std::cell::OnceCell<Stateful<NoState>>,
}

impl VideoControlsBuilder {
    fn new(player: blinc_media::VideoPlayer) -> Self {
        Self {
            player,
            key: InstanceKey::new("video-controls"),
            built: std::cell::OnceCell::new(),
        }
    }

    fn get_or_build(&self) -> &Stateful<NoState> {
        self.built.get_or_init(|| {
            let player = self.player.clone();
            let key = self.key.get().to_string();

            stateful_with_key::<NoState>(&key)
                .deps([
                    player.playing_signal.signal_id(),
                    player.volume_signal.signal_id(),
                ])
                .on_state(move |_ctx| {
                    let theme = ThemeState::get();
                    let fg = theme.color(ColorToken::TextPrimary);
                    let fg_secondary = theme.color(ColorToken::TextSecondary);
                    let fg_tertiary = theme.color(ColorToken::TextTertiary);
                    let surface = theme.color(ColorToken::SurfaceElevated);
                    let accent = theme.color(ColorToken::Primary);
                    let border = theme.color(ColorToken::Border);

                    let is_playing = player.playing_signal.get();
                    let volume = player.volume_signal.get();

                    let play_icon = if is_playing {
                        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor"><path d="M6 19h4V5H6v14zm8-14v14h4V5h-4z"/></svg>"#
                    } else {
                        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor"><path d="M8 5v14l11-7z"/></svg>"#
                    };

                    let btn_id = format!("vp-play-btn-{}", key);
                    let player_click = player.clone();
                    let play_btn = div()
                        .id(&btn_id)
                        .class("blinc-media-play-btn")
                        .w(28.0)
                        .h(28.0)
                        .rounded(14.0)
                        .bg(surface)
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .child(crate::svg::svg(play_icon).square(14.0).color(fg))
                        .on_click(move |_| {
                            if player_click.is_playing() {
                                player_click.pause();
                            } else {
                                player_click.play();
                            }
                        });

                    let seek_id = format!("vp-seek-{}", key);
                    let player_seek = player.clone();

                    let seek_key = format!("vp-seek-canvas-{}", key);
                    let seek_player = player.clone();
                    let seek_surface = surface;
                    let seek_accent = accent;
                    let seek_border = border;
                    let seek_stateful = stateful_with_key::<NoState>(&seek_key)
                        .deps([
                            seek_player.position_signal.signal_id(),
                            seek_player.duration_signal.signal_id(),
                            seek_player.buffered_signal.signal_id(),
                        ])
                        .on_state(move |_ctx| {
                            let pos = seek_player.position_signal.get();
                            let dur = seek_player.duration_signal.get();
                            let buf = seek_player.buffered_signal.get().min(dur);
                            let progress = if dur > 0 {
                                (pos as f32 / dur as f32).clamp(0.0, 1.0)
                            } else {
                                0.0
                            };
                            let buffered = if dur > 0 {
                                (buf as f32 / dur as f32).clamp(0.0, 1.0)
                            } else {
                                0.0
                            };
                            let track_color = seek_surface;
                            // Buffered sits between the dim track and
                            // the bright progress fill — same hue as
                            // the UI border (one step brighter than
                            // the track) so it reads as "downloaded
                            // but not yet played" without competing
                            // with the accent fill for attention.
                            let buffered_color = seek_border;
                            let fill_color = seek_accent;
                            div().flex_grow().h(12.0).child(
                                crate::canvas::canvas(
                                    move |ctx: &mut dyn blinc_core::DrawContext, bounds| {
                                        let track_y = (bounds.height - 4.0) / 2.0;
                                        ctx.fill_rect(
                                            blinc_core::Rect::new(
                                                0.0,
                                                track_y,
                                                bounds.width,
                                                4.0,
                                            ),
                                            2.0.into(),
                                            blinc_core::Brush::Solid(track_color),
                                        );
                                        if buffered > progress {
                                            ctx.fill_rect(
                                                blinc_core::Rect::new(
                                                    0.0,
                                                    track_y,
                                                    bounds.width * buffered,
                                                    4.0,
                                                ),
                                                2.0.into(),
                                                blinc_core::Brush::Solid(buffered_color),
                                            );
                                        }
                                        if progress > 0.0 {
                                            ctx.fill_rect(
                                                blinc_core::Rect::new(
                                                    0.0,
                                                    track_y,
                                                    bounds.width * progress,
                                                    4.0,
                                                ),
                                                2.0.into(),
                                                blinc_core::Brush::Solid(fill_color),
                                            );
                                        }
                                    },
                                )
                                .flex_grow()
                                .h(12.0),
                            )
                        });

                    let vol_icon = if volume < 0.01 {
                        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor"><path d="M16.5 12c0-1.77-1.02-3.29-2.5-4.03v2.21l2.45 2.45c.03-.2.05-.41.05-.63zm2.5 0c0 .94-.2 1.82-.54 2.64l1.51 1.51C20.63 14.91 21 13.5 21 12c0-4.28-2.99-7.86-7-8.77v2.06c2.89.86 5 3.54 5 6.71zM4.27 3L3 4.27 7.73 9H3v6h4l5 5v-6.73l4.25 4.25c-.67.52-1.42.93-2.25 1.18v2.06c1.38-.31 2.63-.95 3.69-1.81L19.73 21 21 19.73l-9-9L4.27 3zM12 4L9.91 6.09 12 8.18V4z"/></svg>"#
                    } else if volume < 0.5 {
                        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor"><path d="M18.5 12c0-1.77-1.02-3.29-2.5-4.03v8.05c1.48-.73 2.5-2.25 2.5-4.02zM5 9v6h4l5 5V4L9 9H5z"/></svg>"#
                    } else {
                        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor"><path d="M3 9v6h4l5 5V4L7 9H3zm13.5 3c0-1.77-1.02-3.29-2.5-4.03v8.05c1.48-.73 2.5-2.25 2.5-4.02zM14 3.23v2.06c2.89.86 5 3.54 5 6.71s-2.11 5.85-5 6.71v2.06c4.01-.91 7-4.49 7-8.77s-2.99-7.86-7-8.77z"/></svg>"#
                    };

                    let mute_id = format!("vp-mute-{}", key);
                    let player_mute = player.clone();
                    let mute_btn = div()
                        .id(&mute_id)
                        .class("blinc-media-mute-btn")
                        .w(20.0)
                        .h(20.0)
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .child(crate::svg::svg(vol_icon).square(16.0).color(fg_secondary))
                        .on_click(move |_| {
                            if player_mute.volume() < 0.01 {
                                player_mute.set_volume(1.0);
                            } else {
                                player_mute.set_volume(0.0);
                            }
                        });

                    let vol_slider_id = format!("vp-vol-sl-{}", key);
                    let player_vol = player.clone();
                    let vol_sig = player.volume_signal.clone();
                    let vol_active = surface;
                    let vol_inactive = fg_secondary;
                    let vol_canvas = crate::canvas::canvas(
                        move |ctx: &mut dyn blinc_core::DrawContext, bounds| {
                            let v = vol_sig.get();
                            let w = bounds.width;
                            let h = bounds.height;
                            let cols = 20u32;
                            let col_w = w / cols as f32;
                            for i in 0..cols {
                                let frac = (i as f32 + 0.5) / cols as f32;
                                let col_h = h * frac;
                                let y = h - col_h;
                                let color = if frac <= v + 0.025 { vol_active } else { vol_inactive };
                                ctx.fill_rect(
                                    blinc_core::Rect::new(i as f32 * col_w, y, col_w - 1.0, col_h),
                                    0.0.into(),
                                    blinc_core::Brush::Solid(color),
                                );
                            }
                        },
                    )
                    .w(50.0)
                    .h(14.0);

                    div()
                        .class("blinc-media-controls")
                        .flex_row()
                        .items_center()
                        .gap_px(8.0)
                        .p_px(6.0)
                        .child(play_btn)
                        .child({
                            let time_player = player.clone();
                            let time_key = format!("vp-time-{}", key);
                            let time_color = fg_tertiary;
                            stateful_with_key::<NoState>(&time_key)
                                .deps([
                                    time_player.position_signal.signal_id(),
                                    time_player.duration_signal.signal_id(),
                                ])
                                .on_state(move |_ctx| {
                                    let pos = time_player.position_signal.get();
                                    let dur = time_player.duration_signal.get();
                                    div().child(
                                        text(format!(
                                            "{} / {}",
                                            format_time_ms(pos),
                                            format_time_ms(dur)
                                        ))
                                        .size(11.0)
                                        .color(time_color)
                                        .monospace()
                                        .class("blinc-media-time"),
                                    )
                                })
                        })
                        .child(
                            div()
                                .id(&seek_id)
                                .class("blinc-media-seek")
                                .flex_grow()
                                .h(12.0)
                                .cursor_pointer()
                                .on_click(move |evt| {
                                    let dur = player_seek.duration_ms();
                                    if dur > 0 && evt.bounds_width > 0.0 {
                                        let ratio =
                                            (evt.local_x / evt.bounds_width).clamp(0.0, 1.0);
                                        let target_ms = (ratio * dur as f32) as u64;
                                        player_seek.seek(target_ms);
                                    }
                                })
                                .child(seek_stateful),
                        )
                        .child(
                            div()
                                .class("blinc-media-volume")
                                .flex_row()
                                .items_center()
                                .gap_px(4.0)
                                .child(mute_btn)
                                .child(
                                    div()
                                        .id(&vol_slider_id)
                                        .class("blinc-media-vol-slider")
                                        .w(50.0)
                                        .h(14.0)
                                        .cursor_pointer()
                                        .on_click(move |evt| {
                                            if evt.bounds_width > 0.0 {
                                                let ratio =
                                                    (evt.local_x / evt.bounds_width).clamp(0.0, 1.0);
                                                player_vol.set_volume(ratio);
                                            }
                                        })
                                        .child(vol_canvas),
                                ),
                        )
                })
        })
    }
}

impl ElementBuilder for VideoControlsBuilder {
    fn build(&self, tree: &mut LayoutTree) -> LayoutNodeId {
        self.get_or_build().build(tree)
    }

    fn render_props(&self) -> RenderProps {
        self.get_or_build().render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        self.get_or_build().children_builders()
    }

    fn element_type_id(&self) -> ElementTypeId {
        ElementTypeId::Div
    }

    fn event_handlers(&self) -> Option<&crate::event_handler::EventHandlers> {
        self.get_or_build().event_handlers()
    }

    fn layout_style(&self) -> Option<&taffy::Style> {
        self.get_or_build().layout_style()
    }

    fn element_id(&self) -> Option<&str> {
        self.get_or_build().element_id()
    }

    fn element_classes(&self) -> &[String] {
        self.get_or_build().element_classes()
    }
}
