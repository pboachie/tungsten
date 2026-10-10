// SPDX-License-Identifier: AGPL-3.0-only
//! Semantic validation of a deserialized manifest (rules a JSON Schema
//! cannot express). Every problem is an error with the JSON Pointer of the
//! offending node; with a [`ManifestSource`] the message also names its
//! line and column.

use std::collections::BTreeMap;

use tungsten_core::{Diagnostic, Diagnostics};

use crate::{
    AuthProfile, CompositePartConfig, ManifestSource, PaginationConfig, TungstenConfig, pointer,
};

/// The only supported manifest format version.
pub const MANIFEST_VERSION: u32 = 1;

/// Target names accepted under `targets`.
pub const KNOWN_TARGETS: &[&str] = &["docs", "mcp", "mock", "python", "rust", "typescript"];

/// Option keys of an external target that belong to tungsten; every other
/// key of the target is passed to the emitter as its options.
pub const EXTERNAL_RESERVED_KEYS: &[&str] = &["out", "external", "timeout_ms", "max_output_bytes"];

/// Longest `timeout_ms` accepted for an external emitter (one hour).
pub const MAX_EXTERNAL_TIMEOUT_MS: u64 = 3_600_000;

/// How an external emitter is run, from the `external` key of a target and
/// its two limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalTarget {
    /// The command line (`./emit.sh`, `node emit.js --flag`); `None` is
    /// `external: true`: find `tungsten-emit-<name>` on `PATH`.
    pub command: Option<String>,
    /// `timeout_ms`, when set.
    pub timeout_ms: Option<u64>,
    /// `max_output_bytes`, when set.
    pub max_output_bytes: Option<u64>,
}

/// The external emitter settings of a target's options. `Ok(None)` when the
/// target has no `external` key; `Err` names the first malformed value.
pub fn external_target(options: &serde_json::Value) -> Result<Option<ExternalTarget>, String> {
    use serde_json::Value;
    let Some(external) = options.get("external") else {
        return Ok(None);
    };
    let command = match external {
        Value::Bool(true) => None,
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => {
            return Err(
                "`external` must be true (find `tungsten-emit-<name>` on PATH) or a non-empty command"
                    .into(),
            );
        }
    };
    let number = |key: &str, max: u64| -> Result<Option<u64>, String> {
        match options.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => match v.as_u64() {
                Some(n) if (1..=max).contains(&n) => Ok(Some(n)),
                _ => Err(format!("`{key}` must be an integer from 1 to {max}")),
            },
        }
    };
    Ok(Some(ExternalTarget {
        command,
        timeout_ms: number("timeout_ms", MAX_EXTERNAL_TIMEOUT_MS)?,
        max_output_bytes: number("max_output_bytes", 1 << 40)?,
    }))
}

/// Whether `name` is a valid name for an external target:
/// `^[a-z][a-z0-9_-]*$` (it becomes part of `tungsten-emit-<name>`).
pub fn is_external_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Whether `name` is a valid namespace or API machine name:
/// `^[a-z][a-z0-9_]*$`.
pub fn is_machine_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Whether `name` is a valid method name override in `naming.operations`:
/// `^[A-Za-z][A-Za-z0-9_]*$`.
fn is_method_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Check the manifest rules that the JSON Schema cannot express. `file`
/// names the manifest in labels; `source`, when given, adds line and
/// column to each message.
pub fn validate(
    config: &TungstenConfig,
    file: &str,
    source: Option<&ManifestSource>,
) -> Diagnostics {
    let mut v = Validator {
        file,
        source,
        out: Diagnostics::new(),
    };
    v.version(config);
    v.api(config);
    let namespaces = v.inputs(config);
    v.resources(config, &namespaces);
    v.naming(config, &namespaces);
    v.auth_profiles(config);
    v.pagination(config, &namespaces);
    v.targets(config);
    v.agent(config);
    v.out
}

struct Validator<'a> {
    file: &'a str,
    source: Option<&'a ManifestSource>,
    out: Diagnostics,
}

impl Validator<'_> {
    fn report(&mut self, code: &str, pointer: &str, message: String) {
        let message = match self.source.and_then(|s| s.line_col(pointer)) {
            Some((line, col)) => format!("{message} (line {line}, column {col})"),
            None => message,
        };
        self.out
            .push(Diagnostic::error(code, message).at(self.file, pointer, None));
    }

    fn version(&mut self, config: &TungstenConfig) {
        if config.tungsten != MANIFEST_VERSION {
            self.report(
                "TG0602",
                "/tungsten",
                format!(
                    "unsupported manifest version {}; this tungsten reads version {MANIFEST_VERSION}",
                    config.tungsten
                ),
            );
        }
    }

    fn api(&mut self, config: &TungstenConfig) {
        if !is_machine_name(&config.api.name) {
            self.report(
                "TG0602",
                "/api/name",
                format!(
                    "api.name `{}` must match ^[a-z][a-z0-9_]*$",
                    config.api.name
                ),
            );
        }
    }

    /// Checks `inputs` and returns the declared namespaces with the index
    /// of their first declaration.
    fn inputs(&mut self, config: &TungstenConfig) -> BTreeMap<String, usize> {
        let mut namespaces = BTreeMap::new();
        if config.inputs.is_empty() {
            self.report(
                "TG0602",
                "/inputs",
                "inputs must list at least one OpenAPI document".into(),
            );
        }
        for (i, input) in config.inputs.iter().enumerate() {
            let at = format!("/inputs/{i}/namespace");
            if !is_machine_name(&input.namespace) {
                self.report(
                    "TG0602",
                    &at,
                    format!(
                        "namespace `{}` must match ^[a-z][a-z0-9_]*$",
                        input.namespace
                    ),
                );
            }
            if let Some(first) = namespaces.get(&input.namespace) {
                self.report(
                    "TG0602",
                    &at,
                    format!(
                        "duplicate namespace `{}` (first declared at /inputs/{first}/namespace)",
                        input.namespace
                    ),
                );
            } else {
                namespaces.insert(input.namespace.clone(), i);
            }
        }
        namespaces
    }

    fn resources(&mut self, config: &TungstenConfig, namespaces: &BTreeMap<String, usize>) {
        for ns in config.resources.keys() {
            if !namespaces.contains_key(ns) {
                let at = pointer::from_tokens(["resources", ns]);
                self.report(
                    "TG0604",
                    &at,
                    format!(
                        "resources names unknown namespace `{ns}`; declared: {}",
                        declared(namespaces)
                    ),
                );
            }
        }
    }

    fn naming(&mut self, config: &TungstenConfig, namespaces: &BTreeMap<String, usize>) {
        for (op, method) in &config.naming.operations {
            let at = pointer::from_tokens(["naming", "operations", op]);
            self.operation_ref(op, &at, namespaces);
            if !is_method_name(method) {
                self.report(
                    "TG0602",
                    &at,
                    format!("method name `{method}` for `{op}` must match ^[A-Za-z][A-Za-z0-9_]*$"),
                );
            }
        }
    }

    /// An operation ref is `<namespace>.<operationId>` with a declared
    /// namespace.
    fn operation_ref(&mut self, op: &str, at: &str, namespaces: &BTreeMap<String, usize>) {
        match op.split_once('.') {
            Some((ns, rest)) if !ns.is_empty() && !rest.is_empty() => {
                if !namespaces.contains_key(ns) {
                    self.report(
                        "TG0604",
                        at,
                        format!(
                            "operation ref `{op}` names unknown namespace `{ns}`; declared: {}",
                            declared(namespaces)
                        ),
                    );
                }
            }
            _ => self.report(
                "TG0602",
                at,
                format!("operation ref `{op}` must have the form <namespace>.<operationId>"),
            ),
        }
    }

    fn auth_profiles(&mut self, config: &TungstenConfig) {
        for (name, profile) in &config.auth_profiles {
            let at = pointer::from_tokens(["auth_profiles", name]);
            let kinds: Vec<&str> = [
                ("composite", profile.composite.is_some()),
                ("bearer", profile.bearer.is_some()),
                ("api_key", profile.api_key.is_some()),
            ]
            .into_iter()
            .filter_map(|(k, present)| present.then_some(k))
            .collect();
            if kinds.len() != 1 {
                let found = list_or_none(&kinds);
                self.report(
                    "TG0602",
                    &at,
                    format!(
                        "auth profile `{name}` must declare exactly one of composite, bearer, api_key (found: {found})"
                    ),
                );
            }
            if let Some(parts) = &profile.composite {
                self.composite(name, profile, parts, &pointer::child(&at, "composite"));
            }
        }
    }

    fn composite(
        &mut self,
        name: &str,
        profile: &AuthProfile,
        parts: &[CompositePartConfig],
        at: &str,
    ) {
        if parts.is_empty() {
            self.report(
                "TG0602",
                at,
                format!("composite auth profile `{name}` has no parts"),
            );
        }
        let cookies: Vec<&str> = parts
            .iter()
            .filter_map(|p| match p {
                CompositePartConfig::Cookie { cookie } => Some(cookie.as_str()),
                _ => None,
            })
            .collect();
        for (i, part) in parts.iter().enumerate() {
            let CompositePartConfig::Header { header } = part else {
                continue;
            };
            let at = format!("{at}/{i}/header");
            match (&header.equals_cookie, &header.from_config) {
                (Some(_), Some(_)) | (None, None) => self.report(
                    "TG0602",
                    &at,
                    format!(
                        "header `{}` needs exactly one value source: equals_cookie or from_config",
                        header.name
                    ),
                ),
                _ => {}
            }
            if let Some(cookie) = &header.equals_cookie
                && !cookies.contains(&cookie.as_str())
            {
                self.report(
                    "TG0602",
                    &format!("{at}/equals_cookie"),
                    format!(
                        "header `{}` copies cookie `{cookie}`, which is not a cookie part of `{name}`",
                        header.name
                    ),
                );
            }
            if let Some(key) = &header.from_config
                && !profile.config.contains_key(key)
            {
                self.report(
                    "TG0602",
                    &format!("{at}/from_config"),
                    format!(
                        "header `{}` reads config key `{key}`, which is not declared in auth_profiles.{name}.config",
                        header.name
                    ),
                );
            }
            if let Some(when) = &header.when
                && when != "mutation"
            {
                self.report(
                    "TG0602",
                    &format!("{at}/when"),
                    format!(
                        "header `{}` has when: `{when}`; the only supported value is `mutation`",
                        header.name
                    ),
                );
            }
        }
    }

    fn pagination(&mut self, config: &TungstenConfig, namespaces: &BTreeMap<String, usize>) {
        for (op, entry) in &config.pagination {
            let at = pointer::from_tokens(["pagination", op]);
            self.operation_ref(op, &at, namespaces);
            let styles = pagination_styles(entry);
            if entry.none && !styles.is_empty() {
                self.report(
                    "TG0602",
                    &at,
                    format!(
                        "pagination for `{op}` sets none: true and also declares {}",
                        styles.join(", ")
                    ),
                );
            } else if !entry.none && styles.len() != 1 {
                let found = list_or_none(&styles);
                self.report(
                    "TG0602",
                    &at,
                    format!(
                        "pagination for `{op}` must declare exactly one of cursor, offset, page, link_header, or none: true (found: {found})"
                    ),
                );
            }
        }
    }

    fn agent(&mut self, config: &TungstenConfig) {
        if config.agent.as_deref().is_some_and(|p| p.trim().is_empty()) {
            self.report(
                "TG0602",
                "/agent",
                "agent must be the path of an agent manifest, relative to this file".into(),
            );
        }
    }

    fn targets(&mut self, config: &TungstenConfig) {
        for (target, options) in &config.targets {
            let at = pointer::from_tokens(["targets", target]);
            let external = options.get("external").is_some();
            if KNOWN_TARGETS.contains(&target.as_str()) {
                if external {
                    self.report(
                        "TG0602",
                        &format!("{at}/external"),
                        format!(
                            "target `{target}` is built in; `external` is only for targets with another name"
                        ),
                    );
                }
            } else if !external {
                self.report(
                    "TG0602",
                    &at,
                    format!(
                        "unknown target `{target}`; known targets: {} (an external emitter is declared with `external: true` or `external: <command>`)",
                        KNOWN_TARGETS.join(", ")
                    ),
                );
            } else if !is_external_name(target) {
                self.report(
                    "TG0602",
                    &at,
                    format!(
                        "external target name `{target}` must match ^[a-z][a-z0-9_-]*$ (it names the executable `tungsten-emit-{target}`)"
                    ),
                );
            }
            if external && let Err(message) = external_target(options) {
                self.report("TG0602", &format!("{at}/external"), message);
            }
        }
        // Each target owns its output directory (its `.tungsten/manifest.json`
        // lists its files and removes the stale ones), so two targets must
        // never share a directory or nest one inside the other.
        let outs: Vec<(&String, String, Vec<String>)> = config
            .targets
            .iter()
            .map(|(name, options)| {
                let out = options
                    .get("out")
                    .and_then(|v| v.as_str())
                    .map_or_else(|| format!("generated/{name}"), str::to_string);
                let parts = lexical_components(&out);
                (name, out, parts)
            })
            .collect();
        for (i, (a, a_out, a_parts)) in outs.iter().enumerate() {
            for (b, b_out, b_parts) in outs.iter().skip(i + 1) {
                let relation = if a_parts == b_parts {
                    "the same output directory"
                } else if b_parts.starts_with(a_parts) || a_parts.starts_with(b_parts) {
                    "nested output directories"
                } else {
                    continue;
                };
                let at = pointer::from_tokens(["targets", b.as_str(), "out"]);
                let at = if config
                    .targets
                    .get(b.as_str())
                    .and_then(|o| o.get("out"))
                    .is_some()
                {
                    at
                } else {
                    pointer::from_tokens(["targets", b.as_str()])
                };
                self.report(
                    "TG0612",
                    &at,
                    format!(
                        "targets `{a}` (out: {a_out}) and `{b}` (out: {b_out}) have {relation}; each target deletes files it did not generate in its own directory, so give every target a separate, non-nested `out`"
                    ),
                );
            }
        }
    }
}

/// The components of a relative or absolute `out` path, with `.` dropped
/// and `..` applied lexically (a leading `..` is kept), so `gen/./ts/` and
/// `gen/ts` compare equal.
fn lexical_components(path: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    if path.starts_with('/') || path.starts_with('\\') {
        out.push("/".into());
    }
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." if out.last().is_some_and(|l| l != ".." && l != "/") => {
                out.pop();
            }
            other => out.push(other.to_string()),
        }
    }
    out
}

fn pagination_styles(entry: &PaginationConfig) -> Vec<&'static str> {
    [
        ("cursor", entry.cursor.is_some()),
        ("offset", entry.offset.is_some()),
        ("page", entry.page.is_some()),
        ("link_header", entry.link_header.is_some()),
    ]
    .into_iter()
    .filter_map(|(k, present)| present.then_some(k))
    .collect()
}

/// Declared namespaces in declaration order.
fn declared(namespaces: &BTreeMap<String, usize>) -> String {
    let mut names: Vec<(&usize, &str)> = namespaces.iter().map(|(n, i)| (i, n.as_str())).collect();
    names.sort();
    let names: Vec<&str> = names.into_iter().map(|(_, n)| n).collect();
    list_or_none(&names)
}

fn list_or_none(items: &[&str]) -> String {
    if items.is_empty() {
        "none".into()
    } else {
        items.join(", ")
    }
}
