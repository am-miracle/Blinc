# Changelog

All notable changes to `blinc_media` will be documented in this file.

## [0.4.0] - 2026-04-05

### Added
- `AudioPlayer` — Desktop: rodio (Vorbis/WAV/FLAC), Mobile: native bridge
- `VideoDecoder` — Desktop: OpenH264 (H.264 NAL to RGBA), Mobile: native bridge
- `VideoPlayer` — play/pause/seek/volume, frame push API
- `CameraStream` — RTC-like reactive capture with RGBA frame delivery
- `AudioRecorder` — Desktop: cpal, Mobile: native bridge stream
- `Player` trait shared by `AudioPlayer`, `VideoPlayer`, and live streams
- `Player::is_live()` for live stream detection (LIVE badge, seek-less controls)
- `Frame` struct: RGBA/RGB/BGRA/YUV420/Gray conversion, scale, format convert
- `AudioSamples` struct: f32/i16/u8 conversion, resample, mono downmix
- `audio_player()` widget — waveform canvas, `MediaControls` via `Player` trait
- `video_player()` widget — canvas surface, shared controls, dimensions
- All widget elements have CSS classes (`.blinc-media-*`, `.blinc-audio-*`, `.blinc-video-*`)
