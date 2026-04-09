//! `blinc-build-web-examples` — codegen tool that auto-discovers
//! cross-target examples in `crates/blinc_app/examples/*.rs` and
//! emits one wasm wrapper crate per example under
//! `examples/_generated/<name>/`.
//!
//! See [`docs/book/src/contributing/examples.md`] for the convention
//! an example must satisfy to be picked up by this tool. The
//! short version: the example file must define a top-level
//! `pub fn build_ui(ctx: &mut WindowedContext) -> impl ElementBuilder`.
//!
//! # Discovery
//!
//! Every `.rs` file under `crates/blinc_app/examples/` is
//! syntactically parsed. A file is selected if:
//!
//! 1. Its top `//!` doc-comment block does NOT contain `no-web:`
//!    (explicit opt-out marker — the codegen tool skips the file
//!    gracefully and doesn't error).
//! 2. It contains exactly one top-level `pub fn build_ui` item.
//!
//! Files that don't match are silently ignored, which is how we
//! keep examples like `multi_window_demo.rs` (inherently desktop-
//! only) out of the web build without a separate allowlist.
//!
//! # Output
//!
//! For each matched example, the tool writes:
//!
//! - `examples/_generated/<name>/Cargo.toml` — crate manifest
//!   derived from a template; dependency set is inferred from
//!   `use blinc_cn::` / `use blinc_icons::` / etc. imports in the
//!   example source.
//! - `examples/_generated/<name>/build.rs` — strips `//!` inner
//!   doc comments from the upstream example source and writes the
//!   result into `$OUT_DIR/example.rs`. See the hand-crafted
//!   `examples/_generated/scroll/build.rs` for the rationale (rustc
//!   limitation rust-lang/rust#66043 prevents `include!` from
//!   expanding into inner-doc-comment regions).
//! - `examples/_generated/<name>/src/lib.rs` — the wrapper. Gated
//!   `#![cfg(target_arch = "wasm32")]`; brings the example into a
//!   private `mod example { include!(…$OUT_DIR/example.rs) }`; runs
//!   `WebApp::run_with_setup` against `example::build_ui` from a
//!   `#[wasm_bindgen(start)]` entry point.
//! - `examples/_generated/<name>/index.html` — canvas host page
//!   with the WebGPU probe and module bootstrap. Title comes from
//!   the example's first doc-comment line.
//! - `examples/_generated/<name>/serve.sh` — the same static-file
//!   server script every other web example ships.
//!
//! # Idempotency + pruning
//!
//! Running the tool twice produces the same output. After
//! generating fresh wrappers, the tool walks `examples/_generated/`
//! and deletes any subdirectory whose matching upstream example
//! either no longer exists or no longer satisfies the convention
//! (the `pub fn build_ui` check). This keeps the generated tree in
//! sync with the source of truth without manual cleanup.
//!
//! # Running
//!
//! ```ignore
//! # From the workspace root:
//! cargo run -p blinc-build-web-examples
//! ```
//!
//! The tool is intentionally not in `default-members`, so regular
//! `cargo build` / `cargo check --workspace` does not pull it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

// ============================================================================
// Constants / paths
// ============================================================================

/// Directory holding the upstream cross-target examples.
const EXAMPLES_DIR: &str = "crates/blinc_app/examples";

/// Directory where this tool writes generated wrapper crates.
const GENERATED_DIR: &str = "examples/_generated";

/// Crates we know how to infer as wrapper dependencies. Each entry
/// is `(use_path_prefix, cargo_package_name, relative_path_from_wrapper_dir)`.
///
/// The inference is deliberately conservative — we only add a dep
/// if the example actually uses the crate, so wrappers for simple
/// examples stay small. A new workspace crate that examples start
/// depending on needs one line added here.
const INFERABLE_DEPS: &[(&str, &str, &str)] = &[
    ("blinc_animation::", "blinc_animation", "../../../crates/blinc_animation"),
    ("blinc_cn::", "blinc_cn", "../../../crates/blinc_cn"),
    ("blinc_icons::", "blinc_icons", "../../../crates/blinc_icons"),
    (
        "blinc_tabler_icons::",
        "blinc_tabler_icons",
        "../../../crates/blinc_tabler_icons",
    ),
    ("blinc_canvas_kit::", "blinc_canvas_kit", "../../../crates/blinc_canvas_kit"),
    ("blinc_theme::", "blinc_theme", "../../../crates/blinc_theme"),
    ("blinc_text::", "blinc_text", "../../../crates/blinc_text"),
    ("blinc_paint::", "blinc_paint", "../../../crates/blinc_paint"),
    ("blinc_router::", "blinc_router", "../../../crates/blinc_router"),
    ("blinc_svg::", "blinc_svg", "../../../crates/blinc_svg"),
    ("blinc_image::", "blinc_image", "../../../crates/blinc_image"),
    ("blinc_media::", "blinc_media", "../../../crates/blinc_media"),
    ("blinc_platform::", "blinc_platform", "../../../crates/blinc_platform"),
    ("blinc_macros::", "blinc_macros", "../../../crates/blinc_macros"),
];

// ============================================================================
// Discovery + metadata extraction
// ============================================================================

/// Everything the codegen stage needs to know about one example.
#[derive(Debug)]
struct ExampleMeta {
    /// Base name of the source file without `.rs`, e.g. `"scroll"`.
    name: String,
    /// Source file path relative to the workspace root.
    source_path: PathBuf,
    /// First non-empty line of the `//!` doc comment block. Used as
    /// the display title in the index.html `<title>` and in the
    /// gallery page.
    title: String,
    /// Extra `blinc_*` workspace crates this example imports, in
    /// deterministic order. Each entry is `(package_name, path)`.
    extra_deps: Vec<(String, String)>,
}

/// Walk `EXAMPLES_DIR`, parse each `.rs`, and return metadata for
/// every file that passes the convention check.
fn discover_examples(workspace_root: &Path) -> Vec<ExampleMeta> {
    let examples_root = workspace_root.join(EXAMPLES_DIR);
    let entries = match fs::read_dir(&examples_root) {
        Ok(e) => e,
        Err(e) => {
            eprintln!(
                "blinc-build-web-examples: cannot read {}: {e}",
                examples_root.display()
            );
            return Vec::new();
        }
    };

    let mut found: Vec<ExampleMeta> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else {
            continue;
        };

        let source = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "  skip {stem}: cannot read {}: {e}",
                    path.display()
                );
                continue;
            }
        };

        // Opt-out marker: any `//! no-web:` line in the upstream doc
        // block. This is how examples that can't run on the web
        // target (multi-window, filesystem assets, etc.) declare
        // themselves out without a separate manifest.
        if source
            .lines()
            .take_while(|l| l.trim_start().starts_with("//!") || l.trim().is_empty())
            .any(|l| l.contains("no-web:"))
        {
            println!("  skip {stem}: `//! no-web:` opt-out");
            continue;
        }

        let ast = match syn::parse_file(&source) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("  skip {stem}: parse error {e}");
                continue;
            }
        };

        if !has_pub_build_ui(&ast) {
            // Common case for examples that haven't been migrated to
            // the convention yet. Not an error — just not picked up.
            continue;
        }

        let title = extract_title(&source).unwrap_or_else(|| format_fallback_title(&stem));
        let extra_deps = infer_extra_deps(&source);

        let relative_path = path
            .strip_prefix(workspace_root)
            .unwrap_or(&path)
            .to_path_buf();

        println!("  discovered {stem} ({title})");
        found.push(ExampleMeta {
            name: stem,
            source_path: relative_path,
            title,
            extra_deps,
        });
    }

    // Deterministic order so re-runs produce byte-identical output.
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// Returns true if the parsed syn AST contains a top-level item
/// matching `pub fn build_ui(...)`. We don't verify the argument or
/// return types — the upstream desktop build will fail loudly if
/// the signature drifts from the convention, and the wrapper build
/// will fail at `WebApp::run_with_setup(…, build_ui)` if the
/// signature doesn't satisfy its `FnMut(&mut WindowedContext) -> E
/// where E: ElementBuilder` bound. No need to duplicate either
/// check here.
fn has_pub_build_ui(ast: &syn::File) -> bool {
    ast.items.iter().any(|item| {
        if let syn::Item::Fn(f) = item {
            matches!(f.vis, syn::Visibility::Public(_)) && f.sig.ident == "build_ui"
        } else {
            false
        }
    })
}

/// Pull the first non-empty `//!` doc-comment line from the top of
/// the file. Used as the gallery title. We look at the raw source
/// rather than the AST because `syn` collapses inner doc comments
/// into attribute nodes and we want to preserve the original
/// single-line text without round-tripping.
fn extract_title(source: &str) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim_start();
        if let Some(body) = trimmed.strip_prefix("//!") {
            let body = body.trim();
            if !body.is_empty() {
                // Strip a trailing "Example" suffix if present, to
                // make the gallery titles feel less repetitive
                // ("Scroll Container" vs "Scroll Container Example").
                let cleaned = body
                    .strip_suffix(" Example")
                    .or_else(|| body.strip_suffix(" Demo"))
                    .unwrap_or(body);
                return Some(cleaned.to_string());
            }
        } else if trimmed.is_empty() {
            continue;
        } else {
            // First non-comment, non-blank line — we're past the doc
            // block and haven't found anything. Bail.
            return None;
        }
    }
    None
}

/// Fallback title for examples whose doc block is missing or
/// doesn't have a leading text line. We take the file stem, replace
/// underscores with spaces, and title-case each word.
fn format_fallback_title(stem: &str) -> String {
    stem.split('_')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(first) => first.to_uppercase().chain(c).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Scan the example source for `use <crate>::` patterns from our
/// inferable-dep allowlist. Returns the matched entries as a sorted
/// deduplicated list. The scan is a simple substring search — it
/// intentionally doesn't try to be smart about `use foo::bar` vs
/// fully-qualified paths, since both count as "this example
/// depends on `foo`".
fn infer_extra_deps(source: &str) -> Vec<(String, String)> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (prefix, package, path) in INFERABLE_DEPS {
        if source.contains(prefix) {
            out.insert((*package).to_string(), (*path).to_string());
        }
    }
    out.into_iter().collect()
}

// ============================================================================
// Codegen — one wrapper crate per discovered example
// ============================================================================

/// Emit the full set of wrapper files for one example. Overwrites
/// any existing files in the wrapper directory so re-runs pick up
/// edits to the upstream example immediately.
fn generate_wrapper(workspace_root: &Path, meta: &ExampleMeta) -> std::io::Result<()> {
    let wrapper_dir = workspace_root.join(GENERATED_DIR).join(&meta.name);
    let src_dir = wrapper_dir.join("src");
    fs::create_dir_all(&src_dir)?;

    let crate_name = crate_name_for(&meta.name);
    let source_path_str = meta.source_path.to_string_lossy().replace('\\', "/");
    // build.rs reads EXAMPLE_PATH relative to the wrapper dir
    // (`CARGO_MANIFEST_DIR`), which is three levels down from the
    // workspace root (`examples/_generated/<name>/`). So the
    // relative path from the wrapper up to the example is:
    //     ../../../<source_path_from_workspace_root>
    let example_path_from_wrapper = format!("../../../{source_path_str}");

    fs::write(
        wrapper_dir.join("Cargo.toml"),
        render_cargo_toml(&crate_name, meta),
    )?;
    fs::write(
        wrapper_dir.join("build.rs"),
        render_build_rs(&example_path_from_wrapper, &meta.name),
    )?;
    fs::write(src_dir.join("lib.rs"), render_lib_rs(&meta.name, &crate_name))?;
    fs::write(wrapper_dir.join("index.html"), render_index_html(&meta.title, &crate_name))?;

    let serve_sh_path = wrapper_dir.join("serve.sh");
    fs::write(&serve_sh_path, render_serve_sh())?;
    // Make serve.sh executable (best effort; Windows ignores this).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = fs::metadata(&serve_sh_path) {
            let mut perms = metadata.permissions();
            perms.set_mode(0o755);
            let _ = fs::set_permissions(&serve_sh_path, perms);
        }
    }

    // A `.gitignore` in each wrapper excludes the `pkg/` directory
    // produced by wasm-pack. Matches what `examples/web_hello` etc.
    // ship.
    fs::write(wrapper_dir.join(".gitignore"), "pkg/\n")?;

    Ok(())
}

/// Convert an example file stem (`keyframe_canvas`) into a cargo
/// package name (`blinc-example-keyframe-canvas-web`). Cargo
/// package names use hyphens, not underscores; the `example` infix
/// keeps these generated crates from colliding with any real
/// `blinc-<name>-web` crate that happens to exist.
fn crate_name_for(stem: &str) -> String {
    format!("blinc-example-{}-web", stem.replace('_', "-"))
}

// ============================================================================
// Templates — each `render_*` returns the full file contents
// ============================================================================

fn render_cargo_toml(crate_name: &str, meta: &ExampleMeta) -> String {
    let mut extra_deps = String::new();
    for (package, path) in &meta.extra_deps {
        extra_deps.push_str(&format!("{package} = {{ path = \"{path}\" }}\n"));
    }

    format!(
        r#"[package]
# Auto-generated by `tools/build-web-examples`. DO NOT EDIT by hand —
# the next run of the tool will overwrite your changes. To tweak the
# wrapper shape, edit the templates in
# `tools/build-web-examples/src/main.rs`.
#
# Source of truth for this wrapper's behavior is
# `{source_path}` — the upstream example file
# included via `build.rs` + `include!` into `src/lib.rs`.
name = "{crate_name}"
version.workspace = true
edition.workspace = true
license.workspace = true
publish = false

[lib]
crate-type = ["cdylib", "rlib"]

[package.metadata.wasm-pack.profile.release]
wasm-opt = ['-O', '--all-features']

[package.metadata.wasm-pack.profile.dev]
wasm-opt = false

# Strictly a wasm32 wrapper. The desktop side of the same example
# is `cargo run -p blinc_app --example {name} --features windowed`.
# Native builds of this wrapper are intentionally no-ops — the whole
# crate is `#![cfg(target_arch = "wasm32")]`-gated inside `src/lib.rs`.
[target.'cfg(target_arch = "wasm32")'.dependencies]
blinc_app = {{ path = "../../../crates/blinc_app", default-features = false, features = ["web"] }}
blinc_layout = {{ path = "../../../crates/blinc_layout" }}
blinc_core = {{ path = "../../../crates/blinc_core" }}
{extra_deps}wasm-bindgen = "0.2"
wasm-bindgen-futures = "0.4"
web-sys = {{ version = "0.3", features = ["console"] }}
console_error_panic_hook = "0.1"
tracing = {{ workspace = true }}
tracing-wasm = "0.2"
tracing-subscriber = {{ workspace = true, features = ["registry"] }}
"#,
        crate_name = crate_name,
        name = meta.name,
        source_path = meta.source_path.display(),
        extra_deps = extra_deps,
    )
}

fn render_build_rs(example_path_from_wrapper: &str, stem: &str) -> String {
    // Double-hash raw-string delimiter (`r##"..."##`) because the
    // template body contains the two-char sequence `"#` inside
    // `starts_with("#![")`, which would otherwise terminate a
    // single-hash `r#"..."#` literal early.
    format!(
        r##"//! Auto-generated by `tools/build-web-examples`.
//!
//! Pre-processes the upstream example source for consumption by
//! `include!` inside `src/lib.rs`. Strips inner doc comments (`//!`)
//! because `include!` can't paste them into a non-module-start
//! context (rust-lang/rust#66043), while preserving line numbers
//! via blank-line replacement so panic backtraces still point at
//! the right line in the upstream file.
//!
//! Emits `cargo:rerun-if-changed` so cargo re-runs this script
//! whenever the upstream example changes. The wrapper's copy of
//! the example lives in `$OUT_DIR/example.rs` and never ends up in
//! source control.

use std::env;
use std::fs;
use std::path::PathBuf;

/// Path to the upstream example, relative to this wrapper's
/// `CARGO_MANIFEST_DIR` (i.e. `examples/_generated/{stem}/`).
const EXAMPLE_PATH: &str = "{example_path}";

fn main() {{
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo"));
    let example_path = manifest_dir.join(EXAMPLE_PATH);

    println!("cargo:rerun-if-changed={{}}", example_path.display());
    println!("cargo:rerun-if-changed=build.rs");

    let raw = fs::read_to_string(&example_path)
        .unwrap_or_else(|e| panic!("read {{}}: {{e}}", example_path.display()));

    // Drop inner-scope tokens that `include!` can't paste outside
    // the literal start of a module: `//!` doc comments and
    // `#![…]` inner attributes. Both are valid at the top of the
    // upstream example file but illegal when pasted inside the
    // wrapper's `mod example {{ include!(…) }}` body (rustc bug
    // rust-lang/rust#66043). Blank-line replacement preserves
    // line numbers so panic backtraces still point at the right
    // line in the original example.
    //
    // `#![…]` can span multiple lines (e.g. `#![allow(
    //     foo,
    //     bar,
    // )]`), so we track an open-bracket counter while we're
    // inside an inner attribute and keep blanking until brackets
    // balance back to zero.
    let mut bracket_depth: i32 = 0;
    let stripped: String = raw
        .lines()
        .map(|line| {{
            let trimmed = line.trim_start();
            if bracket_depth > 0 {{
                // Mid-attribute continuation line: blank it AND
                // update the bracket balance from this line's
                // contents, so a closing `)]` on its own line ends
                // the attribute cleanly.
                bracket_depth += line.chars().filter(|c| *c == '[').count() as i32;
                bracket_depth -= line.chars().filter(|c| *c == ']').count() as i32;
                ""
            }} else if trimmed.starts_with("//!") {{
                ""
            }} else if trimmed.starts_with("#![") {{
                // Single-line or multi-line inner attribute. Count
                // brackets on the first line to know whether we
                // need to keep eating continuation lines.
                bracket_depth += line.chars().filter(|c| *c == '[').count() as i32;
                bracket_depth -= line.chars().filter(|c| *c == ']').count() as i32;
                ""
            }} else {{
                line
            }}
        }})
        .collect::<Vec<_>>()
        .join("\n");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR set by cargo"));
    let out_path = out_dir.join("example.rs");
    fs::write(&out_path, stripped)
        .unwrap_or_else(|e| panic!("write {{}}: {{e}}", out_path.display()));
}}
"##,
        example_path = example_path_from_wrapper,
        stem = stem,
    )
}

fn render_lib_rs(stem: &str, crate_name: &str) -> String {
    format!(
        r#"//! Auto-generated by `tools/build-web-examples`.
//!
//! Wasm wrapper for the `{stem}` example. The entire crate is
//! `#![cfg(target_arch = "wasm32")]`-gated — native builds of this
//! wrapper are no-ops. The desktop side of the same example is
//! `cargo run -p blinc_app --example {stem} --features windowed`.
//!
//! Flow:
//!
//! 1. `build.rs` reads the upstream example file, strips `//!`
//!    inner doc comments (workaround for rust-lang/rust#66043),
//!    and writes the result to `$OUT_DIR/example.rs`.
//! 2. `mod example {{ include!(…$OUT_DIR/example.rs) }}` pulls the
//!    cleaned example in under a private sub-module. Wrapping in
//!    an inner mod gives the `include!` expansion its own module
//!    namespace and sidesteps symbol collisions with this file.
//! 3. `pub fn build_ui` from the example becomes reachable as
//!    `example::build_ui`, which the `#[wasm_bindgen(start)]` shim
//!    hands to `WebApp::run_with_setup`.
//!
//! See `tools/build-web-examples/src/main.rs` for the codegen
//! logic, and `docs/book/src/contributing/examples.md` for the
//! convention examples must satisfy to be picked up.

#![cfg(target_arch = "wasm32")]

// The upstream example may have carried `#![allow(...)]` inner
// attributes (commonly `deprecated`, `dead_code`, various
// `clippy::*` lints) that `build.rs` stripped because `include!`
// can't paste inner attributes mid-module. Without a
// replacement, every wrapper would re-trip those lints here.
// An outer `#[allow(...)]` on the `mod example` item scopes the
// same suppression to every item inside the included example.
//
// `clippy::all` + `clippy::pedantic` are intentionally wide: example
// code is allowed to be loose about naming, explicit types, and
// idioms in service of clarity. Production crates don't inherit
// this allow — only the auto-generated wrapper does.
#[allow(dead_code, deprecated, unused_imports, unused_variables, unused_mut)]
#[allow(clippy::all, clippy::pedantic, clippy::nursery)]
mod example {{
    include!(concat!(env!("OUT_DIR"), "/example.rs"));
}}

use blinc_app::web::WebApp;
use example::build_ui;
use wasm_bindgen::prelude::*;

/// Bundled font shared with the other web examples in this repo.
/// Browsers can't hand wgpu their system fonts (those live in the
/// compositor's 2D pipeline, not in WebGPU), so the font bytes have
/// to live on the wasm side. Reusing `web_hello/fonts/Arial.ttf`
/// keeps every example's wasm artifact pulling from the same source.
const ARIAL_TTF: &[u8] = include_bytes!("../../../web_hello/fonts/Arial.ttf");

#[wasm_bindgen(start)]
pub fn _start() {{
    console_error_panic_hook::set_once();

    tracing_wasm::set_as_global_default_with_config(
        tracing_wasm::WASMLayerConfigBuilder::new()
            .set_max_level(tracing::Level::INFO)
            .build(),
    );

    wasm_bindgen_futures::spawn_local(async {{
        let result = WebApp::run_with_setup(
            "blinc-canvas",
            |app| {{
                let faces = app.load_font_data(ARIAL_TTF.to_vec());
                web_sys::console::log_1(
                    &format!(
                        "{crate_name}: registered {{faces}} font face(s) from Arial.ttf"
                    )
                    .into(),
                );
            }},
            build_ui,
        )
        .await;

        if let Err(e) = result {{
            web_sys::console::error_1(
                &format!("{crate_name}: WebApp::run failed: {{e}}").into(),
            );
        }}
    }});
}}
"#,
        stem = stem,
        crate_name = crate_name,
    )
}

fn render_index_html(title: &str, crate_name: &str) -> String {
    // Convert crate name back to the JS import shim filename. wasm-pack
    // emits `<package_name_with_underscores>.js`, not hyphens.
    let js_name = crate_name.replace('-', "_");
    format!(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Blinc · {title}</title>
    <style>
      html, body {{
        margin: 0;
        padding: 0;
        height: 100%;
        background: #14141c;
        color: #ededf0;
        font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
      }}
      body {{
        display: flex;
        flex-direction: column;
        align-items: stretch;
        justify-content: stretch;
      }}
      #blinc-canvas {{
        display: block;
        width: 100vw;
        height: 100vh;
      }}
      #unsupported {{
        display: none;
        position: absolute;
        inset: 0;
        align-items: center;
        justify-content: center;
        flex-direction: column;
        gap: 12px;
        font-size: 14px;
        line-height: 1.5;
        text-align: center;
        padding: 24px;
      }}
      #unsupported a {{ color: #6db3ff; }}
      .no-webgpu #blinc-canvas {{ display: none; }}
      .no-webgpu #unsupported {{ display: flex; }}
    </style>
  </head>
  <body>
    <canvas id="blinc-canvas"></canvas>

    <div id="unsupported">
      <strong>WebGPU not available</strong>
      <span>
        Blinc's web target requires WebGPU.
        Try Chrome / Edge 113+, or enable WebGPU in
        <a href="https://caniuse.com/webgpu">your browser</a>.
      </span>
    </div>

    <script type="module">
      const hasWebGPU = "gpu" in navigator;
      const probeCanvas = document.createElement("canvas");
      const hasWebGL2 = !!probeCanvas.getContext("webgl2");

      if (!hasWebGPU && !hasWebGL2) {{
        document.body.classList.add("no-webgpu");
      }} else {{
        const {{ default: init }} = await import("./pkg/{js_name}.js");
        await init();
      }}
    </script>
  </body>
</html>
"#,
        title = title,
        js_name = js_name,
    )
}

fn render_serve_sh() -> &'static str {
    r#"#!/usr/bin/env bash
# Auto-generated by `tools/build-web-examples`.
#
# Serve this example over HTTP for local iteration. Run
# `wasm-pack build --target web --release` first to populate pkg/.

set -euo pipefail

cd "$(dirname "$0")"

PORT="${1:-8000}"

if [ ! -d pkg ]; then
  echo "pkg/ not found. Run \`wasm-pack build --target web --release\` first." >&2
  exit 64
fi

URL="http://localhost:${PORT}/"
echo "Serving $(pwd) on ${URL}"
echo "Press Ctrl-C to stop."
echo

if command -v python3 >/dev/null 2>&1; then
  exec python3 -m http.server "${PORT}"
elif command -v python >/dev/null 2>&1; then
  exec python -m http.server "${PORT}"
elif command -v ruby >/dev/null 2>&1; then
  exec ruby -run -e httpd . -p "${PORT}"
elif command -v npx >/dev/null 2>&1; then
  exec npx --yes http-server -p "${PORT}" -c-1 .
else
  echo "No static HTTP server found. Install one of:" >&2
  echo "  python3, python, ruby, or npx (Node.js)" >&2
  exit 127
fi
"#
}

// ============================================================================
// Pruning — remove wrapper dirs whose upstream example has gone away
// ============================================================================

/// Delete any subdirectory of `examples/_generated/` whose base name
/// is not in `keep_names`. Skips the directory's own marker files
/// (`.gitignore`, `.gitkeep`) so the workspace member glob stays
/// resolvable on a fresh clone.
fn prune_stale_wrappers(workspace_root: &Path, keep_names: &[String]) -> std::io::Result<()> {
    let generated = workspace_root.join(GENERATED_DIR);
    let Ok(entries) = fs::read_dir(&generated) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // Never touch the marker files that keep the directory
        // tracked in git.
        if name.starts_with('.') {
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        if !keep_names.iter().any(|k| k == name) {
            println!("  prune stale wrapper {name}");
            fs::remove_dir_all(&path)?;
        }
    }
    Ok(())
}

// ============================================================================
// main
// ============================================================================

fn main() {
    // Resolve the workspace root from CARGO_MANIFEST_DIR. The tool's
    // own Cargo.toml lives at `tools/build-web-examples/`, so two
    // `..` jumps land us at the workspace root.
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect(
            "CARGO_MANIFEST_DIR set by cargo. Run this tool via `cargo run -p blinc-build-web-examples`.",
        ),
    );
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root two levels above tool crate")
        .to_path_buf();

    println!("blinc-build-web-examples: workspace at {}", workspace_root.display());
    println!("  source   : {}", workspace_root.join(EXAMPLES_DIR).display());
    println!("  output   : {}", workspace_root.join(GENERATED_DIR).display());
    println!();
    println!("Discovering examples…");
    let found = discover_examples(&workspace_root);

    println!();
    println!("Generating {} wrapper crate(s)…", found.len());
    let mut ok = 0usize;
    let mut failed = Vec::new();
    for meta in &found {
        match generate_wrapper(&workspace_root, meta) {
            Ok(()) => {
                println!("  wrote examples/_generated/{}/", meta.name);
                ok += 1;
            }
            Err(e) => {
                eprintln!("  FAILED {}: {e}", meta.name);
                failed.push(meta.name.clone());
            }
        }
    }

    println!();
    println!("Pruning stale wrappers…");
    let keep: Vec<String> = found.iter().map(|m| m.name.clone()).collect();
    if let Err(e) = prune_stale_wrappers(&workspace_root, &keep) {
        eprintln!("  WARN: prune failed: {e}");
    }

    println!();
    println!(
        "Done. {ok}/{total} wrappers written.{extra}",
        ok = ok,
        total = found.len(),
        extra = if failed.is_empty() {
            String::new()
        } else {
            format!(" Failures: {}", failed.join(", "))
        }
    );

    if !failed.is_empty() {
        std::process::exit(1);
    }
}
