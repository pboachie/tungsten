// SPDX-License-Identifier: AGPL-3.0-only
use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use crate::*;

fn fail(code: &str, message: &str) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        message: message.into(),
    }
}

/// Parse bounded JSON without exposing serde error excerpts.
pub fn parse_evidence(bytes: &[u8], limits: &Limits) -> Result<EvidenceDocument, Diagnostic> {
    if bytes.len() > limits.max_bytes {
        return Err(fail(
            "TC001",
            "Evidence exceeds byte limit; split the import.",
        ));
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| fail("TC002", "Invalid evidence JSON; check syntax and nesting."))?;
    bounded(&value, limits)?;
    let document = serde_json::from_value(value).map_err(|_| {
        fail(
            "TC003",
            "Unsupported evidence model; use version 1 HTTP fields.",
        )
    })?;
    validate(&document, limits)?;
    Ok(document)
}

pub fn stable_operation_id(method: &str, path: &str) -> String {
    let key = format!("{}\n{}", method.to_ascii_lowercase(), path);
    format!("import_{}", blake3::hash(key.as_bytes()).to_hex())
}

fn bounded(value: &Value, limits: &Limits) -> Result<(), Diagnostic> {
    let mut pending = vec![(value, 0usize)];
    let mut nodes = 0;
    let mut bytes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if depth > limits.max_depth || nodes > limits.max_nodes {
            return Err(fail(
                "TC001",
                "Evidence exceeds nesting or node limit; reduce the import.",
            ));
        }
        match value {
            Value::Array(values) => pending.extend(values.iter().map(|v| (v, depth + 1))),
            Value::Object(values) => {
                for (key, value) in values {
                    bytes = bytes.saturating_add(key.len());
                    pending.push((value, depth + 1));
                }
            }
            Value::String(s) => bytes = bytes.saturating_add(s.len()),
            _ => bytes = bytes.saturating_add(8),
        }
        if bytes > limits.max_bytes {
            return Err(fail(
                "TC001",
                "Evidence exceeds byte limit; split the import.",
            ));
        }
    }
    Ok(())
}

fn validate(document: &EvidenceDocument, limits: &Limits) -> Result<(), Diagnostic> {
    if document.version != EVIDENCE_VERSION {
        return Err(fail(
            "TC003",
            "Unsupported evidence version; use version 1.",
        ));
    }
    if document.operations.len() > limits.max_operations {
        return Err(fail(
            "TC001",
            "Operation count exceeds limit; split the import.",
        ));
    }
    let encoded =
        serde_json::to_value(document).map_err(|_| fail("TC003", "Invalid evidence model."))?;
    bounded(&encoded, limits)?;
    let mut sources = BTreeSet::new();
    let mut samples = 0usize;
    for operation in &document.operations {
        let method = operation.method.to_ascii_lowercase();
        if ![
            "get", "put", "post", "delete", "options", "head", "patch", "trace",
        ]
        .contains(&method.as_str())
        {
            return Err(fail(
                "TC004",
                "Unsupported HTTP method; provide an OpenAPI HTTP operation.",
            ));
        }
        let path = &operation.path;
        if !path.starts_with('/')
            || path.contains(['?', '#', '\\'])
            || path.chars().any(char::is_whitespace)
        {
            return Err(fail(
                "TC004",
                "Provide an explicit path template without URL, query, fragment or whitespace.",
            ));
        }
        let mut placeholders = BTreeSet::new();
        for segment in path.split('/') {
            if segment.contains(['{', '}']) {
                if !segment.starts_with('{')
                    || !segment.ends_with('}')
                    || segment.len() < 3
                    || segment[1..segment.len() - 1].contains(['{', '}'])
                {
                    return Err(fail(
                        "TC004",
                        "Path placeholders must occupy complete segments.",
                    ));
                }
                if !placeholders.insert(&segment[1..segment.len() - 1]) {
                    return Err(fail("TC004", "Path placeholder names must be unique."));
                }
            }
        }
        let source = &operation.source.document;
        if source.is_empty()
            || source.len() > 64
            || !source
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(fail(
                "TC005",
                "Use an opaque alphanumeric source ID, never a filename or URL.",
            ));
        }
        sources.insert(source);
        let mut shapes = Vec::new();
        let mut path_parameters = BTreeSet::new();
        for parameter in &operation.parameters {
            if parameter.name.is_empty() || parameter.name.chars().any(char::is_control) {
                return Err(fail(
                    "TC004",
                    "Parameter names must be nonempty and contain no control characters.",
                ));
            }
            if parameter.location == ParameterLocation::Path {
                path_parameters.insert(parameter.name.as_str());
            }
            shapes.push(&parameter.shape);
        }
        if path_parameters != placeholders {
            return Err(fail(
                "TC004",
                "Declare exactly one path parameter for every explicit path placeholder.",
            ));
        }
        for media in operation
            .request
            .iter()
            .chain(operation.responses.iter().flat_map(|r| &r.content))
        {
            if !media.media_type.contains('/') || media.media_type.contains([';', '\r', '\n', ' '])
            {
                return Err(fail(
                    "TC004",
                    "Media types must be bare type/subtype values without parameters.",
                ));
            }
            shapes.push(&media.shape);
        }
        for response in &operation.responses {
            if !(100..=599).contains(&response.status) {
                return Err(fail(
                    "TC004",
                    "Response status must be an HTTP status between 100 and 599.",
                ));
            }
        }
        for shape in shapes {
            samples = samples.saturating_add(shape.samples.len());
            if let Some(schema) = &shape.declared_schema {
                validate_schema(schema)?;
            }
        }
    }
    if sources.len() > limits.max_sources || samples > limits.max_samples {
        return Err(fail(
            "TC001",
            "Source or sample count exceeds limit; split the import.",
        ));
    }
    Ok(())
}

fn validate_schema(schema: &Value) -> Result<(), Diagnostic> {
    let Some(object) = schema.as_object() else {
        return Err(fail("TC006", "Structural schemas must be JSON objects."));
    };
    for (key, value) in object {
        let valid = match key.as_str() {
            "type" => value.as_str().is_some_and(|t| {
                [
                    "null", "boolean", "object", "array", "number", "integer", "string",
                ]
                .contains(&t)
            }),
            "properties" => {
                if let Some(properties) = value.as_object() {
                    for schema in properties.values() {
                        validate_schema(schema)?;
                    }
                    true
                } else {
                    false
                }
            }
            "items" => {
                validate_schema(value)?;
                true
            }
            "additionalProperties" => {
                if value.is_boolean() {
                    true
                } else {
                    validate_schema(value)?;
                    true
                }
            }
            "required" => value
                .as_array()
                .is_some_and(|a| a.iter().all(Value::is_string)),
            "anyOf" => {
                if let Some(schemas) = value.as_array() {
                    for schema in schemas {
                        validate_schema(schema)?;
                    }
                    !schemas.is_empty()
                } else {
                    false
                }
            }
            "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" => value.is_number(),
            "minLength" | "maxLength" | "minItems" | "maxItems" | "minProperties"
            | "maxProperties" => value.as_u64().is_some(),
            "multipleOf" => value.as_f64().is_some_and(|v| v > 0.0),
            _ => {
                return Err(fail(
                    "TC006",
                    "Unsupported schema keyword; use structural fields or retain this declaration in the original OpenAPI document. References and value-bearing annotations are not imported.",
                ));
            }
        };
        if !valid {
            return Err(fail(
                "TC006",
                "Invalid structural schema keyword value; check the adapter contract.",
            ));
        }
    }
    Ok(())
}

fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, canonical(v)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
        value => value,
    }
}

fn inferred(value: &Value) -> Value {
    match value {
        Value::Null => json!({"type":"null"}),
        Value::Bool(_) => json!({"type":"boolean"}),
        Value::Number(n) => {
            json!({"type": if n.is_i64() || n.is_u64() {"integer"} else {"number"}})
        }
        Value::String(_) => json!({"type":"string"}),
        Value::Array(values) => {
            json!({"type":"array", "items": merge(values.iter().map(inferred).collect())})
        }
        Value::Object(map) => {
            json!({"type":"object", "additionalProperties":true, "properties": map.iter().map(|(k,v)| (k.clone(),inferred(v))).collect::<BTreeMap<_,_>>()})
        }
    }
}

// Join complete sets at once: pairwise folding would make empty/optional
// evidence order-dependent. Unknown is the top element of the shape lattice.
fn merge(schemas: Vec<Value>) -> Value {
    let mut unique = BTreeMap::new();
    for schema in schemas {
        let schema = canonical(schema);
        if schema == json!({}) {
            return schema;
        }
        unique.insert(schema.to_string(), schema);
    }
    let schemas: Vec<_> = unique.into_values().collect();
    if schemas.is_empty() {
        return json!({});
    }
    if schemas.len() == 1 {
        return schemas[0].clone();
    }
    if schemas.iter().all(|s| {
        s["type"] == "object"
            && s.as_object().is_some_and(|o| {
                o.keys().all(|k| {
                    ["type", "properties", "additionalProperties", "required"].contains(&k.as_str())
                })
            })
    }) {
        let mut properties: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let mut required: Option<BTreeSet<String>> = None;
        let mut additional = Vec::new();
        for schema in &schemas {
            if let Some(props) = schema["properties"].as_object() {
                for (key, value) in props {
                    properties
                        .entry(key.clone())
                        .or_default()
                        .push(value.clone());
                }
            }
            let keys: BTreeSet<_> = schema["required"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            required = Some(required.map_or(keys.clone(), |prior| {
                prior.intersection(&keys).cloned().collect()
            }));
            additional.push(
                schema
                    .get("additionalProperties")
                    .cloned()
                    .unwrap_or(Value::Bool(true)),
            );
        }
        let mut result = json!({"type":"object", "properties":properties.into_iter().map(|(k,v)| (k,merge(v))).collect::<BTreeMap<_,_>>(), "additionalProperties": if additional.iter().all(|v| v == &Value::Bool(false)) {Value::Bool(false)} else {Value::Bool(true)}});
        if let Some(required) = required.filter(|r| !r.is_empty()) {
            result["required"] = json!(required);
        }
        return canonical(result);
    }
    if schemas.iter().all(|s| {
        s["type"] == "array"
            && s.as_object()
                .is_some_and(|o| o.keys().all(|k| ["type", "items"].contains(&k.as_str())))
    }) {
        return json!({"type":"array", "items":merge(schemas.iter().map(|s| s.get("items").cloned().unwrap_or(json!({}))).collect())});
    }
    json!({"anyOf":schemas})
}

fn escape(text: &str) -> String {
    text.replace('~', "~0").replace('/', "~1")
}

#[derive(Default)]
struct Shapes {
    schemas: Vec<Value>,
    states: BTreeSet<EvidenceState>,
    sources: BTreeSet<SourceLocation>,
}

impl Shapes {
    fn add(&mut self, shape: &ShapeEvidence, source: &SourceLocation) {
        self.sources.insert(source.clone());
        if let Some(schema) = &shape.declared_schema {
            self.states.insert(EvidenceState::Declared);
            self.schemas.push(schema.clone());
        }
        for sample in &shape.samples {
            self.states.insert(EvidenceState::Observed);
            self.schemas.push(inferred(sample));
        }
        if shape.samples.is_empty() && shape.declared_schema.is_none() {
            self.states.insert(EvidenceState::Unknown);
            self.schemas.push(json!({}));
        }
    }
    fn finish(self, pointer: &str, facts: &mut BTreeMap<String, FactProvenance>) -> Value {
        let schema = merge(self.schemas);
        let provenance = FactProvenance {
            states: self.states,
            sources: self.sources,
        };
        record(&schema, pointer, &provenance, facts);
        schema
    }
}

fn record(
    value: &Value,
    pointer: &str,
    provenance: &FactProvenance,
    facts: &mut BTreeMap<String, FactProvenance>,
) {
    facts.insert(pointer.into(), provenance.clone());
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                record(
                    value,
                    &format!("{pointer}/{}", escape(key)),
                    provenance,
                    facts,
                );
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                record(value, &format!("{pointer}/{index}"), provenance, facts);
            }
        }
        _ => {}
    }
}

/// Infer a portable contract. Reviewed overrides are returned unchanged,
/// including entries whose operation disappeared from the current observations.
pub fn infer(
    document: &EvidenceDocument,
    overrides: &ReviewedOverrides,
    limits: &Limits,
) -> Result<ContractOutput, Diagnostic> {
    validate(document, limits)?;
    bounded(
        &serde_json::to_value(overrides).map_err(|_| fail("TC003", "Invalid overrides."))?,
        limits,
    )?;
    for (pointer, schema) in &overrides.schemas {
        if !pointer.starts_with("/paths/") || !pointer.ends_with("/schema") {
            return Err(fail(
                "TC007",
                "Reviewed schema pointers must identify an operation schema under /paths/.",
            ));
        }
        validate_schema(schema)?;
    }
    let mut groups: BTreeMap<(String, String), Vec<&HttpEvidence>> = BTreeMap::new();
    for op in &document.operations {
        groups
            .entry((op.path.clone(), op.method.to_ascii_lowercase()))
            .or_default()
            .push(op);
    }
    let mut paths = Map::new();
    let mut report = InferenceReport {
        version: 1,
        facts: BTreeMap::new(),
        review_diff: Vec::new(),
        unknown: BTreeMap::new(),
    };
    let mut seen_safety = BTreeSet::new();
    for ((path, method), operations) in groups {
        let id = stable_operation_id(&method, &path);
        let pointer = format!("/paths/{}/{}", escape(&path), method);
        let mut parameters: BTreeMap<(ParameterLocation, String), Shapes> = BTreeMap::new();
        let mut requests: BTreeMap<String, Shapes> = BTreeMap::new();
        let mut responses: BTreeMap<u16, BTreeMap<String, Shapes>> = BTreeMap::new();
        let mut unknown = BTreeSet::from([
            UnknownFact::Authentication,
            UnknownFact::Authorization,
            UnknownFact::Safety,
            UnknownFact::Requiredness,
            UnknownFact::Completeness,
        ]);
        let mut sources = BTreeSet::new();
        for op in operations {
            sources.insert(op.source.clone());
            unknown.extend(&op.unknown);
            for p in &op.parameters {
                parameters
                    .entry((
                        p.location,
                        if p.location == ParameterLocation::Header {
                            p.name.to_ascii_lowercase()
                        } else {
                            p.name.clone()
                        },
                    ))
                    .or_default()
                    .add(&p.shape, &op.source);
            }
            for m in &op.request {
                requests
                    .entry(m.media_type.to_ascii_lowercase())
                    .or_default()
                    .add(&m.shape, &op.source);
            }
            for response in &op.responses {
                let media = responses.entry(response.status).or_default();
                for m in &response.content {
                    media
                        .entry(m.media_type.to_ascii_lowercase())
                        .or_default()
                        .add(&m.shape, &op.source);
                }
            }
        }
        let structural = FactProvenance {
            states: BTreeSet::from([EvidenceState::Declared]),
            sources,
        };
        report
            .facts
            .insert(format!("{pointer}/operationId"), structural.clone());
        let mut operation = json!({"operationId":id, "x-tungsten-imported":true});
        let mut params = Vec::new();
        for ((location, name), shapes) in parameters {
            let schema_pointer = format!("{pointer}/parameters/{}/schema", params.len());
            let mut parameter = json!({"name":name,"in":location,"schema":shapes.finish(&schema_pointer,&mut report.facts)});
            // Required here is a path-template declaration, never a sample inference.
            if location == ParameterLocation::Path {
                parameter["required"] = Value::Bool(true);
            }
            params.push(parameter);
        }
        if !params.is_empty() {
            operation["parameters"] = json!(params);
        }
        if !requests.is_empty() {
            let mut content = Map::new();
            for (media, shapes) in requests {
                let p = format!("{pointer}/requestBody/content/{}/schema", escape(&media));
                content.insert(media, json!({"schema":shapes.finish(&p,&mut report.facts)}));
            }
            operation["requestBody"] = json!({"content":content});
        }
        let mut response_map = Map::new();
        for (status, media) in responses {
            let mut response = json!({"description":"Imported response"});
            let mut content = Map::new();
            for (media, shapes) in media {
                let p = format!(
                    "{pointer}/responses/{status}/content/{}/schema",
                    escape(&media)
                );
                content.insert(media, json!({"schema":shapes.finish(&p,&mut report.facts)}));
            }
            if !content.is_empty() {
                response["content"] = Value::Object(content);
            }
            report
                .facts
                .insert(format!("{pointer}/responses/{status}"), structural.clone());
            response_map.insert(status.to_string(), response);
        }
        if response_map.is_empty() {
            response_map.insert(
                "default".into(),
                json!({"description":"Response not observed"}),
            );
        }
        operation["responses"] = Value::Object(response_map);
        if let Some(safety) = overrides.safety.get(&id) {
            seen_safety.insert(id.clone());
            if *safety != SafetyDecision::Unknown {
                operation["x-tungsten-reviewed-safety"] = json!(safety);
                unknown.remove(&UnknownFact::Safety);
                report.facts.insert(
                    format!("{pointer}/x-tungsten-reviewed-safety"),
                    FactProvenance {
                        states: BTreeSet::from([EvidenceState::UserReviewed]),
                        sources: BTreeSet::new(),
                    },
                );
            }
        }
        report.unknown.insert(id, unknown);
        paths.entry(path).or_insert_with(|| json!({}))[method] = operation;
    }
    let mut openapi = json!({"openapi":"3.1.0", "info":{"title":"Imported contract","version":"0.0.0"},"paths":paths});
    for (pointer, schema) in &overrides.schemas {
        let generated = openapi.pointer(pointer).cloned();
        if generated.as_ref() != Some(schema) {
            report.review_diff.push(ReviewDiff {
                pointer: pointer.clone(),
                generated,
                reviewed: schema.clone(),
            });
        }
        if let Some(target) = openapi.pointer_mut(pointer) {
            *target = schema.clone();
            report
                .facts
                .retain(|p, _| p != pointer && !p.starts_with(&format!("{pointer}/")));
            record(
                schema,
                pointer,
                &FactProvenance {
                    states: BTreeSet::from([EvidenceState::UserReviewed]),
                    sources: BTreeSet::new(),
                },
                &mut report.facts,
            );
        }
    }
    for (id, safety) in &overrides.safety {
        if !seen_safety.contains(id) {
            report.review_diff.push(ReviewDiff {
                pointer: format!("/overrides/safety/{}", escape(id)),
                generated: None,
                reviewed: json!(safety),
            });
        }
    }
    Ok(ContractOutput {
        openapi: canonical(openapi),
        report,
        overrides: overrides.clone(),
    })
}
