// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten bundle export|import`: portable single-project source bundles.
//!
//! A bundle is a plain directory: the project's declared input closure at its
//! manifest-relative paths, the current generation lock and an inventory
//! (`tungsten-bundle.json`) naming every file with its size and digest. Export
//! copies only the files the lock records, never credentials, input caches,
//! generated output or unrelated files. Import validates the whole inventory
//! (limits, path confinement, no links, digests, lock consistency) before it
//! writes anything, never overwrites a differing file, and runs nothing.
//! Bundled metadata grants no trust: external emitters and execution policy
//! are evaluated again wherever the project is generated.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tungsten_core::Digest;

use crate::args::{BundleExportArgs, BundleImportArgs};
use crate::commands::lock::{self, Existing, FileRole, GapKind, GenerationRecipe};
use crate::output::{CliError, CommandName, CommandResult, ErrorKind};
use crate::{Report, exit, input};

/// Bundle inventory format version.
const BUNDLE_VERSION: u32 = 1;
/// File name of the inventory at the bundle root.
pub(crate) const INVENTORY_FILE: &str = "tungsten-bundle.json";
const MAX_INVENTORY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_FILES: usize = 10_000;
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 1024;
const MAX_DEPTH: usize = 32;
const NOTICE: &str = "A bundle may contain private API contracts and is not automatically safe to publish. It grants no trust: external emitters and execution policy are evaluated again where it is generated.";

/// `tungsten-bundle.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BundleInventory {
    /// Inventory format version. Must be 1.
    pub tungsten_bundle: u32,
    /// `recipe_digest` of the bundled `tungsten.lock.json`.
    pub recipe_digest: String,
    pub manifest: String,
    /// Every file in the bundle, sorted by path.
    pub files: Vec<BundleFile>,
    /// Recorded inputs the bundle does not carry, with the reason. Sorted.
    pub excluded: Vec<ExcludedFile>,
    /// True when nothing is excluded and no target needs an external emitter.
    pub complete: bool,
    pub notice: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BundleFile {
    /// Relative path with `/` separators.
    pub path: String,
    /// The file's role in the lock; null for the lock itself.
    pub role: Option<FileRole>,
    pub bytes: u64,
    /// `blake3:` digest of the content.
    pub digest: String,
    /// The name suggests credential or key material.
    pub sensitive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExcludedFile {
    pub path: String,
    pub reason: String,
}

/// `tungsten bundle export` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct BundleExportResult {
    /// The bundle directory written; null for a preview.
    pub bundle_dir: Option<String>,
    pub written: bool,
    pub inventory: BundleInventory,
}

/// `tungsten bundle import` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct BundleImportResult {
    pub destination: String,
    pub written: bool,
    pub recipe_digest: String,
    pub complete: bool,
    pub imported: Vec<ImportedFile>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ImportedFile {
    pub path: String,
    pub status: ImportStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportStatus {
    /// Written by this import, or would be written by a preview.
    New,
    /// Already present with the same content; left untouched.
    Identical,
}

/// JSON Schema of `tungsten-bundle.json`.
pub(crate) fn json_schema() -> serde_json::Value {
    let mut schema = schemars::generate::SchemaSettings::draft2020_12()
        .for_deserialize()
        .into_generator()
        .into_root_schema_for::<BundleInventory>()
        .to_value();
    if let Some(object) = schema.as_object_mut() {
        object.insert(
            "title".into(),
            "tungsten project bundle inventory v1".into(),
        );
    }
    schema
}

pub(crate) fn export(args: &BundleExportArgs) -> Report {
    let mut report = Report::new(CommandName::BundleExport);
    let manifest = match input::resolve(&args.path) {
        Ok(input::Input::Project(manifest)) => manifest,
        _ => {
            return report.failed(
                exit::FAILED,
                CliError::new(
                    ErrorKind::Usage,
                    "bundle export requires a local project directory or tungsten manifest",
                )
                .with_help(
                    "pass a directory containing tungsten.yml or a local tungsten*.yml manifest",
                ),
            );
        }
    };
    let base = manifest.parent().map(Path::to_path_buf).unwrap_or_default();
    let compiled = lock::compile_offline(&manifest);
    report.sources = compiled.workspace.sources.clone();
    report.diagnostics = compiled.diagnostics.0.clone();
    let Some(recipe) = lock::capture(&manifest, &compiled) else {
        return report.failed(
            exit::FAILED,
            CliError::new(
                ErrorKind::Usage,
                "the project does not compile offline, so its input closure cannot be exported",
            )
            .with_help(
                "fix the reported diagnostics; bundle export never fetches or refreshes inputs",
            ),
        );
    };
    let lock_path = input::join(&base, lock::LOCK_FILE);
    let current = match lock::read_lock(&lock_path) {
        Existing::Valid(locked) => lock::compare(&locked, &recipe).is_empty(),
        _ => false,
    };
    if !current {
        return report.failed(
            exit::FAILED,
            CliError::new(
                ErrorKind::Usage,
                format!("{} is missing, stale or corrupt", lock_path.display()),
            )
            .with_help("review the project, run `tungsten lock` (or `tungsten lock --update`), then export again; a bundle carries only reviewed, locked identities"),
        );
    }
    let inventory = match inventory(&base, &recipe, &lock_path, args.include_input_cache) {
        Ok(inventory) => inventory,
        Err(message) => {
            return report.failed(
                exit::FAILED,
                CliError::new(ErrorKind::Io, message).with_help(
                    "run `tungsten lock --check`; an input changed or is not a regular file",
                ),
            );
        }
    };
    let mut written = false;
    let mut bundle_dir = None;
    if let Some(output) = &args.output {
        if !inventory.complete && !args.allow_incomplete {
            report.exit = exit::REFUSED;
            report.error = Some(
                CliError::new(
                    ErrorKind::Refused,
                    "the bundle would be incomplete; refusing to export it",
                )
                .with_help("review the excluded inputs and external emitters with --preview, then pass --allow-incomplete to export anyway"),
            );
        } else if inventory.files.iter().any(|f| f.sensitive) && !args.allow_sensitive {
            report.exit = exit::REFUSED;
            report.error = Some(
                CliError::new(
                    ErrorKind::Refused,
                    "a declared input is named like credential or key material; refusing to export it",
                )
                .with_help("review the files marked sensitive with --preview, then pass --allow-sensitive to export them"),
            );
        } else if let Err(message) = write_bundle(&base, output, &inventory) {
            return report.failed(
                exit::INTERNAL,
                CliError::new(ErrorKind::Io, message).with_help(
                    "choose a new path in an existing, writable directory; export never overwrites",
                ),
            );
        } else {
            written = true;
            bundle_dir = Some(output.display().to_string());
        }
    }
    report.human = export_human(&inventory, bundle_dir.as_deref());
    report.result = Some(CommandResult::BundleExport(Box::new(BundleExportResult {
        bundle_dir,
        written,
        inventory,
    })));
    report
}

fn inventory(
    base: &Path,
    recipe: &GenerationRecipe,
    lock_path: &Path,
    include_input_cache: bool,
) -> Result<BundleInventory, String> {
    let mut files = vec![];
    let mut excluded = vec![];
    let mut total = 0u64;
    let mut cached = BTreeSet::new();
    if include_input_cache {
        // Remote entry documents and the reviewed sources of converted imports
        // reproduce offline from exactly their verified pins.
        let sources = recipe.inputs.iter().flat_map(|input| {
            input
                .remote
                .then_some(input.spec.as_str())
                .into_iter()
                .chain(input.source_url.as_deref())
        });
        for source in sources {
            let pinned =
                tungsten_openapi::acquire::pinned_files(source, base).map_err(|error| {
                    format!(
                        "cannot include the pinned cache of {source}: {}",
                        error.message
                    )
                })?;
            cached.insert(source.to_string());
            for path in pinned {
                if files.iter().any(|f: &BundleFile| f.path == path) {
                    continue;
                }
                let bytes = read_regular(&base.join(&path), base)
                    .map_err(|reason| format!("cannot export {path}: {reason}"))?;
                total += bytes.len() as u64;
                files.push(BundleFile {
                    path,
                    role: Some(FileRole::InputCache),
                    bytes: bytes.len() as u64,
                    digest: Digest::of(&bytes).0,
                    sensitive: false,
                });
            }
        }
    }
    for file in &recipe.files {
        if file.role == FileRole::RemoteInput && cached.contains(&file.path) {
            continue;
        }
        if file.role == FileRole::RemoteInput {
            excluded.push(ExcludedFile {
                path: file.path.clone(),
                reason: "remote input; its pinned cache in .tungsten-inputs is not bundled (see --include-input-cache)".into(),
            });
            continue;
        }
        if safe_relative(&file.path).is_none() {
            excluded.push(ExcludedFile {
                path: file.path.clone(),
                reason: "outside the project directory".into(),
            });
            continue;
        }
        let bytes = read_regular(&base.join(&file.path), base)
            .map_err(|reason| format!("cannot export {}: {reason}", file.path))?;
        if Digest::of(&bytes).0 != file.digest {
            return Err(format!("{} changed since it was compiled", file.path));
        }
        total += bytes.len() as u64;
        files.push(BundleFile {
            path: file.path.clone(),
            role: Some(file.role),
            bytes: bytes.len() as u64,
            digest: file.digest.clone(),
            sensitive: sensitive(&file.path),
        });
    }
    for input in &recipe.inputs {
        if let Some(source) = &input.source_url
            && !cached.contains(source)
        {
            excluded.push(ExcludedFile {
                path: source.clone(),
                reason: "reviewed source of a converted import; its pin is verified offline but not bundled (see --include-input-cache)".into(),
            });
        }
    }
    let lock_bytes = read_regular(lock_path, base)
        .map_err(|reason| format!("cannot export {}: {reason}", lock::LOCK_FILE))?;
    total += lock_bytes.len() as u64;
    files.push(BundleFile {
        path: lock::LOCK_FILE.into(),
        role: None,
        bytes: lock_bytes.len() as u64,
        digest: Digest::of(&lock_bytes).0,
        sensitive: false,
    });
    if files.len() > MAX_FILES || total > MAX_TOTAL_BYTES {
        return Err("the input closure exceeds the bundle limits (10000 files, 256 MiB)".into());
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    excluded.sort();
    let complete = excluded.is_empty()
        && !recipe
            .gaps
            .iter()
            .any(|gap| gap.kind == GapKind::ExternalEmitter);
    Ok(BundleInventory {
        tungsten_bundle: BUNDLE_VERSION,
        recipe_digest: recipe.recipe_digest.clone(),
        manifest: recipe.project.manifest.clone(),
        files,
        excluded,
        complete,
        notice: NOTICE.into(),
    })
}

/// Copy into a fresh sibling staging directory, verify every digest again,
/// then publish it with one rename. Never replaces an existing entry.
fn write_bundle(base: &Path, output: &Path, inventory: &BundleInventory) -> Result<(), String> {
    if std::fs::symlink_metadata(output).is_ok() {
        return Err(format!("{} already exists", output.display()));
    }
    let name = output
        .file_name()
        .ok_or_else(|| format!("{} does not name a new directory", output.display()))?;
    let staging = output.with_file_name(format!(
        ".{}.{}.partial",
        name.to_string_lossy(),
        std::process::id()
    ));
    std::fs::create_dir(&staging)
        .map_err(|error| format!("cannot create {}: {error}", staging.display()))?;
    let result = (|| {
        for file in &inventory.files {
            let bytes = read_regular(&base.join(&file.path), base)?;
            if Digest::of(&bytes).0 != file.digest {
                return Err(format!("{} changed during export", file.path));
            }
            write_new(&staging, &file.path, &bytes)?;
        }
        let mut text = serde_json::to_string_pretty(inventory).map_err(|e| e.to_string())?;
        text.push('\n');
        write_new(&staging, INVENTORY_FILE, text.as_bytes())?;
        if std::fs::symlink_metadata(output).is_ok() {
            return Err(format!("{} already exists", output.display()));
        }
        std::fs::rename(&staging, output)
            .map_err(|error| format!("cannot publish {}: {error}", output.display()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

pub(crate) fn import(args: &BundleImportArgs) -> Report {
    let mut report = Report::new(CommandName::BundleImport);
    let fail = |report: Report, message: String| {
        report.failed(
            exit::FAILED,
            CliError::new(ErrorKind::Usage, format!("refusing bundle: {message}")).with_help(
                "obtain an intact bundle from `tungsten bundle export`; nothing was written",
            ),
        )
    };
    let (inventory, contents) = match validate(&args.bundle) {
        Ok(validated) => validated,
        Err(message) => return fail(report, message),
    };
    let dest = &args.dest;
    match std::fs::symlink_metadata(dest) {
        Ok(meta) if meta.file_type().is_dir() => {}
        Ok(_) => {
            return fail(
                report,
                format!("{} exists and is not a directory", dest.display()),
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return fail(
                report,
                format!("cannot inspect {}: {error}", dest.display()),
            );
        }
    }
    let mut imported = vec![];
    let mut conflicts = vec![];
    for (file, bytes) in inventory.files.iter().zip(&contents) {
        match existing(dest, &file.path) {
            Ok(None) => imported.push(ImportedFile {
                path: file.path.clone(),
                status: ImportStatus::New,
            }),
            Ok(Some(present)) if Digest::of(&present).0 == Digest::of(bytes).0 => {
                imported.push(ImportedFile {
                    path: file.path.clone(),
                    status: ImportStatus::Identical,
                })
            }
            Ok(Some(_)) | Err(_) => conflicts.push(file.path.clone()),
        }
    }
    if !conflicts.is_empty() {
        report.exit = exit::REFUSED;
        report.error = Some(
            CliError::new(
                ErrorKind::Refused,
                format!(
                    "{} conflicts with existing entries: {}",
                    dest.display(),
                    conflicts.join(", ")
                ),
            )
            .with_help("import into a new or empty directory, or move the conflicting files; import never overwrites"),
        );
        return report;
    }
    let mut written = false;
    if !args.preview {
        if let Err(error) = std::fs::create_dir_all(dest) {
            return report.failed(
                exit::INTERNAL,
                CliError::new(
                    ErrorKind::Io,
                    format!("cannot create {}: {error}", dest.display()),
                ),
            );
        }
        for ((file, bytes), entry) in inventory.files.iter().zip(&contents).zip(&imported) {
            if entry.status == ImportStatus::New
                && let Err(message) = write_new(dest, &file.path, bytes)
            {
                return report.failed(
                    exit::INTERNAL,
                    CliError::new(ErrorKind::Io, message).with_help(
                        "files written before the failure remain; remove the destination and retry",
                    ),
                );
            }
        }
        written = true;
    }
    let destination = dest.display().to_string();
    report.human = import_human(&destination, &inventory, &imported, written);
    report.result = Some(CommandResult::BundleImport(Box::new(BundleImportResult {
        destination,
        written,
        recipe_digest: inventory.recipe_digest.clone(),
        complete: inventory.complete,
        imported,
    })));
    report
}

/// Validate a bundle completely and return its inventory with each file's
/// content, in inventory order.
fn validate(bundle: &Path) -> Result<(BundleInventory, Vec<Vec<u8>>), String> {
    match std::fs::symlink_metadata(bundle) {
        Ok(meta) if meta.file_type().is_dir() => {}
        _ => return Err(format!("{} is not a bundle directory", bundle.display())),
    }
    let text = read_bounded(&bundle.join(INVENTORY_FILE), MAX_INVENTORY_BYTES)
        .map_err(|reason| format!("{INVENTORY_FILE}: {reason}"))?;
    let inventory: BundleInventory = serde_json::from_slice(&text)
        .map_err(|_| format!("{INVENTORY_FILE} is not a valid bundle inventory"))?;
    if inventory.tungsten_bundle != BUNDLE_VERSION {
        return Err(format!(
            "unsupported bundle version {}",
            inventory.tungsten_bundle
        ));
    }
    if inventory.files.len() > MAX_FILES {
        return Err("more than 10000 files".into());
    }
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    for file in &inventory.files {
        let normalized =
            safe_relative(&file.path).ok_or_else(|| format!("unsafe path {:?}", file.path))?;
        if file.path == INVENTORY_FILE {
            return Err(format!("{INVENTORY_FILE} listed as content"));
        }
        // Case-folded, so no two entries alias on case-insensitive file systems.
        if !seen.insert(normalized.to_lowercase()) {
            return Err(format!("duplicate path {:?}", file.path));
        }
        if file.bytes > MAX_FILE_BYTES {
            return Err(format!("{} exceeds 32 MiB", file.path));
        }
        total = total.saturating_add(file.bytes);
        if total > MAX_TOTAL_BYTES {
            return Err("total size exceeds 256 MiB".into());
        }
    }
    let mut contents = vec![];
    for file in &inventory.files {
        let bytes = read_regular(&bundle.join(&file.path), bundle)
            .map_err(|reason| format!("{}: {reason}", file.path))?;
        if bytes.len() as u64 != file.bytes || Digest::of(&bytes).0 != file.digest {
            return Err(format!(
                "{} does not match its recorded size or digest",
                file.path
            ));
        }
        contents.push(bytes);
    }
    // The bundled lock must be intact and agree with the inventory.
    let lock_index = inventory
        .files
        .iter()
        .position(|f| f.path == lock::LOCK_FILE && f.role.is_none())
        .ok_or_else(|| format!("no {}", lock::LOCK_FILE))?;
    let recipe: GenerationRecipe = serde_json::from_slice(&contents[lock_index])
        .map_err(|_| format!("{} is not a valid lock", lock::LOCK_FILE))?;
    if recipe.tungsten_lock != lock::LOCK_VERSION
        || lock::digest_of(&recipe) != recipe.recipe_digest
        || recipe.recipe_digest != inventory.recipe_digest
    {
        return Err(format!(
            "{} is corrupt or does not match the inventory",
            lock::LOCK_FILE
        ));
    }
    for locked in recipe
        .files
        .iter()
        .filter(|f| f.role != FileRole::RemoteInput)
    {
        let carried = inventory.files.iter().find(|f| f.path == locked.path);
        match carried {
            Some(file) if file.digest == locked.digest && file.role == Some(locked.role) => {}
            Some(_) => return Err(format!("{} differs from the locked identity", locked.path)),
            None if inventory.excluded.iter().any(|e| e.path == locked.path) => {}
            None => return Err(format!("locked input {} is missing", locked.path)),
        }
    }
    for file in &inventory.files {
        if file.role == Some(FileRole::InputCache) && !file.path.starts_with(".tungsten-inputs/") {
            return Err(format!(
                "input cache entry {} lies outside .tungsten-inputs",
                file.path
            ));
        }
    }
    for file in inventory
        .files
        .iter()
        .filter(|f| f.role.is_some_and(|role| role != FileRole::InputCache))
    {
        if !recipe.files.iter().any(|l| l.path == file.path) {
            return Err(format!(
                "{} is not part of the locked input closure",
                file.path
            ));
        }
    }
    Ok((inventory, contents))
}

/// Normalized `/`-separated relative path, or `None` when it could escape
/// its root or is not portable.
fn safe_relative(path: &str) -> Option<String> {
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.contains('\\')
        || path.contains(':')
        || path.contains('\0')
        || path.starts_with('/')
    {
        return None;
    }
    let mut parts = vec![];
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.chars().any(char::is_control) {
            return None;
        }
        parts.push(part);
    }
    if parts.len() > MAX_DEPTH {
        return None;
    }
    let joined = parts.join("/");
    Path::new(&joined)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then_some(joined)
}

/// Read a regular file under `root`, refusing links at every component below it.
fn read_regular(path: &Path, root: &Path) -> Result<Vec<u8>, String> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err("is or lies under a symbolic link".into());
            }
            Ok(_) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    read_bounded(path, MAX_FILE_BYTES)
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.file_type().is_file() {
        return Err("not a regular file".into());
    }
    if meta.len() > limit {
        return Err("exceeds its size limit".into());
    }
    let mut bytes = vec![];
    std::fs::File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("exceeds its size limit".into());
    }
    Ok(bytes)
}

/// The content at `root/relative`, `None` when absent. A link or non-regular
/// entry anywhere along the path is an error (a conflict).
fn existing(root: &Path, relative: &str) -> Result<Option<Vec<u8>>, String> {
    let mut current = root.to_path_buf();
    let parts: Vec<&str> = relative.split('/').collect();
    for (index, part) in parts.iter().enumerate() {
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
            Ok(meta) if meta.file_type().is_symlink() => return Err("symbolic link".into()),
            Ok(meta) if index + 1 < parts.len() && !meta.file_type().is_dir() => {
                return Err("not a directory".into());
            }
            Ok(_) => {}
        }
    }
    read_bounded(&current, MAX_FILE_BYTES).map(Some)
}

/// Create `root/relative` and missing parents; never follows or replaces an
/// existing entry.
fn write_new(root: &Path, relative: &str, bytes: &[u8]) -> Result<(), String> {
    let mut current: PathBuf = root.to_path_buf();
    let parts: Vec<&str> = relative.split('/').collect();
    for part in &parts[..parts.len() - 1] {
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_dir() => {}
            Ok(_) => return Err(format!("{} is not a directory", current.display())),
            Err(_) => std::fs::create_dir(&current)
                .map_err(|error| format!("cannot create {}: {error}", current.display()))?,
        }
    }
    current.push(parts[parts.len() - 1]);
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&current)
        .map_err(|error| format!("cannot create {}: {error}", current.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot write {}: {error}", current.display()))
}

/// Names that commonly hold credentials or keys.
fn sensitive(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name.starts_with(".env")
        || matches!(
            name.as_str(),
            ".npmrc"
                | ".pypirc"
                | ".netrc"
                | "_netrc"
                | ".git-credentials"
                | "id_rsa"
                | "id_ed25519"
        )
        || name.contains("credential")
        || name.contains("secret")
        || [".pem", ".key", ".p12", ".pfx", ".jks", ".keystore"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn export_human(inventory: &BundleInventory, bundle_dir: Option<&str>) -> String {
    let mut text = match bundle_dir {
        Some(dir) => format!("bundle written to {dir}\n"),
        None => "bundle preview (nothing written)\n".to_string(),
    };
    let _ = writeln!(text, "  recipe {}", inventory.recipe_digest);
    for file in &inventory.files {
        let _ = writeln!(
            text,
            "    {} ({} bytes){}",
            file.path,
            file.bytes,
            if file.sensitive {
                "  [named like credential material]"
            } else {
                ""
            }
        );
    }
    for file in &inventory.excluded {
        let _ = writeln!(text, "  excluded: {}: {}", file.path, file.reason);
    }
    let _ = writeln!(
        text,
        "  complete: {}",
        if inventory.complete {
            "yes"
        } else {
            "no (see excluded inputs and external emitters)"
        }
    );
    if inventory
        .files
        .iter()
        .any(|f| f.role == Some(FileRole::InputCache))
    {
        text.push_str("  includes pinned remote input documents from .tungsten-inputs; check that their terms allow redistribution\n");
    }
    let _ = writeln!(text, "{}", inventory.notice);
    text
}

fn import_human(
    destination: &str,
    inventory: &BundleInventory,
    imported: &[ImportedFile],
    written: bool,
) -> String {
    let mut text = if written {
        format!("bundle imported into {destination}\n")
    } else {
        format!("bundle import preview into {destination} (nothing written)\n")
    };
    let _ = writeln!(text, "  recipe {}", inventory.recipe_digest);
    for file in imported {
        let status = match file.status {
            ImportStatus::New => "new",
            ImportStatus::Identical => "identical, kept",
        };
        let _ = writeln!(text, "    {} ({status})", file.path);
    }
    for file in &inventory.excluded {
        let _ = writeln!(text, "  not bundled: {}: {}", file.path, file.reason);
    }
    let _ = writeln!(
        text,
        "Run `tungsten lock --check` in {destination} to verify the locked identities before generating.\n{}",
        inventory.notice
    );
    text
}
