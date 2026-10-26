// SPDX-License-Identifier: AGPL-3.0-only
//! Target options (`tungsten.yml` `targets.ruby`).
//!
//! | Option | Default | Meaning |
//! |---|---|---|
//! | `gem` | `<api>-sdk` | Gem name (lowercase letters, digits, `-` and `_`). |
//! | `module` | `<Api>Sdk` | The SDK's module, a Ruby constant path (`Acme::Sdk`). |
//! | `version` | `0.1.0` | Gem version (`1.2.3`, `1.2.3.pre1`). |
//! | `runtime` | [`DEFAULT_RUNTIME_REQUIREMENT`] | Version requirement of the `tungsten-runtime` gem. |
//! | `runtime_path` | none | A local `tungsten-runtime` checkout, as the Gemfile's `path:` source. |
//!
//! An invalid value is reported with TG1060 and the default is used.

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::TargetConfig;
use tungsten_ir::Ir;
use tungsten_ir::naming::{self, Case};

/// Default requirement on the `tungsten-runtime` gem.
pub const DEFAULT_RUNTIME_REQUIREMENT: &str =
    tungsten_config::option_fields::RUBY_RUNTIME_REQUIREMENT;

/// Constants a generated module must not be named: the runtime's root and
/// the core classes generated code relies on.
const TAKEN_MODULES: &[&str] = &[
    "Tungsten",
    "Object",
    "Kernel",
    "BasicObject",
    "Module",
    "Class",
    "Data",
    "Struct",
    "Hash",
    "Array",
    "String",
    "Integer",
    "Float",
    "JSON",
    "Net",
    "URI",
    "OpenSSL",
    "Comparable",
];

/// Resolved target options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Gem name.
    pub gem: String,
    /// The SDK module's constant path, segments in order (`["Acme", "Sdk"]`).
    pub module: Vec<String>,
    /// Gem version.
    pub version: String,
    /// Requirement on `tungsten-runtime` (gemspec dependency).
    pub runtime: String,
    /// A local runtime checkout for the Gemfile.
    pub runtime_path: Option<String>,
}

impl Options {
    /// The module's constant path (`Acme::Sdk`).
    pub fn module_path(&self) -> String {
        self.module.join("::")
    }

    /// The file path of the gem's entry point under `lib/`, without `.rb`
    /// (the gem name with `-` written as `_`).
    pub fn lib_path(&self) -> String {
        self.gem.replace('-', "_")
    }

    /// Read the options of `cfg`, falling back to defaults with a TG1060
    /// warning for each invalid value.
    pub fn resolve(ir: &Ir, cfg: &TargetConfig) -> (Options, Diagnostics) {
        let mut diags = Diagnostics::new();
        let words = &ir.api.name.words;
        let mut read =
            |key: &str, valid: &dyn Fn(&str) -> bool, expected: &str| -> Option<String> {
                let v = cfg.options.get(key)?;
                match v.as_str() {
                    Some(s) if valid(s) => Some(s.to_string()),
                    _ => {
                        diags.push(
                        Diagnostic::warning(
                            "TG1060",
                            format!(
                                "ruby target option `{key}` must be {expected}; the default is used"
                            ),
                        )
                        .with_help(format!("got {v}")),
                    );
                        None
                    }
                }
            };
        let gem = read(
            "gem",
            &is_gem_name,
            "a gem name of lowercase letters, digits, `-` and `_`",
        )
        .unwrap_or_else(|| default_gem(words));
        let module = read(
            "module",
            &is_module_path,
            "a Ruby constant path such as Acme::Sdk",
        )
        .map(|m| m.split("::").map(str::to_string).collect())
        .unwrap_or_else(|| vec![default_module(words)]);
        let version = read("version", &is_version, "a gem version such as 1.2.3")
            .unwrap_or_else(|| tungsten_config::option_fields::PACKAGE_VERSION.to_string());
        let runtime = read(
            "runtime",
            &is_requirement,
            "a gem requirement such as ~> 0.1",
        )
        .unwrap_or_else(|| DEFAULT_RUNTIME_REQUIREMENT.to_string());
        let runtime_path = read(
            "runtime_path",
            &|s| !s.trim().is_empty() && !s.contains(['"', '\\', '#', '\n']),
            "a non-empty path",
        );
        (
            Options {
                gem,
                module,
                version,
                runtime,
                runtime_path,
            },
            diags,
        )
    }
}

/// `<api>-sdk`, prefixed with `sdk-` when the API name starts with a digit.
fn default_gem(words: &[String]) -> String {
    let name = format!("{}-sdk", naming::to_case(words, Case::Kebab));
    if name.starts_with(|c: char| c.is_ascii_lowercase()) {
        name
    } else {
        format!("sdk-{}", name.trim_start_matches(['-', '_']))
    }
}

/// `<Api>Sdk`, prefixed with `Sdk` when the API name does not start with a
/// letter.
fn default_module(words: &[String]) -> String {
    let mut all = words.to_vec();
    all.push("sdk".to_string());
    let name = naming::to_case(&all, Case::Pascal);
    if name.starts_with(|c: char| c.is_ascii_uppercase()) {
        name
    } else {
        format!("Sdk{name}")
    }
}

fn is_gem_name(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_lowercase())
        && !s.ends_with(['-', '_'])
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && s != "tungsten-runtime"
        && s != "tungsten_runtime"
}

/// A constant path: segments that start with an uppercase ASCII letter and
/// continue with ASCII letters, digits and `_`; the first is not a core
/// class or the runtime's module.
fn is_module_path(s: &str) -> bool {
    let segments: Vec<&str> = s.split("::").collect();
    segments.iter().all(|seg| {
        seg.starts_with(|c: char| c.is_ascii_uppercase())
            && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    }) && !TAKEN_MODULES.contains(&segments[0])
}

/// A gem version: numeric segments, then optional alphanumeric ones
/// (`1.2.3`, `1.2.3.pre1`, `2.0.0.rc.1`).
fn is_version(s: &str) -> bool {
    let mut parts = s.split('.');
    let first = parts.next().unwrap_or("");
    !first.is_empty()
        && first.chars().all(|c| c.is_ascii_digit())
        && parts.all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// A gem requirement: comma-separated clauses of an operator and a version
/// (`~> 0.1`, `>= 0.1.0, < 0.2`).
fn is_requirement(s: &str) -> bool {
    !s.trim().is_empty()
        && s.split(',').all(|clause| {
            let c = clause.trim();
            let op = ["~>", ">=", "<=", "!=", "=", ">", "<"]
                .into_iter()
                .find(|op| c.starts_with(op));
            let rest = op.map_or(c, |op| &c[op.len()..]);
            is_version(rest.trim())
        })
}
