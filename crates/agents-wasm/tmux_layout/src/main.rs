use std::{
    fs,
    path::PathBuf,
};

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use runloop_agent_wasm_sdk::exec_capture;
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
#[command(about = "Apply tmux layout presets and refresh the tmux config")]
struct Cli {
    /// Preset to apply (controls the managed block content).
    #[arg(long, value_enum, default_value_t = Preset::Sensible)]
    preset: Preset,
    /// Path to the tmux configuration file to manage.
    #[arg(long, default_value = "~/.tmux.conf")]
    tmux_conf: String,
    /// Reload the tmux config after writing the managed block.
    #[arg(long, default_value_t = true)]
    reload: bool,
    /// Optional live layout to apply to the current session.
    #[arg(long)]
    apply_layout: Option<String>,
    /// Extra tmux lines appended after the preset.
    #[arg(long, value_name = "LINE")]
    extra_lines: Vec<String>,
}

#[derive(Copy, Clone, Debug, Serialize, ValueEnum, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum Preset {
    /// General quality-of-life defaults and layout bindings.
    Sensible,
    /// Minimal tweaks (base index, layout bindings only).
    Minimal,
    /// Stacked layout bias with main-pane sizing.
    Stacked,
}

impl Preset {
    fn as_str(&self) -> &'static str {
        match self {
            Preset::Sensible => "sensible",
            Preset::Minimal => "minimal",
            Preset::Stacked => "stacked",
        }
    }
}

impl std::fmt::Display for Preset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Serialize)]
struct ExecOutcome {
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "String::is_empty")]
    stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ApplyResult {
    tmux_conf: String,
    preset: Preset,
    reload: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    apply_layout: Option<String>,
    updated: bool,
    snippet_lines: usize,
    exec: Vec<ExecOutcome>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    host::signal_ready();

    let tmux_conf = expand_home(&cli.tmux_conf)?;
    let existing = fs::read_to_string(&tmux_conf).unwrap_or_default();
    let block = render_block(cli.preset, &cli.extra_lines);
    let merged = upsert_block(&existing, &block);
    let updated = merged != existing;

    if let Some(parent) = tmux_conf.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }
    fs::write(&tmux_conf, merged)
        .with_context(|| format!("failed to write {}", tmux_conf.display()))?;

    let mut exec = Vec::new();
    if cli.reload {
        let path = sh_escape(&tmux_conf.display().to_string());
        exec.push(run_tmux(&format!("tmux source-file {}", path)));
    }
    if let Some(layout) = cli.apply_layout.as_deref() {
        let layout = sh_escape(layout);
        exec.push(run_tmux(&format!("tmux select-layout {}", layout)));
    }

    let result = ApplyResult {
        tmux_conf: tmux_conf.display().to_string(),
        preset: cli.preset,
        reload: cli.reload,
        apply_layout: cli.apply_layout,
        updated,
        snippet_lines: block.lines().count(),
        exec,
    };

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

const MARKER_START: &str = "# >>> runloop:tmux_layout";
const MARKER_END: &str = "# <<< runloop:tmux_layout";

fn expand_home(input: &str) -> Result<PathBuf> {
    if let Some(rest) = input.strip_prefix("~/") {
        let home = std::env::var("HOME").context("HOME not set in environment")?;
        return Ok(PathBuf::from(home).join(rest));
    }
    Ok(PathBuf::from(input))
}

fn render_block(preset: Preset, extra_lines: &[String]) -> String {
    let mut lines = Vec::new();
    lines.push(format!("{MARKER_START} preset={}", preset.as_str()));
    lines.push("# managed by the tmux_layout agent".into());
    lines.extend(preset_snippet(preset).lines().map(ToOwned::to_owned));
    if !extra_lines.is_empty() {
        lines.push("# extra_lines from params:".into());
        lines.extend(extra_lines.iter().cloned());
    }
    lines.push(MARKER_END.into());
    lines.join("\n") + "\n"
}

fn preset_snippet(preset: Preset) -> &'static str {
    match preset {
        Preset::Sensible => r##"set -g base-index 1
setw -g pane-base-index 1
set -g renumber-windows on
set -g mouse on
set -g history-limit 20000
setw -g aggressive-resize on
bind r source-file ~/.tmux.conf \; display-message "reloaded ~/.tmux.conf"
bind | split-window -h -c '#{pane_current_path}'
bind - split-window -v -c '#{pane_current_path}'
bind -n M-h select-pane -L
bind -n M-j select-pane -D
bind -n M-k select-pane -U
bind -n M-l select-pane -R
bind T select-layout tiled
bind E select-layout even-horizontal
bind V select-layout even-vertical
bind M select-layout main-vertical
bind m select-layout main-horizontal
set -g status-position top
set -g status-style "bg=colour236 fg=colour250"
setw -g window-status-current-format "#[fg=colour50]#I:#W#[default]"
setw -g window-status-format "#[fg=colour244]#I:#W#[default]"
"##,
        Preset::Minimal => r##"set -g base-index 1
setw -g pane-base-index 1
bind T select-layout tiled
bind E select-layout even-horizontal
bind V select-layout even-vertical
bind M select-layout main-vertical
bind m select-layout main-horizontal
bind -n M-h select-pane -L
bind -n M-j select-pane -D
bind -n M-k select-pane -U
bind -n M-l select-pane -R
"##,
        Preset::Stacked => r##"set -g base-index 1
setw -g pane-base-index 1
set -g renumber-windows on
set -g mouse on
set -g main-pane-height 30
set -g main-pane-width 120
setw -g aggressive-resize on
bind r source-file ~/.tmux.conf \; display-message "reloaded ~/.tmux.conf"
bind m select-layout main-horizontal
bind M select-layout main-vertical
bind T select-layout tiled
bind -n M-h select-pane -L
bind -n M-j select-pane -D
bind -n M-k select-pane -U
bind -n M-l select-pane -R
set -g status-style "bg=colour234 fg=colour250"
set -g status-left-length 30
set -g status-right-length 80
"##,
    }
}

fn upsert_block(existing: &str, block: &str) -> String {
    if let Some(start) = existing.find(MARKER_START) {
        if let Some(end_rel) = existing[start..].find(MARKER_END) {
            let end = start + end_rel + MARKER_END.len();
            let before = existing[..start].trim_end();
            let after = existing[end..].trim_start_matches(['\n', '\r']);
            let mut combined = String::new();
            if !before.is_empty() {
                combined.push_str(before);
                combined.push_str("\n\n");
            }
            combined.push_str(block.trim_end());
            if !after.is_empty() {
                combined.push_str("\n\n");
                combined.push_str(after);
                combined.push('\n');
            } else {
                combined.push('\n');
            }
            return combined;
        }
    }

    if existing.trim().is_empty() {
        block.to_string()
    } else {
        format!("{}\n\n{}\n", existing.trim_end(), block.trim_end())
    }
}

fn sh_escape(input: &str) -> String {
    if input.is_empty() {
        return "''".into();
    }

    let mut escaped = String::with_capacity(input.len() + 2);
    escaped.push('\'');
    for ch in input.chars() {
        if ch == '\'' {
            escaped.push_str("'\\''");
        } else {
            escaped.push(ch);
        }
    }
    escaped.push('\'');
    escaped
}

fn run_tmux(command: &str) -> ExecOutcome {
    const CAP: usize = 4096;
    match exec_capture(command, CAP, CAP) {
        Ok(output) => ExecOutcome {
            command: command.into(),
            exit_code: Some(output.exit_code),
            stdout: output.stdout,
            stderr: output.stderr,
            error: None,
        },
        Err(err) => ExecOutcome {
            command: command.into(),
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            error: Some(err.to_string()),
        },
    }
}
