use std::path::PathBuf;

use anyhow::Result;
use archon::config::{LaunchSettings, default_config_path};
use archon::crypto::CryptoStack;
use archon::ghostdns::GhostDns;
use archon::ghostdns::daemon::GhostDnsDaemon;
use archon::telemetry::ServiceTelemetry;
use clap::{ArgAction, Parser};
use tracing::{info, warn};

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

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let launcher_config = resolve_launcher_config(args.config.as_ref())?;
    let mut settings = LaunchSettings::load_or_default(&launcher_config)?;

    if let Err(err) = archon::telemetry::init_tracing("ghostdns", args.verbose, &settings.telemetry)
    {
        eprintln!("warning: failed to initialise ghostdns tracing: {err}");
    }

    let telemetry = ServiceTelemetry::new("ghostdns", &settings.telemetry);
    telemetry.record_startup();

    if let Some(path) = &args.ghostdns_config {
        settings.ghostdns.config_path = Some(path.clone());
    }

    let ghostdns = match GhostDns::from_settings(&settings.ghostdns) {
        Ok(instance) => instance,
        Err(err) => {
            telemetry.record_error(&err);
            return Err(err);
        }
    };
    let crypto = CryptoStack::from_settings(&settings.crypto);
    let resolvers = settings.crypto.resolvers.clone();

    if let Err(err) = ensure_default_config(&ghostdns, &resolvers) {
        telemetry.record_error(&err);
        return Err(err);
    }
    let config_path = args
        .ghostdns_config
        .as_ref()
        .cloned()
        .unwrap_or_else(|| ghostdns.config_path().clone());
    let runtime = match GhostDnsDaemon::load_config_file(&config_path) {
        Ok(runtime) => runtime,
        Err(err) => {
            telemetry.record_error(&err);
            return Err(err);
        }
    };

    let daemon = match GhostDnsDaemon::new(runtime, crypto) {
        Ok(daemon) => daemon,
        Err(err) => {
            telemetry.record_error(&err);
            return Err(err);
        }
    };
    info!(config = %config_path.display(), "Starting GhostDNS daemon");
    let result = daemon.run().await;
    match &result {
        Ok(_) => telemetry.record_shutdown(),
        Err(err) => telemetry.record_error(err),
    }
    result
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
