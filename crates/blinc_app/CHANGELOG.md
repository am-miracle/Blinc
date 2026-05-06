# Changelog

All notable changes to `blinc_app` will be documented in this file.

## [Unreleased]

### Added
- Display refresh rate detection at window resume — feeds `WindowConfig.max_frame_latency`-clamped `set_target_fps` so the scheduler doesn't burn 120 ticks/sec on a 60 Hz panel.
- `WindowConfig::max_frame_latency` is honoured for the primary surface (cap captured pre-move as `primary_max_frame_latency`).
- `WindowConfig::animation_fps_cap` is honoured by the redraw chain. When the only active redraw signals are animation progress (CSS keyframes / transitions / motion / theme / flow), the chain calls `WakeProxy::wake_at(1000/N ms)` instead of `request_redraw()` — pacing animation-only frames at the configured rate while leaving input / scroll / drag / cursor frames at native vsync. Halved idle CPU on `styling_demo` at `Some(30)`.
- `tracing::trace!` target `blinc_app::redraw_signals` logs which of the nine end-of-frame signals (animation / cursor / motion / scroll / overlay / theme / css / pointer-query / flow) drove a redraw — silent on a quiet idle.

### Changed
- Removed the perpetual no-op tick callback that kept the animation scheduler thread alive at 120 fps. Combined with the scheduler park/wake change, idle apps now use ~0% CPU (issue #28).
- `needs_overlay_redraw` derived from `has_animating_overlays()` instead of `has_visible_overlays()` — a static popover no longer pins the redraw chain at vsync.
- `SvgAtlas` allocates its 1024×1024 RGBA texture and CPU shadow buffer (~8 MB total) lazily on first SVG insert. Apps that never render an SVG never pay it.
- Scrollbar-state diagnostic logs in `render_node` downgraded from `info` to `trace` (was firing every frame for every scroll container).

### Added
- `BlincApp::has_storage_buffers()` API for platform capability detection

### Changed
- Web surface configuration: COPY_SRC conditionally based on adapter support
- Web surface format: non-sRGB on WebGL2 GL adapter

## [0.5.0] - 2026-04-10

### Added

- **Web/WASM platform support** (`WebApp` runner):
  - Desktop-parity frame loop: overlays, motion containers, CSS animations/transitions, FLIP, pointer query, blend modes, @flow shaders, theme tick, focus sync
  - `WebAssetLoader` for image loading via browser `fetch()` API + `insert_asset()` for bundled assets
  - `preload_assets(&[urls])` async method for runtime image fetching
  - Per-frame wheel delta coalescing with sub-linear damping for smooth scroll
  - Scroll bounce-back physics with wheel-idle debounce (32ms overscroll / 120ms in-bounds)
  - Touch gesture-end fires `on_gesture_end()` immediately (no momentum tail)
  - `set_blend_target()` wired for CSS `mix-blend-mode` support
  - `OverlayContext::init()` for blinc_cn component overlay support
  - Fixed canvas viewport via `data-width`/`data-height`/`data-dpr` attributes
  - `BlincConfig { max_primitives: 20_000 }` default for wasm (up from 10k)
  - `RenderState` created in `WebApp::new` for full render path
  - `process_pending_scroll_refs()` wired for `ScrollRef::scroll_to`
  - Global animation scheduler registered (`blinc_animation::set_global_scheduler`)
  - Animation-driven stateful rebuilds (`check_stateful_animations`)
  - CSS stylesheet layout overrides on subtree rebuilds
- **Auto-generated web example wrappers** (`tools/build-web-examples`):
  - Cross-target convention: `pub fn build_ui(ctx) -> impl ElementBuilder`
  - 40+ examples auto-discovered, wrapper crates generated
  - mdBook gallery with lazy-loaded iframes
  - `--build` flag with git-diff incremental planner
  - `--force-rebuild` escape hatch
  - Image asset auto-detection and `preload_assets` codegen
  - CI staging copies per-example assets alongside wasm output
  - Gallery sub-pages with "best viewed in full window" tip
- **Monospace font resolution**: JetBrains Mono bundled, generic font cache invalidation after font load
- **SVG color tint**: post-rasterization `apply_tint()` for non-currentColor sources
- **SVG atlas eviction**: per-frame mark-and-sweep prevents animated SVG atlas overflow
- Mobile iOS edit menu visibility fix, paste round-trip, long-press word select
- Soft keyboard + text input cursor positioning on mobile

### Fixed

- Glass shader `textureSample` → `textureSampleLevel` for WebGPU uniform-control-flow compliance
- Image shader `fwidth` → constant AA width for WebGPU compliance
- Composite shader (layer effects) `textureSample` → `textureSampleLevel`
- Drop shadow + glow shader `textureSample` → `textureSampleLevel`
- `std::time::SystemTime` → `web_time::SystemTime` in code editor double-click detection
- SVG atlas blink on animated SVGs (eviction moved to `begin_frame`)
- CSS styling lost after stateful interaction (missing `apply_stylesheet_layout_overrides` on subtree rebuild)
- 13 examples moved `ctx.add_css()` from `fn main` into `build_ui` for web compatibility
- CI: removed fragile pkg/ cache, widened paths trigger, fresh binaryen for cn_demo wasm-opt crash
- Adapter limits: wasm uses `supported.clone()` for browser-safe device creation
- VERTEX_STORAGE capability check with descriptive error for Safari
- Surface `COPY_SRC` usage for blend mode two-pass compositing on Chrome

## [Unreleased]

### Added

- Lazy image loading completion:
  - Skeleton shimmer placeholder rendering with auto-redraw animation
  - Image placeholder rendering (type 2) using preloaded thumbnail/blur-hash
  - Fade-in animation when images finish loading (configurable per element)
  - `image_load_times` HashMap tracks when each image was first cached
  - Placeholder images are eagerly preloaded so they're ready before main render
  - CSS overrides for `loading_strategy`, `placeholder_type`, `placeholder_color`, `placeholder_image`, `fade_duration_ms` flow through `RenderProps`

## [0.4.0] - 2026-04-05

### Added

- Multi-window support (desktop):
  - `open_window(config)` / `open_window_with(config, builder)` — global APIs
  - `WindowId` type for platform-agnostic window identification
  - `WindowState` struct bundles all per-window state
  - `AppCommand` enum for cross-thread window creation via `EventLoopProxy`
  - `GpuRenderer::create_surface()` for shared-device surface creation
  - Secondary windows render custom UI via builder closures with DPI scaling
- Window management:
  - `WindowConfig::min_size()`, `max_size()` — size constraints
  - `WindowConfig::position()`, `center()` — initial positioning
  - `Window::set_position()`, `center_on_screen()`, `set_size()` — runtime positioning
  - `Window::drag_window()`, `minimize()`, `maximize()`, `close()` — window controls
- Custom title bars:
  - `.drag_region()` on Div — OS window drag on mouse down
  - `window_actions` module — `drag_window()`, `minimize_window()`, `maximize_window()`, `close_window()` callable from anywhere
  - `DesktopApp` manages `HashMap<WinitWindowId, DesktopWindow>`
  - Events tagged with `WindowId` for per-window routing
- Native file dialogs: `open_file()`, `save_file()`, `pick_folder()` with builder API
  - File type filters via `FileFilter::new("Images").ext("png").ext("jpg")`
  - Multi-file selection via `pick_many()`
  - Starting directory and default file name options
  - Desktop-only (behind `windowed` feature, uses `rfd` crate)
- Soft keyboard show/hide on mobile text widget focus (Android + iOS)

### Fixed

- Combobox dropdown scroll by using registered scroll physics directly
- Checkbox background not updating on check/uncheck
- Toast slide-in entering from middle instead of right edge
- Toast positioning via absolute layout for full viewport coverage

## [0.1.15] - 2026-03-22

### Fixed

- Combobox dropdown scroll by using registered scroll physics directly from the windowed context

## [0.1.14] - 2026-02-24

### Added

- `css_debug` example updated with stateful container tests for CSS var(), percentage, color inheritance, and inner child click persistence

## [0.1.13] - 2026-02-18

### Added

#### Pointer Query Pressure & Touch Physics

- Desktop: mouse press → binary 0/1 pressure, touch events → hardware pressure via `Force::Normalized`
- Desktop: `HashSet<u64>` active touch ID tracking for accurate `pointer-touch-count`
- iOS: forward `touch.force` to `pointer_query.set_pressure()`, track active touch count
- iOS: `blinc_handle_touch_with_force` FFI for Swift callers to pass force data (backward-compatible)
- Android: forward primary pointer pressure and touch count from `MotionEvent`

#### Flow Shader Direct Graph Support

- `FlowElement` carries optional `Arc<FlowGraph>` for direct graph rendering (bypasses stylesheet lookup)
- Render loop prefers direct graph over stylesheet-defined flow when both are available
- `semantic_flow_demo` example: added `flow!` macro plasma card demonstrating direct `div().flow(graph)` API

#### SVG CSS Animations

- SVG fill, stroke, stroke-width animatable via `@keyframes` and CSS transitions
- Stroke-dasharray/dashoffset animation for SVG line-drawing effects
- SVG path morphing via `d: path("...")` CSS animation (cubic bezier interpolation)
- SVG sub-element metadata extraction (`extract_element_metadata`) for future per-element targeting
- `svg_animation_demo` example demonstrating all SVG animation phases

### Fixed

- Double border on CSS-transformed image containers: removed redundant `parent_border` overlay from image rendering (border from `render_layer_with_motion` is sufficient and transform-aware)
- Text in stacked/absolute elements now clips correctly within ancestor scroll containers (sharp clip intersects with existing scroll clip instead of replacing it)
- Text decorations now render for all z-layers in the fast path (was only rendering z=0, dropping decorations when blend mode layers activated the fast path)
- Text and SVG elements now clip to scroll container boundaries (regression from dual-clip refactor)
- SVG own-transform applied correctly (not just inherited parent transforms)
- CSS `transform: rotate()` animation uses original angle values instead of lossy atan2 decomposition
- Performance: SVG string manipulation only runs on cache miss (not every frame)

#### 3D SDF & Styling Demo

- Expanded `styling_demo` example with 3D shape showcases (box, sphere, cylinder, torus, capsule)
- 3D boolean operations demo (union, subtract, intersect, smooth variants)
- 3D group composition examples with compound shapes
- UV-mapped gradient backgrounds on 3D surfaces
- `translate-z` depth positioning examples
- Blinn-Phong lighting configuration examples

#### Music Player Glass Card Demo

- `music_player` example: iOS-style "Now Playing" card with liquid glass morphism
- All visual styling driven by CSS via `ctx.add_css()`
- Glass card with `backdrop-filter: liquid-glass()` refracted bevel borders
- Album art, song info, progress bar with track glow animation
- Playback controls with glass icon wrappers
- Hover effects: icon/badge scale + glass brightening + shadow deepening + SVG tint transitions
- Progress bar hover-reveal: height transition with overflow clip, opacity-faded time labels

#### SVG CSS Transform Propagation

- SVGs now inherit CSS transforms from ancestor elements
- Affine transform decomposed into scale (applied to SVG bounds) + rotation (GPU shader)
- Layout recomputed after state style changes that affect layout (visibility, display, height, etc.)

#### Stylesheet Runtime Integration

- CSS animation support wired through the app runtime
- `backdrop-filter` property support in windowed runner
- Stylesheet base styles applied after tree construction
- CSS transition ticking and application in frame loop
- Animated layout property support with per-frame `compute_layout()` recomputation

#### Styling Demo Enhancements

- CSS filter hover demo (`.filter-card:hover` with brightness, saturate, contrast)
- Filter blur & drop-shadow demos (static blur, hover transition, keyframe animation, combo)
- Backdrop-filter animation demos (static blur, hover transition, blur+saturate combo, keyframe pulse)
- `:is()` / `:where()` / `*-of-type` selector demos

#### CSS Form Input Styling

- Form input styling demo section with CSS-styled TextInput and TextArea widgets
- `#demo-input` with `:hover`, `:focus`, `::placeholder` pseudo-class/element demos
- `#accent-input` with warm color scheme (yellow/amber) CSS styling
- `#disabled-input` with `opacity: 0.5` disabled state demo
- `#demo-textarea` with `:hover`, `:focus` CSS styling and `caret-color` demo
- Text input focus bridge to EventRouter for `:focus` CSS matching in windowed runner

### Fixed

- Backdrop-filter demo parent containers now include `rounded(12.0)` so glass corner radius is visible against page background
- CSS animation ticking moved to synchronous main-thread execution to eliminate phase jitter caused by background-thread timing misalignment
- Mid-frame transition redraw: transitions created during `apply_complex_selector_styles` now properly trigger frame requests (prevents stalled hover-leave animations)
- iOS runner cleanup for platform trait consistency
- Clippy warnings in windowed.rs and ios.rs

## [0.1.12] - 2025-01-19

### Added
- Momentum scrolling for touch devices with velocity tracking
- `dispatch_scroll_chain_with_time()` method for mobile scroll dispatch with time-based velocity
- Single-threaded animation scheduler for mobile efficiency

### Changed
- Android render loop now uses vsync for frame pacing instead of manual timing
- Non-blocking poll when animating, 100ms idle timeout for power saving
- Re-enabled scroll physics for bounce animations on Android

### Fixed
- Animation smoothness on Android by removing mutex contention between threads
- Double-waiting issue that was cutting frame rate in half
- Added expected cfg values for fuchsia and ohos targets to fix CI warnings

## [0.1.1] - Initial Release

- Initial public release with desktop, Android, and iOS support
