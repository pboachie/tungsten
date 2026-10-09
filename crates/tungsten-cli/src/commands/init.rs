// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten init`: write `tungsten.yml` and `agent.yml` for a new project.
//!
//! The manifests are inferred from the OpenAPI document when one is given
//! or found next to them. Existing manifests are never overwritten without
//! `--force` (exit 4 instead), and nothing is written when either one
//! exists. Any directory entry counts as existing, a dangling symlink
//! included, and a manifest is never written through a symlink: new files
//! are created exclusively, and `--force` replaces a regular file but
//! refuses a symlink.

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use tungsten_openapi::{LoadEntry, LoadOptions};

use crate::args::InitArgs;
use crate::output::{
    CliError, CommandName, CommandResult, ErrorKind, InitAction, InitFile, InitResult,
};
use crate::{Report, exit, input};

/// Spec file names looked for in the target directory without `--from`.
const SPEC_CANDIDATES: [&str; 3] = ["openapi.json", "openapi.yaml", "openapi.yml"];
const TUNGSTEN_SCHEMA_URL: &str = "https://tungsten.dev/schemas/tungsten-v1.json";
const AGENT_SCHEMA_URL: &str = "https://tungsten.dev/schemas/agent-v1.json";
/// Methods with side effects, in the order they are listed.
const MUTATING_METHODS: [&str; 4] = ["post", "put", "patch", "delete"];
/// Name words that say nothing about an API or namespace.
const GENERIC_WORDS: [&str; 6] = ["openapi", "swagger", "spec", "api", "schema", "json"];
const MAX_NAME_LEN: usize = 64;
const FALLBACK_NAME: &str = "api";

/// What the OpenAPI document tells us.
#[derive(Debug, Default)]
struct SpecInfo {
    title: Option<String>,
    server: Option<String>,
    /// Operations with side effects, sorted by path, then method.
    mutating: Vec<MutatingOp>,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct MutatingOp {
    path: String,
    /// Index into [`MUTATING_METHODS`].
    method: usize,
    operation_id: Option<String>,
}

pub(crate) fn run(args: &InitArgs) -> Report {
    let mut report = Report::new(CommandName::Init);
    let dir = &args.dir;
    let spec = args.from.clone().or_else(|| {
        SPEC_CANDIDATES
            .iter()
            .map(|name| input::join(dir, name))
            .find(|p| p.is_file())
    });
    let info = match &spec {
        Some(path) => match read_spec(path) {
            Ok(info) => info,
            Err(failed) => {
                report.diagnostics = failed.diagnostics.0;
                report.sources = failed.sources;
                report.exit = exit::FAILED;
                return report;
            }
        },
        None => SpecInfo::default(),
    };

    let api_name = args
        .name
        .clone()
        .or_else(|| info.title.as_deref().and_then(slug))
        .or_else(|| dir_name(dir).as_deref().and_then(slug))
        .unwrap_or_else(|| FALLBACK_NAME.to_string());
    let namespace = spec
        .as_deref()
        .and_then(namespace_from_path)
        .or_else(|| first_meaningful_word(&api_name))
        .unwrap_or_else(|| FALLBACK_NAME.to_string());

    let manifest_path = input::join(dir, "tungsten.yml");
    let agent_path = input::join(dir, "agent.yml");
    let entry = |p: &PathBuf| std::fs::symlink_metadata(p).ok();
    let existing: Vec<&PathBuf> = [&manifest_path, &agent_path]
        .into_iter()
        .filter(|p| entry(p).is_some())
        .collect();
    if !args.force && !existing.is_empty() {
        let names: Vec<String> = existing.iter().map(|p| p.display().to_string()).collect();
        return report.failed(
            exit::REFUSED,
            CliError::new(
                ErrorKind::Refused,
                format!("refusing to overwrite {}", names.join(" and ")),
            )
            .with_help("pass --force to overwrite"),
        );
    }
    let links: Vec<String> = existing
        .iter()
        .filter(|p| entry(p).is_some_and(|m| !m.file_type().is_file()))
        .map(|p| p.display().to_string())
        .collect();
    if !links.is_empty() {
        return report.failed(
            exit::REFUSED,
            CliError::new(
                ErrorKind::Refused,
                format!(
                    "refusing to write through {}: not a regular file",
                    links.join(" and ")
                ),
            )
            .with_help("remove the symlink or other entry, then run init again"),
        );
    }
    if let Err(err) = std::fs::create_dir_all(dir) {
        return report.failed(
            exit::INTERNAL,
            CliError::new(
                ErrorKind::Io,
                format!("cannot create directory {}: {err}", dir.display()),
            ),
        );
    }
    let spec_ref = spec
        .as_deref()
        .map(|s| spec_reference(s, dir))
        .unwrap_or_else(|| SPEC_CANDIDATES[0].to_string());

    let manifest = tungsten_yml(&api_name, &namespace, &spec_ref, spec.is_some(), &info);
    if tungsten_config::parse_str("tungsten.yml", &manifest)
        .0
        .is_none()
    {
        return report.failed(
            exit::INTERNAL,
            CliError::new(
                ErrorKind::Internal,
                "generated tungsten.yml does not validate",
            )
            .with_help("this is a bug in tungsten; please report it with the spec"),
        );
    }
    let agent = agent_yml(&namespace, &spec_ref, spec.is_some(), &info);

    let mut files = vec![];
    for (path, text) in [(&manifest_path, &manifest), (&agent_path, &agent)] {
        let action = if entry(path).is_some() {
            InitAction::Overwritten
        } else {
            InitAction::Created
        };
        if let Err(err) = write_new(path, text, action == InitAction::Overwritten) {
            return report.failed(
                exit::INTERNAL,
                CliError::new(
                    ErrorKind::Io,
                    format!("cannot write {}: {err}", path.display()),
                ),
            );
        }
        files.push(InitFile {
            path: path.display().to_string(),
            action,
        });
    }

    let result = InitResult {
        dir: dir.display().to_string(),
        api_name,
        namespace,
        spec: spec_ref,
        spec_found: spec.is_some(),
        files,
    };
    report.human = human(&result);
    report.result = Some(CommandResult::Init(result));
    report
}

/// Write `text` to a new file at `path`, never following a symlink: an
/// existing regular file is removed first when `replace`, and the file is
/// created exclusively, so an entry that appeared meanwhile fails instead
/// of being written through.
fn write_new(path: &Path, text: &str, replace: bool) -> std::io::Result<()> {
    use std::io::Write as _;
    if replace {
        std::fs::remove_file(path)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(text.as_bytes())
}

struct SpecFailure {
    diagnostics: tungsten_core::Diagnostics,
    sources: tungsten_core::SourceMap,
}

fn read_spec(path: &Path) -> Result<SpecInfo, SpecFailure> {
    let entry = LoadEntry {
        path: path.to_path_buf(),
        display_name: path.display().to_string(),
        overlays: vec![],
    };
    let mut ws = tungsten_openapi::load(&[entry], &LoadOptions::default());
    let Some(&doc) = ws.entries.first() else {
        ws.diagnostics.sort();
        return Err(SpecFailure {
            diagnostics: ws.diagnostics,
            sources: ws.sources,
        });
    };
    let root = &ws.documents[doc].root;
    let text = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let mut mutating = vec![];
    if let Some(paths) = root.get("paths").and_then(Value::as_object) {
        for (path, item) in paths {
            for (method, name) in MUTATING_METHODS.iter().enumerate() {
                if let Some(op) = item.get(name).filter(|op| op.is_object()) {
                    mutating.push(MutatingOp {
                        path: path.clone(),
                        method,
                        operation_id: text(op.get("operationId")),
                    });
                }
            }
        }
    }
    mutating.sort();
    Ok(SpecInfo {
        title: text(root.pointer("/info/title")),
        server: text(root.pointer("/servers/0/url")),
        mutating,
    })
}

/// Lowercase machine name: ASCII letters and digits joined by `_`,
/// starting with a letter. `None` when nothing usable remains.
fn slug(s: &str) -> Option<String> {
    let words = words(s);
    if words.is_empty() {
        return None;
    }
    let mut name = words.join("_");
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        name.insert_str(0, "api_");
    }
    name.truncate(MAX_NAME_LEN);
    Some(name.trim_end_matches('_').to_string())
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// A namespace from the spec file stem (`public-v1.json` → `public`):
/// the first word that is not a version or a generic word.
fn namespace_from_path(spec: &Path) -> Option<String> {
    first_meaningful_word(&spec.file_stem()?.to_string_lossy())
}

fn first_meaningful_word(s: &str) -> Option<String> {
    words(s).into_iter().find(|w| {
        w.starts_with(|c: char| c.is_ascii_lowercase())
            && !GENERIC_WORDS.contains(&w.as_str())
            && !is_version_word(w)
    })
}

/// `v1`, `v20`.
fn is_version_word(w: &str) -> bool {
    w.strip_prefix('v')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// The directory's own name; `.` and `..` resolve through the file system.
fn dir_name(dir: &Path) -> Option<String> {
    let name = match dir.file_name() {
        Some(name) => name.to_os_string(),
        None => std::fs::canonicalize(dir).ok()?.file_name()?.to_os_string(),
    };
    Some(name.to_string_lossy().into_owned())
}

/// The spec path as written into tungsten.yml: relative to `dir`, with `/`
/// separators. Falls back to the path as given when no relative path exists
/// (for example different drives).
fn spec_reference(spec: &Path, dir: &Path) -> String {
    let relative = std::fs::canonicalize(spec)
        .ok()
        .zip(std::fs::canonicalize(dir).ok())
        .and_then(|(spec, dir)| relative_path(&spec, &dir));
    let path = relative.unwrap_or_else(|| spec.to_path_buf());
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// `target` relative to `base`; both absolute and normalized. `None` when
/// they share no root.
pub(crate) fn relative_path(target: &Path, base: &Path) -> Option<PathBuf> {
    let t: Vec<Component<'_>> = target.components().collect();
    let b: Vec<Component<'_>> = base.components().collect();
    let common = t.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if common == 0 {
        return None;
    }
    let mut out = PathBuf::new();
    for _ in common..b.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c.as_os_str());
    }
    Some(out)
}

/// A YAML scalar: plain when unambiguous, otherwise double-quoted. A JSON
/// string literal is a valid YAML double-quoted scalar.
pub(crate) fn yaml_scalar(s: &str) -> String {
    const RESERVED: [&str; 11] = [
        "true", "false", "null", "yes", "no", "on", "off", "y", "n", "~", "",
    ];
    let plain = s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/'))
        && !RESERVED.contains(&s.to_ascii_lowercase().as_str());
    if plain {
        s.to_string()
    } else {
        Value::String(s.to_string()).to_string()
    }
}

/// Text safe inside a YAML comment: one line, no control characters.
fn comment_text(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn tungsten_yml(
    api_name: &str,
    namespace: &str,
    spec_ref: &str,
    spec_found: bool,
    info: &SpecInfo,
) -> String {
    let mut out = format!(
        "# yaml-language-server: $schema={TUNGSTEN_SCHEMA_URL}\n\
         # Shape manifest: what the generated SDKs look like. Written by `tungsten init`;\n\
         # run `tungsten check` after editing.\n\
         tungsten: 1\n\
         \n\
         api:\n  name: {}\n",
        yaml_scalar(api_name)
    );
    if let Some(title) = &info.title {
        let _ = writeln!(out, "  title: {}", yaml_scalar(title));
    }
    out.push_str("\ninputs:\n");
    if !spec_found {
        out.push_str("  # TODO: point `spec` at your OpenAPI document (relative to this file).\n");
    }
    let _ = write!(
        out,
        "  - namespace: {}\n    spec: {}\n",
        yaml_scalar(namespace),
        yaml_scalar(spec_ref)
    );
    if let Some(server) = &info.server {
        let _ = write!(
            out,
            "\nservers:\n  default: {}\n  allow_override: true\n",
            yaml_scalar(server)
        );
    }
    out.push_str(
        "\n# What `tungsten generate` writes. Other targets: rust (SDK and CLI), mcp\n\
         # (an MCP server on top of the TypeScript SDK).\n\
         targets:\n  \
         typescript: { out: generated/typescript }\n  \
         python: { out: generated/python }\n  \
         docs: { out: generated/docs }\n",
    );
    out
}

const AGENT_DEFAULTS: &str = "\
agent: 1

defaults:
  safety:
    get: read_only
    post: mutating
    put: mutating
    patch: mutating
    delete: destructive
  idempotency: none
  preview: local
  unknown_outcome:
    on_timeout: unknown
    on_connection_reset: unknown
    on_ambiguous_status: unknown
  retries:
    read_only: { max: 3, backoff: { base_ms: 200, max_ms: 5000, jitter: full } }
    mutating: { max: 0 }
    honor_retry_after: true
  disclosure:
    mode: auto
    threshold: 24
    list_budget_tokens: 10000
    description_budget_tokens: 60
    schema_budget_tokens: 600
";

/// Longest path shown in the operation list before alignment stops.
const MAX_PATH_COLUMN: usize = 48;

fn agent_yml(namespace: &str, spec_ref: &str, spec_found: bool, info: &SpecInfo) -> String {
    let mut out = format!(
        "# yaml-language-server: $schema={AGENT_SCHEMA_URL}\n\
         # Agent manifest: how agents may use the API (safety tiers, idempotency,\n\
         # confirmation, remediation). Written by `tungsten init`.\n\
         {AGENT_DEFAULTS}\n\
         # TODO: review every operation with side effects. Add an entry under `tools:`\n\
         # for each one that is destructive or irreversible, needs a caller-owned\n\
         # idempotency key, or deserves remediation text for agents.\n"
    );
    if !spec_found {
        out.push_str("# No OpenAPI document was found; rerun with `--from <spec> --force`.\n");
    } else if info.mutating.is_empty() {
        let _ = writeln!(
            out,
            "# No operations with side effects were found in {}.",
            comment_text(spec_ref)
        );
    } else {
        let _ = writeln!(
            out,
            "# Operations with side effects in {}:",
            comment_text(spec_ref)
        );
        let width = info
            .mutating
            .iter()
            .map(|op| op.path.chars().count())
            .max()
            .unwrap_or(0)
            .min(MAX_PATH_COLUMN);
        for op in &info.mutating {
            let method = MUTATING_METHODS[op.method].to_ascii_uppercase();
            let reference = match &op.operation_id {
                Some(id) => format!("{namespace}.{id}"),
                None => "(no operationId)".to_string(),
            };
            let line = format!(
                "#   {method:<6} {:<width$}  {}",
                comment_text(&op.path),
                comment_text(&reference)
            );
            let _ = writeln!(out, "{}", line.trim_end());
        }
    }
    out.push_str("tools: []\n");
    out
}

fn human(r: &InitResult) -> String {
    let mut out = String::new();
    for f in &r.files {
        let action = match f.action {
            InitAction::Created => "created",
            InitAction::Overwritten => "overwritten",
        };
        let _ = writeln!(out, "wrote {} ({action})", f.path);
    }
    let _ = writeln!(
        out,
        "api {} · namespace {} · spec {}{}",
        r.api_name,
        r.namespace,
        r.spec,
        if r.spec_found { "" } else { " (placeholder)" }
    );
    let _ = writeln!(
        out,
        "next: resolve the TODO markers, then run `tungsten check {}`",
        r.dir
    );
    out
}
