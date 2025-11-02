//! agtop observability utilities (placeholder)

pub fn init_tracing() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .map_err(|err| anyhow::anyhow!("failed to initialise tracing: {err}"))
}
