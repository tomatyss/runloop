use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::ValueEnum;
use dirs::home_dir;

const USER_SHELL_DIR: &str = ".runloop/shell";
const USER_OPENING_DEFAULT: &str = ".runloop/openings/router-default.yaml";
const ZSH_SNIPPET: &str = "runloop.zsh";
const BASH_SNIPPET: &str = "runloop.bash";

const MARK_ZSH_START: &str = "# >>> runloop shell (zsh) >>>";
const MARK_ZSH_END: &str = "# <<< runloop shell (zsh) <<<";
const MARK_BASH_START: &str = "# >>> runloop shell (bash) >>>";
const MARK_BASH_END: &str = "# <<< runloop shell (bash) <<<";

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum ShellCliFlavor {
    Zsh,
    Bash,
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellFlavor {
    Zsh,
    Bash,
}

impl ShellCliFlavor {
    pub fn resolve(self) -> ShellFlavor {
        match self {
            ShellCliFlavor::Zsh => ShellFlavor::Zsh,
            ShellCliFlavor::Bash => ShellFlavor::Bash,
            ShellCliFlavor::Auto => detect_shell_from_env(),
        }
    }
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
    Updated,
    Removed,
    AlreadyPresent,
    NotFound,
}

#[derive(Debug)]
pub struct ShellEditResult {
    pub rc_path: PathBuf,
    pub snippet_path: Option<PathBuf>,
    pub opening_path: Option<PathBuf>,
    pub block_preview: Option<String>,
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

    let (start_marker, end_marker) = markers(request.flavor);
    let contents = fs::read_to_string(&rc_path).unwrap_or_default();
    let mut notes = Vec::new();

    let opening_path = resolve_opening_path(
        request.opening_path.as_deref(),
        &mut notes,
        !request.dry_run,
    )?;
    let block = build_block(request.flavor, &snippet_path, opening_path.as_deref());

    if let Some((start_idx, end_idx)) = locate_block(&contents, start_marker, end_marker) {
        let existing_block = contents.get(start_idx..end_idx).unwrap_or_default();
        if blocks_semantically_equivalent(existing_block, &snippet_path, opening_path.as_deref())
            || blocks_equivalent(existing_block, &block)
        {
            notes.push(format!(
                "runloop shell block already present in {}",
                rc_path.display()
            ));
            return Ok(ShellEditResult {
                rc_path,
                snippet_path: Some(snippet_path),
                opening_path,
                block_preview: Some(block),
                action: ShellAction::AlreadyPresent,
                dry_run: request.dry_run,
                notes,
            });
        }

        if request.dry_run {
            notes.push("dry-run: existing block would be replaced".into());
            return Ok(ShellEditResult {
                rc_path,
                snippet_path: Some(snippet_path),
                opening_path,
                block_preview: Some(block),
                action: ShellAction::Updated,
                dry_run: true,
                notes,
            });
        }

        ensure_parent_dir(&rc_path)?;
        backup_file(&rc_path)?;
        let mut new_contents = String::with_capacity(contents.len() + block.len() + 2);
        new_contents.push_str(&contents[..start_idx]);
        if !new_contents.ends_with(['\n', '\r']) && !new_contents.is_empty() {
            new_contents.push('\n');
        }
        new_contents.push_str(&block);
        if !new_contents.ends_with('\n') {
            new_contents.push('\n');
        }
        let tail = &contents[end_idx..];
        if !tail.is_empty() && !new_contents.ends_with('\n') {
            new_contents.push('\n');
        }
        new_contents.push_str(tail);
        fs::write(&rc_path, new_contents)?;
        notes.push("replaced existing runloop shell block".into());
        return Ok(ShellEditResult {
            rc_path,
            snippet_path: Some(snippet_path),
            opening_path,
            block_preview: Some(block),
            action: ShellAction::Updated,
            dry_run: false,
            notes,
        });
    }

    if request.dry_run {
        notes.push("dry-run: no changes applied".into());
        return Ok(ShellEditResult {
            rc_path,
            snippet_path: Some(snippet_path),
            opening_path,
            block_preview: Some(block),
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
        opening_path,
        block_preview: Some(block),
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
            opening_path: None,
            block_preview: None,
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
                opening_path: None,
                block_preview: None,
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
            opening_path: None,
            block_preview: None,
            action: ShellAction::Removed,
            dry_run: false,
            notes,
        });
    }

    notes.push("runloop shell block not found".into());
    Ok(ShellEditResult {
        rc_path,
        snippet_path: None,
        opening_path: None,
        block_preview: None,
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
    paths.push(PathBuf::from("/opt/homebrew/share/runloop/shell").join(snippet_name(flavor)));
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

fn resolve_opening_path(
    explicit: Option<&Path>,
    notes: &mut Vec<String>,
    allow_create: bool,
) -> Result<Option<PathBuf>, ShellError> {
    if let Some(path) = explicit {
        let expanded = expand_path(path)?;
        if !expanded.exists() {
            return Err(ShellError::MissingOpening(expanded));
        }
        let canonical = fs::canonicalize(&expanded)
            .map_err(|_| ShellError::MissingOpening(expanded.clone()))?;
        return Ok(Some(canonical));
    }

    let Some(home) = home_dir() else {
        return Err(ShellError::MissingHome);
    };
    let user_default = home.join(USER_OPENING_DEFAULT);
    if user_default.exists() {
        let canonical = fs::canonicalize(&user_default)
            .map_err(|_| ShellError::MissingOpening(user_default.clone()))?;
        notes.push(format!(
            "using existing router-default opening at {}",
            canonical.display()
        ));
        return Ok(Some(canonical));
    }

    if let Some(source) = default_opening_source()? {
        if allow_create {
            if let Err(err) = ensure_parent_dir(&user_default) {
                notes.push(format!(
                    "failed to create {}: {}; RUNLOOP_ROUTER_OPENING_PATH not set",
                    user_default.display(),
                    err
                ));
                return Ok(None);
            }
            if let Err(err) = fs::copy(&source, &user_default) {
                notes.push(format!(
                    "failed to copy {} to {}: {}; RUNLOOP_ROUTER_OPENING_PATH not set",
                    source.display(),
                    user_default.display(),
                    err
                ));
                return Ok(None);
            }
            let canonical = fs::canonicalize(&user_default)
                .map_err(|_| ShellError::MissingOpening(user_default.clone()))?;
            notes.push(format!(
                "copied router-default opening to {}",
                canonical.display()
            ));
            return Ok(Some(canonical));
        } else {
            notes.push(format!(
                "would copy router-default opening from {} to {}",
                source.display(),
                user_default.display()
            ));
            return Ok(Some(user_default));
        }
    }

    notes.push(
        "router-default opening not found; RUNLOOP_ROUTER_OPENING_PATH will not be set".into(),
    );
    Ok(None)
}

fn default_opening_source() -> Result<Option<PathBuf>, ShellError> {
    let mut candidates = vec![
        PathBuf::from("/usr/share/runloop/openings/router-default.yaml"),
        PathBuf::from("/usr/local/share/runloop/openings/router-default.yaml"),
        PathBuf::from("/opt/homebrew/share/runloop/openings/router-default.yaml"),
    ];
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(
            cwd.join("packaging")
                .join("openings")
                .join("router-default.yaml"),
        );
    }
    for candidate in candidates {
        if let Ok(canonical) = fs::canonicalize(&candidate) {
            return Ok(Some(canonical));
        }
    }
    Ok(None)
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

fn blocks_equivalent(existing: &str, desired: &str) -> bool {
    existing.trim_end_matches(['\n', ' ']) == desired.trim_end_matches(['\n', ' '])
}

fn blocks_semantically_equivalent(
    existing: &str,
    desired_snippet: &Path,
    desired_opening: Option<&Path>,
) -> bool {
    if let Some((existing_opening, existing_snippet)) = parse_block_paths(existing) {
        if normalize_path(&existing_snippet) != normalize_path(desired_snippet) {
            return false;
        }
        match (existing_opening, desired_opening) {
            (None, None) => return true,
            (Some(e), Some(d)) => return normalize_path(&e) == normalize_path(d),
            _ => return false,
        }
    }
    false
}

fn parse_block_paths(block: &str) -> Option<(Option<PathBuf>, PathBuf)> {
    let mut opening: Option<PathBuf> = None;
    let mut snippet: Option<PathBuf> = None;
    for line in block.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("export RUNLOOP_ROUTER_OPENING_PATH=") {
            let val = trimmed.splitn(2, '=').nth(1)?;
            let unquoted = unquote_single(val.trim());
            let expanded = expand_path(Path::new(&unquoted)).ok()?;
            let normalized = normalize_path(&expanded);
            opening = Some(normalized);
        } else if trimmed.starts_with("source ") {
            let val = trimmed.trim_start_matches("source ").trim();
            let unquoted = unquote_single(val);
            let expanded = expand_path(Path::new(&unquoted)).ok()?;
            let normalized = normalize_path(&expanded);
            snippet = Some(normalized);
        }
    }
    snippet.map(|s| (opening, s))
}

fn normalize_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn markers(flavor: ShellFlavor) -> (&'static str, &'static str) {
    match flavor {
        ShellFlavor::Zsh => (MARK_ZSH_START, MARK_ZSH_END),
        ShellFlavor::Bash => (MARK_BASH_START, MARK_BASH_END),
    }
}

fn detect_shell_from_env() -> ShellFlavor {
    let shell = std::env::var("SHELL").unwrap_or_default();
    if shell.contains("bash") {
        ShellFlavor::Bash
    } else {
        ShellFlavor::Zsh
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

fn unquote_single(value: &str) -> String {
    let mut s = value.trim_matches('\'').to_string();
    s = s.replace("'\\''", "'");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::tempdir;

    static CWD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    #[test]
    fn enable_inserts_block() {
        let dir = tempdir().unwrap();
        let rc_path = dir.path().join(".testrc");
        let snippet = dir.path().join("runloop.zsh");
        fs::write(&snippet, "echo snippet").unwrap();
        let opening = dir.path().join("router.yaml");
        fs::write(&opening, "id: router").unwrap();
        let result = enable(EnableRequest {
            flavor: ShellFlavor::Zsh,
            rc_path: Some(rc_path.clone()),
            snippet_override: Some(snippet.clone()),
            opening_path: Some(opening),
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
        let opening = dir.path().join("openings/router.yaml");
        fs::create_dir_all(opening.parent().unwrap()).unwrap();
        fs::write(&opening, "id: router").unwrap();
        let _guard = DirGuard::new(dir.path());
        let relative = PathBuf::from("snippets/runloop.zsh");
        let result = enable(EnableRequest {
            flavor: ShellFlavor::Zsh,
            rc_path: Some(rc_path.clone()),
            snippet_override: Some(relative),
            opening_path: Some(PathBuf::from("openings/router.yaml")),
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
        let opening = dir.path().join("router.yaml");
        fs::write(&opening, "id: router").unwrap();
        enable(EnableRequest {
            flavor: ShellFlavor::Zsh,
            rc_path: Some(rc_path.clone()),
            snippet_override: Some(snippet),
            opening_path: Some(opening),
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

    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn new(key: &'static str, value: &Path) -> Self {
            let lock = ENV_LOCK
                .get_or_init(|| Mutex::new(()))
                .lock()
                .expect("env mutex poisoned");
            let previous = env::var_os(key);
            unsafe {
                env::set_var(key, value);
            }
            Self {
                key,
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(val) => unsafe { env::set_var(self.key, val) },
                None => unsafe { env::remove_var(self.key) },
            }
        }
    }

    #[test]
    fn enable_copies_packaged_opening_when_missing() {
        let dir = tempdir().unwrap();
        let _cwd = DirGuard::new(dir.path());
        let _home = EnvGuard::new("HOME", dir.path());

        let packaging = dir.path().join("packaging/openings");
        fs::create_dir_all(&packaging).unwrap();
        let packaged_opening = packaging.join("router-default.yaml");
        fs::write(&packaged_opening, "id: router").unwrap();

        let snippet = dir.path().join("runloop.zsh");
        fs::write(&snippet, "echo snippet").unwrap();

        let result = enable(EnableRequest {
            flavor: ShellFlavor::Zsh,
            rc_path: None,
            snippet_override: Some(snippet),
            opening_path: None,
            dry_run: false,
        })
        .unwrap();

        let user_copy = dir.path().join(".runloop/openings/router-default.yaml");
        assert!(user_copy.exists());
        let user_contents = fs::read_to_string(user_copy).unwrap();
        assert_eq!(user_contents, "id: router");
        assert_eq!(result.action, ShellAction::Added);
    }

    #[test]
    fn enable_dry_run_does_not_copy_opening() {
        let dir = tempdir().unwrap();
        let _cwd = DirGuard::new(dir.path());
        let _home = EnvGuard::new("HOME", dir.path());

        let packaging = dir.path().join("packaging/openings");
        fs::create_dir_all(&packaging).unwrap();
        let packaged_opening = packaging.join("router-default.yaml");
        fs::write(&packaged_opening, "id: router").unwrap();

        let snippet = dir.path().join("runloop.zsh");
        fs::write(&snippet, "echo snippet").unwrap();

        let result = enable(EnableRequest {
            flavor: ShellFlavor::Zsh,
            rc_path: None,
            snippet_override: Some(snippet),
            opening_path: None,
            dry_run: true,
        })
        .unwrap();

        let user_copy = dir.path().join(".runloop/openings/router-default.yaml");
        assert!(
            !user_copy.exists(),
            "dry-run should not create opening copy"
        );
        assert_eq!(result.action, ShellAction::Added);
        assert!(result.dry_run);
    }

    #[test]
    fn enable_replaces_existing_block_when_paths_change() {
        let dir = tempdir().unwrap();
        let rc_path = dir.path().join(".testrc");

        let old_snippet = dir.path().join("old/runloop.zsh");
        fs::create_dir_all(old_snippet.parent().unwrap()).unwrap();
        fs::write(&old_snippet, "echo old").unwrap();

        let new_snippet = dir.path().join("new/runloop.zsh");
        fs::create_dir_all(new_snippet.parent().unwrap()).unwrap();
        fs::write(&new_snippet, "echo new").unwrap();

        let opening = dir.path().join("router.yaml");
        fs::write(&opening, "id: router").unwrap();

        let old_block = format!(
            "{}\nsource '{}'\n{}\n",
            MARK_ZSH_START,
            old_snippet.display(),
            MARK_ZSH_END
        );
        fs::write(&rc_path, old_block).unwrap();

        let result = enable(EnableRequest {
            flavor: ShellFlavor::Zsh,
            rc_path: Some(rc_path.clone()),
            snippet_override: Some(new_snippet.clone()),
            opening_path: Some(opening),
            dry_run: false,
        })
        .unwrap();

        assert_eq!(result.action, ShellAction::Updated);
        let contents = fs::read_to_string(rc_path).unwrap();
        let new_path = fs::canonicalize(&new_snippet).unwrap();
        assert!(contents.contains(&new_path.display().to_string()));
        assert_eq!(contents.match_indices(MARK_ZSH_START).count(), 1);
        assert!(!contents.contains(&old_snippet.display().to_string()));
    }

    #[test]
    fn enable_dry_run_includes_block_preview() {
        let dir = tempdir().unwrap();
        let rc_path = dir.path().join(".testrc");
        let snippet = dir.path().join("runloop.zsh");
        fs::write(&snippet, "echo snippet").unwrap();
        let opening = dir.path().join("router.yaml");
        fs::write(&opening, "id: router").unwrap();

        let result = enable(EnableRequest {
            flavor: ShellFlavor::Zsh,
            rc_path: Some(rc_path),
            snippet_override: Some(snippet.clone()),
            opening_path: Some(opening.clone()),
            dry_run: true,
        })
        .unwrap();

        let preview = result.block_preview.as_ref().expect("preview missing");
        let canonical_snippet = fs::canonicalize(&snippet).unwrap();
        assert!(preview.contains(MARK_ZSH_START));
        assert!(preview.contains(&canonical_snippet.display().to_string()));
        let canonical_opening = fs::canonicalize(&opening).unwrap();
        assert!(preview.contains(&canonical_opening.display().to_string()));
        assert!(result.dry_run);
    }

    #[test]
    fn enable_preserves_block_when_paths_equivalent() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let _home = EnvGuard::new("HOME", &home);

        let rc_path = home.join(".zshrc");
        let shell_dir = home.join(".runloop/shell");
        fs::create_dir_all(&shell_dir).unwrap();
        let snippet = shell_dir.join("runloop.zsh");
        fs::write(&snippet, "echo snippet").unwrap();

        let openings_dir = home.join(".runloop/openings");
        fs::create_dir_all(&openings_dir).unwrap();
        let opening = openings_dir.join("router.yaml");
        fs::write(&opening, "id: router").unwrap();

        let block = format!(
            "{}\nexport RUNLOOP_ROUTER_OPENING_PATH='~/.runloop/openings/router.yaml'\nsource '~/.runloop/shell/runloop.zsh'\n{}\n",
            MARK_ZSH_START, MARK_ZSH_END
        );
        fs::write(&rc_path, block).unwrap();

        let result = enable(EnableRequest {
            flavor: ShellFlavor::Zsh,
            rc_path: Some(rc_path.clone()),
            snippet_override: Some(snippet.clone()),
            opening_path: Some(opening.clone()),
            dry_run: false,
        })
        .unwrap();

        assert_eq!(result.action, ShellAction::AlreadyPresent);
        let contents = fs::read_to_string(rc_path).unwrap();
        // Ensure we didn't duplicate blocks
        assert_eq!(contents.match_indices(MARK_ZSH_START).count(), 1);
    }

    #[test]
    fn shell_auto_resolves_from_env() {
        {
            let _shell = EnvGuard::new("SHELL", Path::new("/bin/bash"));
            assert_eq!(ShellCliFlavor::Auto.resolve(), ShellFlavor::Bash);
        }
        {
            let _shell = EnvGuard::new("SHELL", Path::new("/bin/zsh"));
            assert_eq!(ShellCliFlavor::Auto.resolve(), ShellFlavor::Zsh);
        }
    }

    #[test]
    fn enable_warns_when_opening_copy_dir_missing() {
        let dir = tempdir().unwrap();
        let _cwd = DirGuard::new(dir.path());
        let _home = EnvGuard::new("HOME", dir.path());

        // make ~/.runloop a file to block directory creation
        let obstruct = dir.path().join(".runloop");
        fs::write(&obstruct, "no dir").unwrap();

        let packaging = dir.path().join("packaging/openings");
        fs::create_dir_all(&packaging).unwrap();
        let packaged_opening = packaging.join("router-default.yaml");
        fs::write(&packaged_opening, "id: router").unwrap();

        let snippet = dir.path().join("runloop.zsh");
        fs::write(&snippet, "echo snippet").unwrap();

        let result = enable(EnableRequest {
            flavor: ShellFlavor::Zsh,
            rc_path: None,
            snippet_override: Some(snippet.clone()),
            opening_path: None,
            dry_run: false,
        })
        .unwrap();

        assert_eq!(result.action, ShellAction::Added);
        assert!(result.opening_path.is_none());
        assert!(result.notes.iter().any(|n| n.contains("failed to create")));
        let contents = fs::read_to_string(dir.path().join(".zshrc")).unwrap();
        assert!(!contents.contains("RUNLOOP_ROUTER_OPENING_PATH"));
        let canonical_snippet = fs::canonicalize(&snippet).unwrap();
        assert!(contents.contains(&canonical_snippet.display().to_string()));
    }

    #[test]
    fn default_snippet_candidates_include_homebrew_prefix() {
        let paths = default_snippet_candidates(ShellFlavor::Bash);
        let expected = PathBuf::from("/opt/homebrew/share/runloop/shell")
            .join(snippet_name(ShellFlavor::Bash));
        assert!(
            paths.contains(&expected),
            "expected Homebrew share path to be included"
        );
    }

    #[test]
    fn default_snippet_candidates_include_usr_share() {
        let paths = default_snippet_candidates(ShellFlavor::Zsh);
        let expected =
            PathBuf::from("/usr/share/runloop/shell").join(snippet_name(ShellFlavor::Zsh));
        assert!(paths.contains(&expected));
    }
}
