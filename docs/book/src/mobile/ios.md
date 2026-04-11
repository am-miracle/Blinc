# iOS Development

This guide covers building Blinc apps for iOS — toolchain setup, native bridge, camera/audio streams, deep linking, lifecycle, and platform integration.

## Prerequisites

### 1. Xcode

Install Xcode 15+ from the App Store.

```bash
xcode-select -p
```

### 2. Rust Targets

```bash
rustup target add aarch64-apple-ios        # Device
rustup target add aarch64-apple-ios-sim    # Simulator (Apple Silicon)
rustup target add x86_64-apple-ios         # Simulator (Intel)
```

## Building

```bash
#!/bin/bash
# build-ios.sh
set -e
MODE=${1:-debug}
PROJECT_NAME="my_app"
[ "$MODE" = "release" ] && CARGO_FLAGS="--release" || CARGO_FLAGS=""
TARGET_DIR=$([ "$MODE" = "release" ] && echo "release" || echo "debug")

cargo build --target aarch64-apple-ios $CARGO_FLAGS
cargo build --target aarch64-apple-ios-sim $CARGO_FLAGS

mkdir -p platforms/ios/libs/{device,simulator}
cp target/aarch64-apple-ios/$TARGET_DIR/lib${PROJECT_NAME}.a platforms/ios/libs/device/
cp target/aarch64-apple-ios-sim/$TARGET_DIR/lib${PROJECT_NAME}.a platforms/ios/libs/simulator/
```

```bash
./build-ios.sh         # debug
./build-ios.sh release
```

Then open `platforms/ios/BlincApp.xcodeproj` in Xcode and press Cmd+R.

## Project Configuration

### Cargo.toml

```toml
[lib]
name = "my_app"
crate-type = ["cdylib", "staticlib"]

[target.'cfg(target_os = "ios")'.dependencies]
blinc_app = { version = "0.5", features = ["ios"] }
blinc_platform_ios = "0.5"
```

### Xcode Build Settings

1. **Link static library**: Build Phases → Link Binary With Libraries → add `libmy_app.a`
2. **Bridging header**: Build Settings → Objective-C Bridging Header → `BlincApp/Blinc-Bridging-Header.h`
3. **Frameworks**: Metal, MetalKit, QuartzCore, AVFoundation (camera), CoreHaptics

### Info.plist

```xml
<!-- Camera + microphone permissions -->
<key>NSCameraUsageDescription</key>
<string>This app uses the camera for photo capture.</string>
<key>NSMicrophoneUsageDescription</key>
<string>This app records audio.</string>

<!-- Deep link URL scheme -->
<key>CFBundleURLTypes</key>
<array>
    <dict>
        <key>CFBundleURLSchemes</key>
        <array><string>myapp</string></array>
    </dict>
</array>
```

## Native Bridge

The native bridge provides a typed function-call protocol between Rust and Swift. Use it for any platform feature not in the framework core: camera, biometrics, native dialogs, push notifications, etc.

### Swift side — register handlers

```swift
// AppDelegate.swift — call once during app launch
BlincNativeBridge.shared.connectToRust()
BlincNativeBridge.shared.registerDefaults()  // built-in: haptics, device info, etc.

// Custom handler returning a string
BlincNativeBridge.shared.registerString(
    namespace: "device",
    name: "get_battery_level"
) { _ in
    UIDevice.current.isBatteryMonitoringEnabled = true
    return String(Int(UIDevice.current.batteryLevel * 100))
}

// Handler returning Void
BlincNativeBridge.shared.registerVoid(
    namespace: "notify",
    name: "show"
) { args in
    let title = args[0] as? String ?? ""
    let body = args[1] as? String ?? ""
    NotificationHelper.show(title: title, body: body)
}
```

### Rust side — call into native

```rust
use blinc_core::native_bridge::native_call;

// Synchronous call
let level: String = native_call("device", "get_battery_level", ())?;

// Pass arguments
native_call::<(), _>("notify", "show", ("Hello", "World"))?;

// Built-in haptic helpers (UIImpactFeedbackGenerator under the hood)
native_call::<(), _>("haptics", "selection", ())?;
native_call::<(), _>("haptics", "impact", (1i32,))?; // 0=light, 1=medium, 2=heavy
native_call::<(), _>("haptics", "success", ())?;
native_call::<(), _>("haptics", "warning", ())?;
native_call::<(), _>("haptics", "error", ())?;
```

### Streams (camera, audio, sensors)

Streams deliver continuous data without polling. Swift pushes data via `blinc_dispatch_stream_data`, which fires the registered Rust callback.

```rust
use blinc_core::native_bridge::{native_stream, NativeValue};

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

```rust
use blinc_media::{CameraStream, CameraConfig, CameraFacing};

let camera = CameraStream::open(CameraConfig {
    width: 640,
    height: 480,
    fps: 30,
    facing: CameraFacing::Front,
});

if let Some(frame) = camera.latest_frame() {
    canvas(move |ctx, bounds| {
        ctx.draw_rgba_pixels(frame.as_rgba(), frame.width, frame.height, bounds);
    })
}

// drop(camera) stops capture and releases the AVCaptureSession
```

The Swift side uses `AVCaptureSession` + `AVCaptureVideoDataOutput` and pushes BGRA → RGBA frames into Rust via `blinc_dispatch_stream_data(stream_id, ptr, len)`.

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

The Swift side uses `AVAudioRecorder` or `AudioUnit` and streams 16-bit PCM through the bridge.

## Deep Linking

Blinc Router auto-handles deep links — no manual wiring required after `RouterBuilder::build()`.

### Swift — forward URLs to Rust

```swift
// AppDelegate.swift
func application(
    _ app: UIApplication,
    open url: URL,
    options: [UIApplication.OpenURLOptionsKey : Any] = [:]
) -> Bool {
    blinc_ios_handle_deep_link(url.absoluteString)
    return true
}

// SceneDelegate.swift (for scene-based apps)
func scene(_ scene: UIScene, openURLContexts URLContexts: Set<UIOpenURLContext>) {
    URLContexts.forEach { ctx in
        blinc_ios_handle_deep_link(ctx.url.absoluteString)
    }
}
```

### Rust — define routes

```rust
use blinc_router::{Router, RouterBuilder};

let router = RouterBuilder::new()
    .route("/", home_page)
    .route("/users/:id", user_detail)
    .route("/products/:slug", product_page)
    .build();

// router is auto-wired to dispatch_deep_link
// myapp://users/42 → router.push("/users/42") → user_detail({id: "42"})
```

## App Lifecycle

```rust
use blinc_platform::event::{Event, LifecycleEvent};

match event {
    Event::Lifecycle(LifecycleEvent::Resumed) => {
        camera.resume();
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

iOS lifecycle mapping:
- `applicationDidBecomeActive` → `LifecycleEvent::Resumed`
- `applicationWillResignActive` → `LifecycleEvent::Suspended`
- `applicationDidReceiveMemoryWarning` → `LifecycleEvent::LowMemory`

## Soft Keyboard

Text input widgets (`text_input()`, `text_area()`) automatically show/hide the soft keyboard on focus. The keyboard inset is reported back through the platform → Rust:

```c
// Bridging header — called by iOS when keyboard appears/disappears
void blinc_ios_set_keyboard_inset(IOSRenderContext* ctx, float inset);
```

```swift
// Keyboard observer
NotificationCenter.default.addObserver(forName: UIResponder.keyboardWillShowNotification, ...) { note in
    let frame = (note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? NSValue)?.cgRectValue ?? .zero
    blinc_ios_set_keyboard_inset(ctx, Float(frame.height))
}
```

The inset flows into `WindowedContext.safe_bottom()` so layouts can adjust above the keyboard.

## Edit Menu (iOS 16+)

Text input widgets natively integrate with `UIEditMenuInteraction` for iOS 16+. Long-press a text field to see the system Cut/Copy/Paste/Select menu — no manual wiring required.

The native bridge handles:
- `UIPasteboard` clipboard read/write
- `UIEditMenuInteraction` presentation
- Word selection on long-press

## Safe Area Insets

```rust
pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    div()
        .w(ctx.width).h(ctx.height)
        .pt(ctx.safe_top())     // status bar / notch
        .pb(ctx.safe_bottom())  // home indicator
        .pl(ctx.safe_left())
        .pr(ctx.safe_right())
        .child(/* ... */)
}
```

## Touch Event Handling

| iOS Phase | Blinc Event |
|---|---|
| `touchesBegan` | `pointer_down` |
| `touchesMoved` | `pointer_move` |
| `touchesEnded` | `pointer_up` + `pointer_leave` |
| `touchesCancelled` | `pointer_leave` |

Two-finger pinch gestures emit `PINCH` events with center + scale ratio. Use `.on_pinch()` on a `Div`.

## Debugging

### Console Logs

View Rust logs in Xcode's console or `Console.app`:

```
subsystem:com.blinc.my_app
```

### Common Issues

**"Library not found: -lmy_app"** — run `./build-ios.sh` first.

**Black screen on simulator** — verify the right simulator target (`aarch64-apple-ios-sim` for Apple Silicon, `x86_64-apple-ios` for Intel) and that the static library is in `libs/simulator/`.

**Touch events not working** — verify `blinc_create_context` succeeds (check console). Touch coordinates must be in logical points, not physical pixels.

**Native call failed** — verify Swift handler is registered with matching `namespace.name`. Check that `BlincNativeBridge.shared.connectToRust()` was called at app launch.

## Performance Tips

```toml
[profile.release]
lto = "fat"
opt-level = "z"
panic = "abort"
strip = true
codegen-units = 1
```

- **Test on real devices** — simulators use software rendering for some Metal operations
- **Profile with Instruments** — use the Metal System Trace template for GPU analysis
- **Use `release-small` profile** for App Store submissions
