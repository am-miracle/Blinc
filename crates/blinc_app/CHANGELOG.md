# Changelog

All notable changes to `blinc_app` will be documented in this file.

## [Unreleased]

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
