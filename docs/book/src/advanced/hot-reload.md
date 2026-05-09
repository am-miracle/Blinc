# Hot-reload (experimental)

> **Status:** Experimental, debug-only. Currently driven by Dioxus's
> [`dx`](https://github.com/DioxusLabs/dioxus) CLI through its
> `--hotpatch` mode. A native `blinc dev` driver that vendors the
> websocket protocol is on the roadmap (issue #30, level 2). This
> page documents the level-1 integration: the in-process patch
> point is wired, but the daemon side is borrowed from `dx`.

Blinc can hot-patch the body of your UI builder closure while the
app is running, so iterating on a layout or styling tweak doesn't
need a full rebuild + relaunch. State held by `Stateful` widgets
and hooks (`use_state`, `use_shared_state`, …) is preserved across
patches because Blinc keys it by `InstanceKey` rather than by
closure identity — your reactive graph survives the swap.

The integration uses the [`subsecond`](https://crates.io/crates/subsecond)
crate. `subsecond` is itself gated on `debug_assertions`, so even if
you ship a release binary with the `hot-reload` feature enabled, the
patch points compile down to direct calls and there is zero runtime
overhead.

## Setup

1. Enable the feature on `blinc_app`:

   ```toml
   [dependencies]
   blinc_app = { version = "0.5", features = ["windowed", "hot-reload"] }
   ```

2. Install the Dioxus CLI (one-time):

   ```bash
   cargo install dioxus-cli
   ```

3. Run your binary crate under `dx`:

   ```bash
   dx serve --hotpatch
   ```

   `dx` builds your binary, launches it, and watches the source
   tree. When it detects a change in the binary crate it compiles a
   patch, ships it over a local websocket, and `subsecond` applies
   it in place. The next frame Blinc renders will pick up the new
   closure body.

## What gets hot-reloaded

- **Code in the binary crate** — i.e. the crate where `main.rs`
  lives. `subsecond` only patches the "tip" crate; this is a
  hard limitation of the dynamic-linking approach it uses.
- **State held by `Stateful` widgets and hooks**. Blinc's
  `InstanceKey` is derived from `#[track_caller]` + a per-frame
  call counter, so the same logical widget gets the same key
  before and after a patch. `use_state`, `use_shared_state`,
  reactive signals, and FSM state all survive.

## What doesn't

- **Code in workspace dependencies** (`blinc_layout`, `blinc_app`,
  `blinc_gpu`, etc.). Editing the framework itself still requires
  a full rebuild.
- **Static initialisers in the patched crate.** `subsecond`
  tracks statics across patches but does not re-run their
  destructors, and thread-locals get reset. If your binary crate
  initialises a heavy `OnceLock` at startup, that initialiser
  won't re-run after a patch.
- **Struct field-layout changes.** `subsecond` cannot safely
  patch a closure if a struct it captures has had fields added,
  removed, or reordered — the in-memory layout has shifted out
  from under the patched code. In practice this means: change
  *behaviour* freely, but if you change a field on a struct that
  participates in your reactive graph (e.g. a custom
  `#[derive(BlincComponent)]` struct), restart.
- **Release builds.** `subsecond::call` is a no-op when
  `debug_assertions` is off. Release builds compile the
  `hot-reload` feature in but receive no patches at runtime.

## Multi-window apps

The integration patches both the primary-window UI builder and any
secondary windows opened via `WindowedApp::create_window(...)`.
Each window's builder closure goes through its own `subsecond::call`
patch point, so a code change is picked up on the next frame of
every open window.

## Troubleshooting

- **"Patch had no effect."** Double-check that the change is in
  your binary crate, not in a workspace dependency. `dx serve
  --hotpatch` prints which crate it patched; if the line doesn't
  mention your binary, the edit was outside the patchable scope.
- **State got cleared after a patch.** This means `subsecond`
  detected a structural change (a captured struct's fields moved)
  and forced a full re-instance. The next frame rebuilds from
  scratch. This is expected — restart and rebuild if you need the
  old state back.
- **Crash after a patch.** Most likely a struct-layout edit that
  squeaked past `subsecond`'s safety checks. Restart the app to
  recover, then file an issue against Blinc with the diff that
  triggered it. Even if the root cause is in `subsecond`, the
  Blinc team wants to know which patterns are unsafe in practice
  so we can document them here.

## Roadmap

Level 1 (this milestone) installs the in-process patch points
behind a feature flag so the integration can be exercised against
the Dioxus CLI today.

Level 2 will vendor the websocket protocol so `blinc dev` becomes
a first-party command — no `dx` install, no Dioxus dependency at
runtime, and a chance to tighten the rebuild-detection rules to
Blinc's tree (e.g. invalidate `Stylesheet` caches when CSS-only
files change).
