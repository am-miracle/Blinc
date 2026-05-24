# Blinc

[![Build Status](https://github.com/project-blinc/Blinc/actions/workflows/ci.yml/badge.svg)](https://github.com/project-blinc/Blinc/actions/workflows/ci.yml)
[![Rust Version](https://img.shields.io/badge/rust-1.85%2B-blue.svg)](https://www.rust-lang.org)
[![Edition](https://img.shields.io/badge/edition-2024-blue.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/)
[![Crates.io](https://img.shields.io/crates/v/blinc_app.svg)](https://crates.io/crates/blinc_app)
[![License](https://img.shields.io/badge/license-Apache--2.0-green.svg)](LICENSE)
[![Blinc Book](https://img.shields.io/badge/Blinc_Book-blue.svg?logo=gitbook&logoColor=white)](https://project-blinc.github.io/Blinc)
[![Live Demos](https://img.shields.io/badge/Live_Demos-WebGPU-orange.svg)](https://project-blinc.github.io/Blinc/web/example-gallery.html)
[![Discord](https://img.shields.io/badge/Discord-Join-5865F2.svg?logo=discord&logoColor=white)](https://discord.gg/WXADUBBgzP)

![Blinc UI](glass_music_player.png)

**A GPU-accelerated, cross-platform UI framework** for building desktop, mobile, and web applications from a single Rust codebase.

## Highlights

- **GPU-Accelerated**: SDF rendering, glassmorphism, spring physics, @flow custom shaders
- **Cross-Platform**: Desktop (macOS/Windows/Linux), Android, iOS, Web (WebGPU)
- **Builder API**: Declarative, chainable: `div().flex_col().gap(16.0).child(text("Hello"))`
- **40+ Components**: shadcn/ui-style library with CSS-overridable theming
- **40+ Live Demos**: [Try them in your browser](https://project-blinc.github.io/Blinc/web/example-gallery.html) (WebGPU required)

## Quick Start

```rust
use blinc_app::prelude::*;
use blinc_app::windowed::WindowedApp;

fn main() -> Result<()> {
    WindowedApp::run(WindowConfig::default(), |ctx| {
        div()
            .w(ctx.width).h(ctx.height)
            .bg(Color::rgb(0.1, 0.1, 0.15))
            .justify_center().items_center()
            .child(text("Hello Blinc!").size(48.0).color(Color::WHITE))
    })
}
```

The same `build_ui` function runs on desktop and web and no separate codebase.

## Platform Support

| Platform | Status | Backend |
|----------|--------|---------|
| macOS | Stable | wgpu (Metal) |
| Windows | Stable | wgpu (DX12/Vulkan) |
| Linux | Stable | wgpu (Vulkan) |
| Android | Stable | wgpu (Vulkan) |
| iOS | Stable | wgpu (Metal) |
| **Web (WASM)** | **Preview** | wgpu (WebGPU) — Chrome 113+, Edge 113+, Firefox 141+, Safari 18+ (macOS Tahoe) |
| HarmonyOS | In progress | wgpu (Vulkan/OpenGL ES) |

## Documentation

**[Blinc Book](https://project-blinc.github.io/Blinc)**: comprehensive guide covering layout, styling, animation, widgets, routing, media, and more.

**[Live Example Gallery](https://project-blinc.github.io/Blinc/web/example-gallery.html)**: 40+ interactive WebGPU demos running in your browser.

**[API Reference](https://docs.rs/blinc_app)**: rustdoc for all crates.

**[Skills.md](Skills.md)**: concise, example-driven reference for AI code agents.

## Crates

| | Crate | Description |
|-|-------|-------------|
| **Core** | [blinc_app](crates/blinc_app) | App framework, windowed + web runners |
| | [blinc_core](crates/blinc_core) | Signals, state machines, types |
| | [blinc_layout](crates/blinc_layout) | Flexbox layout, element builders, widgets |
| | [blinc_gpu](crates/blinc_gpu) | SDF rendering, glass, shaders |
| **Rendering** | [blinc_text](crates/blinc_text) | Text shaping, glyph atlas |
| | [blinc_svg](crates/blinc_svg) | SVG parsing + rasterization |
| | [blinc_image](crates/blinc_image) | Image decoding, lazy loading |
| | [blinc_paint](crates/blinc_paint) | Canvas/paint API |
| **Animation** | [blinc_animation](crates/blinc_animation) | Springs, keyframes, timelines |
| | [blinc_theme](crates/blinc_theme) | Design tokens, light/dark mode |
| **Components** | [blinc_cn](crates/blinc_cn) | 40+ shadcn/ui-style components |
| | [blinc_icons](crates/blinc_icons) | Lucide icon set |
| **Platform** | [blinc_platform](crates/blinc_platform) | Cross-platform traits |
| | [blinc_platform_desktop](extensions/blinc_platform_desktop) | Desktop (winit) |
| | [blinc_platform_android](extensions/blinc_platform_android) | Android (NDK) |
| | [blinc_platform_ios](extensions/blinc_platform_ios) | iOS (UIKit/Metal) |
| | [blinc_platform_web](extensions/blinc_platform_web) | Web (WebGPU/WASM) |

## Roadmap

See [ROADMAP.md](ROADMAP.md) for detailed milestones. Current focus:

1. Missing widgets (date/time/color picker, data grid)
2. Zyntax DSL: `.blinc` domain specific language for UI definitions, with support for live editing and hot reload in the future
3. Accessibility (screen reader, keyboard navigation)
4. Developer tooling (hot reload, visual inspector)

## Community

Join us on **[Discord](https://discord.gg/WXADUBBgzP)**: questions, showcases, design discussion, and roadmap chatter. Also feel free to open a [GitHub Discussion](https://github.com/project-blinc/Blinc/discussions) for longer-form conversations.

## License

Apache License 2.0 - see [LICENSE](LICENSE)
