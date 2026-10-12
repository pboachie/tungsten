// SPDX-License-Identifier: AGPL-3.0-only
//! Browser WebMCP package emitter. The neutral catalogue compiler owns tool names,
//! schemas and safety metadata; the browser runtime owns registration lifetime.

mod options;
mod package;

use std::collections::{BTreeMap, BTreeSet};

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::{Emitter, FileSet, TargetConfig};
use tungsten_ir::Ir;

pub use options::Options;

/// Emits the browser-only `webmcp` target.
#[derive(Debug, Clone, Copy, Default)]
pub struct WebMcpEmitter;

impl Emitter for WebMcpEmitter {
    fn id(&self) -> &'static str {
        "webmcp"
    }

    fn supports(&self, ir: &Ir) -> Diagnostics {
        let (manifest, mut diagnostics) = catalog_value(ir);
        // Disclosure mode belongs to the MCP server transport; browser scopes
        // are chosen by the host application when registering tools.
        diagnostics
            .0
            .retain(|diagnostic| diagnostic.code != "TG0725");
        if let Some(tools) = manifest.get("tools").and_then(serde_json::Value::as_array) {
            let names: BTreeSet<&str> = tools
                .iter()
                .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
                .collect();
            for tool in tools {
                if tool
                    .pointer("/reserved/confirmationToken")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
                    && let Some(name) = tool.get("name").and_then(serde_json::Value::as_str)
                    && names.contains(format!("{name}__preview").as_str())
                {
                    diagnostics.push(Diagnostic::warning("TG0793", format!(
                        "WebMCP preview tool `{name}__preview` collides with a catalog tool; registering both is rejected"
                    )).with_help("Rename the conflicting operation with naming.operations, or scope registration to avoid the collision."));
                }
            }
        }
        diagnostics
    }

    fn emit(&self, ir: &Ir, cfg: &TargetConfig, out: &mut FileSet) -> Diagnostics {
        let (options, mut diagnostics) = Options::resolve(ir, cfg);
        for (path, text) in generate(ir, &options) {
            if let Err(error) = out.add(path, text) {
                diagnostics.push(Diagnostic::error(
                    "TG0791",
                    format!("webmcp target: {error}"),
                ));
            }
        }
        diagnostics
    }
}

/// Stable tool names for API surface snapshots, including browser preview helpers.
pub fn tool_names(ir: &Ir) -> BTreeMap<String, String> {
    let mut names = tungsten_emit::tool_catalog::build(ir)
        .0
        .tools
        .into_iter()
        .map(|tool| (tool.name, tool.target))
        .collect::<BTreeMap<_, _>>();
    let manifest = browser_manifest(ir);
    if let Some(tools) = manifest.get("tools").and_then(serde_json::Value::as_array) {
        for tool in tools {
            if tool
                .pointer("/reserved/confirmationToken")
                .and_then(serde_json::Value::as_str)
                .is_some()
                && let Some(name) = tool.get("name").and_then(serde_json::Value::as_str)
                && let Some(target) = tool.get("target").and_then(serde_json::Value::as_str)
            {
                names
                    .entry(format!("{name}__preview"))
                    .or_insert_with(|| target.to_string());
            }
        }
    }
    names
}

/// Project the shared catalog onto the browser transport contract. Server search
/// indexes and MCP initialization instructions are not part of browser discovery.
fn browser_manifest(ir: &Ir) -> serde_json::Value {
    let (shared, _) = catalog_value(ir);
    let mut browser = shared;
    browser["manifestVersion"] = serde_json::json!(1);
    browser
}

fn catalog_value(ir: &Ir) -> (serde_json::Value, Diagnostics) {
    let (catalog, diagnostics) = tungsten_emit::tool_catalog::build(ir);
    (
        serde_json::to_value(catalog).unwrap_or_default(),
        diagnostics,
    )
}

/// Deterministic browser package files in a fixed order.
pub fn generate(ir: &Ir, options: &Options) -> Vec<(String, String)> {
    let manifest = browser_manifest(ir);
    let mut files = vec![
        ("README.md".into(), package::readme(options, &manifest)),
        ("manifest.json".into(), package::json_text(&manifest)),
        ("package.json".into(), package::package_json(options)),
        ("src/custom/index.ts".into(), package::custom_template()),
        (
            "src/index.ts".into(),
            package::index_ts(ir, options, &manifest),
        ),
        ("tsconfig.json".into(), package::tsconfig_json()),
    ];
    let (catalog, _) = tungsten_emit::tool_catalog::build(ir);
    files.push(("src/catalog.ts".into(), format!("/** Transport-neutral discovery data; no client or registration imports. */\nexport const catalog = {} as const;\n", package::json_text(&serde_json::to_value(&catalog).unwrap_or_default()).trim_end())));
    files.extend(group_files(ir, options, &catalog));
    files
}

/// Resource groups compile their own descriptors and reachable model schemas.
/// Their import graph never goes through the eager SDK client or root catalogue.
fn group_files(
    ir: &Ir,
    options: &Options,
    catalog: &tungsten_emit::tool_catalog::ToolCatalog,
) -> Vec<(String, String)> {
    use tungsten_emit::tool_catalog::{ToolKind, select_group};
    let mut files = vec![];
    for group in &catalog.groups {
        let key = &group.module_key;
        let Some(selected) = select_group(catalog, &group.name) else {
            continue;
        };
        let mut operations: BTreeSet<String> = selected
            .tools
            .iter()
            .filter(|tool| tool.kind == ToolKind::Operation)
            .map(|tool| tool.target.clone())
            .collect();
        let macros: BTreeSet<String> = selected
            .tools
            .iter()
            .filter(|tool| tool.kind == ToolKind::Macro)
            .map(|tool| tool.target.clone())
            .collect();
        for mac in &ir.agent.macros {
            if !macros.contains(&mac.name.0) {
                continue;
            }
            if let Some(base) = mac
                .input
                .pointer("/extends")
                .and_then(serde_json::Value::as_str)
            {
                operations.insert(base.to_string());
            }
            for step in mac.steps.as_array().into_iter().flatten() {
                if let Some(operation) = step.get("operation").and_then(serde_json::Value::as_str) {
                    operations.insert(operation.to_string());
                }
            }
        }
        loop {
            let before = operations.len();
            for operation in ir.operations() {
                if !operations.contains(&operation.id.0) {
                    continue;
                }
                if let Some(verify) = &operation.agent.verify {
                    operations.insert(verify.operation.0.clone());
                }
                if let tungsten_ir::PreviewMode::Endpoint { operation } = &operation.agent.preview {
                    operations.insert(operation.0.clone());
                }
            }
            if operations.len() == before {
                break;
            }
        }
        let mut scoped = ir.clone();
        fn retain(resource: &mut tungsten_ir::Resource, ids: &BTreeSet<String>) {
            resource
                .operations
                .retain(|operation| ids.contains(&operation.id.0));
            for child in &mut resource.children {
                retain(child, ids);
            }
            resource
                .children
                .retain(|child| !child.operations.is_empty() || !child.children.is_empty());
        }
        for namespace in &mut scoped.namespaces {
            for resource in &mut namespace.resources {
                retain(resource, &operations);
            }
            namespace.resources.retain(|resource| {
                !resource.operations.is_empty() || !resource.children.is_empty()
            });
            namespace.planned.clear();
        }
        scoped
            .agent
            .macros
            .retain(|mac| macros.contains(&mac.name.0));
        let (plan, _) = tungsten_emit::sdk::SdkPlan::new(&scoped);
        let reachable: BTreeSet<_> = plan
            .types
            .iter()
            .filter(|(_, ty)| ty.reachable)
            .map(|(id, _)| id.clone())
            .collect();
        scoped.types.types.retain(|ty| reachable.contains(&ty.id));
        let ts_options = tungsten_emit_ts::Options {
            package: options.sdk_package.clone(),
            version: options.version.clone(),
            runtime: options.sdk_runtime.clone(),
            dependencies: Default::default(),
        };
        for (path, content) in tungsten_emit_ts::generate(&scoped, &ts_options) {
            if path == "src/descriptors.ts"
                || path == "src/internal.ts"
                || path == "src/macros.ts"
                || path.starts_with("src/models/")
            {
                files.push((
                    format!("src/groups/{key}/{}", path.trim_start_matches("src/")),
                    content,
                ));
            }
        }
        let mut manifest = serde_json::to_value(&selected).unwrap_or_default();
        manifest["manifestVersion"] = serde_json::json!(1);
        let mut module = "import { ClientCore, type ClientOptions } from \"@elysra/tungsten-runtime\";\nimport type { WebMcpManifest, WebMcpScope } from \"@elysra/tungsten-webmcp\";\nimport { api, operations } from \"./descriptors.js\";\n".to_string();
        if !macros.is_empty() {
            module.push_str("import { macroDescriptors } from \"./macros.js\";\n");
        }
        module.push_str(&format!("\nexport const manifest: WebMcpManifest = {};\n\n/** Instantiate only this group's descriptor graph. No registration side effects. */\nexport function createScope(client: ClientOptions = {{}}): WebMcpScope {{\n  return {{ core: new ClientCore(api, {{ ...client, operations }}), manifest", package::json_text(&manifest).trim_end()));
        if !macros.is_empty() {
            module.push_str(
                ", macros: Object.fromEntries(macroDescriptors.map(macro => [macro.name, macro]))",
            );
        }
        module.push_str(" };\n}\n");
        files.push((format!("src/groups/{key}/index.ts"), module));
    }
    files
}
