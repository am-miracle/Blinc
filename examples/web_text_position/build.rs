use std::env;
use std::fs;
use std::path::PathBuf;

const EXAMPLE_PATH: &str = "../../crates/blinc_app/examples/text_position_test.rs";

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo"));
    let example_path = manifest_dir.join(EXAMPLE_PATH);

    println!("cargo:rerun-if-changed={}", example_path.display());
    println!("cargo:rerun-if-changed=build.rs");

    let source = fs::read_to_string(&example_path)
        .unwrap_or_else(|e| panic!("Cannot read {}: {e}", example_path.display()));

    let mut bracket_depth: i32 = 0;
    let stripped: String = source
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            if bracket_depth > 0 {
                bracket_depth += trimmed.chars().filter(|c| *c == '[').count() as i32;
                bracket_depth -= trimmed.chars().filter(|c| *c == ']').count() as i32;
                ""
            } else if trimmed.starts_with("//!") {
                ""
            } else if trimmed.starts_with("#![") {
                bracket_depth += trimmed.chars().filter(|c| *c == '[').count() as i32;
                bracket_depth -= trimmed.chars().filter(|c| *c == ']').count() as i32;
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR set by cargo"));
    fs::write(out_dir.join("example.rs"), stripped)
        .expect("Failed to write stripped example to OUT_DIR");
}
