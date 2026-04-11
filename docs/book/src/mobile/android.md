# Android Development

This guide covers building Blinc apps for Android — toolchain setup, native bridge, camera/audio streams, deep linking, lifecycle, and platform integration.

## Prerequisites

### 1. Android SDK & NDK

```bash
# macOS
brew install --cask android-studio

export ANDROID_HOME=$HOME/Library/Android/sdk
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/26.1.10909125
export PATH=$PATH:$ANDROID_HOME/platform-tools
```

### 2. Rust Targets

```bash
rustup target add aarch64-linux-android
rustup target add armv7-linux-androideabi
rustup target add x86_64-linux-android
cargo install cargo-ndk
```

## Building

```bash
# Debug — single arch
cargo ndk -t arm64-v8a build

# Release — multi-arch
cargo ndk -t arm64-v8a -t armeabi-v7a build --release

# Or via Gradle (from platforms/android/)
./gradlew assembleDebug
```

## Project Configuration

### Cargo.toml

```toml
[lib]
name = "my_app"
crate-type = ["cdylib", "staticlib"]

[target.'cfg(target_os = "android")'.dependencies]
blinc_app = { version = "0.5", features = ["android"] }
blinc_platform_android = "0.5"
android-activity = { version = "0.6", features = ["native-activity"] }
log = "0.4"
android_logger = "0.14"
```

### AndroidManifest.xml

```xml
<manifest xmlns:android="http://schemas.android.com/apk/res/android">
    <uses-feature android:glEsVersion="0x00030000" android:required="true" />
    <uses-permission android:name="android.permission.CAMERA" />
    <uses-permission android:name="android.permission.RECORD_AUDIO" />
    <uses-permission android:name="android.permission.VIBRATE" />

    <application
        android:label="My App"
        android:theme="@android:style/Theme.DeviceDefault.NoActionBar.Fullscreen"
        android:hardwareAccelerated="true">

        <activity
            android:name=".MainActivity"
            android:configChanges="orientation|screenSize|keyboardHidden"
            android:exported="true"
            android:launchMode="singleTask">

            <meta-data android:name="android.app.lib_name" android:value="my_app" />

            <intent-filter>
                <action android:name="android.intent.action.MAIN" />
                <category android:name="android.intent.category.LAUNCHER" />
            </intent-filter>

            <!-- Deep link: myapp://path/to/route -->
            <intent-filter android:autoVerify="true">
                <action android:name="android.intent.action.VIEW" />
                <category android:name="android.intent.category.DEFAULT" />
                <category android:name="android.intent.category.BROWSABLE" />
                <data android:scheme="myapp" />
            </intent-filter>
        </activity>
    </application>
</manifest>
```

## Native Bridge

Blinc's native bridge lets Rust call into Kotlin (and vice versa) via a typed function-call protocol. Use it for any platform feature that's not in the framework core: camera, biometrics, push notifications, native dialogs, etc.

### Kotlin side — register handlers

```kotlin
// MainActivity.kt — call once during onCreate after BlincNativeBridge.init(this)
BlincNativeBridge.init(this)
BlincNativeBridge.registerDefaults(context)  // built-in handlers (haptics, device info, etc.)

// Custom handler returning a string
BlincNativeBridge.registerString("device", "get_battery_level") {
    val bm = context.getSystemService(Context.BATTERY_SERVICE) as BatteryManager
    bm.getIntProperty(BatteryManager.BATTERY_PROPERTY_CAPACITY).toString()
}

// Handler returning Unit
BlincNativeBridge.registerVoid("notify", "show") { args ->
    val title = args.getString(0)
    val body = args.getString(1)
    NotificationHelper.show(context, title, body)
}
```

### Rust side — call into native

```rust
use blinc_core::native_bridge::{native_call, NativeValue};

// Synchronous call
let level: String = native_call("device", "get_battery_level", ())?;
println!("Battery: {}%", level);

// Pass arguments
native_call::<(), _>("notify", "show", ("Hello", "World"))?;

// Built-in haptic helpers
native_call::<(), _>("haptics", "selection", ())?;
native_call::<(), _>("haptics", "impact", (1i32,))?; // 0=light, 1=medium, 2=heavy
native_call::<(), _>("haptics", "success", ())?;
```

### Streams (camera, audio, sensors)

Streams deliver continuous data (frames, samples, sensor readings) from the platform back to Rust without polling. The platform pushes data via `dispatch_stream_data`, which fires the registered callback.

```rust
use blinc_core::native_bridge::{native_stream, NativeValue};

// Open a stream — keep the handle alive; drop stops the stream
let stream = native_stream(
    "sensors",
    "accelerometer",
    NativeValue::Null,
    |data| {
        if let Some(arr) = data.as_array() {
            let x = arr[0].as_f32().unwrap_or(0.0);
            let y = arr[1].as_f32().unwrap_or(0.0);
            let z = arr[2].as_f32().unwrap_or(0.0);
            println!("accel: {x}, {y}, {z}");
        }
    },
)?;
// drop(stream) → stream stops
```

## Camera Capture

`CameraStream` from `blinc_media` wraps the native bridge stream API in a typed reactive interface:

```rust
use blinc_media::{CameraStream, CameraConfig, CameraFacing};

let camera = CameraStream::open(CameraConfig {
    width: 640,
    height: 480,
    fps: 30,
    facing: CameraFacing::Front,
});

// Read latest frame in build_ui
if let Some(frame) = camera.latest_frame() {
    canvas(move |ctx, bounds| {
        ctx.draw_rgba_pixels(frame.as_rgba(), frame.width, frame.height, bounds);
    })
}

// drop(camera) stops capture and releases the device
```

The camera frames flow through the native bridge stream protocol: Kotlin's `Camera2` API delivers `Image` buffers, the bridge converts them to RGBA, and `JNIEnv::nativeDispatchStreamData(streamId, byteArray)` ferries the bytes into Rust.

See `crates/blinc_app/examples/notch_demo.rs` for a production camera example.

## Audio Recording

```rust
use blinc_media::{AudioRecorder, AudioRecorderConfig};

let recorder = AudioRecorder::open(AudioRecorderConfig {
    sample_rate: 44100,
    channels: 1,
});

if let Some(samples) = recorder.latest_samples() {
    process_audio(samples.as_f32());
}
```

The Kotlin side uses `AudioRecord` and pushes 16-bit PCM through the same stream protocol.

## Deep Linking

Blinc Router auto-handles deep links — no manual wiring required after `RouterBuilder::build()`.

### Kotlin — forward intents to Rust

```kotlin
// MainActivity.kt
override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    intent.data?.toString()?.let { uri ->
        nativeDispatchDeepLink(uri)
    }
}

// External JNI declaration
external fun nativeDispatchDeepLink(uri: String)
```

### Rust — define routes

```rust
use blinc_router::{Router, RouterBuilder};

let router = RouterBuilder::new()
    .route("/", home_page)
    .route("/users/:id", user_detail)
    .route("/products/:slug", product_page)
    .build();

// router is now wired to handle blinc_router::dispatch_deep_link(uri)
// A URL like myapp://users/42 → router.push("/users/42") → user_detail({id: "42"})
```

The system back button is also auto-registered: `Key::Back` events route through `router.back()`.

## App Lifecycle

```rust
use blinc_platform::event::{Event, LifecycleEvent};

// In your event handler:
match event {
    Event::Lifecycle(LifecycleEvent::Resumed) => {
        camera.resume();
        analytics.session_start();
    }
    Event::Lifecycle(LifecycleEvent::Suspended) => {
        camera.pause();
        save_state();
    }
    Event::Lifecycle(LifecycleEvent::LowMemory) => {
        clear_image_cache();
    }
    _ => {}
}
```

Mapping:
- `MainEvent::Resume` → `LifecycleEvent::Resumed`
- `MainEvent::Pause` → `LifecycleEvent::Suspended`
- `MainEvent::LowMemory` → `LifecycleEvent::LowMemory`

## Soft Keyboard

Text input widgets (`text_input()`, `text_area()`) automatically show/hide the soft keyboard on focus. The keyboard inset is reported back via `WindowedContext.safe_bottom()` so your layout can adjust.

```rust
text_input(state)
    .placeholder("Type something...")
    .on_focus(|| println!("keyboard shown"))
```

The keyboard show/hide commands are dispatched via the native bridge under `keyboard.show` / `keyboard.hide`. The default implementations (registered by `BlincNativeBridge.registerDefaults`) call `InputMethodManager.showSoftInput` / `hideSoftInputFromWindow`.

## Safe Area Insets

```rust
pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    div()
        .w(ctx.width).h(ctx.height)
        .pt(ctx.safe_top())     // status bar
        .pb(ctx.safe_bottom())  // gesture bar / nav buttons
        .pl(ctx.safe_left())    // landscape notch
        .pr(ctx.safe_right())
        .child(/* ... */)
}
```

## Touch Event Handling

Touch events are automatically routed:

| Android Action | Blinc Event |
|---|---|
| `ACTION_DOWN` | `pointer_down` |
| `ACTION_MOVE` | `pointer_move` |
| `ACTION_UP` | `pointer_up` + `pointer_leave` |
| `ACTION_CANCEL` | `pointer_leave` |

Two-finger pinch gestures emit `PINCH` events with center + scale delta. Use `.on_pinch()` on a `Div` to receive them.

## Debugging

```bash
# View Rust logs
adb logcat | grep -E "(blinc|BlincApp)"

# Filter for native bridge calls
adb logcat | grep BlincNativeBridge
```

### Common Issues

**"Library not found"** — ensure the native library is in `app/src/main/jniLibs/<arch>/`. `cargo ndk` writes to `target/<rust-target>/`; copy to jniLibs or use the Gradle plugin.

**"Vulkan not supported"** — check device capability with `adb shell getprop ro.hardware.vulkan`. API 24+ devices generally support Vulkan, but emulators may not.

**"Native call failed"** — verify the namespace+name matches between Kotlin and Rust. Check logcat for `BlincNativeBridge: handler not found for X.Y`.

## Performance Tips

```toml
[profile.release]
lto = "fat"
opt-level = "z"      # optimize for size on mobile
panic = "abort"
strip = true
codegen-units = 1
```

- **Test on real devices** — emulators have different GPU characteristics
- **Profile with `Android Studio Profiler`** for CPU/GPU/memory
- **Bundle assets via `assets/`** — `AndroidAssetLoader` auto-resolves them through the platform `AssetLoader` trait
