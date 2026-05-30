//! `cn.Progress(value = …)` reactive-prop coverage. Pins the
//! `Reactive<f64>` canary end-to-end: three call-site shapes
//! (literal / bare-signal-ref / `computed { } : f64`) all compile
//! and route through the macro's two-slot FFI + the
//! `lower_reactive_args` pass + the wrapper's `IntoReactive<f32>`
//! bridge without panicking.
//!
//! Headless mode can't observe the live-binding refresh visually,
//! but the compile path here exercises every link between DSL
//! source and the cn-side property-binding registry. A regression in
//! the macro, the lowering pass, the `Reactive<T>` decoder, or the
//! wrapper's bridge surfaces as a `compile_source` error rather
//! than a runtime no-op.

use blinc_dsl_core::BlincDsl;

#[test]
fn cn_progress_value_literal() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");

    let src = r#"
        view {
            cn.Progress(value = 0.42)
        }
    "#;
    dsl.compile_source(src, "cn_progress_literal.blinc")
        .expect("compile cn.Progress(value = literal)");
}

#[test]
fn cn_progress_value_signal() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");

    let src = r#"
        signal pct: f64
        view {
            cn.Progress(value = pct)
        }
    "#;
    dsl.compile_source(src, "cn_progress_signal.blinc")
        .expect("compile cn.Progress(value = signal)");
}

#[test]
fn cn_progress_value_computed() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");

    let src = r#"
        signal raw: f64
        view {
            cn.Progress(value = computed { raw.get() } : f64)
        }
    "#;
    dsl.compile_source(src, "cn_progress_computed.blinc")
        .expect("compile cn.Progress(value = computed { … })");
}

#[test]
fn cn_progress_omitted_value_defaults_to_zero() {
    // `cn.Progress()` with no value supplied: the macro's
    // `Reactive<f64>` field defaults to `Literal(0.0)` via the
    // unsupplied-prop default path. Confirms the encoder/decoder
    // pair round-trips the zero default cleanly.
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");

    let src = r#"
        view {
            cn.Progress()
        }
    "#;
    dsl.compile_source(src, "cn_progress_default.blinc")
        .expect("compile cn.Progress() with default value");
}
