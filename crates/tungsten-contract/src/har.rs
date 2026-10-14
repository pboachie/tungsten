// SPDX-License-Identifier: AGPL-3.0-only
//! Offline, bounded HAR 1.2 observations. No requests, decoding or scripts run.
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Value, json};

use crate::*;

#[derive(Clone, Default)]
pub struct HarOptions {
    /// Required for a multi-origin capture, canonical exact HTTP(S) origin.
    pub origin: Option<String>,
    /// Literal path prefixes; exclusions win. Empty includes all selected paths.
    pub include_prefixes: Vec<String>,
    pub exclude_prefixes: Vec<String>,
    /// Explicitly reviewed literal path -> path template mapping.
    pub path_templates: BTreeMap<String, String>,
    /// Reviewed route templates such as `/items/{id}`. A literal path without an
    /// explicit `path_templates` entry uses the most literal matching template; a
    /// tie or a template that matches no imported entry is an error.
    pub route_templates: Vec<String>,
    /// Additional case-insensitive names to omit; built-in credentials stay omitted.
    pub redact_headers: BTreeSet<String>,
    pub redact_query: BTreeSet<String>,
    pub redact_properties: BTreeSet<String>,
    /// Optional recursive allowlist for property names, useful for dynamic keys.
    pub allowed_properties: Option<BTreeSet<String>>,
}

impl std::fmt::Debug for HarOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarOptions")
            .field("origin_selected", &self.origin.is_some())
            .field("path_mapping_count", &self.path_templates.len())
            .field("route_template_count", &self.route_templates.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CaptureNote {
    PartialCapture,
    LiteralPathNeedsReview,
    ReviewedPathTemplate,
    MissingRequestBody,
    MissingResponseBody,
    EncodedBodyUnknown,
    TruncatedBodyUnknown,
    NonJsonBodyUnknown,
    MalformedJsonBodyUnknown,
    RedactedHeaders,
    RedactedQuery,
    RedactedCookies,
    RedactedProperties,
    RepeatedParameterUnknown,
    ResponseHeadersUnknown,
    ConflictingShapes,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct HarReport {
    pub version: u32,
    /// The origin itself is not persisted in reports or generated contracts.
    pub selected_origin_digest: String,
    pub imported_entries: usize,
    pub filtered_entries: usize,
    pub observations: BTreeMap<String, BTreeSet<CaptureNote>>,
}

#[derive(Debug, Clone)]
pub struct HarImport {
    pub evidence: EvidenceDocument,
    pub report: HarReport,
}

#[derive(Debug, Clone, Serialize)]
pub struct HarOutput {
    pub contract: ContractOutput,
    pub report: HarReport,
}

impl HarImport {
    pub fn infer(
        &self,
        overrides: &ReviewedOverrides,
        limits: &Limits,
    ) -> Result<HarOutput, Diagnostic> {
        let contract = crate::infer(&self.evidence, overrides, limits)?;
        let mut report = self.report.clone();
        for operation in &self.evidence.operations {
            let id = stable_operation_id(&operation.method, &operation.path);
            let value =
                &contract.openapi["paths"][&operation.path][operation.method.to_ascii_lowercase()];
            if has_conflict(value) {
                report
                    .observations
                    .entry(id)
                    .or_default()
                    .insert(CaptureNote::ConflictingShapes);
            }
        }
        Ok(HarOutput { contract, report })
    }
}

fn has_conflict(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.get("anyOf")
                .and_then(Value::as_array)
                .is_some_and(|a| a.len() > 1)
                || map.values().any(has_conflict)
        }
        Value::Array(values) => values.iter().any(has_conflict),
        _ => false,
    }
}

fn error(code: &str, message: &str) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        message: message.into(),
    }
}
fn unknown() -> ShapeEvidence {
    ShapeEvidence {
        samples: vec![],
        declared_schema: None,
    }
}
fn samples(value: Value) -> ShapeEvidence {
    ShapeEvidence {
        samples: vec![value],
        declared_schema: None,
    }
}
fn items<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], Diagnostic> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(v)) => Ok(v),
        _ => Err(error(
            "HR002",
            "Expected a HAR array; export a valid HAR 1.2 capture.",
        )),
    }
}

fn sensitive(name: &str, additional: &BTreeSet<String>) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "authorization",
        "proxy-authorization",
        "cookie",
        "set-cookie",
        "token",
        "access_token",
        "refresh_token",
        "api_key",
        "apikey",
        "x-api-key",
        "password",
        "secret",
        "client_secret",
        "session",
        "sessionid",
        "auth",
        "code",
    ]
    .contains(&name.as_str())
        || additional.iter().any(|n| n.eq_ignore_ascii_case(&name))
}

fn redact(value: &mut Value, options: &HarOptions, notes: &mut BTreeSet<CaptureNote>) {
    match value {
        Value::Object(map) => {
            map.retain(|name, _| {
                let keep = !sensitive(name, &options.redact_properties)
                    && options
                        .allowed_properties
                        .as_ref()
                        .is_none_or(|allowed| allowed.contains(name));
                if !keep {
                    notes.insert(CaptureNote::RedactedProperties);
                }
                keep
            });
            for value in map.values_mut() {
                redact(value, options, notes);
            }
        }
        Value::Array(values) => {
            for value in values {
                redact(value, options, notes);
            }
        }
        _ => {}
    }
}

fn body(
    value: Option<&Value>,
    request: bool,
    options: &HarOptions,
    limits: &Limits,
    notes: &mut BTreeSet<CaptureNote>,
) -> Result<Vec<MediaEvidence>, Diagnostic> {
    let missing = if request {
        CaptureNote::MissingRequestBody
    } else {
        CaptureNote::MissingResponseBody
    };
    let Some(value) = value else {
        notes.insert(missing);
        return Ok(vec![]);
    };
    let media = value
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("application/octet-stream")
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if !media.contains('/') || media.chars().any(char::is_whitespace) {
        return Err(error(
            "HR002",
            "Invalid HAR media type; supply a valid type/subtype or remove the malformed entry.",
        ));
    }
    let shape = if value
        .get("encoding")
        .is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
    {
        // A zero decoding budget avoids decompression bombs and accidental binary
        // interpretation. Unknown evidence prevents other samples narrowing it.
        notes.insert(CaptureNote::EncodedBodyUnknown);
        unknown()
    } else if let Some(text) = value.get("text").and_then(Value::as_str) {
        if text.len() > limits.max_bytes
            || value
                .get("size")
                .and_then(Value::as_u64)
                .is_some_and(|size| size > text.len() as u64)
        {
            notes.insert(CaptureNote::TruncatedBodyUnknown);
            unknown()
        } else if media != "application/json" && !media.ends_with("+json") {
            notes.insert(CaptureNote::NonJsonBodyUnknown);
            unknown()
        } else {
            match serde_json::from_str::<Value>(text) {
                Ok(mut sample) => {
                    bounded(&sample, limits)?;
                    redact(&mut sample, options, notes);
                    samples(sample)
                }
                Err(_) => {
                    notes.insert(CaptureNote::MalformedJsonBodyUnknown);
                    unknown()
                }
            }
        }
    } else {
        notes.insert(missing);
        unknown()
    };
    Ok(vec![MediaEvidence {
        media_type: media,
        shape,
    }])
}

fn bounded(value: &Value, limits: &Limits) -> Result<(), Diagnostic> {
    let mut pending = vec![(value, 0usize)];
    let mut nodes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if nodes > limits.max_nodes || depth > limits.max_depth {
            return Err(error(
                "HR001",
                "HAR exceeds nesting or node budget; reduce the capture.",
            ));
        }
        match value {
            Value::Array(values) => pending.extend(values.iter().map(|v| (v, depth + 1))),
            Value::Object(values) => pending.extend(values.values().map(|v| (v, depth + 1))),
            _ => {}
        }
    }
    Ok(())
}

/// Parse an entry URL. Non-HTTP(S) entries (WebSocket, extension or data URLs that
/// browsers record) are `None`: they are filtered, never an API candidate.
fn parse_url(entry: &Value) -> Result<Option<url::Url>, Diagnostic> {
    let raw = entry
        .pointer("/request/url")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            error(
                "HR002",
                "HAR request URL is missing; export a complete request entry.",
            )
        })?;
    let url = url::Url::parse(raw).map_err(|_| {
        error(
            "HR002",
            "HAR request URL is invalid; provide an absolute HTTP(S) URL.",
        )
    })?;
    Ok(matches!(url.scheme(), "http" | "https").then_some(url))
}

/// Validate a selected entry URL after origin and prefix filtering.
fn selected_url(url: &url::Url) -> Result<(), Diagnostic> {
    if url.host_str().is_none() || url.fragment().is_some() {
        return Err(error(
            "HR002",
            "HAR supports HTTP(S) requests without fragments; exclude unsupported entries.",
        ));
    }
    Ok(())
}

/// The most literal reviewed route template matching `path`, if any.
fn route_template<'a>(path: &str, routes: &'a [String]) -> Result<Option<&'a String>, Diagnostic> {
    let parts: Vec<_> = path.split('/').collect();
    let mut best: Option<(usize, &String)> = None;
    let mut tied = false;
    for route in routes {
        let segments: Vec<_> = route.split('/').collect();
        if segments.len() != parts.len() {
            continue;
        }
        let mut literal = 0;
        let matches = parts.iter().zip(&segments).all(|(part, segment)| {
            if segment.starts_with('{') && segment.ends_with('}') && segment.len() > 2 {
                !part.is_empty()
            } else {
                literal += 1;
                part == segment
            }
        });
        if !matches {
            continue;
        }
        match best {
            Some((count, prior)) if count == literal && prior != route => tied = true,
            Some((count, _)) if count >= literal => {}
            _ => {
                best = Some((literal, route));
                tied = false;
            }
        }
    }
    if tied {
        return Err(error(
            "HR006",
            "A captured path matches several equally specific route templates; make the reviewed routes unambiguous.",
        ));
    }
    Ok(best.map(|(_, route)| route))
}

fn reviewed_path(
    path: &str,
    options: &HarOptions,
    notes: &mut BTreeSet<CaptureNote>,
    used_routes: &mut BTreeSet<String>,
) -> Result<(String, Vec<ParameterEvidence>), Diagnostic> {
    let template = match options.path_templates.get(path) {
        Some(template) => template,
        None => match route_template(path, &options.route_templates)? {
            Some(template) => {
                used_routes.insert(template.clone());
                template
            }
            None => {
                notes.insert(CaptureNote::LiteralPathNeedsReview);
                return Ok((path.into(), vec![]));
            }
        },
    };
    let parts: Vec<_> = path.split('/').collect();
    let templates: Vec<_> = template.split('/').collect();
    if parts.len() != templates.len() {
        return Err(error(
            "HR006",
            "Reviewed path templates must preserve segment count and literal segments.",
        ));
    }
    let mut parameters = Vec::new();
    for (part, template) in parts.iter().zip(templates) {
        if template.starts_with('{') && template.ends_with('}') && template.len() > 2 {
            parameters.push(ParameterEvidence {
                name: template[1..template.len() - 1].into(),
                location: ParameterLocation::Path,
                shape: unknown(),
            });
        } else if *part != template {
            return Err(error(
                "HR006",
                "Reviewed path templates may replace whole parameter segments only; preserve literal routing.",
            ));
        }
    }
    notes.insert(CaptureNote::ReviewedPathTemplate);
    Ok((template.clone(), parameters))
}

pub fn import_har(
    bytes: &[u8],
    options: &HarOptions,
    limits: &Limits,
) -> Result<HarImport, Diagnostic> {
    if bytes.len() > limits.max_bytes {
        return Err(error(
            "HR001",
            "HAR exceeds capture byte limit; export a smaller capture.",
        ));
    }
    let root: Value = serde_json::from_slice(bytes)
        .map_err(|_| error("HR002", "Invalid HAR JSON; export a HAR 1.2 capture."))?;
    bounded(&root, limits)?;
    let log = root
        .get("log")
        .ok_or_else(|| error("HR002", "HAR log is missing; export HAR 1.2."))?;
    if log.get("version").and_then(Value::as_str) != Some("1.2") {
        return Err(error("HR002", "Unsupported HAR version; export HAR 1.2."));
    }
    let entries = log
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| error("HR002", "HAR entries array is missing."))?;
    if entries.len() > limits.max_operations {
        return Err(error(
            "HR001",
            "HAR entry count exceeds limit; filter or split the capture.",
        ));
    }
    let urls = entries
        .iter()
        .map(parse_url)
        .collect::<Result<Vec<_>, _>>()?;
    let origins: BTreeSet<_> = urls
        .iter()
        .flatten()
        .map(|url| url.origin().ascii_serialization())
        .collect();
    let selected = if let Some(origin) = &options.origin {
        if !origins.contains(origin) {
            return Err(error(
                "HR003",
                "Selected origin is absent or noncanonical; supply one exact HTTP(S) origin from the capture.",
            ));
        }
        origin.clone()
    } else if origins.len() == 1 {
        origins.first().cloned().unwrap_or_default()
    } else if origins.is_empty() {
        return Err(error(
            "HR002",
            "Capture has no entries; export observed API requests.",
        ));
    } else {
        return Err(error(
            "HR003",
            "Capture has multiple origins; explicitly select one exact HTTP(S) API origin before import.",
        ));
    };
    let mut output = HarImport {
        evidence: EvidenceDocument {
            version: EVIDENCE_VERSION,
            frontend: Frontend::Http,
            operations: vec![],
        },
        report: HarReport {
            version: 1,
            selected_origin_digest: blake3::hash(selected.as_bytes()).to_hex().to_string(),
            ..Default::default()
        },
    };
    let mut used_routes = BTreeSet::new();
    for (entry, url) in entries.iter().zip(urls) {
        let Some(url) = url else {
            output.report.filtered_entries += 1;
            continue;
        };
        if url.origin().ascii_serialization() != selected
            || (!options.include_prefixes.is_empty()
                && !options
                    .include_prefixes
                    .iter()
                    .any(|p| url.path().starts_with(p)))
            || options
                .exclude_prefixes
                .iter()
                .any(|p| url.path().starts_with(p))
        {
            output.report.filtered_entries += 1;
            continue;
        }
        selected_url(&url)?;
        let request = entry
            .get("request")
            .ok_or_else(|| error("HR002", "HAR request is missing."))?;
        let response = entry.get("response").ok_or_else(|| {
            error(
                "HR002",
                "HAR response is missing; exclude incomplete request entries.",
            )
        })?;
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| error("HR002", "HAR request method is missing."))?
            .to_ascii_uppercase();
        let mut notes = BTreeSet::from([CaptureNote::PartialCapture]);
        let (path, mut parameters) =
            reviewed_path(url.path(), options, &mut notes, &mut used_routes)?;
        let id = stable_operation_id(&method, &path);
        let mut seen = BTreeSet::new();
        for header in items(request, "headers")? {
            let name = header
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| error("HR002", "HAR header name is missing."))?
                .to_ascii_lowercase();
            if sensitive(&name, &options.redact_headers) {
                notes.insert(CaptureNote::RedactedHeaders);
                continue;
            }
            if ["host", "content-length", "content-type"].contains(&name.as_str()) {
                continue;
            }
            if !seen.insert(name.clone()) {
                notes.insert(CaptureNote::RepeatedParameterUnknown);
                if let Some(parameter) = parameters
                    .iter_mut()
                    .find(|p| p.location == ParameterLocation::Header && p.name == name)
                {
                    parameter.shape = unknown();
                }
                continue;
            }
            parameters.push(ParameterEvidence {
                name,
                location: ParameterLocation::Header,
                shape: samples(json!("")),
            });
        }
        if !items(request, "cookies")?.is_empty()
            || !items(response, "cookies")?.is_empty()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            notes.insert(CaptureNote::RedactedCookies);
        }
        let mut query: BTreeMap<String, usize> = BTreeMap::new();
        // The URL defines the actual wire query; a duplicate HAR queryString
        // listing must not count the same field twice.
        for (name, _) in url.query_pairs() {
            *query.entry(name.into_owned()).or_default() += 1;
        }
        if query.is_empty() {
            for parameter in items(request, "queryString")? {
                let name = parameter
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| error("HR002", "HAR query parameter name is missing."))?;
                *query.entry(name.into()).or_default() += 1;
            }
        }
        for (name, count) in query {
            if sensitive(&name, &options.redact_query) {
                notes.insert(CaptureNote::RedactedQuery);
                continue;
            }
            let shape = if count > 1 {
                notes.insert(CaptureNote::RepeatedParameterUnknown);
                unknown()
            } else {
                samples(json!(""))
            };
            parameters.push(ParameterEvidence {
                name,
                location: ParameterLocation::Query,
                shape,
            });
        }
        let request_body = body(request.get("postData"), true, options, limits, &mut notes)?;
        let status=response.get("status").and_then(Value::as_u64).filter(|s|(100..=599).contains(s)).ok_or_else(||error("HR002","HAR response has no completed HTTP status; exclude failed or pending requests."))? as u16;
        let response_body = body(response.get("content"), false, options, limits, &mut notes)?;
        for header in items(response, "headers")? {
            let name = header
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            notes.insert(if sensitive(name, &options.redact_headers) {
                CaptureNote::RedactedHeaders
            } else {
                CaptureNote::ResponseHeadersUnknown
            });
        }
        output
            .report
            .observations
            .entry(id)
            .or_default()
            .extend(notes);
        output.report.imported_entries += 1;
        output.evidence.operations.push(HttpEvidence {
            method,
            path,
            source: SourceLocation {
                document: "har".into(),
                line: None,
                column: None,
            },
            parameters,
            request: request_body,
            responses: vec![ResponseEvidence {
                status,
                content: response_body,
            }],
            unknown: BTreeSet::from([UnknownFact::Completeness]),
        });
    }
    if options
        .route_templates
        .iter()
        .any(|route| !used_routes.contains(route))
    {
        return Err(error(
            "HR006",
            "A reviewed route template matches no imported entry; check the template and the origin and prefix filters.",
        ));
    }
    crate::infer(&output.evidence, &Default::default(), limits)?;
    Ok(output)
}
