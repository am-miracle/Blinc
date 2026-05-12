//! Smoke probe — does compile_source now succeed against the Zyntax
//! checkout post-fix?

use blinc_dsl_core::BlincDsl;

fn main() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .try_init();

    let dsl = BlincDsl::new().expect("dsl");
    let res = dsl.compile_source(
        r##"
        signal count: i32
        view {
            Div(on_click = || { count.set(count.get() + 1) }) { Text("+1") }
        }
        "##,
        "probe_closure.blinc",
    );
    println!("compile result: {res:?}");

    if res.is_ok() {
        let renderer = dsl.view_renderer();
        let value = blinc_runtime::view::render_main(&renderer);
        println!("render_main result: {value:?}");
    }
}
