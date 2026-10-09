// SPDX-License-Identifier: AGPL-3.0-only
//! The environment variables the generated server reads (one per secret,
//! never a command-line argument, so secrets stay out of process listings and shell history).
//!
//! `<API>` is the API name and `<SCHEME>` the auth scheme name in
//! SCREAMING_SNAKE_CASE:
//!
//! | Scheme | Variable |
//! |---|---|
//! | HTTP bearer | the profile's `bearer.env` from tungsten.yml, else `<API>_<SCHEME>_TOKEN` |
//! | API key | `<API>_<SCHEME>_KEY` |
//! | HTTP basic | `<API>_<SCHEME>_CREDENTIALS` (`user:password`) |
//! | OAuth 2, OpenID Connect | `<API>_<SCHEME>_TOKEN` (an access token) |
//! | composite profile | `<API>_<SCHEME>_<PART>` per part, `<PART>` being the part's key in the profile's credentials object (cookie name, config key, `bearer`); a bearer part with `env` uses it |
//!
//! plus `<API>_BASE_URL` (the base URL) and `<API>_MCP_MODE` (`discrete` or
//! `progressive`, overriding the manifest's mode). A plain scheme whose
//! name is also a composite profile's is configured through the profile
//! (credentials are keyed by name).

use tungsten_ir::{AuthScheme, CompositePart, Ident, Ir};

/// Where the credentials of one auth scheme come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Credentials {
    /// One secret: `{ scheme: env }`.
    Secret { env: String, what: String },
    /// A composite profile: `(credential key, env, what)` per part.
    Parts(Vec<(String, String, String)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchemeEnv {
    pub scheme: String,
    pub credentials: Credentials,
}

/// `<API>`: the API name in SCREAMING_SNAKE_CASE (`API` when empty).
pub(crate) fn prefix(ir: &Ir) -> String {
    let p = ir.api.name.screaming();
    if p.is_empty() { "API".into() } else { p }
}

fn screaming(name: &str) -> String {
    let s = Ident::new(name).screaming();
    if s.is_empty() { "X".into() } else { s }
}

pub(crate) fn base_url(ir: &Ir) -> String {
    format!("{}_BASE_URL", prefix(ir))
}

pub(crate) fn mode(ir: &Ir) -> String {
    format!("{}_MCP_MODE", prefix(ir))
}

/// The credentials of every scheme, in IR order.
pub(crate) fn schemes(ir: &Ir) -> Vec<SchemeEnv> {
    let api = prefix(ir);
    let composites: Vec<&str> = ir
        .auth
        .iter()
        .filter(|s| matches!(s, AuthScheme::Composite { .. }))
        .map(AuthScheme::name)
        .collect();
    let var = |scheme: &str, suffix: &str| format!("{api}_{}_{suffix}", screaming(scheme));
    let mut out = vec![];
    for scheme in &ir.auth {
        let name = scheme.name().to_string();
        let credentials = match scheme {
            AuthScheme::Composite { parts, .. } => {
                Credentials::Parts(composite_parts(&api, &name, parts))
            }
            _ if composites.contains(&name.as_str()) => continue,
            AuthScheme::HttpBearer { env, prefix, .. } => Credentials::Secret {
                env: env.clone().unwrap_or_else(|| var(&name, "TOKEN")),
                what: match prefix {
                    Some(p) => format!("bearer token (starts with `{p}`)"),
                    None => "bearer token".into(),
                },
            },
            AuthScheme::ApiKey {
                location,
                wire_name,
                ..
            } => Credentials::Secret {
                env: var(&name, "KEY"),
                what: format!(
                    "API key (sent in {} `{wire_name}`)",
                    match location {
                        tungsten_ir::ApiKeyIn::Header => "header",
                        tungsten_ir::ApiKeyIn::Query => "query parameter",
                        tungsten_ir::ApiKeyIn::Cookie => "cookie",
                    }
                ),
            },
            AuthScheme::HttpBasic { .. } => Credentials::Secret {
                env: var(&name, "CREDENTIALS"),
                what: "HTTP basic credentials as `user:password`".into(),
            },
            AuthScheme::OAuth2 { .. } => Credentials::Secret {
                env: var(&name, "TOKEN"),
                what: "OAuth 2 access token".into(),
            },
            AuthScheme::OpenIdConnect { .. } => Credentials::Secret {
                env: var(&name, "TOKEN"),
                what: "OpenID Connect access token (sent as a bearer token)".into(),
            },
        };
        out.push(SchemeEnv {
            scheme: name,
            credentials,
        });
    }
    out
}

/// One entry per credential key of a composite profile, in part order; a
/// header that must equal a cookie reuses the cookie's value.
fn composite_parts(
    api: &str,
    scheme: &str,
    parts: &[CompositePart],
) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = vec![];
    for part in parts {
        let (key, env, what) = match part {
            CompositePart::Cookie { name } => (name.clone(), None, format!("cookie `{name}`")),
            CompositePart::Bearer { env, prefix } => (
                "bearer".to_string(),
                env.clone(),
                match prefix {
                    Some(p) => format!("bearer token (starts with `{p}`)"),
                    None => "bearer token".into(),
                },
            ),
            CompositePart::Header {
                name,
                equals_cookie,
                from_config,
                ..
            } => match (equals_cookie, from_config) {
                (Some(cookie), _) => (cookie.clone(), None, format!("cookie `{cookie}`")),
                (None, Some(key)) => (
                    key.clone(),
                    None,
                    format!("`{key}`, sent as header `{name}`"),
                ),
                (None, None) => (name.clone(), None, format!("header `{name}`")),
            },
        };
        if out.iter().any(|(k, _, _)| *k == key) {
            continue;
        }
        let env = env.unwrap_or_else(|| format!("{api}_{}_{}", screaming(scheme), screaming(&key)));
        out.push((key, env, what));
    }
    out
}
