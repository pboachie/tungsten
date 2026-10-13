// SPDX-License-Identifier: AGPL-3.0-only
//! Postman v2.1 structural importer. Scripts and requests are never executed.
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Value, json};

use crate::*;

#[derive(Clone, Default)]
pub struct PostmanOptions {
    /// Explicit environment overrides; highest precedence, never persisted.
    pub environment: BTreeMap<String, String>,
}

impl std::fmt::Debug for PostmanOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostmanOptions")
            .field("variable_count", &self.environment.len())
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Authentication {
    Unknown,
    Noauth,
    Bearer,
    Basic,
    ApiKey {
        name: String,
        location: ParameterLocation,
    },
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PostmanReport {
    pub authentication: BTreeMap<String, BTreeSet<Authentication>>,
    /// Origin-only variables whose values must be supplied at client setup.
    pub configure_origin_variables: BTreeMap<String, BTreeSet<String>>,
    /// Stable operation ID to source JSON pointers (never item names).
    pub locations: BTreeMap<String, BTreeSet<String>>,
    pub skipped_test_scripts: usize,
    pub disabled_items: usize,
}

#[derive(Debug, Clone)]
pub struct PostmanImport {
    pub evidence: EvidenceDocument,
    pub report: PostmanReport,
}

impl PostmanImport {
    /// Infer shapes through the shared engine, then apply explicit auth helpers.
    /// No credential or captured auth value is copied into the resulting contract.
    pub fn infer(
        &self,
        overrides: &ReviewedOverrides,
        limits: &Limits,
    ) -> Result<ContractOutput, Diagnostic> {
        let mut output = crate::infer(&self.evidence, overrides, limits)?;
        let mut schemes = BTreeMap::new();
        for operation in &self.evidence.operations {
            let id = stable_operation_id(&operation.method, &operation.path);
            let Some(auths) = self.report.authentication.get(&id) else {
                continue;
            };
            if auths.contains(&Authentication::Unknown) && auths.len() > 1 {
                return Err(error(
                    "PM007",
                    "Duplicate operations mix unknown and explicit auth; declare one consistent authentication policy.",
                ));
            }
            let mut security = Vec::new();
            for auth in auths {
                let schema = match auth {
                    Authentication::Unknown => continue,
                    Authentication::Noauth => {
                        security.push(json!({}));
                        continue;
                    }
                    Authentication::Bearer => json!({"type":"http","scheme":"bearer"}),
                    Authentication::Basic => json!({"type":"http","scheme":"basic"}),
                    Authentication::ApiKey { name, location } => {
                        json!({"type":"apiKey","name":name,"in":location})
                    }
                };
                let name = format!(
                    "import_auth_{}",
                    blake3::hash(schema.to_string().as_bytes()).to_hex()
                );
                schemes.insert(name.clone(), schema);
                security.push(json!({name:[]}));
            }
            if !security.is_empty() {
                let method = operation.method.to_ascii_lowercase();
                output.openapi["paths"][&operation.path][&method]["security"] =
                    if auths == &BTreeSet::from([Authentication::Noauth]) {
                        json!([])
                    } else {
                        json!(security)
                    };
                let pointer = format!("/paths/{}/{method}/security", escape(&operation.path));
                output.report.facts.insert(
                    pointer,
                    FactProvenance {
                        states: BTreeSet::from([EvidenceState::Declared]),
                        sources: self
                            .evidence
                            .operations
                            .iter()
                            .filter(|op| stable_operation_id(&op.method, &op.path) == id)
                            .map(|op| op.source.clone())
                            .collect(),
                    },
                );
                if let Some(unknown) = output.report.unknown.get_mut(&id) {
                    unknown.remove(&UnknownFact::Authentication);
                }
            }
        }
        if !schemes.is_empty() {
            output.openapi["components"] = json!({"securitySchemes":schemes});
        }
        Ok(output)
    }
}

fn error(code: &str, message: &str) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        message: message.into(),
    }
}
fn escape(text: &str) -> String {
    text.replace('~', "~0").replace('/', "~1")
}
fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], Diagnostic> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(items)) => Ok(items),
        _ => Err(error(
            "PM002",
            "Expected a collection array; export Postman Collection v2.1 JSON.",
        )),
    }
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, Diagnostic> {
    value.get(key).and_then(Value::as_str).ok_or_else(|| {
        error(
            "PM002",
            "Missing or invalid required collection string; check the v2.1 export.",
        )
    })
}
fn enabled(value: &Value) -> bool {
    value.get("disabled").and_then(Value::as_bool) != Some(true)
}

/// Variables declared by one collection, folder or item, chained to the enclosing
/// scopes. Values borrow from the parsed collection, so nesting never copies them.
struct Scope<'a, 'p> {
    values: BTreeMap<&'a str, Cow<'a, str>>,
    parent: Option<&'p Scope<'a, 'p>>,
}

impl<'a, 'p> Scope<'a, 'p> {
    fn new(value: &'a Value, parent: Option<&'p Scope<'a, 'p>>) -> Result<Self, Diagnostic> {
        let mut values = BTreeMap::new();
        for variable in array(value, "variable")? {
            if !enabled(variable) {
                continue;
            }
            let key = text(variable, "key")?;
            if let Some(value) = variable.get("value") {
                let value = match value {
                    Value::String(v) => Cow::Borrowed(v.as_str()),
                    Value::Bool(_) | Value::Number(_) => Cow::Owned(value.to_string()),
                    _ => {
                        return Err(error(
                            "PM005",
                            "Collection variable values must be scalar strings, booleans or numbers.",
                        ));
                    }
                };
                values.insert(key, value);
            }
        }
        Ok(Self { values, parent })
    }

    fn get(&self, key: &str) -> Option<&str> {
        let mut scope = Some(self);
        while let Some(current) = scope {
            if let Some(value) = current.values.get(key) {
                return Some(value);
            }
            scope = current.parent;
        }
        None
    }
}

/// Template URL path data. Path text becomes persisted contract keys, so a variable
/// value is never substituted there: a whole `{{name}}` segment naming a declared
/// path variable (`url.variable`) becomes the `:name` parameter, any other variable
/// is rejected, and dynamic variables are rejected in every segment (PM004).
fn template_path(path: &str, declared: &BTreeSet<&str>) -> Result<String, Diagnostic> {
    let mut rest = path;
    while let Some(start) = rest.find("{{") {
        rest = &rest[start + 2..];
        if rest.starts_with('$') {
            return Err(error(
                "PM004",
                "Dynamic variables are not executed; replace them with explicit examples or declared parameters.",
            ));
        }
    }
    path.split('/')
        .map(|segment| {
            let name = segment
                .strip_prefix("{{")
                .and_then(|s| s.strip_suffix("}}"))
                .filter(|name| !name.contains(['{', '}']) && declared.contains(name));
            if let Some(name) = name {
                Ok(format!(":{name}"))
            } else if segment.contains("{{") || segment.contains("}}") {
                Err(error(
                    "PM005",
                    "Path variables must be :parameter segments or whole {{name}} segments declared in the URL's variable list; variable values are never copied into contract paths.",
                ))
            } else {
                Ok(segment.to_owned())
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|segments| segments.join("/"))
}

/// Count JSON nodes without recursion.
fn nodes(value: &Value) -> usize {
    let mut pending = vec![value];
    let mut count = 0usize;
    while let Some(value) = pending.pop() {
        count += 1;
        match value {
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => pending.extend(values.values()),
            _ => {}
        }
    }
    count
}

fn events(value: &Value, report: &mut PostmanReport) -> Result<(), Diagnostic> {
    for event in array(value, "event")? {
        if !enabled(event) {
            continue;
        }
        let script = event.get("script").unwrap_or(&Value::Null);
        let nonempty = script.get("src").is_some()
            || script.get("exec").is_some_and(|v| match v {
                Value::String(s) => !s.trim().is_empty(),
                Value::Array(a) => a
                    .iter()
                    .any(|v| v.as_str().is_none_or(|s| !s.trim().is_empty())),
                _ => true,
            });
        if !nonempty {
            continue;
        }
        match event.get("listen").and_then(Value::as_str) {
            Some("test") => report.skipped_test_scripts += 1,
            _ => {
                return Err(error(
                    "PM004",
                    "Pre-request or unknown scripts are not executed; materialize the request without script-dependent behavior before importing.",
                ));
            }
        }
    }
    if value
        .get("protocolProfileBehavior")
        .is_some_and(|v| v.as_object().is_none_or(|o| !o.is_empty()))
    {
        return Err(error(
            "PM004",
            "Custom protocol behavior is unsupported; export a request using standard HTTP semantics.",
        ));
    }
    Ok(())
}

fn shape(value: Value) -> ShapeEvidence {
    ShapeEvidence {
        samples: vec![value],
        declared_schema: None,
    }
}
fn unknown_shape() -> ShapeEvidence {
    ShapeEvidence {
        samples: vec![],
        declared_schema: None,
    }
}

fn budget_error() -> Diagnostic {
    error(
        "PM001",
        "Variable expansion and parsed samples exceed the import-wide byte or node budget; reduce large or repeated variables and examples.",
    )
}

struct Context<'a> {
    options: &'a PostmanOptions,
    limits: &'a Limits,
    output: PostmanImport,
    /// Running totals across the whole import, checked on every expansion and parse.
    expanded_bytes: usize,
    sample_nodes: usize,
}

impl Context<'_> {
    fn resolve<'s>(&'s self, scope: &'s Scope<'_, '_>, key: &str) -> Option<&'s str> {
        self.options
            .environment
            .get(key)
            .map(String::as_str)
            .or_else(|| scope.get(key))
    }

    /// Expand variables, charging the result to one import-wide byte budget so a
    /// large value referenced many times cannot amplify past `Limits::max_bytes`.
    fn expand(&mut self, input: &str, scope: &Scope<'_, '_>) -> Result<String, Diagnostic> {
        let limits = self.limits;
        let mut value = input.to_owned();
        for _ in 0..limits.max_depth {
            let Some(start) = value.find("{{") else {
                return self.charge_bytes(value);
            };
            let end = value[start + 2..]
                .find("}}")
                .map(|n| start + 2 + n)
                .ok_or_else(|| {
                    error(
                        "PM005",
                        "Malformed variable template; use a complete variable reference.",
                    )
                })?;
            let key = &value[start + 2..end];
            if key.starts_with('$') {
                return Err(error(
                    "PM004",
                    "Dynamic variables are not executed; replace them with explicit examples or declared parameters.",
                ));
            }
            let replacement = self.resolve(scope, key).ok_or_else(|| error("PM005","Unresolved variable affects request shape; provide an explicit environment override or replace it with a declared path parameter."))?;
            if self
                .expanded_bytes
                .saturating_add(value.len())
                .saturating_add(replacement.len())
                > limits.max_bytes
            {
                return Err(budget_error());
            }
            let replacement = replacement.to_owned();
            value.replace_range(start..end + 2, &replacement);
        }
        Err(error(
            "PM005",
            "Variable expansion is cyclic or exceeds depth; provide a nonrecursive value.",
        ))
    }

    fn charge_bytes(&mut self, value: String) -> Result<String, Diagnostic> {
        self.expanded_bytes = self.expanded_bytes.saturating_add(value.len());
        if self.expanded_bytes > self.limits.max_bytes {
            return Err(budget_error());
        }
        Ok(value)
    }

    /// Charge parsed sample nodes to the import-wide node budget.
    fn sample(&mut self, value: Value) -> Result<ShapeEvidence, Diagnostic> {
        self.sample_nodes = self.sample_nodes.saturating_add(nodes(&value));
        if self.sample_nodes > self.limits.max_nodes {
            return Err(budget_error());
        }
        Ok(shape(value))
    }

    fn auth(
        &mut self,
        value: Option<&Value>,
        inherited: &Authentication,
        scope: &Scope<'_, '_>,
    ) -> Result<Authentication, Diagnostic> {
        let Some(value) = value.filter(|v| !v.is_null()) else {
            return Ok(inherited.clone());
        };
        Ok(match text(value, "type")? {
            "noauth" => Authentication::Noauth,
            "bearer" => Authentication::Bearer,
            "basic" => Authentication::Basic,
            "apikey" => {
                let fields = array(value, "apikey")?;
                let get = |key| {
                    fields
                        .iter()
                        .find(|f| f["key"] == key)
                        .and_then(|f| f["value"].as_str())
                };
                let name = get("key").ok_or_else(|| error("PM003","API key authentication needs an explicit key name and header/query location."))?;
                let name = self.expand(name, scope)?;
                let location = match get("in") {
                    Some("header") => ParameterLocation::Header,
                    Some("query") => ParameterLocation::Query,
                    _ => {
                        return Err(error(
                            "PM003",
                            "API key authentication supports only explicit header or query locations.",
                        ));
                    }
                };
                Authentication::ApiKey { name, location }
            }
            _ => {
                return Err(error(
                    "PM003",
                    "Unsupported authentication helper; use noauth, bearer, basic or header/query API key, or supply a reviewed OpenAPI security scheme.",
                ));
            }
        })
    }

    fn visit<'a>(
        &mut self,
        parent: &'a Value,
        scope: Option<&Scope<'a, '_>>,
        inherited: &Authentication,
        pointer: &str,
        depth: usize,
    ) -> Result<(), Diagnostic> {
        if depth > self.limits.max_depth {
            return Err(error(
                "PM001",
                "Folder nesting exceeds the limit; flatten the collection.",
            ));
        }
        events(parent, &mut self.output.report)?;
        let scope = Scope::new(parent, scope)?;
        let inherited = self.auth(parent.get("auth"), inherited, &scope)?;
        for (index, item) in array(parent, "item")?.iter().enumerate() {
            if !enabled(item) {
                self.output.report.disabled_items += 1;
                continue;
            }
            let pointer = format!("{pointer}/item/{index}");
            if item.get("item").is_some() {
                if item.get("request").is_some() {
                    return Err(error(
                        "PM002",
                        "An item cannot be both a folder and a request.",
                    ));
                }
                self.visit(item, Some(&scope), &inherited, &pointer, depth + 1)?;
            } else {
                self.request(item, &scope, &inherited, &pointer)
                    .map_err(|mut diagnostic| {
                        diagnostic.message.push_str(&format!(" Source: {pointer}."));
                        diagnostic
                    })?;
            }
        }
        Ok(())
    }

    fn request<'a>(
        &mut self,
        item: &'a Value,
        scope: &Scope<'a, '_>,
        inherited: &Authentication,
        pointer: &str,
    ) -> Result<(), Diagnostic> {
        events(item, &mut self.output.report)?;
        let scope = Scope::new(item, Some(scope))?;
        let request = item.get("request").ok_or_else(|| {
            error(
                "PM002",
                "Collection item has no request; export a valid v2.1 collection.",
            )
        })?;
        if !enabled(request) {
            self.output.report.disabled_items += 1;
            return Ok(());
        }
        if request.get("proxy").is_some_and(enabled) || request.get("certificate").is_some() {
            return Err(error(
                "PM004",
                "Proxy and client-certificate request semantics require an explicit reviewed client configuration.",
            ));
        }
        let authentication = self.auth(request.get("auth"), inherited, &scope)?;
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_ascii_uppercase();
        let url = request
            .as_str()
            .map(Value::from)
            .or_else(|| request.get("url").cloned())
            .ok_or_else(|| {
                error(
                    "PM002",
                    "Request URL is missing; export a raw or structured URL.",
                )
            })?;
        let (path, mut parameters, origin_variables) = self.url(&url, &scope, &authentication)?;
        let id = stable_operation_id(&method, &path);
        let source = SourceLocation {
            document: "postman".into(),
            line: None,
            column: None,
        };
        self.output
            .report
            .locations
            .entry(id.clone())
            .or_default()
            .insert(format!("{pointer}/request"));
        self.output
            .report
            .configure_origin_variables
            .entry(id.clone())
            .or_default()
            .extend(origin_variables);
        self.output
            .report
            .authentication
            .entry(id)
            .or_default()
            .insert(authentication.clone());
        let request_headers = headers(request)?;
        let mut content_type = None;
        for (name, value) in request_headers {
            if name.eq_ignore_ascii_case("content-type") {
                content_type = Some(value.to_ascii_lowercase());
                continue;
            }
            if name.eq_ignore_ascii_case("authorization")
                || name.eq_ignore_ascii_case("cookie")
                || name.eq_ignore_ascii_case("proxy-authorization")
            {
                return Err(error(
                    "PM003",
                    "Captured authentication headers are not persisted; use an explicit supported authentication helper and configure credentials at runtime.",
                ));
            }
            if matches!(&authentication,Authentication::ApiKey {name:key,location:ParameterLocation::Header} if name.eq_ignore_ascii_case(key))
            {
                continue;
            }
            // Header values remain sample evidence and are never persisted.
            parameters.push(ParameterEvidence {
                name,
                location: ParameterLocation::Header,
                shape: {
                    let value = self.expand(&value, &scope)?;
                    self.sample(json!(value))?
                },
            });
        }
        if let Authentication::ApiKey {
            name,
            location: ParameterLocation::Query,
        } = &authentication
        {
            parameters.retain(|p| p.location != ParameterLocation::Query || p.name != *name);
        }
        let mut body = Vec::new();
        if let Some(value) = request.get("body").filter(|v| !v.is_null() && enabled(v)) {
            if value.get("mode").and_then(Value::as_str) != Some("raw") {
                return Err(error(
                    "PM006",
                    "Only raw JSON bodies are supported; convert multipart, form, XML, file or GraphQL requests to a reviewed contract.",
                ));
            }
            let media = content_type
                .as_deref()
                .unwrap_or("application/json")
                .split(';')
                .next()
                .unwrap_or_default()
                .trim();
            if media != "application/json" && !media.ends_with("+json") {
                return Err(error(
                    "PM006",
                    "Only JSON request media are supported; supply a reviewed contract for other encodings.",
                ));
            }
            let raw = self.expand(text(value, "raw")?, &scope)?;
            let parsed = serde_json::from_str(&raw).map_err(|_| error("PM006","Raw body is not valid JSON after variable resolution; provide explicit JSON examples."))?;
            body.push(MediaEvidence {
                media_type: media.into(),
                shape: self.sample(parsed)?,
            });
        }
        let mut responses = Vec::new();
        for response in array(item, "response")? {
            let status = response
                .get("code")
                .and_then(Value::as_u64)
                .filter(|n| (100..=599).contains(n))
                .ok_or_else(|| {
                    error(
                        "PM002",
                        "Saved response needs an HTTP status between 100 and 599.",
                    )
                })? as u16;
            let mut content = Vec::new();
            if let Some(raw) = response
                .get("body")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                let media = headers(response)?
                    .into_iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                    .map(|(_, v)| v)
                    .unwrap_or_else(|| "application/json".into());
                let media = media
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase();
                if media != "application/json" && !media.ends_with("+json") {
                    return Err(error(
                        "PM006",
                        "Saved non-JSON responses are unsupported; provide JSON examples or a reviewed media schema.",
                    ));
                }
                let sample = serde_json::from_str(raw).map_err(|_| error("PM006","Saved response body is not JSON; remove the unsupported example or provide a reviewed media schema."))?;
                content.push(MediaEvidence {
                    media_type: media,
                    shape: self.sample(sample)?,
                });
            }
            responses.push(ResponseEvidence { status, content });
        }
        self.output.evidence.operations.push(HttpEvidence {
            method,
            path,
            source,
            parameters,
            request: body,
            responses,
            unknown: BTreeSet::new(),
        });
        if self.output.evidence.operations.len() > self.limits.max_operations {
            return Err(error(
                "PM001",
                "Collection exceeds operation count limit; split the collection.",
            ));
        }
        Ok(())
    }

    fn url(
        &mut self,
        value: &Value,
        scope: &Scope<'_, '_>,
        authentication: &Authentication,
    ) -> Result<(String, Vec<ParameterEvidence>, BTreeSet<String>), Diagnostic> {
        let mut origins = BTreeSet::new();
        let raw = if let Some(raw) = value.as_str() {
            raw.to_owned()
        } else if let Some(raw) = value.get("raw").and_then(Value::as_str) {
            raw.to_owned()
        } else {
            let protocol = value
                .get("protocol")
                .and_then(Value::as_str)
                .unwrap_or("https");
            let host = components(value.get("host"), ".")?;
            let path = components(value.get("path"), "/")?;
            format!("{protocol}://{host}/{path}")
        };
        let (raw_path, raw_query) = raw
            .split_once('?')
            .map_or((raw.as_str(), None), |(p, q)| (p, Some(q)));
        // Only the origin may use variables. Path text is persisted as contract keys
        // and provenance pointers, so it must be literal or an explicit :parameter.
        let path_start = if raw_path.starts_with("{{") {
            raw_path.find("}}").map(|end| end + 2)
        } else {
            raw_path.split_once("://").map(|(scheme, authority)| {
                scheme.len() + 3 + authority.find('/').unwrap_or(authority.len())
            })
        }
        .ok_or_else(|| {
            error(
                "PM005",
                "Request URL requires an explicit HTTP(S) origin or origin-only base URL variable.",
            )
        })?;
        let literal = &raw_path[path_start..];
        if !literal.is_empty() && !literal.starts_with('/') {
            return Err(error(
                "PM005",
                "A base URL variable must occupy the entire origin before the path.",
            ));
        }
        // Path variables are declared by the structured URL's variable list; their
        // values are never read, so they remain unknown parameter shapes.
        let mut declared = BTreeSet::new();
        for variable in array(value, "variable")? {
            if enabled(variable) {
                declared.insert(text(variable, "key")?);
            }
        }
        let templated = template_path(literal, &declared)?;
        let expected = normalized_path(&templated)?;
        let origin = &raw_path[..path_start];
        let raw = if raw_path.starts_with("{{") {
            let key = &raw_path[2..path_start - 2];
            if key.starts_with('$') {
                return Err(error(
                    "PM004",
                    "Dynamic URL variables are not executed; configure an explicit origin.",
                ));
            }
            if self.resolve(scope, key).is_none() {
                origins.insert(key.into());
                format!("https://import.invalid{templated}")
            } else {
                format!("{}{templated}", self.expand(origin, scope)?)
            }
        } else {
            format!("{}{templated}", self.expand(origin, scope)?)
        };
        let parsed = url::Url::parse(&raw).map_err(|_| error("PM005","Request URL must be HTTP(S), with an explicit path and resolvable structural variables."))?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(error(
                "PM005",
                "Use an HTTP(S) URL without inline credentials or fragment; configure auth through a supported helper.",
            ));
        }
        if parsed.path() != expected {
            return Err(error(
                "PM005",
                "Base URL variables must hold an origin only; move any path prefix into the request path.",
            ));
        }
        if let Some(path) = value.get("path") {
            let path = template_path(&components(Some(path), "/")?, &declared)?;
            if normalized_path(&format!("/{}", path.trim_start_matches('/')))? != expected {
                return Err(error(
                    "PM005",
                    "Structured and raw URL paths disagree; re-export a consistent URL before importing.",
                ));
            }
        }
        let mut parameters = Vec::new();
        let mut segments = Vec::new();
        for segment in parsed.path().split('/') {
            if let Some(name) = segment.strip_prefix(':') {
                if name.is_empty() {
                    return Err(error(
                        "PM005",
                        "Path variables need an explicit nonempty name.",
                    ));
                }
                parameters.push(ParameterEvidence {
                    name: name.into(),
                    location: ParameterLocation::Path,
                    shape: unknown_shape(),
                });
                segments.push(format!("{{{name}}}"));
            } else {
                segments.push(segment.into());
            }
        }
        if let Some(path) = value.get("path") {
            let path = format!("/{}", components(Some(path), "/")?.trim_start_matches('/'));
            if path != parsed.path() {
                return Err(error(
                    "PM005",
                    "Structured and raw URL paths disagree; re-export a consistent URL before importing.",
                ));
            }
        }
        let mut keys = BTreeSet::new();
        if value.get("query").is_some() {
            for query in array(value, "query")? {
                if !enabled(query) {
                    continue;
                }
                let name = text(query, "key")?.to_owned();
                if matches!(authentication,Authentication::ApiKey {name:key,location:ParameterLocation::Query} if *key == name)
                {
                    continue;
                }
                if !keys.insert(name.clone()) {
                    return Err(error(
                        "PM005",
                        "Repeated query names need explicit array serialization in a reviewed endpoint contract.",
                    ));
                }
                let value = query.get("value").and_then(Value::as_str).unwrap_or("");
                parameters.push(ParameterEvidence {
                    name,
                    location: ParameterLocation::Query,
                    shape: {
                        let value = self.expand(value, scope)?;
                        self.sample(json!(value))?
                    },
                });
            }
        } else {
            for (name, value) in
                url::form_urlencoded::parse(raw_query.unwrap_or_default().as_bytes())
            {
                if matches!(authentication,Authentication::ApiKey {name:key,location:ParameterLocation::Query} if *key == name)
                {
                    continue;
                }
                if !keys.insert(name.to_string()) {
                    return Err(error(
                        "PM005",
                        "Repeated query names need explicit array serialization in a reviewed endpoint contract.",
                    ));
                }
                parameters.push(ParameterEvidence {
                    name: name.into_owned(),
                    location: ParameterLocation::Query,
                    shape: {
                        let value = self.expand(&value, scope)?;
                        self.sample(json!(value))?
                    },
                });
            }
        }
        Ok((segments.join("/"), parameters, origins))
    }
}

/// The path as `url` normalizes it, so raw and structured forms compare equally.
fn normalized_path(path: &str) -> Result<String, Diagnostic> {
    url::Url::parse(&format!("https://import.invalid{path}"))
        .map(|url| url.path().to_owned())
        .map_err(|_| {
            error(
                "PM005",
                "Request path must be an explicit literal or :parameter template.",
            )
        })
}

fn components(value: Option<&Value>, separator: &str) -> Result<String, Diagnostic> {
    match value {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|p| {
                p.as_str()
                    .or_else(|| p.get("value").and_then(Value::as_str))
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        error(
                            "PM005",
                            "Unsupported structured URL component; use explicit string components.",
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|parts| parts.join(separator)),
        None => Ok(String::new()),
        _ => Err(error(
            "PM005",
            "Unsupported structured URL; use a raw HTTP(S) URL or string components.",
        )),
    }
}

fn headers(value: &Value) -> Result<Vec<(String, String)>, Diagnostic> {
    let mut out = Vec::new();
    let mut keys = BTreeSet::new();
    for header in array(value, "header")? {
        if !enabled(header) {
            continue;
        }
        let key = text(header, "key")?.to_owned();
        if !keys.insert(key.to_ascii_lowercase()) {
            return Err(error(
                "PM006",
                "Repeated header names require an explicit reviewed serialization contract.",
            ));
        }
        out.push((key, text(header, "value")?.into()));
    }
    Ok(out)
}

pub fn import_collection(
    bytes: &[u8],
    options: &PostmanOptions,
    limits: &Limits,
) -> Result<PostmanImport, Diagnostic> {
    if bytes.len() > limits.max_bytes {
        return Err(error(
            "PM001",
            "Collection exceeds byte limit; split the collection.",
        ));
    }
    let root: Value = serde_json::from_slice(bytes).map_err(|_| {
        error(
            "PM002",
            "Invalid or excessively nested collection JSON; export Postman Collection v2.1.",
        )
    })?;
    let mut pending = vec![(&root, 0usize)];
    let mut nodes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if nodes > limits.max_nodes || depth > limits.max_depth {
            return Err(error(
                "PM001",
                "Collection exceeds node or nesting budget; reduce the collection.",
            ));
        }
        match value {
            Value::Array(values) => pending.extend(values.iter().map(|v| (v, depth + 1))),
            Value::Object(values) => pending.extend(values.values().map(|v| (v, depth + 1))),
            _ => {}
        }
    }
    if options.environment.len() > limits.max_nodes
        || options.environment.iter().fold(0usize, |n, (k, v)| {
            n.saturating_add(k.len()).saturating_add(v.len())
        }) > limits.max_bytes
    {
        return Err(error(
            "PM001",
            "Environment overrides exceed the input budget; provide only needed values.",
        ));
    }
    let schema = root
        .pointer("/info/schema")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if ![
        "https://schema.getpostman.com/json/collection/v2.1.0/collection.json",
        "https://schema.getpostman.com/json/collection/v2.1.0/collection.json/",
        "https://schema.postman.com/collection/json/v2.1.0/draft-07/collection.json",
    ]
    .contains(&schema)
    {
        return Err(error(
            "PM002",
            "Unsupported collection version; export Postman Collection v2.1 JSON.",
        ));
    }
    if root.get("item").and_then(Value::as_array).is_none() {
        return Err(error("PM002", "Collection item array is required."));
    }
    let mut context = Context {
        options,
        limits,
        output: PostmanImport {
            evidence: EvidenceDocument {
                version: EVIDENCE_VERSION,
                frontend: Frontend::Http,
                operations: Vec::new(),
            },
            report: PostmanReport::default(),
        },
        expanded_bytes: 0,
        sample_nodes: 0,
    };
    context.visit(&root, None, &Authentication::Unknown, "", 0)?;
    // Reuse the engine's total bytes/nodes/samples/source/schema validation.
    crate::infer(
        &context.output.evidence,
        &ReviewedOverrides::default(),
        limits,
    )?;
    Ok(context.output)
}
