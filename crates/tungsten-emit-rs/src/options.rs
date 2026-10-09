// SPDX-License-Identifier: AGPL-3.0-only
//! Target options (`tungsten.yml` `targets.rust`), shared by the SDK and CLI
//! halves. The options are stable; new ones are additive.
//!
//! | Option | Default | Meaning |
//! |---|---|---|
//! | `crate` | `<api>-sdk` | SDK crate name (letters, digits, `-`, `_`; starts with a letter). |
//! | `version` | `0.1.0` | Version of both crates (semver). |
//! | `runtime` | [`DEFAULT_RUNTIME_REQ`] | Version requirement of `tungsten-runtime`. |
//! | `runtime_path` | none | A local `tungsten-runtime` crate, as a path dependency. |
//! | `cli` | enabled | `false` skips the CLI crate; an object sets `bin` (default `<api>`), `crate` (default `<api>-cli`), `kit` (requirement of `tungsten-cli-kit`, default [`DEFAULT_RUNTIME_REQ`]) and `kit_path` (a local `tungsten-cli-kit` crate). |

use serde_json::Value;
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::TargetConfig;
use tungsten_ir::Ir;
use tungsten_ir::naming::{self, Case};

/// Default version requirement of `tungsten-runtime` and `tungsten-cli-kit`.
pub const DEFAULT_RUNTIME_REQ: &str = "0.1";

/// Where a dependency comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dep {
    /// A version requirement (`0.1`).
    Version(String),
    /// A local path.
    Path(String),
}

/// The CLI crate's options; `None` in [`Options::cli`] when disabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOptions {
    /// Binary name.
    pub bin: String,
    /// CLI crate name.
    pub package: String,
    pub kit: Dep,
}

/// Resolved target options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// SDK crate name (`zrotext-sdk`); its library name is the same with
    /// `-` replaced by `_`.
    pub package: String,
    pub version: String,
    pub runtime: Dep,
    pub cli: Option<CliOptions>,
}

impl Options {
    /// The SDK crate's library name (`zrotext_sdk`).
    pub fn lib_name(&self) -> String {
        self.package.replace('-', "_")
    }

    /// Read the options of `cfg`, falling back to defaults with a TG0740
    /// warning for each invalid value.
    pub fn resolve(ir: &Ir, cfg: &TargetConfig) -> (Options, Diagnostics) {
        let mut diags = Diagnostics::new();
        let words = &ir.api.name.words;
        let kebab = naming::to_case(words, Case::Kebab);
        let mut read = |obj: &Value,
                        scope: &str,
                        key: &str,
                        valid: &dyn Fn(&str) -> bool,
                        expected: &str| {
            let v = obj.get(key)?;
            match v.as_str() {
                Some(s) if valid(s) => Some(s.to_string()),
                _ => {
                    diags.push(
                        Diagnostic::warning(
                            "TG0740",
                            format!("rust target option `{scope}{key}` must be {expected}; the default is used"),
                        )
                        .with_help(format!("got {v}")),
                    );
                    None
                }
            }
        };
        let o = &cfg.options;
        let package = read(
            o,
            "",
            "crate",
            &is_crate_name,
            "a crate name (letters, digits, `-`, `_`)",
        )
        .unwrap_or_else(|| format!("{kebab}-sdk"));
        let version = read(o, "", "version", &is_semver, "a version such as 1.2.3")
            .unwrap_or_else(|| "0.1.0".into());
        let runtime_path = read(o, "", "runtime_path", &is_path, "a non-empty path");
        let runtime_req = read(
            o,
            "",
            "runtime",
            &is_req,
            "a version requirement such as 0.1",
        );
        let runtime = match runtime_path {
            Some(p) => Dep::Path(p),
            None => Dep::Version(runtime_req.unwrap_or_else(|| DEFAULT_RUNTIME_REQ.into())),
        };
        let cli = match o.get("cli") {
            Some(Value::Bool(false)) => None,
            Some(c @ Value::Object(_)) => {
                let bin = read(
                    c,
                    "cli.",
                    "bin",
                    &is_bin_name,
                    "a binary name (letters, digits, `-`, `_`)",
                )
                .unwrap_or_else(|| kebab.clone());
                let package = read(
                    c,
                    "cli.",
                    "crate",
                    &is_crate_name,
                    "a crate name (letters, digits, `-`, `_`)",
                )
                .unwrap_or_else(|| format!("{kebab}-cli"));
                let kit_path = read(c, "cli.", "kit_path", &is_path, "a non-empty path");
                let kit_req = read(
                    c,
                    "cli.",
                    "kit",
                    &is_req,
                    "a version requirement such as 0.1",
                );
                let kit = match kit_path {
                    Some(p) => Dep::Path(p),
                    None => Dep::Version(kit_req.unwrap_or_else(|| DEFAULT_RUNTIME_REQ.into())),
                };
                Some(CliOptions { bin, package, kit })
            }
            Some(other) if !matches!(other, Value::Bool(true)) => {
                diags.push(
                    Diagnostic::warning(
                        "TG0740",
                        "rust target option `cli` must be a boolean or an object; the default is used",
                    )
                    .with_help(format!("got {other}")),
                );
                Some(default_cli(&kebab))
            }
            _ => Some(default_cli(&kebab)),
        };
        (
            Options {
                package,
                version,
                runtime,
                cli,
            },
            diags,
        )
    }
}

fn default_cli(kebab: &str) -> CliOptions {
    CliOptions {
        bin: kebab.to_string(),
        package: format!("{kebab}-cli"),
        kit: Dep::Version(DEFAULT_RUNTIME_REQ.into()),
    }
}

fn is_crate_name(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !matches!(
            s,
            "tungsten-runtime" | "tungsten-cli-kit" | "tungsten_runtime" | "tungsten_cli_kit"
        )
}

fn is_bin_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn is_path(s: &str) -> bool {
    !s.trim().is_empty() && !s.contains('"')
}

/// `1`, `1.2`, `1.2.3`, with an optional `-pre` suffix.
fn is_semver(s: &str) -> bool {
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
fn is_req(s: &str) -> bool {
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
