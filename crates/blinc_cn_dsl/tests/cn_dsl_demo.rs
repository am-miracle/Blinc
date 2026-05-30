//! Smoke test for `examples/cn_dsl_demo.blinc`.
//!
//! Compiles the same source the runner's `include_str!` pulls in.
//! Catches grammar / cn-registration / Reactive<T> regressions before
//! they hit the demo's startup. Same posture
//! `blinc_dsl_core`'s `example_*_compiles` tests use.

use blinc_dsl_core::BlincDsl;

#[test]
fn example_cn_dsl_demo_compiles() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_all(&dsl).expect("register cn.* widgets");
    dsl.compile_source(
        include_str!("../examples/cn_dsl_demo.blinc"),
        "cn_dsl_demo.blinc",
    )
    .expect("cn_dsl_demo.blinc should compile");
}
