//! Phase A: multi-file compilation via `BlincDsl::compile_directory`.

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
/// result.
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
            import { Counter } from "widgets"
            view { Counter() }
            "#,
        )
        .unwrap();

    let dsl = BlincDsl::new().expect("runtime init");
    let names = dsl
        .compile_project(&entry, dir.path())
        .expect("compile_project");
    assert!(
        names.iter().any(|s| s == "Counter$view"),
        "merged import should expose Counter$view, got: {names:?}"
    );
    assert!(
        names.iter().any(|s| s == "render_view"),
        "entry should expose render_view, got: {names:?}"
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
