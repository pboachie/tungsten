// SPDX-License-Identifier: AGPL-3.0-only
//! The emitter registry and how `tungsten.yml` targets become emitter runs.
//!
//! Every target name of `tungsten.yml` is listed here; the ones without an
//! emitter in this version (`python`, `rust`, `mcp`, `mock`) are reported
//! with TG0702 and skipped.

use std::path::Path;

use tungsten_config::TungstenConfig;
use tungsten_core::Diagnostic;
use tungsten_emit::{Emitter, TargetConfig};
use tungsten_emit_docs::DocsEmitter;
use tungsten_emit_ts::TypeScriptEmitter;

static TYPESCRIPT: TypeScriptEmitter = TypeScriptEmitter;
static DOCS: DocsEmitter = DocsEmitter;

/// The emitter for a target id, if this version has one.
pub fn emitter(id: &str) -> Option<&'static dyn Emitter> {
    match id {
        "typescript" => Some(&TYPESCRIPT),
        "docs" => Some(&DOCS),
        _ => None,
    }
}

/// The target id for a name given on the command line: the ids themselves
/// and the short forms `ts`, `py` and `rs`.
pub fn canonical(name: &str) -> &str {
    match name {
        "ts" => "typescript",
        "py" => "python",
        "rs" => "rust",
        other => other,
    }
}

/// The configuration of target `name`: its options from `tungsten.yml`
/// (an empty object when absent) and its output directory, the `out`
/// option resolved against `base_dir` (default `generated/<name>`).
pub fn target_config(config: Option<&TungstenConfig>, name: &str, base_dir: &Path) -> TargetConfig {
    let options = config
        .and_then(|c| c.targets.get(name))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let out = options
        .get("out")
        .and_then(|v| v.as_str())
        .map_or_else(|| format!("generated/{name}"), str::to_string);
    TargetConfig {
        name: name.to_string(),
        out_dir: crate::input::join(base_dir, &out),
        options,
    }
}

/// TG0702: the target has no emitter in this version.
pub(crate) fn not_implemented(name: &str, manifest: &str) -> Diagnostic {
    Diagnostic::info(
        "TG0702",
        format!(
            "target `{name}` has no emitter in tungsten {} yet; skipped",
            tungsten_build::TUNGSTEN_VERSION
        ),
    )
    .at(manifest, format!("/targets/{name}"), None)
}
