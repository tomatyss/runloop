use anyhow::Result;
use tokio::signal;
use tracing::info;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    info!("msg" = "runloopd starting", "profile" = %profile_mode());
    signal::ctrl_c().await?;
    info!("msg" = "runloopd shutting down");
    Ok(())
}

fn profile_mode() -> &'static str {
    if cfg!(target_os = "linux") {
        "system"
    } else {
        "user"
    }
}
