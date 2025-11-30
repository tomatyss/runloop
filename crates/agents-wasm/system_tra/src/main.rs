use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};

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
    /// JSON input describing the requested system tweaks.
    #[arg(long)]
    input: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct Input {
    tmux_conf: Option<String>,
    history_limit: Option<u32>,
    extra_tmux_lines: Option<Vec<String>>,
    bashrc: Option<String>,
    hist_size: Option<u32>,
    hist_file_size: Option<u32>,
}

#[derive(Debug, Serialize)]
struct FileChange {
    path: String,
    updated: bool,
}

#[derive(Debug, Serialize)]
struct Output {
    tmux: FileChange,
    history: FileChange,
    history_limit: u32,
    hist_size: u32,
    hist_file_size: u32,
    extra_tmux_lines: usize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    host::signal_ready();

    let input = parse_input(cli.input.as_deref())?;
    let tmux_path = expand_home(input.tmux_conf.as_deref().unwrap_or("~/.tmux.conf"))?;
    let history_limit = input.history_limit.unwrap_or(50_000);
    let extra_lines = input.extra_tmux_lines.unwrap_or_default();

    let tmux_existing = fs::read_to_string(&tmux_path).unwrap_or_default();
    let tmux_block = render_tmux_block(history_limit, &extra_lines);
    let tmux_merged = upsert_tmux_block(&tmux_existing, &tmux_block);
    let tmux_changed = tmux_merged != tmux_existing;

    if let Some(parent) = tmux_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }
    fs::write(&tmux_path, tmux_merged)
        .with_context(|| format!("failed to write {}", tmux_path.display()))?;

    let bashrc_path = expand_home(input.bashrc.as_deref().unwrap_or("~/.bashrc"))?;
    let hist_size = input.hist_size.unwrap_or(50_000);
    let hist_file_size = input.hist_file_size.unwrap_or(100_000);
    let bash_existing = fs::read_to_string(&bashrc_path).unwrap_or_default();
    let bash_updated =
        upsert_history_limits(&bash_existing, hist_size, hist_file_size, "# runloop:system_tra");
    let bash_changed = bash_updated != bash_existing;
    if let Some(parent) = bashrc_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }
    fs::write(&bashrc_path, bash_updated)
        .with_context(|| format!("failed to write {}", bashrc_path.display()))?;

    let output = Output {
        tmux: FileChange {
            path: tmux_path.display().to_string(),
            updated: tmux_changed,
        },
        history: FileChange {
            path: bashrc_path.display().to_string(),
            updated: bash_changed,
        },
        history_limit,
        hist_size,
        hist_file_size,
        extra_tmux_lines: extra_lines.len(),
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn parse_input(raw: Option<&str>) -> Result<Input> {
    match raw {
        Some(text) if !text.trim().is_empty() => {
            let parsed: Input = serde_json::from_str(text)
                .with_context(|| "input must be JSON (see agents/system_tra/README.md for shape)")?;
            Ok(parsed)
        }
        _ => Ok(Input::default()),
    }
}

fn expand_home(path: &str) -> Result<PathBuf> {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var("HOME").context("HOME not set in environment")?;
        return Ok(PathBuf::from(home).join(rest));
    }
    Ok(PathBuf::from(path))
}

const TMUX_MARKER_START: &str = "# >>> runloop:system_tra";
const TMUX_MARKER_END: &str = "# <<< runloop:system_tra";

fn render_tmux_block(history_limit: u32, extra_lines: &[String]) -> String {
    let mut lines = Vec::new();
    lines.push(format!("{TMUX_MARKER_START} history_limit={history_limit}"));
    lines.push("# managed by the system_tra agent".into());
    lines.push(format!("set -g history-limit {history_limit}"));
    if !extra_lines.is_empty() {
        lines.push("# extra tmux lines from params:".into());
        lines.extend(extra_lines.iter().cloned());
    }
    lines.push(TMUX_MARKER_END.into());
    lines.join("\n") + "\n"
}

fn upsert_tmux_block(existing: &str, block: &str) -> String {
    if let Some(start) = existing.find(TMUX_MARKER_START) {
        if let Some(end_rel) = existing[start..].find(TMUX_MARKER_END) {
            let end = start + end_rel + TMUX_MARKER_END.len();
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

fn upsert_history_limits(
    existing: &str,
    hist_size: u32,
    hist_file_size: u32,
    tag: &str,
) -> String {
    let mut kept: Vec<String> = existing
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !(trimmed.starts_with("HISTSIZE=") || trimmed.starts_with("HISTFILESIZE="))
        })
        .map(|s| s.to_owned())
        .collect();

    if !kept.is_empty() && kept.last().is_some_and(|line| !line.is_empty()) {
        kept.push(String::new());
    }

    kept.push(tag.to_owned());
    kept.push(format!("HISTSIZE={hist_size}"));
    kept.push(format!("HISTFILESIZE={hist_file_size}"));
    kept.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_empty_input() {
        let input = parse_input(None).unwrap();
        assert!(input.tmux_conf.is_none());
    }

    #[test]
    fn renders_and_upserts_tmux_block() {
        let block = render_tmux_block(42, &["set -g mouse on".into()]);
        assert!(block.contains("history_limit=42"));

        let merged = upsert_tmux_block("set -g base-index 1\n", &block);
        assert!(merged.contains(TMUX_MARKER_START));
        let merged_again = upsert_tmux_block(&merged, &block);
        assert_eq!(merged, merged_again);
    }

    #[test]
    fn updates_history_limits() {
        let existing = "export PATH=/bin\nHISTSIZE=10\n# keep above\nHISTFILESIZE=10\n";
        let updated = upsert_history_limits(existing, 100, 200, "# managed");
        assert!(updated.contains("HISTSIZE=100"));
        assert!(updated.contains("HISTFILESIZE=200"));
        assert!(updated.ends_with('\n'));
        assert!(!updated.contains("HISTSIZE=10\n"));
        assert!(!updated.contains("HISTFILESIZE=10\n"));
    }
}
