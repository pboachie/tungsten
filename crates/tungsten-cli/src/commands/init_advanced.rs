// SPDX-License-Identifier: AGPL-3.0-only
//! Restrictive wizard settings share ordinary manifests and validation.
use std::io::Read;
use std::path::{Path, PathBuf};

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
    if args.profile.len() > tungsten_config::profiles::MAX_IDENTITIES {
        return Err("select at most 64 profiles");
    }
    for profile in &args.profile {
        if !tungsten_config::profiles::is_relative_local_path(profile) || profile.contains('\\') {
            return Err(
                "profiles must be relative local paths using `/`; remote profiles are never downloaded",
            );
        }
        // `init` creates `--dir`; until it exists, `..` cannot be resolved
        // through it, so a profile beside a new project is found lexically.
        let path = if args.dir.exists() {
            args.dir.join(profile)
        } else {
            tungsten_config::workspace::normalize(&args.dir.join(profile))
        };
        if !path.is_file() {
            return Err("a selected profile file does not exist relative to the project directory");
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

/// The include root written for a selected profile: its own directory.
pub(super) fn profile_root(profile: &str) -> String {
    match profile.rsplit_once('/') {
        Some((parent, _)) if !parent.is_empty() => parent.to_owned(),
        _ => ".".to_owned(),
    }
}

/// Compose the selected profiles exactly as compilation will, without writing.
/// `pending` is a profile init is about to write, read from memory.
pub(super) fn check_profiles(
    config: &mut tungsten_config::TungstenConfig,
    dir: &Path,
    pending: Option<(&Path, &str)>,
) -> tungsten_core::Diagnostics {
    if config.profiles.is_empty() {
        return tungsten_core::Diagnostics::new();
    }
    let project = tungsten_config::profiles::Declaration {
        kind: tungsten_config::profiles::SourceKind::Project,
        identity: dir.join("tungsten.yml"),
        file: "tungsten.yml".into(),
        pointer: "/profiles".into(),
        digest: None,
    };
    let pending = pending.and_then(|(path, text)| {
        let parent = std::fs::canonicalize(path.parent()?).ok()?;
        Some((parent.join(path.file_name()?), text.to_owned()))
    });
    let source = LocalProfiles { pending };
    tungsten_config::profiles::compose(config, dir, project, &source).diagnostics
}

/// Local files only; reads stop at the composer's byte allowance.
struct LocalProfiles {
    /// Canonical identity and text of a profile not yet written.
    pending: Option<(PathBuf, String)>,
}

impl tungsten_config::profiles::ProfileSource for LocalProfiles {
    fn canonical_root(&self, path: &Path) -> Option<PathBuf> {
        std::fs::canonicalize(path)
            .ok()
            .filter(|root| root.is_dir())
    }
    fn canonical_path(&self, path: &Path) -> Option<PathBuf> {
        std::fs::canonicalize(path).ok().or_else(|| {
            let (pending, _) = self.pending.as_ref()?;
            let parent = std::fs::canonicalize(path.parent()?).ok()?;
            let candidate = parent.join(path.file_name()?);
            (&candidate == pending).then_some(candidate)
        })
    }
    fn read(
        &self,
        path: &Path,
        max_bytes: usize,
    ) -> Result<(String, tungsten_core::Digest), tungsten_core::Diagnostic> {
        if let Some((pending, text)) = &self.pending
            && pending == path
            && !path.exists()
        {
            let digest = tungsten_core::Digest::of(text.as_bytes());
            return Ok((text.clone(), digest));
        }
        let refused = || tungsten_core::Diagnostic::error("TG0101", "cannot read local profile");
        let file = std::fs::File::open(path).map_err(|_| refused())?;
        let mut bytes = vec![];
        file.take(max_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| refused())?;
        if bytes.len() > max_bytes {
            return Err(refused());
        }
        let digest = tungsten_core::Digest::of(&bytes);
        String::from_utf8(bytes)
            .map(|text| (text, digest))
            .map_err(|_| refused())
    }
    fn is_cancelled(&self) -> bool {
        false
    }
}
