// SPDX-License-Identifier: Apache-2.0
//! Auth resolution: an operation's security is
//! an OR of AND-sets of scheme names; the first alternative that
//! `ClientOptions::auth` fully satisfies is applied. A composite profile
//! satisfies the scheme names in its `satisfies` list when every part it
//! needs for this request is configured.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use crate::serialize::base64_text;
use crate::types::{
    ApiDescriptor, ApiKeyLocation, AuthConfig, AuthSchemeDescriptor, CompositePart, Credential,
    HttpMethod, OperationDescriptor,
};

/// A boxed future, for the object-safe [`TokenSource`].
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One header of an auth plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanHeader {
    pub name: String,
    pub value: String,
    pub secret: bool,
}

/// Credentials to apply to one request.
#[derive(Debug, Clone, Default)]
pub struct AuthPlan {
    pub headers: Vec<PlanHeader>,
    pub cookies: Vec<(String, String)>,
    pub query: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub enum AuthResolution {
    Ok {
        plan: AuthPlan,
        secrets: Vec<String>,
    },
    Failed {
        remediation: String,
    },
}

/// The client-credentials grant of an OAuth2 scheme.
#[derive(Debug, Clone, Copy)]
pub struct OAuthClient<'a> {
    pub name: &'a str,
    pub token_url: &'a str,
    pub scopes: &'a [String],
    pub client_id: &'a str,
    pub client_secret: &'a str,
}

/// Fetches (and caches) OAuth2 client-credentials tokens. The error is the
/// text shown in the `AUTH_FAILED` envelope.
pub trait TokenSource: Sync {
    fn token<'a>(&'a self, client: OAuthClient<'a>) -> BoxFuture<'a, Result<String, String>>;
}

pub fn is_safe_method(method: HttpMethod) -> bool {
    matches!(
        method,
        HttpMethod::Get | HttpMethod::Head | HttpMethod::Options | HttpMethod::Trace
    )
}

enum Satisfier<'a> {
    Composite {
        name: &'a str,
        parts: &'a [CompositePart],
        values: &'a BTreeMap<String, String>,
        mutation: bool,
    },
    Secret {
        scheme: &'a AuthSchemeDescriptor,
        secret: &'a str,
    },
    OAuth {
        name: &'a str,
        token_url: &'a str,
        scopes: &'a [String],
        client_id: &'a str,
        client_secret: &'a str,
    },
}

enum Attempt<'a> {
    Found(String, Satisfier<'a>),
    Missing(Vec<String>),
}

fn composite_config_key(part: &CompositePart) -> &str {
    match part {
        CompositePart::Cookie { name } => name,
        CompositePart::Bearer { .. } => "bearer",
        CompositePart::Header {
            name,
            equals_cookie,
            from_config,
            ..
        } => equals_cookie
            .as_deref()
            .or(from_config.as_deref())
            .unwrap_or(name),
    }
}

fn check_prefix(token: &str, prefix: Option<&str>, what: &str) -> Option<String> {
    match prefix {
        Some(prefix) if !prefix.is_empty() && !token.starts_with(prefix) => Some(format!(
            "The {what} credential must start with \"{prefix}\"; check that the right kind of token is configured."
        )),
        _ => None,
    }
}

fn skips(part: &CompositePart, mutation: bool) -> bool {
    matches!(
        part,
        CompositePart::Header {
            mutation_only: true,
            ..
        }
    ) && !mutation
}

fn try_composite<'a>(
    name: &'a str,
    parts: &'a [CompositePart],
    config: &'a Credential,
    method: HttpMethod,
) -> Attempt<'a> {
    let Credential::Parts(values) = config else {
        return Attempt::Missing(vec![format!(
            "auth.{name} (an object for the composite profile {name})"
        )]);
    };
    let mutation = !is_safe_method(method);
    let mut missing = Vec::new();
    for part in parts {
        if skips(part, mutation) {
            continue;
        }
        let key = composite_config_key(part);
        if values.get(key).is_none_or(String::is_empty) {
            let role = match part {
                CompositePart::Cookie { name } => format!("cookie {name}"),
                CompositePart::Bearer { .. } => "bearer token".to_owned(),
                CompositePart::Header {
                    name,
                    equals_cookie,
                    ..
                } => format!(
                    "header {name}{}",
                    equals_cookie
                        .as_ref()
                        .map(|c| format!(" (equal to cookie {c})"))
                        .unwrap_or_default()
                ),
            };
            missing.push(format!(
                "auth.{name}[{}] ({role} of the composite profile {name})",
                crate::util::js_string(key)
            ));
        }
    }
    if !missing.is_empty() {
        return Attempt::Missing(missing);
    }
    Attempt::Found(
        format!("composite:{name}"),
        Satisfier::Composite {
            name,
            parts,
            values,
            mutation,
        },
    )
}

fn part<'a>(parts: &'a BTreeMap<String, String>, snake: &str, camel: &str) -> Option<&'a str> {
    parts
        .get(snake)
        .or_else(|| parts.get(camel))
        .map(String::as_str)
}

fn try_direct<'a>(scheme: &'a AuthSchemeDescriptor, config: Option<&'a Credential>) -> Attempt<'a> {
    let name = scheme.name();
    if let AuthSchemeDescriptor::Oauth2 {
        token_url, scopes, ..
    } = scheme
        && let Some(Credential::Parts(parts)) = config
    {
        if let (Some(client_id), Some(client_secret), Some(token_url)) = (
            part(parts, "client_id", "clientId"),
            part(parts, "client_secret", "clientSecret"),
            token_url.as_deref().filter(|u| !u.is_empty()),
        ) {
            return Attempt::Found(
                format!("scheme:{name}"),
                Satisfier::OAuth {
                    name,
                    token_url,
                    scopes,
                    client_id,
                    client_secret,
                },
            );
        }
        return Attempt::Missing(vec![format!(
            "auth.{name} (an access token, or {{client_id, client_secret}} for the token URL)"
        )]);
    }
    match config {
        Some(Credential::Secret(secret)) if !secret.is_empty() => Attempt::Found(
            format!("scheme:{name}"),
            Satisfier::Secret { scheme, secret },
        ),
        _ => {
            let what = match scheme {
                AuthSchemeDescriptor::HttpBasic { .. } => {
                    format!("\"user:password\" for HTTP Basic scheme {name}")
                }
                AuthSchemeDescriptor::ApiKey { location, wire, .. } => {
                    let place = match location {
                        ApiKeyLocation::Header => "header",
                        ApiKeyLocation::Query => "query",
                        ApiKeyLocation::Cookie => "cookie",
                    };
                    format!("API key for scheme {name} ({place} {wire})")
                }
                _ => format!("bearer token for scheme {name}"),
            };
            Attempt::Missing(vec![format!("auth.{name} ({what})")])
        }
    }
}

fn try_scheme_name<'a>(
    api: &'a ApiDescriptor,
    auth: &'a AuthConfig,
    name: &str,
    method: HttpMethod,
) -> Attempt<'a> {
    let mut missing: Vec<String> = Vec::new();
    for scheme in &api.auth {
        if let AuthSchemeDescriptor::Composite {
            name: composite,
            satisfies,
            parts,
        } = scheme
            && satisfies.iter().any(|s| s == name)
        {
            let Some(config) = auth.get(composite) else {
                missing.push(format!("auth.{composite} (composite profile {composite})"));
                continue;
            };
            match try_composite(composite, parts, config, method) {
                found @ Attempt::Found(..) => return found,
                Attempt::Missing(more) => missing.extend(more),
            }
        }
    }
    let direct = api
        .auth
        .iter()
        .find(|s| s.name() == name && !matches!(s, AuthSchemeDescriptor::Composite { .. }));
    if let Some(scheme) = direct {
        match try_direct(scheme, auth.get(name)) {
            found @ Attempt::Found(..) => return found,
            // When a composite profile covers the name, its missing parts
            // explain the gap better than the raw spec scheme.
            Attempt::Missing(more) => {
                if missing.is_empty() {
                    missing.extend(more);
                }
            }
        }
    } else if missing.is_empty() {
        missing.push(format!(
            "a scheme named {name} (the API descriptor does not define it)"
        ));
    }
    Attempt::Missing(missing)
}

/// Whether `auth` has an entry for the scheme `name` or a composite profile
/// satisfying it.
fn configured(api: &ApiDescriptor, auth: &AuthConfig, name: &str) -> bool {
    auth.contains_key(name)
        || api.auth.iter().any(|s| {
            matches!(s, AuthSchemeDescriptor::Composite { name: composite, satisfies, .. }
                if satisfies.iter().any(|n| n == name) && auth.contains_key(composite))
        })
}

fn bearer(plan: &mut AuthPlan, token: &str) {
    plan.headers.push(PlanHeader {
        name: "Authorization".to_owned(),
        value: format!("Bearer {token}"),
        secret: true,
    });
}

async fn apply(
    satisfier: &Satisfier<'_>,
    plan: &mut AuthPlan,
    secrets: &mut Vec<String>,
    tokens: &dyn TokenSource,
) -> Option<String> {
    match satisfier {
        Satisfier::Composite {
            name,
            parts,
            values,
            mutation,
        } => {
            for part in *parts {
                if skips(part, *mutation) {
                    continue;
                }
                let value = values
                    .get(composite_config_key(part))
                    .map(String::as_str)
                    .unwrap_or_default();
                match part {
                    CompositePart::Cookie { name } => {
                        plan.cookies.push((name.clone(), value.to_owned()));
                        secrets.push(value.to_owned());
                    }
                    CompositePart::Bearer { prefix } => {
                        if let Some(problem) =
                            check_prefix(value, prefix.as_deref(), &format!("{name} bearer"))
                        {
                            return Some(problem);
                        }
                        bearer(plan, value);
                        secrets.push(value.to_owned());
                    }
                    CompositePart::Header {
                        name, from_config, ..
                    } => {
                        let secret = from_config.is_none();
                        plan.headers.push(PlanHeader {
                            name: name.clone(),
                            value: value.to_owned(),
                            secret,
                        });
                        if secret {
                            secrets.push(value.to_owned());
                        }
                    }
                }
            }
            None
        }
        Satisfier::Secret { scheme, secret } => {
            secrets.push((*secret).to_owned());
            match scheme {
                AuthSchemeDescriptor::ApiKey { location, wire, .. } => {
                    match location {
                        ApiKeyLocation::Header => plan.headers.push(PlanHeader {
                            name: wire.clone(),
                            value: (*secret).to_owned(),
                            secret: true,
                        }),
                        ApiKeyLocation::Query => {
                            plan.query.push((wire.clone(), (*secret).to_owned()));
                        }
                        ApiKeyLocation::Cookie => {
                            plan.cookies.push((wire.clone(), (*secret).to_owned()));
                        }
                    }
                    None
                }
                AuthSchemeDescriptor::HttpBearer { name, prefix } => {
                    if let Some(problem) = check_prefix(secret, prefix.as_deref(), name) {
                        return Some(problem);
                    }
                    bearer(plan, secret);
                    None
                }
                AuthSchemeDescriptor::HttpBasic { .. } => {
                    let encoded = base64_text(secret);
                    secrets.push(encoded.clone());
                    plan.headers.push(PlanHeader {
                        name: "Authorization".to_owned(),
                        value: format!("Basic {encoded}"),
                        secret: true,
                    });
                    None
                }
                _ => {
                    bearer(plan, secret);
                    None
                }
            }
        }
        Satisfier::OAuth {
            name,
            token_url,
            scopes,
            client_id,
            client_secret,
        } => {
            secrets.push((*client_secret).to_owned());
            let client = OAuthClient {
                name,
                token_url,
                scopes,
                client_id,
                client_secret,
            };
            match tokens.token(client).await {
                Ok(token) => {
                    secrets.push(token.clone());
                    bearer(plan, &token);
                    None
                }
                Err(problem) => Some(problem),
            }
        }
    }
}

/// Resolve the credentials for `op`. Never fails: a problem is an
/// `AuthResolution::Failed` with its remediation.
pub async fn resolve_auth(
    api: &ApiDescriptor,
    op: &OperationDescriptor,
    auth: &AuthConfig,
    method: HttpMethod,
    tokens: &dyn TokenSource,
) -> AuthResolution {
    let security = &op.security;
    if security.is_empty() {
        return AuthResolution::Ok {
            plan: AuthPlan::default(),
            secrets: Vec::new(),
        };
    }
    let mut best: Option<(Vec<String>, bool)> = None;
    // An empty alternative (anonymous access) is the fallback, never preferred
    // over configured credentials.
    let ordered = security
        .iter()
        .filter(|alt| !alt.is_empty())
        .chain(security.iter().filter(|alt| alt.is_empty()));
    for alternative in ordered {
        let mut satisfiers: Vec<(String, Satisfier<'_>)> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        for name in alternative {
            match try_scheme_name(api, auth, name, method) {
                Attempt::Found(key, satisfier) => {
                    match satisfiers.iter_mut().find(|(k, _)| *k == key) {
                        Some(slot) => slot.1 = satisfier,
                        None => satisfiers.push((key, satisfier)),
                    }
                }
                Attempt::Missing(more) => missing.extend(more),
            }
        }
        if missing.is_empty() {
            let mut plan = AuthPlan::default();
            let mut secrets = Vec::new();
            for (_, satisfier) in &satisfiers {
                if let Some(problem) = apply(satisfier, &mut plan, &mut secrets, tokens).await {
                    return AuthResolution::Failed {
                        remediation: format!("{problem} Operation {} was not sent.", op.id),
                    };
                }
            }
            return AuthResolution::Ok { plan, secrets };
        }
        // Report the alternative the caller started to configure, then the one
        // with the fewest missing pieces.
        let mut unique: Vec<String> = Vec::new();
        for item in missing {
            if !unique.contains(&item) {
                unique.push(item);
            }
        }
        let touched = alternative.iter().any(|name| configured(api, auth, name));
        let better = match &best {
            None => true,
            Some((current, current_touched)) => {
                (touched && !current_touched)
                    || (touched == *current_touched && unique.len() < current.len())
            }
        };
        if better {
            best = Some((unique, touched));
        }
    }
    let alternatives = security
        .iter()
        .map(|alt| {
            if alt.is_empty() {
                "no credentials".to_owned()
            } else {
                alt.join(" AND ")
            }
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    let missing_text = best.map_or_else(|| "credentials".to_owned(), |(m, _)| m.join("; "));
    AuthResolution::Failed {
        remediation: format!(
            "Operation {} needs {alternatives}. Missing: {missing_text}. Configure it in ClientOptions.auth and call again; the request was not sent.",
            op.id
        ),
    }
}
