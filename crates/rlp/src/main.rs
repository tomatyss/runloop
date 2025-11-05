use clap::{Args, Parser, Subcommand};
use runloop_core::Config;
use runloop_kb::{KnowledgeBase, Materializer};
use runloop_router::{Classification, Router};
use serde_json::to_string_pretty;
use thiserror::Error;

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
    /// Prompt to execute via router.
    #[arg(value_name = "PROMPT", trailing_var_arg = true)]
    prompt: Vec<String>,
}

#[derive(Args, Debug)]
struct ReplayArgs {
    /// Trace identifier to replay.
    #[arg(value_name = "TRACE_ID")]
    trace_id: Option<String>,
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
    let prompt = args.prompt.join(" ");
    if prompt.is_empty() {
        println!("run command not yet implemented; provide a prompt to execute");
    } else {
        println!(
            "run command not yet implemented; received prompt: {}",
            prompt
        );
    }
    Ok(())
}

async fn handle_replay(args: ReplayArgs) -> Result<(), CliError> {
    if let Some(trace_id) = args.trace_id {
        println!("replay command not yet implemented; trace id: {trace_id}");
    } else {
        println!("replay command not yet implemented; supply a trace id");
    }
    Ok(())
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

fn catch_up_views(kb: &KnowledgeBase) -> Result<(), runloop_kb::Error> {
    let materializer = Materializer::new(kb.clone());
    while materializer.sync()? {}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catch_up_views_is_noop_for_empty_kb() {
        let kb = KnowledgeBase::new();
        catch_up_views(&kb).expect("catch up succeeds");
    }
}
