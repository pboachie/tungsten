// SPDX-License-Identifier: AGPL-3.0-only
//! Versioned declarations of built-in target source support. These describe
//! compiler behavior, not tested platforms or published package availability.
//! Semantic validation remains with each target; external emitters are never
//! executed to build this catalog.

use schemars::JsonSchema;

use crate::option_fields;
use serde::Serialize;

/// Component-owned schema declaration. An absent declaration is unknown,
/// rather than a claim that a target has no options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetOptionSchema {
    TypeScript,
    Python,
    Rust,
    /// Field declarations mirroring the emitter's options module; see
    /// [`crate::option_fields`]. Configuration inspection does not cover them.
    Declared(&'static crate::option_fields::DeclaredOptions),
}

impl TargetOptionSchema {
    pub fn json_schema(self) -> serde_json::Value {
        match self {
            Self::TypeScript => crate::target_options::typescript_json_schema(),
            Self::Python => crate::target_options::python_json_schema(),
            Self::Rust => crate::target_options::rust_json_schema(),
            Self::Declared(options) => options.json_schema(),
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
    /// At least one [`EvidenceRecord`] of this scope is listed. The state is
    /// derived from the records and can never be declared without them.
    Recorded,
}

/// What an [`EvidenceRecord`] supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceScope {
    /// A clean consumer built and exercised generated output.
    Qualification,
    /// A runtime artifact is published for the combination.
    Artifact,
    /// The combination ran on the named platform.
    Platform,
}

/// Evidence for one exact supported combination, linked to where it can be
/// checked. Records are added by the owner of the qualification, never inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct EvidenceRecord {
    pub scope: EvidenceScope,
    /// The exact combination, such as `python 3.10 / linux-x86_64 / runtime 0.1.3`.
    pub combination: &'static str,
    /// A public, stable reference to the evidence (workflow, release or report URL).
    pub reference: &'static str,
}

/// [`EvidenceState::Recorded`] exactly when a record of `scope` exists.
pub const fn evidence_state(records: &[EvidenceRecord], scope: EvidenceScope) -> EvidenceState {
    let mut i = 0;
    while i < records.len() {
        if records[i].scope as u8 == scope as u8 {
            return EvidenceState::Recorded;
        }
        i += 1;
    }
    EvidenceState::Unknown
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
    /// One-line description of what the target emits, used by the support matrix.
    pub summary: &'static str,
    /// Top-level `tungsten.yml` option keys the target reads. Editor completion
    /// derives from this list; `options_schema`, when declared, types them.
    pub option_keys: &'static [&'static str],
    /// Where generated output is designed to run. A declared environment is
    /// a source design statement, not platform qualification.
    pub execution_environments: &'static [ExecutionEnvironment],
    /// Features from [`FEATURES`] the target implements in source.
    pub features: &'static [&'static str],
    /// Evidence behind `qualification`, `artifact_availability` and
    /// `platform_qualification`; each is `recorded` only with a record here.
    pub evidence: &'static [EvidenceRecord],
}

/// Environment generated output is designed to run in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEnvironment {
    /// Node.js, at the target's `node` prerequisite.
    Node,
    /// A browser exposing the APIs the runtime requires.
    Browser,
    /// CPython.
    Cpython,
    /// A native binary built by Cargo.
    Native,
    /// A Java virtual machine.
    Jvm,
    /// The .NET runtime.
    Dotnet,
    /// The Dart VM or AOT with `dart:io`; not Dart web.
    DartIo,
    /// CRuby.
    Ruby,
    /// Static files read by people and agents; nothing executes.
    Static,
}

/// What part of the product a feature describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeatureDomain {
    /// Generated SDK or server surface.
    Target,
    /// Runtime behaviour shipped with generated output.
    Runtime,
    /// Wire protocol or transport exposed by generated output.
    Protocol,
}

/// A registered feature id. Components add a definition here and list the
/// id on each target implementing it; [`validate`] rejects unregistered ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct FeatureDefinition {
    pub id: &'static str,
    pub domain: FeatureDomain,
    pub description: &'static str,
    /// The target option that enables the feature, when it is opt-in.
    pub option: Option<&'static str>,
}

const fn def(
    id: &'static str,
    domain: FeatureDomain,
    description: &'static str,
    option: Option<&'static str>,
) -> FeatureDefinition {
    FeatureDefinition {
        id,
        domain,
        description,
        option,
    }
}

/// Registered features. Ids are stable; descriptions may be refined.
pub const FEATURES: &[FeatureDefinition] = &[
    def(
        "typed_sdk",
        FeatureDomain::Target,
        "typed models and operation methods for every compiled operation",
        None,
    ),
    def(
        "pagination_helpers",
        FeatureDomain::Target,
        "page iteration helpers for operations with declared pagination",
        None,
    ),
    def(
        "generated_cli",
        FeatureDomain::Target,
        "a command-line client crate next to the SDK",
        Some("cli"),
    ),
    def(
        "dataclass_models",
        FeatureDomain::Target,
        "stdlib dataclass models without a Pydantic dependency",
        Some("models"),
    ),
    def(
        "coroutines",
        FeatureDomain::Target,
        "suspend functions and cold Flows next to the blocking surface",
        Some("coroutines"),
    ),
    def(
        "mcp_stdio",
        FeatureDomain::Protocol,
        "Model Context Protocol server over stdio",
        None,
    ),
    def(
        "mcp_streamable_http",
        FeatureDomain::Protocol,
        "Model Context Protocol server over Streamable HTTP",
        None,
    ),
    def(
        "mcp_embedding_search",
        FeatureDomain::Target,
        "optional embedding index for tool search",
        Some("search"),
    ),
    def(
        "mcp_script_sandbox",
        FeatureDomain::Runtime,
        "opt-in run_script sandbox",
        Some("sandbox"),
    ),
    def(
        "webmcp_registration",
        FeatureDomain::Protocol,
        "browser tool manifest and WebMCP registration helper",
        None,
    ),
    def(
        "llms_txt",
        FeatureDomain::Target,
        "llms.txt, llms-full.txt and tools.json documentation",
        None,
    ),
];

/// Look up a registered feature.
pub fn feature_definition(id: &str) -> Option<&'static FeatureDefinition> {
    FEATURES.iter().find(|feature| feature.id == id)
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

/// Typed SDK features shared by every built-in SDK target.
const SDK: &[&str] = &["typed_sdk", "pagination_helpers"];

macro_rules! targets {
    ($({
        id: $id:literal, aliases: $aliases:expr, source: $source:ident, family: $family:expr,
        policy: $policy:literal, tools: $tools:expr, $(schema: $schema:expr,)?
        $(evidence: $evidence:expr,)?
        summary: $summary:literal, options: $options:expr, environments: [$($env:ident),*],
        features: $features:expr $(,)?
    }),* $(,)?) => {
        /// Target ids accepted in a manifest; aliases are CLI-only.
        pub const KNOWN_TARGETS: &[&str] = &[$($id),*];
        pub const TARGETS: &[TargetCapability] = &[$(TargetCapability {
            id: $id, aliases: $aliases, source_support: SourceSupport::$source,
            runtime_family: $family, imported_operation_policy: $policy,
            prerequisites: $tools,
            qualification: evidence_state(targets!(@evidence $($evidence)?), EvidenceScope::Qualification),
            options_schema: targets!(@schema $($schema)?),
            artifact_availability: evidence_state(targets!(@evidence $($evidence)?), EvidenceScope::Artifact),
            platform_qualification: evidence_state(targets!(@evidence $($evidence)?), EvidenceScope::Platform),
            evidence: targets!(@evidence $($evidence)?),
            summary: $summary, option_keys: $options,
            execution_environments: &[$(ExecutionEnvironment::$env),*],
            features: $features,
        }),*];
    };
    (@schema $schema:expr) => { $schema };
    (@schema) => { None };
    (@evidence $evidence:expr) => { $evidence };
    (@evidence) => { &[] };
}

targets! {
    {
        id: "csharp", aliases: &["cs"], source: BuiltInEmitter, family: Some("dotnet"),
        policy: true, tools: DOTNET,
        schema: Some(TargetOptionSchema::Declared(&option_fields::CSHARP)), summary: "C# SDK",
        options: &["package_id", "namespace", "version", "runtime", "runtime_path", "out"],
        environments: [Dotnet], features: SDK,
    },
    {
        id: "dart", aliases: &[], source: BuiltInEmitter, family: Some("dart"), policy: true,
        tools: &[ToolRequirement {
            tool: "dart", minimum: Some([3, 6]), maximum_exclusive: Some([4, 0]), minimum_option: None, excluded_versions: &[],
        }],
        schema: Some(TargetOptionSchema::Declared(&option_fields::DART)),
        summary: "Dart SDK (sealed unions, Future methods returning Result, Stream pages)",
        options: &["package", "version", "runtime", "runtime_path", "out"],
        environments: [DartIo], features: SDK,
    },
    {
        id: "docs", aliases: &[], source: BuiltInEmitter, family: None, policy: true, tools: &[],
        schema: Some(TargetOptionSchema::Declared(&option_fields::DOCS)),
        summary: "llms.txt, llms-full.txt, tools.json and README", options: &["out"],
        environments: [Static], features: &["llms_txt"],
    },
    {
        id: "fsharp", aliases: &["fs"], source: BuiltInEmitter, family: Some("dotnet"),
        policy: true, tools: DOTNET,
        schema: Some(TargetOptionSchema::Declared(&option_fields::FSHARP)),
        summary: "F# SDK (records, discriminated unions, Task of Result)",
        options: &["package_id", "namespace", "version", "runtime", "runtime_path", "csharp_runtime_path", "out"],
        environments: [Dotnet], features: SDK,
    },
    {
        id: "java", aliases: &[], source: BuiltInEmitter, family: Some("jvm"), policy: true,
        tools: &[ToolRequirement::new("java", Some([17, 0])), ToolRequirement::new("mvn", None)],
        schema: Some(TargetOptionSchema::Declared(&option_fields::JAVA)),
        summary: "Java SDK",
        options: &["group_id", "artifact_id", "package", "version", "runtime", "runtime_path", "out"],
        environments: [Jvm], features: SDK,
    },
    {
        id: "kotlin", aliases: &["kt"], source: BuiltInEmitter, family: Some("jvm"), policy: true,
        tools: &[ToolRequirement::new("java", Some([17, 0])), ToolRequirement::new("gradle", None)],
        schema: Some(TargetOptionSchema::Declared(&option_fields::KOTLIN)),
        summary: "Kotlin/JVM SDK (blocking functions returning Result; opt-in coroutines)",
        options: &[
            "group", "artifact", "package", "version", "runtime", "runtime_path",
            "java_runtime_path", "coroutines", "coroutines_runtime_path", "out",
        ],
        environments: [Jvm], features: &["typed_sdk", "pagination_helpers", "coroutines"],
    },
    {
        id: "mcp", aliases: &[], source: BuiltInEmitter, family: Some("typescript"), policy: true,
        tools: MCP,
        schema: Some(TargetOptionSchema::Declared(&option_fields::MCP)),
        summary: "MCP server over stdio or Streamable HTTP, discrete or progressive",
        options: &[
            "package", "version", "sdk", "sdk_path", "runtime", "runtime_path", "sdk_runtime",
            "sdk_runtime_path", "search", "sandbox", "out",
        ],
        environments: [Node],
        features: &[
            "mcp_stdio", "mcp_streamable_http", "mcp_embedding_search", "mcp_script_sandbox",
        ],
    },
    {
        id: "mock", aliases: &[], source: Reserved, family: None, policy: false, tools: &[],
        summary: "reserved; no emitter in this version", options: &["out"],
        environments: [], features: &[],
    },
    {
        id: "python", aliases: &["py"], source: BuiltInEmitter, family: Some("python"), policy: true,
        tools: &[ToolRequirement {
            tool: "python3", minimum: Some([3, 12]), maximum_exclusive: None, minimum_option: Some("python_version"), excluded_versions: &[],
        }], schema: Some(TargetOptionSchema::Python),
        summary: "Sync and async Python SDK, Pydantic v2 or dataclass models",
        options: &["package", "module", "version", "runtime", "runtime_path", "python_version", "models", "out"],
        environments: [Cpython],
        features: &["typed_sdk", "pagination_helpers", "dataclass_models"],
    },
    {
        id: "ruby", aliases: &["rb"], source: BuiltInEmitter, family: Some("ruby"), policy: true,
        tools: &[ToolRequirement::new("ruby", Some([3, 3])), ToolRequirement::new("bundle", None)],
        schema: Some(TargetOptionSchema::Declared(&option_fields::RUBY)),
        summary: "Ruby SDK",
        options: &["gem", "module", "version", "runtime", "runtime_path", "out"],
        environments: [Ruby], features: SDK,
    },
    {
        id: "rust", aliases: &["rs"], source: BuiltInEmitter, family: Some("rust"), policy: true,
        tools: &[ToolRequirement::new("cargo", Some([1, 88])), ToolRequirement::new("rustc", Some([1, 88]))],
        schema: Some(TargetOptionSchema::Rust),
        summary: "Async Rust SDK (serde, reqwest) and optional generated CLI",
        options: &["crate", "version", "runtime", "runtime_path", "cli", "out"],
        environments: [Native],
        features: &["typed_sdk", "pagination_helpers", "generated_cli"],
    },
    {
        id: "typescript", aliases: &["ts"], source: BuiltInEmitter, family: Some("typescript"),
        policy: true, tools: JS, schema: Some(TargetOptionSchema::TypeScript),
        summary: "Typed TypeScript SDK, Result values, Zod validation",
        options: &["package", "version", "runtime", "runtime_path", "dependencies", "out"],
        environments: [Node], features: SDK,
    },
    {
        id: "webmcp", aliases: &[], source: BuiltInEmitter, family: Some("typescript"), policy: true,
        tools: JS,
        schema: Some(TargetOptionSchema::Declared(&option_fields::WEBMCP)),
        summary: "Browser ESM package with tool manifest and registration helper",
        options: &[
            "package", "version", "runtime", "runtime_path", "sdk", "sdk_path", "sdk_runtime",
            "sdk_runtime_path", "out",
        ],
        environments: [Browser],
        features: &["webmcp_registration"],
    },
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
    /// Registered feature ids that target `features` lists refer to.
    pub features: Vec<FeatureDefinition>,
    pub openapi_inputs: Vec<OpenApiVersionCapability>,
    /// Every input format, native or converted; `openapi_inputs` details the
    /// native OpenAPI lines.
    pub input_formats: Vec<InputFormatCapability>,
    pub execution_policy: ExecutionPolicyCapability,
}

/// How an input format enters a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InputRoute {
    /// Read by the compiler on every run (`inputs[].spec`, compiler paths).
    Compiler,
    /// Converted once by `tungsten init` into a saved OpenAPI document.
    InitConversion,
    /// Inferred by `tungsten init` from flags and examples, without a document.
    InitInference,
}

/// Where `tungsten init --from` can acquire the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InputAcquisition {
    Local,
    Https,
    Git,
}

/// Source declaration for one input format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct InputFormatCapability {
    pub id: &'static str,
    /// Document versions read; empty when the format is unversioned.
    pub versions: &'static [&'static str],
    pub route: InputRoute,
    /// How the format is recognized.
    pub detection: &'static str,
    /// Operations are marked imported, so only targets declaring
    /// `imported_operation_policy` may be generated from them.
    pub imported_operations: bool,
    /// CLI flags whose reviewed values the conversion requires.
    pub review_flags: &'static [&'static str],
    pub acquisition: &'static [InputAcquisition],
    /// The saved document a conversion or inference writes.
    pub saved_as: Option<&'static str>,
}

const DOCUMENT: &[InputAcquisition] = &[
    InputAcquisition::Local,
    InputAcquisition::Https,
    InputAcquisition::Git,
];

const fn converted(
    id: &'static str,
    versions: &'static [&'static str],
    detection: &'static str,
    review_flags: &'static [&'static str],
) -> InputFormatCapability {
    InputFormatCapability {
        id,
        versions,
        route: InputRoute::InitConversion,
        detection,
        imported_operations: true,
        review_flags,
        acquisition: DOCUMENT,
        saved_as: Some("imported.openapi.json"),
    }
}

/// Input formats `tungsten init` and the compiler accept, one input per project.
pub const INPUT_FORMATS: &[InputFormatCapability] = &[
    InputFormatCapability {
        id: "openapi",
        versions: &["3.0", "3.1", "3.2"],
        route: InputRoute::Compiler,
        detection: "`openapi` member",
        imported_operations: false,
        review_flags: &[],
        acquisition: DOCUMENT,
        saved_as: None,
    },
    converted("swagger", &["2.0"], "`swagger` member", &[]),
    converted("postman", &["2.1"], "`info` and `item` members", &[]),
    converted(
        "curl",
        &[],
        "a leading `curl`, a `.curl` extension or `--input-format curl`",
        &["--route"],
    ),
    converted(
        "http",
        &[],
        "a `.http` or `.rest` extension or `--input-format http`",
        &["--route"],
    ),
    converted("har", &["1.2"], "`log` member", &["--har-paths"]),
    converted(
        "endpoint_manifest",
        &["1"],
        "`endpoint_manifest` member",
        &[],
    ),
    InputFormatCapability {
        id: "manual_endpoint",
        versions: &[],
        route: InputRoute::InitInference,
        detection: "`--method` and `--endpoint`, without `--from`",
        imported_operations: true,
        review_flags: &[],
        acquisition: &[],
        saved_as: Some("inferred.openapi.json"),
    },
];

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

/// One construct of a version line that has no 3.1 equivalent and is
/// compiled with its own semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SupportedInputFeature {
    pub id: &'static str,
    /// The document member that introduces the construct.
    pub member: &'static str,
    /// What the compiled client does with it.
    pub behavior: &'static str,
}

/// Source declaration for one `openapi` version line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct OpenApiVersionCapability {
    /// Major.minor line; every patch version of it is accepted.
    pub line: &'static str,
    pub support: OpenApiSupport,
    /// Constructs new in this line that are compiled.
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub supported_features: &'static [SupportedInputFeature],
    pub unsupported_features: &'static [UnsupportedInputFeature],
}

use InputFeaturePolicy::{Ignored, Rejected};

/// OpenAPI 3.2 constructs without a 3.1 equivalent that are compiled.
pub const OPENAPI_32_SUPPORTED: &[SupportedInputFeature] = &[
    SupportedInputFeature {
        id: "query_method",
        member: "query",
        behavior: "an operation sent with the HTTP QUERY method and its request body; safe, so read-only by default",
    },
    SupportedInputFeature {
        id: "additional_operations",
        member: "additionalOperations",
        behavior: "each entry is an operation sent with its method token exactly as written; unsafe and not idempotent by default. Tokens that cannot be sent (not an RFC 9110 token, a fixed method, CONNECT or TRACK) are refused with TG0142",
    },
    SupportedInputFeature {
        id: "reusable_media_types",
        member: "components.mediaTypes, $ref media type",
        behavior: "a media type object given by $ref is read as if written in place",
    },
    SupportedInputFeature {
        id: "item_schema_json_lines",
        member: "itemSchema (JSON Lines response)",
        behavior: "the type of one line of an application/jsonl or application/x-ndjson response",
    },
    SupportedInputFeature {
        id: "item_schema_event_stream",
        member: "itemSchema (text/event-stream response, JSON `data`)",
        behavior: "the event type of a server-sent event stream: the `contentSchema` of the event's `data`",
    },
    SupportedInputFeature {
        id: "discriminator_default_mapping",
        member: "discriminator.defaultMapping",
        behavior: "a tagged union decodes a missing or unmapped tag as the default variant",
    },
    SupportedInputFeature {
        id: "security_scheme_by_uri",
        member: "security requirement naming a URI",
        behavior: "the scheme under components.securitySchemes of an input document the URI references; any other URI is TG0151",
    },
    SupportedInputFeature {
        id: "oauth2_device_authorization",
        member: "flows.deviceAuthorization",
        behavior: "an OAuth2 flow kept with its deviceAuthorizationUrl; SDKs accept an access token obtained through it and do not run the device flow",
    },
];

/// OpenAPI 3.2 constructs without a 3.1 equivalent that are diagnosed. A 3.2
/// document using none of these is read as its 3.1 twin plus the supported
/// constructs above.
pub const OPENAPI_32_FEATURES: &[UnsupportedInputFeature] = &[
    feature("document_self", "$self", "TG0140", Rejected),
    feature(
        "querystring_parameter",
        "in: querystring",
        "TG0143",
        Rejected,
    ),
    feature("cookie_style", "style: cookie", "TG0144", Rejected),
    feature("item_schema", "itemSchema (other uses)", "TG0145", Rejected),
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
        supported_features: &[],
        unsupported_features: &[],
    },
    OpenApiVersionCapability {
        line: "3.1",
        support: OpenApiSupport::Supported,
        supported_features: &[],
        unsupported_features: &[],
    },
    OpenApiVersionCapability {
        line: "3.2",
        support: OpenApiSupport::Subset,
        supported_features: OPENAPI_32_SUPPORTED,
        unsupported_features: OPENAPI_32_FEATURES,
    },
];

/// Check the built-in declarations for conflicts: duplicate ids or aliases,
/// unregistered features, empty or inverted version ranges, and option
/// references the target does not declare. Returns one message per conflict;
/// an empty list means the catalog is consistent.
pub fn validate() -> Vec<String> {
    let mut errors = validate_parts(TARGETS, FEATURES, OPENAPI_INPUTS);
    errors.extend(validate_inputs(INPUT_FORMATS, OPENAPI_INPUTS));
    errors
}

/// Input-format conflicts: duplicate ids, and native OpenAPI lines that
/// disagree with the per-line declarations.
pub fn validate_inputs(
    formats: &[InputFormatCapability],
    openapi: &[OpenApiVersionCapability],
) -> Vec<String> {
    let mut errors = vec![];
    let mut ids = std::collections::BTreeSet::new();
    for format in formats {
        if !ids.insert(format.id) {
            errors.push(format!("input format `{}` is declared twice", format.id));
        }
        if format.route == InputRoute::Compiler && format.imported_operations {
            errors.push(format!(
                "input format `{}` is compiled natively but marks operations imported",
                format.id
            ));
        }
        if format.route != InputRoute::Compiler && !format.imported_operations {
            errors.push(format!(
                "input format `{}` is converted but does not mark operations imported",
                format.id
            ));
        }
    }
    if let Some(native) = formats.iter().find(|format| format.id == "openapi") {
        let lines: Vec<&str> = openapi.iter().map(|input| input.line).collect();
        if native.versions != lines.as_slice() {
            errors.push(
                "input format `openapi` versions disagree with the OpenAPI line declarations"
                    .into(),
            );
        }
    }
    errors
}

/// [`validate`] over explicit declarations, so a component can check a
/// catalog it extends before registering it.
pub fn validate_parts(
    targets: &[TargetCapability],
    features: &[FeatureDefinition],
    inputs: &[OpenApiVersionCapability],
) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut errors = vec![];
    let mut names = BTreeSet::new();
    for target in targets {
        for name in std::iter::once(&target.id).chain(target.aliases) {
            if !names.insert(*name) {
                errors.push(format!(
                    "target name `{name}` is declared more than once (target `{}`)",
                    target.id
                ));
            }
        }
    }
    let mut feature_ids = BTreeSet::new();
    for feature in features {
        if !feature_ids.insert(feature.id) {
            errors.push(format!("feature `{}` is registered twice", feature.id));
        }
    }
    for target in targets {
        let id = target.id;
        let keys: BTreeSet<_> = target.option_keys.iter().copied().collect();
        if keys.len() != target.option_keys.len() {
            errors.push(format!("target `{id}` lists an option key twice"));
        }
        let mut seen = BTreeSet::new();
        for name in target.features {
            if !seen.insert(*name) {
                errors.push(format!("target `{id}` lists feature `{name}` twice"));
            }
            match features.iter().find(|feature| feature.id == *name) {
                None => errors.push(format!(
                    "target `{id}` lists unregistered feature `{name}`"
                )),
                Some(FeatureDefinition {
                    option: Some(option),
                    ..
                }) if !keys.contains(option) => errors.push(format!(
                    "target `{id}` lists feature `{name}`, enabled by option `{option}` it does not declare"
                )),
                Some(_) => {}
            }
        }
        if target.source_support == SourceSupport::Reserved
            && (!target.prerequisites.is_empty()
                || !target.features.is_empty()
                || !target.execution_environments.is_empty()
                || target.imported_operation_policy)
        {
            errors.push(format!(
                "reserved target `{id}` declares support it has no emitter for"
            ));
        }
        if let Some(schema) = target.options_schema {
            let schema = schema.json_schema();
            let properties = schema
                .get("properties")
                .and_then(serde_json::Value::as_object);
            for property in properties.into_iter().flat_map(|map| map.keys()) {
                if !keys.contains(property.as_str()) {
                    errors.push(format!(
                        "target `{id}`: option schema property `{property}` is not a declared option key"
                    ));
                }
            }
            // Field declarations are complete: every key but `out` is typed.
            if let Some(TargetOptionSchema::Declared(options)) = target.options_schema {
                for key in target.option_keys {
                    if *key != "out" && !options.fields.iter().any(|field| field.key == *key) {
                        errors.push(format!(
                            "target `{id}`: option key `{key}` has no declared field"
                        ));
                    }
                }
            }
        }
        for record in target.evidence {
            if record.combination.trim().is_empty() || record.reference.trim().is_empty() {
                errors.push(format!(
                    "target `{id}` lists evidence without a combination or reference"
                ));
            }
        }
        for (state, scope) in [
            (target.qualification, EvidenceScope::Qualification),
            (target.artifact_availability, EvidenceScope::Artifact),
            (target.platform_qualification, EvidenceScope::Platform),
        ] {
            if state != evidence_state(target.evidence, scope) {
                errors.push(format!(
                    "target `{id}` declares {scope:?} evidence state {state:?} without matching records"
                ));
            }
        }
        for requirement in target.prerequisites {
            let tool = requirement.tool;
            if let Some(option) = requirement.minimum_option
                && !keys.contains(option)
            {
                errors.push(format!(
                    "target `{id}`: `{tool}` minimum depends on undeclared option `{option}`"
                ));
            }
            if let (Some(min), Some(max)) = (requirement.minimum, requirement.maximum_exclusive)
                && min >= max
            {
                errors.push(format!("target `{id}`: `{tool}` version range is empty"));
            }
            for excluded in requirement.excluded_versions {
                if requirement.minimum.is_some_and(|min| *excluded < min)
                    || requirement
                        .maximum_exclusive
                        .is_some_and(|max| *excluded >= max)
                {
                    errors.push(format!(
                        "target `{id}`: `{tool}` excludes {}.{}, outside its range",
                        excluded[0], excluded[1]
                    ));
                }
            }
        }
    }
    let mut lines = BTreeSet::new();
    let mut diagnostics = BTreeSet::new();
    for input in inputs {
        if !lines.insert(input.line) {
            errors.push(format!("OpenAPI line {} is declared twice", input.line));
        }
        if (input.support == OpenApiSupport::Supported) != input.unsupported_features.is_empty() {
            errors.push(format!(
                "OpenAPI line {}: support level disagrees with its unsupported-feature list",
                input.line
            ));
        }
        for feature in input.unsupported_features {
            if !diagnostics.insert(feature.diagnostic) {
                errors.push(format!(
                    "OpenAPI diagnostic {} is declared for more than one construct",
                    feature.diagnostic
                ));
            }
        }
    }
    errors
}
