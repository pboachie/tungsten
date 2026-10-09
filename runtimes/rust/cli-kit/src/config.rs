// SPDX-License-Identifier: Apache-2.0
//! Settings and credentials: flag, then environment, then the profile of the
//! configuration file, then the default.
//!
//! Environment: `<PREFIX>_BASE_URL`, `<PREFIX>_PROFILE`, `<PREFIX>_CONFIG`,
//! `<PREFIX>_TIMEOUT` (seconds), and one variable per credential,
//! `<PREFIX>_<SCHEME>` for a token or key and `<PREFIX>_<SCHEME>_<PART>` for
//! each part of a composite profile (a cookie by its name, a header by the
//! cookie it equals or the configuration key it takes). The same names,
//! lower-cased and without the prefix, are the keys of a profile.
//!
//! The configuration file is `$XDG_CONFIG_HOME/<config_dir>/config.toml`
//! (`~/.config/<config_dir>/config.toml` without it, `%APPDATA%` on Windows):
//!
//! ```toml
//! profile = "work"            # default profile; "default" without it
//!
//! [profile.work]
//! base_url = "https://api.example.test"
//! timeout = 20
//! bearer_auth = "token"
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tungsten_runtime::{AuthConfig, AuthSchemeDescriptor, CompositePart, Credential};

use crate::spec::CliSpec;
use crate::toml::{self, Doc};

/// Where a setting came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Flag,
    Env,
    Profile,
    Default,
}

impl Source {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Source::Flag => "flag",
            Source::Env => "env",
            Source::Profile => "profile",
            Source::Default => "default",
        }
    }
}

/// The flags that feed the settings.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Flags<'a> {
    pub base_url: Option<&'a str>,
    pub profile: Option<&'a str>,
    pub config: Option<&'a str>,
    pub timeout: Option<&'a str>,
}

#[derive(Debug)]
pub(crate) struct Settings {
    pub profile: String,
    pub config_path: Option<PathBuf>,
    pub base_url: Option<(String, Source)>,
    pub timeout: Option<(Duration, Source)>,
    doc: Option<Doc>,
}

fn nonempty<'a>(env: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    env.get(name).map(String::as_str).filter(|v| !v.is_empty())
}

fn parse_seconds(text: &str, from: &str) -> Result<Duration, String> {
    match text.trim().parse::<f64>() {
        Ok(s) if s.is_finite() && s > 0.0 && s < 1.0e9 => Ok(Duration::from_secs_f64(s)),
        _ => Err(format!(
            "{from}: the timeout must be a positive number of seconds"
        )),
    }
}

fn default_config_path(spec: &CliSpec, env: &BTreeMap<String, String>) -> Option<PathBuf> {
    let base = if let Some(x) = nonempty(env, "XDG_CONFIG_HOME") {
        PathBuf::from(x)
    } else if let Some(h) = nonempty(env, "HOME") {
        Path::new(h).join(".config")
    } else if let Some(a) = nonempty(env, "APPDATA") {
        PathBuf::from(a)
    } else {
        let u = nonempty(env, "USERPROFILE")?;
        Path::new(u).join(".config")
    };
    Some(base.join(&spec.config_dir).join("config.toml"))
}

pub(crate) fn resolve(
    spec: &CliSpec,
    env: &BTreeMap<String, String>,
    flags: Flags<'_>,
) -> Result<Settings, String> {
    let var = |suffix: &str| format!("{}_{suffix}", spec.env_prefix);

    let explicit = flags
        .config
        .map(PathBuf::from)
        .or_else(|| nonempty(env, &var("CONFIG")).map(PathBuf::from));
    let path = explicit.clone().or_else(|| default_config_path(spec, env));
    let mut doc = None;
    if let Some(p) = &path {
        match std::fs::read_to_string(p) {
            Ok(text) => {
                doc = Some(
                    toml::parse(&text).map_err(|e| format!("config file {}: {e}", p.display()))?,
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && explicit.is_none() => {}
            Err(e) => return Err(format!("config file {}: {e}", p.display())),
        }
    }

    let requested = flags
        .profile
        .map(str::to_string)
        .or_else(|| nonempty(env, &var("PROFILE")).map(str::to_string));
    let profile = requested
        .clone()
        .or_else(|| {
            doc.as_ref()
                .and_then(|d| d.get(&["profile"]))
                .and_then(|v| v.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| "default".to_string());
    let defined = doc
        .as_ref()
        .is_some_and(|d| d.has_table(&["profile", &profile]));
    if !defined && (requested.is_some() || profile != "default") {
        return Err(match &path {
            Some(p) if doc.is_some() => {
                format!("profile `{profile}` is not defined in {}", p.display())
            }
            _ => format!("profile `{profile}` is requested but there is no config file"),
        });
    }
    let from_profile = |key: &str| {
        doc.as_ref()
            .and_then(|d| d.get(&["profile", &profile, key]))
    };

    let base_url = if let Some(v) = flags.base_url {
        Some((v.to_string(), Source::Flag))
    } else if let Some(v) = nonempty(env, &var("BASE_URL")) {
        Some((v.to_string(), Source::Env))
    } else {
        from_profile("base_url")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(|v| (v.to_string(), Source::Profile))
    };
    let timeout = if let Some(v) = flags.timeout {
        Some((parse_seconds(v, "--timeout")?, Source::Flag))
    } else if let Some(v) = nonempty(env, &var("TIMEOUT")) {
        Some((parse_seconds(v, &var("TIMEOUT"))?, Source::Env))
    } else {
        match from_profile("timeout") {
            Some(v) => {
                let secs = v.as_seconds().ok_or("profile timeout must be a number")?;
                Some((
                    parse_seconds(&secs.to_string(), "profile timeout")?,
                    Source::Profile,
                ))
            }
            None => None,
        }
    };
    Ok(Settings {
        profile,
        config_path: path.filter(|_| doc.is_some()),
        base_url,
        timeout,
        doc,
    })
}

impl Settings {
    /// A string key of the selected profile.
    fn profile_str(&self, key: &str) -> Option<&str> {
        self.doc
            .as_ref()?
            .get(&["profile", &self.profile, key])?
            .as_str()
            .filter(|v| !v.is_empty())
    }

    /// Keys of the selected profile that are neither a setting nor a
    /// credential of the API.
    pub(crate) fn unknown_keys(&self, known: &[String]) -> Vec<String> {
        let Some(doc) = &self.doc else { return vec![] };
        doc.keys(&["profile", &self.profile])
            .into_iter()
            .filter(|k| !matches!(*k, "base_url" | "timeout") && !known.iter().any(|n| n == k))
            .map(str::to_string)
            .collect()
    }
}

/// `ownerSession` and `__Host-zrotext_session` as `OWNER_SESSION` and
/// `HOST_ZROTEXT_SESSION`.
pub(crate) fn upper_words(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if i > 0 && !cur.is_empty() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(char::is_ascii_lowercase);
            let boundary = c.is_ascii_uppercase()
                && (prev.is_ascii_lowercase()
                    || prev.is_ascii_digit()
                    || (prev.is_ascii_uppercase() && next_lower));
            if boundary {
                words.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c.to_ascii_uppercase());
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words.join("_")
}

/// One credential the CLI can read: a secret, or one part of a composite
/// profile or OAuth2 client.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Slot {
    pub scheme: String,
    /// Key in the scheme's parts (`None`: the scheme's secret itself).
    pub part: Option<String>,
    pub env: String,
    pub profile_key: String,
}

/// The credential slots of an API's auth schemes.
pub(crate) fn slots(
    prefix: &str,
    overrides: &[(String, String)],
    auth: &[AuthSchemeDescriptor],
) -> Vec<Slot> {
    let covered = |name: &str| {
        auth.iter().any(|s| match s {
            AuthSchemeDescriptor::Composite {
                name: c, satisfies, ..
            } => c == name || satisfies.iter().any(|x| x == name),
            _ => false,
        })
    };
    let slot = |scheme: &str, part: Option<&str>, words: &str| {
        let mut suffix = upper_words(scheme);
        if !words.is_empty() {
            suffix.push('_');
            suffix.push_str(words);
        }
        Slot {
            scheme: scheme.to_string(),
            part: part.map(str::to_string),
            env: match overrides.iter().find(|(name, _)| name == scheme) {
                Some((_, variable)) if part.is_none() => variable.clone(),
                _ => format!("{prefix}_{suffix}"),
            },
            profile_key: suffix.to_ascii_lowercase(),
        }
    };
    let mut out: Vec<Slot> = Vec::new();
    for s in auth {
        match s {
            AuthSchemeDescriptor::Composite { name, parts, .. } => {
                for p in parts {
                    let key = match p {
                        CompositePart::Cookie { name } => name.clone(),
                        CompositePart::Bearer { .. } => "bearer".to_string(),
                        CompositePart::Header {
                            name,
                            equals_cookie,
                            from_config,
                            ..
                        } => equals_cookie
                            .clone()
                            .or_else(|| from_config.clone())
                            .unwrap_or_else(|| name.clone()),
                    };
                    if !out
                        .iter()
                        .any(|o| o.scheme == *name && o.part.as_deref() == Some(&key))
                    {
                        let words = upper_words(&key);
                        out.push(slot(name, Some(&key), &words));
                    }
                }
            }
            AuthSchemeDescriptor::Oauth2 { name, .. } if !covered(name) => {
                out.push(slot(name, Some("clientId"), "CLIENT_ID"));
                out.push(slot(name, Some("clientSecret"), "CLIENT_SECRET"));
            }
            other if !covered(other.name()) => out.push(slot(other.name(), None, "")),
            _ => {}
        }
    }
    out
}

/// A credential slot and where its value was found.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Found {
    pub slot: Slot,
    pub source: Option<Source>,
}

/// The credentials of `auth` from the environment and the profile, and what
/// was found for each slot. A profile that holds credentials is reported in
/// `warnings` when other users can read the file.
pub(crate) fn credentials(
    prefix: &str,
    overrides: &[(String, String)],
    auth: &[AuthSchemeDescriptor],
    env: &BTreeMap<String, String>,
    settings: &Settings,
) -> (AuthConfig, Vec<Found>, Vec<String>) {
    let mut config = AuthConfig::new();
    let mut parts: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut found = Vec::new();
    let mut used_profile = false;
    for slot in slots(prefix, overrides, auth) {
        let value = nonempty(env, &slot.env)
            .map(|v| (v.to_string(), Source::Env))
            .or_else(|| {
                settings
                    .profile_str(&slot.profile_key)
                    .map(|v| (v.to_string(), Source::Profile))
            });
        let source = value.as_ref().map(|(_, s)| *s);
        if let Some((v, s)) = value {
            used_profile |= s == Source::Profile;
            match &slot.part {
                Some(p) => {
                    parts
                        .entry(slot.scheme.clone())
                        .or_default()
                        .insert(p.clone(), v);
                }
                None => {
                    config.insert(slot.scheme.clone(), Credential::Secret(v));
                }
            }
        }
        found.push(Found { slot, source });
    }
    for (scheme, map) in parts {
        config.insert(scheme, Credential::Parts(map));
    }
    let mut warnings = Vec::new();
    if used_profile
        && let Some(p) = &settings.config_path
        && readable_by_others(p)
    {
        warnings.push(format!(
            "warning: {} holds credentials and can be read by other users; run `chmod 600 {}`",
            p.display(),
            p.display()
        ));
    }
    (config, found, warnings)
}

#[cfg(unix)]
fn readable_by_others(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o077 != 0)
}

#[cfg(not(unix))]
fn readable_by_others(_path: &Path) -> bool {
    false
}
