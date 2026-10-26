// SPDX-License-Identifier: AGPL-3.0-only
//! Export-safe inspection of supported typed declarations, never authorization.

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use crate::profiles::{Declaration, EffectiveConfiguration, FieldProvenance, SourceKind};
use crate::target_options::{PythonOptions, RustOptions, TypeScriptOptions};
use crate::{TungstenConfig, capabilities, pointer};
use tungsten_core::Diagnostics;

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ConfigurationInspection {
    pub configuration_version: u32,
    /// Authored declarations and schema defaults only; never a replayable manifest.
    pub targets: Vec<InspectedTarget>,
    /// Portable opaque source identities, with captured content digests.
    pub profiles: Vec<InspectedProfile>,
    /// Host restrictions, never replayable authority or executable approval.
    pub policy: tungsten_core::policy::PolicyInspection,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct InspectedProfile {
    pub source: String,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct InspectedTarget {
    pub target: String,
    pub status: InspectionTargetStatus,
    pub fields: Vec<InspectedField>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InspectionTargetStatus {
    Supported,
    /// This target has no shared typed option declaration in this compiler.
    UnsupportedOptions,
    /// External options and executable capabilities are not inspected or verified.
    ExternalUnverified,
    /// Legacy non-mapping target values use emitter defaults.
    NonMappingDefaults,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct InspectedField {
    /// Stable RFC 6901 pointer in the composed configuration.
    pub path: String,
    /// Null for redacted, absent or unresolved values; consult state.
    pub value: Option<Value>,
    pub state: InspectionValueState,
    /// False when a supported sibling option takes precedence.
    pub applicable: bool,
    pub overridden_by: Option<String>,
    pub default_value: Option<Value>,
    pub default_policy: String,
    pub invalid_policy: String,
    pub winner: Option<InspectedOrigin>,
    /// Earlier authored declarations in application order, without their values.
    pub shadowed: Vec<InspectedOrigin>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InspectionValueState {
    Declared,
    Redacted,
    Default,
    Unresolved,
    InvalidFallback,
    InvalidError,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct InspectedOrigin {
    pub kind: InspectionSourceKind,
    /// Project or captured profile ordinal; physical paths are never exported.
    pub source: String,
    pub pointer: String,
}

#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InspectionSourceKind {
    Project,
    Profile,
    Invocation,
}

/// Inspect only schemas declared by the shared capability catalog. The existing
/// parser owns validation; schema metadata owns visibility and default policy.
pub fn inspect(
    config: &TungstenConfig,
    effective: &EffectiveConfiguration,
) -> (ConfigurationInspection, Diagnostics) {
    let mut diagnostics = Diagnostics::new();
    let mut targets = Vec::new();
    for (index, (name, raw)) in config.targets.iter().enumerate() {
        let external = crate::external_target(raw).ok().flatten().is_some();
        // Declared field schemas carry no shared parser, so their targets
        // stay uninspected.
        let declaration = capabilities::target(name)
            .and_then(|target| target.options_schema)
            .filter(|schema| !matches!(schema, capabilities::TargetOptionSchema::Declared(_)));
        let mut target = InspectedTarget {
            target: capabilities::target(name).map_or_else(
                || format!("external[{index}]"),
                |target| target.id.to_string(),
            ),
            status: if external {
                InspectionTargetStatus::ExternalUnverified
            } else {
                InspectionTargetStatus::UnsupportedOptions
            },
            fields: vec![],
        };
        if !external && let Some(declaration) = declaration {
            let (id, typed, parse): (&str, Value, fn(&Value) -> Diagnostics) = match declaration {
                capabilities::TargetOptionSchema::TypeScript => (
                    "typescript",
                    serde_json::to_value(&effective.typescript)
                        .expect("typed configuration serializes"),
                    |value| TypeScriptOptions::parse(value).1,
                ),
                capabilities::TargetOptionSchema::Python => (
                    "python",
                    serde_json::to_value(&effective.python)
                        .expect("typed configuration serializes"),
                    |value| PythonOptions::parse(value).1,
                ),
                capabilities::TargetOptionSchema::Rust => (
                    "rust",
                    serde_json::to_value(&effective.rust).expect("typed configuration serializes"),
                    |value| RustOptions::parse(value).1,
                ),
                capabilities::TargetOptionSchema::Declared(_) => {
                    unreachable!("declared field schemas are filtered above")
                }
            };
            let base = format!("/targets/{id}");
            target.status = if raw.is_object() {
                InspectionTargetStatus::Supported
            } else {
                InspectionTargetStatus::NonMappingDefaults
            };
            let schema = declaration.json_schema();
            for (key, schema) in schema["properties"].as_object().expect("option schema") {
                let path = pointer::child(&base, key);
                let found = raw.get(key).map_or_else(Diagnostics::new, |value| {
                    parse(&serde_json::json!({key:value}))
                });
                let errors = found.has_errors();
                let origin = effective
                    .provenance
                    .get(&path)
                    .or_else(|| effective.provenance.get(&base));
                for mut diagnostic in found.0 {
                    diagnostic.message = format!(
                        "invalid `{key}` declaration; {}",
                        schema["x-tungsten-inspection"]["invalid_policy"]
                            .as_str()
                            .unwrap_or("unsupported value")
                    );
                    diagnostic.help = Some(format!(
                        "use a supported value from `tungsten capabilities {id} --json`; authored values and source excerpts are omitted"
                    ));
                    if let Some(origin) = origin {
                        diagnostic =
                            diagnostic.at(&origin.winner.file, &origin.winner.pointer, None);
                    }
                    diagnostics.push(diagnostic);
                }
                let invalid = raw.get(key).is_some() && (typed.get(key).is_none() || errors);
                let mut field =
                    inspect_field(&path, typed.get(key), schema, origin, invalid, effective);
                // A merged map has per-entry winners rather than a
                // single winning object. Empty maps do not shadow entries.
                if schema["x-tungsten-inspection"]["entries"] == true && !invalid {
                    field.winner = None;
                    field.shadowed.clear();
                }
                if let Some(overridden) = schema["x-tungsten-inspection"]["overridden_by"].as_str()
                    && typed.get(overridden).is_some()
                {
                    field.applicable = false;
                    field.overridden_by = Some(pointer::child(&base, overridden));
                }
                target.fields.push(field);
                if schema["x-tungsten-inspection"]["entries"] == true
                    && let Some(entries) = typed.get(key).and_then(Value::as_object)
                {
                    for (entry, value) in entries {
                        let path = pointer::child(&path, entry);
                        target.fields.push(inspect_field(
                            &path,
                            Some(value),
                            &schema["additionalProperties"],
                            effective.provenance.get(&path),
                            false,
                            effective,
                        ));
                    }
                }
            }
        }
        targets.push(target);
    }
    (
        ConfigurationInspection {
            configuration_version: 1,
            policy: tungsten_core::policy::PolicyInspection::absent(),
            targets,
            profiles: effective
                .profiles
                .iter()
                .enumerate()
                .map(|(index, profile)| InspectedProfile {
                    source: format!("profile[{index}]"),
                    digest: profile.digest.0.clone(),
                })
                .collect(),
        },
        diagnostics,
    )
}

fn inspect_field(
    path: &str,
    typed: Option<&Value>,
    schema: &Value,
    provenance: Option<&FieldProvenance>,
    invalid: bool,
    effective: &EffectiveConfiguration,
) -> InspectedField {
    let policy = &schema["x-tungsten-inspection"];
    let redact = policy["redact"].as_bool().unwrap_or(true);
    let default_value = schema.get("default").cloned();
    let invalid_policy = policy["invalid_policy"]
        .as_str()
        .unwrap_or("unsupported value");
    let state = if invalid {
        if invalid_policy.starts_with("error") {
            InspectionValueState::InvalidError
        } else {
            InspectionValueState::InvalidFallback
        }
    } else if typed.is_some() {
        if redact {
            InspectionValueState::Redacted
        } else {
            InspectionValueState::Declared
        }
    } else if default_value.is_some() {
        InspectionValueState::Default
    } else {
        InspectionValueState::Unresolved
    };
    InspectedField {
        path: path.into(),
        value: if typed.is_some() {
            typed.filter(|_| !redact).cloned()
        } else {
            default_value.clone()
        },
        state,
        applicable: true,
        overridden_by: None,
        default_value,
        default_policy: policy["default_policy"]
            .as_str()
            .unwrap_or("unresolved without compilation")
            .into(),
        invalid_policy: invalid_policy.into(),
        winner: provenance.map(|p| origin(&p.winner, effective)),
        shadowed: provenance
            .map(|p| p.shadowed.iter().map(|d| origin(d, effective)).collect())
            .unwrap_or_default(),
    }
}

fn source_name(file: &str, effective: &EffectiveConfiguration) -> String {
    effective
        .profiles
        .iter()
        .position(|profile| profile.name == file)
        .map_or_else(|| "project".into(), |index| format!("profile[{index}]"))
}

fn origin(declaration: &Declaration, effective: &EffectiveConfiguration) -> InspectedOrigin {
    InspectedOrigin {
        kind: match declaration.kind {
            SourceKind::Project => InspectionSourceKind::Project,
            SourceKind::Profile => InspectionSourceKind::Profile,
            SourceKind::Invocation => InspectionSourceKind::Invocation,
        },
        source: source_name(&declaration.file, effective),
        pointer: declaration.pointer.clone(),
    }
}
