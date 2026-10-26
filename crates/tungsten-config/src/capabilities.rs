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
    Python,
    Rust,
}

impl TargetOptionSchema {
    pub fn json_schema(self) -> serde_json::Value {
        match self {
            Self::TypeScript => crate::target_options::typescript_json_schema(),
            Self::Python => crate::target_options::python_json_schema(),
            Self::Rust => crate::target_options::rust_json_schema(),
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
    /// Major/minor lines lacking a required API, even above the minimum.
    #[serde(skip_serializing_if = "<[[u32; 2]]>::is_empty")]
    pub excluded_versions: &'static [[u32; 2]],
}

impl ToolRequirement {
    const fn new(tool: &'static str, minimum: Option<[u32; 2]>) -> Self {
        Self {
            tool,
            minimum,
            maximum_exclusive: None,
            minimum_option: None,
            excluded_versions: &[],
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
            && !self.excluded_versions.contains(&version)
    }

    pub fn description(&self) -> String {
        self.description_with_exclusions(&[])
    }

    /// Describe additional exclusions introduced by intersecting target requirements.
    pub fn description_with_exclusions(&self, additional: &[[u32; 2]]) -> String {
        let mut text = self.minimum.map_or_else(
            || "installed".into(),
            |[major, minor]| format!(">={major}.{minor}"),
        );
        if let Some([major, minor]) = self.maximum_exclusive {
            text.push_str(&format!(" <{major}.{minor}"));
        }
        let excluded: std::collections::BTreeSet<_> =
            self.excluded_versions.iter().chain(additional).collect();
        if !excluded.is_empty() {
            text.push_str(" excluding ");
            text.push_str(
                &excluded
                    .into_iter()
                    .map(|[major, minor]| format!("{major}.{minor}.x"))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
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
// The MCP root graph imports stripTypeScriptTypes, added in 22.13 and 23.2.
const MCP: &[ToolRequirement] = &[
    ToolRequirement {
        tool: "node",
        minimum: Some([22, 13]),
        maximum_exclusive: None,
        minimum_option: None,
        excluded_versions: &[[23, 0], [23, 1]],
    },
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
        tool: "dart", minimum: Some([3, 6]), maximum_exclusive: Some([4, 0]), minimum_option: None, excluded_versions: &[],
    }]),
    ("docs", &[], BuiltInEmitter, None, true, &[]),
    ("fsharp", &["fs"], BuiltInEmitter, Some("dotnet"), false, DOTNET),
    ("java", &[], BuiltInEmitter, Some("jvm"), false, &[
        ToolRequirement::new("java", Some([17, 0])), ToolRequirement::new("mvn", None),
    ]),
    ("kotlin", &["kt"], BuiltInEmitter, Some("jvm"), false, &[
        ToolRequirement::new("java", Some([17, 0])), ToolRequirement::new("gradle", None),
    ]),
    ("mcp", &[], BuiltInEmitter, Some("typescript"), true, MCP),
    ("mock", &[], Reserved, None, false, &[]),
    ("python", &["py"], BuiltInEmitter, Some("python"), true, &[ToolRequirement {
        tool: "python3", minimum: Some([3, 12]), maximum_exclusive: None, minimum_option: Some("python_version"), excluded_versions: &[],
    }], Some(TargetOptionSchema::Python)),
    ("ruby", &["rb"], BuiltInEmitter, Some("ruby"), false, &[
        ToolRequirement::new("ruby", Some([3, 3])), ToolRequirement::new("bundle", None),
    ]),
    ("rust", &["rs"], BuiltInEmitter, Some("rust"), true, &[
        ToolRequirement::new("cargo", Some([1, 88])), ToolRequirement::new("rustc", Some([1, 88])),
    ], Some(TargetOptionSchema::Rust)),
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
    pub openapi_inputs: Vec<OpenApiVersionCapability>,
    pub execution_policy: ExecutionPolicyCapability,
}

/// Source contract only; this is neither a policy selection nor approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ExecutionPolicyCapability {
    pub policy_version: u32,
    pub document_schema: String,
    pub enforcing_commands: Vec<String>,
    pub inspection_commands: Vec<String>,
    pub digest_command: String,
    pub unlisted_commands: String,
    pub max_document_bytes: usize,
    pub max_documents: usize,
    pub max_capture_bytes: usize,
}

pub fn execution_policy() -> ExecutionPolicyCapability {
    ExecutionPolicyCapability {
        policy_version: 1,
        document_schema: "execution-policy".into(),
        enforcing_commands: ["generate", "check", "report", "diff", "emitters"]
            .map(String::from)
            .to_vec(),
        inspection_commands: vec!["config".into()],
        digest_command: "policy digest".into(),
        unlisted_commands: "reject supplied execution policy before effects".into(),
        max_document_bytes: tungsten_core::policy::MAX_DOCUMENT_BYTES,
        max_documents: tungsten_core::policy::MAX_DOCUMENTS,
        max_capture_bytes: tungsten_core::policy::MAX_CAPTURE_BYTES,
    }
}

/// The independent query schema is also embedded in the CLI output schema.
pub fn json_schema() -> serde_json::Value {
    schemars::generate::SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<CapabilityCatalog>()
        .to_value()
}

/// How far the native frontend supports one OpenAPI version line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpenApiSupport {
    /// The whole line is read.
    Supported,
    /// Constructs equivalent to 3.1 are read; each listed construct is
    /// diagnosed instead of being ignored.
    Subset,
}

/// What the frontend does with a construct it does not compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InputFeaturePolicy {
    /// An error diagnostic; no artifact is produced from the document.
    Rejected,
    /// A warning diagnostic; the construct has no effect on the output.
    Ignored,
}

/// One construct of a version line that is diagnosed, never silently dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct UnsupportedInputFeature {
    pub id: &'static str,
    /// The document member that introduces the construct.
    pub member: &'static str,
    /// Stable diagnostic code reported at that member.
    pub diagnostic: &'static str,
    pub policy: InputFeaturePolicy,
}

const fn feature(
    id: &'static str,
    member: &'static str,
    diagnostic: &'static str,
    policy: InputFeaturePolicy,
) -> UnsupportedInputFeature {
    UnsupportedInputFeature {
        id,
        member,
        diagnostic,
        policy,
    }
}

/// Source declaration for one `openapi` version line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct OpenApiVersionCapability {
    /// Major.minor line; every patch version of it is accepted.
    pub line: &'static str,
    pub support: OpenApiSupport,
    pub unsupported_features: &'static [UnsupportedInputFeature],
}

use InputFeaturePolicy::{Ignored, Rejected};

/// OpenAPI 3.2 constructs without a 3.1 equivalent. A 3.2 document using none
/// of them is read exactly as its 3.1 twin.
pub const OPENAPI_32_FEATURES: &[UnsupportedInputFeature] = &[
    feature("document_self", "$self", "TG0140", Rejected),
    feature("query_method", "query", "TG0141", Rejected),
    feature(
        "additional_operations",
        "additionalOperations",
        "TG0142",
        Rejected,
    ),
    feature(
        "querystring_parameter",
        "in: querystring",
        "TG0143",
        Rejected,
    ),
    feature("cookie_style", "style: cookie", "TG0144", Rejected),
    feature("item_schema", "itemSchema", "TG0145", Rejected),
    feature(
        "positional_multipart_encoding",
        "prefixEncoding, itemEncoding",
        "TG0146",
        Rejected,
    ),
    feature(
        "nested_encoding",
        "encoding, prefixEncoding, itemEncoding (Encoding Object)",
        "TG0147",
        Rejected,
    ),
    feature(
        "reusable_media_types",
        "components.mediaTypes, $ref media type",
        "TG0148",
        Rejected,
    ),
    feature(
        "discriminator_default_mapping",
        "discriminator.defaultMapping",
        "TG0149",
        Rejected,
    ),
    feature(
        "oauth2_device_authorization",
        "flows.deviceAuthorization",
        "TG0150",
        Rejected,
    ),
    feature(
        "security_scheme_by_uri",
        "security requirement naming a URI",
        "TG0151",
        Rejected,
    ),
    feature(
        "oauth2_metadata_url",
        "oauth2MetadataUrl",
        "TG0152",
        Ignored,
    ),
    feature("tag_hierarchy", "tag parent, kind", "TG0153", Ignored),
    feature(
        "example_data_value",
        "dataValue, serializedValue",
        "TG0154",
        Ignored,
    ),
    feature("xml_node_type", "xml.nodeType", "TG0155", Ignored),
    feature(
        "descriptive_fields",
        "tag summary, response summary, server name, security scheme deprecated",
        "TG0156",
        Ignored,
    ),
];

/// OpenAPI input versions the native frontend reads.
pub const OPENAPI_INPUTS: &[OpenApiVersionCapability] = &[
    OpenApiVersionCapability {
        line: "3.0",
        support: OpenApiSupport::Supported,
        unsupported_features: &[],
    },
    OpenApiVersionCapability {
        line: "3.1",
        support: OpenApiSupport::Supported,
        unsupported_features: &[],
    },
    OpenApiVersionCapability {
        line: "3.2",
        support: OpenApiSupport::Subset,
        unsupported_features: OPENAPI_32_FEATURES,
    },
];
