use anyhow::Result;
use clap::Parser;
use serde::Serialize;

#[allow(unsafe_code)]
mod host {
    #[link(wasm_import_module = "runloop")]
    unsafe extern "C" {
        fn notify_ready();
    }

    pub(super) fn signal_ready() {
        unsafe { notify_ready() };
    }
}

#[derive(Parser, Debug)]
#[command(about = "Runloop system_tra agent (wasm32-wasip1)")]
struct Cli {
    /// Input payload from the opening (customize for your agent).
    #[arg(long)]
    input: Option<String>,
}

#[derive(Debug, Serialize)]
struct StubOutput {
    message: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    host::signal_ready();
    let output = StubOutput {
        message: cli
            .input
            .unwrap_or_else(|| "replace with real agent logic".into()),
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
