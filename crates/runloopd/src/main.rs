use std::path::{Path, PathBuf};

use runloop_bus::{Bus, BusServerHandle, PublisherKind};
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
    let bus_path = bus_socket_path(&config)?;
    let mut bus = start_bus(bus_path.as_path(), &config).await?;

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
    bus.close();

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

fn bus_socket_path(config: &Config) -> Result<PathBuf, Error> {
    if let Some(path) = config.runtime.socket_path.as_deref() {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err(Error::Config(
                "runtime.socket_path cannot be empty when specified".into(),
            ));
        }
        return Ok(PathBuf::from(trimmed));
    }
    let dir = config.runtime.sockets_dir.trim();
    if dir.is_empty() {
        return Err(Error::Config(
            "runtime.sockets_dir cannot be empty when runtime.socket_path is unset".into(),
        ));
    }
    Ok(PathBuf::from(dir).join("runloopd.sock"))
}

async fn start_bus(socket_path: &Path, config: &Config) -> Result<BusServerHandle, Error> {
    let handle = Bus::bind(socket_path).await.map_err(|err| {
        Error::Bus(format!(
            "failed to bind bus at {}: {err}",
            socket_path.display()
        ))
    })?;
    let allowed = action_decision_acl(&config.bus.auth.publishers.action_decision.allowed_kinds)?;
    handle.configure_action_decision_acl(allowed.clone());
    log_action_decision_acl(socket_path, &allowed);
    Ok(handle)
}

fn action_decision_acl(kinds: &[String]) -> Result<Vec<PublisherKind>, Error> {
    let mut allowed = Vec::new();
    for raw in kinds {
        let normalized = raw.trim();
        if normalized.is_empty() {
            return Err(Error::Config(
                "empty publisher kind entry in bus.auth.publishers.action_decision.allowed_kinds"
                    .into(),
            ));
        }
        let normalized = normalized.to_ascii_lowercase();
        let kind = match normalized.as_str() {
            "ui" => PublisherKind::Ui,
            "tui" => PublisherKind::Tui,
            "agent" => PublisherKind::Agent,
            other => {
                return Err(Error::Config(format!(
                    "unknown publisher kind '{other}' in bus.auth.publishers.action_decision.allowed_kinds"
                )));
            }
        };
        if !allowed.contains(&kind) {
            allowed.push(kind);
        }
    }
    Ok(allowed)
}

fn log_action_decision_acl(path: &Path, allowed: &[PublisherKind]) {
    if allowed.is_empty() {
        tracing::warn!(
            path = %path.display(),
            "bus listening; no publishers permitted to emit action.decision"
        );
        return;
    }
    let labels: Vec<&'static str> = allowed.iter().map(publisher_kind_label).collect();
    tracing::info!(path = %path.display(), allowed = %labels.join(","), "bus listening");
}

fn publisher_kind_label(kind: &PublisherKind) -> &'static str {
    match kind {
        PublisherKind::Ui => "ui",
        PublisherKind::Tui => "tui",
        PublisherKind::Agent => "agent",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_decision_acl_parses_known_kinds() {
        let kinds = vec!["tui".into(), "UI".into(), "agent".into(), "tui".into()];
        let acl = action_decision_acl(&kinds).expect("parsed kinds");
        assert_eq!(
            acl,
            vec![PublisherKind::Tui, PublisherKind::Ui, PublisherKind::Agent]
        );
    }

    #[test]
    fn action_decision_acl_rejects_blank_entries() {
        let kinds = vec![" ".into(), "".into()];
        let err = action_decision_acl(&kinds).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("empty publisher kind"));
    }

    #[test]
    fn action_decision_acl_rejects_unknown_values() {
        let kinds = vec!["foo".into()];
        let err = action_decision_acl(&kinds).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unknown publisher kind"));
    }
}
