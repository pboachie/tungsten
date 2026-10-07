// SPDX-License-Identifier: AGPL-3.0-only
//! Target options (`tungsten.yml` `targets.typescript`).

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::TargetConfig;
use tungsten_ir::Ir;
use tungsten_ir::naming::{self, Case};

/// Default `@tungsten/runtime` version range.
pub const DEFAULT_RUNTIME_RANGE: &str = "^0.1.0";
/// Zod version range of generated packages: `exactOptional()`, which keeps
/// optional properties exact, exists since 4.3.0.
pub const ZOD_RANGE: &str = "^4.3.0";
/// TypeScript version pinned in generated packages.
pub const TYPESCRIPT_VERSION: &str = "5.9.3";

/// Resolved target options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// npm package name.
    pub package: String,
    /// Package version.
    pub version: String,
    /// `@tungsten/runtime` dependency specifier.
    pub runtime: String,
}

impl Options {
    /// Read the options of `cfg`, falling back to defaults with a TG0711
    /// warning for each invalid value.
    pub fn resolve(ir: &Ir, cfg: &TargetConfig) -> (Options, Diagnostics) {
        let mut diags = Diagnostics::new();
        let default_package = format!("{}-sdk", naming::to_case(&ir.api.name.words, Case::Kebab));
        let mut read =
            |key: &str, valid: &dyn Fn(&str) -> bool, expected: &str| -> Option<String> {
                let v = cfg.options.get(key)?;
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
        let package = read("package", &is_package_name, "a valid npm package name")
            .unwrap_or(default_package);
        let version = read("version", &is_semver, "a semantic version such as 1.2.3")
            .unwrap_or_else(|| "0.1.0".to_string());
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
        let runtime = match runtime_path {
            Some(p) => format!("file:{p}"),
            None => runtime_range.unwrap_or_else(|| DEFAULT_RUNTIME_RANGE.to_string()),
        };
        (
            Options {
                package,
                version,
                runtime,
            },
            diags,
        )
    }
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
