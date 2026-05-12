use blinc_dsl_core::BlincDsl;

const SOURCE: &str = r##"
signal count: i32

view {
    Div(on_click = || {
        match "fast" {
            "fast" -> count.set(1),
            _      -> count.set(0),
        }
    }) { Text("tick") }
}
"##;

fn main() {
    let _ = tracing_subscriber::fmt().try_init();
    let dsl = BlincDsl::new().expect("dsl");
    let res = dsl.compile_source(SOURCE, "probe.blinc");
    println!("baseline: {res:?}");
}
