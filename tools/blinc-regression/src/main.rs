//! Regression harness for the reactive-architecture-v2 phase work
//! (see `project_reactive_architecture_v2.md` in dev memory).
//!
//! Phase 1 of P1 lands this as a skeleton — scenario catalogue + CLI shape.
//! Actual scripted interaction + CPU/visual capture will land alongside
//! P1's plumbing as harness fidelity is needed (today the binary just
//! enumerates the scenarios and writes a baseline manifest, so phase work
//! has a stable reference point even before the capture step exists).
//!
//! Workflow:
//!
//!   # Before phase work begins
//!   cargo run -p blinc-regression -- baseline --out baselines/main.json
//!
//!   # After phase work lands
//!   cargo run -p blinc-regression -- compare \
//!       --baseline baselines/main.json \
//!       --phase 1 \
//!       --out reports/phase-1.json
//!
//! See `project_reactive_architecture_v2.md` Testing methodology section
//! for the coverage map (cn_demo section → phases it targets) and the
//! per-phase regression watchlist.

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

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
    /// Capture the current main-branch baseline (CPU + visual) across all
    /// scenarios. Writes a manifest file the `compare` step reads back.
    Baseline {
        /// Output manifest path (JSON).
        #[arg(long, default_value = "baselines/main.json")]
        out: String,
    },
    /// Re-run scenarios and diff against a captured baseline. Use after
    /// landing a phase to validate the envelope.
    Compare {
        #[arg(long)]
        baseline: String,
        /// Which phase landed (1-9). Determines the scenarios scrutinised
        /// most + the expected envelope delta.
        #[arg(long)]
        phase: u32,
        #[arg(long, default_value = "reports/phase.json")]
        out: String,
    },
    /// List the scenario catalogue and which phases each scenario targets.
    List,
}

/// A scripted interaction scenario against `cn_demo`. Each maps to a
/// section in `examples/blinc_app_examples/examples/cn_demo.rs` plus an
/// interaction script (hover sweep, drag, open/close, scroll, etc.).
///
/// `Serialize` only — this is a `const` catalogue baked into the binary,
/// never read back from disk. Runtime `Manifest` / `ScenarioResult`
/// carry the deserialisable mirror types.
#[derive(Debug, Clone, Serialize)]
struct Scenario {
    /// Stable identifier, kebab-case. Used in manifest filenames + report keys.
    id: &'static str,
    /// Human description for `list` output.
    description: &'static str,
    /// `cn_demo.rs` section function name (without parens) this scenario
    /// exercises. E.g. `"slider_section"`.
    cn_demo_section: &'static str,
    /// Phases whose envelope projection includes this scenario.
    targets_phases: &'static [u32],
    /// What kind of measurement matters for this scenario.
    measure: Measure,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Serialize)]
enum Measure {
    /// CPU% timeseries over the scripted interaction.
    Cpu,
    /// Both CPU% and pixel-diff against the baseline screenshot.
    CpuAndVisual,
    /// Visual only — pure rendering test, no interaction.
    Visual,
}

/// Scenario catalogue. Coupled to `project_reactive_architecture_v2.md`'s
/// per-phase coverage map — when adding scenarios here, update that doc.
const SCENARIOS: &[Scenario] = &[
    // ── idle / static (compositor fast path should engage) ──────────
    Scenario {
        id: "idle",
        description: "cn_demo open, no interaction (compositor fast path baseline)",
        cn_demo_section: "*",
        targets_phases: &[1, 2, 3, 4, 5, 6, 7, 8],
        measure: Measure::Cpu,
    },
    // ── hover / state-style ────────────────────────────────────────
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
    // ── drag-driven ────────────────────────────────────────────────
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
    // ── overlay enter/exit (P4 subtree-as-texture) ─────────────────
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
    // ── animation steady-state ─────────────────────────────────────
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
    // ── scroll ────────────────────────────────────────────────────
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
    // ── compound (cross-component CPU stack) ───────────────────────
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

// Placeholder serialisable types for the eventual capture step.
// Allowed dead code while the harness is a skeleton.
#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    /// Captured-against branch + commit SHA (filled by `baseline`).
    git_ref: String,
    scenarios: Vec<ScenarioResult>,
}

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize)]
struct ScenarioResult {
    id: String,
    /// CPU% measurements timeseries (placeholder — capture not wired yet).
    cpu_pct: Vec<f32>,
    /// Average CPU% over the scenario (the headline number used in the
    /// envelope table). NaN if Visual-only.
    cpu_avg: f32,
    /// Path to captured screenshot, if any.
    screenshot_path: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::List => cmd_list(),
        Cmd::Baseline { out } => cmd_baseline(&out),
        Cmd::Compare {
            baseline,
            phase,
            out,
        } => cmd_compare(&baseline, phase, &out),
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

fn cmd_baseline(_out: &str) -> anyhow::Result<()> {
    eprintln!(
        "[skeleton] baseline capture not yet wired — placeholder for the\n\
         scripted-interaction + CPU/visual capture harness that lands\n\
         alongside P1's plumbing finalisation."
    );
    eprintln!(
        "Catalogue size: {} scenarios. Run `list` to see them.",
        SCENARIOS.len()
    );
    Ok(())
}

fn cmd_compare(_baseline: &str, phase: u32, _out: &str) -> anyhow::Result<()> {
    if !(1..=9).contains(&phase) {
        anyhow::bail!("phase must be 1..=9 (see project_reactive_architecture_v2.md)");
    }
    let relevant: Vec<_> = SCENARIOS
        .iter()
        .filter(|s| s.targets_phases.contains(&phase))
        .collect();
    eprintln!(
        "[skeleton] compare not yet wired. Phase {phase} would scrutinise \
         {} scenarios:",
        relevant.len()
    );
    for s in relevant {
        eprintln!("  - {}: {}", s.id, s.description);
    }
    Ok(())
}
