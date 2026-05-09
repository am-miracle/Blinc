//! Blinc CLI
//!
//! Build, run, and hot-reload Blinc applications.

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;
use tracing::{info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

mod config;
mod doctor;
mod project;

use config::BlincConfig;

#[derive(Parser)]
#[command(name = "blinc")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Blinc UI Framework CLI", long_about = None)]
struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build a Blinc application
    Build {
        /// Source file or directory
        #[arg(default_value = ".")]
        source: String,

        /// Target platform (desktop, android, ios, macos, windows, linux)
        #[arg(short, long, default_value = "desktop")]
        target: String,

        /// Build in release mode
        #[arg(short, long)]
        release: bool,

        /// Output path
        #[arg(short, long)]
        output: Option<String>,
    },

    /// Run a Blinc application with hot-reload (development mode)
    Dev {
        /// Source file or directory
        #[arg(default_value = ".")]
        source: String,

        /// Target platform
        #[arg(short, long, default_value = "desktop")]
        target: String,

        /// Port for hot-reload server
        #[arg(short, long, default_value = "3000")]
        port: u16,

        /// How to compile changes — `rust` (default, drives subsecond
        /// hot-patches) or `dsl` (Zyntax DSL, in plan).
        #[arg(short = 'm', long, value_enum, default_value_t = DevMode::Rust)]
        mode: DevMode,

        /// Device to run on (for mobile targets)
        #[arg(long)]
        device: Option<String>,
    },

    /// Run a compiled Blinc application
    Run {
        /// Compiled binary or source file
        #[arg(default_value = ".")]
        source: String,
    },

    /// Build a ZRTL plugin
    Plugin {
        #[command(subcommand)]
        command: PluginCommands,
    },

    /// Create a new Blinc project
    New {
        /// Project name
        name: String,

        /// Template to use (default, minimal, counter)
        #[arg(short, long, default_value = "default")]
        template: String,

        /// Organization/package prefix (e.g., "com.mycompany" results in "com.mycompany.appname")
        #[arg(short, long, default_value = "com.example")]
        org: String,

        /// Create a Rust-first project (native code instead of .blinc DSL)
        #[arg(long)]
        rust: bool,
    },

    /// Initialize a Blinc project in the current directory
    Init {
        /// Template to use
        #[arg(short, long, default_value = "default")]
        template: String,

        /// Organization/package prefix (e.g., "com.mycompany" results in "com.mycompany.appname")
        #[arg(short, long, default_value = "com.example")]
        org: String,
    },

    /// Check a Blinc project for errors
    Check {
        /// Source file or directory
        #[arg(default_value = ".")]
        source: String,
    },

    /// Show toolchain and target information
    Info,

    /// Check platform setup and dependencies
    Doctor,
}

/// How `blinc dev` compiles your project on file change.
///
/// Defaults to `rust` because the Zyntax DSL toolchain is still in
/// planning. Once it lands, `dsl` will route through Zyntax's
/// Runtime2 JIT instead of cargo + subsecond.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum DevMode {
    /// Hot-patch a Rust binary crate via the `subsecond` runtime
    /// (driven by `cargo build` + the dx-CLI websocket protocol
    /// today; native driver is on the roadmap).
    #[default]
    Rust,
    /// Re-compile a `.blinc` DSL project through Zyntax Grammar2 +
    /// Runtime2 JIT and push the new bytecode into the running app.
    /// Currently unimplemented — emits a friendly "not yet ready"
    /// message and exits.
    Dsl,
}

#[derive(Subcommand)]
enum PluginCommands {
    /// Build a plugin
    Build {
        /// Plugin directory
        #[arg(default_value = ".")]
        path: String,

        /// Plugin mode (dynamic or static)
        #[arg(short, long, default_value = "dynamic")]
        mode: String,
    },

    /// Create a new plugin project
    New {
        /// Plugin name
        name: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };

    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(filter)
        .init();

    match cli.command {
        Commands::Build {
            source,
            target,
            release,
            output,
        } => cmd_build(&source, &target, release, output.as_deref()),

        Commands::Dev {
            source,
            target,
            port,
            device,
            mode,
        } => cmd_dev(&source, &target, port, device.as_deref(), mode),

        Commands::Run { source } => cmd_run(&source),

        Commands::Plugin { command } => match command {
            PluginCommands::Build { path, mode } => cmd_plugin_build(&path, &mode),
            PluginCommands::New { name } => cmd_plugin_new(&name),
        },

        Commands::New {
            name,
            template,
            org,
            rust,
        } => cmd_new(&name, &template, &org, rust),

        Commands::Init { template, org } => cmd_init(&template, &org),

        Commands::Check { source } => cmd_check(&source),

        Commands::Info => cmd_info(),

        Commands::Doctor => cmd_doctor(),
    }
}

fn cmd_build(source: &str, target: &str, release: bool, output: Option<&str>) -> Result<()> {
    let path = PathBuf::from(source);
    let config = BlincConfig::load_from_dir(&path)?;

    info!(
        "Building {} for {} ({})",
        config.project.name,
        target,
        if release { "release" } else { "debug" }
    );

    // Validate target
    let valid_targets = [
        "desktop", "android", "ios", "macos", "windows", "linux", "wasm",
    ];
    if !valid_targets.contains(&target) {
        anyhow::bail!(
            "Invalid target '{}'. Valid targets: {:?}",
            target,
            valid_targets
        );
    }

    // TODO: When Zyntax Grammar2 is ready:
    // 1. Parse .blinc files
    // 2. Generate Rust code
    // 3. Compile with cargo

    warn!("Build not yet implemented - waiting for Zyntax Grammar2");

    if let Some(out) = output {
        info!("Output will be written to: {}", out);
    }

    Ok(())
}

fn cmd_dev(
    source: &str,
    target: &str,
    port: u16,
    device: Option<&str>,
    mode: DevMode,
) -> Result<()> {
    let path = PathBuf::from(source);
    let config = BlincConfig::load_from_dir(&path)?;

    info!(
        "Starting dev server for {} on port {} targeting {}",
        config.project.name, port, target
    );

    if let Some(dev) = device {
        info!("Running on device: {}", dev);
    }

    match mode {
        DevMode::Rust => cmd_dev_rust(source, target, port, device),
        DevMode::Dsl => cmd_dev_dsl(source, target, port, device),
    }
}

fn cmd_dev_rust(_source: &str, _target: &str, _port: u16, _device: Option<&str>) -> Result<()> {
    info!(
        "Rust hot-reload mode. Native driver is on the roadmap; \
         install `dioxus-cli` and run `dx serve --hotpatch` to drive \
         patches over the subsecond protocol meanwhile."
    );
    // Existing stub body — file watcher + cargo + subsecond websocket
    // is the level-2 milestone (issue #30).
    warn!(
        "blinc dev --mode rust is not yet implemented — see docs/book/src/advanced/hot-reload.md"
    );
    Ok(())
}

fn cmd_dev_dsl(_source: &str, _target: &str, _port: u16, _device: Option<&str>) -> Result<()> {
    info!("Blinc DSL hot-reload mode (Zyntax Runtime2 JIT).");
    warn!("Blinc DSL toolchain is still in plan — Zyntax Grammar2 + Runtime2 JIT not yet ready");
    Ok(())
}

fn cmd_run(source: &str) -> Result<()> {
    info!("Running {}", source);

    // TODO: Execute compiled binary or interpret source
    warn!("Run not yet implemented - waiting for Zyntax Runtime2");

    Ok(())
}

fn cmd_plugin_build(path: &str, mode: &str) -> Result<()> {
    info!("Building plugin at {} (mode: {})", path, mode);

    let valid_modes = ["dynamic", "static"];
    if !valid_modes.contains(&mode) {
        anyhow::bail!("Invalid mode '{}'. Valid modes: {:?}", mode, valid_modes);
    }

    // TODO: Build the plugin crate with appropriate flags
    warn!("Plugin build not yet implemented");

    Ok(())
}

fn cmd_plugin_new(name: &str) -> Result<()> {
    info!("Creating new plugin: {}", name);

    let path = PathBuf::from(name);
    if path.exists() {
        anyhow::bail!("Directory '{}' already exists", name);
    }

    fs::create_dir_all(&path)?;
    project::create_plugin_project(&path, name)?;

    info!("Plugin created at {}/", name);
    Ok(())
}

fn cmd_new(name: &str, template: &str, org: &str, rust: bool) -> Result<()> {
    let path = PathBuf::from(name);

    // Extract the actual project name from the path (last component)
    let project_name = path.file_name().and_then(|n| n.to_str()).unwrap_or(name);

    if rust {
        info!("Creating new Rust project: {}", project_name);
    } else {
        info!(
            "Creating new project: {} (template: {})",
            project_name, template
        );
    }
    info!("Organization prefix: {}", org);

    if path.exists() {
        anyhow::bail!("Directory '{}' already exists", name);
    }

    fs::create_dir_all(&path)?;

    if rust {
        project::create_rust_project(&path, project_name, org)?;
        info!("Rust project created at {}/", name);
        info!("To get started:");
        info!("  cd {}", name);
        info!("  cargo run --features desktop");
    } else {
        project::create_project(&path, name, template, org)?;
        info!("Project created at {}/", name);
        info!("To get started:");
        info!("  cd {}", name);
        info!("  blinc dev");
    }

    Ok(())
}

fn cmd_init(template: &str, org: &str) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let name = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("blinc_app");

    info!(
        "Initializing Blinc project in current directory (template: {})",
        template
    );
    info!("Organization prefix: {}", org);

    // Check if already initialized
    if cwd.join(".blincproj").exists() {
        anyhow::bail!("This directory already contains a .blincproj");
    }
    if cwd.join("blinc.toml").exists() {
        anyhow::bail!("This directory already contains a blinc.toml (legacy format)");
    }

    project::create_project(&cwd, name, template, org)?;

    info!("Project initialized!");
    info!("Run `blinc dev` to start development");

    Ok(())
}

fn cmd_check(source: &str) -> Result<()> {
    let path = PathBuf::from(source);
    let config = BlincConfig::load_from_dir(&path)?;

    info!("Checking project: {}", config.project.name);

    // TODO: Parse and validate all .blinc files
    warn!("Check not yet implemented - waiting for Zyntax Grammar2");

    Ok(())
}

fn cmd_info() -> Result<()> {
    println!("Blinc UI Framework");
    println!("==================");
    println!();
    let git_hash = option_env!("BLINC_GIT_HASH").unwrap_or("unknown");
    println!("Version: {} ({})", env!("CARGO_PKG_VERSION"), git_hash);
    println!();
    println!("Supported targets:");
    println!("  - desktop (native window)");
    println!("  - macos");
    println!("  - windows");
    println!("  - linux");
    println!("  - android");
    println!("  - ios");
    println!("  - wasm (WebGPU/WebGL2)");
    println!();
    println!("Build modes:");
    println!("  - JIT (development, hot-reload) - requires Zyntax Runtime2");
    println!("  - AOT (production) - requires Zyntax Grammar2");
    println!();
    println!("Status:");
    println!("  - Core reactive system: Ready");
    println!("  - FSM runtime: Ready");
    println!("  - Animation system: Ready");
    println!("  - Zyntax integration: Pending Grammar2/Runtime2");

    Ok(())
}

fn cmd_doctor() -> Result<()> {
    let categories = doctor::run_doctor();
    doctor::print_doctor_results(&categories);

    // Return error if there are critical issues
    let has_errors = categories
        .iter()
        .any(|c| c.status() == doctor::CheckStatus::Error);

    if has_errors {
        std::process::exit(1);
    }

    Ok(())
}
