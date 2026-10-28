// SPDX-License-Identifier: AGPL-3.0-only
//! Target options (`tungsten.yml` `targets.fsharp`).
//!
//! | Option | Default | Meaning |
//! |---|---|---|
//! | `package_id` | `<Api>.Sdk` | NuGet package id and assembly name of the SDK. |
//! | `namespace` | the package id | Root F# namespace of the SDK. |
//! | `version` | `0.1.0` | SDK version (project and `User-Agent`). |
//! | `runtime` | [`DEFAULT_RUNTIME`] | Version of the runtime packages: `Tungsten.Runtime.FSharp`, which depends on `Tungsten.Runtime`. |
//! | `runtime_path` | none | The F# layer's directory (`runtimes/fsharp` of a tungsten checkout), relative to the output directory: the project references `Tungsten.Runtime.FSharp.fsproj` there and the .NET runtime's project instead of the packages, so a local build needs no published runtime. |
//! | `csharp_runtime_path` | `<runtime_path>/../csharp` | The .NET runtime's directory (`runtimes/csharp`), relative to the output directory; only with `runtime_path`. |
//!
//! An invalid value is reported as TG1080 and the default is used.

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::TargetConfig;
use tungsten_ir::Ir;
use tungsten_ir::naming::{self, Role, Target};

/// The version of the runtime packages generated SDKs depend on.
pub const DEFAULT_RUNTIME: &str = "0.1.5";

/// The FSharp.Core version the SDK pins: the one the .NET SDK 10.0.401
/// bundles in its library pack, so a restore needs no package source.
pub const FSHARP_CORE: &str = "10.1.501";

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
    pub csharp_runtime_path: Option<String>,
}

impl Options {
    /// The options of `cfg`, with a TG1080 warning for each invalid value.
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
                            "TG1080",
                            format!(
                                "fsharp target option `{key}` must be {expected}; the default is used"
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
            "a dotted F# namespace such as Acme.Sdk",
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
        let runtime_path = read("runtime_path", &is_path, "a non-empty relative path");
        let csharp_runtime_path =
            read("csharp_runtime_path", &is_path, "a non-empty relative path");
        (
            Options {
                package_id,
                namespace,
                version,
                runtime,
                runtime_path,
                csharp_runtime_path,
            },
            diags,
        )
    }

    /// The .NET runtime's directory of a local build: `csharp_runtime_path`,
    /// else `csharp` next to the F# layer.
    pub fn csharp_dir(&self) -> Option<String> {
        let layer = self.runtime_path.as_ref()?;
        Some(match &self.csharp_runtime_path {
            Some(p) => p.trim_end_matches('/').to_string(),
            None => format!("{}/../csharp", layer.trim_end_matches('/')),
        })
    }
}

fn is_path(s: &str) -> bool {
    !s.trim().is_empty() && !s.contains(['<', '>', '&', '"', '\'', ';', '*', '?'])
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

/// An identifier usable as an F# namespace segment: ASCII letters, digits
/// and `_`, starting with a letter, not an F# or C# keyword (the SDK is
/// also used from C#).
fn is_segment(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !crate::fs::is_keyword(s)
        && !naming::is_reserved(s, Target::CSharp, Role::Module)
}

/// A dotted F# namespace.
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
