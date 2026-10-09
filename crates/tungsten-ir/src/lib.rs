// SPDX-License-Identifier: AGPL-3.0-only
//! The tungsten intermediate representation (planning/03).
//!
//! The IR is the only input emitters see. Every collection is ordered
//! deterministically (sorted ids or source order), so serializing the same
//! IR twice yields identical bytes. `ir_version` follows the stability
//! policy in planning/03.

pub mod agent;
pub mod ident;
pub mod naming;
pub mod types;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tungsten_core::{Diagnostic, Digest};

pub use agent::*;
pub use ident::Ident;
pub use types::*;

/// Current IR version. Minor bumps are additive.
pub const IR_VERSION: &str = "1.0.0";

macro_rules! ir_struct {
    ($(#[$m:meta])* pub struct $name:ident { $($body:tt)* }) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
        pub struct $name { $($body)* }
    };
}
pub(crate) use ir_struct;

ir_struct! {
    /// The whole compiled API.
    pub struct Ir {
        pub ir_version: String,
        pub generator: GeneratorStamp,
        pub api: ApiInfo,
        pub namespaces: Vec<Namespace>,
        pub types: TypeTable,
        pub auth: Vec<AuthScheme>,
        pub errors: ErrorModel,
        #[serde(default)]
        pub agent: AgentModel,
        #[serde(default)]
        pub diagnostics: Vec<Diagnostic>,
    }
}

ir_struct! {
    pub struct GeneratorStamp {
        /// tungsten version that produced the IR.
        pub tungsten_version: String,
        /// Digest of every input (spec files, overlays, manifests), keyed by
        /// the path as given. Sorted.
        pub inputs: BTreeMap<String, Digest>,
    }
}

ir_struct! {
    pub struct ApiInfo {
        /// Machine name from tungsten.yml `api.name` (for example `zrotext`).
        pub name: Ident,
        pub title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        /// The first input document's `info.version`.
        pub version: String,
        #[serde(default)]
        pub servers: Vec<Server>,
    }
}

ir_struct! {
    pub struct Server {
        pub url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
    }
}

ir_struct! {
    /// Where an IR element came from: file plus JSON Pointer.
    pub struct SourceRef {
        pub file: String,
        pub pointer: String,
    }
}

ir_struct! {
    pub struct Doc {
        /// Summary or first sentence, trimmed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub summary: Option<String>,
        /// Full description as written in the spec.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
    }
}

ir_struct! {
    /// One input document.
    pub struct Namespace {
        pub name: Ident,
        pub source: SourceRef,
        pub digest: Digest,
        /// Title and version of the source document.
        pub title: String,
        pub version: String,
        pub resources: Vec<Resource>,
        /// Operations excluded by an `include` predicate but kept for docs
        /// (`planned_from`). Never callable.
        #[serde(default)]
        pub planned: Vec<Operation>,
        /// The error model of this document's callable operations: how to
        /// decode their error bodies.
        #[serde(default)]
        pub errors: ErrorModel,
    }
}

ir_struct! {
    pub struct Resource {
        pub name: Ident,
        /// Longest common literal path prefix of the resource's operations.
        pub path_prefix: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub doc: Option<Doc>,
        pub operations: Vec<Operation>,
        #[serde(default)]
        pub children: Vec<Resource>,
    }
}

/// Stable operation id: `namespace.operationId`, or for rpc-unflattened
/// methods `namespace.<method without prefix>` (for example
/// `workflow.action.send`).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(transparent)]
pub struct OperationId(pub String);

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Put,
    Post,
    Delete,
    Options,
    Head,
    Patch,
    Trace,
}

impl HttpMethod {
    pub fn is_safe(self) -> bool {
        matches!(
            self,
            HttpMethod::Get | HttpMethod::Head | HttpMethod::Options | HttpMethod::Trace
        )
    }
}

ir_struct! {
    pub struct Operation {
        pub id: OperationId,
        /// Method name inside its resource (`replay`).
        pub name: Ident,
        /// The spec's operationId, when present.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub operation_id: Option<String>,
        pub method: HttpMethod,
        pub path: PathTemplate,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub doc: Option<Doc>,
        #[serde(default)]
        pub tags: Vec<String>,
        pub params: ParamSet,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub body: Option<Body>,
        /// Ordered: exact statuses ascending, then ranges, then default.
        pub responses: Vec<Response>,
        /// OR of AND-sets. Empty means no auth.
        #[serde(default)]
        pub security: Vec<SecurityRequirement>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pagination: Option<Pagination>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub streaming: Option<Streaming>,
        #[serde(default)]
        pub deprecated: bool,
        pub status: OperationStatus,
        /// For rpc-unflattened operations: the discriminator value to send.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub rpc: Option<RpcBinding>,
        /// Every `x-*` key of the operation object, verbatim, sorted.
        #[serde(default)]
        pub extensions: BTreeMap<String, serde_json::Value>,
        #[serde(default)]
        pub agent: OperationAgentMeta,
        pub source: SourceRef,
    }
}

ir_struct! {
    pub struct PathTemplate {
        /// As written in the spec (`/v1/webhooks/{endpoint_id}/rotate`).
        pub raw: String,
        pub segments: Vec<PathSegment>,
        /// Constant query parameters written in the spec's path key
        /// (`/v1/messages?beta=true`). They are part of `raw`, so the
        /// runtime sends them on every call; they are not arguments.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub query: Vec<ConstQuery>,
    }
}

ir_struct! {
    /// One `name=value` pair of the query string of a path key.
    pub struct ConstQuery {
        pub name: String,
        pub value: String,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PathSegment {
    /// Literal text between two `/`.
    Literal { value: String },
    /// A segment that is exactly one `{name}`.
    Param { name: String },
    /// A segment mixing literal text and placeholders (`{date}.csv`,
    /// `{id}:archive`): its parts in order, each a `Literal` or a `Param`.
    Template { parts: Vec<PathSegment> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationStatus {
    Implemented,
    Planned { reason: String },
    Gated { gate: RuntimeGate },
}

ir_struct! {
    pub struct RuntimeGate {
        pub env_var: String,
        pub default_on: bool,
        pub disabled_status: u16,
    }
}

ir_struct! {
    /// How an rpc-unflattened operation is put back on the wire.
    pub struct RpcBinding {
        /// Body field carrying the method (`method`).
        pub discriminator_field: String,
        /// Value to send (`workflow.action.send`).
        pub discriminator_value: String,
        /// Body field carrying the parameters (`params`).
        pub params_field: String,
        /// Other required members of the envelope whose schema is a
        /// constant, sent verbatim (JSON-RPC `jsonrpc: "2.0"`).
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pub constants: BTreeMap<String, serde_json::Value>,
    }
}

ir_struct! {
    pub struct ParamSet {
        #[serde(default)]
        pub path: Vec<Param>,
        #[serde(default)]
        pub query: Vec<Param>,
        #[serde(default)]
        pub header: Vec<Param>,
        #[serde(default)]
        pub cookie: Vec<Param>,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ParamStyle {
    Simple,
    Form,
    Label,
    Matrix,
    SpaceDelimited,
    PipeDelimited,
    DeepObject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ParamRole {
    Plain,
    IdempotencyKey,
    DryRun,
    Origin,
    Auth,
}

ir_struct! {
    pub struct Param {
        pub wire_name: String,
        pub name: Ident,
        pub ty: TypeRef,
        pub required: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub doc: Option<Doc>,
        pub style: ParamStyle,
        pub explode: bool,
        pub role: ParamRole,
        #[serde(default)]
        pub deprecated: bool,
        /// Set when the parameter is declared with `content` instead of
        /// `schema`: the value is serialized with this media type (for
        /// example `application/json` for `?filter=<JSON>`), and `style`
        /// and `explode` do not apply.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub media_type: Option<String>,
    }
}

ir_struct! {
    pub struct Body {
        /// One entry per media type, in spec order.
        pub content: Vec<BodyContent>,
        pub required: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub doc: Option<Doc>,
    }
}

ir_struct! {
    pub struct BodyContent {
        pub media_type: String,
        pub ty: TypeRef,
        pub encoding: BodyEncoding,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BodyEncoding {
    Json,
    Form,
    Multipart,
    Bytes,
    Text,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum StatusMatch {
    Exact(u16),
    /// `4XX` is `Range(4)`.
    Range(u8),
    Default,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponseKind {
    Success,
    Error,
    /// The request may have been applied (manifest `ambiguous_statuses`).
    Ambiguous,
}

ir_struct! {
    pub struct Response {
        pub status: StatusMatch,
        /// Empty means a bare status with no body.
        #[serde(default)]
        pub content: Vec<BodyContent>,
        #[serde(default)]
        pub headers: Vec<Header>,
        pub kind: ResponseKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub doc: Option<Doc>,
    }
}

ir_struct! {
    pub struct Header {
        pub wire_name: String,
        pub ty: TypeRef,
        pub required: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub doc: Option<Doc>,
    }
}

ir_struct! {
    /// All schemes must be satisfied; scopes per scheme.
    pub struct SecurityRequirement {
        pub all_of: Vec<SchemeUse>,
    }
}

ir_struct! {
    pub struct SchemeUse {
        pub scheme: String,
        #[serde(default)]
        pub scopes: Vec<String>,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyIn {
    Header,
    Query,
    Cookie,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthScheme {
    ApiKey {
        name: String,
        location: ApiKeyIn,
        wire_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        doc: Option<Doc>,
    },
    HttpBearer {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        doc: Option<Doc>,
        /// Required token prefix (`tungsten.yml` `auth_profiles.<name>.bearer.prefix`,
        /// e.g. `ztw_`). Unlike `format`, the runtime enforces it before sending.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
        /// Environment variable the profile reads the token from
        /// (`auth_profiles.<name>.bearer.env`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<String>,
    },
    HttpBasic {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        doc: Option<Doc>,
    },
    OAuth2 {
        name: String,
        flows: Vec<OAuthFlow>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        doc: Option<Doc>,
    },
    OpenIdConnect {
        name: String,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        doc: Option<Doc>,
    },
    /// Manifest-defined profile satisfying several spec schemes at once.
    Composite {
        name: String,
        satisfies: Vec<String>,
        parts: Vec<CompositePart>,
    },
}

impl AuthScheme {
    pub fn name(&self) -> &str {
        match self {
            AuthScheme::ApiKey { name, .. }
            | AuthScheme::HttpBearer { name, .. }
            | AuthScheme::HttpBasic { name, .. }
            | AuthScheme::OAuth2 { name, .. }
            | AuthScheme::OpenIdConnect { name, .. }
            | AuthScheme::Composite { name, .. } => name,
        }
    }
}

ir_struct! {
    pub struct OAuthFlow {
        /// `clientCredentials`, `authorizationCode`, `password`, `implicit`.
        pub kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub token_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub authorization_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub refresh_url: Option<String>,
        #[serde(default)]
        pub scopes: BTreeMap<String, String>,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompositePart {
    Cookie {
        name: String,
    },
    Header {
        name: String,
        /// Value must equal this cookie's value (double-submit CSRF).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        equals_cookie: Option<String>,
        /// Value comes from this client configuration key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_config: Option<String>,
        /// Only sent on non-safe methods when true.
        #[serde(default)]
        mutation_only: bool,
    },
    Bearer {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaginationStyle {
    Cursor {
        request_param: String,
        response_field: String,
    },
    Offset {
        offset_param: String,
        limit_param: String,
    },
    Page {
        page_param: String,
        size_param: String,
    },
    LinkHeader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Exhausted {
    CursorNull,
    EmptyItems,
    NoLink,
}

ir_struct! {
    pub struct Pagination {
        pub style: PaginationStyle,
        /// Response field holding the page items (`deliveries`).
        pub items_field: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub page_size_param: Option<String>,
        pub exhausted_when: Exhausted,
        /// True when inferred by heuristic rather than declared.
        pub inferred: bool,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Streaming {
    Sse,
    Ndjson,
    Bytes,
}

ir_struct! {
    /// An error model. On a namespace it describes that document's errors.
    /// `Ir.errors` is the API-wide view: every code of every namespace with
    /// its statuses merged, and the envelope, code field and message field
    /// only when every namespace that has one agrees (always, for a single
    /// namespace).
    #[derive(Default)]
    pub struct ErrorModel {
        /// The JSON error schema used by most error responses, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub envelope: Option<TypeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub code_field: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub message_field: Option<String>,
        #[serde(default)]
        pub codes: Vec<ErrorCode>,
    }
}

ir_struct! {
    pub struct ErrorCode {
        pub code: String,
        /// Statuses where the code may appear, sorted, deduplicated.
        #[serde(default)]
        pub statuses: Vec<u16>,
    }
}

impl Ir {
    /// All callable operations in deterministic order (namespace, then
    /// depth-first resource order).
    pub fn operations(&self) -> Vec<&Operation> {
        fn walk<'a>(r: &'a Resource, out: &mut Vec<&'a Operation>) {
            out.extend(r.operations.iter());
            for c in &r.children {
                walk(c, out);
            }
        }
        let mut out = vec![];
        for ns in &self.namespaces {
            for r in &ns.resources {
                walk(r, &mut out);
            }
        }
        out
    }

    /// JSON Schema of the IR, published as `specs/ir.schema.json`.
    pub fn json_schema() -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(Ir)).expect("schema serializes")
    }
}

/// A title without a trailing `API` word, for text that appends ` API`
/// itself ("the Things API"): `Things API` gives `Things`, so the text
/// never reads "Things API API". A title that is only `API` is kept.
pub fn title_stem(title: &str) -> &str {
    let trimmed = title.trim_end();
    match trimmed
        .len()
        .checked_sub(3)
        .and_then(|at| trimmed.split_at_checked(at))
    {
        Some((head, tail))
            if tail.eq_ignore_ascii_case("api") && head.ends_with(char::is_whitespace) =>
        {
            let stem = head.trim_end();
            if stem.is_empty() { title } else { stem }
        }
        _ => title,
    }
}
