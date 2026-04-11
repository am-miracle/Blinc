# Mobile Development

Blinc supports building native mobile applications for both Android and iOS. The same Rust UI code runs on mobile with platform-specific rendering backends (Vulkan for Android, Metal for iOS) and a unified API for native platform features.

## Cross-Platform Architecture

```text
┌─────────────────────────────────────────────────────────────┐
│                      Your Blinc App                          │
│         (Shared Rust UI code, state, animations)             │
└─────────────────────────────┬───────────────────────────────┘
                              │
         ┌────────────────────┼────────────────────┐
         │                    │                    │
    ┌────▼────┐         ┌─────▼─────┐        ┌────▼────┐
    │ Desktop │         │  Android  │        │   iOS   │
    │ (wgpu)  │         │ (Vulkan)  │        │ (Metal) │
    └─────────┘         └───────────┘        └─────────┘
```

## Key Features

- **Shared UI Code**: Write your UI once in Rust, deploy everywhere
- **Native Performance**: GPU-accelerated rendering via Vulkan/Metal
- **Touch Support**: Full multi-touch gesture handling
- **Native Bridge**: Typed function-call protocol between Rust and Kotlin/Swift
- **Reactive State**: Same reactive state system as desktop
- **Animations**: Spring physics and keyframe animations work seamlessly

## Supported Platforms

| Platform | Backend | Min Version  | Status |
|----------|---------|--------------|--------|
| Android  | Vulkan  | API 24 (7.0) | Stable |
| iOS      | Metal   | iOS 15+      | Stable |

## Project Structure

```text
my-app/
├── Cargo.toml           # Rust dependencies
├── blinc.toml           # Blinc project config
├── src/
│   └── main.rs          # Shared UI code
├── platforms/
│   ├── android/         # Android-specific files
│   │   ├── app/
│   │   │   └── src/main/
│   │   │       ├── AndroidManifest.xml
│   │   │       └── kotlin/.../MainActivity.kt
│   │   └── build.gradle.kts
│   └── ios/             # iOS-specific files
│       ├── BlincApp/
│       │   ├── AppDelegate.swift
│       │   ├── BlincViewController.swift
│       │   └── Info.plist
│       └── BlincApp.xcodeproj/
└── build-android.sh     # Build scripts
```

## Quick Start

```bash
blinc new my-app --template rust
cd my-app
blinc run android   # or: blinc run ios
```

```rust
use blinc_app::prelude::*;

fn app(ctx: &mut WindowedContext) -> impl ElementBuilder {
    let count = ctx.use_state_keyed("count", || 0i32);

    div()
        .w(ctx.width).h(ctx.height)
        .bg(Color::from_hex(0x1a1a2e))
        .flex_col().items_center().justify_center().gap(20.0)
        .child(text(format!("Count: {}", count.get())).size(48.0).color(Color::WHITE))
        .child(
            button(state.clone(), "+")
                .on_click(move |_| count.set(count.get() + 1))
        )
}
```

---

## Native Bridge

Blinc's native bridge provides a typed function-call protocol between Rust and Kotlin/Swift. Use it for any platform feature not in the framework core: camera, biometrics, push notifications, native dialogs, etc.

### Rust side — call into native

```rust
use blinc_core::native_bridge::native_call;

// Synchronous call returning a value
let level: String = native_call("device", "get_battery_level", ())?;

// Pass arguments
native_call::<(), _>("notify", "show", ("Hello", "World"))?;

// Built-in haptic helpers
native_call::<(), _>("haptics", "selection", ())?;
native_call::<(), _>("haptics", "impact", (1i32,))?; // 0=light, 1=medium, 2=heavy
native_call::<(), _>("haptics", "success", ())?;
```

### Kotlin side — register handlers

```kotlin
// MainActivity.kt — call once during onCreate
BlincNativeBridge.init(this)
BlincNativeBridge.registerDefaults(context)  // built-in: haptics, device info

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

### Swift side — register handlers

```swift
// AppDelegate.swift — call once during app launch
BlincNativeBridge.shared.connectToRust()
BlincNativeBridge.shared.registerDefaults()

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

---

## Streams (camera, audio, sensors)

Streams deliver continuous data (frames, samples, sensor readings) from the platform back to Rust without polling. The platform pushes data via `dispatch_stream_data`, which fires the registered Rust callback. Drop the returned `NativeStream` handle to stop the stream and release resources.

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

The platform side calls `nativeDispatchStreamData(streamId, byteArray)` (Android JNI) or `blinc_dispatch_stream_data(stream_id, ptr, len)` (iOS C FFI) to push data into the Rust callback.

### Camera capture

`CameraStream` from `blinc_media` wraps the bridge stream API in a typed reactive interface:

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

The platform side uses `Camera2` (Android) or `AVCaptureSession` (iOS) and pushes frames through the native bridge stream protocol.

> **Note**: A complete camera demo example is on the roadmap. The API surface above is stable.

### Audio recording

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

Platform side: `AudioRecord` (Android) or `AVAudioRecorder` (iOS) streams 16-bit PCM through the bridge.

---

## Deep Linking

Blinc Router auto-handles deep links — no manual wiring required after `RouterBuilder::build()`.

### Rust — define routes

```rust
use blinc_router::RouterBuilder;

let router = RouterBuilder::new()
    .route("/", home_page)
    .route("/users/:id", user_detail)
    .route("/products/:slug", product_page)
    .build();

// router is auto-wired to dispatch_deep_link
// myapp://users/42 → router.push("/users/42") → user_detail({id: "42"})
```

### Android — forward intents to Rust

```kotlin
// MainActivity.kt
override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    intent.data?.toString()?.let { uri ->
        nativeDispatchDeepLink(uri)
    }
}

external fun nativeDispatchDeepLink(uri: String)
```

### iOS — forward URLs to Rust

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

The system back button is also auto-registered: `Key::Back` events route through `router.back()`.

---

## App Lifecycle

```rust
use blinc_platform::event::{Event, LifecycleEvent};

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

| Blinc Event | Android | iOS |
|---|---|---|
| `Resumed` | `MainEvent::Resume` | `applicationDidBecomeActive` |
| `Suspended` | `MainEvent::Pause` | `applicationWillResignActive` |
| `LowMemory` | `MainEvent::LowMemory` | `applicationDidReceiveMemoryWarning` |

---

## Soft Keyboard

Text input widgets (`text_input()`, `text_area()`) automatically show/hide the soft keyboard on focus. The keyboard inset is reported back via `WindowedContext.safe_bottom()` so your layout can adjust.

```rust
text_input(state)
    .placeholder("Type something...")
```

Implementation:
- **Android**: keyboard show/hide commands dispatched via the native bridge under `keyboard.show` / `keyboard.hide`. Default handlers (registered by `BlincNativeBridge.registerDefaults`) call `InputMethodManager.showSoftInput` / `hideSoftInputFromWindow`.
- **iOS**: `blinc_ios_show_keyboard()` / `blinc_ios_hide_keyboard()` C FFI invoked from the frame loop. Inset reported back via `blinc_ios_set_keyboard_inset(ctx, inset)` from a `keyboardWillShow` observer.

---

## Edit Menu (iOS 16+)

Text input widgets automatically integrate with `UIEditMenuInteraction` on iOS 16+. Long-press a text field to see the system Cut/Copy/Paste/Select menu — no manual wiring required. The native bridge handles `UIPasteboard` clipboard read/write, menu presentation, and word selection.

---

## Safe Area Insets

```rust
pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder {
    div()
        .w(ctx.width).h(ctx.height)
        .pt(ctx.safe_top())     // status bar / notch
        .pb(ctx.safe_bottom())  // home indicator / gesture bar
        .pl(ctx.safe_left())    // landscape notch
        .pr(ctx.safe_right())
        .child(/* ... */)
}
```

`safe_area` is populated at context creation by calling `window.safe_area_insets()`. On desktop this returns zeros; on mobile, the platform's window implementation supplies real values from the platform API.

---

## Touch Event Handling

Touch events are automatically routed to your UI:

| Android Action | iOS Phase | Blinc Event |
|---|---|---|
| `ACTION_DOWN` | `touchesBegan` | `pointer_down` |
| `ACTION_MOVE` | `touchesMoved` | `pointer_move` |
| `ACTION_UP` | `touchesEnded` | `pointer_up` + `pointer_leave` |
| `ACTION_CANCEL` | `touchesCancelled` | `pointer_leave` |

Two-finger pinch gestures emit `PINCH` events with center + scale. Use `.on_pinch()` and `.on_rotate()` on a `Div` to receive them.

---

## Next Steps

- [Android Development](./android.md) — Toolchain setup, build commands, manifest configuration, debugging
- [iOS Development](./ios.md) — Toolchain setup, build commands, Xcode configuration, debugging
- [CLI Reference](./cli.md) — Full CLI command reference
