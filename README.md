# Blinc

[![Build Status](https://github.com/project-blinc/Blinc/actions/workflows/ci.yml/badge.svg)](https://github.com/project-blinc/Blinc/actions/workflows/ci.yml)
[![Tests](https://github.com/project-blinc/Blinc/actions/workflows/ci.yml/badge.svg?event=push)](https://github.com/project-blinc/Blinc/actions/workflows/ci.yml)
[![Rust Version](https://img.shields.io/badge/rust-1.75%2B-blue.svg)](https://www.rust-lang.org)
[![Crates.io](https://img.shields.io/crates/v/blinc_app.svg)](https://crates.io/crates/blinc_app)
[![License](https://img.shields.io/badge/license-Apache--2.0-green.svg)](LICENSE)
[![Blinc Book](https://img.shields.io/badge/Blinc_Book-blue.svg?logo=gitbook&logoColor=white)](https://project-blinc.github.io/Blinc)

![Blinc UI](glass_music_player.png)

**A GPU-accelerated, cross-platform UI framework** with a GPUI-inspired builder API, glassmorphism effects, spring physics animations, and native rendering on Desktop, Android, and iOS.

## Features

- **GPU-Accelerated Rendering** - SDF-based primitives via wgpu with automatic batching
- **Glassmorphism Effects** - Apple-style vibrancy with backdrop blur and frosted glass
- **GPUI-Style Builder API** - Declarative, chainable element builders (`div()`, `text()`, `svg()`, `image()`)
- **Flexbox Layout** - Powered by Taffy with 100+ Tailwind-style builder methods
- **Spring Physics** - Interruptible animations with RK4 integration (Framer Motion quality)
- **Cross-Platform** - Desktop (macOS, Windows, Linux), Android, iOS, Web (wasm32 + WebGPU, [preview](docs/web.md))
- **Fine-Grained Reactivity** - Signal-based state without VDOM overhead
- **State Machines** - Harel statecharts for complex widget interactions
- **Image Support** - PNG, JPEG, GIF, WebP, BMP with cross-platform asset loading
- **SVG Rendering** - Vector graphics with fill/stroke support
- **Text Rendering** - HarfBuzz shaping, glyph atlas, proper anchoring

## Quick Start

### Installation

```bash
# Build from source
git clone https://github.com/project-blinc/Blinc
cd Blinc
cargo build --release
```

### Team Bootstrap

```bash
# Install optional local tooling used by CI
rustup component add rustfmt clippy
cargo install cargo-nextest cargo-audit cargo-deny
```

### Local P0 Check Parity

```bash
# Keep checks aligned with CI hardening commands
cargo fmt --all -- --check
cargo clippy --workspace --all-features -- -W clippy::all -A clippy::type_complexity -A dead_code
cargo check --workspace --all-features
cargo nextest run --workspace --all-features
cargo test --doc --workspace --all-features
cargo audit
cargo deny check
```

### Hello World

```rust
use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;

fn main() -> Result<()> {
    WindowedApp::run(WindowConfig::default(), |ctx| {
        div()
            .w(ctx.width).h(ctx.height)
            .bg(Color::rgb(0.1, 0.1, 0.15))
            .flex_col()
            .justify_center()
            .items_center()
            .child(
                text("Hello Blinc!")
                    .size(48.0)
                    .color(Color::WHITE)
            )
    })
}
```

### Glassmorphism Example

```rust
div()
    .w(ctx.width).h(ctx.height)
    .bg(Color::rgb(0.2, 0.3, 0.5))
    .flex_col()
    .justify_center()
    .items_center()
    .child(
        // Glass card with backdrop blur
        div()
            .glass()
            .rounded(16.0)
            .p(24.0)
            .child(text("Frosted Glass").size(24.0).color(Color::WHITE))
            .child(text("With backdrop blur").size(14.0).color(Color::rgb(0.8, 0.8, 0.8)))
    )
```

### Image Support

```rust
div()
    .w(400.0).h(300.0)
    .flex_col()
    .justify_center()
    .items_center()
    .child(
        image("assets/photo.png")
            .w(200.0)
            .h(150.0)
            .rounded(8.0)
    )
```

### Layout with Flexbox

```rust
div()
    .w_full().h_full()
    .flex_col()
    .gap(16.0)
    .p(24.0)
    .child(
        // Header
        div().h(60.0).bg(Color::rgb(0.2, 0.2, 0.25)).rounded(8.0),
    )
    .child(
        // Content area
        div()
            .flex_1()
            .flex_row()
            .gap(16.0)
            .child(div().w(200.0).bg(Color::rgb(0.15, 0.15, 0.2)).rounded(8.0))  // Sidebar
            .child(div().flex_1().bg(Color::rgb(0.18, 0.18, 0.22)).rounded(8.0)), // Main
    )
    .child(
        // Footer
        div().h(40.0).bg(Color::rgb(0.2, 0.2, 0.25)).rounded(8.0),
    )
```

## Architecture

```text
┌─────────────────────────────────────────────────────────────────────┐
│                           blinc_app                                  │
│          High-level API: BlincApp, WindowedApp, RenderContext        │
├─────────────────────────────────────────────────────────────────────┤
│  blinc_layout         │  blinc_gpu           │  blinc_paint          │
│  Flexbox (Taffy)      │  SDF Rendering       │  Canvas API           │
│  Element Builders     │  Glass/Blur          │  Paths/Shapes         │
│  RenderTree           │  MSAA                │  Transforms           │
├─────────────────────────────────────────────────────────────────────┤
│  blinc_text           │  blinc_svg           │  blinc_image          │
│  Font Loading         │  SVG Parsing         │  Image Decoding       │
│  Text Shaping         │  Vector Rendering    │  Texture Management   │
│  Glyph Atlas          │  Fill/Stroke         │  Cross-platform Load  │
├─────────────────────────────────────────────────────────────────────┤
│  blinc_core           │  blinc_animation     │  blinc_platform       │
│  Signals/Reactivity   │  Springs (RK4)       │  Window/Event Traits  │
│  State Machines       │  Keyframes           │  Input Events         │
│  Brush/Color Types    │  Timelines           │  Asset Loading        │
├─────────────────────────────────────────────────────────────────────┤
│     blinc_platform_desktop    │  blinc_platform_android  │   _ios   │
│     winit + wgpu              │  NDK + Vulkan            │  UIKit   │
└─────────────────────────────────────────────────────────────────────┘
```

## Crates

### Core

| Crate | Description |
| ----- | ----------- |
| [**blinc_app**](crates/blinc_app/README.md) | High-level app framework with windowed runner |
| [**blinc_core**](crates/blinc_core/README.md) | Reactive signals, state machines, brush types |
| [**blinc_layout**](crates/blinc_layout/README.md) | Flexbox layout engine with GPUI-style builders |
| [**blinc_gpu**](crates/blinc_gpu/README.md) | GPU rendering: SDF shapes, glass effects, MSAA |

### Rendering & Media

| Crate | Description |
| ----- | ----------- |
| [**blinc_paint**](crates/blinc_paint/README.md) | Canvas/paint API for custom drawing |
| [**blinc_text**](crates/blinc_text/README.md) | Text shaping, font loading, glyph atlas |
| [**blinc_image**](crates/blinc_image/README.md) | Image loading and cross-platform assets |
| [**blinc_svg**](crates/blinc_svg/README.md) | SVG parsing and rendering |

### Animation & Theming

| Crate | Description |
| ----- | ----------- |
| [**blinc_animation**](crates/blinc_animation/README.md) | Spring physics and keyframe animations |
| [**blinc_theme**](crates/blinc_theme/README.md) | Design tokens, theming, light/dark mode |

### Component Library

| Crate | Description |
| ----- | ----------- |
| [**blinc_cn**](crates/blinc_cn/README.md) | shadcn/ui-style component library (40+ components) |
| [**blinc_icons**](crates/blinc_icons/README.md) | Lucide icon set integration |

### Platform

| Crate | Description |
| ----- | ----------- |
| [**blinc_platform**](crates/blinc_platform/README.md) | Cross-platform traits and asset loading |
| [**blinc_platform_desktop**](extensions/blinc_platform_desktop/README.md) | Desktop backend (winit) |
| [**blinc_platform_android**](extensions/blinc_platform_android/README.md) | Android backend (NDK) |
| [**blinc_platform_ios**](extensions/blinc_platform_ios/README.md) | iOS backend (UIKit/Metal) |

### Tooling & Development

| Crate | Description |
| ----- | ----------- |
| [**blinc_cli**](crates/blinc_cli/README.md) | Command-line tooling |
| [**blinc_macros**](crates/blinc_macros/README.md) | Procedural macros for components |
| [**blinc_debugger**](crates/blinc_debugger/README.md) | Visual debugger overlay |
| [**blinc_recorder**](crates/blinc_recorder/README.md) | Frame recording and debugging |
| [**blinc_runtime**](crates/blinc_runtime/README.md) | Embedding SDK for host applications |
| [**blinc_test_suite**](crates/blinc_test_suite/README.md) | Visual regression testing framework |

## Builder API Reference

### Layout Methods

```rust
// Size
.w(100.0)  .h(100.0)  .size(100.0, 100.0)
.w_full()  .h_full()  .w_auto()  .h_auto()
.min_w()   .max_w()   .min_h()   .max_h()

// Flexbox
.flex_row()    .flex_col()    .flex_1()
.justify_start() .justify_center() .justify_end() .justify_between()
.items_start()   .items_center()   .items_end()   .items_stretch()
.gap(16.0)     .gap_x(8.0)      .gap_y(8.0)

// Spacing
.p(16.0)    .px(8.0)    .py(8.0)
.pt(4.0)    .pb(4.0)    .pl(4.0)    .pr(4.0)
.m(16.0)    .mx(8.0)    .my(8.0)
```

### Styling Methods

```rust
// Background
.bg(Color::rgb(r, g, b))
.bg(Color::from_hex(0xRRGGBB))
.glass()

// Border & Corners
.rounded(8.0)
.border(1.0, Color::GRAY)

// Shadow
.shadow_sm()  .shadow_md()  .shadow_lg()  .shadow_xl()

// Opacity & Clipping
.opacity(0.8)
.clip()
```

### Elements

```rust
div()                      // Container element
text("Hello")              // Text element
    .size(16.0)
    .color(Color::WHITE)

svg(svg_string)            // SVG element
    .w(24.0).h(24.0)

image("path/to/image.png") // Image element
    .w(200.0).h(150.0)
    .rounded(8.0)
```

## Canvas API 
Custom drawing with paths, shapes, and transforms:

![Canvas API](<Screenshot 2025-12-26 at 18.52.49.png>)

```rust
use blinc_paint::prelude::*;

fn canvas_example() -> Canvas{
    canvas(move |ctx: &mut dyn DrawContext, bounds| {
        let bar_height = 20.0;
        let bar_y = (bounds.height - bar_height) / 2.0;
        let radius = CornerRadius::uniform(bar_height / 2.0);

        // Background track
        ctx.fill_rect(
            Rect::new(0.0, bar_y, bounds.width, bar_height),
            radius,
            Brush::Solid(Color::rgba(0.2, 0.2, 0.25, 1.0)),
        );

        // Progress fill with gradient
        let fill_width = bounds.width * progress.clamp(0.0, 1.0);
        if fill_width > 0.0 {
            let gradient = Brush::Gradient(Gradient::linear(
                Point::new(0.0, bar_y),
                Point::new(fill_width, bar_y),
                Color::rgba(0.4, 0.6, 1.0, 1.0),
                Color::rgba(0.6, 0.4, 1.0, 1.0),
            ));
            ctx.fill_rect(
                Rect::new(0.0, bar_y, fill_width, bar_height),
                radius,
                gradient,
            );
        }

        // Progress percentage indicator with text
        let percent = (progress * 100.0) as i32;
        let text_x = bounds.width / 2.0 - 15.0;
        let text_bg = Rect::new(text_x - 5.0, bar_y - 25.0, 50.0, 18.0);
        ctx.fill_rect(
            text_bg,
            CornerRadius::uniform(4.0),
            Brush::Solid(Color::rgba(0.1, 0.1, 0.15, 0.9)),
        );

        // Draw the percentage text
        ctx.draw_text(
            &format!("{}%", percent),
            Point::new(text_x, bar_y),
            &TextStyle::new(18.0).with_color(Color::WHITE),
        );
    })
    .w(228.0)
    .h(80.0)
}
```

## Animation

Blinc provides a comprehensive animation system with spring physics, keyframe animations, and declarative motion containers.

![keyframe animations](ScreenRecording2025-12-27at13.02.25-ezgif.com-video-to-gif-converter.gif)

### Spring Animations

Spring physics animations with RK4 integration for natural, interruptible motion:

```rust
use blinc_animation::{AnimatedValue, SpringConfig};

// Create a spring-animated value
let mut position = AnimatedValue::new(0.0);

// Animate to a target with spring physics
position.animate_to(100.0, SpringConfig::default());

// Or use presets
position.animate_to(100.0, SpringConfig::snappy());   // Quick, responsive
position.animate_to(100.0, SpringConfig::bouncy());   // Playful bounce
position.animate_to(100.0, SpringConfig::smooth());   // Gentle, smooth
```

### Keyframe Animations

Multi-keyframe animations with custom easing:

```rust
use blinc_animation::{AnimatedTimeline, AnimatedKeyframe, Easing};

let timeline = AnimatedTimeline::new()
    .keyframe(AnimatedKeyframe::new(0.0).opacity(0.0).scale(0.8))
    .keyframe(AnimatedKeyframe::new(0.5).opacity(1.0).scale(1.1))
    .keyframe(AnimatedKeyframe::new(1.0).opacity(1.0).scale(1.0))
    .duration_ms(500)
    .easing(Easing::EaseOutCubic);
```

### Animation Presets

Built-in presets for common animations:

```rust
use blinc_animation::AnimationPreset;

// Fade animations
AnimationPreset::fade_in(300)
AnimationPreset::fade_out(200)

// Scale animations
AnimationPreset::scale_in(300)
AnimationPreset::scale_out(200)

// Bounce animations
AnimationPreset::bounce_in(500)
AnimationPreset::bounce_out(400)

// Pop (scale with overshoot)
AnimationPreset::pop_in(400)

// Slide animations
AnimationPreset::slide_in_left(300, 50.0)
AnimationPreset::slide_in_right(300, 50.0)
AnimationPreset::slide_in_top(300, 50.0)
AnimationPreset::slide_in_bottom(300, 50.0)
```

### Motion Container

The `motion()` container provides declarative enter/exit animations:

```rust
use blinc_layout::prelude::*;

// Single element with enter/exit animations
motion()
    .fade_in(300)
    .scale_in(300)
    .fade_out(200)
    .child(
        div()
            .w(100.0).h(100.0)
            .bg(Color::rgb(0.4, 0.7, 1.0))
            .rounded(8.0)
    )

// Slide animations
motion()
    .slide_in(SlideDirection::Left, 400)
    .slide_out(SlideDirection::Right, 300)
    .child(panel)

// Custom animation presets
motion()
    .enter_animation(AnimationPreset::bounce_in(500))
    .exit_animation(AnimationPreset::fade_out(200))
    .child(modal)
```

### Stagger Animations

Animate lists with staggered delays:

```rust
use blinc_layout::prelude::*;

// Forward stagger (first to last)
motion()
    .gap(8.0)
    .stagger(StaggerConfig::new(50, AnimationPreset::fade_in(300)))
    .children(items.iter().map(|item| card(item)))

// Reverse stagger (last to first)
motion()
    .stagger(StaggerConfig::new(50, AnimationPreset::fade_in(300)).reverse())
    .children(items)

// From center outward
motion()
    .stagger(StaggerConfig::new(50, AnimationPreset::fade_in(300)).from_center())
    .children(items)

// Limit stagger delay (cap at N items)
motion()
    .stagger(StaggerConfig::new(50, AnimationPreset::fade_in(300)).limit(5))
    .children(items)
```

### Easing Functions

```rust
use blinc_animation::Easing;

Easing::Linear
Easing::EaseIn
Easing::EaseOut
Easing::EaseInOut
Easing::EaseInCubic
Easing::EaseOutCubic
Easing::EaseInOutCubic
Easing::EaseOutBack      // Overshoot
Easing::EaseOutBounce    // Bounce effect
```

## Platform Support

| Platform | Status | Backend |
|----------|--------|---------|
| macOS | Stable | wgpu (Metal) |
| Windows | Stable | wgpu (DX12/Vulkan) |
| Linux | Stable | wgpu (Vulkan) |
| Android | Stable | wgpu (Vulkan), ~530KB |
| iOS | Stable | wgpu (Metal) |
| Fuschia | In progress | wgpu (Vulkan/Scenic) |
| HarmonyOS | In progress | wgpu (Vulkan/OpenGL ES) |

## Roadmap

See [ROADMAP.md](ROADMAP.md) for the full roadmap with detailed milestones.

**Current priorities:**
1. Desktop production readiness (file dialogs, multi-window, IME)
2. Mobile production readiness (gestures, navigation, platform APIs)
3. **Zyntax DSL** — `.blinc` file syntax with compile-time optimization to Rust
4. 3D mesh rendering and custom shader pipeline
5. Accessibility (screen reader, keyboard navigation)
6. Developer tooling (hot reload, visual inspector, LSP)

## Documentation

For comprehensive documentation, tutorials, and API reference, visit the **[Blinc Book](https://project-blinc.github.io/Blinc)**.

The book covers:

- Getting started guide
- Core concepts (elements, layout, styling, theming)
- Animation system (springs, keyframes, motion containers)
- Widget library
- Architecture deep-dives

### For AI Agents

See [Skills.md](Skills.md) — a concise, example-driven reference optimized for AI code agents building with Blinc.

## License

Apache License 2.0 - see [LICENSE](LICENSE)
