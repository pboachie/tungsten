// SPDX-License-Identifier: AGPL-3.0-only
//! Declared option fields of the built-in targets without a typed options
//! struct here. Each declaration mirrors the option table of the target's
//! emitter `options` module, which reads its fixed defaults from the
//! constants below; validators stay in the emitter, so a schema states the
//! accepted JSON type and default, not every semantic rule.

use std::collections::BTreeMap;

use serde_json::{Value, json};

/// Fixed defaults shared with the emitters.
pub const PACKAGE_VERSION: &str = "0.1.0";
pub const CSHARP_RUNTIME: &str = "0.1.3";
pub const FSHARP_RUNTIME: &str = "0.1.3";
pub const JAVA_RUNTIME: &str = "0.1.3";
pub const KOTLIN_RUNTIME: &str = "0.1.3";
pub const DART_RUNTIME_CONSTRAINT: &str = "^0.1.3";
pub const RUBY_RUNTIME_REQUIREMENT: &str = "~> 0.1.3";
/// `@elysra/tungsten-mcp` and `@elysra/tungsten-runtime` ranges of MCP and WebMCP packages.
pub const NPM_RUNTIME_RANGE: &str = "^0.1.3";

/// JSON type a field accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    String,
    Boolean,
    Object,
}

/// One declared `targets.<id>` option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionField {
    pub key: &'static str,
    pub kind: FieldType,
    pub description: &'static str,
    /// The fixed default, when it does not depend on the API.
    pub default: Option<&'static str>,
}

impl OptionField {
    /// Whether `value` has the declared JSON type. Semantic validation stays
    /// with the emitter.
    pub fn accepts(&self, value: &Value) -> bool {
        match self.kind {
            FieldType::String => value.is_string(),
            FieldType::Boolean => value.is_boolean(),
            FieldType::Object => value.is_object(),
        }
    }

    /// The fixed default as JSON, when there is one.
    pub fn default_value(&self) -> Option<Value> {
        self.default.map(|default| match self.kind {
            FieldType::Boolean => Value::Bool(default == "true"),
            _ => Value::String(default.into()),
        })
    }
}

/// A target's declared fields and the diagnostic reporting an invalid value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeclaredOptions {
    /// Warning code of an invalid value; the emitter then uses the default.
    pub diagnostic: Option<&'static str>,
    pub fields: &'static [OptionField],
}

impl DeclaredOptions {
    /// Supported-value schema; undeclared keys remain permitted, as the
    /// emitters ignore them.
    pub fn json_schema(&self) -> Value {
        let properties: BTreeMap<_, _> = self
            .fields
            .iter()
            .map(|field| {
                let mut schema = json!({
                    "type": match field.kind {
                        FieldType::String => "string",
                        FieldType::Boolean => "boolean",
                        FieldType::Object => "object",
                    },
                    "description": field.description,
                });
                if let Some(code) = self.diagnostic {
                    schema["x-tungsten-invalid"] =
                        format!("{code} warning and emitter default").into();
                }
                if let Some(default) = field.default_value() {
                    schema["default"] = default;
                }
                (field.key, schema)
            })
            .collect();
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": properties,
            "additionalProperties": true,
        })
    }
}

const fn s(key: &'static str, description: &'static str) -> OptionField {
    OptionField {
        key,
        kind: FieldType::String,
        description,
        default: None,
    }
}

const fn d(key: &'static str, description: &'static str, default: &'static str) -> OptionField {
    OptionField {
        key,
        kind: FieldType::String,
        description,
        default: Some(default),
    }
}

const VERSION: OptionField = d("version", "package version", PACKAGE_VERSION);

pub const CSHARP: DeclaredOptions = DeclaredOptions {
    diagnostic: Some("TG1020"),
    fields: &[
        s(
            "package_id",
            "NuGet package id and assembly name; default `<Api>.Sdk`",
        ),
        s("namespace", "root C# namespace; default the package id"),
        VERSION,
        d(
            "runtime",
            "version of the `Tungsten.Runtime` NuGet package",
            CSHARP_RUNTIME,
        ),
        s(
            "runtime_path",
            "`runtimes/csharp` of a tungsten checkout, relative to the output directory, referenced instead of the package",
        ),
    ],
};

pub const FSHARP: DeclaredOptions = DeclaredOptions {
    diagnostic: Some("TG1080"),
    fields: &[
        s(
            "package_id",
            "NuGet package id and assembly name; default `<Api>.Sdk`",
        ),
        s("namespace", "root F# namespace; default the package id"),
        VERSION,
        d(
            "runtime",
            "version of `Tungsten.Runtime.FSharp` and `Tungsten.Runtime`",
            FSHARP_RUNTIME,
        ),
        s(
            "runtime_path",
            "`runtimes/fsharp` of a tungsten checkout, relative to the output directory, referenced instead of the packages",
        ),
        s(
            "csharp_runtime_path",
            "`runtimes/csharp`, relative to the output directory; only with `runtime_path`; default `<runtime_path>/../csharp`",
        ),
    ],
};

pub const DART: DeclaredOptions = DeclaredOptions {
    diagnostic: Some("TG1070"),
    fields: &[
        s(
            "package",
            "pub package name (lowercase letters, digits and `_`); default `<api>_sdk`",
        ),
        VERSION,
        d(
            "runtime",
            "version constraint on the `tungsten_runtime` package",
            DART_RUNTIME_CONSTRAINT,
        ),
        s(
            "runtime_path",
            "a local `tungsten_runtime` checkout, written as a `path:` dependency",
        ),
    ],
};

pub const JAVA: DeclaredOptions = DeclaredOptions {
    diagnostic: Some("TG1010"),
    fields: &[
        d("group_id", "Maven groupId", "com.example"),
        s("artifact_id", "Maven artifactId; default `<api>-sdk`"),
        s("package", "Java package; default `com.example.<api>`"),
        VERSION,
        d(
            "runtime",
            "version of `io.github.pboachie.tungsten:tungsten-runtime`",
            JAVA_RUNTIME,
        ),
        s(
            "runtime_path",
            "`runtimes/java` of a tungsten checkout, relative to the output directory, compiled into the SDK",
        ),
    ],
};

pub const KOTLIN: DeclaredOptions = DeclaredOptions {
    diagnostic: Some("TG1030"),
    fields: &[
        d("group", "Gradle group (Maven groupId)", "com.example"),
        s(
            "artifact",
            "artifact name (`rootProject.name`); default `<api>-sdk`",
        ),
        s("package", "Kotlin package; default `com.example.<api>`"),
        VERSION,
        d(
            "runtime",
            "version of `tungsten-runtime-kotlin` and `tungsten-runtime`",
            KOTLIN_RUNTIME,
        ),
        s(
            "runtime_path",
            "`runtimes/kotlin` of a tungsten checkout, relative to the output directory, compiled into the SDK",
        ),
        s(
            "java_runtime_path",
            "`runtimes/java`, relative to the output directory, compiled into the SDK",
        ),
        OptionField {
            key: "coroutines",
            kind: FieldType::Boolean,
            description: "add `suspend` functions and cold `Flow`s next to the blocking surface",
            default: Some("false"),
        },
        s(
            "coroutines_runtime_path",
            "with `coroutines`: `runtimes/kotlin/coroutines`, relative to the output directory, compiled into the SDK",
        ),
    ],
};

pub const RUBY: DeclaredOptions = DeclaredOptions {
    diagnostic: Some("TG1060"),
    fields: &[
        s(
            "gem",
            "gem name (lowercase letters, digits, `-` and `_`); default `<api>-sdk`",
        ),
        s(
            "module",
            "the SDK's module, a Ruby constant path; default `<Api>Sdk`",
        ),
        VERSION,
        d(
            "runtime",
            "version requirement of the `tungsten-runtime` gem",
            RUBY_RUNTIME_REQUIREMENT,
        ),
        s(
            "runtime_path",
            "a local `tungsten-runtime` checkout, as the Gemfile's `path:` source",
        ),
    ],
};

pub const MCP: DeclaredOptions = DeclaredOptions {
    diagnostic: Some("TG0720"),
    fields: &[
        s(
            "package",
            "npm package name of the server; default `<api>-mcp`",
        ),
        VERSION,
        s(
            "sdk",
            "the generated TypeScript SDK, `<package>` or `<package>@<range>`; default from the `typescript` target",
        ),
        s(
            "sdk_path",
            "depend on the SDK by path (`file:<path>`) instead of a range",
        ),
        d(
            "runtime",
            "`@elysra/tungsten-mcp` version range",
            NPM_RUNTIME_RANGE,
        ),
        s("runtime_path", "depend on `@elysra/tungsten-mcp` by path"),
        d(
            "sdk_runtime",
            "direct `@elysra/tungsten-runtime` version range",
            NPM_RUNTIME_RANGE,
        ),
        s(
            "sdk_runtime_path",
            "use the same local client runtime as the linked SDK",
        ),
        OptionField {
            key: "search",
            kind: FieldType::Object,
            description: "`embeddings`: the optional embedding index of tool search (TG0570 to TG0574)",
            default: None,
        },
        OptionField {
            key: "sandbox",
            kind: FieldType::Boolean,
            description: "enable the opt-in `run_script` sandbox",
            default: Some("false"),
        },
    ],
};

pub const WEBMCP: DeclaredOptions = DeclaredOptions {
    diagnostic: Some("TG0790"),
    fields: &[
        s("package", "npm package name of the browser adapter"),
        VERSION,
        d(
            "runtime",
            "`@elysra/tungsten-webmcp` version range",
            NPM_RUNTIME_RANGE,
        ),
        s(
            "runtime_path",
            "depend on `@elysra/tungsten-webmcp` by path",
        ),
        s(
            "sdk",
            "the generated TypeScript SDK, `<package>` or `<package>@<range>`; default from the `typescript` target",
        ),
        s(
            "sdk_path",
            "depend on the SDK by path (`file:<path>`) instead of a range",
        ),
        d(
            "sdk_runtime",
            "the SDK's `@elysra/tungsten-runtime` version range",
            NPM_RUNTIME_RANGE,
        ),
        s(
            "sdk_runtime_path",
            "use the same local client runtime as the linked SDK",
        ),
    ],
};

/// The docs target reads no options besides `out`.
pub const DOCS: DeclaredOptions = DeclaredOptions {
    diagnostic: None,
    fields: &[],
};
