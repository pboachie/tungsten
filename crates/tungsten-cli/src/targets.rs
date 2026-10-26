// SPDX-License-Identifier: AGPL-3.0-only
//! The emitter registry and how `tungsten.yml` targets become emitter runs.
//!
//! Every target name of `tungsten.yml` is listed here; the ones without an
//! emitter in this version (`mock`) are reported
//! with TG0702 and skipped.

use std::collections::BTreeMap;
use std::path::Path;

use tungsten_config::TungstenConfig;
use tungsten_core::Diagnostic;
use tungsten_emit::{Emitter, TargetConfig};
use tungsten_emit_csharp::CSharpEmitter;
use tungsten_emit_dart::DartEmitter;
use tungsten_emit_docs::DocsEmitter;
use tungsten_emit_fsharp::FSharpEmitter;
use tungsten_emit_java::JavaEmitter;
use tungsten_emit_kotlin::KotlinEmitter;
use tungsten_emit_mcp::McpEmitter;
use tungsten_emit_py::PythonEmitter;
use tungsten_emit_rs::RustEmitter;
use tungsten_emit_ruby::RubyEmitter;
use tungsten_emit_ts::TypeScriptEmitter;
use tungsten_emit_webmcp::WebMcpEmitter;
use tungsten_ir::Ir;

static TYPESCRIPT: TypeScriptEmitter = TypeScriptEmitter;
static DOCS: DocsEmitter = DocsEmitter;
static MCP: McpEmitter = McpEmitter;
static WEBMCP: WebMcpEmitter = WebMcpEmitter;
static PYTHON: PythonEmitter = PythonEmitter;
static RUST: RustEmitter = RustEmitter;
static JAVA: JavaEmitter = JavaEmitter;
static KOTLIN: KotlinEmitter = KotlinEmitter;
static CSHARP: CSharpEmitter = CSharpEmitter;
static FSHARP: FSharpEmitter = FSharpEmitter;
static DART: DartEmitter = DartEmitter;
static RUBY: RubyEmitter = RubyEmitter;

/// The emitter for a target id, if this version has one.
pub fn emitter(id: &str) -> Option<&'static dyn Emitter> {
    match id {
        "typescript" => Some(&TYPESCRIPT),
        "docs" => Some(&DOCS),
        "mcp" => Some(&MCP),
        "webmcp" => Some(&WEBMCP),
        "python" => Some(&PYTHON),
        "rust" => Some(&RUST),
        "java" => Some(&JAVA),
        "kotlin" => Some(&KOTLIN),
        "csharp" => Some(&CSHARP),
        "fsharp" => Some(&FSHARP),
        "dart" => Some(&DART),
        "ruby" => Some(&RUBY),
        _ => None,
    }
}

/// Whether built-in target `id` enforces imported-operation policy in its
/// generated runtime. Only these may be generated for a project whose
/// operations come from Postman, cURL, `.http` or HAR input; an external
/// emitter opts in through `import_policy` in its `--describe` response.
pub(crate) fn serves_import_policy(id: &str) -> bool {
    tungsten_config::capabilities::target(id).is_some_and(|target| target.imported_operation_policy)
}

/// The refusal of target `name` for a project with imported-operation
/// policy, one target at a time.
pub(crate) fn import_policy_refusal(name: &str) -> Diagnostic {
    Diagnostic::error(
        "TG0705",
        format!("target `{name}` does not declare enforced imported-operation policy support"),
    )
    .with_help(
        "select a supported TypeScript, Python, Rust, Java, Kotlin, MCP, WebMCP or docs target, or an \
         external emitter that declares `import_policy` (`tungsten init --resume --add-targets \
         <targets>` adds one); imported-policy metadata must not be discarded. `tungsten \
         explain TG0705` has the details",
    )
}

/// A protocol-1 external emitter must explicitly declare policy enforcement
/// before receiving imported operations.
pub(crate) fn external_import_policy_refusal(name: &str) -> Diagnostic {
    Diagnostic::error(
        "TG0808",
        format!(
            "external target `{name}` does not declare enforced imported-operation policy support"
        ),
    )
    .with_help(
        "use an emitter whose --describe response declares `import_policy: true` and \
         enforces imported-operation policy, or select a supported TypeScript, Python, \
         Rust, Java, Kotlin, MCP, WebMCP or docs target; `tungsten explain TG0808` has the details",
    )
}

/// The agent tool names target `id` serves (tool name → operation id or
/// macro name), recorded in its API surface snapshot: the MCP server's
/// tools and the browser WebMCP invocation and preview tools; none for SDK and
/// docs targets.
pub fn tool_names(id: &str, ir: &Ir) -> BTreeMap<String, String> {
    match id {
        "mcp" => tungsten_emit_mcp::tool_names(ir),
        "webmcp" => tungsten_emit_webmcp::tool_names(ir),
        _ => BTreeMap::new(),
    }
}

/// The target id for a name given on the command line: the ids themselves
/// and the short forms `ts`, `py`, `rs`, `cs`, `fs`, `kt` and `rb`.
pub fn canonical(name: &str) -> &str {
    tungsten_config::capabilities::target(name).map_or(name, |target| target.id)
}

/// The configuration of target `name`: its options from `tungsten.yml`
/// (an empty object when absent) and its output directory, the `out`
/// option resolved against `base_dir` (default `generated/<name>`).
pub fn target_config(config: Option<&TungstenConfig>, name: &str, base_dir: &Path) -> TargetConfig {
    target_config_for(config, name, base_dir, false)
}

/// [`target_config`] for a run that may call the MCP target's embedding
/// provider (`live`, a write that is not a dry run). The MCP target gets
/// the reserved options `project_dir` (where `tungsten.yml` is, the working
/// directory of a command provider) and `embeddings_live`.
pub fn target_config_for(
    config: Option<&TungstenConfig>,
    name: &str,
    base_dir: &Path,
    live: bool,
) -> TargetConfig {
    let mut options = config
        .and_then(|c| c.targets.get(name))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if matches!(name, "mcp" | "webmcp") && options.is_null() {
        options = serde_json::json!({});
    }
    if matches!(name, "mcp" | "webmcp")
        && let Some(map) = options.as_object_mut()
        && let Some(ts) = config.and_then(|c| c.targets.get("typescript"))
    {
        // Agent adapters depend on the SDK the `typescript` target generates.
        let pick: serde_json::Map<String, serde_json::Value> = ["package", "version"]
            .iter()
            .filter_map(|k| Some(((*k).to_string(), ts.get(*k)?.clone())))
            .collect();
        map.insert("typescript_target".into(), serde_json::Value::Object(pick));
    }
    if name == "mcp"
        && let Some(map) = options.as_object_mut()
    {
        map.insert(
            "project_dir".into(),
            serde_json::Value::String(base_dir.display().to_string()),
        );
        map.insert("embeddings_live".into(), serde_json::Value::Bool(live));
    }
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

/// TG0807: an `external` target was not run because external emitters are
/// off (`--no-external` or `TUNGSTEN_NO_EXTERNAL=1`).
pub(crate) fn external_skipped(name: &str, manifest: &str) -> Diagnostic {
    Diagnostic::info(
        "TG0807",
        format!(
            "external target `{name}` was not run: external emitters are off (--no-external or TUNGSTEN_NO_EXTERNAL)"
        ),
    )
    .at(manifest, format!("/targets/{name}"), None)
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
