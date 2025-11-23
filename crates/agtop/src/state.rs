use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use runloop_bus::Bus;
use runloop_core::content::CT_ACTION_DECISION;
use runloop_rmp::Header;
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

pub enum Event {
    Run(Value),
    Metrics(Value, Option<String>), // Payload, Source/AgentID
    ActionRequest(Header, Value),
    BusConnected(Bus),
    Error(String),
}

// New event for sending actions back to collector
pub enum ActionCommand {
    PublishDecision(Header, Value, String), // Header, Payload, Topic
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Log,
    Plan,
    Metrics,
    Trace,
}

impl Pane {
    fn next(&self) -> Self {
        match self {
            Pane::Log => Pane::Plan,
            Pane::Plan => Pane::Metrics,
            Pane::Metrics => Pane::Trace,
            Pane::Trace => Pane::Log,
        }
    }

    fn prev(&self) -> Self {
        match self {
            Pane::Log => Pane::Trace,
            Pane::Plan => Pane::Log,
            Pane::Metrics => Pane::Plan,
            Pane::Trace => Pane::Metrics,
        }
    }
}

pub struct App {
    pub should_quit: bool,
    pub active_pane: Pane,
    pub show_help: bool,
    pub trace_id: String,
    pub bus: Option<Bus>,
    pub state: AppState,
    pub confirm_modal: Option<ConfirmModal>,
    pub filter_input: Option<String>,
    pub paused: bool,
    pub buffered_events: VecDeque<Event>,
    pub action_tx: Option<tokio::sync::mpsc::UnboundedSender<ActionCommand>>,
}

#[derive(Default)]
pub struct AppState {
    pub logs: Vec<LogEntry>,
    pub plan: BTreeMap<String, NodeStatus>,
    pub metrics: BTreeMap<String, MetricGroup>, // Key is source (system or agent)
    pub trace: Vec<TraceEntry>,
    pub last_updated: Option<u64>,
}

#[derive(Clone)]
pub struct MetricGroup {
    pub values: BTreeMap<String, String>,
}

#[derive(Clone)]
pub struct LogEntry {
    pub ts: u64,
    pub level: String,
    pub msg: String,
    pub node: Option<String>,
}

#[derive(Clone)]
pub struct NodeStatus {
    pub id: String,
    pub status: String,
    pub attempt: u32,
    pub duration: u64,
    pub error_count: u32,
    // Add DAG fields (even if inferred/empty for now)
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

#[derive(Clone)]
pub struct TraceEntry {
    pub ts: u64,
    pub msg: String,
    pub kind: String,
}

pub struct ConfirmModal {
    pub header: Header,
    pub proposal: Value,
}

impl App {
    pub fn new(
        trace_id: String,
        action_tx: Option<tokio::sync::mpsc::UnboundedSender<ActionCommand>>,
    ) -> Self {
        Self {
            should_quit: false,
            active_pane: Pane::Plan,
            show_help: false,
            trace_id,
            bus: None,
            state: AppState::default(),
            confirm_modal: None,
            filter_input: None,
            paused: false,
            buffered_events: VecDeque::new(),
            action_tx,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if let Some(input) = &mut self.filter_input {
            match key.code {
                KeyCode::Enter => self.filter_input = None,
                KeyCode::Esc => self.filter_input = None,
                KeyCode::Char(c) => input.push(c),
                KeyCode::Backspace => {
                    input.pop();
                }
                _ => {}
            }
            return;
        }

        if self.confirm_modal.is_some() {
            match key.code {
                KeyCode::Enter => self.approve_action(),
                KeyCode::Esc => self.reject_action(),
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.show_help = !self.show_help,
            KeyCode::Tab => self.active_pane = self.active_pane.next(),
            KeyCode::BackTab => self.active_pane = self.active_pane.prev(),
            KeyCode::Char('/') => self.filter_input = Some(String::new()),
            KeyCode::Char('.') => {
                self.paused = !self.paused;
                if !self.paused {
                    self.unpause();
                }
            }
            KeyCode::Char('!') => self.clear_pane(),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            _ => {}
        }
    }

    pub fn on_tick(&mut self) {}

    pub fn handle_event(&mut self, event: Event) {
        if self.paused {
            // Buffer event if paused, unless it is a key event (handled separately) or error/connection
            match event {
                Event::BusConnected(_) => self.process_event(event), // Always connect
                _ => self.buffered_events.push_back(event),
            }
            return;
        }
        self.process_event(event);
    }

    fn unpause(&mut self) {
        while let Some(event) = self.buffered_events.pop_front() {
            self.process_event(event);
        }
    }

    fn process_event(&mut self, event: Event) {
        match event {
            Event::Run(payload) => self.process_run_event(payload),
            Event::Metrics(payload, source) => self.process_metrics(payload, source),
            Event::ActionRequest(header, payload) => {
                self.confirm_modal = Some(ConfirmModal {
                    header,
                    proposal: payload,
                });
            }
            Event::BusConnected(bus) => self.bus = Some(bus),
            Event::Error(err) => {
                self.state.logs.push(LogEntry {
                    ts: current_millis(),
                    level: "ERROR".into(),
                    msg: format!("App Error: {err}"),
                    node: None,
                });
            }
        }
    }

    fn process_run_event(&mut self, payload: Value) {
        let kind = payload
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let meta = payload.get("meta").cloned().unwrap_or(json!({}));
        let ts = payload
            .get("ts_ms")
            .and_then(|v| v.as_u64())
            .or_else(|| meta.get("ts_ms").and_then(|v| v.as_u64()))
            .unwrap_or_else(current_millis);

        self.state.last_updated = Some(ts);
        // Normalize common meta fields
        let node = meta
            .get("node_id")
            .or_else(|| meta.get("node"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let level = payload
            .get("level")
            .and_then(|v| v.as_str())
            .unwrap_or("info");
        let message = payload
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match kind {
            // Logs emitted by nodes
            "log" | "node.stdout" | "node.stderr" => {
                self.state.logs.push(LogEntry {
                    ts,
                    level: level.to_string(),
                    msg: message.to_string(),
                    node,
                });
            }
            // Node lifecycle/status
            "plan" | "status" | "node.started" | "node.finished" | "run.finished" => {
                if let Some(node_id) = node {
                    let entry =
                        self.state
                            .plan
                            .entry(node_id.clone())
                            .or_insert_with(|| NodeStatus {
                                id: node_id.clone(),
                                status: "pending".into(),
                                attempt: 0,
                                duration: 0,
                                error_count: 0,
                                inputs: vec![],
                                outputs: vec![],
                            });
                    if let Some(s) = meta.get("state").and_then(|v| v.as_str()) {
                        entry.status = s.to_string();
                    } else {
                        entry.status = kind.to_string();
                    }
                    if let Some(a) = meta.get("attempt").and_then(|v| v.as_u64()) {
                        entry.attempt = a as u32;
                    }
                    if let Some(d) = meta.get("duration_ms").and_then(|v| v.as_u64()) {
                        entry.duration = d;
                    }
                    if message.contains("failed") {
                        entry.error_count += 1;
                    }
                    if let Some(inputs) = meta.get("inputs").and_then(|v| v.as_array()) {
                        entry.inputs = inputs
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect();
                    }
                    if let Some(outputs) = meta.get("outputs").and_then(|v| v.as_array()) {
                        entry.outputs = outputs
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect();
                    }
                } else {
                    // Fallback: treat as log if no node
                    self.state.logs.push(LogEntry {
                        ts,
                        level: level.to_string(),
                        msg: message.to_string(),
                        node: None,
                    });
                }
            }
            // Trace ladder
            "trace" | "trace.line" => {
                self.state.trace.push(TraceEntry {
                    ts,
                    msg: message.to_string(),
                    kind: kind.to_string(),
                });
            }
            // Unknown kinds -> log
            other => {
                self.state.logs.push(LogEntry {
                    ts,
                    level: level.to_string(),
                    msg: format!("{other}: {message}"),
                    node,
                });
            }
        }
    }

    fn process_metrics(&mut self, payload: Value, source: Option<String>) {
        let scope_key = source.unwrap_or_else(|| "system".to_string());
        let group = self
            .state
            .metrics
            .entry(scope_key.clone())
            .or_insert_with(|| MetricGroup {
                values: BTreeMap::new(),
            });

        if let Some(obj) = payload.as_object() {
            for (k, v) in obj {
                let val_str = if let Some(n) = v.as_f64() {
                    format!("{:.2}", n)
                } else if let Some(s) = v.as_str() {
                    s.to_string()
                } else {
                    v.to_string()
                };
                group.values.insert(k.clone(), val_str);
            }
        }
    }

    fn clear_pane(&mut self) {
        match self.active_pane {
            Pane::Log => self.state.logs.clear(),
            Pane::Plan => { /* Don't clear plan, it's stateful */ }
            Pane::Trace => self.state.trace.clear(),
            Pane::Metrics => self.state.metrics.clear(),
        }
    }

    fn approve_action(&mut self) {
        self.resolve_action(true);
    }

    fn reject_action(&mut self) {
        self.resolve_action(false);
    }

    fn resolve_action(&mut self, approved: bool) {
        if let Some(modal) = self.confirm_modal.take()
            && let Some(tx) = &self.action_tx
        {
            let decision = json!({
                "approved": approved,
                "rationale": if approved { Some("User approved via TUI") } else { Some("User rejected via TUI") },
                "proposal_id": modal.header.msg_id.to_string() // Best effort correlation
            });

            let mut header = Header::default();
            header.schema_id = CT_ACTION_DECISION;
            header.created_at_ms = current_millis();
            header.ttl_ms = 30_000;
            header.trace_id = modal.header.trace_id;
            header.msg_id = next_msg_id();

            let _ = tx.send(ActionCommand::PublishDecision(
                header,
                decision,
                "action.decision".to_string(),
            ));
        }
    }
}

fn current_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn next_msg_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_handle_event_run_event_log() {
        let mut app = App::new("test-trace".into(), None);
        let payload = json!({
            "kind": "log",
            "level": "INFO",
            "message": "test message",
            "meta": {
                "ts_ms": 12345,
                "node_id": "node-1"
            }
        });
        app.handle_event(Event::Run(payload));
        assert_eq!(app.state.logs.len(), 1);
        assert_eq!(app.state.logs[0].msg, "test message");
        assert_eq!(app.state.logs[0].level, "INFO");
        assert_eq!(app.state.logs[0].node.as_deref(), Some("node-1"));
    }

    #[test]
    fn test_handle_event_metrics() {
        let mut app = App::new("test-trace".into(), None);
        let payload = json!({
            "cpu": 10.5,
            "mem": "512MB"
        });
        app.handle_event(Event::Metrics(payload, Some("agent-1".into())));

        let group = app.state.metrics.get("agent-1").expect("agent-1 group");
        assert_eq!(group.values.get("cpu").map(|s| s.as_str()), Some("10.50"));
        assert_eq!(group.values.get("mem").map(|s| s.as_str()), Some("512MB"));
    }

    #[test]
    fn test_pause_buffering() {
        let mut app = App::new("test-trace".into(), None);
        app.paused = true;

        let payload = json!({
            "kind": "log",
            "level": "INFO",
            "message": "buffered",
            "meta": { "ts_ms": 1 }
        });
        app.handle_event(Event::Run(payload));

        assert!(app.state.logs.is_empty());
        assert_eq!(app.buffered_events.len(), 1);

        // Simulate unpause
        app.paused = false;
        app.unpause(); // unpause is private to App, so we need to be in module or use pub method
        // Since tests are in mod tests inside state.rs, they can access private items if `mod tests` is child.

        assert_eq!(app.state.logs.len(), 1);
        assert!(app.buffered_events.is_empty());
    }
}
