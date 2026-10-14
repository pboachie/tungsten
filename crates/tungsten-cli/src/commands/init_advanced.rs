// SPDX-License-Identifier: AGPL-3.0-only
//! Restrictive wizard settings share ordinary manifests and validation.
use crate::args::InitArgs;

pub(super) fn validate(args: &InitArgs) -> Result<(), &'static str> {
    if let Some(value) = &args.server_url {
        let url =
            url::Url::parse(value).map_err(|_| "server URL must be an absolute HTTP(S) URL")?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "server URL must not contain credentials, query or fragment; use environment authentication for live calls",
            );
        }
    }
    if !args.cli_profile.is_empty() {
        if !args
            .targets
            .iter()
            .any(|t| matches!(t.as_str(), "rust" | "cli"))
        {
            return Err("CLI environment profiles require the Rust or CLI output");
        }
        if std::fs::symlink_metadata(args.dir.join("tungsten-profiles.toml")).is_ok() {
            return Err(
                "existing tungsten-profiles.toml is preserved; choose a fresh project or edit profiles separately",
            );
        }
        let mut names = std::collections::BTreeSet::new();
        for profile in &args.cli_profile {
            let (name, value) = profile
                .split_once('=')
                .ok_or("CLI profile must be NAME=URL")?;
            if name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
                || !names.insert(name)
            {
                return Err(
                    "CLI profile names must be unique letters, numbers, hyphens or underscores",
                );
            }
            let url =
                url::Url::parse(value).map_err(|_| "CLI profile URL must be absolute HTTP(S)")?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err("CLI profile URLs cannot contain credentials, query or fragment");
            }
        }
    }
    for operation in args.hide_operation.iter().chain(&args.require_confirmation) {
        if !operation.contains('.')
            || operation.len() > 256
            || operation
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '*' | '?' | '[' | ']' | '{' | '}'))
        {
            return Err(
                "advanced operation controls require an exact namespace.operationId, without wildcards or whitespace",
            );
        }
    }
    Ok(())
}

pub(super) fn profiles(args: &InitArgs) -> Option<String> {
    if args.cli_profile.is_empty() {
        return None;
    }
    let mut text = "# Generated CLI profiles only. Credentials stay in environment variables.\n# Precedence: explicit flag, environment, selected profile, SDK default.\n".to_owned();
    for profile in &args.cli_profile {
        if let Some((name, value)) = profile.split_once('=') {
            text.push_str(&format!(
                "\n[profile.{}]\nbase_url = {}\n",
                serde_json::json!(name),
                serde_json::json!(value)
            ));
        }
    }
    Some(text)
}

pub(super) fn tools(args: &InitArgs) -> serde_json::Value {
    let mut operations = std::collections::BTreeMap::new();
    for operation in args.hide_operation.iter().chain(&args.require_confirmation) {
        let tool = operations
            .entry(operation)
            .or_insert_with(|| serde_json::json!({"operation": operation}));
        if args.hide_operation.contains(operation) {
            tool["hidden"] = true.into();
        }
        if args.require_confirmation.contains(operation) {
            tool["safety"] = "irreversible".into();
            tool["preview"] = "local".into();
        }
    }
    operations.into_values().collect()
}
