use runloop_core::{Config, Error};
use runloop_kb::{KnowledgeBase, Materializer};
use tokio::signal;
use tokio::sync::oneshot;
use tokio::time::{Duration, sleep};

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt::init();
    tracing::info!("runloopd starting");

    let config = Config::load()?;
    let kb = KnowledgeBase::open(&config.kb).map_err(|err| Error::Kb(err.to_string()))?;
    kb.migrate()
        .map_err(|err| Error::Kb(format!("migration failed: {err}")))?;

    let materializer = Materializer::new(kb.clone());

    tokio::task::spawn_blocking({
        let materializer = materializer.clone();
        move || -> Result<(), runloop_kb::Error> {
            while materializer.sync()? {}
            Ok(())
        }
    })
    .await
    .map_err(|err| Error::Kb(format!("materializer startup join error: {err}")))?
    .map_err(|err| Error::Kb(format!("materializer catch-up failed: {err}")))?;

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let worker = tokio::spawn(run_materializer(materializer, shutdown_rx));

    wait_for_shutdown().await;
    tracing::info!("shutdown signal received; stopping services");
    let _ = shutdown_tx.send(());
    if let Err(err) = worker.await {
        tracing::warn!("materializer task ended unexpectedly: {err}");
    }

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
        _ = ctrl_c => {}
        _ = sleep(Duration::from_secs(u64::MAX)) => {}
    }
}

async fn run_materializer(materializer: Materializer, mut shutdown: oneshot::Receiver<()>) {
    tracing::info!("materializer loop started");
    let mut idle_backoff = Duration::from_millis(200);
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                tracing::info!("materializer loop stopping");
                break;
            }
            result = tokio::task::spawn_blocking({
                let materializer = materializer.clone();
                move || materializer.sync()
            }) => {
                match result {
                    Ok(Ok(true)) => {
                        idle_backoff = Duration::from_millis(50);
                    }
                    Ok(Ok(false)) => {
                        idle_backoff = (idle_backoff * 2).min(Duration::from_secs(5));
                        sleep(idle_backoff).await;
                    }
                    Ok(Err(err)) => {
                        tracing::error!("materializer sync failed: {err}");
                        sleep(Duration::from_secs(1)).await;
                    }
                    Err(join_err) => {
                        tracing::error!("materializer task panicked: {join_err}");
                        break;
                    }
                }
            }
        }
    }
}
