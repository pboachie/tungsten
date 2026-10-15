// SPDX-License-Identifier: AGPL-3.0-only
//! Target options (`tungsten.yml` `targets.kotlin`).
//!
//! | Option | Default | Meaning |
//! |---|---|---|
//! | `group` | `com.example` | Gradle group (Maven groupId) of the SDK. |
//! | `artifact` | `<api>-sdk` | Artifact name of the SDK (`rootProject.name`). |
//! | `package` | `com.example.<api>` | Kotlin package of the SDK. |
//! | `version` | `0.1.0` | SDK version (Gradle and `User-Agent`). |
//! | `runtime` | [`DEFAULT_RUNTIME`] | Version of both runtime artifacts, `io.github.pboachie.tungsten:tungsten-runtime-kotlin` and `io.github.pboachie.tungsten:tungsten-runtime`. |
//! | `runtime_path` | none | The Kotlin runtime layer's directory (`runtimes/kotlin` of a tungsten checkout), relative to the output directory: its sources are compiled into the SDK instead of the `tungsten-runtime-kotlin` dependency. |
//! | `java_runtime_path` | none | The Java runtime's directory (`runtimes/java`), relative to the output directory: its sources are compiled into the SDK instead of the `tungsten-runtime` dependency. |
//!
//! An invalid value is reported as TG1030 and the default is used.

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::TargetConfig;
use tungsten_ir::Ir;
use tungsten_ir::naming::{self, Case, Role, Target};

/// The runtime version generated SDKs depend on.
pub const DEFAULT_RUNTIME: &str = "0.1.3";

/// Resolved target options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub group: String,
    pub artifact: String,
    /// The Kotlin package (`com.example.zrotext`).
    pub package: String,
    pub version: String,
    pub runtime: String,
    pub runtime_path: Option<String>,
    pub java_runtime_path: Option<String>,
}

impl Options {
    /// The options of `cfg`, with a TG1030 warning for each invalid value.
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
                                "TG1030",
                                format!(
                                    "kotlin target option `{key}` must be {expected}; the default is used"
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
        let group = read(
            "group",
            &is_package,
            "a Gradle group such as io.github.acme",
        )
        .unwrap_or_else(|| "com.example".to_string());
        let artifact = read(
            "artifact",
            &is_artifact,
            "an artifact name such as acme-sdk",
        )
        .unwrap_or_else(|| format!("{}-sdk", naming::to_case(words, Case::Kebab)));
        let package = read(
            "package",
            &is_package,
            "a Kotlin package name such as io.github.acme.sdk",
        )
        .unwrap_or_else(|| format!("com.example.{api}"));
        let version = read("version", &is_version, "a version such as 1.2.3")
            .unwrap_or_else(|| "0.1.0".to_string());
        let runtime = read("runtime", &is_version, "a runtime version such as 0.1.0")
            .unwrap_or_else(|| DEFAULT_RUNTIME.to_string());
        let runtime_path = read("runtime_path", &is_path, "a non-empty relative path");
        let java_runtime_path = read("java_runtime_path", &is_path, "a non-empty relative path");
        (
            Options {
                group,
                artifact,
                package,
                version,
                runtime,
                runtime_path,
                java_runtime_path,
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
        Target::Kotlin,
        Role::Module,
    );
    if is_segment(&name) {
        name
    } else {
        "api".to_string()
    }
}

/// A package segment usable in Kotlin and in Java (the SDK's classes are
/// JVM classes): ASCII letters, digits and `_`, not starting with a digit,
/// neither a Kotlin hard keyword nor a Java keyword.
fn is_segment(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !naming::is_reserved(s, Target::Kotlin, Role::Module)
        && !naming::is_reserved(s, Target::Java, Role::Module)
}

/// A dotted Kotlin package name.
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

/// A path written into a Kotlin string literal of the build script: no
/// quote, backslash, `$` (a string template) or control character.
fn is_path(s: &str) -> bool {
    !s.trim().is_empty()
        && !s
            .chars()
            .any(|c| matches!(c, '"' | '\\' | '$') || c.is_control())
}
