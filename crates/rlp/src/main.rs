mod agent;
mod output;
mod run_events;
mod shell;

use agent::{AgentCommands, handle_agent};
use async_trait::async_trait;
use bytes::Bytes;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use dirs::home_dir;
use futures_util::StreamExt;
use output::{
    Cell, OutputArgs, OutputMode, Table, display_value, print_json, print_table, summarize_json,
};
use run_events::{RunEventEmitter, emit_ndjson_from_run_event_env, node_records_to_meta};
use runloop_agent_registry::{
    AgentRegistry, AgentRegistryError, SchemaValidationError, digests_from, schema_property_names,
    validate_instance,
};
use runloop_agents_common::{
    ActionDecision, ActionProposal, AgentError, AgentResult, ConfirmationProvider,
};
use runloop_bus::{Bus, Subscription};
use runloop_core::config::{ConfigLayer, ConfigSource};
use runloop_core::content::{CT_CTRL_REQ, CT_CTRL_RESP, CT_RUN_EVENT};
use runloop_core::{AgentDigest, AgentRef, Config, DescribedAgent, TraceId};
use runloop_core::{ControlRequest, ControlResponse, DescribeAgentsRequest, RunSubmitRequest};
use runloop_executor_local::{
    ExecutorInitError, LocalExecutor, build_executor as build_local_executor, catch_up_views,
};
use runloop_kb::{
    EventRecord, KbBackupReport, KnowledgeBase, Materializer, TraceStore, VerifyReport,
};
use runloop_openings::{
    LadderHop, NodeKind, Opening, ReplayMismatch, RunReport, RunTrace, Runner, RunnerError,
    SchemaHintFragment, parse_opening_str, replay,
};
use runloop_rmp::{Header, decode_payload, encode_payload};
use runloop_router::{Classification as RouterClassification, Route as RouterRoute, Router};
use serde::Serialize;
use serde_json::{Value as JsonValue, json, to_string_pretty, to_value, to_writer_pretty};
use serde_yaml::{self, Mapping as YamlMapping, Value as YamlValue};
use shell::{
    DisableRequest, EnableRequest, ShellAction, ShellCliFlavor, ShellEditResult,
    disable as disable_shell, enable as enable_shell,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsStr;
use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant as StdInstant, SystemTime, UNIX_EPOCH};
use std::{env, fs, fs::File};
use thiserror::Error;
use tokio::time::Duration;
use tokio::{net::UnixStream, sync::mpsc, task, time};
use uuid::Uuid;

const CLI_AGENT_ID: &str = "agent:rlp-cli";
const ROUTE_PAYLOAD_VERSION: u32 = 1;
const ROUTE_EXIT_CODE_SHELL: i32 = 10;
const ROUTE_EXIT_CODE_AGENT: i32 = 11;
const ROUTE_EXIT_CODE_TIMEOUT: i32 = 12;
const ROUTE_DEFAULT_TIMEOUT_MS: u64 = 200;

#[derive(Parser, Debug)]
#[command(
    name = "rlp",
    about = "Runloop CLI (work in progress)",
    version,
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Route(RouteArgs),
    Why(WhyArgs),
    Run(RunArgs),
    Replay(ReplayArgs),
    Trace(TraceArgs),
    #[command(subcommand)]
    Kb(KbCommands),
    #[command(subcommand)]
    Config(ConfigCommands),
    #[command(subcommand)]
    Shell(ShellCommands),
    #[command(subcommand)]
    Agent(AgentCommands),
    /// Print version info and available agent commands.
    Version,
}

#[derive(Args, Debug)]
struct WhyArgs {
    /// Prompt to explain classification for.
    prompt: String,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Args, Debug)]
struct RouteArgs {
    /// Prompt to classify; use --stdin to read from standard input instead.
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,
    /// Read the prompt from STDIN (mutually exclusive with PROMPT).
    #[arg(long = "stdin")]
    read_stdin: bool,
    /// Timeout in milliseconds; defaults to RUNLOOP_ROUTER_TIMEOUT_MS or 200ms.
    #[arg(long = "timeout-ms", value_name = "MILLISECONDS")]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
struct RoutePayload {
    version: u32,
    route: RouterRoute,
    rule: String,
    blocked: bool,
}

impl From<&RouterClassification> for RoutePayload {
    fn from(classification: &RouterClassification) -> Self {
        Self {
            version: ROUTE_PAYLOAD_VERSION,
            route: classification.route,
            rule: classification.rule.clone(),
            blocked: classification.blocked,
        }
    }
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
    /// Execute directly via the local executor instead of the daemon.
    #[arg(long)]
    local: bool,
    /// Format for rendering parameter validation errors.
    #[arg(long = "errors-format", value_enum, default_value = "table")]
    errors_format: ErrorsFormat,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ErrorsFormat {
    Table,
    Json,
}

#[derive(Args, Debug)]
struct ReplayArgs {
    /// Trace identifier (`trace:<uuid>` or bare UUID) or path to a trace JSON file.
    #[arg(value_name = "TRACE_ID|TRACE_PATH")]
    trace_ref: String,
    /// Opening YAML to use when replaying the trace.
    #[arg(long, value_name = "OPENING_PATH")]
    opening: String,
}

#[derive(Args, Debug)]
struct TraceArgs {
    /// Trace identifier (`trace:<uuid>` or bare UUID) or path to a trace JSON file.
    #[arg(value_name = "TRACE_ID|TRACE_PATH")]
    trace_ref: String,
    /// Include control-plane frames (CT_CTRL_REQ/RESP) when available.
    #[arg(long)]
    include_control: bool,
    /// Include drop notices from rlp/sys/drops when available.
    #[arg(long)]
    include_drops: bool,
    /// Display timestamps relative to the first hop.
    #[arg(long)]
    relative: bool,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Subcommand, Debug)]
enum KbCommands {
    Query(QueryArgs),
    Search(SearchArgs),
    Why(KbWhyArgs),
    Migrate,
    Verify(KbVerifyArgs),
    Backup(KbBackupArgs),
    Vacuum(KbVacuumArgs),
}

#[derive(Args, Debug)]
struct QueryArgs {
    /// Query expression for the knowledge base.
    #[arg(value_name = "EXPR", trailing_var_arg = true)]
    expression: Vec<String>,
    #[command(flatten)]
    output: OutputArgs,
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
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Args, Debug)]
struct KbVerifyArgs {
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Args, Debug)]
struct KbBackupArgs {
    /// Output directory for the backup; defaults to <kb.root_dir>/backups/<unix_ts>.
    #[arg(long = "out-dir", value_name = "PATH")]
    out_dir: Option<PathBuf>,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Args, Debug, Default)]
struct KbVacuumArgs {
    /// Also run ANALYZE after VACUUM (always on for now; reserved for future flags).
    #[arg(long)]
    analyze: bool,
}

#[derive(Subcommand, Debug)]
enum ConfigCommands {
    Path(ConfigPathArgs),
}

#[derive(Args, Debug)]
struct ConfigPathArgs {
    /// Show the full provenance chain instead of just the active file.
    #[arg(long)]
    all: bool,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Subcommand, Debug)]
enum ShellCommands {
    Enable(ShellEnableArgs),
    Disable(ShellDisableArgs),
}

#[derive(Args, Debug)]
struct ShellEnableArgs {
    /// Shell flavor to configure.
    #[arg(long = "shell", value_enum, default_value_t = ShellCliFlavor::Zsh)]
    flavor: ShellCliFlavor,
    /// Override rc file path (defaults to ~/.zshrc or ~/.bashrc).
    #[arg(long = "rc-path", value_name = "PATH")]
    rc_path: Option<PathBuf>,
    /// Override snippet path (defaults to system/user share locations).
    #[arg(long = "snippet", value_name = "PATH")]
    snippet_path: Option<PathBuf>,
    /// Opening YAML path exported as RUNLOOP_ROUTER_OPENING_PATH.
    #[arg(long = "opening", value_name = "PATH")]
    opening_path: Option<PathBuf>,
    /// Preview the edits without touching files.
    #[arg(long = "dry-run")]
    dry_run: bool,
}

#[derive(Args, Debug)]
struct ShellDisableArgs {
    /// Shell flavor to remove integration from.
    #[arg(long = "shell", value_enum, default_value_t = ShellCliFlavor::Zsh)]
    flavor: ShellCliFlavor,
    /// Override rc file path (defaults to ~/.zshrc or ~/.bashrc).
    #[arg(long = "rc-path", value_name = "PATH")]
    rc_path: Option<PathBuf>,
    /// Preview the edits without touching files.
    #[arg(long = "dry-run")]
    dry_run: bool,
}

#[derive(Debug, Error)]
enum CliError {
    #[error(transparent)]
    Core(#[from] runloop_core::Error),
    #[error(transparent)]
    Rmp(#[from] runloop_rmp::Error),
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
    #[error("trace {0} not found in knowledge base")]
    TraceNotFound(TraceId),
    #[error("opening run failed (trace {0})")]
    RunFailure(TraceId),
    #[error("replay detected mismatches")]
    ReplayFailure(Vec<ReplayMismatch>),
    #[error("runloop daemon unavailable; tried {0}")]
    DaemonUnavailable(String),
    #[error("daemon at {0} is unreachable; start it or use --local")]
    DaemonUnreachable(String),
    #[error("daemon does not support agent metadata describe requests")]
    DaemonDescribeUnsupported,
    #[error("agent metadata error: {0}")]
    AgentMetadata(String),
    #[error("control handshake timed out waiting for acceptance")]
    AcceptTimeout,
    #[error("shell integration error: {0}")]
    ShellIntegration(String),
    #[error("agent command error: {0}")]
    AgentCommand(String),
    #[error("run rejected: {code} — {message}")]
    RunRejected { code: String, message: String },
    #[error("parameter validation failed")]
    ValidationFailed(ValidationFailure),
}

impl CliError {
    fn exit_code(&self) -> i32 {
        match self {
            CliError::ValidationFailed(_) => 2,
            _ => 1,
        }
    }
}

impl From<AgentRegistryError> for CliError {
    fn from(err: AgentRegistryError) -> Self {
        CliError::AgentMetadata(err.to_string())
    }
}

impl From<shell::ShellError> for CliError {
    fn from(err: shell::ShellError) -> Self {
        CliError::ShellIntegration(err.to_string())
    }
}

impl From<agent::AgentError> for CliError {
    fn from(err: agent::AgentError) -> Self {
        CliError::AgentCommand(err.to_string())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ReplaySource {
    File(PathBuf),
    TraceId(TraceId),
}

impl ReplaySource {
    fn parse(raw: &str) -> Result<Self, CliError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(CliError::InvalidTrace(
                "trace reference cannot be empty".to_string(),
            ));
        }
        if trimmed.starts_with("trace:") {
            return trimmed
                .parse::<TraceId>()
                .map(ReplaySource::TraceId)
                .map_err(|err| CliError::InvalidTrace(err.to_string()));
        }
        if Path::new(trimmed).exists() {
            return Ok(ReplaySource::File(trimmed.into()));
        }
        trimmed
            .parse::<TraceId>()
            .map(ReplaySource::TraceId)
            .map_err(|err| {
                CliError::InvalidTrace(format!(
                    "expected trace:<uuid>, bare uuid, or readable file (got '{raw}'): {err}"
                ))
            })
    }
}

#[tokio::main]
async fn main() {
    match run().await {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            match &err {
                CliError::ValidationFailed(failure) => {
                    if let Err(print_err) = print_validation_failure(failure) {
                        eprintln!("error: failed to render validation errors: {print_err}");
                    }
                }
                _ => eprintln!("error: {err}"),
            }
            std::process::exit(err.exit_code());
        }
    }
}

async fn run() -> Result<i32, CliError> {
    if invoked_with_version_flag(env::args_os()) {
        print_version();
        return Ok(0);
    }
    let cli = Cli::parse();
    match cli.command {
        Commands::Route(args) => handle_route(args).await,
        Commands::Why(args) => {
            handle_why(args).await?;
            Ok(0)
        }
        Commands::Run(args) => {
            handle_run(args).await?;
            Ok(0)
        }
        Commands::Replay(args) => {
            handle_replay(args).await?;
            Ok(0)
        }
        Commands::Trace(args) => {
            handle_trace(args).await?;
            Ok(0)
        }
        Commands::Kb(cmd) => {
            handle_kb(cmd).await?;
            Ok(0)
        }
        Commands::Config(cmd) => {
            handle_config(cmd).await?;
            Ok(0)
        }
        Commands::Shell(cmd) => {
            handle_shell(cmd).await?;
            Ok(0)
        }
        Commands::Agent(cmd) => {
            let config = Config::load()?;
            handle_agent(cmd, &config)?;
            Ok(0)
        }
        Commands::Version => {
            print_version();
            Ok(0)
        }
    }
}

fn print_version() {
    if let Err(err) = write_version(&mut io::stdout()) {
        eprintln!("failed to write version info: {err}");
    }
}

fn write_version(writer: &mut impl Write) -> io::Result<()> {
    writeln!(writer, "rlp {}", env!("CARGO_PKG_VERSION"))?;
    let agent_subs = agent_subcommand_names();
    if !agent_subs.is_empty() {
        writeln!(writer, "agent subcommands: {}", agent_subs.join(", "))?;
    }
    if let Ok(path) = env::current_exe() {
        writeln!(writer, "binary: {}", path.display())?;
    }
    Ok(())
}

fn agent_subcommand_names() -> Vec<String> {
    let mut names: Vec<String> = Cli::command()
        .get_subcommands()
        .find(|cmd| cmd.get_name() == "agent")
        .map(|agent_cmd| {
            agent_cmd
                .get_subcommands()
                .map(|cmd| cmd.get_name().to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    names.sort();
    names.dedup();
    names
}

fn invoked_with_version_flag<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    for arg in args.into_iter().skip(1) {
        let arg = arg.as_ref();
        if arg == OsStr::new("--") {
            break;
        }
        if arg == "--version" || arg == "-V" {
            return true;
        }
    }
    false
}

async fn handle_route(args: RouteArgs) -> Result<i32, CliError> {
    if args.read_stdin && args.prompt.is_some() {
        return Err(CliError::InvalidParams(
            "provide either PROMPT argument or --stdin, not both".into(),
        ));
    }

    let mut prompt = if args.read_stdin {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        buffer
    } else if let Some(prompt) = args.prompt {
        prompt
    } else {
        return Err(CliError::InvalidParams(
            "rlp route requires a prompt argument or --stdin".into(),
        ));
    };

    if args.read_stdin {
        while matches!(prompt.chars().last(), Some('\n' | '\r')) {
            prompt.pop();
        }
    }

    let timeout_ms = resolve_route_timeout(args.timeout_ms)?;
    let timeout = Duration::from_millis(timeout_ms);
    let prompt_for_task = prompt.clone();
    let classify = task::spawn_blocking(move || -> Result<RouterClassification, CliError> {
        let config = Config::load()?;
        let router = Router::from_config(&config.router);
        Ok(router.classify(&prompt_for_task))
    });

    let classification = match time::timeout(timeout, classify).await {
        Ok(join_result) => join_result
            .map_err(|err| CliError::ShellIntegration(format!("router task panicked: {err}")))??,
        Err(_) => {
            eprintln!("router timeout after {timeout_ms} ms");
            return Ok(ROUTE_EXIT_CODE_TIMEOUT);
        }
    };

    let payload = RoutePayload::from(&classification);
    let rendered = serde_json::to_string(&payload)?;
    println!("{rendered}");

    let exit_code = match classification.route {
        RouterRoute::Shell => ROUTE_EXIT_CODE_SHELL,
        RouterRoute::Agent => ROUTE_EXIT_CODE_AGENT,
    };
    Ok(exit_code)
}

fn resolve_route_timeout(cli_value: Option<u64>) -> Result<u64, CliError> {
    if let Some(ms) = cli_value {
        return Ok(ms);
    }
    if let Ok(env_val) = env::var("RUNLOOP_ROUTER_TIMEOUT_MS") {
        let parsed = env_val.parse::<u64>().map_err(|_| {
            CliError::InvalidParams(format!(
                "RUNLOOP_ROUTER_TIMEOUT_MS must be an integer, got '{env_val}'"
            ))
        })?;
        return Ok(parsed);
    }
    Ok(ROUTE_DEFAULT_TIMEOUT_MS)
}

#[cfg(test)]
mod route_tests {
    use super::*;
    use std::env;
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn with_env_var(key: &str, val: Option<&str>, f: impl FnOnce()) {
        let _guard = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env mutex poisoned");
        let prev = env::var_os(key);
        match val {
            Some(v) => unsafe { env::set_var(key, v) },
            None => unsafe { env::remove_var(key) },
        }
        f();
        match prev {
            Some(v) => unsafe { env::set_var(key, v) },
            None => unsafe { env::remove_var(key) },
        }
    }

    #[test]
    fn timeout_prefers_cli_over_env() {
        with_env_var("RUNLOOP_ROUTER_TIMEOUT_MS", Some("999"), || {
            let ms = resolve_route_timeout(Some(50)).unwrap();
            assert_eq!(ms, 50);
        });
    }

    #[test]
    fn timeout_reads_env_when_cli_missing() {
        with_env_var("RUNLOOP_ROUTER_TIMEOUT_MS", Some("150"), || {
            let ms = resolve_route_timeout(None).unwrap();
            assert_eq!(ms, 150);
        });
    }

    #[test]
    fn timeout_defaults_when_not_set() {
        with_env_var("RUNLOOP_ROUTER_TIMEOUT_MS", None, || {
            let ms = resolve_route_timeout(None).unwrap();
            assert_eq!(ms, ROUTE_DEFAULT_TIMEOUT_MS);
        });
    }

    #[test]
    fn timeout_invalid_env_errors() {
        with_env_var("RUNLOOP_ROUTER_TIMEOUT_MS", Some("abc"), || {
            let err = resolve_route_timeout(None).unwrap_err();
            let CliError::InvalidParams(_) = err else {
                panic!("expected InvalidParams");
            };
        });
    }
}

async fn handle_why(args: WhyArgs) -> Result<(), CliError> {
    let config = Config::load()?;
    let router = Router::from_config(&config.router);
    let classification = router.classify(&args.prompt);
    let settings = args.output.resolve();
    match settings.mode {
        OutputMode::Json => {
            let value = serde_json::to_value(&classification)?;
            print_json(&value)?;
        }
        OutputMode::Table => {
            let mut table = Table::new(vec![
                "route".into(),
                "rule".into(),
                "blocked".into(),
                "features".into(),
                "reason".into(),
            ]);
            let features = if classification.features.is_empty() {
                "—".into()
            } else {
                classification.features.join(", ")
            };
            table.add_row(vec![
                Cell::text(classification.route.to_string()),
                Cell::text(classification.rule.clone()),
                Cell::text(classification.blocked.to_string()),
                Cell::text(features),
                Cell::text(classification.reason.clone()),
            ]);
            print_table(&table, &settings)?;
        }
    }
    Ok(())
}

async fn handle_run(args: RunArgs) -> Result<(), CliError> {
    let config = Config::load()?;
    let prepared = prepare_opening(&args.path, args.params.as_deref())?;
    let agent_refs = prepared.opening.agent_refs();
    let registry = AgentRegistry::new(config.agents.search_dirs.clone());

    if args.local {
        let descriptors = load_descriptors_local(&registry, &agent_refs)?;
        validate_opening_params(&prepared.opening, &descriptors, args.errors_format)?;
        return run_opening_locally(&config, prepared, args.trace_out.clone()).await;
    }

    let mut client = DaemonClient::connect(&config).await?;
    let descriptors = match client.describe_agents(&agent_refs).await {
        Ok(desc) => desc,
        Err(CliError::DaemonDescribeUnsupported) => {
            eprintln!(
                "warning: daemon does not provide agent metadata; falling back to local manifests"
            );
            load_descriptors_local(&registry, &agent_refs)?
        }
        Err(err) => return Err(err),
    };
    validate_opening_params(&prepared.opening, &descriptors, args.errors_format)?;
    let digests = digests_from(&descriptors);
    let trace_out = args.trace_out.clone();
    let outcome = client.run(prepared, digests).await?;
    if let Some(path) = trace_out.as_deref()
        && let Err(err) = write_daemon_trace(&config, outcome.trace_id, path).await
    {
        match err {
            CliError::TraceNotFound(_) => {
                eprintln!(
                    "warning: daemon finished before the canonical trace was materialized; \
                     rerun with --trace-out if you need the replay file"
                );
            }
            other => return Err(other),
        }
    }
    if outcome.success {
        Ok(())
    } else {
        Err(CliError::RunFailure(outcome.trace_id))
    }
}

async fn handle_replay(args: ReplayArgs) -> Result<(), CliError> {
    let config = Config::load()?;
    let replay_source = ReplaySource::parse(&args.trace_ref)?;
    let trace = match replay_source {
        ReplaySource::File(path) => load_trace_from_file(&path)?,
        ReplaySource::TraceId(trace_id) => load_trace_from_kb(&config, trace_id)?,
    };

    let opening_source = fs::read_to_string(&args.opening)?;
    let opening = parse_opening_str(&opening_source)?;

    let executor = build_executor(config)?;
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

fn load_trace_from_file(path: &Path) -> Result<RunTrace, CliError> {
    let trace_data = fs::read_to_string(path)?;
    serde_json::from_str(&trace_data).map_err(|err| CliError::InvalidTrace(err.to_string()))
}

fn load_trace_from_kb(config: &Config, trace_id: TraceId) -> Result<RunTrace, CliError> {
    let kb = KnowledgeBase::open(&config.kb)?;
    catch_up_views(&kb)?;
    let value = kb
        .load_run_trace(&trace_id)?
        .ok_or(CliError::TraceNotFound(trace_id))?;
    serde_json::from_value(value).map_err(|err| CliError::InvalidTrace(err.to_string()))
}

async fn handle_trace(args: TraceArgs) -> Result<(), CliError> {
    let config = Config::load()?;
    let source = ReplaySource::parse(&args.trace_ref)?;
    let (mut trace, trace_id_opt) = match source {
        ReplaySource::File(path) => (load_trace_from_file(&path)?, None),
        ReplaySource::TraceId(trace_id) => (load_trace_from_kb(&config, trace_id)?, Some(trace_id)),
    };

    // Best-effort live enrichment (hybrid).
    if let Some(trace_id) = trace_id_opt
        && let Ok(live) =
            collect_live_ladder(&config, trace_id, args.include_control, args.include_drops).await
    {
        trace.ladder = merge_ladders(trace.ladder.clone(), live);
    }

    let ladder = sorted_ladder(&trace.ladder, args.relative);

    if ladder.is_empty() {
        println!("no ladder entries captured for this trace");
        return Ok(());
    }

    let settings = args.output.resolve();
    match (args.output.json, args.output.table) {
        (true, _) => {
            let mut trace_json = trace.clone();
            trace_json.ladder = ladder;
            let value = to_value(&trace_json)?;
            print_json(&value)?;
        }
        (_, true) => {
            let mut table = Table::new(vec![
                "ts_ms".into(),
                "from→to".into(),
                "kind".into(),
                "schema".into(),
                "frame_bytes".into(),
            ]);
            for hop in ladder {
                table.add_row(vec![
                    Cell::number(hop.ts_ms.to_string()),
                    Cell::text(format!("{} → {}", hop.from, hop.to)),
                    Cell::text(hop.kind.unwrap_or_else(|| "-".into())),
                    Cell::text(format!("{}", hop.schema_id)),
                    Cell::number(hop.frame_len.to_string()),
                ]);
            }
            print_table(&table, &settings)?;
        }
        _ => {
            render_ladder_text(&ladder);
        }
    }
    Ok(())
}

fn sorted_ladder(ladder: &[LadderHop], relative: bool) -> Vec<LadderHop> {
    let mut hops = ladder.to_vec();
    hops.sort_by(|a, b| {
        a.ts_ms
            .cmp(&b.ts_ms)
            .then_with(|| a.msg_id.cmp(&b.msg_id))
            .then_with(|| a.topic.cmp(&b.topic))
    });
    if relative && let Some(first) = hops.first().cloned() {
        let base = first.ts_ms;
        for hop in &mut hops {
            hop.ts_ms = hop.ts_ms.saturating_sub(base);
        }
    }
    hops
}

fn render_ladder_text(ladder: &[LadderHop]) {
    for hop in ladder {
        let kind = hop.kind.as_deref().unwrap_or("-");
        println!(
            "[{}] {} -> {} | {} (schema {}) | frame={}B body={}B",
            hop.ts_ms, hop.from, hop.to, kind, hop.schema_id, hop.frame_len, hop.body_len
        );
    }
}

fn merge_ladders(mut persisted: Vec<LadderHop>, live: Vec<LadderHop>) -> Vec<LadderHop> {
    let mut seen: HashSet<u64> = persisted.iter().map(|h| h.msg_id).collect();
    for hop in live {
        if hop.msg_id != 0 && seen.contains(&hop.msg_id) {
            continue;
        }
        seen.insert(hop.msg_id);
        persisted.push(hop);
    }
    persisted
}

async fn collect_live_ladder(
    config: &Config,
    trace_id: TraceId,
    include_control: bool,
    include_drops: bool,
) -> Result<Vec<LadderHop>, CliError> {
    let path = resolve_socket_path(config).await?;
    let bus = Bus::connect(&path)
        .await
        .map_err(|_| CliError::DaemonUnreachable(path.display().to_string()))?;

    let mut subs: Vec<(String, Subscription)> = Vec::new();
    let run_topic = format!("rlp/runs/{}/events", trace_id);
    subs.push((
        run_topic.clone(),
        bus.subscribe(&run_topic)
            .await
            .map_err(|_| CliError::DaemonUnreachable(path.display().to_string()))?,
    ));
    if include_control {
        subs.push((
            "rlp/ctrl".into(),
            bus.subscribe("rlp/ctrl")
                .await
                .map_err(|_| CliError::DaemonUnreachable(path.display().to_string()))?,
        ));
    }
    if include_drops {
        subs.push((
            "rlp/sys/drops".into(),
            bus.subscribe("rlp/sys/drops")
                .await
                .map_err(|_| CliError::DaemonUnreachable(path.display().to_string()))?,
        ));
    }

    let deadline = StdInstant::now() + Duration::from_millis(1500);
    let mut hops = Vec::new();
    while StdInstant::now() < deadline && !subs.is_empty() {
        let mut dead_indices = Vec::new();
        for (idx, (topic, sub)) in subs.iter_mut().enumerate() {
            match time::timeout(Duration::from_millis(50), sub.next()).await {
                Ok(Some(msg)) => {
                    let is_run = topic.starts_with("rlp/runs/");
                    let is_ctrl = include_control
                        && topic == "rlp/ctrl"
                        && msg.header.trace_id == uuid_to_u128(trace_id.0);
                    let is_drop = include_drops
                        && topic == "rlp/sys/drops"
                        && msg.header.trace_id == uuid_to_u128(trace_id.0);
                    if (is_run || is_ctrl || is_drop)
                        && let Some(hop) = ladder_from_message(topic, &msg)
                    {
                        hops.push(hop);
                    }
                }
                Ok(None) => dead_indices.push(idx),
                Err(_) => {}
            }
        }
        if !dead_indices.is_empty() {
            for idx in dead_indices.into_iter().rev() {
                subs.swap_remove(idx);
            }
        }
    }
    Ok(hops)
}

fn ladder_from_message(topic: &str, msg: &runloop_bus::Message) -> Option<LadderHop> {
    let kind = decode_payload::<serde_json::Value>(msg.header.schema_id, &msg.body)
        .ok()
        .and_then(|env| {
            env.payload
                .get("kind")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
    Some(LadderHop {
        ts_ms: msg.header.created_at_ms,
        topic: topic.to_string(),
        schema_id: msg.header.schema_id,
        frame_len: 64u32.saturating_add(msg.body.len() as u32),
        body_len: msg.body.len() as u32,
        from: "daemon".into(),
        to: topic.to_string(),
        msg_id: msg.header.msg_id,
        kind,
    })
}

async fn write_daemon_trace(
    config: &Config,
    trace_id: TraceId,
    trace_path: &Path,
) -> Result<(), CliError> {
    const MAX_ATTEMPTS: usize = 15;
    const BASE_DELAY_MS: u64 = 200;
    let kb = KnowledgeBase::open(&config.kb)?;
    for attempt in 0..MAX_ATTEMPTS {
        catch_up_views(&kb)?;
        if let Some(value) = kb.load_run_trace(&trace_id)? {
            let trace: RunTrace = serde_json::from_value(value)
                .map_err(|err| CliError::InvalidTrace(err.to_string()))?;
            let file = File::create(trace_path)?;
            to_writer_pretty(file, &trace)?;
            return Ok(());
        }
        if attempt + 1 == MAX_ATTEMPTS {
            break;
        }
        let delay = BASE_DELAY_MS.saturating_mul((attempt as u64) + 1);
        time::sleep(Duration::from_millis(delay)).await;
    }
    Err(CliError::TraceNotFound(trace_id))
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
            let settings = args.output.resolve();
            render_query_output(&result, &settings)?;
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
            let settings = args.output.resolve();
            render_why_output(&events, &settings, &args.entity)?;
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
        KbCommands::Verify(args) => {
            let kb = KnowledgeBase::open(&config.kb)?;
            let report = kb.verify()?;
            let settings = args.output.resolve();
            render_verify_report(&report, &settings)?;
        }
        KbCommands::Backup(args) => {
            let kb = KnowledgeBase::open(&config.kb)?;
            let out_dir = args
                .out_dir
                .unwrap_or_else(|| default_backup_dir(&config.kb));
            let report = kb.backup(&out_dir)?;
            let settings = args.output.resolve();
            render_backup_report(&report, &settings)?;
        }
        KbCommands::Vacuum(_args) => {
            let kb = KnowledgeBase::open(&config.kb)?;
            kb.vacuum()?;
            println!("knowledge base vacuum + analyze completed");
        }
    }
    Ok(())
}

async fn handle_config(cmd: ConfigCommands) -> Result<(), CliError> {
    match cmd {
        ConfigCommands::Path(args) => handle_config_path(args),
    }
}

fn handle_config_path(args: ConfigPathArgs) -> Result<(), CliError> {
    let (config, layers) = Config::load_with_layers()?;
    let settings = args.output.resolve();
    if matches!(settings.mode, OutputMode::Json) || args.all {
        return render_config_chain(&config, &layers, &settings);
    }
    if let Some(path) = active_config_path(&layers) {
        println!("{}", path.display());
    } else {
        println!("no config file found; using defaults + environment overrides");
    }
    Ok(())
}

async fn handle_shell(cmd: ShellCommands) -> Result<(), CliError> {
    match cmd {
        ShellCommands::Enable(args) => {
            let flavor = args.flavor.resolve();
            let request = EnableRequest {
                flavor,
                rc_path: args.rc_path,
                snippet_override: args.snippet_path,
                opening_path: args.opening_path,
                dry_run: args.dry_run,
            };
            let result = enable_shell(request)?;
            render_shell_result(&result);
        }
        ShellCommands::Disable(args) => {
            let flavor = args.flavor.resolve();
            let request = DisableRequest {
                flavor,
                rc_path: args.rc_path,
                dry_run: args.dry_run,
            };
            let result = disable_shell(request)?;
            render_shell_result(&result);
        }
    }
    Ok(())
}

fn render_shell_result(result: &ShellEditResult) {
    let status = match result.action {
        ShellAction::Added => "installed runloop shell integration",
        ShellAction::Updated => "updated runloop shell integration",
        ShellAction::Removed => "removed runloop shell integration",
        ShellAction::AlreadyPresent => "shell integration already present",
        ShellAction::NotFound => "shell integration block not found",
    };
    println!("{status} in {}", result.rc_path.display());
    if let Some(snippet) = &result.snippet_path {
        println!("snippet: {}", snippet.display());
    }
    if let Some(opening) = &result.opening_path {
        println!("opening: {}", opening.display());
    }
    if result.dry_run {
        println!("dry-run: no files modified");
    }
    if let Some(block) = &result.block_preview {
        println!("block preview:\n{block}");
    }
    for note in &result.notes {
        println!("- {note}");
    }
}

fn load_descriptors_local(
    registry: &AgentRegistry,
    refs: &[AgentRef],
) -> Result<Vec<DescribedAgent>, CliError> {
    if refs.is_empty() {
        return Ok(Vec::new());
    }
    registry.describe_many(refs.iter()).map_err(CliError::from)
}

fn validate_opening_params(
    opening: &Opening,
    descriptors: &[DescribedAgent],
    format: ErrorsFormat,
) -> Result<(), CliError> {
    let descriptor_map = descriptors_to_map(descriptors);
    let mut errors = Vec::new();
    let mut hint_logger = HintLogger::default();
    for node in &opening.nodes {
        if let NodeKind::Agent { reference } = &node.kind {
            let descriptor = descriptor_map.get(reference).ok_or_else(|| {
                CliError::AgentMetadata(format!(
                    "missing manifest metadata for agent '{}'",
                    reference
                ))
            })?;
            let params = JsonValue::Object(node.with.clone());
            let manifest_props = descriptor
                .schema
                .with
                .as_ref()
                .map(schema_property_names)
                .unwrap_or_default();
            if let Some(schema) = descriptor.schema.with.as_ref() {
                errors.extend(run_schema_validation(
                    node.id.as_str(),
                    reference,
                    descriptor,
                    schema,
                    &params,
                    ValidationSource::Manifest,
                )?);
            }
            if let Some(hint_schema) = node.schema_hints.with_schema() {
                if let Some(SchemaHintFragment::Properties(map)) = &node.schema_hints.with {
                    for field in map.keys() {
                        if manifest_props.is_empty() || !manifest_props.contains(field) {
                            hint_logger.log_missing(reference, field);
                        }
                    }
                }
                errors.extend(run_schema_validation(
                    node.id.as_str(),
                    reference,
                    descriptor,
                    &hint_schema,
                    &params,
                    ValidationSource::Hint,
                )?);
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(CliError::ValidationFailed(ValidationFailure {
            errors,
            format,
        }))
    }
}

fn descriptors_to_map(descriptors: &[DescribedAgent]) -> BTreeMap<AgentRef, DescribedAgent> {
    let mut map = BTreeMap::new();
    for descriptor in descriptors {
        map.insert(descriptor.reference.clone(), descriptor.clone());
    }
    map
}

fn run_schema_validation(
    node_id: &str,
    reference: &AgentRef,
    descriptor: &DescribedAgent,
    schema: &JsonValue,
    params: &JsonValue,
    source: ValidationSource,
) -> Result<Vec<ValidationErrorRow>, CliError> {
    match validate_instance(schema, params) {
        Ok(_) => Ok(Vec::new()),
        Err(SchemaValidationError::InvalidSchema(err)) => Err(CliError::AgentMetadata(format!(
            "invalid schema for {}: {err}",
            reference
        ))),
        Err(SchemaValidationError::Violations(violations)) => {
            let mut rows = Vec::new();
            for violation in violations {
                rows.push(ValidationErrorRow {
                    node_id: node_id.to_string(),
                    agent: reference.clone(),
                    agent_version: descriptor.version.clone(),
                    field: pointer_to_field(&violation.instance_path),
                    rule: violation.kind,
                    message: violation.message,
                    value_summary: summarize_json(&violation.value, 48),
                    source,
                });
            }
            Ok(rows)
        }
    }
}

fn pointer_to_field(pointer: &str) -> String {
    if pointer.is_empty() {
        "(root)".into()
    } else {
        pointer
            .trim_start_matches('/')
            .split('/')
            .map(|segment| segment.replace("~1", "/").replace("~0", "~"))
            .collect::<Vec<_>>()
            .join(".")
    }
}

fn print_validation_failure(failure: &ValidationFailure) -> io::Result<()> {
    let issues = failure.errors.len();
    let suffix = if issues == 1 { "" } else { "s" };
    let label = match failure.format {
        ErrorsFormat::Table => "table",
        ErrorsFormat::Json => "JSON",
    };
    eprintln!("Parameter validation failed; {issues} issue{suffix} (see {label} below)");
    match failure.format {
        ErrorsFormat::Table => render_validation_table(&failure.errors),
        ErrorsFormat::Json => render_validation_json(&failure.errors),
    }
}

fn render_validation_table(errors: &[ValidationErrorRow]) -> io::Result<()> {
    let mut table = Table::new(vec![
        "node".into(),
        "agent".into(),
        "version".into(),
        "field".into(),
        "rule".into(),
        "source".into(),
        "detail".into(),
    ]);
    for error in errors {
        let detail = format!("{} (value: {})", error.message, error.value_summary);
        table.add_row(vec![
            Cell::text(error.node_id.clone()),
            Cell::text(error.agent.to_string()),
            Cell::text(error.agent_version.clone()),
            Cell::text(error.field.clone()),
            Cell::text(error.rule.clone()),
            Cell::text(error.source.to_string()),
            Cell::text(detail),
        ]);
    }
    let settings = OutputArgs::default().resolve();
    print_table(&table, &settings)
}

fn render_validation_json(errors: &[ValidationErrorRow]) -> io::Result<()> {
    let rows: Vec<JsonValue> = errors
        .iter()
        .map(|error| {
            json!({
                "node": error.node_id,
                "agent": error.agent.to_string(),
                "version": error.agent_version,
                "field": error.field,
                "rule": error.rule,
                "source": error.source.to_string(),
                "message": error.message,
                "value": error.value_summary,
            })
        })
        .collect();
    print_json(&JsonValue::Array(rows))?;
    Ok(())
}

fn render_config_chain(
    config: &Config,
    layers: &[ConfigLayer],
    settings: &output::OutputSettings,
) -> Result<(), CliError> {
    if matches!(settings.mode, OutputMode::Json) {
        let json_value = build_config_json(config, layers)?;
        print_json(&json_value)?;
        return Ok(());
    }

    let mut table = Table::new(vec![
        "order".into(),
        "source".into(),
        "status".into(),
        "details".into(),
    ]);
    for (idx, layer) in layers.iter().enumerate() {
        let (source, status, details) = describe_layer(layer);
        table.add_row(vec![
            Cell::number((idx + 1).to_string()),
            Cell::text(source),
            Cell::text(status),
            Cell::text(details),
        ]);
    }
    for note in config_override_notes(layers) {
        table.add_note(note);
    }
    print_table(&table, settings)?;
    Ok(())
}

fn build_config_json(config: &Config, layers: &[ConfigLayer]) -> Result<JsonValue, CliError> {
    let sources = layers
        .iter()
        .map(|layer| match &layer.source {
            ConfigSource::Defaults => json!({
                "kind": "defaults",
                "overrides": layer.overrides.len(),
            }),
            ConfigSource::File { path, exists } => json!({
                "kind": "file",
                "path": path,
                "exists": exists,
                "overrides": layer.overrides.len(),
            }),
            ConfigSource::Env { keys } => json!({
                "kind": "env",
                "keys": keys,
                "overrides": layer.overrides.len(),
            }),
        })
        .collect::<Vec<_>>();
    let overrides = layers
        .iter()
        .flat_map(|layer| {
            let label = layer_label(layer);
            layer.overrides.iter().map(move |entry| {
                json!({
                    "source": label,
                    "key": entry.key,
                    "previous": entry.previous.clone().unwrap_or(JsonValue::Null),
                    "new_value": entry.new_value.clone(),
                })
            })
        })
        .collect::<Vec<_>>();
    let resolved = to_value(config)?;
    Ok(json!({
        "sources": sources,
        "overrides": overrides,
        "resolved": resolved,
    }))
}

fn describe_layer(layer: &ConfigLayer) -> (String, String, String) {
    match &layer.source {
        ConfigSource::Defaults => (
            "defaults".into(),
            "baseline".into(),
            format!("overrides: {}", layer.overrides.len()),
        ),
        ConfigSource::File { path, exists } => (
            path.display().to_string(),
            if *exists {
                "loaded".into()
            } else {
                "missing".into()
            },
            format!("overrides: {}", layer.overrides.len()),
        ),
        ConfigSource::Env { keys } => (
            "environment".into(),
            format!("{} key(s)", keys.len()),
            if keys.is_empty() {
                "RUNLOOP__*".into()
            } else {
                format!("keys: {}", keys.join(", "))
            },
        ),
    }
}

fn config_override_notes(layers: &[ConfigLayer]) -> Vec<String> {
    let mut notes = Vec::new();
    for layer in layers {
        let label = layer_label(layer);
        for entry in &layer.overrides {
            let previous = entry
                .previous
                .as_ref()
                .map(display_value)
                .unwrap_or_else(|| "<unset>".into());
            let new_value = display_value(&entry.new_value);
            notes.push(format!(
                "{0}: {1} -> {2} ({3})",
                entry.key, previous, new_value, label
            ));
        }
    }
    notes
}

fn layer_label(layer: &ConfigLayer) -> String {
    match &layer.source {
        ConfigSource::Defaults => "defaults".into(),
        ConfigSource::File { path, .. } => path.display().to_string(),
        ConfigSource::Env { .. } => "environment".into(),
    }
}

fn active_config_path(layers: &[ConfigLayer]) -> Option<PathBuf> {
    layers.iter().rev().find_map(|layer| match &layer.source {
        ConfigSource::File { path, exists } if *exists => Some(path.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use runloop_core::{AgentArtifacts, AgentPorts, AgentSchemaBundle, Config, EventId, OpeningId};
    use runloop_router::{Classification as RouterClassification, Route as RouterRoute};
    use serde_json::json;
    use tempfile::{NamedTempFile, tempdir};

    #[test]
    fn version_output_includes_agent_commands() {
        let mut buffer = Vec::new();
        write_version(&mut buffer).expect("write version output");
        let output = String::from_utf8(buffer).expect("utf8 output");

        assert!(
            output.contains(env!("CARGO_PKG_VERSION")),
            "version string missing package version"
        );
        for name in agent_subcommand_names() {
            assert!(
                output.contains(&name),
                "version output missing agent subcommand {name}"
            );
        }
    }

    #[test]
    fn detects_version_flags() {
        assert!(invoked_with_version_flag(["rlp", "--version"]));
        assert!(invoked_with_version_flag(["rlp", "-V"]));
        assert!(!invoked_with_version_flag(["rlp", "why"]));
        assert!(!invoked_with_version_flag([
            "rlp",
            "route",
            "--",
            "--version"
        ]));
    }

    fn event_with_payload(payload: JsonValue) -> EventRecord {
        EventRecord {
            id: EventId(1),
            ts_ms: 0,
            kind: "run.started".into(),
            actor: "agent:test".into(),
            scope: "user".into(),
            payload,
            provenance: json!({}),
            masked: false,
        }
    }

    #[test]
    fn summarize_event_prefers_status() {
        let record = event_with_payload(json!({"status":"finished","opening_id":"opening:abc"}));
        assert_eq!(summarize_event(&record), "status=finished");
    }

    #[test]
    fn summarize_event_falls_back_to_pairs() {
        let record = event_with_payload(json!({"foo":"bar","baz":42}));
        assert_eq!(summarize_event(&record), "foo=bar, baz=42");
    }

    #[test]
    fn route_payload_serializes_expected_fields() {
        let classification = RouterClassification {
            route: RouterRoute::Shell,
            rule: "simple".into(),
            features: vec!["cmd:ls".into()],
            reason: "matched".into(),
            blocked: true,
        };
        let payload = RoutePayload::from(&classification);
        let json_value = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            json_value,
            json!({
                "version": ROUTE_PAYLOAD_VERSION,
                "route": "shell",
                "rule": "simple",
                "blocked": true
            })
        );
    }

    #[test]
    fn pointer_to_field_formats_segments() {
        assert_eq!(pointer_to_field(""), "(root)");
        assert_eq!(pointer_to_field("/with/topic"), "with.topic");
        assert_eq!(
            pointer_to_field("/with/tone~1secondary"),
            "with.tone/secondary"
        );
    }

    #[test]
    fn replay_source_parses_prefixed_trace_id() {
        let trace_id = TraceId::new();
        let parsed = ReplaySource::parse(&trace_id.to_string()).expect("parse trace id");
        match parsed {
            ReplaySource::TraceId(id) => assert_eq!(id, trace_id),
            other => panic!("expected trace id, got {:?}", other),
        }
    }

    #[test]
    fn replay_source_prefers_existing_files() {
        let tmp = NamedTempFile::new().expect("temp file");
        let path_str = tmp.path().to_str().expect("path str");
        let parsed = ReplaySource::parse(path_str).expect("parse file");
        match parsed {
            ReplaySource::File(path) => assert_eq!(path, PathBuf::from(path_str)),
            other => panic!("expected file source, got {:?}", other),
        }
    }

    #[test]
    fn load_trace_from_kb_returns_persisted_trace() {
        let temp = tempdir().expect("temp dir");
        let mut config = Config::default();
        config.kb.root_dir = temp.path().to_string_lossy().into_owned();

        let trace_id = TraceId::new();
        let opening_id = OpeningId::new();
        let final_hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let run_trace = RunTrace {
            trace_id,
            opening_id,
            nodes: Vec::new(),
            ladder: Vec::new(),
            final_hash: final_hash.into(),
            success: true,
        };
        let trace_store =
            TraceStore::open(&config.kb, "agent:test", Some("system".into())).expect("trace store");
        trace_store
            .record_run_trace(&run_trace)
            .expect("record trace");
        trace_store.flush().expect("flush views");

        let loaded = load_trace_from_kb(&config, trace_id).expect("load trace");
        assert_eq!(loaded.trace_id, trace_id);
        assert_eq!(loaded.final_hash, run_trace.final_hash);
    }

    #[test]
    fn load_trace_from_kb_surfaces_not_found() {
        let temp = tempdir().expect("temp dir");
        let mut config = Config::default();
        config.kb.root_dir = temp.path().to_string_lossy().into_owned();

        // Ensure KB exists so load run trace operates on a real database.
        let trace_store =
            TraceStore::open(&config.kb, "agent:test", Some("system".into())).expect("trace store");
        trace_store.flush().expect("flush views");

        let missing = TraceId::new();
        let err = load_trace_from_kb(&config, missing).expect_err("expected error");
        assert!(matches!(err, CliError::TraceNotFound(id) if id == missing));
    }

    #[test]
    fn validate_opening_params_reports_manifest_errors() {
        let opening = parse_opening_str(
            r#"
version: 0
name: test
nodes:
  - id: draft
    use: agent:writer
    with: {}
  - id: sink
    use: agent:critic
edges:
  - from: draft.out
    to: sink.in
"#,
        )
        .expect("parse opening");

        let writer_descriptor = DescribedAgent {
            reference: AgentRef::new("writer", None),
            version: "0.1.0".into(),
            digest: "abc123".into(),
            schema: AgentSchemaBundle {
                with: Some(json!({
                    "type": "object",
                    "required": ["topic"],
                    "properties": {
                        "topic": { "type": "string" }
                    },
                    "additionalProperties": false
                })),
            },
            ports: AgentPorts::default(),
            artifacts: AgentArtifacts::default(),
        };
        let critic_descriptor = DescribedAgent {
            reference: AgentRef::new("critic", None),
            version: "0.1.0".into(),
            digest: "def456".into(),
            schema: AgentSchemaBundle::default(),
            ports: AgentPorts::default(),
            artifacts: AgentArtifacts::default(),
        };

        let err = validate_opening_params(
            &opening,
            &[writer_descriptor, critic_descriptor],
            ErrorsFormat::Table,
        )
        .unwrap_err();
        match err {
            CliError::ValidationFailed(failure) => {
                assert_eq!(failure.errors.len(), 1);
                assert_eq!(failure.errors[0].node_id, "draft");
                assert_eq!(failure.errors[0].field, "(root)");
                assert_eq!(failure.errors[0].source.to_string(), "manifest");
            }
            other => panic!("unexpected error {other:?}"),
        }
    }
}

fn render_query_output(
    result: &runloop_kb::QueryResult,
    settings: &output::OutputSettings,
) -> Result<(), CliError> {
    match settings.mode {
        OutputMode::Json => {
            let value = to_value(result)?;
            print_json(&value)?;
        }
        OutputMode::Table => {
            let mut table = Table::new(result.columns.clone());
            for row in &result.rows {
                let mut cells = Vec::new();
                for column in &result.columns {
                    let value = row
                        .as_object()
                        .and_then(|obj| obj.get(column))
                        .cloned()
                        .unwrap_or(JsonValue::Null);
                    if value.is_number() {
                        cells.push(Cell::number(display_value(&value)));
                    } else {
                        cells.push(Cell::text(display_value(&value)));
                    }
                }
                table.add_row(cells);
            }
            if result.rows.is_empty() {
                table.add_note("query returned 0 rows");
            }
            print_table(&table, settings)?;
        }
    }
    Ok(())
}

fn render_verify_report(
    report: &VerifyReport,
    settings: &output::OutputSettings,
) -> Result<(), CliError> {
    match settings.mode {
        OutputMode::Json => {
            let value = serde_json::to_value(report)?;
            print_json(&value)?;
        }
        OutputMode::Table => {
            let mut table = Table::new(vec!["metric".into(), "value".into()]);
            table.add_row(vec![
                Cell::text("events_checked"),
                Cell::text(report.events_checked.to_string()),
            ]);
            table.add_row(vec![
                Cell::text("hash_mismatches"),
                Cell::text(report.hash_mismatches.len().to_string()),
            ]);
            table.add_row(vec![
                Cell::text("non_canonical_fields"),
                Cell::text(report.non_canonical_fields.len().to_string()),
            ]);
            table.add_row(vec![
                Cell::text("parse_failures"),
                Cell::text(report.parse_failures.len().to_string()),
            ]);
            print_table(&table, settings)?;
            if !report.hash_mismatches.is_empty() {
                println!("hash mismatches:");
                for mismatch in &report.hash_mismatches {
                    println!(
                        "  event {}: stored={} computed={}",
                        mismatch.event_id, mismatch.stored, mismatch.computed
                    );
                }
            }
            if !report.non_canonical_fields.is_empty() {
                println!("non-canonical fields:");
                for issue in &report.non_canonical_fields {
                    println!("  event {} -> {}", issue.event_id, issue.field);
                }
            }
            if !report.parse_failures.is_empty() {
                println!("parse failures:");
                for failure in &report.parse_failures {
                    println!(
                        "  event {} {}: {}",
                        failure.event_id, failure.field, failure.error
                    );
                }
            }
        }
    }
    Ok(())
}

fn render_backup_report(
    report: &KbBackupReport,
    settings: &output::OutputSettings,
) -> Result<(), CliError> {
    match settings.mode {
        OutputMode::Json => {
            let value = serde_json::to_value(report)?;
            print_json(&value)?;
        }
        OutputMode::Table => {
            let mut table = Table::new(vec!["artifact".into(), "path".into()]);
            table.add_row(vec![
                Cell::text("events"),
                Cell::text(report.events_backup_path.display().to_string()),
            ]);
            table.add_row(vec![
                Cell::text("views"),
                Cell::text(report.views_backup_path.display().to_string()),
            ]);
            print_table(&table, settings)?;
        }
    }
    Ok(())
}

fn default_backup_dir(config: &runloop_core::config::KbConfig) -> PathBuf {
    let mut root = PathBuf::from(&config.root_dir);
    root.push("backups");
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    root.push(format!("{ts}"));
    root
}

fn render_why_output(
    events: &[EventRecord],
    settings: &output::OutputSettings,
    entity: &str,
) -> Result<(), CliError> {
    match settings.mode {
        OutputMode::Json => {
            let value = to_value(events)?;
            print_json(&value)?;
        }
        OutputMode::Table => {
            if events.is_empty() {
                println!("no events found for {entity}");
                return Ok(());
            }
            let mut table = Table::new(vec![
                "ts_ms".into(),
                "event".into(),
                "kind".into(),
                "actor".into(),
                "scope".into(),
                "summary".into(),
            ]);
            for event in events {
                table.add_row(vec![
                    Cell::number(event.ts_ms.to_string()),
                    Cell::text(event.id.to_string()),
                    Cell::text(event.kind.clone()),
                    Cell::text(event.actor.clone()),
                    Cell::text(event.scope.clone()),
                    Cell::text(summarize_event(event)),
                ]);
            }
            print_table(&table, settings)?;
        }
    }
    Ok(())
}

fn summarize_event(event: &EventRecord) -> String {
    if let Some(status) = event.payload.get("status").and_then(|v| v.as_str()) {
        return format!("status={status}");
    }
    if let Some(opening_id) = event.payload.get("opening_id").and_then(|v| v.as_str()) {
        return format!("opening_id={opening_id}");
    }
    if let Some(obj) = event.payload.as_object() {
        let mut pairs = Vec::new();
        for (idx, (key, value)) in obj.iter().enumerate() {
            if idx >= 2 {
                break;
            }
            pairs.push(format!("{key}={}", summarize_json(value, 32)));
        }
        if !pairs.is_empty() {
            return pairs.join(", ");
        }
    }
    summarize_json(&event.payload, 48)
}

fn build_executor(config: Config) -> Result<Arc<LocalExecutor>, CliError> {
    let confirmation = Arc::new(CliConfirmationProvider::new(
        config.security.confirm_external_actions,
    ));
    let registry = Arc::new(AgentRegistry::new(config.agents.search_dirs.clone()));
    let executor = build_local_executor(config, confirmation, registry)?;
    Ok(executor)
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

struct DaemonClient {
    bus: Bus,
    ctrl_sub: Subscription,
    socket_display: String,
}

struct RunOutcome {
    trace_id: TraceId,
    success: bool,
}

impl DaemonClient {
    async fn connect(config: &Config) -> Result<Self, CliError> {
        let path = resolve_socket_path(config).await?;
        let bus = Bus::connect(&path)
            .await
            .map_err(|_| CliError::DaemonUnreachable(path.display().to_string()))?;
        let ctrl_sub = bus
            .subscribe("rlp/ctrl")
            .await
            .map_err(|_| CliError::DaemonUnreachable(path.display().to_string()))?;
        Ok(Self {
            bus,
            ctrl_sub,
            socket_display: path.display().to_string(),
        })
    }

    async fn describe_agents(
        &mut self,
        refs: &[AgentRef],
    ) -> Result<Vec<DescribedAgent>, CliError> {
        if refs.is_empty() {
            return Ok(Vec::new());
        }
        let request_id = TraceId::new();
        let request = ControlRequest::DescribeAgents(DescribeAgentsRequest {
            request_id,
            agents: refs.to_vec(),
        });
        self.send_request(request_id, request).await?;
        let response = self
            .wait_for_control_response(request_id, Duration::from_millis(800), || {
                CliError::DaemonDescribeUnsupported
            })
            .await?;
        match response {
            ControlResponse::AgentsDescribed { agents, .. } => Ok(agents),
            ControlResponse::AgentsDescribeFailed { reason, .. } => {
                Err(CliError::AgentMetadata(reason))
            }
            _ => Err(CliError::AgentMetadata(
                "unexpected control response while describing agents".into(),
            )),
        }
    }

    async fn run(
        mut self,
        prepared: PreparedOpening,
        digests: Vec<AgentDigest>,
    ) -> Result<RunOutcome, CliError> {
        let request_id = TraceId::new();
        let submit = ControlRequest::RunSubmit(RunSubmitRequest {
            request_id,
            opening_yaml: prepared._yaml.clone(),
            agent_digests: digests,
        });
        self.send_request(request_id, submit).await?;
        let response = self
            .wait_for_control_response(request_id, Duration::from_millis(2000), || {
                CliError::AcceptTimeout
            })
            .await?;
        let accept = match response {
            ControlResponse::RunAccepted(acc) => acc,
            ControlResponse::RunRejected { reason, .. } => {
                return Err(CliError::RunRejected {
                    code: "REJECTED".into(),
                    message: reason,
                });
            }
            ControlResponse::RunCancelled { .. } => {
                return Err(CliError::RunRejected {
                    code: "CANCELLED".into(),
                    message: "run cancelled before start".into(),
                });
            }
            other => {
                return Err(CliError::AgentMetadata(format!(
                    "unexpected control response: {other:?}"
                )));
            }
        };
        let topic = format!("rlp/runs/{}/events", accept.trace_id);
        let mut ev_sub = self
            .bus
            .subscribe(&topic)
            .await
            .map_err(|_| CliError::DaemonUnreachable(self.socket_display.clone()))?;

        let mut final_success: Option<bool> = None;
        while let Some(msg) = ev_sub.next().await {
            if msg.header.schema_id != CT_RUN_EVENT {
                continue;
            }
            match decode_payload::<serde_json::Value>(CT_RUN_EVENT, &msg.body) {
                Ok(env) => {
                    if let Err(err) = emit_ndjson_from_run_event_env(
                        TraceId(uuid_from_u128(msg.header.trace_id)),
                        &accept.opening_id,
                        &accept.opening_name,
                        &env.payload,
                    ) {
                        eprintln!("warning: failed to render event: {err}");
                    }
                    let kind = env.payload.get("kind").and_then(|v| v.as_str());
                    let success = env
                        .payload
                        .get("meta")
                        .and_then(|m| m.get("success"))
                        .and_then(|v| v.as_bool());
                    if matches!(kind, Some("status")) && success.is_some() {
                        final_success = success;
                        break;
                    }
                }
                Err(err) => {
                    eprintln!("warning: failed to decode run event: {err}");
                }
            }
        }

        let success = final_success.unwrap_or(false);
        Ok(RunOutcome {
            trace_id: accept.trace_id,
            success,
        })
    }

    async fn send_request(
        &self,
        request_id: TraceId,
        request: ControlRequest,
    ) -> Result<(), CliError> {
        let payload = encode_payload(CT_CTRL_REQ, &request, None)?;
        let header = Header {
            schema_id: CT_CTRL_REQ,
            created_at_ms: current_millis(),
            ttl_ms: 30_000,
            trace_id: uuid_to_u128(request_id.0),
            msg_id: next_msg_id(),
            ..Header::default()
        };
        let message = runloop_bus::Message::new(header, Bytes::from(payload))
            .map_err(|e| CliError::Core(e.into()))?;
        let publish_result = self.bus.publish("rlp/ctrl", message.clone()).await;
        if publish_result.is_err() {
            let jitter = 100 + (fastrand::u32(0..300) as u64);
            time::sleep(Duration::from_millis(jitter)).await;
            self.bus
                .publish("rlp/ctrl", message)
                .await
                .map_err(|_| CliError::DaemonUnreachable(self.socket_display.clone()))
        } else {
            Ok(())
        }
    }

    async fn wait_for_control_response<F>(
        &mut self,
        request_id: TraceId,
        timeout: Duration,
        on_timeout: F,
    ) -> Result<ControlResponse, CliError>
    where
        F: FnOnce() -> CliError,
    {
        let trace_key = uuid_to_u128(request_id.0);
        let fut = async {
            while let Some(msg) = self.ctrl_sub.next().await {
                if msg.header.schema_id != CT_CTRL_RESP || msg.header.trace_id != trace_key {
                    continue;
                }
                let decoded = decode_payload::<ControlResponse>(CT_CTRL_RESP, &msg.body)
                    .map_err(|e| CliError::Core(e.into()))?;
                return Ok(decoded.payload);
            }
            Err(CliError::AcceptTimeout)
        };
        match time::timeout(timeout, fut).await {
            Ok(result) => result,
            Err(_) => Err(on_timeout()),
        }
    }
}

fn current_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn uuid_to_u128(id: Uuid) -> u128 {
    id.as_u128()
}

fn uuid_from_u128(v: u128) -> Uuid {
    Uuid::from_u128(v)
}

fn next_msg_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

async fn resolve_socket_path(config: &Config) -> Result<PathBuf, CliError> {
    let mut tried = Vec::new();
    // 1) Explicit runtime.socket_path: short-circuit; error if unreachable
    if let Some(path) = config.runtime.socket_path.as_deref() {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err(CliError::DaemonUnavailable(
                "runtime.socket_path is set but empty".into(),
            ));
        }
        let p = PathBuf::from(trimmed);
        tried.push(p.clone());
        // Probe via a simple UnixStream connect (even though Bus::connect will as well)
        match UnixStream::connect(&p).await {
            Ok(_) => return Ok(p),
            Err(_) => return Err(CliError::DaemonUnreachable(p.display().to_string())),
        }
    }

    // 2) ${runtime.sockets_dir}/rmp.sock (string field, check non-empty)
    let sockets_dir = config.runtime.sockets_dir.trim();
    if !sockets_dir.is_empty() {
        let p = PathBuf::from(sockets_dir).join("rmp.sock");
        tried.push(p.clone());
        if UnixStream::connect(&p).await.is_ok() {
            return Ok(p);
        }
    }

    // 3) ~/.runloop/sock/rmp.sock
    if let Some(home) = home_dir() {
        let p = home.join(".runloop").join("sock").join("rmp.sock");
        tried.push(p.clone());
        if UnixStream::connect(&p).await.is_ok() {
            return Ok(p);
        }
    }

    // 4) /run/runloop/rmp.sock
    let p = PathBuf::from("/run/runloop/rmp.sock");
    tried.push(p.clone());
    if UnixStream::connect(&p).await.is_ok() {
        return Ok(p);
    }

    Err(CliError::DaemonUnavailable(format_paths(&tried)))
}

fn format_paths(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "<none>".into();
    }
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

struct PreparedOpening {
    opening: Opening,
    _yaml: String,
    params: JsonValue,
    opening_name: String,
}

#[derive(Clone, Debug)]
struct ValidationFailure {
    errors: Vec<ValidationErrorRow>,
    format: ErrorsFormat,
}

#[derive(Clone, Debug)]
struct ValidationErrorRow {
    node_id: String,
    agent: AgentRef,
    agent_version: String,
    field: String,
    rule: String,
    message: String,
    value_summary: String,
    source: ValidationSource,
}

#[derive(Clone, Copy, Debug)]
enum ValidationSource {
    Manifest,
    Hint,
}

impl fmt::Display for ValidationSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValidationSource::Manifest => f.write_str("manifest"),
            ValidationSource::Hint => f.write_str("hint"),
        }
    }
}

#[derive(Default)]
struct HintLogger {
    seen: BTreeSet<String>,
}

impl HintLogger {
    fn log_missing(&mut self, reference: &AgentRef, field: &str) {
        let key = format!("{}|{}", reference.spec(), field);
        if self.seen.insert(key) {
            eprintln!(
                "note: using schema hint for {}.{field} (manifest lacks schema)",
                reference
            );
        }
    }
}

fn prepare_opening(path: &str, params_override: Option<&str>) -> Result<PreparedOpening, CliError> {
    let source = fs::read_to_string(path)?;
    let mut doc: YamlValue = serde_yaml::from_str(&source)
        .map_err(|err| CliError::InvalidTrace(format!("invalid YAML: {err}")))?;

    if let Some(params_json) = params_override {
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
    let params = JsonValue::Object(opening.params.clone());
    let opening_name = opening.name.clone();
    Ok(PreparedOpening {
        opening,
        _yaml: opening_yaml,
        params,
        opening_name,
    })
}

async fn run_opening_locally(
    config: &Config,
    prepared: PreparedOpening,
    trace_out: Option<PathBuf>,
) -> Result<(), CliError> {
    let PreparedOpening {
        opening,
        opening_name,
        params,
        ..
    } = prepared;
    let executor = build_executor(config.clone())?;
    let trace_store = TraceStore::open(&config.kb, CLI_AGENT_ID, Some("user".into()))?;
    let runner = Runner::new(opening, executor);
    let mut emitter =
        RunEventEmitter::new(runner.trace_id(), runner.opening_id(), opening_name, params);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let runner = runner.with_event_tx(tx);
    let trace_id = runner.trace_id();
    let opening_id = runner.opening_id();

    trace_store.record_run_started(&trace_id, &opening_id)?;
    emitter.emit_run_started()?;

    let mut run_future = Box::pin(async move { runner.run().await });
    let mut receiver_open = true;

    let result: Result<RunReport, RunnerError> = loop {
        tokio::select! {
            res = &mut run_future => break res,
            maybe_event = rx.recv(), if receiver_open => {
                match maybe_event {
                    Some(event) => emitter.handle_runner_event(event)?,
                    None => receiver_open = false,
                }
            }
        }
    };

    drop(run_future);

    while let Some(event) = rx.recv().await {
        emitter.handle_runner_event(event)?;
    }

    match result {
        Ok(report) => {
            let mut trace = report.trace;
            let (node_meta, node_records) = emitter.emit_node_finishes(&trace)?;
            let run_status = if trace.success { "ok" } else { "error" };
            emitter.emit_run_finished(run_status, node_meta)?;
            trace.ladder = emitter.take_ladder();
            if let Some(path) = trace_out {
                let file = File::create(&path)?;
                to_writer_pretty(file, &trace)?;
            }
            trace_store.record_nodes(&trace.trace_id, &trace.opening_id, &node_records)?;
            trace_store.record_run_trace(&trace)?;
            trace_store.record_run_finished(
                &trace.trace_id,
                &trace.opening_id,
                if trace.success { "finished" } else { "failed" },
            )?;
            trace_store.flush()?;
            if trace.success {
                Ok(())
            } else {
                Err(CliError::RunFailure(trace.trace_id))
            }
        }
        Err(err) => {
            let failure_records = emitter.summarize_failure();
            let failure_meta = node_records_to_meta(&failure_records);
            emitter.emit_run_finished("error", failure_meta)?;
            trace_store.record_nodes(&trace_id, &opening_id, &failure_records)?;
            trace_store.record_run_finished(&trace_id, &opening_id, "failed")?;
            trace_store.flush()?;
            Err(CliError::Runner(err))
        }
    }
}
