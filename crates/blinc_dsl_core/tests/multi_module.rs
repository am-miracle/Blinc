//! Multi-file compilation via `BlincDsl::compile_directory`.

use blinc_dsl_core::BlincDsl;
use std::time::{SystemTime, UNIX_EPOCH};

/// Build a uniquely-named temp dir for the test and clean it up
/// on drop. Sidesteps adding `tempfile` to dev-deps.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(prefix: &str) -> std::io::Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("{prefix}_{nanos}"));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    fn write(&self, name: &str, source: &str) -> std::io::Result<std::path::PathBuf> {
        let p = self.0.join(name);
        std::fs::write(&p, source)?;
        Ok(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `compile_directory` walks `*.blinc` files in lex order,
/// compiles each, and returns the per-file function-name map.
#[test]
fn compile_directory_emits_per_file_function_names() {
    let _ = tracing_subscriber::fmt::try_init();

    let dir = TempDir::new("blinc_multi_module_basic").expect("tempdir");
    dir.write(
        "counter.blinc",
        r#"component Counter { view { Text("hello") } }"#,
    )
    .unwrap();
    dir.write(
        "greeting.blinc",
        r#"component Greeting { view { Text("hi") } }"#,
    )
    .unwrap();

    let dsl = BlincDsl::new().expect("runtime init");
    let by_file = dsl.compile_directory(dir.path()).expect("compile dir");

    assert_eq!(by_file.len(), 2, "should compile both .blinc files");
    let all: Vec<String> = by_file.values().flatten().cloned().collect();
    assert!(
        all.iter().any(|s| s == "Counter$view"),
        "Counter$view missing: {all:?}"
    );
    assert!(
        all.iter().any(|s| s == "Greeting$view"),
        "Greeting$view missing: {all:?}"
    );
}

/// ES6 import: an entry file imports a component declared in
/// a sibling file; `compile_project` resolves the dependency
/// through the registered filesystem resolver, merges the
/// imported decls into the entry program, and JIT-compiles the
/// result. With `apply_module_namespace_prefix` in the pipeline,
/// `Counter` from `widgets.blinc` becomes `widgets$Counter` —
/// the entry's `Counter()` call gets rewritten to that mangled
/// name by `inject_imported_view_externs` so the JIT symbol
/// resolves cleanly.
#[test]
fn compile_project_resolves_es6_imports() {
    let _ = tracing_subscriber::fmt::try_init();

    let dir = TempDir::new("blinc_project_import").expect("tempdir");
    dir.write(
        "widgets.blinc",
        r#"component Counter { view { Text("counted") } }"#,
    )
    .unwrap();
    let entry = dir
        .write(
            "main.blinc",
            r#"
            import { Counter } from "./widgets"
            view { Counter() }
            "#,
        )
        .unwrap();

    let dsl = BlincDsl::new().expect("runtime init");
    let names = dsl
        .compile_project(&entry, dir.path())
        .expect("compile_project");
    assert!(
        names.iter().any(|s| s == "widgets$Counter$view"),
        "merged import should expose mangled `widgets$Counter$view`, got: {names:?}"
    );
    assert!(
        names.iter().any(|s| s == "render_view"),
        "entry should expose render_view (the entry view body is not a component, so it stays un-mangled), got: {names:?}"
    );
}

/// Nested ES6 path: `import { X } from "./ui/widgets"` resolves to
/// `<root>/ui/widgets.blinc` via the NodeStyle resolver. With the
/// namespace prefix derived from path-relative-to-source-root, the
/// nested file's `Counter` becomes `ui$widgets$Counter` — multi-
/// segment path components join with the same `$` separator
/// `apply_module_namespace_prefix` uses for the class name itself.
#[test]
fn compile_project_resolves_nested_es6_path() {
    let _ = tracing_subscriber::fmt::try_init();

    let dir = TempDir::new("blinc_project_nested").expect("tempdir");
    std::fs::create_dir_all(dir.path().join("ui")).unwrap();
    std::fs::write(
        dir.path().join("ui/widgets.blinc"),
        r#"component Counter { view { Text("c") } }"#,
    )
    .unwrap();
    let entry = dir
        .write(
            "main.blinc",
            r#"
            import { Counter } from "./ui/widgets"
            view { Counter() }
            "#,
        )
        .unwrap();

    let dsl = BlincDsl::new().expect("runtime init");
    let names = dsl
        .compile_project(&entry, dir.path())
        .expect("compile_project");
    assert!(
        names.iter().any(|s| s == "ui$widgets$Counter$view"),
        "nested import should expose `ui$widgets$Counter$view` (path segments joined with `$`), got: {names:?}"
    );
}

/// Two files each declaring a `component Counter` no longer collide
/// in the JIT symbol table or the component registry — they emit
/// `<module>$Counter$view` and `<other_module>$Counter$view` as
/// distinct symbols. The entry imports both (the last-imported
/// `Counter` wins at the use-site in the entry's own source until
/// alias support lands as a follow-up), but `compile_project` walks
/// every transitive import and both files get compiled to distinct
/// mangled symbols regardless of which one the entry's view body
/// actually references.
///
/// Regression-covers the cross-file collision case the namespacing
/// pass exists to prevent — pre-namespacing, both `Counter$view`
/// symbols would have collapsed onto a single entry in the JIT
/// symbol table and the component registry, and whichever file
/// compiled last would silently overwrite the other.
#[test]
fn cross_file_same_named_components_do_not_collide() {
    let _ = tracing_subscriber::fmt::try_init();

    let dir = TempDir::new("blinc_project_collision").expect("tempdir");
    dir.write("red.blinc", r#"component Counter { view { Text("red") } }"#)
        .unwrap();
    dir.write(
        "blue.blinc",
        r#"component Counter { view { Text("blue") } }"#,
    )
    .unwrap();
    // Entry imports both. Without alias support both bring the local
    // name `Counter` into the entry, but each file's own component
    // still gets compiled to its mangled symbol. `compile_project`
    // walks every import so both red.blinc and blue.blinc are
    // included in the aggregated names list.
    let entry = dir
        .write(
            "main.blinc",
            r#"
            import { Counter } from "./red"
            import { Counter } from "./blue"
            view { Counter() }
            "#,
        )
        .unwrap();

    let dsl = BlincDsl::new().expect("runtime init");
    let names = dsl
        .compile_project(&entry, dir.path())
        .expect("compile_project");

    assert!(
        names.iter().any(|s| s == "red$Counter$view"),
        "red module's Counter should produce `red$Counter$view`, got: {names:?}"
    );
    assert!(
        names.iter().any(|s| s == "blue$Counter$view"),
        "blue module's Counter should produce `blue$Counter$view`, got: {names:?}"
    );
    // Un-mangled `Counter$view` must NOT appear — every component
    // declared inside a `compile_project` run carries its module
    // prefix.
    assert!(
        !names.iter().any(|s| s == "Counter$view"),
        "no un-mangled `Counter$view` should leak, got: {names:?}"
    );
}

/// `recompile_file` re-runs compile for a single path and
/// refreshes the per-file function-name map. Pins the hot-
/// reload entry point.
#[test]
fn recompile_file_replaces_per_file_tracking() {
    let _ = tracing_subscriber::fmt::try_init();

    let dir = TempDir::new("blinc_multi_module_reload").expect("tempdir");
    let path = dir
        .write(
            "widget.blinc",
            r#"component Widget { view { Text("v1") } }"#,
        )
        .unwrap();

    let dsl = BlincDsl::new().expect("runtime init");
    dsl.compile_file(&path).expect("initial compile");
    let v1_names = dsl.compiled_function_names(&path).expect("tracked");
    assert!(v1_names.iter().any(|s| s == "Widget$view"));

    // Edit + recompile. Non-destructive: substrate state for
    // Widget survives the swap (registry replace-by-name).
    std::fs::write(&path, r#"component Widget { view { Text("v2") } }"#).unwrap();
    dsl.recompile_file(&path).expect("hot reload");

    let v2_names = dsl.compiled_function_names(&path).expect("re-tracked");
    assert!(
        v2_names.iter().any(|s| s == "Widget$view"),
        "Widget$view should still be in the post-reload set"
    );
}
