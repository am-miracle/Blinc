# blinc_web_hello

The smallest possible Blinc app running in a browser via WebGPU.

This is the canonical proof-of-life for Blinc's `wasm32-unknown-unknown`
target. It renders the text "Hello, WebGPU!" centered on a dark
background. No input handling, no animations, no asset loading — just
the render path.

If this draws on a real canvas in a real browser, the entire pipeline
is alive end-to-end:

- `wgpu` running on the WebGPU backend (or WebGL2 fallback)
- `blinc_gpu::GpuRenderer::with_canvas` initializing from a JS
  `HtmlCanvasElement`
- `blinc_app::WebApp::run` wiring everything up
- `blinc_animation::AnimationScheduler::start_raf` driving
  `requestAnimationFrame`
- The standard Blinc render path (`render_tree`) producing the same
  output it does on desktop / Android / iOS

## Build

You need [`wasm-pack`](https://rustwasm.github.io/wasm-pack/installer/)
installed:

```bash
curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh
```

Then from the workspace root:

```bash
cd examples/web_hello
wasm-pack build --target web --release
```

This produces `pkg/`:

```
pkg/
├── blinc_web_hello.js          # ES module loader + bindings
├── blinc_web_hello_bg.wasm     # The actual wasm artifact
├── blinc_web_hello.d.ts        # TypeScript type stubs
└── package.json
```

## Run

Any static HTTP server works. Python's built-in is the easiest:

```bash
# from examples/web_hello/
python3 -m http.server 8000
```

Then open <http://localhost:8000/> in a WebGPU-capable browser:

| Browser | Status |
|---|---|
| Chrome 113+ | WebGPU enabled by default |
| Edge 113+ | WebGPU enabled by default |
| Safari Technology Preview | WebGPU behind a flag |
| Firefox Nightly | WebGPU behind a flag |
| Safari (stable) | Falls back to WebGL2; some pipelines may degrade |
| Firefox (stable) | Falls back to WebGL2; some pipelines may degrade |

The page shows a fallback message if neither WebGPU nor WebGL2 is
available.

## Architecture

```
index.html
   │
   ▼
pkg/blinc_web_hello.js   ← wasm-pack generated ES module
   │
   ▼
blinc_web_hello (lib.rs)  ← #[wasm_bindgen(start)] entry point
   │
   ▼
blinc_app::WebApp::run("blinc-canvas", build_ui)
   │
   ├──► BlincApp::with_canvas(canvas)   ← async GPU init
   ├──► WindowedContext::new_web(...)   ← shared collaborator graph
   ├──► run_one_frame()                 ← initial render (no blank flash)
   ├──► scheduler.set_wake_callback(…)  ← render-on-tick closure
   ├──► scheduler.set_continuous_redraw(true)
   └──► scheduler.start_raf()           ← rAF chain self-perpetuates
```

The wake callback re-borrows the `Rc<RefCell<WebApp>>` and runs one
frame on every browser frame. The cycle (wake → Rc → ctx → scheduler →
wake) is intentional: it's what keeps the runner alive after `run`
returns. The browser tears it down on page unload.

## What's deliberately missing

| Capability | Status | Lands in |
|---|---|---|
| Mouse / keyboard / wheel input | Not yet wired | Phase 3d |
| Canvas resize handling | Stays at startup size | Phase 3e |
| Animation tick | No motion / CSS animation | Phase 3d / 5 |
| System fonts | Falls back to renderer's built-in | Phase 6 |
| Async clipboard | `Cmd+C` no-ops on web | Phase 5 |
| `localStorage` window state | None | Future |
| Multi-canvas / multi-view | None | Future |

## Troubleshooting

**The page is blank.** Open DevTools console. The most common cause
is a wasm panic during init — `console_error_panic_hook::set_once()`
puts the message there.

**`Failed to fetch dynamically imported module`.** You're probably
loading `index.html` via `file://` instead of HTTP. Browsers won't
import wasm modules from the file scheme. Run `python3 -m http.server`
in the directory.

**`No element with id 'blinc-canvas'`.** Ensure `index.html` and
`pkg/` are both in the directory you're serving from.

**Tab freezes.** wgpu's debug builds are slow on the wasm side; use
`wasm-pack build --target web --release`.
