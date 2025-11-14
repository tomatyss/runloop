use runloop_core::{OpeningId, TraceId};
use runloop_openings::{NodeState, RunEvent, RunTrace};
use serde::Serialize;
use serde_json::Value as JsonValue;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{self, Write};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct RunEventEmitter {
    trace_id: String,
    run_id: String,
    opening_id: String,
    opening_name: String,
    params: Value,
    nodes: HashMap<String, NodeTelemetry>,
    run_started: Instant,
}

struct NodeTelemetry {
    attempt: u32,
    start_ts: Option<u64>,
    end_ts: Option<u64>,
    status: Option<String>,
    error: Option<String>,
}

impl NodeTelemetry {
    fn new() -> Self {
        Self {
            attempt: 0,
            start_ts: None,
            end_ts: None,
            status: None,
            error: None,
        }
    }

    fn duration_ms(&self) -> u64 {
        match (self.start_ts, self.end_ts) {
            (Some(start), Some(end)) if end >= start => end - start,
            _ => 0,
        }
    }
}

impl RunEventEmitter {
    pub fn new(
        trace_id: TraceId,
        opening_id: OpeningId,
        opening_name: String,
        params: Value,
    ) -> Self {
        Self {
            trace_id: trace_id.to_string(),
            run_id: trace_id.to_string(),
            opening_id: opening_id.to_string(),
            opening_name,
            params,
            nodes: HashMap::new(),
            run_started: Instant::now(),
        }
    }

    pub fn emit_run_started(&self) -> io::Result<()> {
        self.emit_record(
            "run.started",
            "info",
            "run started",
            json!({
                "params": self.params.clone(),
                "opening_id": self.opening_id.clone(),
                "opening_name": self.opening_name.clone(),
            }),
        )
    }

    pub fn handle_runner_event(&mut self, event: RunEvent) -> io::Result<()> {
        match event {
            RunEvent::NodeState {
                node_id,
                state,
                attempt,
            } => self.handle_node_state(node_id, state, attempt),
            RunEvent::LogLine {
                node_id,
                level,
                message,
            } => self.handle_log_line(node_id, level, message),
            RunEvent::TraceLine { line } => {
                self.emit_record("trace.line", "info", &line, json!({ "line": line }))
            }
            RunEvent::Completed { .. } => Ok(()),
        }
    }

    fn handle_node_state(
        &mut self,
        node_id: String,
        state: NodeState,
        attempt: u32,
    ) -> io::Result<()> {
        let telemetry = self
            .nodes
            .entry(node_id.clone())
            .or_insert_with(NodeTelemetry::new);
        match state {
            NodeState::Running => {
                telemetry.attempt = attempt;
                telemetry.start_ts = Some(timestamp_ms());
                telemetry.end_ts = None;
                telemetry.status = Some("running".into());
                telemetry.error = None;
                self.emit_record(
                    "node.started",
                    "info",
                    format!("node {node_id} started (attempt {attempt})"),
                    json!({ "node": node_id, "attempt": attempt }),
                )
            }
            NodeState::Succeeded => {
                telemetry.end_ts = Some(timestamp_ms());
                telemetry.status = Some("ok".into());
                Ok(())
            }
            NodeState::Failed { reason } => {
                telemetry.end_ts = Some(timestamp_ms());
                telemetry.status = Some("error".into());
                telemetry.error = Some(reason.clone());
                Ok(())
            }
            NodeState::Skipped => {
                telemetry.status = Some("skipped".into());
                telemetry.end_ts = Some(timestamp_ms());
                Ok(())
            }
            NodeState::Cancelled => {
                telemetry.status = Some("cancelled".into());
                telemetry.end_ts = Some(timestamp_ms());
                Ok(())
            }
            NodeState::Pending => Ok(()),
        }
    }

    fn handle_log_line(&self, node_id: String, level: String, message: String) -> io::Result<()> {
        let severity = level.to_ascii_lowercase();
        let kind = if severity == "error" || severity == "warn" {
            "node.stderr"
        } else {
            "node.stdout"
        };
        self.emit_record(
            kind,
            &severity,
            &message,
            json!({
                "node": node_id,
                "chunk": truncate_chunk(&message),
            }),
        )
    }

    pub fn emit_node_finishes(&mut self, trace: &RunTrace) -> io::Result<Vec<Value>> {
        let mut node_summaries = Vec::new();
        for node in &trace.nodes {
            let telemetry = self
                .nodes
                .entry(node.node_id.clone())
                .or_insert_with(NodeTelemetry::new);
            let status = telemetry
                .status
                .clone()
                .unwrap_or_else(|| status_for(&node.state));
            let outputs_hash = node
                .final_attempt
                .as_ref()
                .and_then(|attempt| attempt.output_hash.clone())
                .unwrap_or_default();
            let meta = json!({
                "node": node.node_id,
                "attempt": telemetry.attempt,
                "status": status,
                "duration_ms": telemetry.duration_ms(),
                "outputs_hash": outputs_hash,
                "error": telemetry.error,
            });
            self.emit_record(
                "node.finished",
                level_for_status(&status),
                format!("node {} {}", node.node_id, status),
                meta.clone(),
            )?;
            node_summaries.push(meta);
        }
        Ok(node_summaries)
    }

    pub fn emit_run_finished(&self, status: &str, node_summaries: Vec<Value>) -> io::Result<()> {
        self.emit_record(
            "run.finished",
            level_for_status(status),
            format!("run {status}"),
            json!({
                "status": status,
                "duration_ms": self.run_started.elapsed().as_millis() as u64,
                "nodes": node_summaries,
                "opening_id": self.opening_id.clone(),
                "opening_name": self.opening_name.clone(),
            }),
        )
    }

    fn emit_record<M>(&self, kind: &str, level: &str, message: M, meta: Value) -> io::Result<()>
    where
        M: Into<String>,
    {
        let record = NdjsonRecord {
            ts_ms: timestamp_ms(),
            trace_id: self.trace_id.as_str(),
            run_id: self.run_id.as_str(),
            opening_id: self.opening_id.as_str(),
            kind,
            level,
            message: message.into(),
            meta,
        };
        let line = serde_json::to_vec(&record).map_err(io::Error::other)?;
        let mut stdout = io::stdout();
        stdout.write_all(&line)?;
        stdout.write_all(b"\n")?;
        stdout.flush()
    }

    pub fn summarize_failure(&self) -> Vec<Value> {
        self.nodes
            .iter()
            .map(|(node_id, telemetry)| {
                json!({
                    "node": node_id,
                    "attempt": telemetry.attempt,
                    "status": telemetry.status.clone().unwrap_or_else(|| "error".into()),
                    "duration_ms": telemetry.duration_ms(),
                    "error": telemetry.error,
                })
            })
            .collect()
    }
}

#[derive(Serialize)]
struct NdjsonRecord<'a> {
    ts_ms: u64,
    trace_id: &'a str,
    run_id: &'a str,
    opening_id: &'a str,
    kind: &'a str,
    level: &'a str,
    message: String,
    meta: Value,
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn truncate_chunk(value: &str) -> String {
    const LIMIT: usize = 4096;
    if value.len() <= LIMIT {
        return value.to_string();
    }
    let mut end = LIMIT;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        return "…".into();
    }
    format!("{}…", &value[..end])
}

fn level_for_status(status: &str) -> &str {
    match status {
        "ok" | "finished" | "running" => "info",
        "skipped" | "cancelled" => "warn",
        _ => "error",
    }
}

fn status_for(state: &NodeState) -> String {
    match state {
        NodeState::Succeeded => "ok".into(),
        NodeState::Failed { .. } => "error".into(),
        NodeState::Skipped => "skipped".into(),
        NodeState::Cancelled => "cancelled".into(),
        NodeState::Pending => "pending".into(),
        NodeState::Running => "running".into(),
    }
}

/// Emit a single NDJSON line from a daemon-provided CT_RUN_EVENT payload.
/// Payload shape (MVP): { kind, level?, message, meta{ ts_ms, run_id, node_id?, ... } }
pub fn emit_ndjson_from_run_event_env(
    trace_id: TraceId,
    opening_id: &OpeningId,
    opening_name: &str,
    payload: &JsonValue,
) -> io::Result<()> {
    let line = render_ndjson_from_run_event_env(trace_id, opening_id, opening_name, payload)
        .map_err(io::Error::other)?;
    let mut stdout = io::stdout();
    stdout.write_all(&line.into_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

/// Build a JSON line (without trailing newline) for a daemon event payload.
pub fn render_ndjson_from_run_event_env(
    trace_id: TraceId,
    opening_id: &OpeningId,
    opening_name: &str,
    payload: &JsonValue,
) -> Result<String, serde_json::Error> {
    let kind = payload
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("event");
    let level = payload
        .get("level")
        .and_then(|v| v.as_str())
        .unwrap_or("info");
    let message = payload
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let meta = payload.get("meta").and_then(|v| v.as_object());
    let ts_ms = meta
        .and_then(|m| m.get("ts_ms"))
        .and_then(|v| v.as_u64())
        .unwrap_or_else(timestamp_ms);
    let run_id = meta
        .and_then(|m| m.get("run_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("trace:{}", trace_id));

    #[derive(Serialize)]
    struct Out<'a> {
        ts_ms: u64,
        trace_id: String,
        run_id: String,
        opening_id: String,
        kind: &'a str,
        level: &'a str,
        message: String,
        #[serde(flatten)]
        rest: std::collections::BTreeMap<&'a str, JsonValue>,
    }

    let mut rest = std::collections::BTreeMap::new();
    if let Some(m) = meta {
        rest.insert("meta", JsonValue::Object(m.clone()));
    }
    rest.insert("opening_name", JsonValue::String(opening_name.to_string()));

    let out = Out {
        ts_ms,
        trace_id: trace_id.to_string(),
        run_id,
        opening_id: opening_id.to_string(),
        kind,
        level,
        message,
        rest,
    };
    serde_json::to_string(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_chunk_passes_through() {
        let value = "hello";
        assert_eq!(truncate_chunk(value), value);
    }

    #[test]
    fn truncate_long_chunk_appends_ellipsis() {
        let value = "a".repeat(5000);
        let truncated = truncate_chunk(&value);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncated.chars().count(), 4097); // 4096 chars + ellipsis
    }

    #[test]
    fn render_ndjson_maps_daemon_payload() {
        let payload = serde_json::json!({
            "kind": "log",
            "level": "info",
            "message": "planning step 1",
            "meta": {
                "ts_ms": 1731510000000u64,
                "run_id": "trace:abc",
                "node_id": "planner",
                "span_id": "s1"
            }
        });
        let trace_id = TraceId::new();
        let opening_id = OpeningId::new();
        let line =
            render_ndjson_from_run_event_env(trace_id, &opening_id, "compose_email", &payload)
                .expect("json line");
        let value: serde_json::Value = serde_json::from_str(&line).expect("parse");
        assert_eq!(value.get("kind").unwrap(), "log");
        assert_eq!(value.get("level").unwrap(), "info");
        assert_eq!(value.get("message").unwrap(), "planning step 1");
        assert_eq!(
            value.get("trace_id").unwrap(),
            &serde_json::Value::String(trace_id.to_string())
        );
        assert_eq!(
            value.get("opening_id").unwrap(),
            &serde_json::Value::String(opening_id.to_string())
        );
        assert_eq!(value.get("run_id").unwrap(), "trace:abc");
        assert!(value.get("meta").is_some());
        assert_eq!(value.get("opening_name").unwrap(), "compose_email");
    }
}
