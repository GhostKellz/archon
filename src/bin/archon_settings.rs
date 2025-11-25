use std::{path::PathBuf, process};

use anyhow::Result;
use archon::{
    Launcher,
    config::{
        CryptoResolverSettings, EngineKind, LaunchMode, LaunchSettings, TelemetrySettings,
        default_config_path,
    },
};
use clap::Parser;
use dialoguer::{Confirm, Input, Select, theme::ColorfulTheme};

type SettingsResult<T> = Result<T>;

#[derive(Parser, Debug)]
#[command(
    name = "archon-settings",
    about = "Interactive Archon launcher settings editor",
    version
)]
struct SettingsCli {
    /// Optional configuration file override.
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Print current settings in JSON and exit.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    print: bool,
}
fn main() {
    if let Err(err) = run() {
        eprintln!("archon-settings: {err:?}");
        process::exit(1);
    }
}

fn run() -> SettingsResult<()> {
    let args = SettingsCli::parse();
    let config_path = resolve_config_path(args.config.as_ref())?;
    let mut settings = LaunchSettings::load_or_default(&config_path)?;

    if args.print {
        println!("{}", serde_json::to_string_pretty(&settings)?);
        return Ok(());
    }

    let theme = ColorfulTheme::default();

    edit_default_engine(&theme, &mut settings);
    edit_default_mode(&theme, &mut settings);
    edit_ghostdns_enabled(&theme, &mut settings);
    edit_ipfs_gateway(&theme, &mut settings.crypto.resolvers);
    edit_telemetry(&theme, &mut settings.telemetry);

    println!("\nReview:");
    println!("  default_engine  : {:?}", settings.default_engine);
    println!("  default_mode    : {:?}", settings.default_mode);
    println!("  ghostdns.enabled: {}", settings.ghostdns.enabled);
    println!(
        "  crypto.ipfs_gateway: {}",
        settings
            .crypto
            .resolvers
            .ipfs_gateway
            .as_deref()
            .unwrap_or("(derived from GhostDNS)")
    );
    println!("  telemetry.enabled: {}", settings.telemetry.enabled);
    println!(
        "  telemetry.traces.enabled: {}",
        settings.telemetry.traces.enabled
    );
    let trace_dir = settings
        .telemetry
        .traces
        .directory
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "(default)".into());
    println!("  telemetry.traces.dir: {}", trace_dir);
    println!(
        "  telemetry.traces.max_files: {}",
        settings.telemetry.traces.max_files
    );

    if !Confirm::with_theme(&theme)
        .with_prompt("Save changes?")
        .default(true)
        .interact()?
    {
        println!("Changes discarded.");
        return Ok(());
    }

    // Validate using existing launcher logic before writing to disk.
    Launcher::from_settings(settings.clone())?;
    settings.save(&config_path)?;

    println!("Settings saved to {}", config_path.display());

    Ok(())
}

fn resolve_config_path(explicit: Option<&PathBuf>) -> SettingsResult<PathBuf> {
    match explicit {
        Some(path) => Ok(path.clone()),
        None => default_config_path(),
    }
}

fn edit_default_engine(theme: &ColorfulTheme, settings: &mut LaunchSettings) {
    let engines = [EngineKind::Lite, EngineKind::Edge];
    let default_index = engines
        .iter()
        .position(|engine| engine == &settings.default_engine)
        .unwrap_or(0);
    if let Ok(selection) = Select::with_theme(theme)
        .with_prompt("Default engine")
        .items(&["archon-lite", "archon-edge"])
        .default(default_index)
        .interact()
    {
        settings.default_engine = engines[selection];
    }
}

fn edit_default_mode(theme: &ColorfulTheme, settings: &mut LaunchSettings) {
    let modes = [LaunchMode::Privacy, LaunchMode::Ai];
    let default_index = modes
        .iter()
        .position(|mode| mode == &settings.default_mode)
        .unwrap_or(0);
    if let Ok(selection) = Select::with_theme(theme)
        .with_prompt("Default launch mode")
        .items(&["privacy", "ai"])
        .default(default_index)
        .interact()
    {
        settings.default_mode = modes[selection];
    }
}

fn edit_ghostdns_enabled(theme: &ColorfulTheme, settings: &mut LaunchSettings) {
    if let Ok(enabled) = Confirm::with_theme(theme)
        .with_prompt("Enable GhostDNS by default?")
        .default(settings.ghostdns.enabled)
        .interact()
    {
        settings.ghostdns.enabled = enabled;
    }
}

fn edit_ipfs_gateway(theme: &ColorfulTheme, resolvers: &mut CryptoResolverSettings) {
    let current = resolvers.ipfs_gateway.clone().unwrap_or_default();
    let prompt = "Override IPFS gateway (leave blank to derive from GhostDNS)";
    match Input::<String>::with_theme(theme)
        .with_prompt(prompt)
        .allow_empty(true)
        .with_initial_text(current)
        .interact_text()
    {
        Ok(value) => {
            let trimmed = value.trim().to_string();
            if trimmed.is_empty() {
                resolvers.ipfs_gateway = None;
            } else {
                resolvers.ipfs_gateway = Some(trimmed);
            }
        }
        Err(_) => {}
    }
}

fn edit_telemetry(theme: &ColorfulTheme, telemetry: &mut TelemetrySettings) {
    if let Ok(enabled) = Confirm::with_theme(theme)
        .with_prompt("Enable Archon telemetry (crash-only signals)?")
        .default(telemetry.enabled)
        .interact()
    {
        telemetry.enabled = enabled;
    }

    if let Ok(enabled) = Confirm::with_theme(theme)
        .with_prompt("Enable JSON trace capture for Archon services?")
        .default(telemetry.traces.enabled)
        .interact()
    {
        telemetry.traces.enabled = enabled;
    }

    if telemetry.traces.enabled {
        let current_dir = telemetry
            .traces
            .directory
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        if let Ok(value) = Input::<String>::with_theme(theme)
            .with_prompt("Trace output directory (leave blank for default)")
            .allow_empty(true)
            .with_initial_text(current_dir)
            .interact_text()
        {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                telemetry.traces.directory = None;
            } else {
                telemetry.traces.directory = Some(PathBuf::from(trimmed));
            }
        }

        let default_retention = if telemetry.traces.max_files == 0 {
            10
        } else {
            telemetry.traces.max_files
        };
        if let Ok(value) = Input::<usize>::with_theme(theme)
            .with_prompt("Maximum trace files to retain (0 = unlimited)")
            .default(default_retention)
            .interact_text()
        {
            telemetry.traces.max_files = value;
        }
    }
}
