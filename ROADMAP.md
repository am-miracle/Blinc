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
| IME / compose input (mobile) | Planned | Android InputConnection, iOS UITextInput |
| Gesture recognizers | **Done** | `GestureExt` trait: `.on_tap()`, `.on_long_press()`, `.on_swipe()` + pinch/rotate |
| Pull-to-refresh | **Done** | `pull_to_refresh(callback)` widget with threshold, max pull, indicator |
| Safe area insets | **Done** | `ctx.safe_area`, `safe_top/right/bottom/left()`, `safe_width/height()` |
| Haptic feedback | Done | Native bridge handlers on both platforms |

### 2.2 Platform Integration (P1)

| Feature | Status | Notes |
|---------|--------|-------|
| Deep linking / URL schemes | **Done** | `blinc_router::handle_deep_link()` + platform `DeepLink` parser |
| App lifecycle | Partial | Basic resume/pause, needs full handling |
| Native bridge API (Rust side) | Planned | Cross-platform `native_call("namespace", "fn", args)` from Rust |

> **Push notifications, camera, location, biometrics, status bar theming** are implemented as **example native bridge extensions** — not in the framework core, but as documented templates that demonstrate the bridge API:
> - **Android**: `BlincNativeBridge.register("camera", "capture", handler)` in Kotlin
> - **iOS**: `BlincNativeBridge.shared.register(namespace:name:handler:)` in Swift
> - **Rust**: `native_call("camera", "capture", args)` (planned API)
>
> Blinc provides the bridge transport. Platform features ship as reference implementations users can copy and adapt.

### 2.3 Media (P1)

| Feature | Status | Notes |
|---------|--------|-------|
| Audio playback | Planned | Mobile: platform codecs (MediaPlayer/AVAudioPlayer), Desktop: open formats (Vorbis/Opus via `rodio`) |
| Video playback | Planned | Mobile: platform decoders (MediaCodec/AVPlayer), Desktop: open formats (VP9/AV1 via `ffmpeg` or pure-Rust decoders) |
| Audio recording | Planned | Mobile: platform APIs, Desktop: `cpal` for cross-platform capture |
| Video widget | Planned | Texture streaming from decoder → GPU surface, frame-synced rendering |
| Audio context | Planned | Volume, playback state, seek — reactive via signals |

> **Licensing**: Desktop uses Apache-2.0 compatible codecs only (Vorbis, Opus, VP9, AV1).
> Mobile uses platform-provided codecs (no licensing concern — OS handles it).

| Example Extension | Status | Notes |
|-------------------|--------|-------|
| Push notifications | Planned | FCM/APNs example with bridge handlers |
| Camera capture | Planned | RGBA data from platform → FFI → canvas/image texture render |
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
| Mesh loading (glTF/OBJ) | Planned | `draw_mesh()` is currently stubbed |
| Instanced mesh rendering | Planned | `draw_mesh_instanced()` stubbed |
| PBR materials | Planned | Metallic-roughness workflow |
| Shadow mapping | Planned | Depth pass + shadow map sampling |
| Normal / displacement maps | Planned | Texture-based surface detail |
| Skeletal animation | Planned | Bone transforms + skinning |

### 4.2 Custom Shaders (P2)

| Feature | Status | Notes |
|---------|--------|-------|
| Custom render pass API | Planned | Plugin architecture for user passes |
| Custom bind groups | Planned | User-defined GPU resources |
| Compute shader access | Partial | `@flow` DAG exists, general compute planned |
| Post-processing pipeline | Planned | User-defined screen-space effects |

### 4.3 Performance (P1)

| Feature | Status | Notes |
|---------|--------|-------|
| Virtualized list rendering | Planned | Only render visible items |
| Texture atlas improvements | Done | SVG atlas, glyph atlas |
| Lazy image loading | Partial | LoadingStrategy exists, needs completion |
| Render region culling | Planned | Skip off-screen elements |
| GPU memory budget | Planned | Eviction policy for textures/buffers |

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
| Blinc Book | Partial | Core concepts covered |
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
| Fuchsia | In progress | wgpu (Vulkan/Scenic) |
| HarmonyOS | In progress | wgpu (Vulkan/OpenGL ES) |
| Web (WASM) | Future | wgpu WebGPU backend |
| Embedded (RPi) | Future | Framebuffer or Vulkan |

---

## Release Milestones

| Version | Target | Focus |
|---------|--------|-------|
| 0.2.0 | Q2 2026 | Desktop production readiness (file dialogs, multi-window, IME) |
| 0.3.0 | Q3 2026 | Mobile production readiness (gestures, navigation, platform APIs) |
| 0.4.0 | Q4 2026 | Zyntax DSL v1 (compiler, hot reload, basic tooling) |
| 0.5.0 | Q1 2027 | 3D mesh rendering, custom shader API, accessibility v1 |
| 1.0.0 | Q2 2027 | Stable API, full documentation, production certification |

---

## Contributing

See individual crate READMEs for architecture details. The most impactful areas to contribute:

1. **System integration** (Phase 1.1) — file dialogs, tray, DnD
2. **Missing widgets** (Phase 1.4) — date picker, virtualized list
3. **Zyntax DSL** (Phase 3) — parser, codegen
4. **Accessibility** (Phase 6) — screen reader, keyboard nav
5. **Documentation** — API docs, tutorials, examples
