# Audio & Video

The `blinc_media` crate provides cross-platform audio/video with royalty-free codecs.
Media widgets in `blinc_layout` (behind the `media` feature) provide player UIs.

## Audio Playback

```rust
use blinc_media::{AudioPlayer, AudioSource};

let player = AudioPlayer::new();
player.play(AudioSource::file("music.ogg"));
player.set_volume(0.8);
player.pause();
player.seek(30_000); // seek to 30s
player.resume();

println!("Position: {}ms", player.position_ms());
```

Desktop: Vorbis, WAV, FLAC via rodio. Mobile: platform codecs via native bridge.

## Video Playback

```rust
use blinc_media::{VideoPlayer, VideoDecoder};

let mut decoder = VideoDecoder::new();
let player = VideoPlayer::new();

// Decode H.264 NAL units → RGBA frames
if let Some(frame) = decoder.decode_nal(h264_packet) {
    player.push_frame(frame);
}

player.play();
player.seek(10_000);
```

Desktop: OpenH264 (royalty-free). Mobile: platform decoders via native bridge.

## Player Trait

Both players implement the shared `Player` trait:

```rust
use blinc_media::Player;

fn show_status(p: &dyn Player) {
    println!("{} / {} | vol: {}", p.position_ms(), p.duration_ms(), p.volume());
}
```

| Method | Description |
|--------|-------------|
| `play()` / `pause()` / `stop()` | Playback controls |
| `seek(ms)` | Seek to position |
| `position_ms()` / `duration_ms()` | Time tracking |
| `volume()` / `set_volume(f32)` | Volume (0.0–1.0) |
| `is_playing()` / `is_live()` | State queries |

## Audio Widget

```rust
use std::rc::Rc;
use blinc_layout::widgets::media::audio_player;

let player = Rc::new(AudioPlayer::new());

// Basic controls
audio_player(Rc::clone(&player)).w_full().into_div()

// With waveform
audio_player(Rc::clone(&player))
    .waveform_data(&samples)
    .w_full()
    .into_div()
```

## Video Widget

```rust
use std::rc::Rc;
use blinc_layout::widgets::media::video_player;

let player = Rc::new(VideoPlayer::new());

video_player(Rc::clone(&player))
    .show_dimensions()
    .w_full()
    .h(400.0)
    .into_div()
```

## Waveform

Standalone amplitude visualization:

```rust
use blinc_layout::widgets::media::waveform;

waveform(buckets)
    .progress(0.5)
    .played_color(Color::BLUE)
    .unplayed_color(Color::GRAY)
    .w_full().h(60.0)
    .into_div()
```

## Shared Controls

`MediaControls` is generic over `Player`:

```rust
use blinc_layout::widgets::media::MediaControls;

MediaControls::new(player_rc).class("my-controls").into_div()
```

Layout: `[ ▶ ] [ 1:23 / 3:45 ] [ ══seek══ ] [ 80% ]`

Live streams: `[ ▶ ] [ LIVE ] [ ════════════ ]`

## Camera & Recording

```rust
use blinc_media::rtc::{CameraStream, CameraConfig, AudioRecorder};

let camera = CameraStream::open(CameraConfig::default());
let frame = camera.latest_frame(); // RGBA Frame

let recorder = AudioRecorder::open(Default::default());
let samples = recorder.latest_samples(); // AudioSamples

drop(camera);   // stops capture
drop(recorder);  // stops recording
```

## Frame Utilities

```rust
use blinc_media::{Frame, AudioSamples};

// Video
let small = Frame::from_rgba(data, 640, 480).scale(320, 240);
let gray = small.to_gray();

// Audio
let mono = AudioSamples::from_f32(&pcm, 2, 44100).to_mono();
let resampled = mono.resample(48000);
```

## CSS Classes

| Class | Element |
|-------|---------|
| `.blinc-audio-player` | Audio container |
| `.blinc-video-player` | Video container |
| `.blinc-audio-waveform` | Waveform canvas |
| `.blinc-media-controls` | Controls row |
| `.blinc-media-play-btn` | Play/pause |
| `.blinc-media-time` | Time display |
| `.blinc-media-live-badge` | LIVE indicator |
| `.blinc-media-seek-track` | Seek bar |
| `.blinc-media-seek-fill` | Seek progress |
| `.blinc-media-volume` | Volume |

## Licensing

Desktop uses royalty-free codecs only — no ffmpeg, no patent fees:

| Codec | License |
|-------|---------|
| Vorbis, WAV, FLAC | BSD / Public domain |
| OpenH264 | BSD, Cisco covers patents |

Mobile uses OS-provided codecs (licensing handled by the OS).
