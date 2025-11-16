//! Opening DSL parsing and (forthcoming) execution engine.

mod parser;
mod replay;
mod runner;

pub use parser::{
    ArtifactsSpec, ComparisonOp, Edge, Expression, Literal, Node, NodeKind, Opening, Policy,
    PortPredicate, PortReference, Predicate, Retry, SchemaHintFragment, SchemaHints,
    SourceLocation, SuccessCondition,
};
pub use parser::{Error, parse_opening_str};
pub use replay::{ReplayMismatch, ReplayReport, replay};
pub use runner::{
    Executor, NodeAttemptRecord, NodeAttemptTrace, NodeExecution, NodeExecutionRequest, NodeInputs,
    NodeOutputs, NodeRecord, NodeState, NodeTrace, RunEvent, RunReport, RunTrace, Runner,
    RunnerError,
};

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::Value as JsonValue;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::Mutex;

    fn fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("examples")
            .join("openings")
            .join("compose_email.yaml")
    }

    #[test]
    fn compose_email_parses() {
        let yaml = std::fs::read_to_string(fixture_path()).expect("fixture");
        let opening = parse_opening_str(&yaml).expect("parse compose_email");
        assert_eq!(opening.name, "compose_email");
        assert_eq!(opening.nodes.len(), 5);
        assert_eq!(opening.edges.len(), 8);
        assert!(matches!(
            opening.success,
            Some(SuccessCondition::AnyOf(exprs)) if !exprs.is_empty()
        ));
    }

    struct MockExecutor {
        responses: Mutex<HashMap<String, NodeExecution>>,
    }

    impl MockExecutor {
        fn new(map: HashMap<String, NodeExecution>) -> Self {
            Self {
                responses: Mutex::new(map),
            }
        }
    }

    #[async_trait]
    impl Executor for MockExecutor {
        async fn execute(
            &self,
            request: NodeExecutionRequest<'_>,
        ) -> Result<NodeExecution, RunnerError> {
            let guard = self.responses.lock().expect("mock executor poisoned");
            guard
                .get(&request.node.id)
                .cloned()
                .ok_or_else(|| RunnerError::Executor(format!("no mock for {}", request.node.id)))
        }
    }

    #[tokio::test]
    async fn runner_executes_and_replays() {
        let yaml = r#"
version: 0
name: unit_test
nodes:
  - id: first
    use: agent:first
  - id: second
    use: agent:second
edges:
  - from: first.out
    to: second.in
success:
  all_of:
    - second.ok == true
"#;

        let opening = parse_opening_str(yaml).expect("parse opening");

        let mut responses = HashMap::new();
        let mut first_outputs = NodeOutputs::default();
        first_outputs.push("out", JsonValue::String("payload".into()));
        responses.insert("first".into(), NodeExecution::Completed(first_outputs));

        let mut second_outputs = NodeOutputs::default();
        second_outputs.push("ok", JsonValue::Bool(true));
        responses.insert(
            "second".into(),
            NodeExecution::Completed(second_outputs.clone()),
        );

        let executor = Arc::new(MockExecutor::new(responses));
        let runner = Runner::new(opening.clone(), executor.clone());
        let report = runner.run().await.expect("run opening");
        assert!(report.trace.success, "run should succeed");

        let replay_report = replay(&*executor, &opening, &report.trace)
            .await
            .expect("replay succeeded");
        assert!(
            replay_report.matches,
            "replay should match original outputs"
        );
        assert_eq!(replay_report.replay_hash, report.trace.final_hash);
    }

    #[test]
    fn agent_reference_supports_variant() {
        let yaml = r#"
version: 0
name: variants
nodes:
  - id: alpha
    use: agent:writer@nightly
  - id: beta
    use: agent:critic
edges: []
"#;
        let opening = parse_opening_str(yaml).expect("parse opening");
        let node = opening
            .nodes
            .iter()
            .find(|n| n.id == "alpha")
            .expect("alpha node");
        match &node.kind {
            NodeKind::Agent { reference } => {
                assert_eq!(reference.name, "writer");
                assert_eq!(reference.variant.as_deref(), Some("nightly"));
            }
            other => panic!("expected agent node, got {other:?}"),
        }
    }

    #[test]
    fn schema_hints_build_object_schema() {
        let yaml = r#"
version: 0
name: hints
nodes:
  - id: alpha
    use: agent:writer
    schema_hints:
      with:
        tone:
          enum: ["neutral"]
        topic:
          required: true
edges: []
"#;
        let opening = parse_opening_str(yaml).expect("parse opening");
        let node = opening
            .nodes
            .iter()
            .find(|n| n.id == "alpha")
            .expect("node");
        let schema = node.schema_hints.with_schema().expect("schema");
        let properties = schema
            .get("properties")
            .and_then(|value| value.as_object())
            .expect("properties map");
        assert!(properties.contains_key("tone"));
        let required = schema
            .get("required")
            .and_then(|value| value.as_array())
            .expect("required array");
        assert!(required.iter().any(|value| value.as_str() == Some("topic")));
    }

    #[tokio::test]
    async fn predicate_can_skip_downstream_node() {
        let yaml = r#"
version: 0
name: gating
nodes:
  - id: review
    use: agent:critic
  - id: send
    use: agent:mailer
edges:
  - from: review.ok==true
    to: send.in
"#;

        let opening = parse_opening_str(yaml).expect("parse opening");

        let mut responses = HashMap::new();
        let mut review_outputs = NodeOutputs::default();
        review_outputs.push("ok", JsonValue::Bool(false));
        responses.insert("review".into(), NodeExecution::Completed(review_outputs));
        responses.insert(
            "send".into(),
            NodeExecution::Completed(NodeOutputs::default()),
        );

        let executor = Arc::new(MockExecutor::new(responses));
        let runner = Runner::new(opening, executor);
        let report = runner.run().await.expect("run opening");

        let review_record = report
            .node_records
            .iter()
            .find(|record| record.node_id == "review")
            .expect("review record");
        assert!(matches!(review_record.state, NodeState::Succeeded));

        let send_record = report
            .node_records
            .iter()
            .find(|record| record.node_id == "send")
            .expect("send record");
        assert!(matches!(send_record.state, NodeState::Skipped));
    }

    #[test]
    fn templating_supports_numeric_and_boolean_params() {
        let yaml = r#"
version: 0
name: numeric_params
params:
  limit: 5
  enabled: true
nodes:
  - id: worker
    use: agent:worker
    with:
      limit: "{{params.limit}}"
      enabled: "{{params.enabled}}"
  - id: sink
    use: agent:sink
edges:
  - from: worker.out
    to: sink.input
"#;

        let opening = parse_opening_str(yaml).expect("parse opening");
        let worker = opening
            .nodes
            .iter()
            .find(|node| node.id == "worker")
            .expect("worker node");
        let limit = worker.with.get("limit").expect("limit param");
        assert_eq!(limit.as_u64(), Some(5));
        let enabled = worker.with.get("enabled").expect("enabled param");
        assert_eq!(enabled.as_bool(), Some(true));
    }

    #[test]
    fn goals_non_strings_raise_error() {
        let yaml = r#"
version: 0
name: bad_goals
goals:
  - 123
nodes:
  - id: lone
    use: agent:lone
edges: []
"#;

        let err = parse_opening_str(yaml).expect_err("non-string goal should fail");
        let message = format!("{err}");
        assert!(
            message.contains("goals[0]"),
            "error should mention offending goal index: {message}"
        );
    }

    #[tokio::test]
    async fn predicate_checks_all_values_on_port() {
        let yaml = r#"
version: 0
name: multi_value
nodes:
  - id: start
    use: agent:start
  - id: gate
    use: agent:gate
edges:
  - from: start.flag==true
    to: gate.input
success:
  all_of:
    - gate.ok == true
"#;

        let opening = parse_opening_str(yaml).expect("parse opening");

        let mut responses = HashMap::new();
        let mut start_outputs = NodeOutputs::default();
        start_outputs.push("flag", JsonValue::Bool(false));
        start_outputs.push("flag", JsonValue::Bool(true));
        responses.insert("start".into(), NodeExecution::Completed(start_outputs));
        let mut gate_outputs = NodeOutputs::default();
        gate_outputs.push("ok", JsonValue::Bool(true));
        responses.insert("gate".into(), NodeExecution::Completed(gate_outputs));

        let executor = Arc::new(MockExecutor::new(responses));
        let runner = Runner::new(opening, executor);
        let report = runner.run().await.expect("run opening");

        let gate_record = report
            .node_records
            .iter()
            .find(|record| record.node_id == "gate")
            .expect("gate record");
        assert!(matches!(gate_record.state, NodeState::Succeeded));
        assert!(
            report.trace.success,
            "run should succeed when a later port value matches predicate"
        );
    }

    #[tokio::test]
    async fn replay_handles_node_failure() {
        let yaml = r#"
version: 0
name: failing
nodes:
  - id: flake
    use: agent:flake
  - id: sink
    use: agent:sink
edges:
  - from: flake.out
    to: sink.in
"#;

        let opening = parse_opening_str(yaml).expect("parse opening");

        let mut responses = HashMap::new();
        responses.insert(
            "flake".into(),
            NodeExecution::Failed {
                retryable: false,
                reason: "boom".into(),
            },
        );
        let executor = Arc::new(MockExecutor::new(responses));
        let runner = Runner::new(opening.clone(), executor.clone());
        let report = runner.run().await.expect("run opening");
        assert!(!report.trace.success, "run should fail");

        let replay_executor = MockExecutor::new(HashMap::from([(
            "flake".to_string(),
            NodeExecution::Failed {
                retryable: false,
                reason: "boom".into(),
            },
        )]));
        let replay_report = replay(&replay_executor, &opening, &report.trace)
            .await
            .expect("replay succeeds");
        assert!(
            replay_report.matches,
            "replay should match recorded failure"
        );
    }

    #[tokio::test]
    async fn runner_emits_structured_events_in_order() {
        let yaml = r#"
version: 0
name: single_node
nodes:
  - id: first
    use: agent:first
  - id: sink
    use: agent:sink
edges:
  - from: first.out
    to: sink.in
success:
  all_of:
    - sink.ok == true
"#;

        let opening = parse_opening_str(yaml).expect("parse opening");
        let mut responses = HashMap::new();
        let mut first_outputs = NodeOutputs::default();
        first_outputs.push("out", JsonValue::String("ok".into()));
        responses.insert("first".to_string(), NodeExecution::Completed(first_outputs));
        let mut sink_outputs = NodeOutputs::default();
        sink_outputs.push("ok", JsonValue::Bool(true));
        responses.insert("sink".to_string(), NodeExecution::Completed(sink_outputs));

        let executor = Arc::new(MockExecutor::new(responses));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let runner = Runner::new(opening, executor).with_event_tx(tx);

        let report = runner.run().await.expect("run opening");
        let trace_id = report.trace.trace_id;
        drop(runner);

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        assert!(
            events.len() >= 6,
            "expected node lifecycle events before completion"
        );

        let first_slice = &events[0..5];
        assert!(matches!(
            first_slice[0],
            RunEvent::NodeState {
                state: NodeState::Running,
                attempt: 1,
                ref node_id
            } if node_id == "first"
        ));

        assert!(matches!(
            first_slice[1],
            RunEvent::LogLine {
                ref node_id,
                ref level,
                ..
            } if node_id == "first" && level == "info"
        ));

        assert!(matches!(
            first_slice[2],
            RunEvent::NodeState {
                state: NodeState::Succeeded,
                attempt: 1,
                ..
            }
        ));

        assert!(matches!(
            first_slice[3],
            RunEvent::LogLine {
                ref node_id,
                ref message,
                ..
            } if node_id == "first" && message.contains("succeeded")
        ));

        assert!(matches!(first_slice[4], RunEvent::TraceLine { .. }));
        match events.last() {
            Some(RunEvent::Completed { trace }) => {
                assert_eq!(trace.trace_id, trace_id, "completed trace id matches");
            }
            other => panic!("unexpected final event: {other:?}"),
        }
    }
}
