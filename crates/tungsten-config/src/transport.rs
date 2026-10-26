// SPDX-License-Identifier: AGPL-3.0-only
//! Enterprise transport and account references: names of the places a runtime
//! reads proxy, trust and client-certificate material from, never the material.
//!
//! Profiles and the project manifest declare `transport` with the same parser.
//! Every value is a reference (`{env: NAME}`, `{file: path}` or
//! `{store: store:item}`) or a non-secret name; inline proxy URLs,
//! certificates, keys and tokens are refused, and there is no setting that
//! disables certificate verification. References are explained by
//! `tungsten config` with their source; their targets are never read.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tungsten_core::{Diagnostic, Diagnostics};

/// Where a runtime finds one piece of transport material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Reference {
    /// An environment variable name; its value is read by the runtime.
    Env(String),
    /// A relative local file path (public material such as a CA bundle).
    File(String),
    /// A secure-store item, `<store>:<item>`, resolved by the runtime's store adapter.
    Store(String),
}

impl Reference {
    fn kind(&self) -> RefKind {
        match self {
            Self::Env(_) => RefKind::Env,
            Self::File(_) => RefKind::File,
            Self::Store(_) => RefKind::Store,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefKind {
    Env,
    File,
    Store,
}

impl RefKind {
    fn name(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::File => "file",
            Self::Store => "store",
        }
    }
}

/// Declared transport and account references. Absent fields leave the
/// runtime's own environment, trust store and credential handling in place.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransportReferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "proxy_schema")]
    pub proxy: Option<Reference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "no_proxy_schema")]
    pub no_proxy: Option<Reference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "ca_bundle_schema")]
    pub ca_bundle: Option<Reference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "client_certificate_schema")]
    pub client_certificate: Option<Reference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "client_key_schema")]
    pub client_key: Option<Reference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "credential_provider_schema")]
    pub credential_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "account_schema")]
    pub account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "environment_schema")]
    pub environment: Option<String>,
}

/// Reference fields and the reference kinds each accepts.
const REFERENCES: &[(&str, &[RefKind])] = &[
    // Proxy URLs can carry credentials, so they stay in the environment.
    ("proxy", &[RefKind::Env]),
    ("no_proxy", &[RefKind::Env]),
    ("ca_bundle", &[RefKind::Env, RefKind::File]),
    ("client_certificate", &[RefKind::Env, RefKind::File]),
    // A private key never sits next to the configuration.
    ("client_key", &[RefKind::Env, RefKind::Store]),
];

/// Credential providers a runtime may be asked to use. None of them is a
/// plaintext fallback.
pub const CREDENTIAL_PROVIDERS: &[&str] = &["environment", "secure_store", "hook"];

/// Name fields: non-secret identifiers of an account or environment.
const NAMES: &[&str] = &["account", "environment"];

/// Keys that would weaken certificate checks; refused with their own message.
const VERIFICATION_KEYS: &[&str] = &[
    "verify",
    "verify_tls",
    "tls_verify",
    "insecure",
    "insecure_skip_verify",
    "skip_verify",
    "danger_accept_invalid_certs",
    "reject_unauthorized",
];

impl TransportReferences {
    /// Validate a `transport` mapping. Diagnostics carry pointers relative to
    /// the mapping and never quote authored values.
    pub fn parse(value: &Value) -> (Self, Diagnostics) {
        let mut diagnostics = Diagnostics::new();
        let error = |pointer: &str, message: String| {
            Diagnostic::error("TG0602", message).at("", pointer, None)
        };
        let Some(fields) = value.as_object() else {
            diagnostics.push(error("", "`transport` must be a mapping".into()));
            return (Self::default(), diagnostics);
        };
        for (key, value) in fields {
            let at = crate::pointer::child("", key);
            if let Some((_, kinds)) = REFERENCES.iter().find(|(name, _)| name == key) {
                if let Some(message) = reference_problem(key, value, kinds) {
                    diagnostics.push(error(&at, message));
                }
            } else if key == "credential_provider" {
                if !value
                    .as_str()
                    .is_some_and(|provider| CREDENTIAL_PROVIDERS.contains(&provider))
                {
                    diagnostics.push(error(
                        &at,
                        format!(
                            "`credential_provider` must be one of {}",
                            CREDENTIAL_PROVIDERS.join(", ")
                        ),
                    ));
                }
            } else if NAMES.contains(&key.as_str()) {
                if !value.as_str().is_some_and(is_name) {
                    diagnostics.push(error(
                        &at,
                        format!(
                            "`{key}` must be a name of letters, digits, `.`, `_` or `-` (at most 64); it selects an account, it does not hold credentials"
                        ),
                    ));
                }
            } else if VERIFICATION_KEYS.contains(&key.as_str()) {
                diagnostics.push(
                    error(
                        &at,
                        "certificate verification cannot be configured or disabled".into(),
                    )
                    .with_help(
                        "trust a private CA with `ca_bundle`, or keep verification in a custom transport",
                    ),
                );
            } else {
                diagnostics.push(error(&at, "unsupported `transport` field".into()));
            }
        }
        let parsed = if diagnostics.has_errors() {
            Self::default()
        } else {
            serde_json::from_value(value.clone()).unwrap_or_default()
        };
        (parsed, diagnostics)
    }

    /// The fields in declaration order with their values, for inspection.
    pub fn fields(&self) -> Vec<(&'static str, Option<Value>)> {
        let reference =
            |r: &Option<Reference>| r.as_ref().and_then(|r| serde_json::to_value(r).ok());
        let name = |n: &Option<String>| n.clone().map(Value::String);
        vec![
            ("proxy", reference(&self.proxy)),
            ("no_proxy", reference(&self.no_proxy)),
            ("ca_bundle", reference(&self.ca_bundle)),
            ("client_certificate", reference(&self.client_certificate)),
            ("client_key", reference(&self.client_key)),
            ("credential_provider", name(&self.credential_provider)),
            ("account", name(&self.account)),
            ("environment", name(&self.environment)),
        ]
    }

    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

fn reference_problem(key: &str, value: &Value, kinds: &[RefKind]) -> Option<String> {
    let accepted = || {
        kinds
            .iter()
            .map(|kind| format!("`{{{}: ...}}`", kind.name()))
            .collect::<Vec<_>>()
            .join(" or ")
    };
    if value.is_string() {
        return Some(format!(
            "`{key}` takes a reference ({}), not an inline value; proxy URLs, certificates and keys stay out of configuration files",
            accepted()
        ));
    }
    let Ok(reference) = serde_json::from_value::<Reference>(value.clone()) else {
        return Some(format!("`{key}` must be exactly one of {}", accepted()));
    };
    if !kinds.contains(&reference.kind()) {
        return Some(format!("`{key}` accepts only {}", accepted()));
    }
    let valid = match &reference {
        Reference::Env(name) => is_env_name(name),
        Reference::File(path) => {
            crate::profiles::is_relative_local_path(path) && !path.contains('\\')
        }
        Reference::Store(item) => is_store_item(item),
    };
    (!valid).then(|| match reference.kind() {
        RefKind::Env => format!("`{key}` names an invalid environment variable"),
        RefKind::File => format!(
            "`{key}` file must be a relative local path using `/`; remote files are never downloaded"
        ),
        RefKind::Store => format!("`{key}` store reference must be `<store>:<item>`"),
    })
}

fn is_env_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn is_store_item(item: &str) -> bool {
    let Some((store, name)) = item.split_once(':') else {
        return false;
    };
    (1..=32).contains(&store.len())
        && store.starts_with(|c: char| c.is_ascii_lowercase())
        && store
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
        && (1..=128).contains(&name.len())
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-' | b'/' | b'@'))
}

fn is_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// JSON Schema of the `transport` mapping shared by profiles and manifests.
pub fn json_schema() -> Value {
    schemars::schema_for!(TransportReferences).to_value()
}

fn reference_schema(
    kinds: &[RefKind],
    description: &str,
    default_policy: &str,
) -> schemars::Schema {
    let options: Vec<Value> = kinds
        .iter()
        .map(|kind| {
            let pattern = match kind {
                RefKind::Env => r"^[A-Za-z_][A-Za-z0-9_]{0,127}$",
                RefKind::File => r"^[^\\:]+$",
                RefKind::Store => r"^[a-z][a-z0-9_-]{0,31}:[A-Za-z0-9._/@-]{1,128}$",
            };
            serde_json::json!({
                "type": "object", "additionalProperties": false,
                "required": [kind.name()],
                "properties": {kind.name(): {"type": "string", "pattern": pattern}}
            })
        })
        .collect();
    schemars::json_schema!({
        "description": description,
        "oneOf": options,
        "x-tungsten-inspection": {
            "default_policy": default_policy,
            "invalid_policy": "error; the configuration is refused",
            "redact": false
        }
    })
}

fn proxy_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    reference_schema(
        &[RefKind::Env],
        "Environment variable holding the HTTP(S) proxy URL. Proxy URLs can carry credentials, so only `{env: NAME}` is accepted.",
        "absent; the runtime's own proxy environment applies",
    )
}

fn no_proxy_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    reference_schema(
        &[RefKind::Env],
        "Environment variable holding the hosts that bypass the proxy.",
        "absent; the runtime's own NO_PROXY handling applies",
    )
}

fn ca_bundle_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    reference_schema(
        &[RefKind::Env, RefKind::File],
        "Private CA bundle to trust in addition to verification, by environment variable or relative file. Verification itself cannot be disabled.",
        "absent; the platform trust store applies",
    )
}

fn client_certificate_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    reference_schema(
        &[RefKind::Env, RefKind::File],
        "Client certificate for mTLS, by environment variable or relative file.",
        "absent; no client certificate is presented",
    )
}

fn client_key_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    reference_schema(
        &[RefKind::Env, RefKind::Store],
        "Private key of the client certificate, by environment variable or secure-store item. Key files are not accepted.",
        "absent; no client certificate is presented",
    )
}

fn credential_provider_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "description": "Where the runtime obtains credentials. A missing secure store is an error, never a plaintext fallback.",
        "type": "string", "enum": CREDENTIAL_PROVIDERS,
        "x-tungsten-inspection": {
            "default_policy": "absent; the generated client's own credential options apply",
            "invalid_policy": "error; the configuration is refused", "redact": false
        }
    })
}

fn name_schema(description: &str, default_policy: &str) -> schemars::Schema {
    schemars::json_schema!({
        "description": description,
        "type": "string", "pattern": r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$",
        "x-tungsten-inspection": {
            "default_policy": default_policy,
            "invalid_policy": "error; the configuration is refused", "redact": false
        }
    })
}

fn account_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    name_schema(
        "Non-secret name of the account the credential provider selects.",
        "absent; the provider's default account applies",
    )
}

fn environment_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    name_schema(
        "Non-secret name of the deployment environment (for example `staging`).",
        "absent; no environment is selected",
    )
}
