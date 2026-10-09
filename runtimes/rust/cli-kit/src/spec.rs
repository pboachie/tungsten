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
    pub ops: Vec<CliOp>,
    pub macros: Vec<CliMacro>,
}
