use bytes::Bytes;
use runloop_bus::{Bus, BusStatsHandle, Message};
use runloop_core::Error;
use runloop_core::content::CT_METRICS_SNAPSHOT;
use runloop_core::ids::AgentId;
use runloop_executor_local::LocalExecutor;
use runloop_rmp::{Header, encode_payload};
use runloop_runtime::AgentMetricSample;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::time::{Duration, interval};
use tracing::warn;

use crate::utils::{current_millis, next_msg_id};

pub fn spawn_metrics_task(
    bus: Bus,
    bus_stats: BusStatsHandle,
    executor: Arc<LocalExecutor>,
    interval_ms: u64,
    mut shutdown: oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    let ttl_ms = std::cmp::max(interval_ms * 2, interval_ms + 250);
    let executor_id = std::env::var("HOSTNAME").unwrap_or_else(|_| "runloopd".into());
    let node_id = "host".to_string();
    tokio::spawn(async move {
        let runtime = executor.runtime();
        let broker = executor.broker();
        let hostcall_stats = runtime.hostcall_stats();
        let mut prev_agents: HashMap<AgentId, AgentMetricSample> = HashMap::new();
        let mut ticker = interval(Duration::from_millis(interval_ms));
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let ts_ms = current_millis();
                    let bus_snapshot = bus_stats.stats();
                    let broker_stats = broker.stats();
                    let agent_samples = runtime.agent_metrics();
                    let agents_running = agent_samples.len() as u64;
                    let rss_total: u64 = agent_samples
                        .iter()
                        .filter_map(|s| s.rss_bytes)
                        .sum();
                    let drops_total = bus_snapshot
                        .drops_ttl
                        .saturating_add(bus_snapshot.drops_duplicate)
                        .saturating_add(bus_snapshot.drops_backpressure);

                    let mut gauges = serde_json::Map::new();
                    gauges.insert("agents_running".into(), json!(agents_running));
                    gauges.insert(
                        "bus_queue_depth_max".into(),
                        json!(bus_snapshot.queue_depth_max),
                    );
                    gauges.insert(
                        "bus_queue_capacity_max".into(),
                        json!(bus_snapshot.queue_capacity_max),
                    );
                    gauges.insert("rss_total_bytes".into(), json!(rss_total));

                    let mut counters = serde_json::Map::new();
                    counters.insert(
                        "msgs_sent_total".into(),
                        json!(bus_snapshot.published),
                    );
                    counters.insert(
                        "msgs_recv_total".into(),
                        json!(bus_snapshot.delivered),
                    );
                    counters.insert("msgs_dropped_total".into(), json!(drops_total));
                    counters.insert(
                        "cap_denied_total".into(),
                        json!(hostcall_stats.denied()),
                    );
                    counters.insert(
                        "broker_calls_total".into(),
                        json!(broker_stats.calls),
                    );
                    counters.insert(
                        "cache_hits_total".into(),
                        json!(broker_stats.cache_hits),
                    );
                    counters.insert(
                        "tokens_prompt_total".into(),
                        json!(broker_stats.tokens_prompt),
                    );
                    counters.insert(
                        "tokens_completion_total".into(),
                        json!(broker_stats.tokens_completion),
                    );

                    let system_payload = json!({
                        "v": 1,
                        "scope": "system",
                        "ts_ms": ts_ms,
                        "interval_ms": interval_ms,
                        "labels": {
                            "executor_id": executor_id,
                            "node_id": node_id,
                        },
                        "gauges": gauges,
                        "counters": counters,
                    });

                    if let Err(err) = publish_metrics(&bus, "rlp/sys/metrics", system_payload, ttl_ms).await
                    {
                        warn!(?err, "failed to publish system metrics");
                    }

                    let current_ids: HashSet<AgentId> = agent_samples.iter().map(|s| s.agent_id).collect();

                    for sample in &agent_samples {
                        if let Err(err) = publish_agent_metrics(
                            &bus,
                            sample,
                            &executor_id,
                            &node_id,
                            ts_ms,
                            interval_ms,
                            ttl_ms,
                        )
                        .await
                        {
                            warn!(agent=?sample.agent_id, ?err, "failed to publish agent metrics");
                        }
                    }

                    for (agent_id, last) in prev_agents.iter() {
                        #[allow(clippy::collapsible_if)]
                        if !current_ids.contains(agent_id) {
                            if let Err(err) = publish_final_agent_metrics(
                                &bus,
                                last,
                                &executor_id,
                                &node_id,
                                ts_ms,
                                interval_ms,
                                ttl_ms,
                            )
                            .await
                            {
                                warn!(agent=?agent_id, ?err, "failed to publish final agent metrics");
                            }
                        }
                    }

                    prev_agents = agent_samples.into_iter().map(|s| (s.agent_id, s)).collect();
                }
                _ = &mut shutdown => {
                    tracing::info!("metrics task stopping");
                    break;
                }
            }
        }
    })
}

async fn publish_metrics(
    bus: &Bus,
    topic: &str,
    payload: serde_json::Value,
    ttl_ms: u64,
) -> Result<(), Error> {
    let body = encode_payload(CT_METRICS_SNAPSHOT, &payload, None)
        .map(Bytes::from)
        .map_err(|err| Error::Rmp(err.to_string()))?;
    let header = Header {
        schema_id: CT_METRICS_SNAPSHOT,
        created_at_ms: current_millis(),
        ttl_ms,
        msg_id: next_msg_id(),
        ..Header::default()
    };
    let message = Message::new(header, body).map_err(|e| Error::Bus(e.to_string()))?;
    bus.publish(topic, message)
        .await
        .map_err(|e| Error::Bus(e.to_string()))
}

async fn publish_agent_metrics(
    bus: &Bus,
    sample: &AgentMetricSample,
    executor_id: &str,
    node_id: &str,
    ts_ms: u64,
    interval_ms: u64,
    ttl_ms: u64,
) -> Result<(), Error> {
    let mut gauges = serde_json::Map::new();
    gauges.insert("mailbox_depth".into(), json!(sample.mailbox_depth));
    gauges.insert("mailbox_capacity".into(), json!(sample.mailbox_capacity));
    if let Some(rss) = sample.rss_bytes {
        gauges.insert("rss_bytes".into(), json!(rss));
    }
    if let Some(cpu) = sample.cpu_total_ms {
        gauges.insert("cpu_total_ms".into(), json!(cpu));
    }

    let mut counters = serde_json::Map::new();
    counters.insert("msgs_recv_total".into(), json!(sample.msgs_recv_total));
    counters.insert("msgs_sent_total".into(), json!(0));
    counters.insert("msgs_dropped_total".into(), json!(0));

    let payload = json!({
        "v": 1,
        "scope": "agent",
        "ts_ms": ts_ms,
        "interval_ms": interval_ms,
        "labels": {
            "executor_id": executor_id,
            "node_id": node_id,
            "agent_id": sample.agent_id.to_string(),
        },
        "gauges": gauges,
        "counters": counters,
    });

    let topic = format!("rlp/agents/{}/metrics", sample.agent_id);
    publish_metrics(bus, &topic, payload, ttl_ms).await
}

async fn publish_final_agent_metrics(
    bus: &Bus,
    last: &AgentMetricSample,
    executor_id: &str,
    node_id: &str,
    ts_ms: u64,
    interval_ms: u64,
    ttl_ms: u64,
) -> Result<(), Error> {
    let mut counters = serde_json::Map::new();
    counters.insert("msgs_recv_total".into(), json!(last.msgs_recv_total));
    counters.insert("msgs_sent_total".into(), json!(0));
    counters.insert("msgs_dropped_total".into(), json!(0));

    let payload = json!({
        "v": 1,
        "scope": "agent",
        "ts_ms": ts_ms,
        "interval_ms": interval_ms,
        "labels": {
            "executor_id": executor_id,
            "node_id": node_id,
            "agent_id": last.agent_id.to_string(),
        },
        "gauges": {
            "mailbox_depth": 0,
            "mailbox_capacity": last.mailbox_capacity,
        },
        "counters": counters,
    });

    let topic = format!("rlp/agents/{}/metrics", last.agent_id);
    publish_metrics(bus, &topic, payload, ttl_ms).await
}
