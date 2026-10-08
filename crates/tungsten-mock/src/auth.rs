// SPDX-License-Identifier: AGPL-3.0-only
//! Credential checks for an operation's security requirements: presence,
//! shape, and the required prefix of bearer tokens.

use tungsten_ir::{ApiKeyIn, AuthScheme, CompositePart, Operation, SchemeUse};

use crate::model::Model;
use crate::params::RequestView;

/// Why no security alternative was satisfied.
#[derive(Debug)]
pub(crate) enum AuthFailure {
    /// No credential of any alternative was sent, or a credential was
    /// rejected (a bearer token without its required prefix) (401).
    Unauthenticated(String),
    /// Some credentials were sent, but no alternative is complete and
    /// correct (403).
    Forbidden(String),
}

/// One credential an alternative needs.
#[derive(Debug)]
enum Element {
    Present,
    Absent(String),
    /// Sent in the wrong shape, or not matching another part (403).
    Invalid(String),
    /// Sent in the right shape but not a credential the API accepts: a
    /// bearer token without the profile's required prefix (401, as the API
    /// answers an unknown token).
    Rejected(String),
}

pub(crate) fn check(
    model: &Model,
    op: &Operation,
    req: &RequestView<'_>,
) -> Result<(), AuthFailure> {
    if op.security.is_empty() {
        return Ok(());
    }
    let mut failures = vec![];
    for requirement in &op.security {
        match alternative(model, &requirement.all_of, req) {
            Ok(()) => return Ok(()),
            Err(failure) => failures.push(failure),
        }
    }
    let forbidden = failures
        .iter()
        .position(|f| matches!(f, AuthFailure::Forbidden(_)));
    let index = forbidden.unwrap_or(0);
    Err(failures.swap_remove(index))
}

fn alternative(
    model: &Model,
    all_of: &[SchemeUse],
    req: &RequestView<'_>,
) -> Result<(), AuthFailure> {
    let mut elements = vec![];
    let mut composites: Vec<&str> = vec![];
    for scheme_use in all_of {
        let composite = model.ir.auth.iter().find_map(|s| match s {
            AuthScheme::Composite {
                name,
                satisfies,
                parts,
            } if satisfies.contains(&scheme_use.scheme) => Some((name.as_str(), parts)),
            _ => None,
        });
        if let Some((name, parts)) = composite {
            if !composites.contains(&name) {
                composites.push(name);
                elements.extend(composite_elements(parts, req));
            }
            continue;
        }
        let scheme =
            model.ir.auth.iter().find(|s| {
                s.name() == scheme_use.scheme && !matches!(s, AuthScheme::Composite { .. })
            });
        if let Some(scheme) = scheme {
            elements.push(scheme_element(scheme, req));
        }
    }
    if elements.iter().all(|e| matches!(e, Element::Present)) {
        return Ok(());
    }
    if let Some(Element::Invalid(reason)) =
        elements.iter().find(|e| matches!(e, Element::Invalid(_)))
    {
        return Err(AuthFailure::Forbidden(reason.clone()));
    }
    if let Some(Element::Rejected(reason)) =
        elements.iter().find(|e| matches!(e, Element::Rejected(_)))
    {
        return Err(AuthFailure::Unauthenticated(reason.clone()));
    }
    let first_absent = elements
        .iter()
        .find_map(|e| match e {
            Element::Absent(reason) => Some(reason.clone()),
            _ => None,
        })
        .unwrap_or_default();
    if elements.iter().all(|e| matches!(e, Element::Absent(_))) {
        Err(AuthFailure::Unauthenticated(first_absent))
    } else {
        Err(AuthFailure::Forbidden(first_absent))
    }
}

fn scheme_element(scheme: &AuthScheme, req: &RequestView<'_>) -> Element {
    match scheme {
        AuthScheme::ApiKey {
            location,
            wire_name,
            ..
        } => {
            let present = match location {
                ApiKeyIn::Header => req.header(wire_name).is_some_and(|v| !v.is_empty()),
                ApiKeyIn::Query => !req.query_values(wire_name).is_empty(),
                ApiKeyIn::Cookie => req.cookie(wire_name).is_some(),
            };
            if present {
                Element::Present
            } else {
                let place = match location {
                    ApiKeyIn::Header => "header",
                    ApiKeyIn::Query => "query parameter",
                    ApiKeyIn::Cookie => "cookie",
                };
                Element::Absent(format!("missing {place} `{wire_name}`"))
            }
        }
        AuthScheme::HttpBasic { .. } => authorization(req, "Basic"),
        AuthScheme::HttpBearer { prefix, .. } => bearer(req, prefix.as_deref()),
        AuthScheme::OAuth2 { .. }
        | AuthScheme::OpenIdConnect { .. }
        | AuthScheme::Composite { .. } => authorization(req, "Bearer"),
    }
}

fn composite_elements(parts: &[CompositePart], req: &RequestView<'_>) -> Vec<Element> {
    let mut out = vec![];
    for part in parts {
        match part {
            CompositePart::Cookie { name } => out.push(match req.cookie(name) {
                Some(_) => Element::Present,
                None => Element::Absent(format!("missing cookie `{name}`")),
            }),
            CompositePart::Header {
                name,
                equals_cookie,
                mutation_only,
                ..
            } => {
                if *mutation_only && req.is_safe_method() {
                    continue;
                }
                out.push(match (req.header(name), equals_cookie) {
                    (None, _) => Element::Absent(format!("missing header `{name}`")),
                    (Some(value), Some(cookie)) => {
                        if req.cookie(cookie) == Some(value.as_str()) {
                            Element::Present
                        } else {
                            Element::Invalid(format!(
                                "header `{name}` does not equal cookie `{cookie}`"
                            ))
                        }
                    }
                    (Some(_), None) => Element::Present,
                });
            }
            CompositePart::Bearer { prefix, .. } => out.push(bearer(req, prefix.as_deref())),
        }
    }
    out
}

/// `Authorization: Bearer <token>` whose token starts with `prefix` when
/// the profile requires one (`HttpBearer.prefix`, `CompositePart::Bearer`).
fn bearer(req: &RequestView<'_>, prefix: Option<&str>) -> Element {
    match credentials(req, "Bearer") {
        Ok(token) => match prefix {
            Some(prefix) if !prefix.is_empty() && !token.starts_with(prefix) => {
                Element::Rejected(format!("bearer token does not start with `{prefix}`"))
            }
            _ => Element::Present,
        },
        Err(element) => element,
    }
}

/// `Authorization: <scheme> <credentials>` with non-empty credentials.
fn authorization(req: &RequestView<'_>, scheme: &str) -> Element {
    match credentials(req, scheme) {
        Ok(_) => Element::Present,
        Err(element) => element,
    }
}

/// The credentials of `Authorization: <scheme> <credentials>`, trimmed and
/// non-empty, or the element saying why there are none.
fn credentials(req: &RequestView<'_>, scheme: &str) -> Result<String, Element> {
    match req.header("authorization") {
        None => Err(Element::Absent(format!(
            "missing `Authorization: {scheme}` credentials"
        ))),
        Some(value) => match value.trim().split_once(' ') {
            Some((given, credentials))
                if given.eq_ignore_ascii_case(scheme) && !credentials.trim().is_empty() =>
            {
                Ok(credentials.trim().to_string())
            }
            _ => Err(Element::Invalid(format!(
                "Authorization is not `{scheme}` credentials"
            ))),
        },
    }
}
