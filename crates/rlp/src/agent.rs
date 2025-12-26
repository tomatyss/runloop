use crate::output::{Cell, OutputArgs, OutputMode, Table, print_json, print_table};
use clap::{Args, Subcommand};
use dirs::home_dir;
use flate2::read::GzDecoder;
use is_terminal::IsTerminal;
use runloop_agent_registry::{
    AgentRegistry, AgentRegistryError, Budget, Observability, ToolEntry, ToolsDoc, Transport,
    digest_file_hex, load_tools,
};
use runloop_core::{AgentRef, Config};
use runloop_registry::{PathOverrides, RegistryPaths, resolve_paths};
use serde_json::json;
use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use tar::{Archive, EntryType};
use tempfile::TempDir;
use thiserror::Error;
use toml_edit::{DocumentMut, value};
use url::Url;
use uuid::Uuid;

const ZERO_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Subcommand, Debug)]
pub enum AgentCommands {
    List(ListArgs),
    Scaffold(ScaffoldArgs),
    Build(BuildArgs),
    Install(InstallArgs),
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

#[derive(Args, Debug)]
pub struct BuildArgs {
    /// Agent name (snake_case).
    pub name: String,
    /// Root directory for agent bundles (defaults to workspace ./agents or ~/.runloop/agents).
    #[arg(long = "root", value_name = "PATH")]
    pub root_dir: Option<PathBuf>,
    /// Root directory for wasm agent crates (defaults to crates/agents-wasm or ~/.runloop/agents-wasm).
    #[arg(long = "crates-dir", value_name = "PATH")]
    pub crates_dir: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Root directory for agent bundles (overrides configured search dirs).
    #[arg(long = "root", value_name = "PATH")]
    pub root_dir: Option<PathBuf>,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Args, Debug)]
pub struct InstallArgs {
    /// Agent bundle path or file:// URL (directory, .tar, or .tar.gz).
    pub source: String,
    /// Root directory for installed bundles (defaults to first configured search dir).
    #[arg(long = "root", value_name = "PATH")]
    pub root_dir: Option<PathBuf>,
    /// Overwrite an existing bundle if present.
    #[arg(long)]
    pub force: bool,
    /// Skip digest and tools.json validation (not recommended).
    #[arg(long = "skip-verify")]
    pub skip_verify: bool,
}

#[derive(Debug)]
pub struct ScaffoldResult {
    pub agent_dir: PathBuf,
    pub crate_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub tools_path: PathBuf,
    pub opening_path: Option<PathBuf>,
}

#[derive(Debug)]
pub struct BuildResult {
    pub agent_dir: PathBuf,
    pub crate_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub wasm_path: PathBuf,
}

#[derive(Debug)]
pub struct InstallResult {
    pub agent_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub reference: AgentRef,
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error(
        "invalid agent name '{0}' (use lowercase letters, digits, and underscores; must start with a letter)"
    )]
    InvalidName(String),
    #[error("path already exists: {0}")]
    Exists(PathBuf),
    #[error("agent crate not found at {0}")]
    MissingCrate(PathBuf),
    #[error("manifest not found at {0}")]
    MissingManifest(PathBuf),
    #[error("wasm binary not found at {0}")]
    MissingBinary(PathBuf),
    #[error("manifest error: {0}")]
    Manifest(String),
    #[error("capabilities file error: {0}")]
    Caps(String),
    #[error("build failed: {0}")]
    BuildFailed(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Registry(#[from] AgentRegistryError),
    #[error("digest error: {0}")]
    Digest(String),
    #[error("invalid tools.json: {0}")]
    Tools(String),
    #[error("invalid bundle source: {0}")]
    Source(String),
    #[error("bundle validation failed: {0}")]
    Bundle(String),
    #[error("install failed: {0}")]
    Install(String),
    #[error("bundle signing required but signature verification is not available yet")]
    SignatureRequired,
}

pub fn handle_agent(
    cmd: AgentCommands,
    config: &Config,
    overrides: &PathOverrides,
) -> Result<(), AgentError> {
    let registry_paths = resolve_paths(config, overrides);
    match cmd {
        AgentCommands::List(args) => list_agents(args, config, overrides)?,
        AgentCommands::Scaffold(args) => {
            let result = scaffold(args, config, overrides, &registry_paths)?;
            println!("created agent bundle at {}", result.agent_dir.display());
            println!("created wasm crate at {}", result.crate_dir.display());
            println!("manifest: {}", result.manifest_path.display());
            println!("tools.json: {}", result.tools_path.display());
            if let Some(path) = &result.opening_path {
                println!("opening: {}", path.display());
            }
        }
        AgentCommands::Build(args) => {
            let result = build(args, config, overrides)?;
            println!("built wasm to {}", result.wasm_path.display());
            println!("updated manifest: {}", result.manifest_path.display());
            println!("agent bundle at {}", result.agent_dir.display());
            println!("wasm crate at {}", result.crate_dir.display());
        }
        AgentCommands::Install(args) => {
            let result = install(args, config, overrides, &registry_paths)?;
            println!(
                "installed {} to {}",
                result.reference,
                result.agent_dir.display()
            );
            println!("manifest: {}", result.manifest_path.display());
        }
    }
    Ok(())
}

fn list_agents(
    args: ListArgs,
    config: &Config,
    overrides: &PathOverrides,
) -> Result<(), AgentError> {
    let local_overrides = PathOverrides {
        agents_dir: args
            .root_dir
            .clone()
            .or_else(|| overrides.agents_dir.clone()),
        openings_dir: overrides.openings_dir.clone(),
        cwd: overrides.cwd.clone(),
        workspace_root: overrides.workspace_root.clone(),
    };
    let registry_paths = resolve_paths(config, &local_overrides);
    let search_dirs = registry_paths.agents.clone();
    let registry = AgentRegistry::new(search_dirs.clone());
    let listed = registry.list()?;
    let settings = args.output.resolve();
    for info in &registry_paths.info {
        eprintln!("info: {info}");
    }
    for warning in &registry_paths.warnings {
        eprintln!("warning: {warning}");
    }

    let audits = listed
        .iter()
        .map(|entry| {
            audit_bundle(
                &entry.described.reference,
                &entry.manifest_path,
                &search_dirs,
            )
        })
        .collect::<Vec<_>>();

    match settings.mode {
        OutputMode::Json => {
            let payload = json!({
                "agents": listed.iter().zip(audits.iter()).map(|(entry, audit)| {
                    json!({
                        "name": entry.described.reference.name,
                        "variant": entry.described.reference.variant,
                        "version": entry.described.version,
                        "digest": entry.described.digest,
                        "manifest": entry.manifest_path,
                        "status": audit.status,
                        "issues": audit.issues,
                        "source_dir": audit.source_dir,
                    })
                }).collect::<Vec<_>>(),
                "search_dirs": search_dirs,
            });
            print_json(&payload)?;
        }
        OutputMode::Table => {
            let mut table = Table::new(vec![
                "agent".into(),
                "variant".into(),
                "version".into(),
                "digest".into(),
                "status".into(),
                "source".into(),
                "manifest".into(),
            ]);
            for (entry, audit) in listed.iter().zip(audits.iter()) {
                table.add_row(vec![
                    Cell::text(&entry.described.reference.name),
                    Cell::text(
                        entry
                            .described
                            .reference
                            .variant
                            .clone()
                            .unwrap_or_default(),
                    ),
                    Cell::text(&entry.described.version),
                    Cell::text(short_digest(&entry.described.digest)),
                    Cell::text(&audit.status),
                    Cell::text(
                        audit
                            .source_dir
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                    ),
                    Cell::text(entry.manifest_path.display().to_string()),
                ]);
            }
            let missing_all = search_dirs.iter().all(|dir| !dir.exists());
            if listed.is_empty() {
                table.add_note(format!(
                    "no agent manifests found (searched: {})",
                    format_search_dirs(&search_dirs)
                ));
                if missing_all {
                    table.add_note(
                        "no search dirs exist; using demo bundles if packaged (set --agents-dir to add yours)",
                    );
                }
            } else {
                table.add_note(format!("searched: {}", format_search_dirs(&search_dirs)));
                table.add_note("digest = blake3(manifest.toml)");
                if audits.iter().any(|audit| !audit.issues.is_empty()) {
                    table.add_note("use --json to inspect bundle validation issues");
                }
            }
            print_table(&table, &settings)?;
        }
    }

    Ok(())
}

fn scaffold(
    args: ScaffoldArgs,
    config: &Config,
    overrides: &PathOverrides,
    registry_paths: &RegistryPaths,
) -> Result<ScaffoldResult, AgentError> {
    validate_name(&args.name)?;
    let interactive = !args.non_interactive && io::stdin().is_terminal();
    let answers = gather_answers(&args, config, interactive)?;
    let agent_root = resolve_agent_root(&args.root_dir, overrides.agents_dir.as_ref(), config);
    let crate_root = resolve_crate_root(&args.crates_dir);
    let agent_dir = agent_root.join(&args.name);
    let crate_dir = crate_root.join(&args.name);
    let default_opening_dir = default_opening_dir(registry_paths);
    let opening_path = if answers.generate_opening {
        Some(
            answers
                .opening_path
                .clone()
                .or_else(|| args.opening_path.clone())
                .or_else(|| default_opening_dir.map(|dir| dir.join(format!("{}.yaml", args.name))))
                .unwrap_or_else(|| PathBuf::from(format!("examples/openings/{}.yaml", args.name))),
        )
    } else {
        None
    };
    if !args.force {
        for path in [&agent_dir, &crate_dir] {
            if path.exists() {
                return Err(AgentError::Exists(path.to_path_buf()));
            }
        }
        if let Some(path) = &opening_path
            && path.exists()
        {
            return Err(AgentError::Exists(path.to_path_buf()));
        }
    }

    write_bundle(&answers, &agent_dir, &crate_dir)?;
    let tools_path = agent_dir.join("tools.json");
    write_tools(&answers.tools, &tools_path)?;
    let tools_digest =
        digest_file_hex(&tools_path).map_err(|err| AgentError::Digest(err.to_string()))?;
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

fn build(
    args: BuildArgs,
    config: &Config,
    overrides: &PathOverrides,
) -> Result<BuildResult, AgentError> {
    build_with_toolchain(args, config, overrides, "rustc", "cargo")
}

fn install(
    args: InstallArgs,
    config: &Config,
    overrides: &PathOverrides,
    registry_paths: &RegistryPaths,
) -> Result<InstallResult, AgentError> {
    if !config.security.allow_unsigned_agents {
        return Err(AgentError::SignatureRequired);
    }

    let (source_root, _temp) = load_bundle_source(&args.source)?;
    let (reference, bundle) = load_bundle(&source_root)?;
    validate_reference_path(&reference)?;
    let bundle_root = bundle.manifest_dir.clone();
    if !args.skip_verify {
        let issues = validate_bundle(&bundle);
        if !issues.is_empty() {
            return Err(AgentError::Bundle(format!(
                "{}: {}",
                reference.spec(),
                issues.join("; ")
            )));
        }
    }

    let mut install_root =
        resolve_agent_root(&args.root_dir, overrides.agents_dir.as_ref(), config);
    if !registry_paths.agents.is_empty()
        && args.root_dir.is_none()
        && overrides.agents_dir.is_none()
        && let Some(preferred) = registry_paths.agents.iter().find(|dir| {
            registry_paths.demo_agents.as_ref() != Some(*dir) && !(dir.exists() && dir.is_file())
        })
    {
        install_root = preferred.clone();
    }

    fs::create_dir_all(&install_root)?;
    let dest_dir = install_root.join(reference.spec());
    if paths_equivalent(&bundle_root, &dest_dir) {
        return Err(AgentError::Install(
            "bundle source resolves to install destination; choose a different --root".into(),
        ));
    }
    if dest_dir.exists() {
        if args.force {
            fs::remove_dir_all(&dest_dir)?;
        } else {
            return Err(AgentError::Exists(dest_dir));
        }
    }

    let staging_dir = install_root.join(format!(".{}.staging-{}", reference.name, Uuid::new_v4()));
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir)?;
    }
    let mut staging_guard = StagingGuard::new(staging_dir.clone());
    copy_dir_all(&bundle_root, &staging_dir)?;
    fs::rename(&staging_dir, &dest_dir)?;
    staging_guard.commit();

    Ok(InstallResult {
        agent_dir: dest_dir.clone(),
        manifest_path: dest_dir.join("manifest.toml"),
        reference,
    })
}

fn paths_equivalent(lhs: &Path, rhs: &Path) -> bool {
    if lhs == rhs {
        return true;
    }
    match (fs::canonicalize(lhs), fs::canonicalize(rhs)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn build_with_toolchain(
    args: BuildArgs,
    config: &Config,
    overrides: &PathOverrides,
    rustc_cmd: &str,
    cargo_cmd: &str,
) -> Result<BuildResult, AgentError> {
    validate_name(&args.name)?;
    let agent_root = resolve_agent_root(&args.root_dir, overrides.agents_dir.as_ref(), config);
    let crate_root = resolve_crate_root(&args.crates_dir);
    let agent_dir = agent_root.join(&args.name);
    let crate_dir = crate_root.join(&args.name);
    let manifest_path = agent_dir.join("manifest.toml");
    let policy_path = agent_dir.join("policy.caps");
    let crate_manifest = crate_dir.join("Cargo.toml");
    if !crate_manifest.is_file() {
        return Err(AgentError::MissingCrate(crate_dir));
    }
    if !manifest_path.is_file() {
        return Err(AgentError::MissingManifest(manifest_path));
    }

    ensure_wasm_target(rustc_cmd)?;
    let bin_name = format!("{}_wasm", args.name);
    build_wasm_binary(&crate_dir, &bin_name, cargo_cmd)?;
    let wasm_src = locate_wasm_artifact(&crate_dir, &bin_name);
    if !wasm_src.is_file() {
        return Err(AgentError::MissingBinary(wasm_src));
    }

    let wasm_dest = agent_dir.join("bin").join(format!("{}.wasm", args.name));
    if let Some(parent) = wasm_dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&wasm_src, &wasm_dest)?;
    if let Some(bin_dir) = wasm_dest.parent() {
        let _ = fs::remove_file(bin_dir.join(".gitkeep"));
    }

    validate_policy_caps(&policy_path)?;
    let manifest_raw = fs::read_to_string(&manifest_path)?;
    let mut manifest_doc: DocumentMut = manifest_raw.parse().map_err(|err| {
        AgentError::Manifest(format!("parse {} failed: {err}", manifest_path.display()))
    })?;
    let expected_tools_path = manifest_doc
        .get("artifacts")
        .and_then(|a| a.get("tools"))
        .and_then(|t| t.get("path"))
        .and_then(|p| p.as_str())
        .map(|s| s.to_string());

    let wasm_digest =
        digest_file_hex(&wasm_dest).map_err(|err| AgentError::Digest(err.to_string()))?;
    let tools_digest = load_tools_digest(&agent_dir, expected_tools_path.as_deref())?;
    let wasm_rel = relative_path(&agent_dir, &wasm_dest);
    update_manifest_digests(
        &mut manifest_doc,
        &wasm_rel,
        &wasm_digest,
        tools_digest.as_ref(),
    )?;
    fs::write(&manifest_path, manifest_doc.to_string())?;

    Ok(BuildResult {
        agent_dir,
        crate_dir,
        manifest_path,
        wasm_path: wasm_dest,
    })
}

fn validate_name(name: &str) -> Result<(), AgentError> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(AgentError::InvalidName(name.into()));
    };
    if !first.is_ascii_lowercase() {
        return Err(AgentError::InvalidName(name.into()));
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        return Err(AgentError::InvalidName(name.into()));
    }
    Ok(())
}

fn validate_reference_path(reference: &AgentRef) -> Result<(), AgentError> {
    validate_reference_component("agent name", &reference.name)?;
    if let Some(variant) = &reference.variant {
        validate_reference_component("agent variant", variant)?;
    }
    Ok(())
}

fn validate_reference_component(label: &str, value: &str) -> Result<(), AgentError> {
    let mut components = Path::new(value).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(_)), None) => Ok(()),
        _ => Err(AgentError::Install(format!(
            "{label} '{value}' must be a single path segment"
        ))),
    }
}

fn load_bundle_source(raw: &str) -> Result<(PathBuf, Option<TempDir>), AgentError> {
    let path = parse_source_path(raw)?;
    if path.is_dir() {
        return Ok((path, None));
    }
    if path.is_file() {
        if is_tar_archive(&path) || is_tar_gz_archive(&path) {
            let temp = TempDir::new().map_err(|err| AgentError::Install(err.to_string()))?;
            extract_archive(&path, temp.path())?;
            let root = find_manifest_root(temp.path())?;
            return Ok((root, Some(temp)));
        }
        if path.file_name().and_then(|name| name.to_str()) == Some("manifest.toml") {
            let parent = path.parent().ok_or_else(|| {
                AgentError::Source("manifest.toml path has no parent directory".into())
            })?;
            return Ok((parent.to_path_buf(), None));
        }
        return Err(AgentError::Source(format!(
            "unsupported bundle file (expected directory or .tar/.tar.gz): {}",
            path.display()
        )));
    }
    Err(AgentError::Source(format!(
        "bundle source not found: {}",
        path.display()
    )))
}

fn parse_source_path(raw: &str) -> Result<PathBuf, AgentError> {
    if raw.contains("://") || raw.starts_with("file:") {
        let url = Url::parse(raw).map_err(|err| AgentError::Source(err.to_string()))?;
        if url.scheme() != "file" {
            return Err(AgentError::Source(format!(
                "unsupported URL scheme '{}'",
                url.scheme()
            )));
        }
        return url
            .to_file_path()
            .map_err(|_| AgentError::Source("invalid file:// URL".into()));
    }
    Ok(PathBuf::from(raw))
}

fn is_tar_archive(path: &Path) -> bool {
    matches!(path.extension().and_then(|ext| ext.to_str()), Some("tar"))
}

fn is_tar_gz_archive(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.ends_with(".tar.gz") || name.ends_with(".tgz")
}

fn extract_archive(path: &Path, dest: &Path) -> Result<(), AgentError> {
    let file = File::open(path)?;
    if is_tar_gz_archive(path) {
        let decoder = GzDecoder::new(file);
        let mut archive = Archive::new(decoder);
        unpack_archive(&mut archive, dest)?;
        return Ok(());
    }
    if is_tar_archive(path) {
        let mut archive = Archive::new(file);
        unpack_archive(&mut archive, dest)?;
        return Ok(());
    }
    Err(AgentError::Source(format!(
        "unsupported archive type: {}",
        path.display()
    )))
}

fn unpack_archive<R: std::io::Read>(
    archive: &mut Archive<R>,
    dest: &Path,
) -> Result<(), AgentError> {
    for entry in archive
        .entries()
        .map_err(|err| AgentError::Install(err.to_string()))?
    {
        let mut entry = entry.map_err(|err| AgentError::Install(err.to_string()))?;
        let entry_type = entry.header().entry_type();
        if entry_type == EntryType::Symlink || entry_type == EntryType::Link {
            return Err(AgentError::Install(
                "archive contains symlink/hardlink entries; refusing for safety".into(),
            ));
        }
        if matches!(
            entry_type,
            EntryType::XHeader
                | EntryType::XGlobalHeader
                | EntryType::GNULongName
                | EntryType::GNULongLink
        ) {
            continue;
        }
        if entry_type != EntryType::Regular && entry_type != EntryType::Directory {
            return Err(AgentError::Install(format!(
                "unsupported archive entry type: {}",
                entry_type_label(entry_type)
            )));
        }
        let entry_path = entry
            .path()
            .map_err(|err| AgentError::Install(err.to_string()))?;
        if entry_path.is_absolute()
            || entry_path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(AgentError::Install(format!(
                "archive entry has unsafe path: {}",
                entry_path.display()
            )));
        }
        entry
            .unpack_in(dest)
            .map_err(|err| AgentError::Install(err.to_string()))?;
    }
    Ok(())
}

fn find_manifest_root(base: &Path) -> Result<PathBuf, AgentError> {
    let manifests = discover_manifest_paths(base, 4);
    match manifests.len() {
        0 => Err(AgentError::Source(format!(
            "no manifest.toml found under {}",
            base.display()
        ))),
        1 => Ok(manifests[0]
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| base.to_path_buf())),
        _ => Err(AgentError::Source(format!(
            "multiple manifests found under {}",
            base.display()
        ))),
    }
}

fn discover_manifest_paths(base: &Path, max_depth: usize) -> Vec<PathBuf> {
    if base.is_file() {
        if base.file_name().and_then(|name| name.to_str()) == Some("manifest.toml") {
            return vec![base.to_path_buf()];
        }
        return Vec::new();
    }
    if !base.is_dir() {
        return Vec::new();
    }

    let mut manifests = Vec::new();
    let mut queue = Vec::new();
    queue.push((base.to_path_buf(), 0usize));

    while let Some((dir, depth)) = queue.pop() {
        let manifest = dir.join("manifest.toml");
        if manifest.is_file() {
            manifests.push(manifest);
            continue;
        }
        if depth >= max_depth {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut dirs = entries
            .flatten()
            .filter_map(|entry| {
                let Ok(ft) = entry.file_type() else {
                    return None;
                };
                if ft.is_dir() {
                    Some(entry.path())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        dirs.sort();
        for entry in dirs {
            queue.push((entry, depth + 1));
        }
    }

    manifests
}

fn load_bundle(
    bundle_root: &Path,
) -> Result<(AgentRef, runloop_agent_registry::AgentBundle), AgentError> {
    let registry = AgentRegistry::new([bundle_root]);
    let listed = registry.list()?;
    if listed.is_empty() {
        return Err(AgentError::Source(format!(
            "no manifest found in {}",
            bundle_root.display()
        )));
    }
    if listed.len() > 1 {
        return Err(AgentError::Source(format!(
            "multiple manifests found in {}",
            bundle_root.display()
        )));
    }
    let reference = listed[0].described.reference.clone();
    let manifest_dir = listed[0]
        .manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| bundle_root.to_path_buf());
    let bundle_registry = AgentRegistry::new([manifest_dir]);
    let bundle = bundle_registry.bundle(&reference)?;
    Ok((reference, bundle))
}

fn validate_bundle(bundle: &runloop_agent_registry::AgentBundle) -> Vec<String> {
    let mut issues = Vec::new();
    let mut has_entry = false;

    if let Some(entry) = &bundle.wasm_entry {
        has_entry = true;
        if !entry.path.is_file() {
            issues.push(format!("missing wasm binary at {}", entry.path.display()));
        } else if let Ok(digest) = digest_file_hex(&entry.path) {
            if digest != entry.blake3 {
                issues.push(format!(
                    "wasm digest mismatch (expected {}, got {})",
                    entry.blake3, digest
                ));
            }
        } else {
            issues.push(format!(
                "failed to read wasm binary at {}",
                entry.path.display()
            ));
        }
    }

    if let Some(entry) = &bundle.native_entry {
        has_entry = true;
        if !entry.path.is_file() {
            issues.push(format!("missing native binary at {}", entry.path.display()));
        } else if let Ok(digest) = digest_file_hex(&entry.path) {
            if digest != entry.blake3 {
                issues.push(format!(
                    "native digest mismatch (expected {}, got {})",
                    entry.blake3, digest
                ));
            }
        } else {
            issues.push(format!(
                "failed to read native binary at {}",
                entry.path.display()
            ));
        }
    }

    if !has_entry {
        issues.push("manifest missing entry_wasm/entry_native".into());
    }

    if let Some(policy_path) = &bundle.policy_path
        && !policy_path.is_file()
    {
        issues.push(format!("missing policy.caps at {}", policy_path.display()));
    }

    if let Some(tools) = &bundle.tools {
        if !tools.path.is_file() {
            issues.push(format!("missing tools.json at {}", tools.path.display()));
        } else if let Ok(digest) = digest_file_hex(&tools.path) {
            if digest != tools.blake3 {
                issues.push(format!(
                    "tools.json digest mismatch (expected {}, got {})",
                    tools.blake3, digest
                ));
            }
            if load_tools(&tools.path).is_err() {
                issues.push("tools.json failed schema validation".into());
            }
        } else {
            issues.push(format!(
                "failed to read tools.json at {}",
                tools.path.display()
            ));
        }
    }

    issues
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), AgentError> {
    if !src.is_dir() {
        return Err(AgentError::Install(format!(
            "bundle root {} is not a directory",
            src.display()
        )));
    }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dest_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&src_path, &dest_path)?;
        } else if file_type.is_file() {
            fs::copy(&src_path, &dest_path)?;
        } else if file_type.is_symlink() {
            return Err(AgentError::Install(format!(
                "symlinked file not allowed in bundle: {}",
                src_path.display()
            )));
        } else {
            return Err(AgentError::Install(format!(
                "unsupported file type in bundle: {}",
                src_path.display()
            )));
        }
    }
    Ok(())
}

struct StagingGuard {
    path: PathBuf,
    committed: bool,
}

impl StagingGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if !self.committed && self.path.exists() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn entry_type_label(entry_type: EntryType) -> &'static str {
    if entry_type == EntryType::Regular {
        return "regular";
    }
    if entry_type == EntryType::Directory {
        return "directory";
    }
    if entry_type == EntryType::Symlink {
        return "symlink";
    }
    if entry_type == EntryType::Link {
        return "hardlink";
    }
    if entry_type == EntryType::XHeader {
        return "pax-header";
    }
    if entry_type == EntryType::XGlobalHeader {
        return "pax-global";
    }
    if entry_type == EntryType::GNULongName {
        return "gnu-long-name";
    }
    if entry_type == EntryType::GNULongLink {
        return "gnu-long-link";
    }
    "other"
}

#[derive(Debug)]
struct BundleAudit {
    status: String,
    issues: Vec<String>,
    source_dir: Option<PathBuf>,
}

fn audit_bundle(
    reference: &AgentRef,
    manifest_path: &Path,
    search_dirs: &[PathBuf],
) -> BundleAudit {
    let source_dir = source_dir_for(manifest_path, search_dirs);
    let mut issues = Vec::new();
    let manifest_registry = AgentRegistry::new([manifest_path.to_path_buf()]);
    match manifest_registry.bundle(reference) {
        Ok(bundle) => {
            issues.extend(validate_bundle(&bundle));
        }
        Err(err) => {
            issues.push(format!("failed to load bundle: {err}"));
        }
    }
    let status = bundle_status(&issues);
    BundleAudit {
        status,
        issues,
        source_dir,
    }
}

fn bundle_status(issues: &[String]) -> String {
    if issues.is_empty() {
        return "ok".into();
    }
    if issues.iter().any(|issue| issue.contains("digest mismatch")) {
        return "digest_mismatch".into();
    }
    if issues.iter().any(|issue| issue.contains("missing")) {
        return "missing".into();
    }
    "invalid".into()
}

fn source_dir_for(manifest_path: &Path, search_dirs: &[PathBuf]) -> Option<PathBuf> {
    let mut best: Option<PathBuf> = None;
    for dir in search_dirs {
        if manifest_path.starts_with(dir) {
            let replace = match &best {
                Some(current) => dir.as_os_str().len() > current.as_os_str().len(),
                None => true,
            };
            if replace {
                best = Some(dir.clone());
            }
        }
    }
    best
}

fn resolve_agent_root(
    root: &Option<PathBuf>,
    agents_override: Option<&PathBuf>,
    config: &Config,
) -> PathBuf {
    resolve_agent_root_with_cwd(root, agents_override, config, None)
}

fn resolve_agent_root_with_cwd(
    root: &Option<PathBuf>,
    agents_override: Option<&PathBuf>,
    config: &Config,
    cwd: Option<&Path>,
) -> PathBuf {
    if let Some(dir) = root {
        return expand_tilde(dir.clone());
    }
    if let Some(dir) = agents_override {
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
    default_user_agents_wasm_dir()
}

fn default_opening_dir(paths: &RegistryPaths) -> Option<PathBuf> {
    paths
        .openings
        .iter()
        .find(|p| p.to_string_lossy().contains("examples"))
        .cloned()
        .or_else(|| paths.openings.first().cloned())
}

fn format_search_dirs(dirs: &[PathBuf]) -> String {
    dirs.iter()
        .map(|dir| dir.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn short_digest(digest: &str) -> String {
    digest.chars().take(12).collect()
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
    if cwd.is_none()
        && let Some(env_root) = env::var_os("RUNLOOP_WORKSPACE_ROOT")
    {
        let expanded = expand_tilde(PathBuf::from(env_root));
        if expanded.is_dir() {
            return Some(expanded);
        }
    }
    walk_for_workspace_root(cwd)
}

fn walk_for_workspace_root(cwd: Option<&Path>) -> Option<PathBuf> {
    let mut dir = cwd.map(PathBuf::from).or_else(|| env::current_dir().ok())?;
    let mut first_manifest_dir = None;
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file() {
            if let Some(root) = manifest_workspace_root(&manifest) {
                return Some(root);
            }
            if first_manifest_dir.is_none() {
                first_manifest_dir = Some(dir.clone());
            }
        }
        if !dir.pop() {
            break;
        }
    }
    first_manifest_dir
}

fn manifest_workspace_root(manifest: &Path) -> Option<PathBuf> {
    let doc = fs::read_to_string(manifest)
        .ok()
        .and_then(|raw| toml::from_str::<toml::Value>(&raw).ok())?;

    let ws = doc.get("workspace")?;

    // Table means this manifest is the workspace root.
    if ws.is_table() {
        return manifest.parent().map(PathBuf::from);
    }

    // String/path: follow it and accept the pointed manifest only if it has a workspace table.
    if let Some(rel) = ws.as_str() {
        let target = manifest
            .parent()
            .map(|p| p.join(rel))
            .unwrap_or_else(|| PathBuf::from(rel));
        let target_manifest = if target.is_dir() {
            target.join("Cargo.toml")
        } else {
            target.clone()
        };
        if target_manifest.is_file()
            && fs::read_to_string(&target_manifest)
                .ok()
                .and_then(|raw| toml::from_str::<toml::Value>(&raw).ok())
                .and_then(|doc| doc.get("workspace").map(|val| val.is_table()))
                == Some(true)
        {
            return target_manifest.parent().map(PathBuf::from);
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

fn default_user_agents_wasm_dir() -> PathBuf {
    home_dir()
        .map(|h| h.join(".runloop").join("agents-wasm"))
        .unwrap_or_else(|| PathBuf::from("crates/agents-wasm"))
}

fn write_bundle(
    answers: &WizardAnswers,
    agent_dir: &Path,
    crate_dir: &Path,
) -> Result<(), AgentError> {
    let bin_dir = agent_dir.join("bin");
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(agent_dir)?;
    // README lives alongside the bundle; manual build hint includes the resolved crate path.
    fs::write(
        agent_dir.join("README.md"),
        readme_md(answers, agent_dir, crate_dir),
    )?;
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
) -> Result<(), AgentError> {
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

fn write_crate(name: &str, crate_dir: &Path) -> Result<(), AgentError> {
    fs::create_dir_all(crate_dir.join("src"))?;
    let cargo = format!(
        r#"[package]
name = "runloop-agent-{name}-wasm"
version = "0.1.0"
edition = "2024"
license = "Apache-2.0"

[[bin]]
name = "{name}_wasm"
path = "src/main.rs"

[dependencies]
anyhow = "1.0"
clap = {{ version = "4.5", features = ["derive"] }}
serde = {{ version = "1.0", features = ["derive"] }}
serde_json = "1.0"
"#
    );
    fs::write(crate_dir.join("Cargo.toml"), cargo)?;
    fs::write(crate_dir.join("src/main.rs"), crate_main(name))?;
    Ok(())
}

fn write_policy_caps(answers: &WizardAnswers, agent_dir: &Path) -> Result<(), AgentError> {
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

fn write_tools(doc: &ToolsDoc, tools_path: &Path) -> Result<(), AgentError> {
    let tools_json = serde_json::to_string_pretty(doc)?;
    fs::write(tools_path, tools_json)?;
    Ok(())
}

fn readme_md(answers: &WizardAnswers, agent_dir: &Path, crate_dir: &Path) -> String {
    let crate_path = crate_dir.display().to_string();
    let agent_path = agent_dir.display().to_string();
    format!(
        r#"# {name} agent
{description}

Scaffolded by `rlp agent scaffold`. Build the wasm artifact with:

```
rlp agent build {name}
```

Or manually:

```
cargo build --target wasm32-wasip1 --manifest-path {crate_path}/Cargo.toml
```

Model: `{model}` (secret: {secret})
FS caps: {fs}
Net caps: {net}
KB read: {kb_read}
KB write: {kb_write}

Generated files:
- `{agent_path}/manifest.toml`
- `{agent_path}/policy.caps`
- `{agent_path}/tools.json`
- `{agent_path}/README.md`
- `{crate_path}` (wasm stub)
- `{opening}`

Edit `{crate_path}/src/main.rs` to implement your logic and
re-run `rlp agent build {name}` to refresh digests.
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
    #[arg(long = "input", alias = "payload")]
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
"#;
    TEMPLATE.replace("{name}", name)
}

fn write_opening(answers: &WizardAnswers, path: &Path) -> Result<(), AgentError> {
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
) -> Result<WizardAnswers, AgentError> {
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
        return Err(AgentError::Tools(detail));
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

fn ensure_wasm_target(rustc_cmd: &str) -> Result<(), AgentError> {
    let output = Command::new(rustc_cmd)
        .args(["--print", "target-list"])
        .output()
        .map_err(|err| AgentError::BuildFailed(format!("{rustc_cmd} not available: {err}")))?;
    if !output.status.success() {
        return Err(AgentError::BuildFailed(format!(
            "{rustc_cmd} --print target-list failed with status {}",
            output.status
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.split_whitespace().any(|t| t == "wasm32-wasip1") {
        return Err(AgentError::BuildFailed(
            "rustc missing wasm32-wasip1 target (install via 'rustup target add wasm32-wasip1')"
                .into(),
        ));
    }
    Ok(())
}

fn build_wasm_binary(crate_dir: &Path, bin_name: &str, cargo_cmd: &str) -> Result<(), AgentError> {
    let output = Command::new(cargo_cmd)
        .current_dir(crate_dir)
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip1",
            "--bin",
            bin_name,
        ])
        .output()
        .map_err(|err| {
            AgentError::BuildFailed(format!("failed to spawn {cargo_cmd} build: {err}"))
        })?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut detail = stdout.trim().to_string();
        if !stderr.trim().is_empty() {
            if !detail.is_empty() {
                detail.push_str(" | ");
            }
            detail.push_str(stderr.trim());
        }
        return Err(AgentError::BuildFailed(format!(
            "{cargo_cmd} build failed (status {}): {}",
            output.status, detail
        )));
    }
    Ok(())
}

fn locate_wasm_artifact(crate_dir: &Path, bin_name: &str) -> PathBuf {
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .or_else(|| workspace_root(Some(crate_dir)).map(|ws| ws.join("target")))
        .unwrap_or_else(|| crate_dir.join("target"));
    target_dir
        .join("wasm32-wasip1")
        .join("release")
        .join(format!("{bin_name}.wasm"))
}

fn load_tools_digest(
    agent_dir: &Path,
    expected_rel: Option<&str>,
) -> Result<Option<(String, String, u32)>, AgentError> {
    let resolved = match expected_rel {
        Some(rel) => {
            let path = agent_dir.join(rel);
            if !path.is_file() {
                return Err(AgentError::Tools(format!(
                    "tools.json missing at {} (referenced by manifest)",
                    path.display()
                )));
            }
            path
        }
        None => {
            let path = agent_dir.join("tools.json");
            if !path.is_file() {
                return Ok(None);
            }
            path
        }
    };
    let doc = load_tools(&resolved).map_err(|err| AgentError::Tools(err.to_string()))?;
    let digest = digest_file_hex(&resolved).map_err(|err| AgentError::Digest(err.to_string()))?;
    Ok(Some((
        relative_path(agent_dir, &resolved),
        digest,
        doc.version,
    )))
}

fn validate_policy_caps(policy_path: &Path) -> Result<(), AgentError> {
    if !policy_path.is_file() {
        return Err(AgentError::Caps(format!(
            "policy.caps missing at {}",
            policy_path.display()
        )));
    }
    let raw = fs::read_to_string(policy_path)?;
    let parsed: toml::Value = toml::from_str(&raw).map_err(|err| {
        AgentError::Caps(format!(
            "invalid policy.caps {}: {err}",
            policy_path.display()
        ))
    })?;
    if parsed.get("capabilities").is_none() {
        return Err(AgentError::Caps(format!(
            "policy.caps {} missing [capabilities] table",
            policy_path.display()
        )));
    }
    Ok(())
}

fn relative_path(base: &Path, target: &Path) -> String {
    target
        .strip_prefix(base)
        .unwrap_or(target)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn update_manifest_digests(
    doc: &mut DocumentMut,
    wasm_rel_path: &str,
    wasm_digest: &str,
    tools: Option<&(String, String, u32)>,
) -> Result<(), AgentError> {
    doc["agent"]["entry_wasm"]["path"] = value(wasm_rel_path);
    doc["agent"]["entry_wasm"]["blake3"] = value(wasm_digest);
    if let Some((path, digest, version)) = tools {
        doc["artifacts"]["tools"]["path"] = value(path.clone());
        doc["artifacts"]["tools"]["blake3"] = value(digest.clone());
        doc["artifacts"]["tools"]["version"] = value(*version as i64);
    }
    Ok(())
}

struct Prompter {
    interactive: bool,
}

impl Prompter {
    fn new(interactive: bool) -> Self {
        Self { interactive }
    }

    fn prompt_string(&self, prompt: &str, default: Option<&str>) -> Result<String, AgentError> {
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
    ) -> Result<Option<String>, AgentError> {
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

    fn prompt_bool(&self, prompt: &str, default: bool) -> Result<bool, AgentError> {
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

    fn prompt_list(&mut self, prompt: &str, default: &[String]) -> Result<Vec<String>, AgentError> {
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

    fn prompt_tool(&mut self) -> Result<ToolEntry, AgentError> {
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
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use runloop_agent_registry::digest_file_hex;
    use runloop_core::Config;
    use std::os::unix::fs::PermissionsExt;
    use tar::{Builder, Header};
    use tempfile::tempdir;
    use toml;

    fn write_manifest(agent_dir: &Path, name: &str) {
        let manifest = format!(
            r#"[agent]
name = "{name}"
version = "0.1.0"
entry_wasm = {{ path = "bin/{name}.wasm", blake3 = "{ZERO_DIGEST}" }}

[ports]
in = []
out = []

[caps]
file = "policy.caps"

[artifacts.tools]
path = "tools.json"
blake3 = "{ZERO_DIGEST}"
version = 1
"#
        );
        fs::write(agent_dir.join("manifest.toml"), manifest).expect("write manifest");
    }

    fn write_manifest_with_digests(
        agent_dir: &Path,
        name: &str,
        wasm_digest: &str,
        tools_digest: &str,
    ) {
        let manifest = format!(
            r#"[agent]
name = "{name}"
version = "0.1.0"
entry_wasm = {{ path = "bin/{name}.wasm", blake3 = "{wasm_digest}" }}

[ports]
in = []
out = []

[caps]
file = "policy.caps"

[artifacts.tools]
path = "tools.json"
blake3 = "{tools_digest}"
version = 1
"#
        );
        fs::write(agent_dir.join("manifest.toml"), manifest).expect("write manifest");
    }

    fn write_policy(agent_dir: &Path) {
        let policy = r#"[capabilities]
fs = []
net = []
time = false
kb_read = false
kb_write = false
secrets = []
model = false
exec = false
"#;
        fs::write(agent_dir.join("policy.caps"), policy).expect("write policy");
    }

    fn write_tools_file(agent_dir: &Path) {
        fs::write(
            agent_dir.join("tools.json"),
            r#"{ "version": 1, "tools": [] }"#,
        )
        .expect("write tools");
    }

    fn write_wasm_file(agent_dir: &Path, name: &str) -> PathBuf {
        let bin_dir = agent_dir.join("bin");
        fs::create_dir_all(&bin_dir).expect("create bin dir");
        let wasm_path = bin_dir.join(format!("{name}.wasm"));
        fs::write(&wasm_path, b"wasm").expect("write wasm");
        wasm_path
    }

    fn write_valid_bundle(agent_dir: &Path, name: &str) {
        fs::create_dir_all(agent_dir).expect("create agent dir");
        write_policy(agent_dir);
        write_tools_file(agent_dir);
        let wasm_path = write_wasm_file(agent_dir, name);
        let wasm_digest = digest_file_hex(&wasm_path).expect("wasm digest");
        let tools_digest = digest_file_hex(&agent_dir.join("tools.json")).expect("tools digest");
        write_manifest_with_digests(agent_dir, name, &wasm_digest, &tools_digest);
    }

    fn write_tar_archive(src: &Path, tar_path: &Path) {
        let file = File::create(tar_path).expect("create tar");
        let mut builder = Builder::new(file);
        builder
            .append_dir_all("bundle", src)
            .expect("append bundle");
        builder.finish().expect("finish tar");
    }

    fn write_tar_gz_archive(src: &Path, tar_path: &Path) {
        let file = File::create(tar_path).expect("create tar.gz");
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = Builder::new(encoder);
        builder
            .append_dir_all("bundle", src)
            .expect("append bundle");
        builder.finish().expect("finish tar");
        let encoder = builder.into_inner().expect("tar writer");
        encoder.finish().expect("finish gzip");
    }

    fn write_tar_with_raw_entry(tar_path: &Path, entry_name: &str) {
        let mut header = Header::new_gnu();
        header.set_entry_type(EntryType::Regular);
        header.set_size(0);
        header.set_mode(0o644);
        header.set_mtime(0);
        {
            let bytes = header.as_mut_bytes();
            bytes[..100].fill(0);
            let name_bytes = entry_name.as_bytes();
            bytes[..name_bytes.len()].copy_from_slice(name_bytes);
        }
        header.set_cksum();

        let mut file = File::create(tar_path).expect("create tar");
        file.write_all(header.as_bytes()).expect("write header");
        let padding = [0u8; 1024];
        file.write_all(&padding).expect("write trailer");
    }

    fn install_fake_toolchain(bin_dir: &Path) -> (PathBuf, PathBuf) {
        fs::create_dir_all(bin_dir).expect("toolchain bin dir");
        let rustc = bin_dir.join("rustc");
        fs::write(
            &rustc,
            r#"#!/usr/bin/env bash
if [[ "$1" == "--print" && "$2" == "target-list" ]]; then
  echo "wasm32-wasip1"
  exit 0
fi
exit 0
"#,
        )
        .expect("write rustc");
        let cargo = bin_dir.join("cargo");
        fs::write(
            &cargo,
            r#"#!/usr/bin/env bash
set -euo pipefail
crate="$(pwd)"
target="$crate/target/wasm32-wasip1/release"
mkdir -p "$target"
bin_name=""
while [[ $# -gt 0 ]]; do
  if [[ "$1" == "--bin" ]]; then
    shift
    bin_name="$1"
  fi
  shift || true
done
: "${bin_name:=agent_wasm}"
echo "stub wasm" > "$target/${bin_name}.wasm"
exit 0
"#,
        )
        .expect("write cargo");
        for path in [&rustc, &cargo] {
            let mut perms = fs::metadata(path).expect("metadata").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(path, perms).expect("chmod");
        }
        (rustc, cargo)
    }

    fn install_fake_toolchain_with_target_root(
        bin_dir: &Path,
        target_root: &Path,
    ) -> (PathBuf, PathBuf) {
        fs::create_dir_all(bin_dir).expect("toolchain bin dir");
        let rustc = bin_dir.join("rustc");
        fs::write(
            &rustc,
            r#"#!/usr/bin/env bash
if [[ "$1" == "--print" && "$2" == "target-list" ]]; then
  echo "wasm32-wasip1"
  exit 0
fi
exit 0
"#,
        )
        .expect("write rustc");
        let cargo = bin_dir.join("cargo");
        fs::write(
            &cargo,
            format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
target="{target_root}/wasm32-wasip1/release"
mkdir -p "$target"
bin_name=""
while [[ $# -gt 0 ]]; do
  if [[ "$1" == "--bin" ]]; then
    shift
    bin_name="$1"
  fi
  shift || true
done
: "${{bin_name:=agent_wasm}}"
echo "stub wasm" > "$target/${{bin_name}}.wasm"
exit 0
"#,
                target_root = target_root.display()
            ),
        )
        .expect("write cargo");
        for path in [&rustc, &cargo] {
            let mut perms = fs::metadata(path).expect("metadata").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(path, perms).expect("chmod");
        }
        (rustc, cargo)
    }

    #[test]
    fn resolve_defaults_to_workspace_agents_dir() {
        let temp = tempdir().expect("temp");
        fs::write(temp.path().join("Cargo.toml"), "").expect("cargo file");
        fs::create_dir_all(temp.path().join("crates/agents-wasm")).expect("crate dir");

        let config = Config::default();
        let agent_root = resolve_agent_root_with_cwd(&None, None, &config, Some(temp.path()));
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

        let agent_root = resolve_agent_root_with_cwd(&None, None, &config, Some(temp.path()));
        let crate_root = resolve_crate_root_with_cwd(&None, Some(temp.path()));
        assert_eq!(agent_root, agents_root);
        assert_eq!(crate_root, default_user_agents_wasm_dir());
    }

    #[test]
    fn resolve_prefers_config_dir_even_if_missing() {
        let temp = tempdir().expect("temp");
        let agents_root = temp.path().join("custom-agents");

        let mut config = Config::default();
        config.agents.search_dirs = vec![agents_root.to_string_lossy().into_owned()];

        let agent_root = resolve_agent_root_with_cwd(&None, None, &config, Some(temp.path()));
        assert_eq!(agent_root, agents_root);
        let crate_root = resolve_crate_root_with_cwd(&None, Some(temp.path()));
        assert_eq!(crate_root, default_user_agents_wasm_dir());
    }

    #[test]
    fn list_respects_root_override() {
        let temp = tempdir().expect("temp");
        let override_root = temp.path().join("agents");
        let config = Config::default();
        fs::create_dir_all(&override_root).expect("override root");
        let overrides = PathOverrides {
            agents_dir: Some(override_root.clone()),
            ..Default::default()
        };
        let dirs = resolve_paths(&config, &overrides).agents;
        assert_eq!(dirs.first(), Some(&override_root));
    }

    #[test]
    fn list_appends_config_dirs_after_primary() {
        let temp = tempdir().expect("temp");
        let workspace_root = temp.path().join("workspace");
        fs::create_dir_all(&workspace_root).expect("workspace dir");
        fs::write(workspace_root.join("Cargo.toml"), "[workspace]\nmembers=[]")
            .expect("workspace manifest");

        let config_dir = temp.path().join("custom-agents");
        fs::create_dir_all(&config_dir).expect("create search dir");
        let mut config = Config::default();
        config.agents.search_dirs = vec![config_dir.to_string_lossy().into_owned()];

        let dirs = resolve_paths(
            &config,
            &PathOverrides {
                cwd: Some(workspace_root.clone()),
                ..Default::default()
            },
        )
        .agents;
        assert_eq!(dirs.first(), Some(&workspace_root.join("agents")));
        assert!(dirs.contains(&config_dir));
    }

    #[test]
    fn workspace_root_prefers_nearest_workspace_manifest() {
        let temp = tempdir().expect("temp");
        let outer_ws = temp.path().join("outer");
        let inner_ws = outer_ws.join("inner");
        let member = inner_ws.join("member");
        fs::create_dir_all(&member).expect("member dir");

        fs::write(
            outer_ws.join("Cargo.toml"),
            "[workspace]\nmembers=[\"inner/member\"]",
        )
        .expect("outer manifest");
        fs::write(
            inner_ws.join("Cargo.toml"),
            "[workspace]\nmembers=[\"member\"]",
        )
        .expect("inner manifest");
        fs::write(
            member.join("Cargo.toml"),
            "[package]\nname=\"member\"\nversion=\"0.1.0\"",
        )
        .expect("member manifest");

        let found = workspace_root(Some(member.as_path()));
        assert_eq!(found, Some(inner_ws));
    }

    #[test]
    fn workspace_root_follows_string_workspace_pointer() {
        let temp = tempdir().expect("temp");
        let ws_root = temp.path().join("ws");
        let member = ws_root.join("crates").join("member");
        fs::create_dir_all(&member).expect("member dir");

        fs::write(
            ws_root.join("Cargo.toml"),
            "[workspace]\nmembers=[\"crates/member\"]",
        )
        .expect("ws manifest");
        fs::write(
            member.join("Cargo.toml"),
            "workspace = \"..\"\n[package]\nname=\"member\"\nversion=\"0.1.0\"",
        )
        .expect("member manifest");

        let found = workspace_root(Some(member.as_path()));
        assert_eq!(found, Some(ws_root));
    }

    #[test]
    fn locate_wasm_artifact_uses_workspace_target() {
        let temp = tempdir().expect("temp");
        let ws_root = temp.path().join("ws");
        let member = ws_root.join("agent");
        fs::create_dir_all(&member).expect("member dir");
        fs::write(
            ws_root.join("Cargo.toml"),
            "[workspace]\nmembers=[\"agent\"]",
        )
        .expect("ws manifest");
        fs::write(
            member.join("Cargo.toml"),
            "[package]\nname=\"agent\"\nversion=\"0.1.0\"",
        )
        .expect("member manifest");

        let located = locate_wasm_artifact(&member, "agent_wasm");
        assert_eq!(
            located,
            ws_root.join("target/wasm32-wasip1/release/agent_wasm.wasm")
        );
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
        let overrides = PathOverrides {
            cwd: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let registry_paths = resolve_paths(&config, &overrides);
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
        let result = scaffold(args, &config, &overrides, &registry_paths).expect("scaffold");
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
        let cargo_toml =
            fs::read_to_string(result.crate_dir.join("Cargo.toml")).expect("cargo contents");
        assert!(
            cargo_toml.contains("license = \"Apache-2.0\""),
            "scaffolded crate should declare a license"
        );
        assert!(
            !cargo_toml.contains("license.workspace"),
            "scaffolded crate should not rely on workspace metadata"
        );
        assert_eq!(result.opening_path.unwrap(), opening);
    }

    #[cfg(unix)]
    #[test]
    fn build_updates_manifest_and_tool_digests() {
        let temp = tempdir().expect("temp");
        let agents_root = temp.path().join("agents");
        let crates_root = temp.path().join("crates").join("agents-wasm");
        let agent_dir = agents_root.join("demo");
        let crate_dir = crates_root.join("demo");
        fs::create_dir_all(agent_dir.join("bin")).expect("agent bin dir");
        fs::create_dir_all(&crate_dir).expect("crate dir");

        write_manifest(&agent_dir, "demo");
        write_policy(&agent_dir);
        write_tools_file(&agent_dir);
        fs::write(crate_dir.join("Cargo.toml"), "").expect("cargo file");

        let toolchain_bin = temp.path().join("toolchain-bin");
        let (rustc_path, cargo_path) = install_fake_toolchain(&toolchain_bin);

        let config = Config::default();
        let overrides = PathOverrides::default();
        let args = BuildArgs {
            name: "demo".into(),
            root_dir: Some(agents_root.clone()),
            crates_dir: Some(crates_root.clone()),
        };
        let result = build_with_toolchain(
            args,
            &config,
            &overrides,
            rustc_path.to_str().unwrap(),
            cargo_path.to_str().unwrap(),
        )
        .expect("build");

        let manifest = fs::read_to_string(agent_dir.join("manifest.toml")).expect("manifest");
        let parsed: toml::Value = toml::from_str(&manifest).expect("parse manifest");

        let wasm_path = result.wasm_path;
        assert!(wasm_path.is_file(), "wasm should be copied");
        let wasm_digest = digest_file_hex(&wasm_path).expect("wasm digest");
        assert_eq!(
            parsed["agent"]["entry_wasm"]["blake3"].as_str(),
            Some(wasm_digest.as_str())
        );

        let tools_path = agent_dir.join("tools.json");
        let tools_digest = digest_file_hex(&tools_path).expect("tools digest");
        assert_eq!(
            parsed["artifacts"]["tools"]["blake3"].as_str(),
            Some(tools_digest.as_str())
        );
        assert_eq!(
            parsed["artifacts"]["tools"]["path"].as_str(),
            Some("tools.json")
        );
    }

    #[cfg(unix)]
    #[test]
    fn build_uses_workspace_target_dir_when_present() {
        let temp = tempdir().expect("temp");
        let agents_root = temp.path().join("agents");
        let workspace_root = temp.path().join("crates").join("agents-wasm");
        let agent_dir = agents_root.join("demo");
        let crate_dir = workspace_root.join("demo");
        fs::create_dir_all(agent_dir.join("bin")).expect("agent bin dir");
        fs::create_dir_all(&crate_dir).expect("crate dir");

        fs::write(
            workspace_root.join("Cargo.toml"),
            "[workspace]\nmembers=[\"demo\"]",
        )
        .expect("workspace manifest");
        fs::write(
            crate_dir.join("Cargo.toml"),
            "[package]\nname=\"demo\"\nversion=\"0.1.0\"",
        )
        .expect("crate manifest");

        write_manifest(&agent_dir, "demo");
        write_policy(&agent_dir);
        write_tools_file(&agent_dir);

        let toolchain_bin = temp.path().join("toolchain-bin");
        let (rustc_path, cargo_path) =
            install_fake_toolchain_with_target_root(&toolchain_bin, &workspace_root.join("target"));

        let config = Config::default();
        let overrides = PathOverrides::default();
        let args = BuildArgs {
            name: "demo".into(),
            root_dir: Some(agents_root.clone()),
            crates_dir: Some(workspace_root.clone()),
        };
        let result = build_with_toolchain(
            args,
            &config,
            &overrides,
            rustc_path.to_str().unwrap(),
            cargo_path.to_str().unwrap(),
        )
        .expect("build");

        assert!(result.wasm_path.is_file(), "wasm should be copied");
        assert!(
            workspace_root
                .join("target/wasm32-wasip1/release/demo_wasm.wasm")
                .is_file(),
            "wasm should be written under workspace target",
        );
    }

    #[cfg(unix)]
    #[test]
    fn build_fails_when_tools_missing() {
        let temp = tempdir().expect("temp");
        let agents_root = temp.path().join("agents");
        let crates_root = temp.path().join("crates").join("agents-wasm");
        let agent_dir = agents_root.join("demo");
        let crate_dir = crates_root.join("demo");
        fs::create_dir_all(agent_dir.join("bin")).expect("agent bin dir");
        fs::create_dir_all(&crate_dir).expect("crate dir");

        write_manifest(&agent_dir, "demo");
        write_policy(&agent_dir);
        fs::write(crate_dir.join("Cargo.toml"), "").expect("cargo file");

        let toolchain_bin = temp.path().join("toolchain-bin");
        let (rustc_path, cargo_path) = install_fake_toolchain(&toolchain_bin);

        let config = Config::default();
        let overrides = PathOverrides::default();
        let args = BuildArgs {
            name: "demo".into(),
            root_dir: Some(agents_root.clone()),
            crates_dir: Some(crates_root.clone()),
        };
        let result = build_with_toolchain(
            args,
            &config,
            &overrides,
            rustc_path.to_str().unwrap(),
            cargo_path.to_str().unwrap(),
        );
        assert!(
            matches!(result, Err(AgentError::Tools(_))),
            "missing tools should fail build"
        );
    }

    #[test]
    fn install_copies_bundle() {
        let temp = tempdir().expect("temp");
        let source_root = temp.path().join("source");
        let agent_dir = source_root.join("my_agent");
        write_valid_bundle(&agent_dir, "my_agent");

        let dest_root = temp.path().join("dest");
        let config = Config::default();
        let overrides = PathOverrides::default();
        let registry_paths = resolve_paths(&config, &overrides);
        let args = InstallArgs {
            source: agent_dir.display().to_string(),
            root_dir: Some(dest_root.clone()),
            force: false,
            skip_verify: false,
        };

        let result = install(args, &config, &overrides, &registry_paths).expect("install");
        assert_eq!(result.reference.name, "my_agent");
        assert!(result.agent_dir.join("manifest.toml").is_file());
        assert!(result.agent_dir.join("bin/my_agent.wasm").is_file());
    }

    #[test]
    fn install_uses_manifest_parent_when_nested() {
        let temp = tempdir().expect("temp");
        let source_root = temp.path().join("source");
        let nested_dir = source_root.join("nested").join("my_agent");
        write_valid_bundle(&nested_dir, "my_agent");

        let dest_root = temp.path().join("dest");
        let config = Config::default();
        let overrides = PathOverrides::default();
        let registry_paths = resolve_paths(&config, &overrides);
        let args = InstallArgs {
            source: source_root.display().to_string(),
            root_dir: Some(dest_root.clone()),
            force: false,
            skip_verify: false,
        };

        let result = install(args, &config, &overrides, &registry_paths).expect("install");
        assert!(result.agent_dir.join("manifest.toml").is_file());
        assert!(result.agent_dir.join("bin/my_agent.wasm").is_file());
        assert!(
            !result.agent_dir.join("nested").exists(),
            "nested source root should not be copied"
        );
    }

    #[test]
    fn install_rejects_source_is_destination() {
        let temp = tempdir().expect("temp");
        let dest_root = temp.path().join("agents");
        let agent_dir = dest_root.join("my_agent");
        write_valid_bundle(&agent_dir, "my_agent");

        let config = Config::default();
        let overrides = PathOverrides::default();
        let registry_paths = RegistryPaths::default();
        let args = InstallArgs {
            source: agent_dir.display().to_string(),
            root_dir: Some(dest_root),
            force: true,
            skip_verify: false,
        };

        let err = install(args, &config, &overrides, &registry_paths).unwrap_err();
        match err {
            AgentError::Install(msg) => {
                assert!(msg.contains("source resolves"), "unexpected error: {msg}");
            }
            other => panic!("expected install error, got {other:?}"),
        }
    }

    #[test]
    fn install_chooses_non_file_root() {
        let temp = tempdir().expect("temp");
        let file_root = temp.path().join("manifest.toml");
        fs::write(&file_root, "placeholder").expect("write manifest");
        let install_root = temp.path().join("registry");
        fs::create_dir_all(&install_root).expect("create registry dir");
        let source_root = temp.path().join("source");
        let agent_dir = source_root.join("my_agent");
        write_valid_bundle(&agent_dir, "my_agent");

        let config = Config::default();
        let overrides = PathOverrides::default();
        let registry_paths = RegistryPaths {
            agents: vec![file_root, install_root.clone()],
            openings: Vec::new(),
            demo_agents: None,
            info: Vec::new(),
            warnings: Vec::new(),
        };
        let args = InstallArgs {
            source: agent_dir.display().to_string(),
            root_dir: None,
            force: false,
            skip_verify: false,
        };

        let result = install(args, &config, &overrides, &registry_paths).expect("install");
        assert!(result.agent_dir.starts_with(&install_root));
    }

    #[test]
    fn install_from_tar_copies_bundle() {
        let temp = tempdir().expect("temp");
        let source_root = temp.path().join("source");
        write_valid_bundle(&source_root, "my_agent");
        let tar_path = temp.path().join("bundle.tar");
        write_tar_archive(&source_root, &tar_path);

        let dest_root = temp.path().join("dest");
        let config = Config::default();
        let overrides = PathOverrides::default();
        let registry_paths = RegistryPaths::default();
        let args = InstallArgs {
            source: tar_path.display().to_string(),
            root_dir: Some(dest_root),
            force: false,
            skip_verify: false,
        };

        let result = install(args, &config, &overrides, &registry_paths).expect("install");
        assert!(result.agent_dir.join("manifest.toml").is_file());
        assert!(result.agent_dir.join("bin/my_agent.wasm").is_file());
    }

    #[test]
    fn install_from_tar_gz_copies_bundle() {
        let temp = tempdir().expect("temp");
        let source_root = temp.path().join("source");
        write_valid_bundle(&source_root, "my_agent");
        let tar_path = temp.path().join("bundle.tar.gz");
        write_tar_gz_archive(&source_root, &tar_path);

        let dest_root = temp.path().join("dest");
        let config = Config::default();
        let overrides = PathOverrides::default();
        let registry_paths = RegistryPaths::default();
        let args = InstallArgs {
            source: tar_path.display().to_string(),
            root_dir: Some(dest_root),
            force: false,
            skip_verify: false,
        };

        let result = install(args, &config, &overrides, &registry_paths).expect("install");
        assert!(result.agent_dir.join("manifest.toml").is_file());
        assert!(result.agent_dir.join("bin/my_agent.wasm").is_file());
    }

    #[test]
    fn install_rejects_bad_digest() {
        let temp = tempdir().expect("temp");
        let source_root = temp.path().join("source");
        let agent_dir = source_root.join("bad_agent");
        fs::create_dir_all(&agent_dir).expect("create agent dir");
        write_policy(&agent_dir);
        write_tools_file(&agent_dir);
        let _ = write_wasm_file(&agent_dir, "bad_agent");
        write_manifest_with_digests(&agent_dir, "bad_agent", ZERO_DIGEST, ZERO_DIGEST);

        let dest_root = temp.path().join("dest");
        let config = Config::default();
        let overrides = PathOverrides::default();
        let registry_paths = resolve_paths(&config, &overrides);
        let args = InstallArgs {
            source: agent_dir.display().to_string(),
            root_dir: Some(dest_root),
            force: false,
            skip_verify: false,
        };

        let err = install(args, &config, &overrides, &registry_paths).unwrap_err();
        match err {
            AgentError::Bundle(_) => {}
            other => panic!("expected bundle error, got {other:?}"),
        }
    }

    #[test]
    fn install_rejects_unsafe_reference() {
        let temp = tempdir().expect("temp");
        let source_root = temp.path().join("source");
        fs::create_dir_all(&source_root).expect("create source dir");
        write_manifest_with_digests(&source_root, "../evil", ZERO_DIGEST, ZERO_DIGEST);

        let dest_root = temp.path().join("dest");
        let config = Config::default();
        let overrides = PathOverrides::default();
        let registry_paths = resolve_paths(&config, &overrides);
        let args = InstallArgs {
            source: source_root.display().to_string(),
            root_dir: Some(dest_root),
            force: false,
            skip_verify: true,
        };

        let err = install(args, &config, &overrides, &registry_paths).unwrap_err();
        match err {
            AgentError::Install(msg) => {
                assert!(msg.contains("agent name"), "unexpected error: {msg}");
            }
            other => panic!("expected install error, got {other:?}"),
        }
    }

    #[test]
    fn staging_guard_removes_on_drop() {
        let temp = tempdir().expect("temp");
        let staging = temp.path().join(".staging");
        fs::create_dir_all(&staging).expect("create staging");
        fs::write(staging.join("marker.txt"), "test").expect("write marker");
        {
            let _guard = StagingGuard::new(staging.clone());
        }
        assert!(!staging.exists(), "staging dir should be cleaned up");
    }

    #[test]
    fn extract_archive_rejects_symlink_entry() {
        let temp = tempdir().expect("temp");
        let tar_path = temp.path().join("bundle.tar");
        let file = File::create(&tar_path).expect("create tar");
        let mut builder = Builder::new(file);
        let mut header = Header::new_gnu();
        header.set_entry_type(EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_mtime(0);
        header.set_cksum();
        header.set_link_name("target").expect("set link name");
        builder
            .append_data(&mut header, "link", io::empty())
            .expect("append symlink");
        builder.finish().expect("finish tar");

        let dest = temp.path().join("dest");
        fs::create_dir_all(&dest).expect("create dest");
        let err = extract_archive(&tar_path, &dest).unwrap_err();
        match err {
            AgentError::Install(msg) => {
                assert!(msg.contains("symlink"), "unexpected error: {msg}");
            }
            other => panic!("expected install error, got {other:?}"),
        }
    }

    #[test]
    fn extract_archive_rejects_path_traversal() {
        let temp = tempdir().expect("temp");
        let tar_path = temp.path().join("bundle.tar");
        write_tar_with_raw_entry(&tar_path, "../evil");

        let dest = temp.path().join("dest");
        fs::create_dir_all(&dest).expect("create dest");
        let err = extract_archive(&tar_path, &dest).unwrap_err();
        match err {
            AgentError::Install(msg) => {
                assert!(msg.contains("unsafe path"), "unexpected error: {msg}");
            }
            other => panic!("expected install error, got {other:?}"),
        }
    }
}
