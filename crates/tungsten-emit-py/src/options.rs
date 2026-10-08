// SPDX-License-Identifier: AGPL-3.0-only
//! Target options (`tungsten.yml` `targets.python`).
//!
//! | Option | Default | Meaning |
//! |---|---|---|
//! | `package` | `<api>-sdk` | Distribution name (PEP 508). |
//! | `module` | `<api>_sdk` | Import name of the package. |
//! | `version` | `0.1.0` | Package version (PEP 440 release, optional pre/post/dev). |
//! | `runtime` | [`DEFAULT_RUNTIME_SPEC`] | Version specifier of `tungsten-runtime`. |
//! | `runtime_path` | none | A local `tungsten-runtime` checkout, as a uv path source. |
//! | `models` | `pydantic` | `dataclasses` (stdlib-only output) is planned and not supported yet. |

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::TargetConfig;
use tungsten_ir::Ir;
use tungsten_ir::naming::{self, Case};

use crate::py::{is_identifier, is_keyword};

/// Default `tungsten-runtime` version specifier.
pub const DEFAULT_RUNTIME_SPEC: &str = ">=0.1.0,<0.2.0";
/// Pydantic range of generated packages: PEP 695 `type` aliases are
/// supported since 2.10.
pub const PYDANTIC_RANGE: &str = ">=2.10,<3";
/// httpx range of generated packages (the runtime's transport).
pub const HTTPX_RANGE: &str = ">=0.27,<1";

/// Where the runtime dependency comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeDep {
    /// A version specifier (`>=0.1.0,<0.2.0`).
    Spec(String),
    /// A local path (a uv path source).
    Path(String),
}

/// Resolved target options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Distribution name.
    pub package: String,
    /// Import name.
    pub module: String,
    /// Package version.
    pub version: String,
    pub runtime: RuntimeDep,
}

/// Module names a generated package must not take: they would shadow its
/// own dependencies.
const TAKEN_MODULES: &[&str] = &["httpx", "pydantic", "pydantic_core", "tungsten_runtime"];

impl Options {
    /// Read the options of `cfg`, falling back to defaults with a TG0731
    /// warning for each invalid value.
    pub fn resolve(ir: &Ir, cfg: &TargetConfig) -> (Options, Diagnostics) {
        let mut diags = Diagnostics::new();
        let words = &ir.api.name.words;
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
                            "TG0731",
                            format!(
                                "python target option `{key}` must be {expected}; the default is used"
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
            &is_distribution_name,
            "a PEP 508 distribution name",
        )
        .unwrap_or_else(|| format!("{}-sdk", naming::to_case(words, Case::Kebab)));
        let module = read(
            "module",
            &is_module_name,
            "a lowercase Python identifier that is not a keyword",
        )
        .unwrap_or_else(|| default_module(words));
        let version = read("version", &is_version, "a version such as 1.2.3")
            .unwrap_or_else(|| "0.1.0".to_string());
        let runtime_path = read(
            "runtime_path",
            &|s| !s.trim().is_empty() && !s.contains('"'),
            "a non-empty path",
        );
        let runtime_spec = read(
            "runtime",
            &is_specifier,
            "a version specifier such as >=0.1,<0.2",
        );
        read(
            "models",
            &|s| s == "pydantic",
            "`pydantic` (`dataclasses` output is not supported yet)",
        );
        let runtime = match runtime_path {
            Some(p) => RuntimeDep::Path(p),
            None => {
                RuntimeDep::Spec(runtime_spec.unwrap_or_else(|| DEFAULT_RUNTIME_SPEC.to_string()))
            }
        };
        (
            Options {
                package,
                module,
                version,
                runtime,
            },
            diags,
        )
    }
}

/// `<api>_sdk`, or `sdk_<api>_sdk` when the API name starts with a digit.
fn default_module(words: &[String]) -> String {
    let name = format!("{}_sdk", naming::to_case(words, Case::Snake));
    if name.starts_with(|c: char| c.is_ascii_digit() || c == '_') {
        format!("sdk_{}", name.trim_start_matches('_'))
    } else {
        name
    }
}

/// PEP 508 names: ASCII letters and digits, with `.`, `_` and `-` inside.
fn is_distribution_name(s: &str) -> bool {
    let ok_edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    ok_edge(s.chars().next())
        && ok_edge(s.chars().last())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

fn is_module_name(s: &str) -> bool {
    is_identifier(s)
        && !is_keyword(s)
        && !s.starts_with('_')
        && s.chars().all(|c| !c.is_ascii_uppercase())
        && !TAKEN_MODULES.contains(&s)
}

/// A PEP 440 release (`1`, `1.2.3`) with an optional pre-release (`a1`,
/// `b2`, `rc1`), post-release (`.post1`) and dev release (`.dev1`).
fn is_version(s: &str) -> bool {
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
fn is_specifier(s: &str) -> bool {
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
