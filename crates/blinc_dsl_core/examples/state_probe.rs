use blinc_dsl_core::BlincDsl;

fn try_parse(label: &str, src: &str) {
    let dsl = BlincDsl::new().expect("dsl");
    let res = dsl.parse_to_typed_ast(src, &format!("{label}.blinc"));
    println!("[{label}] {:?}", res.map(|p| p.declarations.len()));
}

fn main() {
    let _ = tracing_subscriber::fmt::try_init();

    try_parse("plain+plain",   r#"component A { count: i32, foo: i32 }"#);
    try_parse("state+plain",   r#"component A { state count: i32, foo: i32 }"#);
    try_parse("plain+state",   r#"component A { count: i32, state foo: i32 }"#);
    try_parse("state+state",   r#"component A { state count: i32, state foo: i32 }"#);
    try_parse("just-state",    r#"component A { state count: i32 }"#);

    // Probe the `string` type — original failure used `string` not `i32`.
    try_parse("plain-string",   r#"component A { name: string }"#);
    try_parse("state-string",   r#"component A { state name: string }"#);
    try_parse("state+state-ss", r#"component A { state count: i32, state name: string }"#);
    try_parse("plain+plain-ss", r#"component A { count: i32, name: string }"#);
    try_parse("state+plain-s",  r#"component A { state count: i32, name: string }"#);
}
