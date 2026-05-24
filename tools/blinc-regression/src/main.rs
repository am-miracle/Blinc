//! Regression harness for the reactive-architecture-v2 phase work
//! (see `project_reactive_architecture_v2.md` in dev memory).
//!
//! Two workflows:
//!
//! ## 1. CPU% measurement (substep 1.3 — landed)
//!
//! ```text
//! # Before phase work begins, capture main-branch baseline.
//! # Launches cn_demo as a subprocess, samples its CPU% from outside.
//! # You drive the scenarios (slider drag, toast open, scroll, etc.) by
//! # hand; the harness records the timeseries and dumps JSON on exit.
//! cargo run -p blinc-regression --release -- record --out baselines/main.json
//!
//! # After landing a phase, capture an "after" trace running the same
//! # scenarios.
//! cargo run -p blinc-regression --release -- record --out after-p2.json
//!
//! # Diff them — prints mean / p50 / p95 / max CPU% delta.
//! cargo run -p blinc-regression -- compare \
//!     --baseline baselines/main.json --after after-p2.json --phase 2
//! ```
//!
//! Sampling runs at 100ms intervals via `sysinfo`. Process CPU% is the
//! raw per-process value (matches `top` / `htop` semantics — 200% on
//! Linux/macOS = two cores fully busy). The first ~1s of samples are
//! reported but separately tagged as warmup since cargo/rustc startup
//! dominates that window.
//!
//! ## 2. Scenario reference (substep 1.1 — landed)
//!
//! ```text
//! cargo run -p blinc-regression -- list
//! ```
//!
//! Prints the catalogue of 16 scripted interaction scenarios with the
//! phases each one most exercises. The catalogue is the source of truth
//! for the coverage map in `project_reactive_architecture_v2.md`.
//!
//! The harness does NOT drive scenarios programmatically yet — you run
//! them by hand during `record`. Driving them automatically requires
//! OS-level input injection (X11 / Cocoa / Wayland) which is bigger
//! than the current scope and arguably less reliable than human-driven
//! "do these N gestures in this order" runs.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);
/// Samples within this window after subprocess start are flagged as
/// warmup (cargo build / rustc startup dominates).
const WARMUP_MS: u64 = 1500;

#[derive(Parser, Debug)]
#[command(
    name = "blinc-regression",
    about = "Reactive-architecture-v2 regression harness",
    long_about = "Captures CPU + visual baselines from cn_demo and diffs \
                  against per-phase targets. Source of truth for the \
                  win projections in project_reactive_architecture_v2.md."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Spawn a target subprocess (default: cn_demo release), sample its
    /// CPU% at 100ms intervals, dump JSON timeseries on exit.
    Record {
        /// Output JSON path.
        #[arg(long, default_value = "baselines/main.json")]
        out: PathBuf,
        /// Command + args to run. Default = `cargo run -p
        /// blinc_app_examples --example cn_demo --release --features cn`.
        #[arg(last = true)]
        cmd: Vec<String>,
    },
    /// Read two `record`-produced JSON files and print a CPU% delta:
    /// mean / p50 / p95 / max for baseline, after, and (after - baseline).
    Compare {
        #[arg(long)]
        baseline: PathBuf,
        #[arg(long)]
        after: PathBuf,
        /// Optional phase tag (1-9). Echoes the scenarios that phase
        /// most exercises so you can correlate which numbers should
        /// have moved.
        #[arg(long)]
        phase: Option<u32>,
    },
    /// List the scenario catalogue and which phases each scenario targets.
    List,
}

// ============================================================================
// Scenario catalogue (substep 1.1 — unchanged)
// ============================================================================

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
struct Scenario {
    id: &'static str,
    description: &'static str,
    cn_demo_section: &'static str,
    targets_phases: &'static [u32],
    measure: Measure,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Serialize)]
enum Measure {
    Cpu,
    CpuAndVisual,
    Visual,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        id: "idle",
        description: "cn_demo open, no interaction (compositor fast path baseline)",
        cn_demo_section: "*",
        targets_phases: &[1, 2, 3, 4, 5, 6, 7, 8],
        measure: Measure::Cpu,
    },
    Scenario {
        id: "hover-storm-buttons",
        description: "Sweep mouse rapidly across cn::button row (state-style hover)",
        cn_demo_section: "buttons_section",
        targets_phases: &[2, 5],
        measure: Measure::Cpu,
    },
    Scenario {
        id: "hover-table-rows",
        description: "Sweep mouse across cn::table rows (state-style hover, larger surface)",
        cn_demo_section: "table_section",
        targets_phases: &[2, 5, 7],
        measure: Measure::Cpu,
    },
    Scenario {
        id: "slider-drag",
        description: "Drag cn::slider thumb across full range; targets P3 coalescing + P8 fill",
        cn_demo_section: "slider_section",
        targets_phases: &[2, 3, 8],
        measure: Measure::CpuAndVisual,
    },
    Scenario {
        id: "resizable-drag",
        description: "Drag cn::resizable splitter; targets P3 coalescing",
        cn_demo_section: "resizable_section",
        targets_phases: &[3],
        measure: Measure::Cpu,
    },
    Scenario {
        id: "toast-open-close",
        description: "Open + auto-dismiss a toast; primary motivator for P4 texture cache",
        cn_demo_section: "toast_section",
        targets_phases: &[4, 6],
        measure: Measure::CpuAndVisual,
    },
    Scenario {
        id: "dialog-open-close",
        description: "Open + close a cn::dialog; P4 texture cache target",
        cn_demo_section: "dialog_section",
        targets_phases: &[4],
        measure: Measure::CpuAndVisual,
    },
    Scenario {
        id: "drawer-open-close",
        description: "Open + close a cn::drawer; P4 texture cache target",
        cn_demo_section: "drawer_section",
        targets_phases: &[4],
        measure: Measure::CpuAndVisual,
    },
    Scenario {
        id: "sheet-open-close",
        description: "Open + close a cn::sheet; P4 texture cache target",
        cn_demo_section: "sheet_section",
        targets_phases: &[4],
        measure: Measure::CpuAndVisual,
    },
    Scenario {
        id: "spinner-steady",
        description: "Spinner rotation at 30fps steady; P6 lifecycle target",
        cn_demo_section: "loading_section",
        targets_phases: &[3, 6],
        measure: Measure::Cpu,
    },
    Scenario {
        id: "switch-toggle",
        description: "Toggle a cn::switch; thumb-translate spring + bg transition",
        cn_demo_section: "toggles_section",
        targets_phases: &[4, 6],
        measure: Measure::CpuAndVisual,
    },
    Scenario {
        id: "progress-animated",
        description: "Animated progress bar fill; P8 scale_x target",
        cn_demo_section: "progress_section",
        targets_phases: &[6, 8],
        measure: Measure::Cpu,
    },
    Scenario {
        id: "scroll-momentum",
        description: "Scroll cn::scroll_area with momentum; P7 tiled-cache target",
        cn_demo_section: "scroll_area_section",
        targets_phases: &[7],
        measure: Measure::CpuAndVisual,
    },
    Scenario {
        id: "scroll-table",
        description: "Scroll a long cn::table; P7 + per-row hover invalidation",
        cn_demo_section: "table_section",
        targets_phases: &[5, 7],
        measure: Measure::Cpu,
    },
    Scenario {
        id: "compound-slider-spinner",
        description: "Drag slider while spinner animates; tests simultaneous interaction floor",
        cn_demo_section: "*",
        targets_phases: &[1, 2, 3, 4, 6, 8],
        measure: Measure::Cpu,
    },
    Scenario {
        id: "compound-scroll-hover",
        description: "Scroll while sweeping hover across visible elements",
        cn_demo_section: "*",
        targets_phases: &[5, 7],
        measure: Measure::Cpu,
    },
];

// ============================================================================
// Capture types
// ============================================================================

#[derive(Debug, Serialize, Deserialize)]
struct Trace {
    /// Wall-clock UTC ISO-8601 string for when the recording started.
    captured_at: String,
    /// Command the harness spawned.
    command: Vec<String>,
    /// Total elapsed time the subprocess ran.
    duration_ms: u64,
    /// How the run ended — `"subprocess_exit"` (clean window close)
    /// or `"interrupted"` (Ctrl+C). Lets a reader tell whether the
    /// trace covers the user's intended scenario set in full.
    #[serde(default)]
    exit_reason: String,
    /// One sample per `SAMPLE_INTERVAL`.
    samples: Vec<Sample>,
    /// Aggregate stats over the post-warmup samples.
    summary: Stats,
    /// Same stats including warmup samples, for reference.
    summary_with_warmup: Stats,
}

#[derive(Debug, Serialize, Deserialize)]
struct Sample {
    /// Milliseconds since recording start.
    t_ms: u64,
    /// Per-process CPU% (raw — 200% means two cores busy).
    cpu_pct: f32,
    /// True for samples within the WARMUP_MS window.
    warmup: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Stats {
    mean: f32,
    p50: f32,
    p95: f32,
    max: f32,
    n: usize,
}

impl Stats {
    fn from_samples(values: &[f32]) -> Self {
        if values.is_empty() {
            return Self {
                mean: 0.0,
                p50: 0.0,
                p95: 0.0,
                max: 0.0,
                n: 0,
            };
        }
        let mut sorted: Vec<f32> = values.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mean = values.iter().sum::<f32>() / values.len() as f32;
        let p = |frac: f32| -> f32 {
            let idx = ((sorted.len() as f32 - 1.0) * frac).round() as usize;
            sorted[idx]
        };
        Self {
            mean,
            p50: p(0.50),
            p95: p(0.95),
            max: *sorted.last().unwrap(),
            n: values.len(),
        }
    }
}

// ============================================================================
// Entry point
// ============================================================================

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::List => cmd_list(),
        Cmd::Record { out, cmd } => cmd_record(&out, &cmd),
        Cmd::Compare {
            baseline,
            after,
            phase,
        } => cmd_compare(&baseline, &after, phase),
    }
}

fn cmd_list() -> anyhow::Result<()> {
    println!("blinc-regression scenario catalogue\n");
    println!("({} scenarios)\n", SCENARIOS.len());
    for s in SCENARIOS {
        let phases: Vec<String> = s.targets_phases.iter().map(|p| p.to_string()).collect();
        println!(
            "  {:32}  P{:18}  {}",
            s.id,
            phases.join(","),
            s.description
        );
    }
    println!(
        "\nCoverage map + per-phase regression watchlist: see Testing\n\
         methodology section of project_reactive_architecture_v2.md."
    );
    Ok(())
}

fn cmd_record(out: &Path, user_cmd: &[String]) -> anyhow::Result<()> {
    let (cmd_name, cmd_args, command_display) = if user_cmd.is_empty() {
        let default_args: Vec<String> = [
            "run",
            "-p",
            "blinc_app_examples",
            "--example",
            "cn_demo",
            "--release",
            "--features",
            "cn",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        (
            "cargo".to_string(),
            default_args.clone(),
            std::iter::once("cargo".to_string())
                .chain(default_args)
                .collect::<Vec<_>>(),
        )
    } else {
        (
            user_cmd[0].clone(),
            user_cmd[1..].to_vec(),
            user_cmd.to_vec(),
        )
    };

    // Resolve the output path up-front and print it so the user sees
    // exactly where the trace will land (helps when running from a
    // surprising cwd).
    let out_abs = out
        .canonicalize()
        .ok()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default().join(out));
    eprintln!("→ spawning: {}", command_display.join(" "));
    eprintln!(
        "→ sampling CPU% every {}ms; first {}ms tagged as warmup",
        SAMPLE_INTERVAL.as_millis(),
        WARMUP_MS
    );
    eprintln!("→ will write trace to {}", out_abs.display());
    eprintln!("→ drive your scenarios manually; close the window OR press Ctrl+C when done.\n");

    let start_wall = std::time::SystemTime::now();
    let captured_at = format_utc(start_wall);

    let mut child = Command::new(&cmd_name)
        .args(&cmd_args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn `{}`: {e}", cmd_name))?;

    let pid = sysinfo::Pid::from_u32(child.id());
    let started = Instant::now();
    let mut sys = sysinfo::System::new();
    // First refresh seeds the CPU baseline — sysinfo needs two refreshes
    // before cpu_usage() returns meaningful numbers.
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);

    // Ctrl+C handler — graceful break so we still kill the subprocess
    // AND write the trace. Without this, Ctrl+C SIGINT killed the
    // harness mid-loop and the JSON was never written.
    let interrupted = Arc::new(AtomicBool::new(false));
    {
        let flag = Arc::clone(&interrupted);
        ctrlc::set_handler(move || {
            flag.store(true, Ordering::SeqCst);
        })
        .map_err(|e| anyhow::anyhow!("failed to install Ctrl+C handler: {e}"))?;
    }

    let mut samples: Vec<Sample> = Vec::new();
    let exit_reason = loop {
        std::thread::sleep(SAMPLE_INTERVAL);

        if interrupted.load(Ordering::SeqCst) {
            eprintln!("\n→ Ctrl+C received; killing subprocess and writing trace");
            let _ = child.kill();
            let _ = child.wait();
            break "interrupted";
        }

        // Exit if the subprocess has finished on its own.
        if let Some(status) = child.try_wait()? {
            eprintln!("\n→ subprocess exited (status: {status})");
            break "subprocess_exit";
        }

        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        if let Some(p) = sys.process(pid) {
            let t_ms = started.elapsed().as_millis() as u64;
            samples.push(Sample {
                t_ms,
                cpu_pct: p.cpu_usage(),
                warmup: t_ms < WARMUP_MS,
            });
        }
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    let post_warmup: Vec<f32> = samples
        .iter()
        .filter(|s| !s.warmup)
        .map(|s| s.cpu_pct)
        .collect();
    let all: Vec<f32> = samples.iter().map(|s| s.cpu_pct).collect();

    let trace = Trace {
        captured_at,
        command: command_display,
        duration_ms,
        exit_reason: exit_reason.to_string(),
        summary: Stats::from_samples(&post_warmup),
        summary_with_warmup: Stats::from_samples(&all),
        samples,
    };

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&trace)?;
    std::fs::write(out, json)?;

    eprintln!(
        "\n→ wrote {} ({} samples, {}ms total)",
        out.display(),
        trace.samples.len(),
        trace.duration_ms
    );
    print_stats("summary (post-warmup)", &trace.summary);
    print_stats("summary (with warmup)", &trace.summary_with_warmup);
    Ok(())
}

fn cmd_compare(baseline: &Path, after: &Path, phase: Option<u32>) -> anyhow::Result<()> {
    let base: Trace = serde_json::from_reader(std::fs::File::open(baseline)?)?;
    let aft: Trace = serde_json::from_reader(std::fs::File::open(after)?)?;

    println!("baseline: {}", baseline.display());
    println!("  captured: {}", base.captured_at);
    println!("  duration: {}ms, samples: {}", base.duration_ms, base.samples.len());
    println!("  command:  {}", base.command.join(" "));
    println!();
    println!("after:    {}", after.display());
    println!("  captured: {}", aft.captured_at);
    println!("  duration: {}ms, samples: {}", aft.duration_ms, aft.samples.len());
    println!("  command:  {}", aft.command.join(" "));
    println!();

    print_stats("baseline (post-warmup)", &base.summary);
    print_stats("after    (post-warmup)", &aft.summary);
    println!();

    let delta_mean = aft.summary.mean - base.summary.mean;
    let delta_p50 = aft.summary.p50 - base.summary.p50;
    let delta_p95 = aft.summary.p95 - base.summary.p95;
    let delta_max = aft.summary.max - base.summary.max;
    let pct = |d: f32, b: f32| if b.abs() > 0.001 { d / b * 100.0 } else { 0.0 };

    println!(
        "  delta:  mean {:+.2}% ({:+.1}%)  p50 {:+.2}% ({:+.1}%)  p95 {:+.2}% ({:+.1}%)  max {:+.2}% ({:+.1}%)",
        delta_mean,
        pct(delta_mean, base.summary.mean),
        delta_p50,
        pct(delta_p50, base.summary.p50),
        delta_p95,
        pct(delta_p95, base.summary.p95),
        delta_max,
        pct(delta_max, base.summary.max),
    );

    if let Some(p) = phase {
        if !(1..=9).contains(&p) {
            anyhow::bail!("phase must be 1..=9");
        }
        let relevant: Vec<_> = SCENARIOS
            .iter()
            .filter(|s| s.targets_phases.contains(&p))
            .collect();
        println!(
            "\nphase {p} most exercises these {} scenarios — the numbers above\n\
             should reflect aggregate movement when those scenarios were run\n\
             during the capture:",
            relevant.len()
        );
        for s in relevant {
            println!("  - {}: {}", s.id, s.description);
        }
    }
    Ok(())
}

fn print_stats(label: &str, s: &Stats) {
    println!(
        "  {label}: mean={:6.2}%  p50={:6.2}%  p95={:6.2}%  max={:6.2}%  (n={})",
        s.mean, s.p50, s.p95, s.max, s.n
    );
}

fn format_utc(t: std::time::SystemTime) -> String {
    // Light-weight UTC ISO-8601 without pulling in chrono. Seconds-resolution
    // is plenty for a capture timestamp.
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Compute Y/M/D/H/M/S from secs since 1970-01-01.
    let days = secs / 86_400;
    let mut remaining = secs % 86_400;
    let hour = remaining / 3600;
    remaining %= 3600;
    let minute = remaining / 60;
    let second = remaining % 60;

    // Days → date via a simple civil calendar (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, m, d, hour, minute, second
    )
}
