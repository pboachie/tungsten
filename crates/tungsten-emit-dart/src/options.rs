// SPDX-License-Identifier: AGPL-3.0-only
//! Target options (`tungsten.yml` `targets.dart`).
//!
//! | Option | Default | Meaning |
//! |---|---|---|
//! | `package` | `<api>_sdk` | Pub package name (lowercase letters, digits and `_`). |
//! | `version` | `0.1.0` | Package version (`1.2.3`, `1.2.3-beta.1`, `1.2.3+4`). |
//! | `runtime` | [`DEFAULT_RUNTIME_CONSTRAINT`] | Version constraint on the `tungsten_runtime` package. |
//! | `runtime_path` | none | A local `tungsten_runtime` checkout, written as the pubspec's `path:` dependency. |
//!
//! An invalid value is reported with TG1070 and the default is used.

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::TargetConfig;
use tungsten_ir::Ir;
use tungsten_ir::naming::{self, Case};

/// Default constraint on the `tungsten_runtime` package.
pub const DEFAULT_RUNTIME_CONSTRAINT: &str = "^0.1.0";

/// The pub name of the runtime package.
pub const RUNTIME_PACKAGE: &str = "tungsten_runtime";

/// Words a pub package name must not be: Dart's reserved words and
/// built-in identifiers (pub refuses them) and the packages generated code
/// imports.
const TAKEN_PACKAGES: &[&str] = &[
    "abstract",
    "as",
    "assert",
    "await",
    "base",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "covariant",
    "default",
    "deferred",
    "do",
    "dynamic",
    "else",
    "enum",
    "export",
    "extends",
    "extension",
    "external",
    "factory",
    "false",
    "final",
    "finally",
    "for",
    "get",
    "if",
    "implements",
    "import",
    "in",
    "interface",
    "is",
    "late",
    "library",
    "mixin",
    "new",
    "null",
    "operator",
    "part",
    "required",
    "rethrow",
    "return",
    "sealed",
    "set",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typedef",
    "var",
    "void",
    "when",
    "while",
    "with",
    "yield",
    "crypto",
    "tungsten_runtime",
];

/// Resolved target options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Pub package name, also the entry library's file name
    /// (`lib/<package>.dart`).
    pub package: String,
    /// Package version.
    pub version: String,
    /// Constraint on `tungsten_runtime` (pubspec dependency).
    pub runtime: String,
    /// A local runtime checkout for the pubspec's `path:` dependency.
    pub runtime_path: Option<String>,
}

impl Options {
    /// Read the options of `cfg`, falling back to defaults with a TG1070
    /// warning for each invalid value.
    pub fn resolve(ir: &Ir, cfg: &TargetConfig) -> (Options, Diagnostics) {
        let mut diags = Diagnostics::new();
        let mut read = |key: &str,
                        valid: &dyn Fn(&str) -> bool,
                        expected: &str|
         -> Option<String> {
            let v = cfg.options.get(key)?;
            match v.as_str() {
                Some(s) if valid(s) => Some(s.to_string()),
                _ => {
                    diags.push(
                        Diagnostic::warning(
                            "TG1070",
                            format!(
                                "dart target option `{key}` must be {expected}; the default is used"
                            ),
                        )
                        .with_help(format!("got {v}")),
                    );
                    None
                }
            }
        };
        let package = read(
            "package",
            &is_package_name,
            "a pub package name of lowercase letters, digits and `_` that is not a Dart keyword",
        )
        .unwrap_or_else(|| default_package(&ir.api.name.words));
        let version = read("version", &is_version, "a semantic version such as 1.2.3")
            .unwrap_or_else(|| "0.1.0".to_string());
        let runtime = read(
            "runtime",
            &is_constraint,
            "a pub version constraint such as ^0.1.0",
        )
        .unwrap_or_else(|| DEFAULT_RUNTIME_CONSTRAINT.to_string());
        let runtime_path = read(
            "runtime_path",
            &|s| {
                !s.trim().is_empty() && s.trim() == s && !s.contains(['"', '\\', '\n', '\r', '\t'])
            },
            "a non-empty path",
        );
        (
            Options {
                package,
                version,
                runtime,
                runtime_path,
            },
            diags,
        )
    }
}

/// `<api>_sdk`, prefixed with `sdk_` when the API name does not start with
/// a letter.
fn default_package(words: &[String]) -> String {
    let mut all = words.to_vec();
    all.push("sdk".to_string());
    let name: String = naming::to_case(&all, Case::Snake)
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
        .collect();
    let name = name.trim_matches('_').to_string();
    if name.starts_with(|c: char| c.is_ascii_lowercase()) {
        name
    } else {
        format!("sdk_{name}")
    }
}

/// A pub package name: lowercase ASCII letters, digits and `_`, starting
/// with a letter, not a Dart keyword or built-in identifier and not a
/// package generated code imports.
pub fn is_package_name(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !TAKEN_PACKAGES.contains(&s)
}

/// A semantic version: `MAJOR.MINOR.PATCH`, then an optional `-pre` and
/// `+build` of dot-separated alphanumeric (and `-`) identifiers.
pub fn is_version(s: &str) -> bool {
    let (rest, build) = match s.split_once('+') {
        Some((r, b)) => (r, Some(b)),
        None => (s, None),
    };
    let (core, pre) = match rest.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (rest, None),
    };
    let numeric = core.split('.').collect::<Vec<_>>();
    let idents = |t: &str| {
        t.split('.')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
    };
    numeric.len() == 3
        && numeric
            .iter()
            .all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        && pre.is_none_or(idents)
        && build.is_none_or(idents)
}

/// A pub version constraint: `any`, a version, `^<version>`, or clauses of
/// `>=`, `>`, `<=` or `<` and a version separated by spaces
/// (`>=0.1.0 <0.2.0`).
pub fn is_constraint(s: &str) -> bool {
    let s = s.trim();
    if s == "any" {
        return true;
    }
    if let Some(v) = s.strip_prefix('^') {
        return is_version(v);
    }
    let clauses: Vec<&str> = s.split_whitespace().collect();
    !clauses.is_empty()
        && clauses.iter().all(|c| {
            let op = [">=", "<=", ">", "<"]
                .into_iter()
                .find(|op| c.starts_with(op));
            let rest = op.map_or(*c, |op| &c[op.len()..]);
            is_version(rest)
        })
        && (clauses.len() == 1 || clauses.iter().all(|c| c.starts_with(['>', '<'])))
}
