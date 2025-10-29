use std::path::PathBuf;

use anyhow::Result;
use archon::Launcher;
use archon::config::{EngineKind, LaunchMode, LaunchRequest, default_config_path};
use clap::{ArgAction, Parser};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "archon", author = "GhostKellz", version, about = "Chromium Max launcher", long_about = None)]
struct Args {
    /// Profile name to launch or create.
    #[arg(long, default_value = "default")]
    profile: String,

    /// Launch mode (privacy or ai).
    #[arg(long, value_enum, default_value_t = LaunchMode::Privacy)]
    mode: LaunchMode,

    /// Execute the launch (default true). Pass --dry-run to inspect command only.
    #[arg(long, action = ArgAction::SetTrue)]
    dry_run: bool,

    /// Enable the experimental --enable-unsafe-webgpu flag for this launch.
    #[arg(long, action = ArgAction::SetTrue, alias = "enable-unsafe-webgpu")]
    unsafe_webgpu: bool,

    /// Increase logging verbosity.
    #[arg(long, action = ArgAction::SetTrue)]
    verbose: bool,

    /// Override the default launcher configuration path.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Print the resolved command instead of spawning Chromium.
    #[arg(long, action = ArgAction::SetTrue)]
    print_command: bool,
}

fn init_tracing(verbose: bool) {
    let default_level = if verbose {
        "archon=debug"
    } else {
        "archon=info"
    };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

fn resolve_config_path(override_path: Option<PathBuf>) -> Result<PathBuf> {
    match override_path {
        Some(path) => Ok(path),
        None => default_config_path(),
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_tracing(args.verbose);

    let config_path = resolve_config_path(args.config.clone())?;
    info!(path = %config_path.display(), "using launcher config");

    let mut launcher = Launcher::bootstrap(Some(config_path.clone()))?;

    let unsafe_webgpu_default = launcher.ui().settings().unsafe_webgpu_default;
    if unsafe_webgpu_default && !args.unsafe_webgpu {
        warn!("config enables unsafe WebGPU by default; pass --unsafe-webgpu to align explicitly");
    }

    let mut request = LaunchRequest::default();
    request.engine = Some(EngineKind::Edge);
    request.profile = args.profile.clone();
    request.mode = args.mode;
    request.execute = !args.dry_run;
    request.unsafe_webgpu = args.unsafe_webgpu || unsafe_webgpu_default;
    request.open_url = None;

    let outcome = launcher.run(request)?;

    info!(
        engine = %outcome.engine,
        profile = %outcome.profile.name,
        executed = outcome.executed(),
        "prepared Chromium Max launch"
    );

    if args.print_command || !outcome.executed() {
        let command = outcome.command.clone();
        let mut rendered = command.describe();
        if !command.env().is_empty() {
            let env_pairs: Vec<String> = command
                .env()
                .iter()
                .map(|(key, value)| format!("{}={}", key, value))
                .collect();
            rendered.push_str(&format!("\n  env: {}", env_pairs.join(" ")));
        }
        println!("{rendered}");
    }

    Ok(())
}
