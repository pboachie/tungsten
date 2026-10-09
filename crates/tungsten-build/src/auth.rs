// SPDX-License-Identifier: AGPL-3.0-only
//! Security schemes, requirements and composite auth profiles
//! (a composite profile combines several schemes that a request needs together).
//!
//! Schemes come from `components/securitySchemes` of every namespace. A
//! name defined identically (ignoring descriptions) in several namespaces
//! is one scheme; a name with different definitions is qualified with its
//! namespace (`sealed.bearerAuth`) everywhere, with TG0401. Composite
//! profiles from `tungsten.yml` are added after the spec schemes; a
//! composite may share its name with a spec scheme it satisfies, and sorts
//! after it.
//!
//! A defined scheme tungsten does not support (`mutualTLS`, HTTP `digest`)
//! is TG0509 and left out of the table. A security requirement (one
//! OR-alternative) that needs it is dropped with a TG0509 warning, since a
//! client cannot satisfy it; when every alternative of a list is dropped
//! the operation keeps no requirement and the warning says so. TG0502 is
//! only for names that no `securitySchemes` entry defines.
//!
//! A document that defines no scheme at all, declares no `security`, and
//! documents a credential header (`x-api-key`, `api-key`, `apikey`) as a
//! parameter gets an inferred `apiKey` header scheme required by every
//! operation (TG0112, info), so the credential is configured once on the
//! client and is not an argument of each call.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use tungsten_config::{AuthProfile, CompositePartConfig};
use tungsten_core::Diagnostic;
use tungsten_ir::{ApiKeyIn, AuthScheme, CompositePart, OAuthFlow, SchemeUse, SecurityRequirement};
use tungsten_openapi::RefTarget;

use crate::NamespaceInput;
use crate::ctx::{Ctx, child, doc_of, pointer, str_of};
use crate::operations::METHODS;

/// Header names that carry an API key when the document declares no
/// security scheme (lowercase).
const CREDENTIAL_HEADERS: [&str; 3] = ["x-api-key", "api-key", "apikey"];

/// Name of an inferred scheme.
const INFERRED_SCHEME: &str = "apiKey";

/// The compiled schemes plus what operations need to resolve requirements
/// and recognize credential parameters.
#[derive(Debug, Default)]
pub(crate) struct AuthTable {
    /// Spec schemes then composites, sorted by name (a composite after a
    /// spec scheme of the same name).
    pub schemes: Vec<AuthScheme>,
    /// (namespace index, spec scheme name) → IR scheme name.
    names: BTreeMap<(usize, String), String>,
    /// Per namespace: apiKey credentials as (location, wire name).
    api_keys: Vec<Vec<(ApiKeyIn, String)>>,
    /// Cookie names supplied by composite profiles.
    composite_cookies: BTreeSet<String>,
    /// Header names (lowercase) supplied by composite profiles.
    composite_headers: BTreeSet<String>,
    /// Per namespace: the document-level `security`, resolved.
    root: Vec<Vec<SecurityRequirement>>,
    /// (namespace index, spec scheme name) of defined but unsupported
    /// schemes.
    unsupported: BTreeSet<(usize, String)>,
}

/// One definition of a scheme name in one namespace.
struct Definition {
    namespace: usize,
    scheme: AuthScheme,
    at: RefTarget,
}

impl AuthTable {
    pub fn build(cx: &mut Ctx<'_>, namespaces: &[NamespaceInput]) -> Self {
        let mut table = AuthTable::default();
        let mut by_name: BTreeMap<String, Vec<Definition>> = BTreeMap::new();
        let mut inferred: BTreeSet<usize> = BTreeSet::new();
        for (ns_index, ns) in namespaces.iter().enumerate() {
            let mut keys = vec![];
            let (mut defs, unsupported) = definitions(cx, ns_index, ns.doc);
            if defs.is_empty()
                && unsupported.is_empty()
                && let Some(def) = infer_api_key(cx, ns_index, ns.doc)
            {
                inferred.insert(ns_index);
                defs.push(def);
            }
            table
                .unsupported
                .extend(unsupported.into_iter().map(|name| (ns_index, name)));
            for def in defs {
                if let AuthScheme::ApiKey {
                    location,
                    wire_name,
                    ..
                } = &def.scheme
                {
                    keys.push((*location, wire_name.clone()));
                }
                by_name
                    .entry(def.scheme.name().to_string())
                    .or_default()
                    .push(def);
            }
            table.api_keys.push(keys);
        }
        for (name, defs) in by_name {
            table.add_definitions(cx, namespaces, &name, defs);
        }
        table.apply_bearer_profiles(cx);
        table.add_composites(cx);
        table
            .schemes
            .sort_by(|a, b| (a.name(), is_composite(a)).cmp(&(b.name(), is_composite(b))));
        for (ns_index, ns) in namespaces.iter().enumerate() {
            let at = RefTarget {
                doc: ns.doc,
                pointer: "/security".into(),
            };
            let root = match cx.get(&at) {
                Some(_) => table.resolve(cx, ns_index, &at),
                None if inferred.contains(&ns_index) => {
                    let scheme = table
                        .names
                        .get(&(ns_index, INFERRED_SCHEME.to_string()))
                        .cloned()
                        .unwrap_or_else(|| INFERRED_SCHEME.to_string());
                    vec![SecurityRequirement {
                        all_of: vec![SchemeUse {
                            scheme,
                            scopes: vec![],
                        }],
                    }]
                }
                None => vec![],
            };
            table.root.push(root);
        }
        table
    }

    fn add_definitions(
        &mut self,
        cx: &mut Ctx<'_>,
        namespaces: &[NamespaceInput],
        name: &str,
        defs: Vec<Definition>,
    ) {
        let first = without_doc(&defs[0].scheme);
        if defs.iter().all(|d| without_doc(&d.scheme) == first) {
            for d in &defs {
                self.names
                    .insert((d.namespace, name.to_string()), name.into());
            }
            let mut defs = defs;
            self.schemes.push(defs.swap_remove(0).scheme);
            return;
        }
        for d in defs {
            let ns = &cx.cfg.inputs[namespaces[d.namespace].config_index].namespace;
            let qualified = format!("{ns}.{name}");
            cx.report(
                Diagnostic::warning(
                    "TG0401",
                    format!(
                        "security scheme `{name}` is defined differently in several namespaces; renamed to `{qualified}`"
                    ),
                ),
                &d.at,
            );
            self.names
                .insert((d.namespace, name.to_string()), qualified.clone());
            self.schemes.push(renamed(d.scheme, qualified));
        }
    }

    /// A non-composite profile with `bearer` configures the HTTP bearer
    /// scheme of the same name (spec or IR name): its required token
    /// `prefix` and the `env` variable the token is read from.
    fn apply_bearer_profiles(&mut self, cx: &Ctx<'_>) {
        for (profile_name, profile) in &cx.cfg.auth_profiles {
            let (None, Some(bearer)) = (&profile.composite, &profile.bearer) else {
                continue;
            };
            let targets: BTreeSet<&String> = self
                .names
                .iter()
                .filter(|((_, spec), ir)| *ir == profile_name || spec == profile_name)
                .map(|(_, ir)| ir)
                .collect();
            for scheme in &mut self.schemes {
                if let AuthScheme::HttpBearer {
                    name, prefix, env, ..
                } = scheme
                    && targets.contains(name)
                {
                    *prefix = bearer.prefix.clone();
                    *env = bearer.env.clone();
                }
            }
        }
    }

    fn add_composites(&mut self, cx: &mut Ctx<'_>) {
        let cfg = cx.cfg;
        for (profile_name, profile) in &cfg.auth_profiles {
            let Some(parts) = &profile.composite else {
                continue;
            };
            let satisfies = self.satisfied(cx, profile_name, profile);
            let parts = parts.iter().map(|p| self.part(p)).collect();
            self.schemes.push(AuthScheme::Composite {
                name: profile_name.clone(),
                satisfies,
                parts,
            });
        }
    }

    /// The IR scheme names a composite profile satisfies. A spec name that
    /// was qualified expands to every qualified variant (namespace order).
    fn satisfied(
        &self,
        cx: &mut Ctx<'_>,
        profile_name: &str,
        profile: &AuthProfile,
    ) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        for (i, wanted) in profile.satisfies.iter().enumerate() {
            let matches: Vec<&String> = self
                .names
                .iter()
                .filter(|((_, spec), ir)| *ir == wanted || spec == wanted)
                .map(|(_, ir)| ir)
                .collect();
            if matches.is_empty() {
                let at = pointer(["auth_profiles", profile_name, "satisfies", &i.to_string()]);
                cx.report_manifest(
                    Diagnostic::error(
                        "TG0503",
                        format!(
                            "auth profile `{profile_name}` satisfies `{wanted}`, which is not a security scheme of any namespace"
                        ),
                    )
                    .with_help("list names from components/securitySchemes of the inputs"),
                    &at,
                );
            }
            for m in matches {
                if !out.contains(m) {
                    out.push(m.clone());
                }
            }
        }
        out
    }

    fn part(&mut self, part: &CompositePartConfig) -> CompositePart {
        match part {
            CompositePartConfig::Cookie { cookie } => {
                self.composite_cookies.insert(cookie.clone());
                CompositePart::Cookie {
                    name: cookie.clone(),
                }
            }
            CompositePartConfig::Header { header } => {
                self.composite_headers
                    .insert(header.name.to_ascii_lowercase());
                CompositePart::Header {
                    name: header.name.clone(),
                    equals_cookie: header.equals_cookie.clone(),
                    from_config: header.from_config.clone(),
                    mutation_only: header.when.as_deref() == Some("mutation"),
                }
            }
            CompositePartConfig::Bearer { bearer } => CompositePart::Bearer {
                env: bearer.env.clone(),
                prefix: bearer.prefix.clone(),
            },
        }
    }

    /// Whether a parameter carries a credential: an apiKey of its
    /// namespace in the same location, or a cookie or header part of a
    /// composite profile. Header names compare case-insensitively.
    pub fn is_credential(&self, ns_index: usize, location: ApiKeyIn, wire: &str) -> bool {
        let same = |a: &str| match location {
            ApiKeyIn::Header => a.eq_ignore_ascii_case(wire),
            ApiKeyIn::Query | ApiKeyIn::Cookie => a == wire,
        };
        let api_key = self
            .api_keys
            .get(ns_index)
            .is_some_and(|keys| keys.iter().any(|(l, n)| *l == location && same(n)));
        api_key
            || match location {
                ApiKeyIn::Cookie => self.composite_cookies.contains(wire),
                ApiKeyIn::Header => self.composite_headers.contains(&wire.to_ascii_lowercase()),
                ApiKeyIn::Query => false,
            }
    }

    /// The requirements of an operation: its own `security` when present
    /// (`[]` means none), otherwise the document's.
    pub fn requirements(
        &self,
        cx: &mut Ctx<'_>,
        ns_index: usize,
        op: &RefTarget,
    ) -> Vec<SecurityRequirement> {
        let at = child(op, "security");
        match cx.get(&at) {
            Some(_) => self.resolve(cx, ns_index, &at),
            None => self.root.get(ns_index).cloned().unwrap_or_default(),
        }
    }

    /// Resolve a Security Requirement array. Unknown scheme names are
    /// TG0502 errors.
    fn resolve(
        &self,
        cx: &mut Ctx<'_>,
        ns_index: usize,
        at: &RefTarget,
    ) -> Vec<SecurityRequirement> {
        let Some(Value::Array(items)) = cx.get(at) else {
            cx.report(
                Diagnostic::warning("TG0508", "`security` must be an array; ignored"),
                at,
            );
            return vec![];
        };
        let mut out = vec![];
        let mut dropped = 0;
        for (i, item) in items.iter().enumerate() {
            let item_at = child(at, &i.to_string());
            let Value::Object(schemes) = item else {
                cx.report(
                    Diagnostic::warning(
                        "TG0508",
                        "a security requirement must be an object; ignored",
                    ),
                    &item_at,
                );
                continue;
            };
            if let Some(name) = schemes
                .keys()
                .find(|n| self.unsupported.contains(&(ns_index, (*n).clone())))
            {
                cx.report(
                    Diagnostic::warning(
                        "TG0509",
                        format!(
                            "security requirement needs unsupported scheme `{name}`; this alternative is dropped"
                        ),
                    ),
                    &child(&item_at, name),
                );
                dropped += 1;
                continue;
            }
            let mut all_of = vec![];
            for (name, scopes) in schemes {
                let scheme = match self.names.get(&(ns_index, name.clone())) {
                    Some(ir) => ir.clone(),
                    None => {
                        cx.report(
                            Diagnostic::error(
                                "TG0502",
                                format!("security requirement names undefined scheme `{name}`"),
                            )
                            .with_help("define it under components/securitySchemes"),
                            &child(&item_at, name),
                        );
                        name.clone()
                    }
                };
                let scopes = scopes
                    .as_array()
                    .map(|s| {
                        s.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                all_of.push(SchemeUse { scheme, scopes });
            }
            out.push(SecurityRequirement { all_of });
        }
        if dropped > 0 && out.is_empty() {
            cx.report(
                Diagnostic::warning(
                    "TG0509",
                    "every security alternative needs an unsupported scheme; no requirement is kept, so a generated client sends no credentials",
                ),
                at,
            );
        }
        out
    }
}

/// Every supported scheme of one entry document, in spec order, and the
/// names of the defined but unsupported ones (TG0509 warnings).
fn definitions(cx: &mut Ctx<'_>, namespace: usize, doc: usize) -> (Vec<Definition>, Vec<String>) {
    let base = RefTarget {
        doc,
        pointer: "/components/securitySchemes".into(),
    };
    let Some(Value::Object(map)) = cx.get(&base) else {
        return (vec![], vec![]);
    };
    let mut out = vec![];
    let mut unsupported = vec![];
    for name in map.keys() {
        let at = child(&base, name);
        let Some((_, value)) = cx.deref_value(&at) else {
            continue;
        };
        match scheme_of(name, value) {
            Ok(scheme) => out.push(Definition {
                namespace,
                scheme,
                at,
            }),
            Err(reason) => {
                cx.report(
                    Diagnostic::warning(
                        "TG0509",
                        format!("security scheme `{name}` is not supported ({reason}); ignored"),
                    ),
                    &at,
                );
                unsupported.push(name.clone());
            }
        }
    }
    (out, unsupported)
}

/// The `apiKey` header scheme of a document that defines no scheme and no
/// root `security` but documents a credential header as a parameter.
/// Reports TG0112 at that parameter.
fn infer_api_key(cx: &mut Ctx<'_>, namespace: usize, doc: usize) -> Option<Definition> {
    let root = RefTarget {
        doc,
        pointer: String::new(),
    };
    if cx.get(&child(&root, "security")).is_some() {
        return None;
    }
    let paths = child(&root, "paths");
    let Some(Value::Object(map)) = cx.get(&paths) else {
        return None;
    };
    let mut found: Option<(String, RefTarget)> = None;
    'scan: for path in map.keys() {
        let Some((item_at, item)) = cx.deref_value(&child(&paths, path)) else {
            continue;
        };
        let owners = std::iter::once(item_at.clone()).chain(
            METHODS
                .iter()
                .filter(|(word, _)| item.get(*word).is_some_and(Value::is_object))
                .map(|(word, _)| child(&item_at, word)),
        );
        for owner in owners {
            let list = child(&owner, "parameters");
            let Some(Value::Array(items)) = cx.get(&list) else {
                continue;
            };
            for i in 0..items.len() {
                let Some((at, param)) = cx.deref_value(&child(&list, &i.to_string())) else {
                    continue;
                };
                let name = str_of(param, "name").unwrap_or("");
                if str_of(param, "in") == Some("header")
                    && CREDENTIAL_HEADERS.contains(&name.to_ascii_lowercase().as_str())
                {
                    found = Some((name.to_string(), at));
                    break 'scan;
                }
            }
        }
    }
    let (wire_name, at) = found?;
    cx.report(
        Diagnostic::info(
            "TG0112",
            format!(
                "no security scheme is defined, but header `{wire_name}` is documented as a parameter; inferred an apiKey header scheme `{INFERRED_SCHEME}` required by every operation"
            ),
        )
        .with_help("define the scheme under components/securitySchemes and list it in `security`"),
        &at,
    );
    Some(Definition {
        namespace,
        scheme: AuthScheme::ApiKey {
            name: INFERRED_SCHEME.to_string(),
            location: ApiKeyIn::Header,
            wire_name,
            doc: None,
        },
        at,
    })
}

/// Convert one Security Scheme Object.
fn scheme_of(name: &str, v: &Value) -> Result<AuthScheme, String> {
    let name = name.to_string();
    let doc = doc_of(v);
    match str_of(v, "type") {
        Some("apiKey") => {
            let location = match str_of(v, "in") {
                Some("header") => ApiKeyIn::Header,
                Some("query") => ApiKeyIn::Query,
                Some("cookie") => ApiKeyIn::Cookie,
                _ => return Err("apiKey needs `in`: header, query or cookie".into()),
            };
            let wire_name = str_of(v, "name").ok_or("apiKey needs `name`")?.to_string();
            Ok(AuthScheme::ApiKey {
                name,
                location,
                wire_name,
                doc,
            })
        }
        Some("http") => {
            let scheme = str_of(v, "scheme").unwrap_or("").to_ascii_lowercase();
            match scheme.as_str() {
                "bearer" => Ok(AuthScheme::HttpBearer {
                    name,
                    format: str_of(v, "bearerFormat").map(str::to_string),
                    doc,
                    prefix: None,
                    env: None,
                }),
                "basic" => Ok(AuthScheme::HttpBasic { name, doc }),
                other => Err(format!("http scheme `{other}`")),
            }
        }
        Some("oauth2") => {
            let flows = v
                .get("flows")
                .and_then(Value::as_object)
                .map(|flows| flows.iter().map(|(kind, f)| flow(kind, f)).collect())
                .unwrap_or_default();
            Ok(AuthScheme::OAuth2 { name, flows, doc })
        }
        Some("openIdConnect") => {
            let url = str_of(v, "openIdConnectUrl")
                .ok_or("openIdConnect needs `openIdConnectUrl`")?
                .to_string();
            Ok(AuthScheme::OpenIdConnect { name, url, doc })
        }
        Some(other) => Err(format!("type `{other}`")),
        None => Err("no `type`".into()),
    }
}

fn flow(kind: &str, f: &Value) -> OAuthFlow {
    let url = |k: &str| str_of(f, k).map(str::to_string);
    OAuthFlow {
        kind: kind.to_string(),
        token_url: url("tokenUrl"),
        authorization_url: url("authorizationUrl"),
        refresh_url: url("refreshUrl"),
        scopes: f
            .get("scopes")
            .and_then(Value::as_object)
            .map(|s| {
                s.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn is_composite(s: &AuthScheme) -> bool {
    matches!(s, AuthScheme::Composite { .. })
}

/// The scheme with its documentation removed, for comparing definitions.
fn without_doc(s: &AuthScheme) -> AuthScheme {
    let mut s = s.clone();
    match &mut s {
        AuthScheme::ApiKey { doc, .. }
        | AuthScheme::HttpBearer { doc, .. }
        | AuthScheme::HttpBasic { doc, .. }
        | AuthScheme::OAuth2 { doc, .. }
        | AuthScheme::OpenIdConnect { doc, .. } => *doc = None,
        AuthScheme::Composite { .. } => {}
    }
    s
}

fn renamed(mut s: AuthScheme, new_name: String) -> AuthScheme {
    match &mut s {
        AuthScheme::ApiKey { name, .. }
        | AuthScheme::HttpBearer { name, .. }
        | AuthScheme::HttpBasic { name, .. }
        | AuthScheme::OAuth2 { name, .. }
        | AuthScheme::OpenIdConnect { name, .. }
        | AuthScheme::Composite { name, .. } => *name = new_name,
    }
    s
}
