// SPDX-License-Identifier: AGPL-3.0-only
//! Versioned declarations of built-in target source support. These describe
//! compiler behavior, not tested platforms or published package availability.
//! Semantic validation remains with each target; external emitters are never
//! executed to build this catalog.

use schemars::JsonSchema;
use serde::Serialize;

/// Component-owned schema declaration. An absent declaration is unknown,
/// rather than a claim that a target has no options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetOptionSchema {
    TypeScript,
}

impl TargetOptionSchema {
    pub fn json_schema(self) -> serde_json::Value {
        match self {
            Self::TypeScript => crate::target_options::typescript_json_schema(),
        }
    }
}

impl Serialize for TargetOptionSchema {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.json_schema().serialize(serializer)
    }
}

/// Increment for incompatible changes to the query contract.
pub const VERSION: u32 = 1;

/// A declaration does not establish qualification or artifact availability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceSupport {
    BuiltInEmitter,
    Reserved,
}

/// Local consumer build requirement, separate from compiler prerequisites.
/// Bounds are inclusive minimum and exclusive maximum major/minor pairs;
/// absent bounds require only an installed executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ToolRequirement {
    pub tool: &'static str,
    pub minimum: Option<[u32; 2]>,
    pub maximum_exclusive: Option<[u32; 2]>,
    /// A supported target option can select a different minimum.
    pub minimum_option: Option<&'static str>,
}

impl ToolRequirement {
    const fn new(tool: &'static str, minimum: Option<[u32; 2]>) -> Self {
        Self {
            tool,
            minimum,
            maximum_exclusive: None,
            minimum_option: None,
        }
    }

    /// Resolve only the already-supported Python language modes. Unknown
    /// option values do not relax the declared default; the emitter diagnoses them.
    pub fn for_options(mut self, options: &serde_json::Value) -> Self {
        if self.minimum_option == Some("python_version") {
            self.minimum = match options
                .get("python_version")
                .and_then(serde_json::Value::as_str)
            {
                Some("3.10") => Some([3, 10]),
                Some("3.11") => Some([3, 11]),
                _ => self.minimum,
            };
        }
        self
    }

    pub fn accepts(&self, version: [u32; 2]) -> bool {
        self.minimum.is_none_or(|min| version >= min)
            && self.maximum_exclusive.is_none_or(|max| version < max)
    }

    pub fn description(&self) -> String {
        let mut text = self.minimum.map_or_else(
            || "installed".into(),
            |[major, minor]| format!(">={major}.{minor}"),
        );
        if let Some([major, minor]) = self.maximum_exclusive {
            text.push_str(&format!(" <{major}.{minor}"));
        }
        text
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct TargetCapability {
    pub id: &'static str,
    pub aliases: &'static [&'static str],
    pub source_support: SourceSupport,
    pub runtime_family: Option<&'static str>,
    pub imported_operation_policy: bool,
    pub prerequisites: &'static [ToolRequirement],
    /// Supported raw values. The component retains its diagnostic/fallback policy.
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>")]
    pub options_schema: Option<TargetOptionSchema>,
    pub qualification: EvidenceState,
    pub artifact_availability: EvidenceState,
    pub platform_qualification: EvidenceState,
}

const JS: &[ToolRequirement] = &[
    ToolRequirement::new("node", Some([22, 0])),
    ToolRequirement::new("npm", None),
];
const DOTNET: &[ToolRequirement] = &[ToolRequirement::new("dotnet", Some([10, 0]))];

macro_rules! targets {
    ($(($id:literal, $aliases:expr, $source:ident, $family:expr, $policy:literal, $tools:expr $(, $schema:expr)?)),* $(,)?) => {
        /// Target ids accepted in a manifest; aliases are CLI-only.
        pub const KNOWN_TARGETS: &[&str] = &[$($id),*];
        pub const TARGETS: &[TargetCapability] = &[$(TargetCapability {
            id: $id, aliases: $aliases, source_support: SourceSupport::$source,
            runtime_family: $family, imported_operation_policy: $policy,
            prerequisites: $tools, qualification: EvidenceState::Unknown,
            options_schema: targets!(@schema $($schema)?),
            artifact_availability: EvidenceState::Unknown,
            platform_qualification: EvidenceState::Unknown,
        }),*];
    };
    (@schema $schema:expr) => { $schema };
    (@schema) => { None };
}

targets! {
    ("csharp", &["cs"], BuiltInEmitter, Some("dotnet"), false, DOTNET),
    ("dart", &[], BuiltInEmitter, Some("dart"), false, &[ToolRequirement {
        tool: "dart", minimum: Some([3, 6]), maximum_exclusive: Some([4, 0]), minimum_option: None,
    }]),
    ("docs", &[], BuiltInEmitter, None, true, &[]),
    ("fsharp", &["fs"], BuiltInEmitter, Some("dotnet"), false, DOTNET),
    ("java", &[], BuiltInEmitter, Some("jvm"), false, &[
        ToolRequirement::new("java", Some([17, 0])), ToolRequirement::new("mvn", None),
    ]),
    ("kotlin", &["kt"], BuiltInEmitter, Some("jvm"), false, &[
        ToolRequirement::new("java", Some([17, 0])), ToolRequirement::new("gradle", None),
    ]),
    ("mcp", &[], BuiltInEmitter, Some("typescript"), true, JS),
    ("mock", &[], Reserved, None, false, &[]),
    ("python", &["py"], BuiltInEmitter, Some("python"), true, &[ToolRequirement {
        tool: "python3", minimum: Some([3, 12]), maximum_exclusive: None, minimum_option: Some("python_version"),
    }]),
    ("ruby", &["rb"], BuiltInEmitter, Some("ruby"), false, &[
        ToolRequirement::new("ruby", Some([3, 3])), ToolRequirement::new("bundle", None),
    ]),
    ("rust", &["rs"], BuiltInEmitter, Some("rust"), true, &[
        ToolRequirement::new("cargo", Some([1, 88])), ToolRequirement::new("rustc", Some([1, 88])),
    ]),
    ("typescript", &["ts"], BuiltInEmitter, Some("typescript"), true, JS, Some(TargetOptionSchema::TypeScript)),
    ("webmcp", &[], BuiltInEmitter, Some("typescript"), true, JS),
}

/// Look up a built-in declaration without reading project input or running tools.
pub fn target(name: &str) -> Option<&'static TargetCapability> {
    TARGETS
        .iter()
        .find(|target| target.id == name || target.aliases.contains(&name))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct CapabilityCatalog {
    pub capability_version: u32,
    pub targets: Vec<TargetCapability>,
}

/// The independent query schema is also embedded in the CLI output schema.
pub fn json_schema() -> serde_json::Value {
    schemars::generate::SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<CapabilityCatalog>()
        .to_value()
}
