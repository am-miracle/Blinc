# Blinc Roadmap

> Last updated: 2026-04-04

## Vision

Blinc is a GPU-accelerated, cross-platform UI framework that enables developers to build production-quality desktop, mobile, and embedded applications from a single Rust codebase. The framework aims to match the quality of native platform UIs while providing a unified developer experience across all targets.

---

## Phase 1: Desktop Production Readiness

> Goal: Make Blinc viable for shipping real desktop applications.

### 1.1 System Integration (P0)

| Feature | Status | Notes |
|---------|--------|-------|
| File dialogs (open/save/folder) | **Done** | Via `rfd` crate, `blinc_app::dialog` module |
| System tray / status bar icon | **Done** | `blinc_app::tray::TrayIconBuilder` via `tray-icon` + `muda` |
| Native OS notifications | **Done** | `blinc_app::notify::Notification` via `notify-rust` |
| Drag and drop | **Done** | Window-level `on_file_drop()` + element-level `.on_file_drop()` on Div |
| Clipboard (rich content) | **Done** | Cross-platform text + image via `arboard` crate |
| Global keyboard shortcuts | **Done** | `blinc_app::hotkey::GlobalHotkey` via `global-hotkey` |

### 1.2 Window Management (P0)

| Feature | Status | Notes |
|---------|--------|-------|
| Multi-window support | **Done** | `ctx.open_window(config)`, `WindowState`, `AppCommand` event loop |
| Window min/max size constraints | **Done** | `WindowConfig::min_size()`, `max_size()` |
| Programmatic window positioning | **Done** | `set_position()`, `center()`, `set_size()` on Window trait |
| Window state persistence | **Done** | `WindowStateStore` with JSON save/load |
| Modal window API | **Done** | `WindowConfig::modal()`, input blocking on non-modal windows |
| Custom title bar / drag regions | **Done** | `.drag_region()` on Div, `window_actions` module |

### 1.3 Input (P1)

| Feature | Status | Notes |
|---------|--------|-------|
| IME / compose input | **Done** | Winit Ime::Commit routed as Key::Char events, `set_ime_allowed(true)` |
| Context menu wiring | **Done** | `.on_right_click()` / `.on_context_menu()` on Div |
| Trackpad gestures (pinch/rotate) | **Done** | `.on_pinch()` / `.on_rotate()` on Div, winit gesture events |

### 1.4 Missing Widgets (P1)

| Widget | Status | Notes |
|--------|--------|-------|
| Date picker | Planned | Calendar grid + input |
| Time picker | Planned | Clock face or dropdown |
| Color picker | Planned | HSL/RGB wheel + swatches |
| Range slider (dual thumb) | Planned | Extend existing slider |
| Number input (stepper) | Planned | text_input + increment/decrement |
| Data grid | Planned | Sortable, filterable table |
| Virtualized list | **Done** | `virtual_list(count, builder)` — variable-height items, CSS classes, flexbox layout |
| Rich text editor | Planned | Beyond code editor, styled content |

---

## Phase 2: Mobile Production Readiness

> Goal: Bring mobile targets to feature parity with desktop.

### 2.1 Input & Interaction (P0)

| Feature | Status | Notes |
|---------|--------|-------|
| Soft keyboard show/hide | **Done** | Atomic flags polled in frame loop |
| IME / compose input (mobile) | Partial | Basic text input works, full compose candidates need InputConnection/UITextInput |
| Gesture recognizers | **Done** | `GestureExt` trait: `.on_tap()`, `.on_long_press()`, `.on_swipe()` + pinch/rotate |
| Pull-to-refresh | **Done** | `pull_to_refresh(callback)` widget with threshold, max pull, indicator |
| Safe area insets | **Done** | `ctx.safe_area`, `safe_top/right/bottom/left()`, `safe_width/height()` |
| Haptic feedback | Done | Native bridge handlers on both platforms |

### 2.2 Platform Integration (P1)

| Feature | Status | Notes |
|---------|--------|-------|
| Deep linking / URL schemes | **Done** | `blinc_router::handle_deep_link()` + platform `DeepLink` parser |
| App lifecycle | **Done** | Resumed/Suspended/LowMemory events, Android full lifecycle mapping |
| Native bridge API (Rust side) | **Done** | `native_call()`, `native_register()`, `PlatformAdapter` trait in blinc_core |
| Native bridge streams | **Done** | `native_stream()` — bidirectional data (camera, audio, sensors) with auto-cleanup |

> **Push notifications, camera, location, biometrics, status bar theming** are implemented as **example native bridge extensions** — not in the framework core, but as documented templates that demonstrate the bridge API:
> - **Android**: `BlincNativeBridge.register("camera", "capture", handler)` in Kotlin
> - **iOS**: `BlincNativeBridge.shared.register(namespace:name:handler:)` in Swift
> - **Rust**: `native_call("camera", "capture", args)` (planned API)
>
> Blinc provides the bridge transport. Platform features ship as reference implementations users can copy and adapt.

### 2.3 Media (P1)

| Feature | Status | Notes |
|---------|--------|-------|
| Audio playback | **Done** | `AudioPlayer` — Desktop: rodio (Vorbis/WAV/FLAC), Mobile: native bridge |
| Video decoding | **Done** | `VideoDecoder` — Desktop: OpenH264 (H.264 NAL → RGBA), Mobile: native bridge |
| AV frame utilities | **Done** | `Frame` (RGBA/RGB/BGRA/YUV420/Gray, scale, convert) + `AudioSamples` (f32/i16/u8, resample, mono) |
| Audio recording | **Done** | `AudioRecorder` — Desktop: cpal, Mobile: native bridge stream |
| Video player | **Done** | `VideoPlayer` — play/pause/seek/volume, frame push API |
| Camera stream | **Done** | `CameraStream` — RTC-like reactive capture, RGBA frame delivery |
| Audio widget | **Done** | `audio_player()` — waveform canvas, `MediaControls` via `Player` trait |
| Video widget | **Done** | `video_player()` — canvas surface, shared controls, dimensions |
| Player trait | **Done** | Shared `Player` trait for `AudioPlayer` + `VideoPlayer` + live streams |
| Live streaming | **Done** | `Player::is_live()`, LIVE badge, seek-less controls |

> **Licensing**: Desktop uses royalty-free codecs only — OpenH264 (Cisco's BSD, patent costs covered by Cisco), VP9, AV1, Opus, Vorbis. No ffmpeg, no patent-encumbered codecs.
> Mobile uses platform-provided codecs (OS handles licensing).

| Example Extension | Status | Notes |
|-------------------|--------|-------|
| Push notifications | Planned | FCM/APNs example with bridge handlers |
| Camera capture | **Done** | `CameraStream` → native bridge → RGBA frames via `blinc_dispatch_stream_data` FFI |
| Location services | Planned | GPS via bridge example |
| Biometric auth | Planned | Fingerprint/Face ID bridge example |
| Status bar theming | Planned | Light/dark status bar bridge example |

### 2.3 Navigation & Router (`blinc_router` crate)

> See [docs/plans/blinc_router.md](docs/plans/blinc_router.md) for full design.
> See [docs/book/src/advanced/routing.md](docs/book/src/advanced/routing.md) for usage guide.

| Feature | Status | Notes |
|---------|--------|-------|
| Route definition & trie matching | **Done** | `/users/:id`, `*wildcard`, nested routes, O(depth) trie |
| Scoped `use_router()` hook | **Done** | Thread-local router stack, nested router support |
| History management | **Done** | `RouterHistory` with push/replace/back/forward |
| Page transitions | **Done** | `PageTransition` using `AnimationPreset` + `SpringConfig` |
| Navigation guards | **Done** | Auth guards with redirect/reject |
| Deep linking | **Done** | Auto-registered, zero-config: URI parsing + platform dispatch |
| Desktop deep links | **Done** | CLI `--deep-link=`, macOS/Windows/Linux URL scheme registration |
| iOS deep links | **Done** | `blinc_ios_handle_deep_link()` C FFI, auto-dispatched |
| Android deep links | **Done** | `dispatch_deep_link()` from JNI intent, auto-dispatched |
| System back button | **Done** | Auto-registered by `RouterBuilder::build()`, `Key::Back` dispatch |
| Named routing | **Done** | `push_named("user", &[("id", "42")])` reverse lookup |
| Route outlet | **Done** | `router.outlet()` builds current view with scoped context |
| Stack navigator | **Done** | `stack()` + `motion()` — documented integration pattern |
| Tab navigator | **Done** | `blinc_cn::tabs()` + `router.push()` — documented pattern |
| Bottom sheet navigation | **Done** | `blinc_cn::sheet()` + `router.outlet()` — documented pattern |
| Animation suspension | **Done** | `AnimatedValue::pause/resume`, `Spring::pause/resume` — old views auto-clean via Drop |
| Nested route stacks | **Done** | Sub-routers via `RouterBuilder`, `use_router()` returns innermost |

---

## Phase 3: Zyntax DSL (`.blinc` files)

> Goal: A domain-specific language for Blinc UIs that compiles to optimized Rust, enabling hot reload, visual tooling, and a gentler learning curve.

### 3.1 Language Design

```blinc
// zyntax: declarative UI with embedded logic

import { Color, SpringConfig } from "blinc"

component Counter {
  state count: i32 = 0

  view {
    col(gap: 16, align: center, justify: center) {
      text("Count: {count}")
        .size(32)
        .color(Color::WHITE)
        .animate(spring: SpringConfig::bouncy)

      row(gap: 8) {
        button("- Decrement") {
          on_click: count -= 1
        }
        button("+ Increment") {
          on_click: count += 1
        }
      }
    }
  }

  style {
    .self {
      background: var(--surface);
      padding: 24px;
      border-radius: 12px;
    }
  }
}
```

### 3.2 Compiler Pipeline

| Stage | Description | Status |
|-------|-------------|--------|
| Lexer / tokenizer | `.blinc` source to token stream | Planned |
| Parser | Tokens to Zyntax AST | Planned |
| Type checker | Validate state, props, expressions | Planned |
| Code generator | AST to Rust builder API calls | Planned |
| Optimizer | Dead code elimination, const folding, static layout pre-computation | Planned |
| Build integration | `build.rs` or proc-macro for compile-time processing | Planned |

### 3.3 Features

| Feature | Priority | Notes |
|---------|----------|-------|
| Component definitions | P0 | `component Name { state, view, style }` |
| Reactive state | P0 | `state` block with typed fields |
| Template expressions | P0 | `{variable}` interpolation in text/attributes |
| Scoped CSS | P0 | `style` block compiled to stylesheet |
| Props / inputs | P0 | Typed component parameters |
| Conditional rendering | P0 | `if/else` in template |
| List rendering | P0 | `for item in list { ... }` |
| Event handlers | P0 | `on_click`, `on_change`, etc. |
| Slot / children | P1 | `<slot>` for composition |
| Animation declarations | P1 | `animate`, `transition` in style block |
| Import system | P1 | Cross-file component references |
| Hot reload | P2 | File watcher + incremental recompile |
| LSP server | P2 | Autocomplete, diagnostics, go-to-definition |
| VS Code extension | P2 | Syntax highlighting, inline preview |

### 3.4 Compilation Strategy

```text
.blinc source
    |
    v
[Zyntax Lexer] -> tokens
    |
    v
[Zyntax Parser] -> AST
    |
    v
[Type Checker] -> validated AST
    |
    v
[Rust Codegen] -> fn component_name(ctx) -> impl ElementBuilder { ... }
    |
    v
[Cargo Build] -> native binary (zero runtime overhead)
```

Key principle: **zero-cost abstraction**. The DSL compiles entirely at build time. No interpreter, no runtime template engine. The output is the same Rust builder API calls a developer would write by hand.

---

## Phase 4: Rendering & GPU

> Goal: Complete the rendering pipeline for advanced visual content.

### 4.1 3D Mesh Rendering (P1)

| Feature | Status | Notes |
|---------|--------|-------|
| Generic mesh data | **Done** | `MeshData` + `Vertex` + `Material` — users convert from glTF/OBJ/FBX |
| draw_mesh_data | **Done** | `DrawContext::draw_mesh_data(mesh, transform)` — direct render, no registration |
| PBR materials | **Done** | `Material`: base_color, metallic, roughness, emissive, texture, alpha_mode |
| Shadow mapping | **Done** | 2048² depth pass, 4-tap PCF, front-face culling, depth bias |
| Normal / displacement maps | **Done** | Tangent-space normal maps + parallax occlusion mapping (16-layer) |
| Skeletal animation | **Done** | `Bone`/`Skeleton`/`SkinningData`, GPU skinning via storage buffer (max 256 joints) |

### 4.2 Custom Shaders (P2)

| Feature | Status | Notes |
|---------|--------|-------|
| Custom render pass API | **Done** | `CustomRenderPass` trait, PreRender/PostProcess stages, label-based removal |
| Custom bind groups | **Done** | `BindGroupBuilder` — declarative layout+bind creation for all binding types |
| Compute shader access | **Done** | `ComputeDispatch` + `create_compute_pipeline()` + `@flow` DAG |
| Post-processing pipeline | **Done** | `PostProcessChain` — ping-pong effect chain, auto texture management |

### 4.3 Performance (P1)

| Feature | Status | Notes |
|---------|--------|-------|
| draw_rgba_pixels | **Done** | `DrawContext::draw_rgba_pixels()` — GPU texture upload + render per frame |
| Dynamic image rendering | **Done** | `DynamicImage` in PrimitiveBatch, renderer uploads + draws via image pipeline |
| Virtualized list rendering | Planned | Only render visible items |
| Texture atlas improvements | Done | SVG atlas, glyph atlas |
| Lazy image loading | **Done** | Viewport-aware load + 100px buffer, color/image/skeleton placeholders, fade-in animation, CSS `loading` property |
| Render region culling | **Done** | AABB visibility test before GPU upload, shadow/rotation/affine-aware |
| GPU memory budget | **Done** | `GpuMemoryBudget` with LRU eviction, 128 MB default, env var override |

---

## Phase 5: Developer Experience

> Goal: Make Blinc a joy to develop with.

### 5.1 Tooling (P1)

| Feature | Status | Notes |
|---------|--------|-------|
| Hot reload | Planned | File watcher + incremental rebuild |
| Visual inspector | Partial | blinc_debugger exists, needs UI overlay |
| Animation debugger | Planned | Timeline view, pause/step |
| Layout debugger | Planned | Flexbox visualization (like browser devtools) |
| Performance profiler | Planned | Frame time, GPU utilization, batch count |

### 5.2 IDE Integration (P2)

| Feature | Status | Notes |
|---------|--------|-------|
| VS Code extension | Planned | Zyntax syntax highlighting + preview |
| LSP for `.blinc` files | Planned | Autocomplete, diagnostics |
| Component preview | Planned | Inline rendering in editor |
| Code snippets | Planned | Common patterns and widgets |

### 5.3 Documentation (P1)

| Feature | Status | Notes |
|---------|--------|-------|
| Blinc Book | Partial | Core concepts, 3D rendering, flow shaders (vertex/material), routing, media |
| API reference (rustdoc) | Partial | Many crates need doc improvements |
| Interactive examples | Planned | WASM playground |
| Video tutorials | Planned | Getting started, advanced topics |
| Skills.md (AI agents) | Done | Example-driven reference for LLMs |

---

## Phase 6: Accessibility

> Goal: Make Blinc apps usable by everyone.

| Feature | Priority | Notes |
|---------|----------|-------|
| Semantic element roles | P1 | Button, heading, list, etc. |
| Screen reader announcements | P1 | Platform accessibility APIs |
| Keyboard navigation (Tab order) | P1 | Focus ring, tab index |
| ARIA-like attributes | P1 | Label, description, live regions |
| High contrast mode | P2 | Theme variant |
| Reduced motion | P2 | Respect OS preference |
| Text scaling | P2 | Independent of DPI scaling |

---

## Phase 7: Platform Expansion

| Platform | Status | Notes |
|----------|--------|-------|
| macOS | Stable | wgpu (Metal) |
| Windows | Stable | wgpu (DX12/Vulkan) |
| Linux | Stable | wgpu (Vulkan) |
| Android | Stable | wgpu (Vulkan) |
| iOS | Stable | wgpu (Metal) |
| HarmonyOS | In progress | wgpu (Vulkan/OpenGL ES) |
| Web (WASM) | Future | wgpu WebGPU backend |
| Embedded (RPi) | Future | Framebuffer or Vulkan |

---

## Release Milestones

| Version | Target | Focus | Status |
|---------|--------|-------|--------|
| 0.2.0 | Q2 2026 | Desktop production readiness — system integration, multi-window, IME, clipboard, DnD, tray, hotkeys, code editor, virtual list | **Complete** |
| 0.3.0 | Q3 2026 | Mobile production readiness — gestures, safe area, haptics, navigation/router, deep linking, media (audio/video/camera), native bridge | **Complete** |
| 0.4.0 | Q3 2026 | GPU & rendering — 3D mesh pipeline (PBR, shadows, normal maps, skeletal animation), custom shaders (bind groups, compute, post-processing), render culling, memory budget | **Complete** (pulled forward from 0.5.0) |
| 0.5.0 | Q4 2026 | Missing widgets (date/time/color picker, range slider, number input, data grid, rich text editor), lazy image loading, virtualized list rendering | In progress |
| 0.6.0 | Q4 2026 | Zyntax DSL v1 — lexer, parser, type checker, Rust codegen, build.rs integration | Planned |
| 0.7.0 | Q1 2027 | Developer experience — hot reload, visual inspector, layout/animation debugger, performance profiler | Planned |
| 0.8.0 | Q1 2027 | Accessibility v1 — semantic roles, screen reader, keyboard navigation, ARIA-like attributes, high contrast, reduced motion | Planned |
| 0.9.0 | Q2 2027 | Platform expansion — HarmonyOS stable, Web (WASM) preview | Planned |
| 1.0.0 | Q3 2027 | Stable API, full documentation, Zyntax hot reload + LSP, production certification | Planned |

---

## Contributing

See individual crate READMEs for architecture details. The most impactful areas to contribute:

1. **Missing widgets** (Phase 1.4) — date picker, color picker, data grid, rich text editor
2. **Zyntax DSL** (Phase 3) — lexer, parser, codegen
3. **Accessibility** (Phase 6) — screen reader, keyboard nav, ARIA
4. **Developer tooling** (Phase 5) — hot reload, visual inspector, debugger
5. **Documentation** — API docs, tutorials, interactive examples
