use async_trait::async_trait;
use clap::{Args, Parser, Subcommand};
use runloop_agents_common::{
    ActionDecision, ActionProposal, AgentError, AgentResult, ConfirmationProvider,
};
use runloop_core::{Config, TraceId};
use runloop_executor_local::{
    ExecutorInitError, LocalExecutor, build_executor as build_local_executor, catch_up_views,
};
use runloop_kb::{KnowledgeBase, Materializer};
use runloop_model_broker::SecretResolver;
use runloop_openings::{
    NodeState, ReplayMismatch, RunReport, RunTrace, Runner, RunnerError, parse_opening_str, replay,
};
use runloop_router::{Classification, Router};
use serde_json::{Value as JsonValue, to_string_pretty, to_writer_pretty};
use serde_yaml::{self, Mapping as YamlMapping, Value as YamlValue};
use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::{fs, fs::File};
use thiserror::Error;
use tokio::task;

#[derive(Parser, Debug)]
#[command(name = "rlp", about = "Runloop CLI (work in progress)")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Why(WhyArgs),
    Run(RunArgs),
    Replay(ReplayArgs),
    #[command(subcommand)]
    Kb(KbCommands),
}

#[derive(Args, Debug)]
struct WhyArgs {
    /// Prompt to explain classification for.
    prompt: String,
    /// Emit structured JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Path to the opening YAML file to execute.
    #[arg(value_name = "OPENING_PATH")]
    path: String,
    /// Optional JSON object providing parameter overrides.
    #[arg(long, value_name = "JSON")]
    params: Option<String>,
    /// Optional path to write a serialized run trace for replay.
    #[arg(long, value_name = "TRACE_PATH")]
    trace_out: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct ReplayArgs {
    /// Path to a previously recorded run trace (JSON).
    #[arg(value_name = "TRACE_PATH")]
    trace_path: String,
    /// Opening YAML to use when replaying the trace.
    #[arg(long, value_name = "OPENING_PATH")]
    opening: String,
}

#[derive(Subcommand, Debug)]
enum KbCommands {
    Query(QueryArgs),
    Search(SearchArgs),
    Why(KbWhyArgs),
    Migrate,
}

#[derive(Args, Debug)]
struct QueryArgs {
    /// Query expression for the knowledge base.
    #[arg(value_name = "EXPR", trailing_var_arg = true)]
    expression: Vec<String>,
}

#[derive(Args, Debug)]
struct SearchArgs {
    /// Keyword to search for across contacts, artifacts, events, and runs.
    #[arg(value_name = "KEYWORD", trailing_var_arg = true)]
    keyword: Vec<String>,
}

#[derive(Args, Debug)]
struct KbWhyArgs {
    /// Entity key (e.g. contact:<hash>) to inspect provenance for.
    #[arg(value_name = "ENTITY")]
    entity: String,
}

#[derive(Debug, Error)]
enum CliError {
    #[error(transparent)]
    Core(#[from] runloop_core::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Kb(#[from] runloop_kb::Error),
    #[error(transparent)]
    Openings(#[from] runloop_openings::Error),
    #[error(transparent)]
    Runner(#[from] RunnerError),
    #[error(transparent)]
    ExecutorInit(#[from] ExecutorInitError),
    #[error("invalid parameters: {0}")]
    InvalidParams(String),
    #[error("invalid trace: {0}")]
    InvalidTrace(String),
    #[error("opening run failed (trace {0})")]
    RunFailure(TraceId),
    #[error("replay detected mismatches")]
    ReplayFailure(Vec<ReplayMismatch>),
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), CliError> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Why(args) => handle_why(args).await,
        Commands::Run(args) => handle_run(args).await,
        Commands::Replay(args) => handle_replay(args).await,
        Commands::Kb(cmd) => handle_kb(cmd).await,
    }
}

async fn handle_why(args: WhyArgs) -> Result<(), CliError> {
    let config = Config::load()?;
    let router = Router::from_config(&config.router);
    let classification = router.classify(&args.prompt);
    if args.json {
        let json = to_string_pretty(&classification)?;
        println!("{json}");
    } else {
        print_classification(&classification);
    }
    Ok(())
}

async fn handle_run(args: RunArgs) -> Result<(), CliError> {
    let source = fs::read_to_string(&args.path)?;
    let mut doc: YamlValue = serde_yaml::from_str(&source)
        .map_err(|err| CliError::InvalidTrace(format!("invalid YAML: {err}")))?;

    if let Some(params_json) = args.params.as_deref() {
        let parsed: JsonValue = serde_json::from_str(params_json)?;
        let obj = parsed
            .as_object()
            .ok_or_else(|| CliError::InvalidParams("params must be a JSON object".into()))?;

        let mapping = doc
            .as_mapping_mut()
            .ok_or_else(|| CliError::InvalidTrace("opening YAML must be a mapping".into()))?;

        let params_entry = mapping
            .entry(YamlValue::from("params"))
            .or_insert_with(|| YamlValue::Mapping(YamlMapping::new()));

        let params_mapping = params_entry
            .as_mapping_mut()
            .ok_or_else(|| CliError::InvalidParams("existing params must be a mapping".into()))?;

        for (key, value) in obj {
            let yaml_value = serde_yaml::to_value(value.clone())
                .map_err(|err| CliError::InvalidParams(format!("invalid param value: {err}")))?;
            params_mapping.insert(YamlValue::from(key.clone()), yaml_value);
        }
    }

    let opening_yaml =
        serde_yaml::to_string(&doc).map_err(|err| CliError::InvalidTrace(err.to_string()))?;
    let opening = parse_opening_str(&opening_yaml)?;

    let opening_name = opening.name.clone();
    let executor = build_executor()?;
    let runner = Runner::new(opening, executor);
    let report: RunReport = runner.run().await?;

    println!("opening: {}", opening_name);
    println!("trace id: {}", report.trace.trace_id);
    for record in &report.node_records {
        match &record.state {
            NodeState::Succeeded => println!("  {} -> succeeded", record.node_id),
            NodeState::Failed { reason } => {
                println!("  {} -> failed ({reason})", record.node_id);
            }
            NodeState::Skipped => println!("  {} -> skipped", record.node_id),
            NodeState::Cancelled => println!("  {} -> cancelled", record.node_id),
            other => println!("  {} -> {:?}", record.node_id, other),
        }
    }
    println!("success: {}", report.trace.success);
    println!("final hash: {}", report.trace.final_hash);

    if let Some(path) = args.trace_out {
        let file = File::create(&path)?;
        to_writer_pretty(file, &report.trace)?;
        println!("trace saved to {}", path.display());
    }

    if report.trace.success {
        Ok(())
    } else {
        Err(CliError::RunFailure(report.trace.trace_id))
    }
}

async fn handle_replay(args: ReplayArgs) -> Result<(), CliError> {
    let trace_data = fs::read_to_string(&args.trace_path)?;
    let trace: RunTrace =
        serde_json::from_str(&trace_data).map_err(|err| CliError::InvalidTrace(err.to_string()))?;

    let opening_source = fs::read_to_string(&args.opening)?;
    let opening = parse_opening_str(&opening_source)?;

    let executor = build_executor()?;
    let report = replay(executor.as_ref(), &opening, &trace).await?;

    println!("trace id: {}", trace.trace_id);
    println!("original success: {}", trace.success);
    println!("replay hash: {}", report.replay_hash);
    if report.matches {
        println!("replay matches recorded outputs");
        Ok(())
    } else {
        println!("replay mismatches detected ({}):", report.mismatches.len());
        for mismatch in &report.mismatches {
            let expected = mismatch.expected_hash.as_deref().unwrap_or("<none>");
            let actual = mismatch.actual_hash.as_deref().unwrap_or("<none>");
            println!(
                "  {} -> {} (expected {}, actual {})",
                mismatch.node_id, mismatch.reason, expected, actual
            );
        }
        Err(CliError::ReplayFailure(report.mismatches.clone()))
    }
}

async fn handle_kb(cmd: KbCommands) -> Result<(), CliError> {
    let config = Config::load()?;
    match cmd {
        KbCommands::Query(args) => {
            let expr = args.expression.join(" ");
            if expr.trim().is_empty() {
                println!("kb query requires a SQL expression");
                return Ok(());
            }
            let kb = KnowledgeBase::open(&config.kb)?;
            catch_up_views(&kb)?;
            let result = kb.query(&expr)?;
            let rendered = to_string_pretty(&result)?;
            println!("{rendered}");
        }
        KbCommands::Search(args) => {
            let keyword = args.keyword.join(" ");
            if keyword.trim().is_empty() {
                println!("kb search requires a keyword");
                return Ok(());
            }
            let kb = KnowledgeBase::open(&config.kb)?;
            catch_up_views(&kb)?;
            let results = kb.search(&keyword)?;
            let rendered = to_string_pretty(&results)?;
            println!("{rendered}");
        }
        KbCommands::Why(args) => {
            let kb = KnowledgeBase::open(&config.kb)?;
            catch_up_views(&kb)?;
            let events = kb.why(&args.entity)?;
            if events.is_empty() {
                println!("no events found for {}", args.entity);
            } else {
                let rendered = to_string_pretty(&events)?;
                println!("{rendered}");
            }
        }
        KbCommands::Migrate => {
            let kb = KnowledgeBase::open(&config.kb)?;
            kb.migrate()?;
            let materializer = Materializer::new(kb.clone());
            let mut batches = 0usize;
            while materializer.sync()? {
                batches += 1;
            }
            let watermark = materializer.current_watermark()?;
            println!(
                "knowledge base migrations applied; watermark={} ({} batch(es))",
                watermark, batches
            );
        }
    }
    Ok(())
}

fn print_classification(classification: &Classification) {
    println!("route: {}", classification.route);
    println!("rule: {}", classification.rule);
    if classification.features.is_empty() {
        println!("features: []");
    } else {
        println!("features: [{}]", classification.features.join(", "));
    }
    if classification.blocked {
        println!("blocked: true");
    }
    println!("reason: {}", classification.reason);
}

fn build_executor() -> Result<Arc<LocalExecutor>, CliError> {
    let config = Config::load()?;
    let confirmation = Arc::new(CliConfirmationProvider::new(
        config.security.confirm_external_actions,
    ));
    let secrets = Arc::new(CliSecretResolver);
    let executor = build_local_executor(config, confirmation, secrets)?;
    Ok(executor)
}

struct CliSecretResolver;

impl SecretResolver for CliSecretResolver {
    fn resolve(&self, secret_id: &str) -> Option<String> {
        env::var(secret_id).ok()
    }
}

struct CliConfirmationProvider {
    require: bool,
}

impl CliConfirmationProvider {
    fn new(require: bool) -> Self {
        Self { require }
    }
}

#[async_trait]
impl ConfirmationProvider for CliConfirmationProvider {
    async fn confirm(&self, proposal: ActionProposal) -> AgentResult<ActionDecision> {
        if !self.require {
            return Ok(ActionDecision::approved(Some(
                "auto-approved (confirm_external_actions=false)".into(),
            )));
        }
        let summary = format!(
            "Send to {} ({} recipient(s)) for opening {}",
            proposal.recipients.join(", "),
            proposal.recipients.len(),
            proposal.opening_id
        );
        let prompt = format!("{summary}? [y/N]: ");
        let decision = task::spawn_blocking(move || -> AgentResult<bool> {
            print!("{prompt}");
            io::stdout().flush().map_err(AgentError::Io)?;
            let mut line = String::new();
            io::stdin().read_line(&mut line).map_err(AgentError::Io)?;
            let choice = line.trim().to_ascii_lowercase();
            Ok(choice == "y" || choice == "yes")
        })
        .await
        .map_err(|err| AgentError::Other(err.to_string()))??;
        if decision {
            Ok(ActionDecision::approved(Some("approved via CLI".into())))
        } else {
            Ok(ActionDecision::rejected(Some("declined via CLI".into())))
        }
    }
}
