use blinc_dsl_core::BlincDsl;

// Several incrementally-complex probes — each prints `Ok/Err` so we can
// see exactly which DSL feature first trips today's grammar.

const SOURCES: &[(&str, &str)] = &[
    (
        "1. signal + view, no match",
        r##"
signal count: i32

view { Text("hi") }
"##,
    ),
    (
        "2. match at top-level view body (string scrutinee)",
        r##"
signal count: i32

view {
    match "fast" {
        "fast" -> count.set(1),
        _      -> count.set(0),
    }
    Text("tick")
}
"##,
    ),
    (
        "3. match inside Div on_click closure (the original probe)",
        r##"
signal count: i32

view {
    Div(on_click = || {
        match "fast" {
            "fast" -> count.set(1),
            _      -> count.set(0),
        }
    }) { Text("tick") }
}
"##,
    ),
];

fn main() {
    let _ = tracing_subscriber::fmt().try_init();
    let dsl = BlincDsl::new().expect("dsl");
    for (label, src) in SOURCES {
        match dsl.compile_source(src, "probe.blinc") {
            Ok(syms) => println!("{label}: Ok({} symbols)", syms.len()),
            Err(e) => println!("{label}: Err({e})"),
        }
    }
}
