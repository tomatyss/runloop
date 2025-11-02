use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use runloop_bus::Bus;
use runloop_core::config::ConfigLoader;
use runloop_model_broker::{ModelBroker, StubProvider};
use tracing::info;

#[derive(Parser, Debug)]
#[command(author, version, about = "Runloop CLI", long_about = None)]
struct Cli {
    /// Override config path
    #[arg(short, long)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Validate the configuration file and print derived paths
    CheckConfig,
    /// Show metrics for a bus socket (creates if missing)
    BusMetrics { path: PathBuf },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let loader = ConfigLoader::default();
    let outcome = match cli.config {
        Some(ref path) => loader.load(Some(path))?,
        None => loader.load(None::<&PathBuf>)?,
    };
    outcome.log_warnings();

    match cli.command.unwrap_or(Commands::CheckConfig) {
        Commands::CheckConfig => {
            let config = outcome.config;
            info!("config_version" = config.version, "kb_root" = %config.kb.root_dir.display(), "events_db" = %config.kb.events_db_path().display(), "view_db" = %config.kb.view_db_path().display(), "secrets_provider" = ?config.security.secrets.provider, "logging_format" = ?config.logging.format, "default_opening" = %config.router.default_opening);
            println!(
                "Runloop config v{}\n  KB root: {}\n  Events DB: {}\n  Views DB: {}\n  Secrets provider: {:?}\n  Logging format: {:?}",
                config.version,
                config.kb.root_dir.display(),
                config.kb.events_db_path().display(),
                config.kb.view_db_path().display(),
                config.security.secrets.provider,
                config.logging.format
            );
        }
        Commands::BusMetrics { path } => {
            let bus = Bus::bind(&path)?;
            let metrics = bus.metrics();
            println!(
                "Bus metrics for {}\n  TTL drops: {}\n  Duplicate drops: {}",
                path.display(),
                metrics.ttl_dropped,
                metrics.duplicates
            );
        }
    }

    // Ensure stub broker reachable (sanity smoke test)
    let broker = ModelBroker::new(StubProvider::default());
    let _ = broker.complete(runloop_model_broker::ModelRequest {
        prompt: "ping".into(),
        model: "null".into(),
        stream: false,
        max_tokens: Some(16),
        temperature: None,
    })?;

    Ok(())
}
