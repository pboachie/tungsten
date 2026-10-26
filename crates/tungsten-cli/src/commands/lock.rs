// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten lock`: the versioned single-project generation recipe.
//!
//! The recipe records the declared input closure (manifest, profiles, agent
//! manifest, entry documents, local references and overlays in application
//! order) by content identity, together with compiler, emitter and consumer
//! toolchain identities. Capture compiles offline: it never fetches or
//! refreshes inputs, runs emitters or external commands, or probes tools.
//! Physical paths, option values and credentials are never recorded; options
//! are identified by digest only. A recipe is not an approval: it grants no
//! trust to external emitters and is not a replayable manifest.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tungsten_build::{CompileOptions, Compiled, TUNGSTEN_VERSION, compile_project};
use tungsten_config::TungstenConfig;
use tungsten_core::Digest;

use crate::args::LockArgs;
use crate::output::{CliError, CommandName, CommandResult, ErrorKind};
use crate::{Report, exit, input};

/// Lock format version written in `tungsten_lock`.
pub(crate) const LOCK_VERSION: u32 = 1;
/// File name of the lock, next to the manifest.
pub(crate) const LOCK_FILE: &str = "tungsten.lock.json";
/// Largest lock file read back.
const MAX_LOCK_BYTES: u64 = 4 * 1024 * 1024;

/// One project's generation recipe (`tungsten.lock.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerationRecipe {
    /// Lock format version. Must be 1.
    pub tungsten_lock: u32,
    pub compiler: CompilerIdentity,
    pub project: ProjectIdentity,
    /// The declared input closure, by portable path. Sorted by path.
    pub files: Vec<LockedFile>,
    /// Inputs in manifest order, with their overlays in application order.
    pub inputs: Vec<LockedInput>,
    /// Selected profiles in application order.
    pub profiles: Vec<LockedProfile>,
    /// Targets in manifest order.
    pub targets: Vec<LockedTarget>,
    /// What this recipe cannot reproduce on its own. Sorted.
    pub gaps: Vec<RecipeGap>,
    pub guarantees: Guarantees,
    /// `blake3:` digest of this document with `recipe_digest` empty. A lock
    /// whose digest does not match its content is corrupt.
    pub recipe_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompilerIdentity {
    pub name: String,
    pub version: String,
    pub ir_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectIdentity {
    /// Manifest file name.
    pub manifest: String,
    /// `api.name` from the manifest.
    pub api: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockedFile {
    /// Path relative to the manifest directory with `/` separators, or the
    /// credential-free source URL of a remote input.
    pub path: String,
    pub role: FileRole,
    /// `blake3:` digest of the bytes the compiler read.
    pub digest: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FileRole {
    Manifest,
    Profile,
    Agent,
    Input,
    RemoteInput,
    Reference,
    Overlay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockedInput {
    pub namespace: String,
    pub spec: String,
    pub remote: bool,
    /// Reviewed remote provenance of a converted import, when declared.
    pub source_url: Option<String>,
    pub source_sha256: Option<String>,
    /// Per-input overlays, then project overlays, in application order.
    pub overlays: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockedProfile {
    pub source: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockedTarget {
    pub name: String,
    pub emitter: EmitterIdentity,
    /// `blake3:` digest of the effective target options (after profiles), as
    /// canonical JSON. Option values themselves are never recorded.
    pub options_digest: String,
    /// The target's `out` directory as written in the manifest, if any.
    pub out: Option<String>,
    pub runtime_family: Option<String>,
    /// Tools needed to install or build the generated consumer, not to generate it.
    pub consumer_toolchain: Vec<ToolchainRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EmitterIdentity {
    pub kind: EmitterKind,
    /// `tungsten <version> <target>` for built-in emitters; null otherwise.
    pub identity: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EmitterKind {
    BuiltIn,
    /// An external command; its identity is not verified or approved.
    External,
    Reserved,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolchainRequirement {
    pub tool: String,
    pub requirement: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecipeGap {
    pub kind: GapKind,
    pub subject: String,
    pub detail: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    /// Generation runs a command whose identity tungsten cannot verify.
    ExternalEmitter,
    /// A remote input reproduces offline only from its retained input cache.
    RemoteInput,
    /// A file outside the project directory; not portable by relative path.
    OutsideProject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Guarantees {
    /// True when every input is a local file in the project and every target
    /// is built in, so generation needs no network, cache or external tool.
    pub offline_generation: bool,
    /// Always false: package-manager dependencies of generated consumers are
    /// not vendored, and their installation may need the network.
    pub offline_consumer_build: bool,
}

/// `tungsten lock` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct LockResult {
    /// The lock file, relative to the current directory as given.
    pub lock_file: String,
    pub state: LockState,
    /// Whether this run wrote the lock file.
    pub written: bool,
    /// The recipe of the project as it is now.
    pub recipe: GenerationRecipe,
    /// Differences from the existing lock. Empty when it is current or absent.
    pub drift: Vec<LockDrift>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LockState {
    /// No lock existed.
    Missing,
    /// The existing lock matches the project.
    Current,
    /// The existing lock records different identities.
    Stale,
    /// The existing lock cannot be read, has an unknown version or its
    /// digest does not match its content.
    Corrupt,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct LockDrift {
    pub kind: DriftKind,
    pub subject: String,
    pub change: DriftChange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DriftKind {
    Compiler,
    Project,
    File,
    Input,
    Profile,
    Target,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DriftChange {
    Added,
    Removed,
    Changed,
}

/// JSON Schema of `tungsten.lock.json`.
pub(crate) fn json_schema() -> Value {
    let mut schema = schemars::generate::SchemaSettings::draft2020_12()
        .for_deserialize()
        .into_generator()
        .into_root_schema_for::<GenerationRecipe>()
        .to_value();
    if let Some(object) = schema.as_object_mut() {
        object.insert(
            "title".into(),
            Value::String("tungsten generation lock v1".into()),
        );
    }
    schema
}

pub(crate) fn run(args: &LockArgs) -> Report {
    let mut report = Report::new(CommandName::Lock);
    let manifest = match input::resolve(&args.path) {
        Ok(input::Input::Project(manifest)) => manifest,
        _ => {
            return report.failed(
                exit::FAILED,
                CliError::new(
                    ErrorKind::Usage,
                    "lock requires a local project directory or tungsten manifest",
                )
                .with_help(
                    "pass a directory containing tungsten.yml or a local tungsten*.yml manifest",
                ),
            );
        }
    };
    let base = manifest.parent().map(Path::to_path_buf).unwrap_or_default();
    let lock_path = input::join(&base, LOCK_FILE);
    let compiled = compile_offline(&manifest);
    report.sources = compiled.workspace.sources.clone();
    report.diagnostics = compiled.diagnostics.0.clone();
    let Some(recipe) = capture(&manifest, &compiled) else {
        report.exit = exit::FAILED;
        report.error = Some(
            CliError::new(
                ErrorKind::Usage,
                "the project does not compile offline, so no recipe can be captured or certified",
            )
            .with_help("fix the reported diagnostics; a remote input must already be pinned in .tungsten-inputs (run `tungsten check` online once); lock never fetches or refreshes inputs"),
        );
        return report;
    };
    let existing = read_lock(&lock_path);
    let (state, drift) = match &existing {
        Existing::Missing => (LockState::Missing, vec![]),
        Existing::Corrupt => (LockState::Corrupt, vec![]),
        Existing::Valid(locked) => {
            let drift = compare(locked, &recipe);
            if drift.is_empty() {
                (LockState::Current, drift)
            } else {
                (LockState::Stale, drift)
            }
        }
    };
    let shown = lock_path.display().to_string();
    let mut written = false;
    if args.check {
        report.exit = if state == LockState::Current {
            exit::OK
        } else {
            exit::FAILED
        };
    } else if args.preview {
        report.exit = exit::OK;
    } else {
        match state {
            LockState::Current => {}
            LockState::Missing => written = true,
            LockState::Stale | LockState::Corrupt if args.update => written = true,
            LockState::Stale | LockState::Corrupt => {
                report.exit = exit::REFUSED;
                report.error = Some(
                    CliError::new(
                        ErrorKind::Refused,
                        format!("{shown} does not match the project; refusing to replace locked identities"),
                    )
                    .with_help("review the drift, then pass --update to accept the current identities"),
                );
            }
        }
        if written && let Err(error) = write_lock(&lock_path, &recipe) {
            return report.failed(
                exit::INTERNAL,
                CliError::new(ErrorKind::Io, format!("cannot write {shown}: {error}"))
                    .with_help("check that the project directory is writable"),
            );
        }
    }
    report.human = human(&shown, state, written, &recipe, &drift, args.check);
    report.result = Some(CommandResult::Lock(Box::new(LockResult {
        lock_file: shown,
        state,
        written,
        recipe,
        drift,
    })));
    report
}

/// Compile with acquisition forced offline: pinned remote inputs are reused
/// from the project's input cache and nothing is fetched or refreshed.
pub(crate) fn compile_offline(manifest: &Path) -> Compiled {
    let mut opts = CompileOptions::default();
    opts.load.acquisition = tungsten_openapi::acquire::current_options();
    opts.load.acquisition.offline = true;
    opts.load.acquisition.refresh = false;
    compile_project(manifest, &opts)
}

/// Build the recipe of a compiled project; `None` when it did not compile.
pub(crate) fn capture(manifest: &Path, compiled: &Compiled) -> Option<GenerationRecipe> {
    if compiled.has_errors() {
        return None;
    }
    let ir = compiled.ir.as_ref()?;
    let config = compiled.config.as_ref()?;
    let manifest_name = manifest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tungsten.yml".into());
    let profiles: Vec<LockedProfile> = compiled
        .effective
        .as_ref()
        .map(|effective| {
            effective
                .profiles
                .iter()
                .map(|profile| LockedProfile {
                    source: portable(&profile.name),
                    digest: profile.digest.0.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let mut gaps = vec![];
    let mut files = vec![];
    for (name, digest) in &ir.generator.inputs {
        let role = role_of(name, config, &manifest_name, &profiles);
        let path = portable(name);
        if role != FileRole::RemoteInput && outside_project(name) {
            gaps.push(RecipeGap {
                kind: GapKind::OutsideProject,
                subject: path.clone(),
                detail:
                    "lies outside the project directory; a bundle cannot carry it by relative path"
                        .into(),
            });
        }
        files.push(LockedFile {
            path,
            role,
            digest: digest.0.clone(),
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let inputs: Vec<LockedInput> = config
        .inputs
        .iter()
        .map(|input| {
            let remote = tungsten_openapi::acquire::is_remote(&input.spec);
            if remote || input.source_url.is_some() {
                gaps.push(RecipeGap {
                    kind: GapKind::RemoteInput,
                    subject: input.namespace.clone(),
                    detail: "reproduces offline only from the retained .tungsten-inputs pin; refreshing requires an explicit --refresh-inputs".into(),
                });
            }
            LockedInput {
                namespace: input.namespace.clone(),
                spec: portable(&input.spec),
                remote,
                source_url: input.source_url.clone(),
                source_sha256: input.source_sha256.clone(),
                overlays: input
                    .overlays
                    .iter()
                    .chain(&config.overlays)
                    .map(|overlay| portable(overlay))
                    .collect(),
            }
        })
        .collect();
    let targets: Vec<LockedTarget> = config
        .targets
        .iter()
        .map(|(name, options)| {
            let external = matches!(tungsten_config::external_target(options), Ok(Some(_)));
            let capability = tungsten_config::capabilities::target(name);
            let kind = match capability {
                _ if external => EmitterKind::External,
                Some(c) if c.source_support == tungsten_config::capabilities::SourceSupport::Reserved => {
                    EmitterKind::Reserved
                }
                Some(_) => EmitterKind::BuiltIn,
                None => EmitterKind::Unknown,
            };
            if kind == EmitterKind::External {
                gaps.push(RecipeGap {
                    kind: GapKind::ExternalEmitter,
                    subject: name.clone(),
                    detail: "runs an external command whose identity is not recorded or verified; the lock grants it no trust".into(),
                });
            }
            let builtin = capability.filter(|_| kind == EmitterKind::BuiltIn);
            LockedTarget {
                name: name.clone(),
                emitter: EmitterIdentity {
                    kind,
                    identity: builtin
                        .map(|c| format!("tungsten {TUNGSTEN_VERSION} {}", c.id)),
                },
                options_digest: Digest::of(canonical(options).to_string().as_bytes()).0,
                out: options.get("out").and_then(Value::as_str).map(portable),
                runtime_family: builtin.and_then(|c| c.runtime_family).map(str::to_string),
                consumer_toolchain: builtin
                    .map(|c| {
                        c.prerequisites
                            .iter()
                            .map(|tool| {
                                let tool = tool.for_options(options);
                                ToolchainRequirement {
                                    tool: tool.tool.to_string(),
                                    requirement: tool.description(),
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        })
        .collect();
    gaps.sort();
    gaps.dedup();
    let offline_generation = gaps.is_empty();
    let mut recipe = GenerationRecipe {
        tungsten_lock: LOCK_VERSION,
        compiler: CompilerIdentity {
            name: "tungsten".into(),
            version: TUNGSTEN_VERSION.into(),
            ir_version: ir.ir_version.clone(),
        },
        project: ProjectIdentity {
            manifest: manifest_name,
            api: config.api.name.clone(),
        },
        files,
        inputs,
        profiles,
        targets,
        gaps,
        guarantees: Guarantees {
            offline_generation,
            offline_consumer_build: false,
        },
        recipe_digest: String::new(),
    };
    recipe.recipe_digest = digest_of(&recipe);
    Some(recipe)
}

fn role_of(
    name: &str,
    config: &TungstenConfig,
    manifest: &str,
    profiles: &[LockedProfile],
) -> FileRole {
    if name == manifest {
        FileRole::Manifest
    } else if profiles.iter().any(|p| p.source == portable(name)) {
        FileRole::Profile
    } else if config.agent.as_deref().unwrap_or("agent.yml") == name {
        FileRole::Agent
    } else if config
        .inputs
        .iter()
        .flat_map(|input| &input.overlays)
        .chain(&config.overlays)
        .any(|overlay| overlay == name)
    {
        FileRole::Overlay
    } else if tungsten_openapi::acquire::is_remote(name) {
        FileRole::RemoteInput
    } else if config.inputs.iter().any(|input| input.spec == name) {
        FileRole::Input
    } else {
        FileRole::Reference
    }
}

/// Manifest-relative path with `/` separators; an absolute path keeps only
/// its file name so the lock never embeds machine-specific locations.
fn portable(name: &str) -> String {
    if tungsten_openapi::acquire::is_remote(name) {
        return name.to_string();
    }
    let path = Path::new(name);
    if path.is_absolute() || name.starts_with('/') || name.starts_with('\\') {
        let file = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        return format!("[absolute]/{file}");
    }
    let parts: Vec<String> = path
        .components()
        .filter(|c| *c != Component::CurDir)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.join("/")
}

fn outside_project(name: &str) -> bool {
    let path = Path::new(name);
    path.is_absolute()
        || name.starts_with('/')
        || name.starts_with('\\')
        || path.components().any(|c| c == Component::ParentDir)
}

/// JSON with object keys sorted at every level.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, Value> =
                map.iter().map(|(k, v)| (k, canonical(v))).collect();
            Value::Object(sorted.into_iter().map(|(k, v)| (k.clone(), v)).collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

pub(crate) fn digest_of(recipe: &GenerationRecipe) -> String {
    let mut unsigned = recipe.clone();
    unsigned.recipe_digest = String::new();
    let value = serde_json::to_value(&unsigned).unwrap_or(Value::Null);
    Digest::of(canonical(&value).to_string().as_bytes()).0
}

pub(crate) enum Existing {
    Missing,
    Corrupt,
    Valid(Box<GenerationRecipe>),
}

/// Read a lock back: a bounded regular file (never a link) whose version and
/// digest check out. Anything else is corrupt and cannot certify anything.
pub(crate) fn read_lock(path: &Path) -> Existing {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Existing::Missing,
        Err(_) => return Existing::Corrupt,
    };
    if !meta.file_type().is_file() || meta.len() > MAX_LOCK_BYTES {
        return Existing::Corrupt;
    }
    let mut bytes = vec![];
    let read = std::fs::File::open(path)
        .and_then(|file| file.take(MAX_LOCK_BYTES + 1).read_to_end(&mut bytes));
    if read.is_err() || bytes.len() as u64 > MAX_LOCK_BYTES {
        return Existing::Corrupt;
    }
    match serde_json::from_slice::<GenerationRecipe>(&bytes) {
        Ok(recipe)
            if recipe.tungsten_lock == LOCK_VERSION
                && digest_of(&recipe) == recipe.recipe_digest =>
        {
            Existing::Valid(Box::new(recipe))
        }
        _ => Existing::Corrupt,
    }
}

/// Write through a temporary sibling and rename, so readers never observe a
/// partial lock. An existing link at the lock path is replaced, not followed.
fn write_lock(path: &Path, recipe: &GenerationRecipe) -> std::io::Result<()> {
    let mut text = serde_json::to_string_pretty(recipe).map_err(std::io::Error::other)?;
    text.push('\n');
    let mut temporary = PathBuf::from(path);
    temporary.set_file_name(format!(".{LOCK_FILE}.{}.tmp", std::process::id()));
    {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

/// Differences between a valid lock and the current recipe, sorted.
pub(crate) fn compare(locked: &GenerationRecipe, current: &GenerationRecipe) -> Vec<LockDrift> {
    let mut drift = vec![];
    if locked.compiler != current.compiler {
        drift.push(LockDrift {
            kind: DriftKind::Compiler,
            subject: format!(
                "tungsten {} (IR {})",
                current.compiler.version, current.compiler.ir_version
            ),
            change: DriftChange::Changed,
        });
    }
    if locked.project != current.project {
        drift.push(LockDrift {
            kind: DriftKind::Project,
            subject: current.project.manifest.clone(),
            change: DriftChange::Changed,
        });
    }
    keyed(
        &mut drift,
        DriftKind::File,
        locked.files.iter().map(|f| (f.path.clone(), f)),
        current.files.iter().map(|f| (f.path.clone(), f)),
    );
    keyed(
        &mut drift,
        DriftKind::Input,
        locked.inputs.iter().map(|i| (i.namespace.clone(), i)),
        current.inputs.iter().map(|i| (i.namespace.clone(), i)),
    );
    keyed(
        &mut drift,
        DriftKind::Target,
        locked.targets.iter().map(|t| (t.name.clone(), t)),
        current.targets.iter().map(|t| (t.name.clone(), t)),
    );
    // Profile application order is part of the identity.
    if locked.profiles != current.profiles {
        keyed(
            &mut drift,
            DriftKind::Profile,
            locked.profiles.iter().map(|p| (p.source.clone(), p)),
            current.profiles.iter().map(|p| (p.source.clone(), p)),
        );
        if !drift.iter().any(|d| d.kind == DriftKind::Profile) {
            drift.push(LockDrift {
                kind: DriftKind::Profile,
                subject: "application order".into(),
                change: DriftChange::Changed,
            });
        }
    }
    // Input order matters as well; a reordering alone shows as a project change.
    let order = |r: &GenerationRecipe| {
        r.inputs
            .iter()
            .map(|i| i.namespace.clone())
            .collect::<Vec<_>>()
    };
    if order(locked) != order(current) && !drift.iter().any(|d| d.kind == DriftKind::Input) {
        drift.push(LockDrift {
            kind: DriftKind::Input,
            subject: "input order".into(),
            change: DriftChange::Changed,
        });
    }
    drift.sort();
    drift
}

fn keyed<'a, T: PartialEq + 'a>(
    drift: &mut Vec<LockDrift>,
    kind: DriftKind,
    locked: impl Iterator<Item = (String, &'a T)>,
    current: impl Iterator<Item = (String, &'a T)>,
) {
    let locked: BTreeMap<String, &T> = locked.collect();
    let current: BTreeMap<String, &T> = current.collect();
    for (key, old) in &locked {
        match current.get(key) {
            None => drift.push(LockDrift {
                kind,
                subject: key.clone(),
                change: DriftChange::Removed,
            }),
            Some(new) if new != old => drift.push(LockDrift {
                kind,
                subject: key.clone(),
                change: DriftChange::Changed,
            }),
            Some(_) => {}
        }
    }
    for key in current.keys().filter(|key| !locked.contains_key(*key)) {
        drift.push(LockDrift {
            kind,
            subject: key.clone(),
            change: DriftChange::Added,
        });
    }
}

fn human(
    lock_file: &str,
    state: LockState,
    written: bool,
    recipe: &GenerationRecipe,
    drift: &[LockDrift],
    check: bool,
) -> String {
    let mut text = String::new();
    let status = match (state, written) {
        (LockState::Missing, true) => "created",
        (LockState::Missing, false) => "missing",
        (LockState::Current, _) => "current",
        (LockState::Stale, true) | (LockState::Corrupt, true) => "updated",
        (LockState::Stale, false) => "stale",
        (LockState::Corrupt, false) => "corrupt (unreadable, unknown version or digest mismatch)",
    };
    let _ = writeln!(text, "{lock_file}: {status}");
    let _ = writeln!(
        text,
        "  recipe {} for {} (tungsten {}, IR {})",
        recipe.recipe_digest,
        recipe.project.api,
        recipe.compiler.version,
        recipe.compiler.ir_version
    );
    let _ = writeln!(
        text,
        "  {} files, {} inputs, {} profiles, {} targets",
        recipe.files.len(),
        recipe.inputs.len(),
        recipe.profiles.len(),
        recipe.targets.len()
    );
    if !check {
        for file in &recipe.files {
            let _ = writeln!(text, "    {:?} {} {}", file.role, file.path, file.digest);
        }
        for target in &recipe.targets {
            let tools = target
                .consumer_toolchain
                .iter()
                .map(|t| format!("{} {}", t.tool, t.requirement))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                text,
                "    target {} ({:?}){}",
                target.name,
                target.emitter.kind,
                if tools.is_empty() {
                    String::new()
                } else {
                    format!("; consumer needs {tools}")
                }
            );
        }
    }
    for item in drift {
        let _ = writeln!(
            text,
            "  drift: {:?} {} {:?}",
            item.kind, item.subject, item.change
        );
    }
    for gap in &recipe.gaps {
        let _ = writeln!(
            text,
            "  gap: {:?} {}: {}",
            gap.kind, gap.subject, gap.detail
        );
    }
    let _ = writeln!(
        text,
        "  offline generation: {}; offline consumer build: not guaranteed (package dependencies are not vendored)",
        if recipe.guarantees.offline_generation {
            "self-contained"
        } else {
            "incomplete, see gaps"
        }
    );
    text.push_str("The lock records identities only. It grants no trust to external emitters and is not an approval.\n");
    text
}
