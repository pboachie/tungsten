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

/// Defaults shared by the Python declaration contract and emitter.
pub const PYTHON_PACKAGE_VERSION: &str = "0.1.0";
pub const PYTHON_RUNTIME_SPEC: &str = ">=0.1.3,<0.2.0";
pub const PYTHON_VERSION: &str = "3.12";
/// Defaults shared by the Rust declaration contract and emitter.
pub const RUST_PACKAGE_VERSION: &str = "0.1.0";
pub const RUST_RUNTIME_REQ: &str = "0.1.3";

/// One scalar string option: its key, validator and expected-value phrase.
struct Scalar {
    key: &'static str,
    valid: fn(&str) -> bool,
    expected: &'static str,
}

/// Read `scalars` in order with the scalar-fallback policy: an invalid value
/// warns with `code` and is absent from the result.
fn read_scalars(
    value: &serde_json::Value,
    scalars: &[Scalar],
    code: &str,
    target: &str,
) -> (BTreeMap<&'static str, String>, Diagnostics) {
    let mut diags = Diagnostics::new();
    let mut found = BTreeMap::new();
    for scalar in scalars {
        let Some(v) = value.get(scalar.key) else {
            continue;
        };
        match v.as_str() {
            Some(s) if (scalar.valid)(s) => {
                found.insert(scalar.key, s.to_string());
            }
            _ => diags.push(
                Diagnostic::warning(
                    code,
                    format!(
                        "{target} target option `{}` must be {}; the default is used",
                        scalar.key, scalar.expected
                    ),
                )
                .with_help(format!("got {v}")),
            ),
        }
    }
    (found, diags)
}

/// Strict reusable defaults restricted to `allowed` scalar keys. Diagnostics
/// omit user values and carry pointers relative to this object.
fn parse_scalar_profile(
    value: &serde_json::Value,
    scalars: &[Scalar],
    allowed: &[&str],
    label: &str,
) -> Diagnostics {
    let mut diagnostics = Diagnostics::new();
    let error =
        |pointer: &str, message: String| Diagnostic::error("TG0602", message).at("", pointer, None);
    let Some(fields) = value.as_object() else {
        diagnostics.push(error(
            "",
            format!("profile {label} defaults must be a mapping"),
        ));
        return diagnostics;
    };
    for (key, value) in fields {
        let at = crate::pointer::child("", key);
        let Some(scalar) = scalars
            .iter()
            .find(|scalar| scalar.key == key && allowed.contains(&scalar.key))
        else {
            diagnostics.push(error(
                &at,
                format!("unsupported field in {label} profile defaults"),
            ));
            continue;
        };
        if !value.as_str().is_some_and(scalar.valid) {
            diagnostics.push(error(&at, format!("invalid {label} profile default")));
        }
    }
    diagnostics
}

/// Python option validators, shared with the Python emitter.
pub mod python {
    /// PEP 508 names: ASCII letters and digits, with `.`, `_` and `-` inside.
    pub fn is_distribution_name(s: &str) -> bool {
        let ok_edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
        ok_edge(s.chars().next())
            && ok_edge(s.chars().last())
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    }

    /// A PEP 440 release (`1`, `1.2.3`) with an optional pre-release (`a1`,
    /// `b2`, `rc1`), post-release (`.post1`) and dev release (`.dev1`).
    pub fn is_version(s: &str) -> bool {
        let digits = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit());
        let mut rest = s;
        if let Some((head, dev)) = rest.split_once(".dev") {
            if !digits(dev) {
                return false;
            }
            rest = head;
        }
        if let Some((head, post)) = rest.split_once(".post") {
            if !digits(post) {
                return false;
            }
            rest = head;
        }
        if let Some(i) = rest.find(|c: char| c.is_ascii_alphabetic()) {
            let (head, pre) = rest.split_at(i);
            let number = ["rc", "a", "b"]
                .into_iter()
                .find_map(|p| pre.strip_prefix(p));
            if !number.is_some_and(digits) {
                return false;
            }
            rest = head;
        }
        rest.split('.').all(digits)
    }

    /// A PEP 440 specifier set: comma-separated clauses of an operator and a
    /// version (`>=0.1,<0.2`, `==0.1.*`, `~=0.1`).
    pub fn is_specifier(s: &str) -> bool {
        !s.trim().is_empty()
            && s.split(',').all(|clause| {
                let c = clause.trim();
                let op = ["===", "~=", "==", "!=", "<=", ">=", "<", ">"]
                    .into_iter()
                    .find(|op| c.starts_with(op));
                op.is_some_and(|op| {
                    let v = c[op.len()..].trim();
                    let v = v.strip_suffix(".*").unwrap_or(v);
                    is_version(v)
                })
            })
    }

    pub fn is_path(s: &str) -> bool {
        !s.trim().is_empty() && !s.contains('"')
    }

    pub fn is_models(s: &str) -> bool {
        s == "pydantic" || s == "dataclasses"
    }

    pub fn is_python_version(s: &str) -> bool {
        matches!(s, "3.10" | "3.11" | "3.12")
    }
}

/// Rust option validators, shared with the Rust emitter.
pub mod rust {
    pub fn is_crate_name(s: &str) -> bool {
        s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && !matches!(
                s,
                "tungsten-runtime" | "tungsten-cli-kit" | "tungsten_runtime" | "tungsten_cli_kit"
            )
    }

    pub fn is_bin_name(s: &str) -> bool {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }

    pub fn is_path(s: &str) -> bool {
        !s.trim().is_empty() && !s.contains('"')
    }

    /// `1.2.3`, with an optional `-pre` suffix.
    pub fn is_semver(s: &str) -> bool {
        let (core, pre) = s.split_once('-').map_or((s, None), |(c, p)| (c, Some(p)));
        let parts: Vec<&str> = core.split('.').collect();
        parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            && pre.is_none_or(|p| {
                !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
            })
    }

    /// A Cargo version requirement: comparator clauses (`>=0.1, <0.3`, `^0.1`,
    /// `~0.1.2`, `0.1`, `*`).
    pub fn is_req(s: &str) -> bool {
        !s.trim().is_empty()
            && s.split(',').all(|clause| {
                let c = clause.trim();
                let c = c.trim_start_matches(['^', '~', '=', '<', '>']).trim();
                c == "*"
                    || (!c.is_empty()
                        && c.split('.').all(|p| {
                            p == "*" || (!p.is_empty() && p.chars().all(|ch| ch.is_ascii_digit()))
                        }))
            })
    }
}

const PYTHON_SCALARS: &[Scalar] = &[
    Scalar {
        key: "package",
        valid: python::is_distribution_name,
        expected: "a PEP 508 distribution name",
    },
    Scalar {
        key: "version",
        valid: python::is_version,
        expected: "a version such as 1.2.3",
    },
    Scalar {
        key: "runtime_path",
        valid: python::is_path,
        expected: "a non-empty path",
    },
    Scalar {
        key: "runtime",
        valid: python::is_specifier,
        expected: "a version specifier such as >=0.1,<0.2",
    },
    Scalar {
        key: "models",
        valid: python::is_models,
        expected: "`pydantic` or `dataclasses`",
    },
    Scalar {
        key: "python_version",
        valid: python::is_python_version,
        expected: "`3.10`, `3.11`, or `3.12`",
    },
];

/// Fields a Python profile may default. Paths and import names stay per project.
pub const PYTHON_PROFILE_FIELDS: &[&str] =
    &["package", "version", "runtime", "models", "python_version"];

/// Valid Python declarations shared by profiles and inspection; no defaults are
/// applied. `module` and other keys remain validated by the emitter alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PythonOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "python_package_schema")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "python_version_schema")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "python_runtime_path_schema")]
    pub runtime_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "python_runtime_schema")]
    pub runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "python_models_schema")]
    pub models: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "python_interpreter_schema")]
    pub python_version: Option<String>,
}

impl PythonOptions {
    /// Parse with the emitter's TG0731 scalar-fallback policy.
    pub fn parse(value: &serde_json::Value) -> (Self, Diagnostics) {
        let (mut found, diags) = read_scalars(value, PYTHON_SCALARS, "TG0731", "python");
        (
            Self {
                package: found.remove("package"),
                version: found.remove("version"),
                runtime_path: found.remove("runtime_path"),
                runtime: found.remove("runtime"),
                models: found.remove("models"),
                python_version: found.remove("python_version"),
            },
            diags,
        )
    }

    /// Strict reusable defaults; see [`PYTHON_PROFILE_FIELDS`].
    pub fn parse_profile(value: &serde_json::Value) -> (Self, Diagnostics) {
        let diagnostics =
            parse_scalar_profile(value, PYTHON_SCALARS, PYTHON_PROFILE_FIELDS, "Python");
        (Self::parse(value).0, diagnostics)
    }
}

const RUST_SCALARS: &[Scalar] = &[
    Scalar {
        key: "crate",
        valid: rust::is_crate_name,
        expected: "a crate name (letters, digits, `-`, `_`)",
    },
    Scalar {
        key: "version",
        valid: rust::is_semver,
        expected: "a version such as 1.2.3",
    },
    Scalar {
        key: "runtime_path",
        valid: rust::is_path,
        expected: "a non-empty path",
    },
    Scalar {
        key: "runtime",
        valid: rust::is_req,
        expected: "a version requirement such as 0.1",
    },
];

/// Fields a Rust profile may default. Paths and the CLI crate stay per project.
pub const RUST_PROFILE_FIELDS: &[&str] = &["crate", "version", "runtime"];

/// Valid Rust SDK declarations shared by profiles and inspection; no defaults
/// are applied. `cli` and other keys remain validated by the emitter alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RustOptions {
    #[serde(rename = "crate", default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "rust_crate_schema")]
    pub crate_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "rust_version_schema")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "rust_runtime_path_schema")]
    pub runtime_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "rust_runtime_schema")]
    pub runtime: Option<String>,
}

impl RustOptions {
    /// Parse with the emitter's TG0740 scalar-fallback policy.
    pub fn parse(value: &serde_json::Value) -> (Self, Diagnostics) {
        let (mut found, diags) = read_scalars(value, RUST_SCALARS, "TG0740", "rust");
        (
            Self {
                crate_name: found.remove("crate"),
                version: found.remove("version"),
                runtime_path: found.remove("runtime_path"),
                runtime: found.remove("runtime"),
            },
            diags,
        )
    }

    /// Strict reusable defaults; see [`RUST_PROFILE_FIELDS`].
    pub fn parse_profile(value: &serde_json::Value) -> (Self, Diagnostics) {
        let diagnostics = parse_scalar_profile(value, RUST_SCALARS, RUST_PROFILE_FIELDS, "Rust");
        (Self::parse(value).0, diagnostics)
    }
}

/// Supported-value schema of Python declarations; other keys remain permitted.
pub fn python_json_schema() -> serde_json::Value {
    let mut schema = schemars::schema_for!(PythonOptions).to_value();
    schema["additionalProperties"] = true.into();
    schema
}

/// Supported-value schema of Rust SDK declarations; other keys remain permitted.
pub fn rust_json_schema() -> serde_json::Value {
    let mut schema = schemars::schema_for!(RustOptions).to_value();
    schema["additionalProperties"] = true.into();
    schema
}

/// Strict profile subset of a supported-value schema.
fn profile_subset(mut schema: serde_json::Value, allowed: &[&str]) -> serde_json::Value {
    schema["properties"]
        .as_object_mut()
        .expect("option properties")
        .retain(|key, _| allowed.contains(&key.as_str()));
    schema["additionalProperties"] = false.into();
    schema
}

pub fn python_profile_json_schema() -> serde_json::Value {
    profile_subset(python_json_schema(), PYTHON_PROFILE_FIELDS)
}

pub fn rust_profile_json_schema() -> serde_json::Value {
    profile_subset(rust_json_schema(), RUST_PROFILE_FIELDS)
}

fn inspection(default_policy: &str, invalid_policy: &str, redact: bool) -> serde_json::Value {
    serde_json::json!({
        "default_policy": default_policy, "invalid_policy": invalid_policy, "redact": redact
    })
}

const SCALAR_FALLBACK: &str = "warning and emitter default";

fn python_package_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type":"string", "pattern": r"^[A-Za-z0-9]([A-Za-z0-9._-]*[A-Za-z0-9])?$",
        "x-tungsten-inspection": inspection("derive from the compiled API name; unresolved without compilation", SCALAR_FALLBACK, false)
    })
}

fn python_version_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type":"string",
        "pattern": r"^[0-9]+(\.[0-9]+)*((a|b|rc)[0-9]+)?(\.post[0-9]+)?(\.dev[0-9]+)?$",
        "default": PYTHON_PACKAGE_VERSION,
        "x-tungsten-inspection": inspection("fixed package version", SCALAR_FALLBACK, false)
    })
}

fn python_runtime_path_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type":"string", "pattern": r#"^[^"]*[^\u0009-\u000D \u0085   -     　"][^"]*$"#,
        "default": null,
        "x-tungsten-inspection": inspection("absent; use runtime or its default", "warning and ignore this path", true)
    })
}

fn python_runtime_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut policy = inspection(
        "fixed runtime specifier, narrowed to >=0.1.3 below Python 3.12, unless a valid runtime_path selects a path source",
        SCALAR_FALLBACK,
        true,
    );
    policy["overridden_by"] = "runtime_path".into();
    schemars::json_schema!({
        "type":"string", "minLength": 1, "default": PYTHON_RUNTIME_SPEC,
        "x-tungsten-inspection": policy
    })
}

fn python_models_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type":"string", "enum": ["pydantic", "dataclasses"], "default": "pydantic",
        "x-tungsten-inspection": inspection("Pydantic v2 models", SCALAR_FALLBACK, false)
    })
}

fn python_interpreter_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type":"string", "enum": ["3.10", "3.11", "3.12"], "default": PYTHON_VERSION,
        "x-tungsten-inspection": inspection("minimum interpreter 3.12", SCALAR_FALLBACK, false)
    })
}

fn rust_crate_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type":"string", "pattern": r"^[A-Za-z][A-Za-z0-9_-]*$",
        "not": {"enum": ["tungsten-runtime", "tungsten-cli-kit", "tungsten_runtime", "tungsten_cli_kit"]},
        "x-tungsten-inspection": inspection("derive from the compiled API name; unresolved without compilation", SCALAR_FALLBACK, false)
    })
}

fn rust_version_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type":"string", "pattern": r"^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.]+)?$",
        "default": RUST_PACKAGE_VERSION,
        "x-tungsten-inspection": inspection("fixed version of the SDK and CLI crates", SCALAR_FALLBACK, false)
    })
}

fn rust_runtime_path_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type":"string", "pattern": r#"^[^"]*[^\u0009-\u000D \u0085   -     　"][^"]*$"#,
        "default": null,
        "x-tungsten-inspection": inspection("absent; a path dependency keeps the runtime requirement", "warning and ignore this path", true)
    })
}

fn rust_runtime_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type":"string", "minLength": 1, "default": RUST_RUNTIME_REQ,
        "x-tungsten-inspection": inspection("fixed runtime requirement, also used with runtime_path", SCALAR_FALLBACK, true)
    })
}
