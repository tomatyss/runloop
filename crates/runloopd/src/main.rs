use runloop_core::Error;
use tokio::signal;
use tokio::time::{Duration, sleep};

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt::init();
    tracing::info!("runloopd starting (placeholder)");
    wait_for_shutdown().await;
    Ok(())
}

async fn wait_for_shutdown() {
    tracing::info!("press Ctrl+C to stop runloopd");
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("unable to install Ctrl+C handler");
    };
    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("shutdown signal received");
        }
        // placeholder: keep process alive even if Ctrl+C not supported (e.g., tests)
        _ = sleep(Duration::from_secs(u64::MAX)) => {}
    }
}
