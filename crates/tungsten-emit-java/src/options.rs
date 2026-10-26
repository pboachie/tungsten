// SPDX-License-Identifier: AGPL-3.0-only
//! Target options (`tungsten.yml` `targets.java`).
//!
//! | Option | Default | Meaning |
//! |---|---|---|
//! | `group_id` | `com.example` | Maven groupId of the SDK. |
//! | `artifact_id` | `<api>-sdk` | Maven artifactId of the SDK. |
//! | `package` | `com.example.<api>` | Java package of the SDK. |
//! | `version` | `0.1.0` | SDK version (pom and `User-Agent`). |
//! | `runtime` | [`DEFAULT_RUNTIME`] | Version of `io.github.pboachie.tungsten:tungsten-runtime`. |
//! | `runtime_path` | none | The runtime's directory (`runtimes/java` of a tungsten checkout), relative to the output directory: the pom compiles its sources into the SDK (a `build-helper` source root), so a local build needs no installed runtime. |
//!
//! An invalid value is reported as TG1010 and the default is used.

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::TargetConfig;
use tungsten_ir::Ir;
use tungsten_ir::naming::{self, Case, Role, Target};

/// The runtime version generated SDKs depend on.
pub const DEFAULT_RUNTIME: &str = "0.1.4";

/// Resolved target options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub group_id: String,
    pub artifact_id: String,
    /// The Java package (`com.example.zrotext`).
    pub package: String,
    pub version: String,
    pub runtime: String,
    pub runtime_path: Option<String>,
}

impl Options {
    /// The options of `cfg`, with a TG1010 warning for each invalid value.
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
                            "TG1010",
                            format!(
                                "java target option `{key}` must be {expected}; the default is used"
                            ),
                        )
                        .with_help(format!("got {v}")),
                    );
                        None
                    }
                }
            };
        let words = &ir.api.name.words;
        let api = default_segment(words);
        let group_id = read(
            "group_id",
            &is_package,
            "a Maven groupId such as io.github.acme",
        )
        .unwrap_or_else(|| "com.example".to_string());
        let artifact_id = read(
            "artifact_id",
            &is_artifact,
            "a Maven artifactId such as acme-sdk",
        )
        .unwrap_or_else(|| format!("{}-sdk", naming::to_case(words, Case::Kebab)));
        let package = read(
            "package",
            &is_package,
            "a Java package name such as io.github.acme.sdk",
        )
        .unwrap_or_else(|| format!("com.example.{api}"));
        let version = read("version", &is_version, "a version such as 1.2.3")
            .unwrap_or_else(|| "0.1.0".to_string());
        let runtime = read("runtime", &is_version, "a runtime version such as 0.1.0")
            .unwrap_or_else(|| DEFAULT_RUNTIME.to_string());
        let runtime_path = read(
            "runtime_path",
            &|s| !s.trim().is_empty() && !s.contains(['<', '>', '&', '"']),
            "a non-empty relative path",
        );
        (
            Options {
                group_id,
                artifact_id,
                package,
                version,
                runtime,
                runtime_path,
            },
            diags,
        )
    }
}

/// The API name as one package segment: lower case, a valid identifier.
fn default_segment(words: &[String]) -> String {
    let name = naming::render(
        &tungsten_ir::Ident {
            wire: words.join(" "),
            words: words.to_vec(),
        },
        Target::Java,
        Role::Module,
    );
    if is_segment(&name) {
        name
    } else {
        "api".to_string()
    }
}

/// A Java identifier usable as a package segment: ASCII letters, digits
/// and `_`, not starting with a digit, not a keyword.
fn is_segment(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !naming::is_reserved(s, Target::Java, Role::Module)
}

/// A dotted Java package name.
pub fn is_package(s: &str) -> bool {
    !s.is_empty() && s.split('.').all(is_segment)
}

fn is_artifact(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
}

fn is_version(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}
