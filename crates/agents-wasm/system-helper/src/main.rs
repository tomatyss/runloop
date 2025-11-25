use anyhow::{anyhow, Context, Result};
use clap::Parser;
use runloop_agent_wasm_sdk::{
    complete_model, exec_capture, ModelParams, ModelRequest, TraceId,
};
use serde_json::{Map, Value, json};

#[allow(unsafe_code)]
mod host {
    #[link(wasm_import_module = "runloop")]
    unsafe extern "C" {
        fn notify_ready();
    }

    pub(super) fn signal_ready() {
        // SAFETY: host provides `notify_ready` with no parameters.
        unsafe { notify_ready() };
    }
}

#[derive(Parser, Debug)]
#[command(about = "Minimal wasm helper that calls Gemini and captures host exec output")]
struct Cli {
    /// Prompt to send to the model (skips model call when unset).
    #[arg(long)]
    prompt: Option<String>,
    /// Model identifier to request from the broker.
    #[arg(long, default_value = "gemini-1.5-flash")]
    model: String,
    /// Command to run on the host (skips exec when unset).
    #[arg(long)]
    command: Option<String>,
    /// Buffer capacity for model and exec outputs.
    #[arg(long, default_value_t = 8192)]
    output_cap: usize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    host::signal_ready();

    let mut payload = Map::new();

    if let Some(prompt) = cli.prompt {
        let request = ModelRequest {
            trace_id: TraceId::default(),
            model: cli.model.clone(),
            prompt,
            role_system: None,
            params: Some(ModelParams {
                temperature: Some(0.4),
                top_p: None,
                max_tokens: None,
                stop: None,
            }),
            budget_tokens: Some(4096),
            timeout_ms: Some(15000),
            cache_ttl_ms: None,
            cache_key: None,
            stream: false,
            extras: None,
        };

        let output =
            complete_model(&request, cli.output_cap, 2048).map_err(|err| anyhow!(err.to_string()))?;
        payload.insert(
            "model".into(),
            json!({
                "text": output.text,
                "meta": output.meta
            }),
        );
    }

    if let Some(command) = cli.command {
        let exec = exec_capture(&command, cli.output_cap, cli.output_cap)
            .map_err(|err| anyhow!(err.to_string()))
            .context("exec_spawn_capture failed")?;
        payload.insert(
            "exec".into(),
            json!({
                "command": command,
                "exit_code": exec.exit_code,
                "stdout": exec.stdout,
                "stderr": exec.stderr,
            }),
        );
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&Value::Object(payload))?
    );
    Ok(())
}
