// SPDX-License-Identifier: AGPL-3.0-only
//! Target options (`tungsten.yml` `targets.mcp`).
//!
//! | Option | Meaning | Default |
//! |---|---|---|
//! | `package` | npm package name of the server | `<api>-mcp` |
//! | `version` | package version | `0.1.0` |
//! | `sdk` | the generated TypeScript SDK: `<package>` or `<package>@<range>` | the `typescript` target's default package, `^0.1.0` |
//! | `sdk_path` | depend on the SDK by path (`file:<path>`) instead of a range | |
//! | `runtime` | `@tungsten/mcp` version range | `^0.1.0` |
//! | `runtime_path` | depend on `@tungsten/mcp` by path | |
//! | `sandbox` | enable the opt-in `run_script` sandbox (planning/02 D7) | `false` |
//!
//! An invalid value is a TG0720 warning and the default is used.

use serde_json::Value;
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::TargetConfig;
use tungsten_ir::Ir;
use tungsten_ir::naming::{self, Case};

/// Default version range of `@tungsten/mcp`, `@tungsten/runtime` and the SDK.
pub const DEFAULT_RANGE: &str = "^0.1.0";
/// TypeScript version pinned in the generated package.
pub const TYPESCRIPT_VERSION: &str = "5.9.3";
/// `@types/node` version pinned in the generated package.
pub const NODE_TYPES_VERSION: &str = "22.20.5";

/// Resolved target options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// npm package name of the server.
    pub package: String,
    pub version: String,
    /// Executable name: `<api>-mcp`.
    pub bin: String,
    /// npm package name of the generated SDK.
    pub sdk_package: String,
    /// Dependency specifier of the SDK.
    pub sdk: String,
    /// Dependency specifier of `@tungsten/mcp`.
    pub runtime: String,
    pub sandbox: bool,
}

impl Options {
    /// Read the options of `cfg`, falling back to defaults with a TG0720
    /// warning for each invalid value.
    pub fn resolve(ir: &Ir, cfg: &TargetConfig) -> (Options, Diagnostics) {
        let mut diags = Diagnostics::new();
        let kebab = naming::to_case(&ir.api.name.words, Case::Kebab);
        let kebab = if kebab.is_empty() {
            "api".to_string()
        } else {
            kebab
        };
        let mut invalid = |key: &str, expected: &str, got: &Value| {
            diags.push(
                Diagnostic::warning(
                    "TG0720",
                    format!("target option `mcp.{key}` must be {expected}; the default is used"),
                )
                .with_help(format!("got {got}")),
            );
        };
        let mut read = |key: &str, valid: &dyn Fn(&str) -> bool, expected: &str| {
            let v = cfg.options.get(key)?;
            match v.as_str() {
                Some(s) if valid(s) => Some(s.to_string()),
                _ => {
                    invalid(key, expected, v);
                    None
                }
            }
        };
        let package = read("package", &is_package_name, "a valid npm package name")
            .unwrap_or_else(|| format!("{kebab}-mcp"));
        let version = read("version", &is_semver, "a semantic version such as 1.2.3")
            .unwrap_or_else(|| "0.1.0".to_string());
        let sdk_spec = read(
            "sdk",
            &|s| split_spec(s).is_some(),
            "an npm package name, optionally followed by @<version range>",
        );
        let sdk_path = read("sdk_path", &non_empty, "a non-empty path");
        let runtime_range = read("runtime", &non_empty, "a non-empty version range");
        let runtime_path = read("runtime_path", &non_empty, "a non-empty path");
        let sandbox = match cfg.options.get("sandbox") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(other) => {
                invalid("sandbox", "true or false", other);
                false
            }
        };
        let (sdk_package, sdk_range) = match sdk_spec.as_deref().and_then(split_spec) {
            Some((name, range)) => (name.to_string(), range.map(str::to_string)),
            None => (default_sdk_package(ir), None),
        };
        let sdk = match sdk_path {
            Some(p) => format!("file:{p}"),
            None => sdk_range.unwrap_or_else(|| DEFAULT_RANGE.to_string()),
        };
        let runtime = match runtime_path {
            Some(p) => format!("file:{p}"),
            None => runtime_range.unwrap_or_else(|| DEFAULT_RANGE.to_string()),
        };
        (
            Options {
                package,
                version,
                bin: format!("{kebab}-mcp"),
                sdk_package,
                sdk,
                runtime,
                sandbox,
            },
            diags,
        )
    }
}

/// The package name the `typescript` target uses without options.
fn default_sdk_package(ir: &Ir) -> String {
    let cfg = TargetConfig {
        name: "typescript".into(),
        out_dir: std::path::PathBuf::new(),
        options: serde_json::json!({}),
    };
    tungsten_emit_ts::Options::resolve(ir, &cfg).0.package
}

fn non_empty(s: &str) -> bool {
    !s.trim().is_empty()
}

/// `name` or `name@range` (the scope's `@` excluded) with a valid name and
/// a non-empty range.
fn split_spec(s: &str) -> Option<(&str, Option<&str>)> {
    let at = s
        .char_indices()
        .skip(1)
        .find(|(_, c)| *c == '@')
        .map(|(i, _)| i);
    let (name, range) = match at {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let range_ok = range.is_none_or(non_empty);
    (is_package_name(name) && range_ok).then_some((name, range))
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
