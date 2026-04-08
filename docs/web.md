# Web (`wasm32-unknown-unknown`)

Blinc compiles to wasm32 and runs inside a `<canvas>` element via wgpu's WebGPU backend (with WebGL2 fallback). The web target is **Tier 2**: it ships, has runnable examples, and exercises the same render / event / state pipelines as the native runners — but a few platform-specific bits (touch input, IME, file dialogs, multi-canvas) are deliberately out of scope for the initial cut.

## Quick start

```bash
# Build the smallest possible Blinc web app — text on a dark background
cd examples/web_hello
wasm-pack build --target web --release
./serve.sh
# open http://localhost:8000/
```

The runnable examples under [`examples/`](../examples) are the canonical reference for every pattern documented below. They're each a single file under 250 lines.

| Example | What it demonstrates |
|---------|---------------------|
| [`web_hello`](../examples/web_hello) | The minimum: canvas surface, bundled font, one Div with centered text |
| [`web_scroll`](../examples/web_scroll) | Wheel input → `EventRouter::on_scroll_nested` → `dispatch_scroll_chain`, scroll-physics tick, no-bounce default |
| [`web_drag`](../examples/web_drag) | Mouse drag → DRAG events → Stateful + State::set → incremental update path → `transform: translate(...)` |
| [`web_assets`](../examples/web_assets) | `WebApp::run_with_async_setup` + `WebAssetLoader::fetch_bytes` to fetch fonts at runtime instead of bundling |

## Browser support

| Browser | Status | Notes |
|---------|--------|-------|
| Chrome / Chromium ≥ 113 | Supported | WebGPU enabled by default since Chrome 113 (May 2023) |
| Edge ≥ 113 | Supported | Same Chromium engine, same WebGPU support |
| Safari Technology Preview | Supported (flagged) | Enable `Develop → Feature Flags → WebGPU` |
| Safari (stable) | Coming | WebKit's WebGPU implementation is in progress; ETA depends on Apple |
| Firefox Nightly | Supported (flagged) | `about:config → dom.webgpu.enabled = true` |
| Firefox (stable) | Coming | Tracking issue [Bug 1602129](https://bugzilla.mozilla.org/show_bug.cgi?id=1602129) |

The runtime probes for WebGPU at startup and falls back to WebGL2 where WebGPU is unavailable, but **some Blinc pipelines need WebGPU to be functional** — specifically the SDF aux-buffer (storage buffers) and any future compute shaders. Plain rendering, text, and SVG work on WebGL2; advanced 3D/particles do not.

## Build

```bash
wasm-pack build examples/web_hello --target web --release
```

`--target web` produces an ES-module loader (`pkg/<crate>.js`) that you import directly from your HTML's `<script type="module">`. Don't use `--target bundler` unless you actually have webpack — the `web` target works with any plain static file server.

The generated `pkg/` folder contains:

- `<crate>.js` — JS shim that calls `wasm-bindgen`-generated bindings
- `<crate>_bg.wasm` — the actual wasm artifact
- `<crate>.d.ts` — TypeScript bindings (optional)

Serve `pkg/` and `index.html` together; the `index.html` examples in this repo each ship with a `serve.sh` that picks the first available static server (`python3`, `python`, `ruby`, `npx http-server`).

## Bundle size

A minimal Blinc app is ~6-8 MB pre-strip. Where the bytes go:

- **Renderer + WGSL shaders + wgpu** — ~3 MB
- **rustybuzz / unicode-bidi / unicode-linebreak** (text shaping) — ~600 KB
- **resvg / tiny-skia** (SVG rasterization) — ~700 KB
- **Layout (taffy + flexbox)** — ~200 KB
- **Reactive graph + state hooks + Stateful machinery** — ~400 KB
- **Bundled font (if any)** — ~750 KB per typical TTF

`wasm-pack build --release` runs `wasm-opt -O` automatically; the `Cargo.toml` `[package.metadata.wasm-pack.profile.release]` block in each example passes `--all-features` so bulk-memory and reference-type ops survive the optimizer.

For tighter bundles, see the **fetch-don't-bundle** pattern below.

## Fonts

Browsers can't hand wgpu their system fonts — those live in the compositor's 2D pipeline, not in the WebGPU pipeline. Blinc rasterizes glyphs natively via swash, which needs the actual TTF/OTF bytes in wasm memory. The wasm32 init path **deliberately skips system font discovery** (no filesystem), so the font registry starts empty. **Without a registered font, every text element fails to shape glyphs and renders as nothing.**

Two patterns:

### Pattern 1: bundled font (`include_bytes!`)

Simplest, but the font bytes ship inside the wasm artifact. Adds ~750 KB to the bundle per typical TTF.

```rust
use blinc_app::web::WebApp;

const ARIAL_TTF: &[u8] = include_bytes!("../fonts/Arial.ttf");

#[wasm_bindgen(start)]
pub fn _start() {
    wasm_bindgen_futures::spawn_local(async {
        WebApp::run_with_setup(
            "blinc-canvas",
            |app| {
                app.load_font_data(ARIAL_TTF.to_vec());
            },
            build_ui,
        )
        .await
        .unwrap();
    });
}
```

See [`examples/web_hello/src/lib.rs`](../examples/web_hello/src/lib.rs) for the full example.

### Pattern 2: fetched font (`run_with_async_setup`)

Recommended for real apps that ship more than one font, or for any font over a few hundred KB. The font lives next to `index.html` as a static asset; the browser caches it independently across reloads.

```rust
use blinc_app::web::WebApp;
use blinc_app::BlincError;
use blinc_platform_web::WebAssetLoader;

#[wasm_bindgen(start)]
pub fn _start() {
    wasm_bindgen_futures::spawn_local(async {
        WebApp::run_with_async_setup(
            "blinc-canvas",
            |app| Box::pin(async move {
                let bytes = WebAssetLoader::fetch_bytes("fonts/Inter.ttf")
                    .await
                    .map_err(|e| BlincError::Platform(e.to_string()))?;
                app.load_font_data(bytes);
                Ok(())
            }),
            build_ui,
        )
        .await
        .unwrap();
    });
}
```

The `Box::pin(async move { ... })` ceremony is needed because stable Rust doesn't have `async FnOnce` yet — the closure has to return a boxed future. Once async closures stabilize this drops back to `|app| async move { ... }`.

See [`examples/web_assets/src/lib.rs`](../examples/web_assets/src/lib.rs) for the full example. The web_assets wasm is ~612 KB smaller than web_hello purely because the font is no longer baked into the bundle.

### Mixing both: bundled fallback + fetched main

For production, ship a tiny system-ish fallback inside the wasm and fetch the real font asynchronously. This eliminates the brief blank-text window between page load and font fetch:

```rust
const FALLBACK_TTF: &[u8] = include_bytes!("../fonts/SystemFallback.ttf");

WebApp::run_with_async_setup(
    "blinc-canvas",
    |app| Box::pin(async move {
        // 1. Bundled fallback first — first frame renders text immediately
        app.load_font_data(FALLBACK_TTF.to_vec());
        // 2. Fetch the real font in parallel — replaces the fallback once it lands
        let inter = WebAssetLoader::fetch_bytes("fonts/Inter.ttf").await
            .map_err(|e| BlincError::Platform(e.to_string()))?;
        app.load_font_data(inter);
        Ok(())
    }),
    build_ui,
).await
```

## What's deliberately different from desktop

### `scroll()` defaults to bounce-disabled

The native `scroll()` widget defaults to bounce-back at edges via spring physics. On wasm32, **that default is flipped**: `scroll()` returns a no-bounce config because:

1. DOM wheel events have no reliable "gesture ended" phase. Desktop's bounce timing relies on winit's `ScrollPhase::Ended` from trackpad gestures; web has nothing equivalent.
2. macOS layers ~800ms of OS-level momentum-scroll wheel events on top of the user's gesture. Every workaround for "when did the user finish?" produces either a ~1 second bounce lag or a wobble as the spring restarts each time the OS momentum re-overscrolls a settled scroll.
3. Native HTML scrolling has no rubber-band either, except for iOS / macOS Safari at the *page* level — and that bounce is owned by the OS, not by anything inside a `<canvas>`.

If you want bounce on web specifically, opt in via `Scroll::with_config(ScrollConfig::default())` or supply your own `SharedScrollPhysics` to `Scroll::with_physics`.

### Async clipboard

`web_sys::Clipboard::write_text` / `read_text` are async-only. The `text_edit` widget's Cmd+C / Cmd+V keybinds still trigger on the keypress, but the clipboard write is fire-and-forget and the read can't be `await`-ed inside a synchronous handler. For text-editor-heavy apps, the workaround is to push clipboard ops onto an `AbortController`-managed task queue and surface the result through a State cell — same as you'd do for any async DOM API.

### Single canvas

`WebApp::run` takes a single canvas ID. Multi-canvas / multi-view setups (e.g. an in-page editor preview alongside the main app) work in principle — the architecture supports a shared `ElementRegistry` between trees — but no `WebApp::run_multi` API has shipped. Open an issue if you need it.

## What's missing (Tier 2 gaps)

| Feature | Status | Notes |
|---------|--------|-------|
| Mouse + wheel + keyboard input | ✅ | Routes through `EventRouter` |
| Drag gestures | ✅ | DRAG / DRAG_END events with deltas |
| Touch input | Pending | DOM `touchstart` / `touchmove` / `touchend` need conversion to `InputEvent::Touch` |
| IME composition | Pending | `compositionstart` / `compositionupdate` / `compositionend` → `EventRouter::on_text` |
| File dialogs | Pending | `rfd` doesn't compile on wasm32; need `<input type="file">` bridge |
| System tray / notifications / global hotkeys | Won't fix | Browser sandbox doesn't expose these |
| `localStorage` window-state persistence | Pending | Trivial follow-up using `web-sys::Storage` |
| Service worker / offline assets | Out of scope | App-level concern, not framework |
| Multi-canvas / multi-view | Pending | Architecture supports it; no `WebApp::run_multi` API yet |
| A11y (ARIA roles on canvas, screen reader) | Pending | Larger architecture discussion — needs DOM mirror or `accesskit-html` |

## Architecture notes

The web runner ([`crates/blinc_app/src/web.rs`](../crates/blinc_app/src/web.rs)) is a sibling of [`windowed.rs`](../crates/blinc_app/src/windowed.rs) (desktop), [`android.rs`](../crates/blinc_app/src/android.rs), and [`ios.rs`](../crates/blinc_app/src/ios.rs). It owns the same 5-phase frame loop:

1. **Tick scroll physics** — `tree.tick_scroll_physics(now_ms)` advances any active scroll-decel / spring-bounce one step
2. **Detect rebuild triggers** — polls `tree.needs_rebuild()`, `take_needs_rebuild()`, and `ctx.dirty_flag()` (the `ref_dirty_flag` that `State::set_rebuild` flips)
3. **Drain Stateful pending updates** — `take_pending_prop_updates()` + `tree.process_pending_subtree_rebuilds()` apply the queued render-prop / subtree changes that `State::set` produced via `Stateful::refresh_props_internal`
4. **Rebuild or incrementally update** — `tree.incremental_update(&element)` for normal frames, full `from_element_with_registry` rebuild on viewport resize
5. **Render** — `surface.get_current_texture()` → `BlincApp::render_tree(...)` → `frame.present()`

The driver is a `requestAnimationFrame` chain installed by `AnimationScheduler::start_raf()`; the wake callback is `WebApp::run_one_frame()`. Continuous redraw is enabled by default — same as the desktop runner — so the rAF loop fires every browser frame regardless of whether anything is animating.

DOM event listeners are attached in `WebApp::install_input_listeners` and route through `EventRouter::on_mouse_*` / `on_scroll_nested` / `on_key_*`. Each listener takes a `Closure::<dyn FnMut(_)>` that holds a `Rc<RefCell<WebApp>>` clone; reentrancy with the rAF wake callback is dodged via `try_borrow_mut`.

### What's reused from the rest of the framework

These already work and the wasm runner consumes them as-is:

- `WindowedContext` — same struct as desktop / mobile, with a `from_canvas` constructor for the wasm32 path
- `EventRouter` — same hit-test + event-bubbling code
- `RenderTree` + `incremental_update` — same incremental diff machinery
- `Stateful` + `BlincContextState` — same reactive state machinery
- `blinc_gpu` — same `GpuRenderer` with a parallel `with_canvas` constructor for `SurfaceTarget::Canvas`
- `blinc_text` / `blinc_svg` / `blinc_image` — same pipelines, fed bytes directly instead of via filesystem reads
