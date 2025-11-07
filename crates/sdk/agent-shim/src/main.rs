use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use runloop_core::content::CT_AGENT_HELLO;
use runloop_sdk::{AgentHello, ShimClient, ShimConfig};
use tokio::process::Command;
use tracing::{error, info};

const HELLO_TOPIC: &str = "rlp/runtime/hello";

#[derive(Parser, Debug)]
#[command(author, version, about = "Runloop native-agent bootstrap shim")]
struct Cli {
    /// Command to exec (the native agent binary followed by its arguments).
    #[arg(value_name = "COMMAND", num_args = 1.., trailing_var_arg = true)]
    command: Vec<String>,

    /// Optional working directory for the agent process.
    #[arg(long, value_name = "DIR")]
    workdir: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    if let Err(err) = async_main().await {
        error!("agent-shim failed: {err:#}");
        eprintln!("agent-shim error: {err:#}");
        std::process::exit(1);
    }
}

async fn async_main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    if cli.command.is_empty() {
        anyhow::bail!(
            "missing agent command (pass e.g. `agent-shim ./contact-resolver -- --flag`)"
        );
    }
    let config = ShimConfig::from_env()?;
    let shim = ShimClient::connect(config).await?;
    publish_hello(&shim).await?;
    info!(agent = %shim.agent_id(), "shim connected to bus");

    let exit_code = launch_agent(&cli).await?;
    std::process::exit(exit_code);
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .compact()
        .try_init();
}

async fn publish_hello(shim: &ShimClient) -> Result<()> {
    let hello: AgentHello = shim.hello();
    shim.publish(HELLO_TOPIC, CT_AGENT_HELLO, &hello).await?;
    Ok(())
}

async fn launch_agent(cli: &Cli) -> Result<i32> {
    let mut cmd = Command::new(&cli.command[0]);
    if cli.command.len() > 1 {
        cmd.args(&cli.command[1..]);
    }
    if let Some(dir) = &cli.workdir {
        cmd.current_dir(dir);
    }
    let status = cmd.status().await.context("failed to run agent process")?;
    Ok(status.code().unwrap_or(1))
}
