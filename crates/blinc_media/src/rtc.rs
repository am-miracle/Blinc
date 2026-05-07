//! RTC-like reactive media API
//!
//! Signal-driven audio and camera capture with real-time frame/sample
//! delivery. Built on Blinc's reactive system — components observe
//! media signals and rebuild when new data arrives.
//!
//! # Camera
//!
//! ```ignore
//! use blinc_media::rtc::{CameraStream, CameraConfig};
//!
//! let camera = CameraStream::open(CameraConfig {
//!     width: 640,
//!     height: 480,
//!     fps: 30,
//!     facing: CameraFacing::Front,
//! });
//!
//! // Reactive — rebuilds when a new frame arrives
//! let frame = camera.latest_frame();
//! if let Some(frame) = frame {
//!     canvas(move |ctx, bounds| {
//!         ctx.draw_image_rgba(&frame.as_rgba(), frame.width, frame.height, bounds);
//!     })
//! }
//!
//! // Stop capture
//! drop(camera);
//! ```
//!
//! # Audio Recording
//!
//! ```ignore
//! use blinc_media::rtc::{AudioRecorder, AudioRecorderConfig};
//!
//! let recorder = AudioRecorder::open(AudioRecorderConfig {
//!     sample_rate: 44100,
//!     channels: 1,
//! });
//!
//! // Get latest audio buffer
//! let samples = recorder.latest_samples();
//!
//! // Stop recording
//! drop(recorder);
//! ```

use std::sync::{Arc, Mutex};

use crate::frame::{AudioSamples, Frame};

/// Camera facing direction
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraFacing {
    Front,
    Back,
}

/// Camera configuration
#[derive(Clone, Debug)]
pub struct CameraConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub facing: CameraFacing,
}

impl Default for CameraConfig {
    fn default() -> Self {
        Self {
            width: 640,
            height: 480,
            fps: 30,
            facing: CameraFacing::Front,
        }
    }
}

/// Audio recorder configuration
#[derive(Clone, Debug)]
pub struct AudioRecorderConfig {
    pub sample_rate: u32,
    pub channels: u16,
}

impl Default for AudioRecorderConfig {
    fn default() -> Self {
        Self {
            sample_rate: 44100,
            channels: 1,
        }
    }
}

/// Shared frame buffer — holds the latest frame from the camera
type SharedFrame = Arc<Mutex<Option<Frame>>>;

/// Shared sample buffer — holds the latest audio samples
type SharedSamples = Arc<Mutex<Option<AudioSamples>>>;

/// Reactive camera stream
///
/// Opens the camera via native bridge (mobile) or cpal+platform API (desktop).
/// Frames are delivered as RGBA data via a shared buffer.
/// Drop to stop capture.
pub struct CameraStream {
    latest: SharedFrame,
    #[cfg(not(any(target_os = "android", target_os = "ios", target_arch = "wasm32")))]
    _handle: Option<std::thread::JoinHandle<()>>,
    #[cfg(any(target_os = "android", target_os = "ios", target_arch = "wasm32"))]
    _bridge_stream: Option<blinc_core::native_bridge::NativeStream>,
    active: Arc<std::sync::atomic::AtomicBool>,
}

impl CameraStream {
    /// Open the camera and start capturing frames
    pub fn open(config: CameraConfig) -> Self {
        let latest: SharedFrame = Arc::new(Mutex::new(None));
        let active = Arc::new(std::sync::atomic::AtomicBool::new(true));

        #[cfg(not(any(target_os = "android", target_os = "ios", target_arch = "wasm32")))]
        {
            // Desktop: no built-in camera API — use native bridge or placeholder
            tracing::info!(
                "Camera requested: {}x{} @ {}fps (desktop — use native bridge for real capture)",
                config.width,
                config.height,
                config.fps,
            );
            Self {
                latest,
                _handle: None,
                active,
            }
        }

        #[cfg(any(target_os = "android", target_os = "ios", target_arch = "wasm32"))]
        {
            // Mobile: open camera via native bridge stream
            let latest_for_stream = Arc::clone(&latest);
            let w = config.width;
            let h = config.height;

            let stream = blinc_core::native_bridge::native_stream(
                "camera",
                "preview",
                vec![
                    blinc_core::native_bridge::NativeValue::Int32(w as i32),
                    blinc_core::native_bridge::NativeValue::Int32(h as i32),
                    blinc_core::native_bridge::NativeValue::Int32(config.fps as i32),
                    blinc_core::native_bridge::NativeValue::Int32(
                        if config.facing == CameraFacing::Front {
                            0
                        } else {
                            1
                        },
                    ),
                ],
                move |data| {
                    // Platform sends RGBA bytes as NativeValue::Bytes
                    if let Some(bytes) = data.as_bytes() {
                        let frame = Frame::from_rgba(bytes.to_vec(), w, h);
                        *latest_for_stream.lock().unwrap() = Some(frame);
                    }
                },
            );

            Self {
                latest,
                _bridge_stream: Some(stream),
                active,
            }
        }
    }

    /// Get the latest camera frame (None if no frame yet)
    pub fn latest_frame(&self) -> Option<Frame> {
        self.latest.lock().unwrap().clone()
    }

    /// Push a frame externally (for desktop testing or custom capture)
    pub fn push_frame(&self, frame: Frame) {
        *self.latest.lock().unwrap() = Some(frame);
    }

    /// Check if the stream is active
    pub fn is_active(&self) -> bool {
        self.active.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Drop for CameraStream {
    fn drop(&mut self) {
        self.active
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Reactive audio recorder
///
/// Captures audio from the device microphone.
/// Desktop: via cpal. Mobile: via native bridge.
/// Drop to stop recording.
pub struct AudioRecorder {
    latest: SharedSamples,
    /// Native audio capture stream. Sourced via `rodio::cpal` (the
    /// re-exported `cpal` from rodio's `recording` feature) so the
    /// workspace winds up with a single cpal version instead of the
    /// pre-rodio-0.22 split between rodio's bundled cpal and a
    /// standalone cpal dep.
    #[cfg(not(any(target_os = "android", target_os = "ios", target_arch = "wasm32")))]
    _stream: Option<rodio::cpal::Stream>,
    #[cfg(any(target_os = "android", target_os = "ios", target_arch = "wasm32"))]
    _bridge_stream: Option<blinc_core::native_bridge::NativeStream>,
    active: Arc<std::sync::atomic::AtomicBool>,
}

impl AudioRecorder {
    /// Open the microphone and start recording
    pub fn open(config: AudioRecorderConfig) -> Self {
        let latest: SharedSamples = Arc::new(Mutex::new(None));
        let active = Arc::new(std::sync::atomic::AtomicBool::new(true));

        #[cfg(not(any(target_os = "android", target_os = "ios", target_arch = "wasm32")))]
        {
            use rodio::cpal;
            use rodio::cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

            let host = cpal::default_host();
            let device = host.default_input_device();

            let stream = device.and_then(|dev| {
                let stream_config = cpal::StreamConfig {
                    channels: config.channels,
                    sample_rate: config.sample_rate,
                    buffer_size: cpal::BufferSize::Default,
                };

                let latest_for_cb = Arc::clone(&latest);
                let channels = config.channels;
                let sample_rate = config.sample_rate;

                dev.build_input_stream(
                    &stream_config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        let samples = AudioSamples::from_f32(data, channels, sample_rate);
                        *latest_for_cb.lock().unwrap() = Some(samples);
                    },
                    |err| {
                        tracing::error!("Audio capture error: {}", err);
                    },
                    None,
                )
                .ok()
            });

            if let Some(ref s) = stream {
                let _ = s.play();
            }

            Self {
                latest,
                _stream: stream,
                active,
            }
        }

        #[cfg(any(target_os = "android", target_os = "ios", target_arch = "wasm32"))]
        {
            let latest_for_stream = Arc::clone(&latest);
            let channels = config.channels;
            let sample_rate = config.sample_rate;

            let stream = blinc_core::native_bridge::native_stream(
                "audio",
                "record",
                vec![
                    blinc_core::native_bridge::NativeValue::Int32(sample_rate as i32),
                    blinc_core::native_bridge::NativeValue::Int32(channels as i32),
                ],
                move |data| {
                    if let Some(bytes) = data.as_bytes() {
                        // Platform sends PCM f32 samples as raw bytes
                        let float_samples: Vec<f32> = bytes
                            .chunks_exact(4)
                            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                            .collect();
                        let samples = AudioSamples::from_f32(&float_samples, channels, sample_rate);
                        *latest_for_stream.lock().unwrap() = Some(samples);
                    }
                },
            );

            Self {
                latest,
                _bridge_stream: Some(stream),
                active,
            }
        }
    }

    /// Get the latest audio samples (None if no data yet)
    pub fn latest_samples(&self) -> Option<AudioSamples> {
        self.latest.lock().unwrap().clone()
    }

    /// Push samples externally (for testing or custom capture)
    pub fn push_samples(&self, samples: AudioSamples) {
        *self.latest.lock().unwrap() = Some(samples);
    }

    /// Check if recording is active
    pub fn is_active(&self) -> bool {
        self.active.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Drop for AudioRecorder {
    fn drop(&mut self) {
        self.active
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }
}
