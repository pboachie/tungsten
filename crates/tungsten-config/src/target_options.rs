// SPDX-License-Identifier: AGPL-3.0-only
//! Supported raw TypeScript target options, before IR-dependent defaults.
//!
//! The declaration/schema describe supported values. [`TypeScriptOptions::parse`]
//! preserves the emitter's compatibility policy: invalid scalars warn and become
//! absent, invalid dependency entries error, and unknown keys are ignored.

use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;
use tungsten_core::{Diagnostic, Diagnostics};

/// Defaults shared by the declaration contract and the TypeScript emitter.
pub const TYPESCRIPT_PACKAGE_VERSION: &str = "0.1.0";
pub const TYPESCRIPT_RUNTIME_RANGE: &str = "^0.1.3";

/// Valid user declarations; no package/version/runtime defaults are applied here.
/// Use [`Self::parse`] to retain TG0711 diagnostics for explicit null or invalid
/// values. A missing field is distinct from a field rejected by that parser.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TypeScriptOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "package_schema")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "version_schema")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "runtime_path_schema")]
    pub runtime_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "runtime_schema")]
    pub runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "dependencies_schema")]
    pub dependencies: Option<BTreeMap<String, String>>,
}

impl TypeScriptOptions {
    /// Strict reusable defaults. Values share the legacy validator, while
    /// diagnostics omit user values and carry pointers relative to this object.
    pub fn parse_profile(value: &serde_json::Value) -> (Self, Diagnostics) {
        let mut diagnostics = Diagnostics::new();
        let error = |pointer: &str, message: &str| {
            Diagnostic::error("TG0602", message).at("", pointer, None)
        };
        let Some(fields) = value.as_object() else {
            diagnostics.push(error("", "profile TypeScript defaults must be a mapping"));
            return (Self::default(), diagnostics);
        };
        for (key, value) in fields {
            let at = crate::pointer::child("", key);
            if !["package", "version", "runtime", "dependencies"].contains(&key.as_str()) {
                diagnostics.push(error(
                    &at,
                    "unsupported field in TypeScript profile defaults",
                ));
                continue;
            }
            if key == "dependencies" {
                if let Some(entries) = value.as_object() {
                    for (name, version) in entries {
                        let (_, checked) =
                            Self::parse(&serde_json::json!({"dependencies": {name: version}}));
                        if !checked.is_empty() {
                            diagnostics.push(error(
                                &crate::pointer::child(&at, name),
                                "invalid profile dependency declaration",
                            ));
                        }
                    }
                } else {
                    diagnostics.push(error(&at, "profile dependencies must be a mapping"));
                }
            } else {
                let (_, checked) = Self::parse(&serde_json::json!({key: value}));
                if !checked.is_empty() {
                    diagnostics.push(error(&at, "invalid TypeScript profile default"));
                }
            }
        }
        let (options, _) = Self::parse(value);
        (options, diagnostics)
    }

    /// Parse declarations using the existing scalar-fallback/dependency-error policy.
    pub fn parse(value: &serde_json::Value) -> (Self, Diagnostics) {
        let mut diags = Diagnostics::new();
        let mut read =
            |key: &str, valid: &dyn Fn(&str) -> bool, expected: &str| -> Option<String> {
                let v = value.get(key)?;
                match v.as_str() {
                    Some(s) if valid(s) => Some(s.to_string()),
                    _ => {
                        diags.push(
                            Diagnostic::warning(
                                "TG0711",
                                format!(
                                    "target option `{key}` must be {expected}; the default is used"
                                ),
                            )
                            .with_help(format!("got {v}")),
                        );
                        None
                    }
                }
            };
        let package = read("package", &is_package_name, "a valid npm package name");
        let version = read("version", &is_semver, "a semantic version such as 1.2.3");
        let runtime_path = read(
            "runtime_path",
            &|s| !s.trim().is_empty(),
            "a non-empty path",
        );
        let runtime_range = read(
            "runtime",
            &|s| !s.trim().is_empty(),
            "a non-empty version range",
        );
        let dependencies = value
            .get("dependencies")
            .map(|value| read_dependencies(value, &mut diags));
        (
            Self {
                package,
                version,
                runtime_path,
                runtime: runtime_range,
                dependencies,
            },
            diags,
        )
    }
}

/// Supported-value schema, including the legacy version grammar. It describes
/// valid declarations rather than changing warning/fallback behavior to errors.
/// Driver-owned fields such as `out`, and other unknown keys, remain permitted.
pub fn typescript_json_schema() -> serde_json::Value {
    let mut schema = schemars::schema_for!(TypeScriptOptions).to_value();
    schema["additionalProperties"] = true.into();
    schema
}

/// Strict profile subset of the same supported-value schema.
pub fn typescript_profile_json_schema() -> serde_json::Value {
    let mut schema = typescript_json_schema();
    schema["properties"]
        .as_object_mut()
        .expect("option properties")
        .remove("runtime_path");
    schema["additionalProperties"] = false.into();
    schema
}

fn package_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string", "maxLength": 214,
        "pattern": r"^(@[a-z0-9~-][a-z0-9._~-]*/)?[a-z0-9~-][a-z0-9._~-]*$",
        "not": {"pattern": r"\s"},
        "x-tungsten-inspection": {
            "default_policy":"derive from the compiled API name; unresolved without compilation",
            "invalid_policy":"warning and emitter default", "redact":false
        }
    })
}

fn version_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type":"string",
        "pattern":r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)([-+][A-Za-z0-9.+-]+)?$",
        "not":{"pattern":r"\s"},
        "default": TYPESCRIPT_PACKAGE_VERSION,
        "x-tungsten-inspection": {
            "default_policy":"fixed package version", "invalid_policy":"warning and emitter default", "redact":false
        }
    })
}

fn nonempty_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    // JSON Schema's ECMAScript whitespace differs from Rust's Unicode trim;
    // spell out White_Space so values such as U+0085 retain the same meaning.
    schemars::json_schema!({"type":"string", "pattern":r"[^\u0009-\u000D\u0020\u0085\u00A0\u1680\u2000-\u200A\u2028\u2029\u202F\u205F\u3000]"})
}

fn runtime_path_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut schema = nonempty_schema(generator);
    schema.insert("default".into(), serde_json::Value::Null);
    schema.insert(
        "x-tungsten-inspection".into(),
        serde_json::json!({
            "default_policy":"absent; use runtime or its default",
            "invalid_policy":"warning and ignore this path", "redact":true
        }),
    );
    schema
}

fn runtime_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut schema = nonempty_schema(generator);
    schema.insert("default".into(), TYPESCRIPT_RUNTIME_RANGE.into());
    schema.insert("x-tungsten-inspection".into(), serde_json::json!({
        "default_policy":"fixed runtime range unless a valid runtime_path selects a file dependency",
        "invalid_policy":"warning and emitter default", "redact":true,
        "overridden_by":"runtime_path"
    }));
    schema
}

fn dependencies_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let package = package_schema(generator);
    let mut nonempty = nonempty_schema(generator);
    nonempty.insert("x-tungsten-inspection".into(), serde_json::json!({
        "default_policy":"absent unless declared; profile dependency maps merge by package name",
        "invalid_policy":"error; generation refuses invalid custom dependencies", "redact":true
    }));
    schemars::json_schema!({
        "type":"object", "additionalProperties": nonempty,
        "propertyNames": {"allOf":[package, {"not":{"enum":["@elysra/tungsten-runtime","zod","typescript"]}}]},
        "default": {},
        "x-tungsten-inspection": {
            "default_policy":"empty custom dependency map; declarations merge by package name",
            "invalid_policy":"error; generation refuses invalid custom dependencies",
            "redact":true, "entries":true
        }
    })
}

fn read_dependencies(
    value: &serde_json::Value,
    diags: &mut Diagnostics,
) -> BTreeMap<String, String> {
    let mut dependencies = BTreeMap::new();
    let Some(entries) = value.as_object() else {
        diags.push(
            Diagnostic::error("TG0711", "target option `dependencies` must be an npm package-to-version mapping")
                .with_help("set targets.typescript.dependencies to an object, for example {\"@hpke/core\": \"1.7.5\"}"),
        );
        return dependencies;
    };
    for (name, version) in entries {
        let help = match name.as_str() {
            "@elysra/tungsten-runtime" => {
                Some("configure targets.typescript.runtime or runtime_path instead")
            }
            "zod" | "typescript" => {
                Some("remove this entry; the emitter controls this package's compatible version")
            }
            _ => None,
        };
        if let Some(help) = help {
            diags.push(
                Diagnostic::error("TG0711", format!("target option `dependencies.{name}` conflicts with an emitter-managed package"))
                    .with_help(help),
            );
        } else if !is_package_name(name) {
            diags.push(
                Diagnostic::error(
                    "TG0711",
                    format!(
                        "target option `dependencies.{name}` must use a valid npm package name"
                    ),
                )
                .with_help("use a lowercase npm package name, optionally scoped as @scope/name"),
            );
        } else if let Some(version) = version.as_str().filter(|s| !s.trim().is_empty()) {
            dependencies.insert(name.clone(), version.to_string());
        } else {
            diags.push(
                Diagnostic::error("TG0711", format!("target option `dependencies.{name}` must be a non-empty npm version specifier"))
                    .with_help("quote the version or range, for example \"1.7.5\" or \"^1.7.5\""),
            );
        }
    }
    dependencies
}

/// npm package name rules: at most 214 characters, lowercase, URL-safe,
/// optionally scoped (`@scope/name`), not starting with `.` or `_`.
fn is_package_name(s: &str) -> bool {
    fn part(p: &str) -> bool {
        !p.is_empty()
            && !p.starts_with('.')
            && !p.starts_with('_')
            && p.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-._~".contains(c))
    }
    if s.len() > 214 {
        return false;
    }
    match s.strip_prefix('@') {
        Some(rest) => rest
            .split_once('/')
            .is_some_and(|(scope, name)| part(scope) && part(name)),
        None => part(s),
    }
}

/// `MAJOR.MINOR.PATCH` with optional `-prerelease` and `+build`.
fn is_semver(s: &str) -> bool {
    let (core, rest) = match s.find(['-', '+']) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    let numbers: Vec<&str> = core.split('.').collect();
    let core_ok = numbers.len() == 3
        && numbers.iter().all(|n| {
            !n.is_empty()
                && n.chars().all(|c| c.is_ascii_digit())
                && (n.len() == 1 || !n.starts_with('0'))
        });
    let rest_ok = rest
        .chars()
        .skip(1)
        .all(|c| c.is_ascii_alphanumeric() || "-.+".contains(c))
        && rest.len() != 1;
    core_ok && rest_ok
}
