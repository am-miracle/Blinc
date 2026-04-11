//! `blinc-emoji-scan` — AST-based scanner that walks Rust sources
//! and emits the set of non-ASCII codepoints found in string /
//! char / byte-string literals.
//!
//! The scan drives the emoji font subsetter: Blinc's bundled fallback
//! emoji font only needs glyphs that the application actually uses, so
//! we harvest the codepoint set at build time (or on demand) and ship
//! a subset instead of the full ~10 MB NotoColorEmoji.
//!
//! # Why AST?
//!
//! A naïve byte-level regex would also pick up non-ASCII characters in
//! identifiers, doc comments, attribute arguments, and file paths —
//! none of which end up in the rendered UI. Parsing with `syn` and
//! visiting only `Lit::Str`, `Lit::ByteStr`, `Lit::CStr`, and `Lit::Char`
//! nodes keeps the harvested set tight and prevents the subset from
//! accidentally inflating because a contributor name has an accent.
//!
//! # Usage
//!
//! ```bash
//! # Scan a single crate, print summary to stdout
//! cargo run -p blinc-emoji-scan -- crates/blinc_app/examples
//!
//! # Scan multiple paths, write codepoint set to a file (one
//! # `U+XXXX` per line, sorted, deduplicated)
//! cargo run -p blinc-emoji-scan -- \
//!     --output target/codepoints.txt \
//!     crates/blinc_app/examples crates/blinc_cn/src
//! ```
//!
//! The default output when no `--output` is given is a human summary:
//! total codepoint count, a breakdown by Unicode range, and the first
//! occurrence file:line for each codepoint. Useful for eyeballing the
//! scan result before piping it into the subsetter.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use syn::visit::Visit;
use syn::Lit;

/// A single codepoint observation with the first file/line where it
/// was seen. We only store the *first* occurrence to keep the report
/// short — counting every usage site would be noise for a subsetter.
#[derive(Debug, Clone)]
struct Occurrence {
    file: PathBuf,
    line: usize,
}

/// Accumulator: unique codepoints -> first occurrence.
#[derive(Default)]
struct Harvest {
    /// BTreeMap so output is sorted by codepoint.
    codepoints: BTreeMap<u32, Occurrence>,
    /// Count of files visited (not just Rust files — used for the
    /// "scanned N files" line in the report).
    files_visited: usize,
    /// Count of files that actually contained a non-ASCII codepoint.
    files_with_hits: usize,
}

impl Harvest {
    fn record(&mut self, c: char, file: &Path, line: usize) {
        let cp = c as u32;
        if cp < 0x80 {
            return; // ASCII — font-covered by any baseline font.
        }
        self.codepoints.entry(cp).or_insert_with(|| Occurrence {
            file: file.to_path_buf(),
            line,
        });
    }
}

/// `syn::Visit` implementation that pulls string / char literals out
/// of the parsed AST. `proc-macro2`'s `Span::start()` returns a
/// `LineColumn` whose `line` is 1-based — we pass it through unchanged
/// so the report matches what the user sees in their editor.
struct LiteralVisitor<'a> {
    harvest: &'a mut Harvest,
    file: &'a Path,
}

impl<'a> Visit<'_> for LiteralVisitor<'a> {
    fn visit_lit(&mut self, lit: &Lit) {
        // `proc-macro2::Span::start()` returns a `LineColumn`. On
        // stable Rust the line number is accurate as long as the
        // caller passed `proc_macro2::Span` to `syn::parse_file`
        // (which it did, via `syn::parse_file(source)`).
        let line = match lit {
            Lit::Str(s) => s.span().start().line,
            Lit::ByteStr(s) => s.span().start().line,
            Lit::CStr(s) => s.span().start().line,
            Lit::Char(s) => s.span().start().line,
            _ => return,
        };
        match lit {
            Lit::Str(s) => {
                for c in s.value().chars() {
                    self.harvest.record(c, self.file, line);
                }
            }
            Lit::Char(s) => {
                self.harvest.record(s.value(), self.file, line);
            }
            // Byte strings and C strings are ASCII-only by definition
            // (non-ASCII bytes would be `\u{xxxx}` escapes that resolve
            // to bytes, not codepoints, and they can't appear in the
            // rendered UI as glyphs anyway). Skip them.
            _ => {}
        }
    }
}

/// Recursively walk `root` and scan every `.rs` file found. Symlinks
/// are followed only one level deep to avoid pathological loops in a
/// workspace with `target/` symlinks (the scanner also skips the
/// `target` directory explicitly).
fn walk_and_scan(root: &Path, harvest: &mut Harvest) -> Result<(), Box<dyn std::error::Error>> {
    if !root.exists() {
        return Err(format!("path not found: {}", root.display()).into());
    }
    if root.is_file() {
        scan_file(root, harvest);
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            // Skip target/ and .git/ unconditionally — scanning build
            // artefacts would pick up checked-in test data and dead
            // generated code that isn't actually rendered.
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "target" || name == ".git" || name == "node_modules" {
                continue;
            }
            walk_and_scan(&path, harvest)?;
        } else if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("rs") {
            scan_file(&path, harvest);
        }
    }
    Ok(())
}

fn scan_file(path: &Path, harvest: &mut Harvest) {
    let before = harvest.codepoints.len();
    harvest.files_visited += 1;
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  skip {}: {e}", path.display());
            return;
        }
    };
    let ast = match syn::parse_file(&source) {
        Ok(ast) => ast,
        Err(e) => {
            eprintln!("  skip {} (parse error): {e}", path.display());
            return;
        }
    };
    let mut visitor = LiteralVisitor {
        harvest,
        file: path,
    };
    visitor.visit_file(&ast);
    if harvest.codepoints.len() != before {
        harvest.files_with_hits += 1;
    }
}

/// Group a codepoint into a human-readable Unicode range label.
/// This isn't exhaustive — just the ranges commonly hit by UI code
/// (Latin supplement punctuation, arrows, math symbols, emoji).
/// Everything else falls into "Other".
fn range_label(cp: u32) -> &'static str {
    match cp {
        0x0080..=0x00FF => "Latin-1 Supplement",
        0x2000..=0x206F => "General Punctuation",
        0x2070..=0x209F => "Superscripts & Subscripts",
        0x20A0..=0x20CF => "Currency Symbols",
        0x2100..=0x214F => "Letterlike Symbols",
        0x2190..=0x21FF => "Arrows",
        0x2200..=0x22FF => "Mathematical Operators",
        0x2300..=0x23FF => "Miscellaneous Technical",
        0x2500..=0x257F => "Box Drawing",
        0x2580..=0x259F => "Block Elements",
        0x25A0..=0x25FF => "Geometric Shapes",
        0x2600..=0x26FF => "Miscellaneous Symbols",
        0x2700..=0x27BF => "Dingbats",
        0x2B00..=0x2BFF => "Misc Symbols & Arrows",
        0x1F300..=0x1F5FF => "Miscellaneous Symbols and Pictographs",
        0x1F600..=0x1F64F => "Emoticons",
        0x1F680..=0x1F6FF => "Transport and Map Symbols",
        0x1F700..=0x1F77F => "Alchemical Symbols",
        0x1F900..=0x1F9FF => "Supplemental Symbols and Pictographs",
        0x1FA70..=0x1FAFF => "Symbols and Pictographs Extended-A",
        _ => "Other",
    }
}

/// Human-readable report printed when no `--output` is given. Shows
/// the total count, per-range breakdown, and the first occurrence
/// site for each codepoint (so a developer can jump to it and
/// decide whether to keep the glyph or replace it with an SVG).
fn print_report(harvest: &Harvest) {
    println!("blinc-emoji-scan report");
    println!("=======================");
    println!("Files scanned:     {}", harvest.files_visited);
    println!("Files with glyphs: {}", harvest.files_with_hits);
    println!("Unique codepoints: {}", harvest.codepoints.len());
    println!();

    // Breakdown by range.
    let mut by_range: BTreeMap<&'static str, usize> = BTreeMap::new();
    for cp in harvest.codepoints.keys() {
        *by_range.entry(range_label(*cp)).or_insert(0) += 1;
    }
    println!("By Unicode block:");
    for (label, count) in &by_range {
        println!("  {:<40} {}", label, count);
    }
    println!();

    println!("Codepoints (first occurrence):");
    for (cp, occ) in &harvest.codepoints {
        let c = char::from_u32(*cp)
            .map(|c| c.to_string())
            .unwrap_or_default();
        println!("  U+{:04X}  {}  {}:{}", cp, c, occ.file.display(), occ.line);
    }
}

/// Machine-readable output. One codepoint per line as `U+XXXX`. Sorted
/// and deduplicated. This format is what `pyftsubset` / any subsetter
/// expects as its character list (the `U+XXXX` prefix is standard).
fn write_codepoint_list(path: &Path, harvest: &Harvest) -> std::io::Result<()> {
    let mut out = String::new();
    for cp in harvest.codepoints.keys() {
        out.push_str(&format!("U+{:04X}\n", cp));
    }
    fs::write(path, out)
}

struct Args {
    output: Option<PathBuf>,
    inputs: Vec<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut output = None;
    let mut inputs = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" | "-o" => {
                let path = args
                    .next()
                    .ok_or_else(|| "--output requires a path".to_string())?;
                output = Some(PathBuf::from(path));
            }
            "--help" | "-h" => {
                println!("Usage: blinc-emoji-scan [--output <file>] <path> [<path>...]");
                println!();
                println!("Scans every .rs file under the given paths and reports the set");
                println!("of non-ASCII codepoints found in string / char literals.");
                println!();
                println!("  -o, --output <file>   Write a sorted `U+XXXX` list to <file>");
                println!("                        instead of printing the human report.");
                println!("  -h, --help            Show this message.");
                std::process::exit(0);
            }
            _ => inputs.push(PathBuf::from(arg)),
        }
    }
    if inputs.is_empty() {
        return Err("no input paths given (pass one or more directories or .rs files)".to_string());
    }
    Ok(Args { output, inputs })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("blinc-emoji-scan: {e}");
            eprintln!("run `blinc-emoji-scan --help` for usage");
            return ExitCode::from(2);
        }
    };

    let mut harvest = Harvest::default();
    for input in &args.inputs {
        if let Err(e) = walk_and_scan(input, &mut harvest) {
            eprintln!("blinc-emoji-scan: {e}");
            return ExitCode::FAILURE;
        }
    }

    if let Some(out) = &args.output {
        if let Err(e) = write_codepoint_list(out, &harvest) {
            eprintln!("blinc-emoji-scan: failed to write {}: {e}", out.display());
            return ExitCode::FAILURE;
        }
        eprintln!(
            "blinc-emoji-scan: wrote {} codepoints to {}",
            harvest.codepoints.len(),
            out.display()
        );
    } else {
        print_report(&harvest);
    }
    ExitCode::SUCCESS
}
