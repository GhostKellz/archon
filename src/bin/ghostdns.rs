use std::path::PathBuf;

use anyhow::Result;
use archon::config::{LaunchSettings, default_config_path};
use archon::crypto::CryptoStack;
use archon::ghostdns::GhostDns;
use archon::ghostdns::daemon::GhostDnsDaemon;
use clap::{ArgAction, Parser};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "ghostdns", author = "GhostKellz", version, about = "Archon GhostDNS daemon", long_about = None)]
struct Args {
    /// Override path to Archon launcher config.json
    #[arg(long)]
    config: Option<PathBuf>,

    /// Override path to GhostDNS runtime config (ghostdns.toml)
    #[arg(long, value_name = "PATH")]
    ghostdns_config: Option<PathBuf>,

    /// Increase logging verbosity
    #[arg(long, action = ArgAction::SetTrue)]
    verbose: bool,
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

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    init_tracing(args.verbose);

    let launcher_config = resolve_launcher_config(args.config.as_ref())?;
    let mut settings = LaunchSettings::load_or_default(&launcher_config)?;

    if let Some(path) = &args.ghostdns_config {
        settings.ghostdns.config_path = Some(path.clone());
    }

    let ghostdns = GhostDns::from_settings(&settings.ghostdns)?;
    let crypto = CryptoStack::from_settings(&settings.crypto);
    let resolvers = settings.crypto.resolvers.clone();

    ensure_default_config(&ghostdns, &resolvers)?;
    let config_path = args
        .ghostdns_config
        .as_ref()
        .cloned()
        .unwrap_or_else(|| ghostdns.config_path().clone());
    let runtime = GhostDnsDaemon::load_config_file(&config_path)?;

    let daemon = GhostDnsDaemon::new(runtime, crypto)?;
    info!(config = %config_path.display(), "Starting GhostDNS daemon");
    daemon.run().await
}

fn resolve_launcher_config(override_path: Option<&PathBuf>) -> Result<PathBuf> {
    match override_path {
        Some(path) => Ok(path.clone()),
        None => default_config_path(),
    }
}

fn ensure_default_config(
    ghostdns: &GhostDns,
    resolvers: &archon::config::CryptoResolverSettings,
) -> Result<()> {
    let path = ghostdns.config_path();
    if path.exists() {
        return Ok(());
    }
    warn!(
        "GhostDNS config missing at {}. Writing defaults.",
        path.display()
    );
    let outcome = ghostdns.write_default_config(resolvers, false)?;
    info!(?outcome, "Generated default GhostDNS configuration");
    Ok(())
}
