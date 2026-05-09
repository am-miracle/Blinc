//! Subsecond hot-reload websocket client (debug builds, behind the
//! `hot-reload` feature).
//!
//! Connects to the `dx serve --hot-patch` dev-server, hands it the
//! running process's ASLR reference + build id + pid as query
//! parameters, and applies incoming jump-table patches via
//! [`subsecond::apply_patch`]. The patch points themselves are
//! installed in `windowed.rs` by wrapping the user's UI builder in
//! [`subsecond::call`]; this module is the missing client side that
//! lets the dev-server compute the ASLR delta and ship a patch.
//!
//! # Why not depend on `dioxus-devtools`?
//!
//! `dioxus-devtools` works fine, but transitively it pulls in
//! `dioxus-core` + `dioxus-signals` + the rest of the Dioxus reactive
//! runtime — none of which Blinc apps want in their dep tree. The
//! wire protocol we actually use is small (one externally-tagged
//! enum carrying a `JumpTable`), so we mirror the relevant subset
//! locally and stay decoupled. We do still depend on
//! [`dioxus_cli_config`] for the env-var conventions (`DIOXUS_DEVSERVER_IP`,
//! `DIOXUS_DEVSERVER_PORT`, `DIOXUS_BUILD_ID`) — that crate is tiny
//! and zero-dep on native, and reusing it means our client speaks
//! dx's protocol without us reverse-engineering the env layout.
//!
//! # Compatibility
//!
//! Tracks `dx` CLI 0.7+. The `DevserverMsg` mirror below is kept
//! deliberately permissive — fields we don't act on (`templates`,
//! `assets`, `for_build_id`, `ms_elapsed`) are accepted as
//! [`serde_json::Value`] / `default`-able primitives so a minor
//! schema bump from dx (e.g. adding a new field) won't break us. If
//! dx ever changes a field we do read (`jump_table`, `for_pid`),
//! we'll need to update this file.

use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use serde::Deserialize;
use subsecond::JumpTable;

/// Set to `true` by the websocket thread after `subsecond::apply_patch`
/// succeeds. The frame loop in `windowed.rs` reads + clears it on the
/// next event loop tick to force a tree rebuild — `subsecond::call`
/// only re-evaluates the closure body on a fresh invocation, so a
/// static UI keeps painting the pre-patch tree until something marks
/// it dirty.
static REBUILD_PENDING: AtomicBool = AtomicBool::new(false);

/// Wake callback the windowed runner registers in `connect()` so the
/// patch thread can nudge the event loop awake after flipping
/// `REBUILD_PENDING`. Without this nudge a window sitting in
/// `ControlFlow::Wait` (no input, no animations) would never look at
/// the flag — the patch silently lands but the user sees nothing
/// until they wiggle the mouse.
type WakeFn = Box<dyn Fn() + Send + Sync + 'static>;
static WAKE_FN: OnceLock<WakeFn> = OnceLock::new();

/// Drain the rebuild-pending flag. Returns `true` if a hot-patch
/// landed since the last call. Called once per event-loop tick by
/// the windowed runner; the cost is a single relaxed atomic read +
/// CAS so it's safe to do on every frame.
pub fn take_rebuild_pending() -> bool {
    REBUILD_PENDING.swap(false, Ordering::AcqRel)
}

/// Subset of `dx` CLI's `DevserverMsg` we deserialise. The full
/// upstream enum carries Dioxus VDOM template patches, asset
/// reload notifications, and various lifecycle pings — we only
/// act on `HotReload`. Unrecognised variants land in `Unknown`
/// and are silently ignored, which lets the server add new
/// message kinds without breaking older clients.
///
/// Default serde representation (externally tagged: `{"HotReload":
/// {...}}`) — matches the upstream type which has no
/// `#[serde(tag = ...)]` attribute.
#[derive(Debug, Deserialize)]
enum DevserverMsg {
    HotReload(HotReloadMsg),
    #[serde(other)]
    Unknown,
}

/// Mirror of `dioxus_devtools_types::HotReloadMsg`. Fields we don't
/// consume are kept permissive so dx schema additions don't break
/// deserialisation.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct HotReloadMsg {
    /// Subsecond binary patch — the only field this client acts on.
    jump_table: Option<JumpTable>,
    /// PID guard the dev-server stamps so apps in a multi-window /
    /// multi-process build don't apply each other's patches.
    for_pid: Option<u32>,
    /// VDOM template patches (Dioxus-only). Captured as opaque JSON
    /// so we don't pull `dioxus-core` into the deserialiser.
    #[allow(dead_code)]
    templates: serde_json::Value,
    /// Asset reload list. Not consumed.
    #[allow(dead_code)]
    assets: serde_json::Value,
    /// Build id stamp for de-duping patches across rebuilds.
    #[allow(dead_code)]
    for_build_id: Option<u64>,
    /// Wall-clock build duration the server reports. Informational.
    #[allow(dead_code)]
    ms_elapsed: Option<u64>,
}

/// Spawn the hot-reload client thread.
///
/// `wake` is invoked after every successful patch — the windowed
/// runner passes its [`WakeProxy::wake`] so the event loop comes out
/// of `ControlFlow::Wait` and looks at [`take_rebuild_pending`]. If
/// the app is a non-windowed runner (or for whatever reason has no
/// wake mechanism) pass `|| {}` and the patch will still apply but
/// won't visibly update the UI until the next natural redraw.
///
/// If `DIOXUS_DEVSERVER_PORT` (and friends) aren't set — the normal
/// `cargo run` case — this returns immediately without spawning a
/// thread. When the env vars are present (the app is a child of
/// `dx serve --hot-patch`), a single background thread connects to
/// the dev-server's websocket, sends the running process's ASLR
/// offset, and processes incoming hot-patch messages until the
/// socket closes.
///
/// Safe and cheap to call unconditionally at app startup.
pub fn connect<F>(wake: F)
where
    F: Fn() + Send + Sync + 'static,
{
    // Register the wake callback regardless of whether dx is running —
    // a future reconnect attempt should reuse the same hook.
    let _ = WAKE_FN.set(Box::new(wake));

    let Some(endpoint) = dioxus_cli_config::devserver_ws_endpoint() else {
        tracing::debug!(
            "hot-reload: DIOXUS_DEVSERVER_PORT not set, not connecting (run under `dx serve` to enable)"
        );
        return;
    };

    let _ = std::thread::Builder::new()
        .name("blinc-hot-reload".into())
        .spawn(move || run(endpoint));
}

fn run(endpoint: String) {
    let uri = format!(
        "{endpoint}?aslr_reference={}&build_id={}&pid={}",
        subsecond::aslr_reference(),
        dioxus_cli_config::build_id(),
        process::id(),
    );

    let (mut ws, _resp) = match tungstenite::connect(&uri) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = ?e, endpoint = %uri, "hot-reload: failed to connect");
            return;
        }
    };

    tracing::info!(endpoint = %uri, "hot-reload: connected");

    while let Ok(msg) = ws.read() {
        if let tungstenite::Message::Text(text) = msg {
            handle_text(text.as_str());
        }
    }

    tracing::debug!("hot-reload: websocket closed");
}

fn handle_text(text: &str) {
    let parsed: DevserverMsg = match serde_json::from_str(text) {
        Ok(p) => p,
        Err(e) => {
            tracing::trace!(error = ?e, text, "hot-reload: ignored unparseable message");
            return;
        }
    };

    let DevserverMsg::HotReload(hot) = parsed else {
        return;
    };
    let Some(jump_table) = hot.jump_table else {
        return;
    };
    if hot.for_pid != Some(process::id()) {
        tracing::trace!(
            for_pid = ?hot.for_pid,
            our_pid = process::id(),
            "hot-reload: patch was for a different process, skipping"
        );
        return;
    }

    // SAFETY: `subsecond::apply_patch` is unsafe because the patcher
    // and the running process must agree on layout / linkage. The dx
    // CLI on the other end is what guarantees that contract — it
    // links the new code against this binary's symbol table and
    // ASLR offset (which we sent as a query parameter on connect).
    // Outside that contract the call would be undefined behaviour;
    // we trust the `for_pid` guard above + the build-id stamp the
    // server already filtered on to keep us in scope.
    unsafe {
        match subsecond::apply_patch(jump_table) {
            Ok(()) => {
                tracing::info!("hot-reload: patch applied");
                // Mark the next event-loop tick as a forced rebuild so
                // the user-supplied UI closure gets re-invoked under
                // `subsecond::call`. Without this the patched function
                // body is loaded but never executed — the renderer
                // keeps painting the cached render tree.
                REBUILD_PENDING.store(true, Ordering::Release);
                if let Some(wake) = WAKE_FN.get() {
                    wake();
                } else {
                    tracing::debug!(
                        "hot-reload: no wake callback registered, UI will refresh on next natural redraw"
                    );
                }
            }
            Err(e) => tracing::warn!(error = ?e, "hot-reload: patch failed"),
        }
    }
}
