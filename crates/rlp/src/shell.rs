use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::ValueEnum;
use dirs::home_dir;

const USER_SHELL_DIR: &str = ".runloop/shell";
const ZSH_SNIPPET: &str = "runloop.zsh";
const BASH_SNIPPET: &str = "runloop.bash";

const MARK_ZSH_START: &str = "# >>> runloop shell (zsh) >>>";
const MARK_ZSH_END: &str = "# <<< runloop shell (zsh) <<<";
const MARK_BASH_START: &str = "# >>> runloop shell (bash) >>>";
const MARK_BASH_END: &str = "# <<< runloop shell (bash) <<<";

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum ShellFlavor {
    Zsh,
    Bash,
}

#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    #[error("failed to resolve home directory for rc file")]
    MissingHome,
    #[error("snippet not found; tried: {0}")]
    SnippetNotFound(String),
    #[error("opening file '{0}' does not exist")]
    MissingOpening(PathBuf),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellAction {
    Added,
    Removed,
    AlreadyPresent,
    NotFound,
}

#[derive(Debug)]
pub struct ShellEditResult {
    pub rc_path: PathBuf,
    pub snippet_path: Option<PathBuf>,
    pub action: ShellAction,
    pub dry_run: bool,
    pub notes: Vec<String>,
}

pub struct EnableRequest {
    pub flavor: ShellFlavor,
    pub rc_path: Option<PathBuf>,
    pub snippet_override: Option<PathBuf>,
    pub opening_path: Option<PathBuf>,
    pub dry_run: bool,
}

pub struct DisableRequest {
    pub flavor: ShellFlavor,
    pub rc_path: Option<PathBuf>,
    pub dry_run: bool,
}

pub fn enable(request: EnableRequest) -> Result<ShellEditResult, ShellError> {
    let rc_path = resolve_rc_path(request.flavor, request.rc_path)?;
    let snippet_path = resolve_snippet_path(request.flavor, request.snippet_override.as_deref())?;
    let opening_path = match request.opening_path {
        Some(ref path) => {
            let expanded = expand_path(path)?;
            if !expanded.exists() {
                return Err(ShellError::MissingOpening(expanded));
            }
            let canonical = fs::canonicalize(&expanded)
                .map_err(|_| ShellError::MissingOpening(expanded.clone()))?;
            Some(canonical)
        }
        None => None,
    };

    let (start_marker, end_marker) = markers(request.flavor);
    let mut notes = Vec::new();
    let contents = fs::read_to_string(&rc_path).unwrap_or_default();
    if contents.contains(start_marker) && contents.contains(end_marker) {
        notes.push(format!(
            "runloop shell block already present in {}",
            rc_path.display()
        ));
        return Ok(ShellEditResult {
            rc_path,
            snippet_path: Some(snippet_path),
            action: ShellAction::AlreadyPresent,
            dry_run: request.dry_run,
            notes,
        });
    }

    let block = build_block(request.flavor, &snippet_path, opening_path.as_deref());

    if request.dry_run {
        notes.push("dry-run: no changes applied".into());
        return Ok(ShellEditResult {
            rc_path,
            snippet_path: Some(snippet_path),
            action: ShellAction::Added,
            dry_run: true,
            notes,
        });
    }

    ensure_parent_dir(&rc_path)?;
    backup_file(&rc_path)?;
    let mut new_contents = contents;
    if !new_contents.is_empty() && !new_contents.ends_with('\n') {
        new_contents.push('\n');
    }
    if !new_contents.is_empty() {
        new_contents.push('\n');
    }
    new_contents.push_str(&block);
    if !new_contents.ends_with('\n') {
        new_contents.push('\n');
    }
    fs::write(&rc_path, new_contents)?;
    notes.push(format!(
        "added runloop shell block referencing {}",
        snippet_path.display()
    ));
    Ok(ShellEditResult {
        rc_path,
        snippet_path: Some(snippet_path),
        action: ShellAction::Added,
        dry_run: false,
        notes,
    })
}

pub fn disable(request: DisableRequest) -> Result<ShellEditResult, ShellError> {
    let rc_path = resolve_rc_path(request.flavor, request.rc_path)?;
    let (start_marker, end_marker) = markers(request.flavor);
    let contents = fs::read_to_string(&rc_path).unwrap_or_default();
    let mut notes = Vec::new();
    if contents.is_empty() {
        notes.push("rc file empty or missing; nothing to disable".into());
        return Ok(ShellEditResult {
            rc_path,
            snippet_path: None,
            action: ShellAction::NotFound,
            dry_run: request.dry_run,
            notes,
        });
    }

    if let Some((start_idx, end_idx)) = locate_block(&contents, start_marker, end_marker) {
        if request.dry_run {
            notes.push("dry-run: block would be removed".into());
            return Ok(ShellEditResult {
                rc_path,
                snippet_path: None,
                action: ShellAction::Removed,
                dry_run: true,
                notes,
            });
        }
        backup_file(&rc_path)?;
        let mut new_contents = String::with_capacity(contents.len());
        new_contents.push_str(&contents[..start_idx]);
        new_contents.push_str(&contents[end_idx..]);
        let trimmed = new_contents.trim_end_matches(['\n', ' ']);
        let mut final_contents = trimmed.to_string();
        if !final_contents.is_empty() {
            final_contents.push('\n');
        }
        fs::write(&rc_path, final_contents)?;
        notes.push("removed runloop shell block".into());
        return Ok(ShellEditResult {
            rc_path,
            snippet_path: None,
            action: ShellAction::Removed,
            dry_run: false,
            notes,
        });
    }

    notes.push("runloop shell block not found".into());
    Ok(ShellEditResult {
        rc_path,
        snippet_path: None,
        action: ShellAction::NotFound,
        dry_run: request.dry_run,
        notes,
    })
}

fn resolve_rc_path(
    flavor: ShellFlavor,
    override_path: Option<PathBuf>,
) -> Result<PathBuf, ShellError> {
    if let Some(path) = override_path {
        return expand_path(&path);
    }
    let home = home_dir().ok_or(ShellError::MissingHome)?;
    let rc = match flavor {
        ShellFlavor::Zsh => home.join(".zshrc"),
        ShellFlavor::Bash => home.join(".bashrc"),
    };
    Ok(rc)
}

fn resolve_snippet_path(
    flavor: ShellFlavor,
    override_path: Option<&Path>,
) -> Result<PathBuf, ShellError> {
    if let Some(path) = override_path {
        let expanded = expand_path(path)?;
        let canonical = fs::canonicalize(&expanded)
            .map_err(|_| ShellError::SnippetNotFound(expanded.display().to_string()))?;
        return Ok(canonical);
    }

    let mut candidates = default_snippet_candidates(flavor);
    if let Ok(cwd) = std::env::current_dir() {
        let repo_path = cwd
            .join("packaging")
            .join("shell")
            .join(snippet_name(flavor));
        candidates.push(repo_path);
    }
    let mut tried = Vec::new();
    for candidate in candidates {
        tried.push(candidate.display().to_string());
        if let Ok(canonical) = fs::canonicalize(&candidate) {
            return Ok(canonical);
        }
    }
    Err(ShellError::SnippetNotFound(tried.join(", ")))
}

fn default_snippet_candidates(flavor: ShellFlavor) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("/usr/share/runloop/shell").join(snippet_name(flavor))];
    paths.push(PathBuf::from("/usr/local/share/runloop/shell").join(snippet_name(flavor)));
    if let Some(home) = home_dir() {
        paths.push(home.join(USER_SHELL_DIR).join(snippet_name(flavor)));
    }
    paths
}

fn snippet_name(flavor: ShellFlavor) -> &'static str {
    match flavor {
        ShellFlavor::Zsh => ZSH_SNIPPET,
        ShellFlavor::Bash => BASH_SNIPPET,
    }
}

fn expand_path(path: &Path) -> Result<PathBuf, ShellError> {
    if let Some(stripped) = path.to_str().and_then(|s| s.strip_prefix('~')) {
        let home = home_dir().ok_or(ShellError::MissingHome)?;
        if stripped.is_empty() {
            return Ok(home);
        }
        if let Some(rest) = stripped.strip_prefix('/') {
            if rest.is_empty() {
                return Ok(home);
            }
            return Ok(home.join(rest));
        }
    }
    Ok(path.to_path_buf())
}

fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn build_block(flavor: ShellFlavor, snippet_path: &Path, opening: Option<&Path>) -> String {
    let (start_marker, end_marker) = markers(flavor);
    let mut block = String::new();
    block.push_str(start_marker);
    block.push('\n');
    if let Some(path) = opening {
        block.push_str(&format!(
            "export RUNLOOP_ROUTER_OPENING_PATH={}\n",
            single_quote(&path.display().to_string())
        ));
    }
    block.push_str(&format!(
        "source {}\n",
        single_quote(&snippet_path.display().to_string())
    ));
    block.push_str(end_marker);
    block.push('\n');
    block
}

fn markers(flavor: ShellFlavor) -> (&'static str, &'static str) {
    match flavor {
        ShellFlavor::Zsh => (MARK_ZSH_START, MARK_ZSH_END),
        ShellFlavor::Bash => (MARK_BASH_START, MARK_BASH_END),
    }
}

fn locate_block(contents: &str, start: &str, end: &str) -> Option<(usize, usize)> {
    let start_idx = contents.find(start)?;
    let rest = &contents[start_idx..];
    let end_rel = rest.find(end)?;
    let end_idx = start_idx + end_rel + end.len();
    let mut final_end = end_idx;
    if contents
        .get(end_idx..)
        .is_some_and(|slice| slice.starts_with('\n'))
    {
        final_end += 1;
    }
    Some((start_idx, final_end))
}

fn backup_file(path: &Path) -> io::Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let backup_name = format!(
        "{}.runloop.bak.{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("rc"),
        timestamp
    );
    let backup = path.with_file_name(backup_name);
    fs::copy(path, &backup)?;
    Ok(Some(backup))
}

fn single_quote(value: &str) -> String {
    let mut quoted = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::tempdir;

    static CWD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    #[test]
    fn enable_inserts_block() {
        let dir = tempdir().unwrap();
        let rc_path = dir.path().join(".testrc");
        let snippet = dir.path().join("runloop.zsh");
        fs::write(&snippet, "echo snippet").unwrap();
        let result = enable(EnableRequest {
            flavor: ShellFlavor::Zsh,
            rc_path: Some(rc_path.clone()),
            snippet_override: Some(snippet.clone()),
            opening_path: None,
            dry_run: false,
        })
        .unwrap();
        assert_eq!(result.action, ShellAction::Added);
        let canonical = fs::canonicalize(&snippet).unwrap();
        assert_eq!(result.snippet_path.as_deref(), Some(canonical.as_path()));
        let contents = fs::read_to_string(rc_path).unwrap();
        assert!(contents.contains(MARK_ZSH_START));
        assert!(contents.contains(&canonical.display().to_string()));
    }

    #[test]
    fn enable_canonicalizes_relative_path() {
        let dir = tempdir().unwrap();
        let rc_path = dir.path().join(".testrc");
        let snippet_dir = dir.path().join("snippets");
        fs::create_dir_all(&snippet_dir).unwrap();
        let snippet = snippet_dir.join("runloop.zsh");
        fs::write(&snippet, "echo snippet").unwrap();
        let _guard = DirGuard::new(dir.path());
        let relative = PathBuf::from("snippets/runloop.zsh");
        let result = enable(EnableRequest {
            flavor: ShellFlavor::Zsh,
            rc_path: Some(rc_path.clone()),
            snippet_override: Some(relative),
            opening_path: None,
            dry_run: false,
        })
        .unwrap();
        let canonical = fs::canonicalize(&snippet).unwrap();
        assert_eq!(result.snippet_path.as_deref(), Some(canonical.as_path()));
        let contents = fs::read_to_string(rc_path).unwrap();
        assert!(contents.contains(&canonical.display().to_string()));
    }

    #[test]
    fn enable_canonicalizes_opening_path() {
        let dir = tempdir().unwrap();
        let rc_path = dir.path().join(".testrc");
        let snippet = dir.path().join("runloop.zsh");
        fs::write(&snippet, "echo snippet").unwrap();
        let openings_dir = dir.path().join("openings");
        fs::create_dir_all(&openings_dir).unwrap();
        let opening = openings_dir.join("router.yaml");
        fs::write(&opening, "id: router").unwrap();
        let _guard = DirGuard::new(dir.path());
        let relative_opening = PathBuf::from("openings/router.yaml");
        let result = enable(EnableRequest {
            flavor: ShellFlavor::Zsh,
            rc_path: Some(rc_path.clone()),
            snippet_override: Some(PathBuf::from("runloop.zsh")),
            opening_path: Some(relative_opening),
            dry_run: false,
        })
        .unwrap();
        assert_eq!(result.action, ShellAction::Added);
        let canonical_opening = fs::canonicalize(&opening).unwrap();
        let contents = fs::read_to_string(rc_path).unwrap();
        assert!(contents.contains(&canonical_opening.display().to_string()));
    }

    #[test]
    fn disable_removes_block() {
        let dir = tempdir().unwrap();
        let rc_path = dir.path().join(".testrc");
        let snippet = dir.path().join("runloop.zsh");
        fs::write(&snippet, "echo snippet").unwrap();
        enable(EnableRequest {
            flavor: ShellFlavor::Zsh,
            rc_path: Some(rc_path.clone()),
            snippet_override: Some(snippet),
            opening_path: None,
            dry_run: false,
        })
        .unwrap();
        let result = disable(DisableRequest {
            flavor: ShellFlavor::Zsh,
            rc_path: Some(rc_path.clone()),
            dry_run: false,
        })
        .unwrap();
        assert_eq!(result.action, ShellAction::Removed);
        let contents = fs::read_to_string(rc_path).unwrap();
        assert!(!contents.contains(MARK_ZSH_START));
    }
    struct DirGuard {
        original: PathBuf,
        _lock: MutexGuard<'static, ()>,
    }

    impl DirGuard {
        fn new(path: &Path) -> Self {
            let lock = CWD_LOCK
                .get_or_init(|| Mutex::new(()))
                .lock()
                .expect("cwd mutex poisoned");
            let original = env::current_dir().unwrap();
            env::set_current_dir(path).unwrap();
            Self {
                original,
                _lock: lock,
            }
        }
    }

    impl Drop for DirGuard {
        fn drop(&mut self) {
            let _ = env::set_current_dir(&self.original);
        }
    }
}
