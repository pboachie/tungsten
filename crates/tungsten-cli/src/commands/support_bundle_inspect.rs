// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten support-bundle --inspect <FILE>`: read an existing bundle file
//! locally and show what it holds, after checking it against the bundle schema.
//!
//! Inspection reads one bounded regular file, starts no program, writes
//! nothing and contacts no network. A bundle with an unknown
//! `bundle_schema_version` is refused by number; versions 1 and 2 are read.
//! Every object must have exactly the keys its schema version defines. The six
//! observed entries must be metadata only; version 2 may follow them with up to
//! four `user.attachment[N]` entries labelled user supplied. Recorded sizes and
//! statuses must match the payloads. Error messages name only schema-defined locations; they never echo
//! file content, unknown keys or values.

use std::fmt::Write as _;
use std::io::Read;
use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Map, Value};

use super::support_bundle::{
    BUNDLE_SCHEMA_VERSION, BUNDLE_SCHEMA_VERSION_REHEARSAL, BundleEntryOrigin, BundleEntryStatus,
    BundleSensitivity, ENTRY_IDS, MAX_ENTRY_BYTES, MAX_TOTAL_BYTES, SUPPORTED_SCHEMA_VERSIONS,
};
use super::support_bundle_attach::{MAX_ATTACHMENT_TEXT_BYTES, MAX_ATTACHMENTS, entry_id};
use super::support_bundle_file::MAX_ARTIFACT_BYTES;

/// The `--inspect` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SupportBundleInspection {
    pub mode: BundleInspectMode,
    /// The file's manifest schema version.
    pub bundle_schema_version: u32,
    /// Versions this tungsten reads; others are refused.
    pub supported_schema_versions: Vec<u32>,
    /// Always false: inspection writes nothing.
    pub wrote_files: bool,
    /// Always false: inspection starts no program.
    pub executed_programs: bool,
    /// Size of the file read.
    pub artifact_bytes: usize,
    /// The bundle's recorded `tungsten_version`, when the entry is included.
    pub producer_version: Option<String>,
    /// The bundle's entries in file order with their exact payloads.
    pub entries: Vec<InspectedEntry>,
    /// Sum of the included entries' payload sizes; matches the file.
    pub total_bytes: usize,
    /// Category names the bundle records as excluded by default.
    pub omitted: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BundleInspectMode {
    Inspect,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct InspectedEntry {
    pub id: String,
    /// `user_supplied` entries hold text the user attached (version 2).
    pub origin: BundleEntryOrigin,
    pub sensitivity: BundleSensitivity,
    pub status: BundleEntryStatus,
    pub bytes: usize,
    /// The payload as stored; null unless `included`.
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum InspectError {
    /// No regular file at the path, or it is a symlink.
    NotFound,
    /// Larger than the complete artifact limit.
    TooLarge,
    /// Reading the file failed.
    Io,
    /// Not a JSON object.
    NotJson,
    /// A `bundle_schema_version` this tungsten does not read.
    UnsupportedVersion(u64),
    /// Does not match the schema; the location names schema keys only.
    Invalid(String),
}

pub(super) fn inspect(path: &Path) -> Result<SupportBundleInspection, InspectError> {
    let bytes = read_bounded(path)?;
    let Ok(Value::Object(document)) = serde_json::from_slice::<Value>(&bytes) else {
        return Err(InspectError::NotJson);
    };
    let version = match document.get("bundle_schema_version") {
        Some(Value::Number(number)) => number
            .as_u64()
            .ok_or_else(|| invalid("/bundle_schema_version", "is not a version number"))?,
        _ => return Err(invalid("/bundle_schema_version", "is missing")),
    };
    let Some(version) = SUPPORTED_SCHEMA_VERSIONS
        .into_iter()
        .find(|known| u64::from(*known) == version)
    else {
        return Err(InspectError::UnsupportedVersion(version));
    };
    check_document(&document, version)?;
    let observed_ids = observed_ids(version);
    let mut entries = vec![];
    let mut total = 0usize;
    let mut producer_version = None;
    let listed = document["entries"].as_array().into_iter().flatten();
    for (index, entry) in listed.enumerate() {
        let at = format!("/entries/{index}");
        let id = entry["id"].as_str().unwrap_or_default();
        let expected = observed_ids.get(index).map_or_else(
            || entry_id(index - observed_ids.len()),
            |id| (*id).to_string(),
        );
        if id != expected {
            return Err(invalid(&format!("{at}/id"), "is not the expected entry id"));
        }
        let observed = index < observed_ids.len();
        let status = match entry["status"].as_str() {
            Some("included") => BundleEntryStatus::Included,
            Some("excluded_by_request") => BundleEntryStatus::ExcludedByRequest,
            Some("withheld_size_limit") => BundleEntryStatus::WithheldSizeLimit,
            _ => return Err(invalid(&format!("{at}/status"), "is not a known status")),
        };
        let bytes = as_usize(&entry["bytes"]).unwrap_or(usize::MAX);
        let payload = match (&entry["payload"], status) {
            (Value::Null, BundleEntryStatus::Included) => {
                return Err(invalid(&format!("{at}/payload"), "is missing"));
            }
            (Value::Null, _) if bytes == 0 => None,
            (Value::Null, _) => {
                return Err(invalid(&format!("{at}/bytes"), "is not 0"));
            }
            (_, BundleEntryStatus::Included) => {
                let payload = entry["payload"].clone();
                check(&payload, &payload_shape(id), &format!("{at}/payload"))?;
                if !observed && !matches!(payload["kind"].as_str(), Some("log" | "configuration")) {
                    return Err(invalid(
                        &format!("{at}/payload/kind"),
                        "is not a known attachment kind",
                    ));
                }
                let actual = serde_json::to_string(&payload).map_or(usize::MAX, |s| s.len());
                if actual != bytes || bytes > MAX_ENTRY_BYTES {
                    return Err(invalid(
                        &format!("{at}/bytes"),
                        "does not match the payload size",
                    ));
                }
                total = total.saturating_add(bytes);
                if id == "tool.versions" {
                    producer_version = payload["tungsten_version"].as_str().map(String::from);
                }
                Some(payload)
            }
            (_, _) => {
                return Err(invalid(
                    &format!("{at}/payload"),
                    "is present on an entry that is not included",
                ));
            }
        };
        entries.push(InspectedEntry {
            id: id.to_string(),
            origin: if observed {
                BundleEntryOrigin::Observed
            } else {
                BundleEntryOrigin::UserSupplied
            },
            sensitivity: if observed {
                BundleSensitivity::Metadata
            } else {
                BundleSensitivity::UserSuppliedText
            },
            status,
            bytes,
            payload,
        });
    }
    if entries.len() < observed_ids.len() {
        return Err(invalid("/entries", "does not list every entry"));
    }
    let attachments = entries.len() - observed_ids.len();
    if (version == BUNDLE_SCHEMA_VERSION && attachments > 0) || attachments > MAX_ATTACHMENTS {
        return Err(invalid("/entries", "has too many entries"));
    }
    if as_usize(&document["total_bytes"]) != Some(total) || total > MAX_TOTAL_BYTES {
        return Err(invalid(
            "/total_bytes",
            "does not match the included payload sizes",
        ));
    }
    let omitted = document["omitted"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["category"].as_str().map(String::from))
        .collect();
    Ok(SupportBundleInspection {
        mode: BundleInspectMode::Inspect,
        bundle_schema_version: version,
        supported_schema_versions: SUPPORTED_SCHEMA_VERSIONS.to_vec(),
        wrote_files: false,
        executed_programs: false,
        artifact_bytes: bytes.len(),
        producer_version,
        entries,
        total_bytes: total,
        omitted,
    })
}

fn invalid(location: &str, problem: &str) -> InspectError {
    InspectError::Invalid(format!("{location} {problem}"))
}

fn as_usize(value: &Value) -> Option<usize> {
    value.as_u64().and_then(|n| usize::try_from(n).ok())
}

/// One regular, nonsymlink file of at most the artifact limit. The size is
/// checked again while reading, so a growing file is refused too.
fn read_bounded(path: &Path) -> Result<Vec<u8>, InspectError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| InspectError::NotFound)?;
    if !metadata.is_file() {
        return Err(InspectError::NotFound);
    }
    if metadata.len() > MAX_ARTIFACT_BYTES as u64 {
        return Err(InspectError::TooLarge);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Never follow a symlink swapped in after the check, and never block
        // on a FIFO swapped in for the file.
        options
            .custom_flags((nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK).bits());
    }
    let file = options.open(path).map_err(|_| InspectError::NotFound)?;
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Err(InspectError::NotFound);
    }
    let mut bytes = Vec::new();
    file.take(MAX_ARTIFACT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| InspectError::Io)?;
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(InspectError::TooLarge);
    }
    Ok(bytes)
}

/// The expected JSON shape of a schema value.
enum Shape {
    /// Printable ASCII of at most 512 bytes.
    Text,
    /// User-supplied text: at most `MAX_ATTACHMENT_TEXT_BYTES`, with no
    /// control characters other than newline and tab.
    Attachment,
    /// A snake_case name of at most 64 bytes.
    Token,
    Int,
    Bool,
    Optional(Box<Shape>),
    /// At most 256 items.
    List(Box<Shape>),
    /// Exactly these keys.
    Object(Vec<(&'static str, Shape)>),
}

fn obj(fields: Vec<(&'static str, Shape)>) -> Shape {
    Shape::Object(fields)
}
fn list(item: Shape) -> Shape {
    Shape::List(Box::new(item))
}
fn opt(inner: Shape) -> Shape {
    Shape::Optional(Box::new(inner))
}

/// Top-level keys other than `entries`, which are checked per entry. Both
/// versions share the layout; they differ in privacy level and entry labels.
fn check_document(document: &Map<String, Value>, version: u32) -> Result<(), InspectError> {
    let entry = obj(vec![
        ("id", Shape::Text),
        ("origin", Shape::Token),
        ("sensitivity", Shape::Token),
        ("status", Shape::Token),
        ("bytes", Shape::Int),
        // Payloads are checked by entry id afterwards.
        ("payload", opt(Shape::Object(vec![]))),
    ]);
    let top = [
        ("bundle_schema_version", Shape::Int),
        ("mode", Shape::Token),
        ("wrote_files", Shape::Bool),
        ("executed_programs", Shape::Bool),
        (
            "privacy",
            obj(vec![("level", Shape::Token), ("redaction", Shape::Text)]),
        ),
        (
            "limits",
            obj(vec![
                ("max_entry_bytes", Shape::Int),
                ("max_total_bytes", Shape::Int),
                ("max_diagnostics", Shape::Int),
                ("max_targets", Shape::Int),
            ]),
        ),
        ("entries", list(entry)),
        ("total_bytes", Shape::Int),
        (
            "omitted",
            list(obj(vec![
                ("category", Shape::Token),
                ("description", Shape::Text),
                ("reason", Shape::Token),
            ])),
        ),
    ];
    check_object(document, &top, "")?;
    if document["mode"] != "preview" {
        return Err(invalid("/mode", "is not a known mode"));
    }
    let observed_ids = observed_ids(version);
    let attached = document["entries"]
        .as_array()
        .is_some_and(|entries| entries.len() > observed_ids.len());
    let level = match version {
        BUNDLE_SCHEMA_VERSION => "metadata_only",
        BUNDLE_SCHEMA_VERSION_REHEARSAL if !attached => "metadata_only",
        _ => "metadata_and_user_supplied",
    };
    if document["privacy"]["level"] != level {
        return Err(invalid(
            "/privacy/level",
            "does not match the schema version",
        ));
    }
    for (index, entry) in document["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        if index >= observed_ids.len() && version != BUNDLE_SCHEMA_VERSION {
            if entry["origin"] != "user_supplied" {
                return Err(invalid(
                    &format!("/entries/{index}/origin"),
                    "is not labelled user supplied",
                ));
            }
            if entry["sensitivity"] != "user_supplied_text" {
                return Err(invalid(
                    &format!("/entries/{index}/sensitivity"),
                    "is not labelled user-supplied text",
                ));
            }
            continue;
        }
        if entry["origin"] != "observed" {
            return Err(invalid(
                &format!("/entries/{index}/origin"),
                "is not an observed entry; user-supplied material is not part of schema version 1",
            ));
        }
        if entry["sensitivity"] != "metadata" {
            return Err(invalid(
                &format!("/entries/{index}/sensitivity"),
                "is not metadata",
            ));
        }
    }
    Ok(())
}

/// Observed entry ids in order for a schema version.
fn observed_ids(version: u32) -> Vec<&'static str> {
    let mut ids = ENTRY_IDS.to_vec();
    if version == BUNDLE_SCHEMA_VERSION_REHEARSAL {
        ids.push(super::support_bundle_rehearsal::ENTRY_ID);
    }
    ids
}

fn payload_shape(id: &str) -> Shape {
    match id {
        "upgrade.rehearsal" => obj(vec![
            ("state", Shape::Token),
            ("from_version", opt(Shape::Text)),
            ("to_version", opt(Shape::Text)),
            ("verdict", opt(Shape::Token)),
            ("generated_source", opt(Shape::Token)),
            ("package_availability", opt(Shape::Token)),
            ("runtime_acceptance", opt(Shape::Token)),
            ("apply_state", Shape::Token),
            (
                "targets",
                list(obj(vec![
                    ("target", Shape::Text),
                    ("status", Shape::Token),
                    ("added", Shape::Int),
                    ("changed", Shape::Int),
                    ("removed", Shape::Int),
                    ("conflicts", Shape::Int),
                    ("semver", opt(Shape::Token)),
                ])),
            ),
            ("targets_omitted", Shape::Int),
            ("identity_changes", list(Shape::Token)),
            (
                "findings",
                list(obj(vec![("kind", Shape::Token), ("blocking", Shape::Bool)])),
            ),
            ("findings_omitted", Shape::Int),
        ]),
        "tool.versions" => obj(vec![
            ("tungsten_version", Shape::Text),
            ("cli_output_schema", Shape::Text),
            ("os", Shape::Token),
            ("arch", Shape::Token),
            ("tool_version_probes", Shape::Token),
        ]),
        "capabilities.catalog" => obj(vec![
            ("capability_version", Shape::Int),
            ("execution_policy_version", Shape::Int),
            (
                "targets",
                list(obj(vec![
                    ("id", Shape::Text),
                    ("source_support", Shape::Token),
                ])),
            ),
        ]),
        "project.configuration" => obj(vec![
            ("state", Shape::Token),
            ("configuration_version", opt(Shape::Int)),
            ("profiles", Shape::Int),
            (
                "targets",
                list(obj(vec![
                    ("target", Shape::Text),
                    ("status", Shape::Token),
                    (
                        "fields",
                        list(obj(vec![
                            ("path", Shape::Text),
                            ("state", Shape::Token),
                            ("applicable", Shape::Bool),
                            (
                                "winner",
                                opt(obj(vec![
                                    ("kind", Shape::Token),
                                    ("source", Shape::Text),
                                    ("pointer", Shape::Text),
                                ])),
                            ),
                            ("shadowed", Shape::Int),
                        ])),
                    ),
                    ("fields_omitted", Shape::Int),
                ])),
            ),
            ("targets_omitted", Shape::Int),
        ]),
        "project.diagnostics" => obj(vec![
            ("errors", Shape::Int),
            ("warnings", Shape::Int),
            ("infos", Shape::Int),
            (
                "listed",
                list(obj(vec![
                    ("code", Shape::Text),
                    ("severity", Shape::Token),
                    ("source", opt(Shape::Text)),
                    ("pointer", opt(Shape::Text)),
                    ("line", opt(Shape::Int)),
                    ("column", opt(Shape::Int)),
                ])),
            ),
            ("omitted", Shape::Int),
        ]),
        "project.prerequisites" => obj(vec![
            ("state", Shape::Token),
            (
                "tools",
                list(obj(vec![
                    ("tool", Shape::Text),
                    ("requirement", Shape::Text),
                    ("found", Shape::Bool),
                ])),
            ),
            ("tools_omitted", Shape::Int),
        ]),
        "reproduction.recipe" => obj(vec![
            ("steps", list(Shape::Text)),
            ("offline", Shape::Bool),
            ("no_external", Shape::Bool),
        ]),
        // user.attachment[N]; ids are checked before payloads.
        _ => obj(vec![
            ("kind", Shape::Token),
            ("source_bytes", Shape::Int),
            ("truncated", Shape::Bool),
            ("redactions", Shape::Int),
            ("text", Shape::Attachment),
        ]),
    }
}

fn check(value: &Value, shape: &Shape, at: &str) -> Result<(), InspectError> {
    let ok = match (shape, value) {
        (Shape::Optional(_), Value::Null) => true,
        (Shape::Optional(inner), _) => return check(value, inner, at),
        (Shape::Text, Value::String(text)) => {
            text.len() <= 512 && text.bytes().all(|b| (0x20..0x7f).contains(&b))
        }
        (Shape::Attachment, Value::String(text)) => {
            text.len() <= MAX_ATTACHMENT_TEXT_BYTES
                && text
                    .chars()
                    .all(|c| !c.is_control() || c == '\n' || c == '\t')
        }
        (Shape::Token, Value::String(text)) => {
            !text.is_empty()
                && text.len() <= 64
                && text
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        }
        (Shape::Int, Value::Number(number)) => number.is_u64(),
        (Shape::Bool, Value::Bool(_)) => true,
        (Shape::List(item), Value::Array(items)) => {
            if items.len() > 256 {
                return Err(invalid(at, "has too many items"));
            }
            for (index, value) in items.iter().enumerate() {
                check(value, item, &format!("{at}/{index}"))?;
            }
            true
        }
        // The entry-level placeholder accepts any object; payload_shape
        // checks it afterwards.
        (Shape::Object(fields), Value::Object(_)) if fields.is_empty() => true,
        (Shape::Object(fields), Value::Object(map)) => return check_object(map, fields, at),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(invalid(at, "has the wrong type or format"))
    }
}

fn check_object(
    map: &Map<String, Value>,
    fields: &[(&'static str, Shape)],
    at: &str,
) -> Result<(), InspectError> {
    if map
        .keys()
        .any(|key| !fields.iter().any(|(name, _)| name == key))
    {
        // The unexpected key itself is not echoed.
        return Err(invalid(
            if at.is_empty() { "/" } else { at },
            "has an unexpected field",
        ));
    }
    for (name, shape) in fields {
        let here = format!("{at}/{name}");
        match map.get(*name) {
            Some(value) => check(value, shape, &here)?,
            None => return Err(invalid(&here, "is missing")),
        }
    }
    Ok(())
}

pub(super) fn human(inspection: &SupportBundleInspection) -> String {
    let mut out = format!(
        "support bundle (schema {}, {} bytes, produced by tungsten {})\n",
        inspection.bundle_schema_version,
        inspection.artifact_bytes,
        inspection.producer_version.as_deref().unwrap_or("unknown"),
    );
    out.push_str(
        "matches its schema; read locally, nothing was written, uploaded or executed. Review these entries before sharing the file.\n",
    );
    if inspection
        .entries
        .iter()
        .any(|entry| entry.origin == BundleEntryOrigin::UserSupplied && entry.payload.is_some())
    {
        out.push_str(
            "contains user-supplied text (user.attachment entries); automatic redaction is not a guarantee that it is safe to share.\n",
        );
    }
    for entry in &inspection.entries {
        let _ = writeln!(
            out,
            "  {:<22} {:?} ({} bytes)",
            entry.id, entry.status, entry.bytes
        );
        if let Some(payload) = &entry.payload
            && let Ok(text) = serde_json::to_string_pretty(payload)
        {
            for line in text.lines() {
                let _ = writeln!(out, "    {line}");
            }
        }
    }
    if !inspection.omitted.is_empty() {
        let _ = writeln!(
            out,
            "excluded by default: {}",
            inspection.omitted.join(", ")
        );
    }
    out
}
