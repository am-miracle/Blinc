//! Cross-platform "open a file" with a callback.
//!
//! [`crate::dialog`] is the rich rfd-backed API, but it's
//! **desktop-only** (the `dialogs`/`rfd` feature) and synchronous —
//! `pick()` blocks until the user chooses. The web has neither: there
//! is no synchronous native dialog, and picking happens later via an
//! `<input type="file">` user gesture.
//!
//! This module fills that gap with one callback-based entry point that
//! works on every target:
//!
//! - **Desktop** (`rfd` feature): opens the native dialog with
//!   `rfd::FileDialog::pick_file()` and invokes the callback with the
//!   selected path immediately.
//! - **Web** (`web` feature on wasm32): injects a hidden
//!   `<input type="file">`, clicks it, and invokes the callback with
//!   the chosen file's name when the browser fires `change`. Browsers
//!   never expose a real filesystem path, so the web result is the
//!   file *name*, not a path.
//! - **Otherwise** (desktop without `rfd`): a no-op that calls the
//!   callback with `None`.
//!
//! **Web gesture caveat:** browsers only open the file chooser when
//! `input.click()` runs inside a synchronous user-gesture call stack.
//! Call `open_file_into` directly from the click/tap handler that the
//! user triggered — if the host defers it across a `requestAnimationFrame`
//! or async hop, the browser silently ignores the click.
//!
//! ```ignore
//! use blinc_app::file_open::open_file_into;
//! open_file_into(move |picked| {
//!     if let Some(name_or_path) = picked {
//!         path_signal.set(name_or_path);
//!     }
//! });
//! ```

/// Open a file picker. `on_pick` receives the chosen file's path
/// (desktop) or name (web), or `None` when the user cancelled or no
/// backend is available. On web the callback fires asynchronously
/// (after the `change` event); on desktop it fires synchronously
/// before this function returns.
pub fn open_file_into<F>(on_pick: F)
where
    F: FnOnce(Option<String>) + 'static,
{
    #[cfg(all(not(target_arch = "wasm32"), feature = "rfd"))]
    {
        let picked = rfd::FileDialog::new()
            .pick_file()
            .map(|p| p.display().to_string());
        on_pick(picked);
    }

    #[cfg(all(target_arch = "wasm32", feature = "web"))]
    {
        web_open_file(on_pick);
    }

    // Desktop without the `rfd` feature, or wasm without `web`: no
    // backend. Report cancellation so callers don't hang waiting.
    #[cfg(not(any(
        all(not(target_arch = "wasm32"), feature = "rfd"),
        all(target_arch = "wasm32", feature = "web")
    )))]
    {
        on_pick(None);
    }
}

#[cfg(all(target_arch = "wasm32", feature = "web"))]
fn web_open_file<F>(on_pick: F)
where
    F: FnOnce(Option<String>) + 'static,
{
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    let Some(window) = web_sys::window() else {
        on_pick(None);
        return;
    };
    let Some(document) = window.document() else {
        on_pick(None);
        return;
    };
    let Ok(el) = document.create_element("input") else {
        on_pick(None);
        return;
    };
    let input: web_sys::HtmlInputElement = match el.dyn_into() {
        Ok(i) => i,
        Err(_) => {
            on_pick(None);
            return;
        }
    };
    input.set_type("file");
    // Keep it out of layout — it only exists to trigger the picker.
    let _ = input.style().set_property("display", "none");

    // `change` fires once the user picks (or the dialog is dismissed
    // with a prior selection). `once_into_js` consumes the closure
    // after the single call, so there's no leak beyond the element.
    let input_for_cb = input.clone();
    let doc_for_cb = document.clone();
    let cb = Closure::once_into_js(move |_evt: web_sys::Event| {
        let name = input_for_cb
            .files()
            .and_then(|files| files.get(0))
            .map(|f| f.name());
        // Remove the transient input now that we have the result.
        if let Some(body) = doc_for_cb.body() {
            let _ = body.remove_child(&input_for_cb);
        }
        on_pick(name);
    });
    input.set_onchange(Some(cb.unchecked_ref()));

    if let Some(body) = document.body() {
        let _ = body.append_child(&input);
    }
    input.click();
}
