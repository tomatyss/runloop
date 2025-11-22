use clap::{Args, Subcommand};
use dirs::home_dir;
use is_terminal::IsTerminal;
use runloop_agent_registry::{
    Budget, Observability, ToolEntry, ToolsDoc, Transport, digest_file_hex,
};
use runloop_core::Config;
use serde_json::json;
use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;
use url::Url;

const ZERO_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Subcommand, Debug)]
pub enum AgentCommands {
    Scaffold(ScaffoldArgs),
}

#[derive(Args, Debug)]
pub struct ScaffoldArgs {
    /// Agent name (snake_case).
    pub name: String,
    /// Run without interactive prompts (uses defaults/flags).
    #[arg(long)]
    pub non_interactive: bool,
    /// Optional description used in README.
    #[arg(long)]
    pub description: Option<String>,
    /// Default model id to embed in the scaffold.
    #[arg(long)]
    pub model: Option<String>,
    /// Secret id for the model provider (non-interactive override).
    #[arg(long)]
    pub model_secret: Option<String>,
    /// Filesystem capability entries (comma-separated).
    #[arg(long = "cap-fs", value_delimiter = ',')]
    pub cap_fs: Vec<String>,
    /// Network host allowlist entries (comma-separated).
    #[arg(long = "cap-net", value_delimiter = ',')]
    pub cap_net: Vec<String>,
    /// KB read domains (comma-separated, empty = false).
    #[arg(long = "cap-kb-read", value_delimiter = ',')]
    pub cap_kb_read: Vec<String>,
    /// KB write domains (comma-separated, empty = false).
    #[arg(long = "cap-kb-write", value_delimiter = ',')]
    pub cap_kb_write: Vec<String>,
    /// Root directory for agent bundles (defaults to workspace ./agents via RUNLOOP_WORKSPACE_ROOT or nearest Cargo.toml, else first agents.search_dirs entry).
    #[arg(long = "root", value_name = "PATH")]
    pub root_dir: Option<PathBuf>,
    /// Root directory for wasm agent crates (defaults to crates/agents-wasm).
    #[arg(long = "crates-dir", value_name = "PATH")]
    pub crates_dir: Option<PathBuf>,
    /// Optional path for the starter opening YAML.
    #[arg(long = "opening-path", value_name = "PATH")]
    pub opening_path: Option<PathBuf>,
    /// Overwrite existing files and directories.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug)]
pub struct ScaffoldResult {
    pub agent_dir: PathBuf,
    pub crate_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub tools_path: PathBuf,
    pub opening_path: Option<PathBuf>,
}

#[derive(Debug, Error)]
pub enum ScaffoldError {
    #[error(
        "invalid agent name '{0}' (use lowercase letters, digits, and underscores; must start with a letter)"
    )]
    InvalidName(String),
    #[error("path already exists: {0}")]
    Exists(PathBuf),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("digest error: {0}")]
    Digest(String),
    #[error("invalid tools.json: {0}")]
    Tools(String),
}

pub fn handle_agent(cmd: AgentCommands, config: &Config) -> Result<(), ScaffoldError> {
    match cmd {
        AgentCommands::Scaffold(args) => {
            let result = scaffold(args, config)?;
            println!("created agent bundle at {}", result.agent_dir.display());
            println!("created wasm crate at {}", result.crate_dir.display());
            println!("manifest: {}", result.manifest_path.display());
            println!("tools.json: {}", result.tools_path.display());
            if let Some(path) = &result.opening_path {
                println!("opening: {}", path.display());
            }
        }
    }
    Ok(())
}

fn scaffold(args: ScaffoldArgs, config: &Config) -> Result<ScaffoldResult, ScaffoldError> {
    validate_name(&args.name)?;
    let interactive = !args.non_interactive && io::stdin().is_terminal();
    let answers = gather_answers(&args, config, interactive)?;
    let agent_root = resolve_agent_root(&args.root_dir, config);
    let crate_root = resolve_crate_root(&args.crates_dir);
    let agent_dir = agent_root.join(&args.name);
    let crate_dir = crate_root.join(&args.name);
    let opening_path = if answers.generate_opening {
        Some(
            answers
                .opening_path
                .clone()
                .or_else(|| args.opening_path.clone())
                .unwrap_or_else(|| PathBuf::from(format!("examples/openings/{}.yaml", args.name))),
        )
    } else {
        None
    };
    if !args.force {
        for path in [&agent_dir, &crate_dir] {
            if path.exists() {
                return Err(ScaffoldError::Exists(path.to_path_buf()));
            }
        }
        if let Some(path) = &opening_path
            && path.exists()
        {
            return Err(ScaffoldError::Exists(path.to_path_buf()));
        }
    }

    write_bundle(&answers, &agent_dir)?;
    let tools_path = agent_dir.join("tools.json");
    write_tools(&answers.tools, &tools_path)?;
    let tools_digest =
        digest_file_hex(&tools_path).map_err(|err| ScaffoldError::Digest(err.to_string()))?;
    write_manifest(&answers, &agent_dir, &tools_digest)?;
    write_policy_caps(&answers, &agent_dir)?;
    write_crate(&args.name, &crate_dir)?;
    if answers.generate_opening
        && let Some(path) = opening_path.as_ref()
    {
        write_opening(&answers, path)?;
    }

    Ok(ScaffoldResult {
        agent_dir: agent_dir.clone(),
        crate_dir,
        manifest_path: agent_dir.join("manifest.toml"),
        tools_path,
        opening_path,
    })
}

fn validate_name(name: &str) -> Result<(), ScaffoldError> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(ScaffoldError::InvalidName(name.into()));
    };
    if !first.is_ascii_lowercase() {
        return Err(ScaffoldError::InvalidName(name.into()));
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        return Err(ScaffoldError::InvalidName(name.into()));
    }
    Ok(())
}

fn resolve_agent_root(root: &Option<PathBuf>, config: &Config) -> PathBuf {
    resolve_agent_root_with_cwd(root, config, None)
}

fn resolve_agent_root_with_cwd(
    root: &Option<PathBuf>,
    config: &Config,
    cwd: Option<&Path>,
) -> PathBuf {
    if let Some(dir) = root {
        return expand_tilde(dir.clone());
    }
    if let Some(workspace) = workspace_root(cwd) {
        return workspace.join("agents");
    }
    preferred_config_agent_dir(config).unwrap_or_else(|| PathBuf::from("agents"))
}

fn resolve_crate_root(root: &Option<PathBuf>) -> PathBuf {
    resolve_crate_root_with_cwd(root, None)
}

fn resolve_crate_root_with_cwd(root: &Option<PathBuf>, cwd: Option<&Path>) -> PathBuf {
    if let Some(dir) = root {
        return expand_tilde(dir.clone());
    }
    if let Some(workspace) = workspace_root(cwd) {
        return workspace.join("crates/agents-wasm");
    }
    PathBuf::from("crates/agents-wasm")
}

fn preferred_config_agent_dir(config: &Config) -> Option<PathBuf> {
    config
        .agents
        .search_dirs
        .iter()
        .map(PathBuf::from)
        .map(expand_tilde)
        .next()
}

fn workspace_root(cwd: Option<&Path>) -> Option<PathBuf> {
    if cwd.is_none() {
        if let Some(env_root) = env::var_os("RUNLOOP_WORKSPACE_ROOT") {
            let expanded = expand_tilde(PathBuf::from(env_root));
            if expanded.is_dir() {
                return Some(expanded);
            }
        }
    }
    walk_for_workspace_root(cwd)
}

fn walk_for_workspace_root(cwd: Option<&Path>) -> Option<PathBuf> {
    let mut dir = cwd.map(PathBuf::from).or_else(|| env::current_dir().ok())?;
    loop {
        if dir.join("Cargo.toml").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

fn expand_tilde(path: PathBuf) -> PathBuf {
    match path.to_str() {
        Some(raw) if raw.starts_with("~/") => home_dir()
            .map(|home| home.join(raw.trim_start_matches("~/")))
            .unwrap_or(path),
        _ => path,
    }
}

fn write_bundle(answers: &WizardAnswers, agent_dir: &Path) -> Result<(), ScaffoldError> {
    let bin_dir = agent_dir.join("bin");
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(agent_dir)?;
    fs::write(agent_dir.join("README.md"), readme_md(answers))?;
    let gitkeep = bin_dir.join(".gitkeep");
    if !gitkeep.exists() {
        File::create(&gitkeep)?;
    }
    Ok(())
}

fn write_manifest(
    answers: &WizardAnswers,
    agent_dir: &Path,
    tools_digest: &str,
) -> Result<(), ScaffoldError> {
    let with_schema = r#"[schemas.with]
type = "object"
additionalProperties = false
required = ["input"]

[schemas.with.properties.input]
type = "string"
minLength = 1
"#;
    let contents = format!(
        r#"[agent]
name = "{name}"
version = "0.1.0"
kind = "wasm32-wasip1"

entry_wasm = {{ path = "bin/{name}.wasm", blake3 = "{ZERO_DIGEST}" }}

[ports]
in = []
out = ["out"]

[caps]
file = "policy.caps"

[artifacts.tools]
path = "tools.json"
blake3 = "{tools_digest}"
version = 1
"#,
        name = answers.name
    );
    let mut rendered = contents;
    rendered.push('\n');
    rendered.push_str(with_schema);
    fs::write(agent_dir.join("manifest.toml"), rendered)?;
    Ok(())
}

fn write_crate(name: &str, crate_dir: &Path) -> Result<(), ScaffoldError> {
    fs::create_dir_all(crate_dir.join("src"))?;
    let cargo = format!(
        r#"[package]
name = "runloop-agent-{name}-wasm"
version = "0.1.0"
edition = "2024"
license.workspace = true

[[bin]]
name = "{name}_wasm"
path = "src/main.rs"

[dependencies]
anyhow = "1.0"
clap = {{ version = "4.5", features = ["derive"] }}
serde = {{ version = "1.0", features = ["derive"] }}
serde_json = "1.0"

[lints]
workspace = true
"#
    );
    fs::write(crate_dir.join("Cargo.toml"), cargo)?;
    fs::write(crate_dir.join("src/main.rs"), crate_main(name))?;
    Ok(())
}

fn write_policy_caps(answers: &WizardAnswers, agent_dir: &Path) -> Result<(), ScaffoldError> {
    let secrets = answers
        .secrets
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let fs_caps = render_array_or_empty(&answers.caps.fs);
    let net_caps = render_array_or_empty(&answers.caps.net);
    let kb_read = render_array_or_false(&answers.caps.kb_read);
    let kb_write = render_array_or_false(&answers.caps.kb_write);
    let caps = format!(
        r#"[capabilities]
fs = {fs_caps}
net = {net_caps}
time = false
kb_read = {kb_read}
kb_write = {kb_write}
secrets = [{secrets}]
model = {model}
exec = false
"#,
        model = answers.model_cap
    );
    fs::write(agent_dir.join("policy.caps"), caps)?;
    Ok(())
}

fn render_array(values: &[String]) -> String {
    let rendered = values
        .iter()
        .map(|v| format!("\"{v}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{rendered}]")
}

fn render_array_or_false(values: &[String]) -> String {
    if values.is_empty() {
        "false".into()
    } else {
        render_array(values)
    }
}

fn render_array_or_empty(values: &[String]) -> String {
    if values.is_empty() {
        "[]".into()
    } else {
        render_array(values)
    }
}

fn write_tools(doc: &ToolsDoc, tools_path: &Path) -> Result<(), ScaffoldError> {
    let tools_json = serde_json::to_string_pretty(doc)?;
    fs::write(tools_path, tools_json)?;
    Ok(())
}

fn readme_md(answers: &WizardAnswers) -> String {
    format!(
        r#"# {name} agent
{description}

Scaffolded by `rlp agent scaffold`. Build the wasm artifact with:

```
just build-agents-wasm
```

Model: `{model}` (secret: {secret})
FS caps: {fs}
Net caps: {net}
KB read: {kb_read}
KB write: {kb_write}

Generated files:
- `agents/{name}/manifest.toml`
- `agents/{name}/policy.caps`
- `agents/{name}/tools.json`
- `agents/{name}/README.md`
- `crates/agents-wasm/{name}/` (wasm stub)
- `{opening}`

Edit `crates/agents-wasm/{name}/src/main.rs` to implement your logic and
re-run `just build-agents-wasm` to refresh digests.
"#,
        name = answers.name,
        description = answers.description,
        model = answers.model_id,
        secret = answers.model_secret.as_deref().unwrap_or("<none>"),
        fs = if answers.caps.fs.is_empty() {
            "none".into()
        } else {
            answers.caps.fs.join(", ")
        },
        net = if answers.caps.net.is_empty() {
            "none".into()
        } else {
            answers.caps.net.join(", ")
        },
        kb_read = if answers.caps.kb_read.is_empty() {
            "false".into()
        } else {
            answers.caps.kb_read.join(", ")
        },
        kb_write = if answers.caps.kb_write.is_empty() {
            "false".into()
        } else {
            answers.caps.kb_write.join(", ")
        },
        opening = answers
            .opening_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<skipped>".into())
    )
}

fn crate_main(name: &str) -> String {
    const TEMPLATE: &str = r#"use anyhow::Result;
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
#[command(about = "Runloop {name} agent (wasm32-wasip1)")]
struct Cli {
    /// Placeholder payload input (customize for your agent).
    #[arg(long)]
    payload: Option<String>,
}

#[derive(Debug, Serialize)]
struct StubOutput {
    message: String,
}

fn main() -> Result<()> {
    let _cli = Cli::parse();
    host::signal_ready();
    let output = StubOutput {
        message: "replace with real agent logic".into(),
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
"#;
    TEMPLATE.replace("{name}", name)
}

fn write_opening(answers: &WizardAnswers, path: &Path) -> Result<(), ScaffoldError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let opening = format!(
        r#"version: 0
name: {name}
goals:
  - "{goal}"
params:
  {param}: "example input"

policy:
  budget_tokens: 4000
  timeout_ms: 15000
  confirm_external: true

nodes:
  - id: {node_id}
    use: agent:{name}
    with:
      input: "{{{{params.{param}}}}}"
    timeout_ms: 10000

edges: []

success:
  any_of:
    - exists({node_id}.out)
"#,
        name = answers.name,
        goal = answers.opening_goal,
        param = answers.opening_param,
        node_id = format!("{}_node", answers.name)
    );
    fs::write(path, opening)?;
    Ok(())
}

#[derive(Debug, Default)]
struct CapsSpec {
    fs: Vec<String>,
    net: Vec<String>,
    kb_read: Vec<String>,
    kb_write: Vec<String>,
}

#[derive(Debug)]
struct WizardAnswers {
    name: String,
    description: String,
    model_id: String,
    model_secret: Option<String>,
    model_cap: bool,
    caps: CapsSpec,
    secrets: Vec<String>,
    tools: ToolsDoc,
    generate_opening: bool,
    opening_path: Option<PathBuf>,
    opening_goal: String,
    opening_param: String,
}

fn gather_answers(
    args: &ScaffoldArgs,
    config: &Config,
    interactive: bool,
) -> Result<WizardAnswers, ScaffoldError> {
    let mut prompter = Prompter::new(interactive);
    let default_desc = args
        .description
        .clone()
        .unwrap_or_else(|| format!("Agent {}", args.name));
    let description = if interactive {
        prompter.prompt_string("Description", Some(&default_desc))?
    } else {
        default_desc
    };

    let default_model = args
        .model
        .clone()
        .unwrap_or_else(|| config.models.default.clone());
    let default_secret = args
        .model_secret
        .clone()
        .or_else(|| provider_secret_for_model(&default_model, config));
    let model_id = if interactive {
        prompter.prompt_string("Model id", Some(&default_model))?
    } else {
        default_model
    };
    let model_secret = if interactive {
        prompter.prompt_optional("Secret id for model (optional)", default_secret.clone())?
    } else {
        default_secret
    };

    let mut caps = CapsSpec::default();
    let default_fs = format!("~/.runloop/artifacts/{}", args.name);
    caps.fs = if !args.cap_fs.is_empty() {
        args.cap_fs.clone()
    } else if interactive {
        prompter.prompt_list(
            "Filesystem roots (comma-separated)",
            std::slice::from_ref(&default_fs),
        )?
    } else {
        vec![default_fs]
    };
    caps.net = if !args.cap_net.is_empty() {
        args.cap_net.clone()
    } else if interactive {
        prompter.prompt_list("Network hosts (comma-separated, blank for none)", &[])?
    } else {
        Vec::new()
    };
    caps.kb_read = if !args.cap_kb_read.is_empty() {
        args.cap_kb_read.clone()
    } else if interactive {
        prompter.prompt_list("KB read domains (comma-separated, blank for none)", &[])?
    } else {
        Vec::new()
    };
    caps.kb_write = if !args.cap_kb_write.is_empty() {
        args.cap_kb_write.clone()
    } else if interactive {
        prompter.prompt_list("KB write domains (comma-separated, blank for none)", &[])?
    } else {
        Vec::new()
    };

    let mut tools = Vec::new();
    if interactive {
        while prompter.prompt_bool("Add a tool attachment?", false)? {
            tools.push(prompter.prompt_tool()?);
        }
    }
    let tools_doc = ToolsDoc { version: 1, tools };
    if let Err(detail) = tools_doc.validate() {
        return Err(ScaffoldError::Tools(detail));
    }

    let mut secrets = Vec::new();
    if let Some(secret) = model_secret.clone() {
        secrets.push(secret);
    }
    secrets.extend(
        tools_doc
            .tools
            .iter()
            .flat_map(|tool| tool.secrets.clone())
            .collect::<Vec<_>>(),
    );

    // Add hostnames from tools into net caps.
    for host in tool_hosts(&tools_doc) {
        if !caps.net.contains(&host) {
            caps.net.push(host);
        }
    }

    let generate_opening =
        !interactive || prompter.prompt_bool("Generate starter opening YAML?", true)?;

    let opening_goal = if interactive {
        prompter.prompt_string(
            "Opening goal description",
            Some(&format!("run the {} agent", args.name)),
        )?
    } else {
        format!("run the {} agent", args.name)
    };
    let opening_param = if interactive {
        prompter.prompt_string("Primary param name", Some("prompt"))?
    } else {
        "prompt".into()
    };

    Ok(WizardAnswers {
        name: args.name.clone(),
        description,
        model_id: model_id.clone(),
        model_secret,
        model_cap: !model_id.trim().is_empty(),
        caps,
        secrets,
        tools: tools_doc,
        generate_opening,
        opening_path: args.opening_path.clone(),
        opening_goal,
        opening_param,
    })
}

fn provider_secret_for_model(model_id: &str, config: &Config) -> Option<String> {
    config
        .models
        .broker
        .providers
        .iter()
        .find(|provider| {
            model_id == provider.id || model_id.starts_with(&format!("{}:", provider.id))
        })
        .and_then(|p| p.secret_id.clone())
}

fn tool_hosts(doc: &ToolsDoc) -> Vec<String> {
    let mut hosts = Vec::new();
    for tool in &doc.tools {
        if let Transport::Http { url, .. } = &tool.transport
            && let Ok(parsed) = Url::parse(url)
            && let Some(host) = parsed.host_str()
        {
            let entry = if let Some(port) = parsed.port() {
                format!("{host}:{port}")
            } else {
                host.to_string()
            };
            if !hosts.contains(&entry) {
                hosts.push(entry);
            }
        }
    }
    hosts
}

struct Prompter {
    interactive: bool,
}

impl Prompter {
    fn new(interactive: bool) -> Self {
        Self { interactive }
    }

    fn prompt_string(&self, prompt: &str, default: Option<&str>) -> Result<String, ScaffoldError> {
        if !self.interactive {
            return Ok(default.unwrap_or("").to_string());
        }
        print_prompt(prompt, default);
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let value = line.trim();
        if value.is_empty() {
            Ok(default.unwrap_or("").to_string())
        } else {
            Ok(value.to_string())
        }
    }

    fn prompt_optional(
        &self,
        prompt: &str,
        default: Option<String>,
    ) -> Result<Option<String>, ScaffoldError> {
        if !self.interactive {
            return Ok(default);
        }
        print_prompt(prompt, default.as_deref());
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let value = line.trim();
        if value.is_empty() {
            Ok(default)
        } else {
            Ok(Some(value.to_string()))
        }
    }

    fn prompt_bool(&self, prompt: &str, default: bool) -> Result<bool, ScaffoldError> {
        if !self.interactive {
            return Ok(default);
        }
        let default_hint = if default { "[Y/n]" } else { "[y/N]" };
        print!("{prompt} {default_hint}: ");
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let trimmed = line.trim().to_lowercase();
        if trimmed.is_empty() {
            return Ok(default);
        }
        match trimmed.as_str() {
            "y" | "yes" => Ok(true),
            "n" | "no" => Ok(false),
            _ => Ok(default),
        }
    }

    fn prompt_list(
        &mut self,
        prompt: &str,
        default: &[String],
    ) -> Result<Vec<String>, ScaffoldError> {
        if !self.interactive {
            return Ok(default.to_vec());
        }
        let default_rendered = if default.is_empty() {
            None
        } else {
            Some(default.join(","))
        };
        print_prompt(prompt, default_rendered.as_deref());
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let mut values = if line.trim().is_empty() {
            default.to_vec()
        } else {
            line.split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect::<Vec<_>>()
        };
        values.sort();
        values.dedup();
        Ok(values)
    }

    fn prompt_tool(&mut self) -> Result<ToolEntry, ScaffoldError> {
        let id = self.prompt_string("Tool id (e.g., mail.smtp_send)", None)?;
        let description = self.prompt_string("Tool description", None)?;
        let transport_kind = self.prompt_string("Transport kind (http/exec)", Some("http"))?;
        let transport = match transport_kind.as_str() {
            "exec" => {
                let command = self.prompt_string("Exec command (absolute path)", None)?;
                let args = self.prompt_list("Exec args (comma-separated, optional)", &[])?;
                Transport::Exec { command, args }
            }
            _ => {
                let method = self.prompt_string("HTTP method", Some("POST"))?;
                let url = self.prompt_string("HTTP url", None)?;
                Transport::Http {
                    method,
                    url,
                    headers: Default::default(),
                }
            }
        };

        let secrets = self.prompt_list("Secrets (comma-separated, optional)", &[])?;
        let capabilities =
            self.prompt_list("Capabilities required (comma-separated, optional)", &[])?;
        let requires_confirmation =
            self.prompt_bool("Require confirmation for this tool?", false)?;
        let budget_tokens = self
            .prompt_optional("Budget tokens (optional integer)", None)?
            .and_then(|s| s.parse::<u64>().ok());
        let budget_usd = self
            .prompt_optional("Budget USD (optional decimal)", None)?
            .and_then(|s| s.parse::<f64>().ok());
        let budget = if budget_tokens.is_some() || budget_usd.is_some() {
            Some(Budget {
                tokens: budget_tokens,
                usd: budget_usd,
            })
        } else {
            None
        };
        let observability_tags =
            self.prompt_list("Observability tags (comma-separated, optional)", &[])?;
        let observability = if observability_tags.is_empty() {
            None
        } else {
            Some(Observability {
                tags: observability_tags,
            })
        };

        Ok(ToolEntry {
            id,
            description,
            transport,
            input_schema: json!({}),
            result_schema: json!({}),
            schema_refs: Default::default(),
            capabilities,
            secrets,
            budget,
            requires_confirmation,
            observability,
        })
    }
}

fn print_prompt(prompt: &str, default: Option<&str>) {
    match default {
        Some(def) if !def.is_empty() => print!("{prompt} [{def}]: "),
        _ => print!("{prompt}: "),
    }
    let _ = io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use runloop_core::Config;
    use tempfile::tempdir;

    #[test]
    fn resolve_defaults_to_workspace_agents_dir() {
        let temp = tempdir().expect("temp");
        fs::write(temp.path().join("Cargo.toml"), "").expect("cargo file");
        fs::create_dir_all(temp.path().join("crates/agents-wasm")).expect("crate dir");

        let config = Config::default();
        let agent_root = resolve_agent_root_with_cwd(&None, &config, Some(temp.path()));
        let crate_root = resolve_crate_root_with_cwd(&None, Some(temp.path()));
        assert_eq!(agent_root, temp.path().join("agents"));
        assert_eq!(crate_root, temp.path().join("crates/agents-wasm"));
    }

    #[test]
    fn resolve_falls_back_to_config_dir_when_no_workspace() {
        let temp = tempdir().expect("temp");
        let agents_root = temp.path().join("custom-agents");
        fs::create_dir_all(&agents_root).expect("agents root");

        let mut config = Config::default();
        config.agents.search_dirs = vec![agents_root.to_string_lossy().into_owned()];

        let agent_root = resolve_agent_root_with_cwd(&None, &config, Some(temp.path()));
        let crate_root = resolve_crate_root_with_cwd(&None, Some(temp.path()));
        assert_eq!(agent_root, agents_root);
        assert_eq!(crate_root, PathBuf::from("crates/agents-wasm"));
    }

    #[test]
    fn resolve_prefers_config_dir_even_if_missing() {
        let temp = tempdir().expect("temp");
        let agents_root = temp.path().join("custom-agents");

        let mut config = Config::default();
        config.agents.search_dirs = vec![agents_root.to_string_lossy().into_owned()];

        let agent_root = resolve_agent_root_with_cwd(&None, &config, Some(temp.path()));
        assert_eq!(agent_root, agents_root);
    }

    #[test]
    fn scaffold_creates_bundle_and_crate() {
        let temp = tempdir().expect("temp");
        let agents_root = temp.path().join("agents");
        let crates_root = temp.path().join("crates").join("agents-wasm");
        let opening = temp
            .path()
            .join("examples")
            .join("openings")
            .join("demo_agent.yaml");
        let config = Config::default();
        let args = ScaffoldArgs {
            name: "demo_agent".into(),
            non_interactive: true,
            description: None,
            model: None,
            model_secret: None,
            cap_fs: Vec::new(),
            cap_net: Vec::new(),
            cap_kb_read: Vec::new(),
            cap_kb_write: Vec::new(),
            root_dir: Some(agents_root.clone()),
            crates_dir: Some(crates_root.clone()),
            opening_path: Some(opening.clone()),
            force: false,
        };
        let result = scaffold(args, &config).expect("scaffold");
        assert!(result.manifest_path.is_file());
        assert!(result.tools_path.is_file());
        assert!(result.crate_dir.join("Cargo.toml").is_file());
        assert!(result.crate_dir.join("src/main.rs").is_file());
        assert!(
            result
                .opening_path
                .as_ref()
                .map(|p| p.is_file())
                .unwrap_or(true)
        );
        let policy =
            fs::read_to_string(result.agent_dir.join("policy.caps")).expect("policy contents");
        assert!(
            policy.contains("net = []"),
            "empty net caps should render as an empty array"
        );
        assert_eq!(result.opening_path.unwrap(), opening);
    }
}
