// SPDX-License-Identifier: Apache-2.0
//! The command table a generated CLI hands to the kit (stable; extended
//! additively).

use serde_json::Value;
use tungsten_runtime::Safety;

/// How a flag's text becomes a JSON value of the arguments object.
#[derive(Debug, Clone, PartialEq)]
pub enum FlagKind {
    String,
    Integer,
    Number,
    Boolean,
    /// One of the listed strings.
    Enum(Vec<String>),
    /// Repeatable; each occurrence parsed as the inner kind.
    Array(Box<FlagKind>),
    /// JSON text, or `@path` to read it from a file (`-` reads stdin).
    Json,
    /// `@path` of a file whose bytes are the value (a binary argument).
    File,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CliFlag {
    /// Flag name without dashes, kebab-case (`endpoint-id`).
    pub flag: String,
    /// Key in the arguments object (`endpoint_id`).
    pub arg: String,
    pub kind: FlagKind,
    pub required: bool,
    pub help: String,
    pub sensitive: bool,
}

/// One entry of a remediation table (`agent.yml` `errors.codes`, or the
/// `remediation` of one operation), for `explain-error`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CliRemediation {
    /// The API's error code (`replay_not_eligible`).
    pub code: String,
    /// Error category (`PRECONDITION_FAILED`), when the entry sets one.
    pub category: Option<String>,
    pub text: Option<String>,
    /// `never`, `after_delay`, `same_key_only` or `after_remediation`.
    pub retryable: Option<String>,
    pub next_action: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CliOp {
    /// Operation id (`public.webhooks.rotate`).
    pub id: String,
    /// Command path below the root (`["webhooks", "rotate"]`).
    pub path: Vec<String>,
    pub about: String,
    pub safety: Safety,
    pub flags: Vec<CliFlag>,
    /// Name of the whole-body argument (`--body @file.json` or stdin), if the
    /// operation has one.
    pub body_arg: Option<String>,
    pub paginated: bool,
    /// Compact JSON Schema of the operation (`<api> schema <resource> <op>`).
    pub schema: Value,
    /// Operation-specific remediation by API error code, sorted by code
    /// (`<api> explain-error`).
    pub remediation: Vec<CliRemediation>,
    /// The operation's remediation note, if any.
    pub remediation_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CliMacro {
    pub name: String,
    /// Command path below the root (`["macros", "list-all-owner-devices"]`).
    pub path: Vec<String>,
    pub about: String,
    pub safety: Safety,
    pub flags: Vec<CliFlag>,
    pub schema: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CliSpec {
    /// Binary name (`zrotext`).
    pub bin: String,
    pub about: String,
    pub version: String,
    /// Prefix of the environment variables (`ZROTEXT`): `<PREFIX>_BASE_URL`,
    /// `<PREFIX>_PROFILE`, and one per credential of the auth profiles.
    pub env_prefix: String,
    /// Directory name under the config home for `config.toml`.
    pub config_dir: String,
    /// Credential variables named in `tungsten.yml`
    /// (`auth_profiles.<name>.bearer.env` or `api_key.env`): scheme name,
    /// variable. They replace `<PREFIX>_<SCHEME>` for that scheme.
    pub credential_env: Vec<(String, String)>,
    pub ops: Vec<CliOp>,
    pub macros: Vec<CliMacro>,
    /// Remediation by API error code that applies to every operation, sorted
    /// by code (`<api> explain-error`).
    pub error_codes: Vec<CliRemediation>,
}
