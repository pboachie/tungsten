// SPDX-License-Identifier: AGPL-3.0-only
//! Versioned transport-neutral catalogue of callable operations and macros.
//! MCP adds disclosure and search; browser adapters add registration lifetime.

pub mod compiler;
pub mod package;
pub use compiler::assign_names;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use tungsten_core::Diagnostics;
use tungsten_ir::Ir;

pub const VERSION: u32 = 1;
pub const SCHEMA_ID: &str = "https://tungsten.dev/schemas/tool-catalog-v1.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Operation,
    Macro,
}

/// Reference to an SDK descriptor by its wire identity, never a source path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DescriptorReference {
    pub kind: ToolKind,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Reserved {
    pub idempotency_key: Option<String>,
    pub confirmation_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    pub kind: ToolKind,
    pub target: String,
    pub descriptor: DescriptorReference,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub safety: String,
    pub idempotency: String,
    pub reserved: Reserved,
    pub cluster: Option<String>,
    pub sensitive_response_fields: Vec<String>,
    pub shown_once: bool,
}

/// Explicit selectors for separately emitted resource or cluster modules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Group {
    pub name: String,
    pub module_key: String,
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolCatalog {
    pub catalog_version: u32,
    pub api: String,
    pub api_version: String,
    pub tungsten_version: String,
    pub tools: Vec<Tool>,
    pub groups: Vec<Group>,
}

/// Compile every visible callable operation and macro. SDK adapters can exclude
/// macros they cannot execute using [`build_with_exclusions`].
pub fn build(ir: &Ir) -> (ToolCatalog, Diagnostics) {
    let (plan, _) = crate::sdk::SdkPlan::new(ir);
    let excluded = plan
        .macro_issues
        .iter()
        .map(|issue| issue.name.clone())
        .collect();
    build_with_exclusions(ir, &excluded)
}

pub fn build_with_exclusions(ir: &Ir, excluded: &BTreeSet<String>) -> (ToolCatalog, Diagnostics) {
    let (entries, _, diagnostics) = compiler::compile_entries(ir, excluded);
    let tools: Vec<Tool> = entries
        .into_iter()
        .map(|entry| {
            let kind = match entry.kind {
                compiler::ToolKind::Operation => ToolKind::Operation,
                compiler::ToolKind::Macro => ToolKind::Macro,
            };
            Tool {
                descriptor: DescriptorReference {
                    kind: kind.clone(),
                    id: entry.target.clone(),
                },
                name: entry.name,
                kind,
                target: entry.target,
                description: entry.description,
                input_schema: entry.input_schema,
                output_schema: entry.output_schema,
                safety: entry.safety.into(),
                idempotency: entry.idempotency.into(),
                reserved: Reserved {
                    idempotency_key: entry.reserved.idempotency_key,
                    confirmation_token: entry.reserved.confirmation_token,
                },
                cluster: entry.cluster,
                sensitive_response_fields: entry.sensitive_response_fields,
                shown_once: entry.shown_once,
            }
        })
        .collect();
    fn resources(resource: &tungsten_ir::Resource, path: &str, out: &mut BTreeMap<String, String>) {
        let path = format!("{path}.{}", resource.name.snake());
        for operation in &resource.operations {
            out.insert(operation.id.0.clone(), path.clone());
        }
        for child in &resource.children {
            resources(child, &path, out);
        }
    }
    let mut resource_groups = BTreeMap::new();
    for namespace in &ir.namespaces {
        for resource in &namespace.resources {
            resources(resource, &namespace.name.snake(), &mut resource_groups);
        }
    }
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for tool in &tools {
        let group = tool.cluster.clone().unwrap_or_else(|| {
            resource_groups
                .get(&tool.target)
                .cloned()
                .unwrap_or_else(|| "macros".into())
        });
        groups.entry(group).or_default().push(tool.name.clone());
    }
    let group_names: Vec<(String, String)> = groups
        .keys()
        .map(|name| (name.clone(), name.clone()))
        .collect();
    let module_keys = assign_names(&group_names).0;
    (
        ToolCatalog {
            catalog_version: VERSION,
            api: ir.api.name.wire.clone(),
            api_version: ir.api.version.clone(),
            tungsten_version: ir.generator.tungsten_version.clone(),
            tools,
            groups: groups
                .into_iter()
                .zip(module_keys)
                .map(|((name, tools), module_key)| Group {
                    name,
                    module_key,
                    tools,
                })
                .collect(),
        },
        diagnostics,
    )
}

/// A catalogue projection containing exactly the named group's tools.
pub fn select_group(catalog: &ToolCatalog, name: &str) -> Option<ToolCatalog> {
    let group = catalog.groups.iter().find(|group| group.name == name)?;
    let names: BTreeSet<&str> = group.tools.iter().map(String::as_str).collect();
    let mut selected = catalog.clone();
    selected
        .tools
        .retain(|tool| names.contains(tool.name.as_str()));
    selected.groups = vec![group.clone()];
    Some(selected)
}

/// JSON Schema document for catalogue version 1.
pub fn schema() -> Value {
    let mut value = serde_json::to_value(schemars::schema_for!(ToolCatalog)).unwrap_or_default();
    value["$id"] = json!(SCHEMA_ID);
    value["properties"]["catalogVersion"] = json!({"const": VERSION});
    value["$defs"]["Tool"]["properties"]["name"] =
        json!({"type":"string","pattern":"^[a-z0-9_]+$","maxLength":48,"minLength":1});
    value["$defs"]["Tool"]["properties"]["safety"] =
        json!({"enum":["read_only","mutating","destructive","irreversible"]});
    value["$defs"]["Tool"]["properties"]["idempotency"] =
        json!({"enum":["none","auto","caller_owned","content_hash","content_identity"]});
    value["$defs"]["Tool"]["properties"]["inputSchema"] = json!({"type":"object", "required":["type","properties"], "properties":{"type":{"const":"object"},"properties":{"type":"object"}}});
    value["$defs"]["Tool"]["properties"]["outputSchema"] = json!({"type":["object","null"]});
    value["$defs"]["Tool"]["properties"]["target"] = json!({"type":"string","minLength":1});
    value["$defs"]["DescriptorReference"]["properties"]["id"] =
        json!({"type":"string","minLength":1});
    value["$defs"]["Group"]["properties"]["moduleKey"] =
        json!({"type":"string","pattern":"^[a-z0-9_]+$","minLength":1,"maxLength":48});
    value
}

/// Check structural and execution invariants before accepting a third-party
/// catalogue. Schema references are self-contained JSON pointers only.
pub fn validate(catalog: &ToolCatalog) -> Result<(), String> {
    if catalog.catalog_version != VERSION {
        return Err(format!(
            "unsupported tool catalogue version {}",
            catalog.catalog_version
        ));
    }
    let mut names = BTreeSet::new();
    for tool in &catalog.tools {
        if tool.name.is_empty()
            || tool.name.len() > 48
            || !tool
                .name
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
            || !names.insert(tool.name.as_str())
        {
            return Err(format!("invalid or duplicate tool name {}", tool.name));
        }
        if tool.input_schema.get("type").and_then(Value::as_str) != Some("object")
            || !tool
                .input_schema
                .get("properties")
                .is_some_and(Value::is_object)
            || tool
                .output_schema
                .as_ref()
                .is_some_and(|schema| !schema.is_object())
        {
            return Err(format!(
                "invalid argument or response schema for {}",
                tool.name
            ));
        }
        if tool.target.is_empty()
            || tool.descriptor.kind != tool.kind
            || tool.descriptor.id != tool.target
        {
            return Err(format!("descriptor mismatch for {}", tool.name));
        }
        if !["read_only", "mutating", "destructive", "irreversible"].contains(&tool.safety.as_str())
            || ![
                "none",
                "auto",
                "caller_owned",
                "content_hash",
                "content_identity",
            ]
            .contains(&tool.idempotency.as_str())
        {
            return Err(format!("invalid safety policy for {}", tool.name));
        }
        if ["destructive", "irreversible"].contains(&tool.safety.as_str())
            && tool.reserved.confirmation_token.is_none()
        {
            return Err(format!("missing confirmation field for {}", tool.name));
        }
        if ["auto", "caller_owned", "content_hash"].contains(&tool.idempotency.as_str())
            && tool.reserved.idempotency_key.is_none()
        {
            return Err(format!("missing idempotency field for {}", tool.name));
        }
        for field in [
            &tool.reserved.idempotency_key,
            &tool.reserved.confirmation_token,
        ]
        .into_iter()
        .flatten()
        {
            if tool
                .input_schema
                .get("properties")
                .and_then(|properties| properties.get(field))
                .is_none()
            {
                return Err(format!("missing reserved schema field {field}"));
            }
        }
        validate_refs(&tool.input_schema, &tool.input_schema)?;
        if let Some(schema) = &tool.output_schema {
            validate_refs(schema, schema)?;
        }
    }
    let mut groups = BTreeSet::new();
    let mut module_keys = BTreeSet::new();
    for group in &catalog.groups {
        if !module_keys.insert(group.module_key.as_str())
            || group.module_key.is_empty()
            || group.module_key.len() > compiler::MAX_TOOL_NAME
            || !group
                .module_key
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            || !groups.insert(group.name.as_str())
            || group
                .tools
                .iter()
                .any(|name| !names.contains(name.as_str()))
        {
            return Err(format!("invalid group {}", group.name));
        }
    }
    Ok(())
}

fn validate_refs(value: &Value, root: &Value) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            if let Some(reference) = map.get("$ref") {
                let reference = reference
                    .as_str()
                    .ok_or("schema reference must be a string")?;
                let pointer = reference
                    .strip_prefix('#')
                    .ok_or("external schema references are not permitted")?;
                if root.pointer(pointer).is_none() {
                    return Err(format!("unresolved schema reference {reference}"));
                }
            }
            for child in map.values() {
                validate_refs(child, root)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                validate_refs(child, root)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Deterministic version-checked JSON for external emitters; no credentials,
/// environment values or filesystem paths are present in this document.
pub fn json(catalog: &ToolCatalog) -> Result<String, String> {
    validate(catalog)?;
    let mut text = serde_json::to_string_pretty(catalog).map_err(|error| error.to_string())?;
    text.push('\n');
    Ok(text)
}

/// Compare two independent external-emitter runs. Catalogue safety/schema and
/// descriptor identities are validated first; output paths go through FileSet's
/// standard traversal and duplicate guards. File bytes and ordering must match.
pub fn external_conformance(
    catalog: &ToolCatalog,
    first: &crate::external::EmitResponse,
    second: &crate::external::EmitResponse,
) -> Result<(), String> {
    validate(catalog)?;
    for response in [first, second] {
        if response.protocol != crate::external::PROTOCOL {
            return Err("unsupported external emitter protocol version".into());
        }
        let mut files = crate::FileSet::new();
        for file in &response.files {
            if file.path.split('/').any(|part| part == ".tungsten") {
                return Err("reserved external emitter output path".into());
            }
            match (&file.content, &file.content_base64) {
                (Some(content), None) => files
                    .add(&file.path, content.as_bytes())
                    .map_err(|error| error.to_string())?,
                (None, Some(_)) => {
                    return Err("catalogue conformance fixtures must use UTF-8 text outputs".into());
                }
                _ => return Err("external output requires exactly one content field".into()),
            }
        }
        if let Some(surface) = &response.surface {
            for (name, target) in &surface.tools {
                if !catalog
                    .tools
                    .iter()
                    .any(|tool| &tool.name == name && &tool.target == target)
                {
                    return Err(format!(
                        "external tool {name} is absent or reassigned in the catalogue"
                    ));
                }
            }
        }
    }
    if first != second {
        return Err("external emitter output is not deterministic".into());
    }
    Ok(())
}
