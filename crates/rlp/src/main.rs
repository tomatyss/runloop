use clap::Parser;

/// Runloop CLI placeholder.
#[derive(Parser, Debug)]
#[command(name = "rlp", about = "Runloop CLI (work in progress)")]
struct Cli {}

#[tokio::main]
async fn main() {
    let _ = Cli::parse();
}
