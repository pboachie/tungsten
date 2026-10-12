// SPDX-License-Identifier: AGPL-3.0-only
//! Target options (`tungsten.yml` `targets.csharp`).
//!
//! | Option | Default | Meaning |
//! |---|---|---|
//! | `package_id` | `<Api>.Sdk` | NuGet package id and assembly name of the SDK. |
//! | `namespace` | the package id | Root C# namespace of the SDK. |
//! | `version` | `0.1.0` | SDK version (project and `User-Agent`). |
//! | `runtime` | [`DEFAULT_RUNTIME`] | Version of the `Tungsten.Runtime` NuGet package. |
//! | `runtime_path` | none | The runtime's directory (`runtimes/csharp` of a tungsten checkout), relative to the output directory: the project references `Tungsten.Runtime.csproj` there instead of the package, so a local build needs no published runtime. |
//!
//! An invalid value is reported as TG1020 and the default is used.

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::TargetConfig;
use tungsten_ir::Ir;
use tungsten_ir::naming::{self, Role, Target};

/// The runtime version generated SDKs depend on.
pub const DEFAULT_RUNTIME: &str = "0.1.0";

/// Resolved target options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// NuGet package id and assembly name (`Zrotext.Sdk`).
    pub package_id: String,
    /// Root namespace (`Zrotext.Sdk`).
    pub namespace: String,
    pub version: String,
    pub runtime: String,
    pub runtime_path: Option<String>,
}

impl Options {
    /// The options of `cfg`, with a TG1020 warning for each invalid value.
    pub fn resolve(ir: &Ir, cfg: &TargetConfig) -> (Options, Diagnostics) {
        let mut diags = Diagnostics::new();
        let mut read =
            |key: &str, valid: &dyn Fn(&str) -> bool, expected: &str| -> Option<String> {
                let v = cfg.options.get(key)?;
                match v.as_str() {
                    Some(s) if valid(s) => Some(s.to_string()),
                    _ => {
                        diags.push(
                            Diagnostic::warning(
                                "TG1020",
                                format!(
                                    "csharp target option `{key}` must be {expected}; the default is used"
                                ),
                            )
                            .with_help(format!("got {v}")),
                        );
                        None
                    }
                }
            };
        let api = default_segment(&ir.api.name.words);
        let package_id = read(
            "package_id",
            &is_package_id,
            "a NuGet package id such as Acme.Sdk",
        )
        .unwrap_or_else(|| format!("{api}.Sdk"));
        let namespace = read(
            "namespace",
            &is_namespace,
            "a dotted C# namespace such as Acme.Sdk",
        )
        .unwrap_or_else(|| {
            if is_namespace(&package_id) {
                package_id.clone()
            } else {
                format!("{api}.Sdk")
            }
        });
        let version = read("version", &is_version, "a version such as 1.2.3")
            .unwrap_or_else(|| "0.1.0".to_string());
        let runtime = read("runtime", &is_version, "a runtime version such as 0.1.0")
            .unwrap_or_else(|| DEFAULT_RUNTIME.to_string());
        let runtime_path = read(
            "runtime_path",
            &|s| !s.trim().is_empty() && !s.contains(['<', '>', '&', '"', '\'']),
            "a non-empty relative path",
        );
        (
            Options {
                package_id,
                namespace,
                version,
                runtime,
                runtime_path,
            },
            diags,
        )
    }
}

/// The API name as one namespace segment (PascalCase), else `Api`.
fn default_segment(words: &[String]) -> String {
    let name = naming::render(
        &tungsten_ir::Ident {
            wire: words.join(" "),
            words: words.to_vec(),
        },
        Target::CSharp,
        Role::Module,
    );
    if is_segment(&name) {
        name
    } else {
        "Api".to_string()
    }
}

/// A C# identifier usable as a namespace segment: ASCII letters, digits
/// and `_`, not starting with a digit, not a keyword.
fn is_segment(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !naming::is_reserved(s, Target::CSharp, Role::Module)
}

/// A dotted C# namespace.
pub fn is_namespace(s: &str) -> bool {
    !s.is_empty() && s.split('.').all(is_segment)
}

/// A NuGet package id: letters, digits, `.`, `-` and `_`, starting with a
/// letter or digit, at most 100 characters.
fn is_package_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && !s.contains("..")
        && !s.ends_with('.')
}

fn is_version(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}
