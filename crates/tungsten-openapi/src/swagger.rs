// SPDX-License-Identifier: AGPL-3.0-only
//! Offline, fail-closed Swagger 2 conversion. Input is already size/depth bounded
//! by the document loader. Conversion never resolves external references.

use serde_json::{Map, Value, json};

/// A conversion failure located in the original Swagger document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionError {
    pub pointer: String,
    pub message: String,
}

type Result<T> = std::result::Result<T, ConversionError>;

fn fail<T>(pointer: &str, message: &str) -> Result<T> {
    Err(ConversionError {
        pointer: pointer.into(),
        message: message.into(),
    })
}
fn object<'a>(v: &'a Value, p: &str) -> Result<&'a Map<String, Value>> {
    v.as_object().ok_or_else(|| ConversionError {
        pointer: p.into(),
        message: "expected an object".into(),
    })
}
fn esc(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}
fn text<'a>(v: &'a Value, key: &str, p: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ConversionError {
            pointer: format!("{p}/{key}"),
            message: "expected a string".into(),
        })
}
fn strings(v: Option<&Value>, p: &str) -> Result<Vec<String>> {
    let Some(v) = v else {
        return Ok(vec![]);
    };
    let Some(a) = v.as_array() else {
        return fail(p, "expected an array of strings");
    };
    a.iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| ConversionError {
                    pointer: p.into(),
                    message: "expected a string".into(),
                })
        })
        .collect()
}
fn fields(v: &Value, keys: &[&str]) -> Map<String, Value> {
    v.as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| keys.contains(&k.as_str()) || k.starts_with("x-"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Convert a Swagger 2 document to OpenAPI 3.0.3. No partial output is returned
/// on an unsupported semantic. Call the normal OpenAPI loader on the result.
pub fn convert(input: &Value) -> Result<Value> {
    bounded(input, "", 0, &mut 0)?;
    object(input, "")?;
    known(
        input,
        "",
        &[
            "swagger",
            "info",
            "host",
            "basePath",
            "schemes",
            "consumes",
            "produces",
            "paths",
            "definitions",
            "parameters",
            "responses",
            "securityDefinitions",
            "security",
            "tags",
            "externalDocs",
        ],
    )?;
    if input.get("swagger").and_then(Value::as_str) != Some("2.0") {
        return fail("/swagger", "expected Swagger version 2.0");
    }
    let mut out = fields(input, &["info", "tags", "externalDocs", "security"]);
    out.insert("openapi".into(), json!("3.0.3"));
    out.insert("servers".into(), servers(input, input, "")?);
    let mut schemas = Map::new();
    if let Some(defs) = input.get("definitions") {
        for (name, value) in object(defs, "/definitions")? {
            schemas.insert(
                name.clone(),
                schema(value, &format!("/definitions/{}", esc(name)))?,
            );
        }
    }
    let mut security = Map::new();
    if let Some(defs) = input.get("securityDefinitions") {
        for (name, value) in object(defs, "/securityDefinitions")? {
            security.insert(
                name.clone(),
                security_scheme(value, &format!("/securityDefinitions/{}", esc(name)))?,
            );
        }
    }
    out.insert(
        "components".into(),
        json!({"schemas":schemas,"securitySchemes":security}),
    );
    let paths = input.get("paths").ok_or_else(|| ConversionError {
        pointer: "/paths".into(),
        message: "missing paths".into(),
    })?;
    let mut converted = Map::new();
    for (path, item) in object(paths, "/paths")? {
        if path.starts_with("x-") {
            continue;
        }
        let pointer = format!("/paths/{}", esc(path));
        if !path.starts_with('/') {
            return fail(&pointer, "path must start with /");
        }
        let mut target = Map::new();
        for (method, operation) in object(item, &pointer)? {
            if method == "parameters" || method.starts_with("x-") {
                continue;
            }
            if !["get", "put", "post", "delete", "options", "head", "patch"]
                .contains(&method.as_str())
            {
                return fail(
                    &format!("{pointer}/{}", esc(method)),
                    "unsupported path item member (path-item references are not supported)",
                );
            }
            let p = format!("{pointer}/{method}");
            object(operation, &p)?;
            known(
                operation,
                &p,
                &[
                    "tags",
                    "summary",
                    "description",
                    "externalDocs",
                    "operationId",
                    "consumes",
                    "produces",
                    "parameters",
                    "responses",
                    "schemes",
                    "deprecated",
                    "security",
                ],
            )?;
            let mut op = fields(
                operation,
                &[
                    "tags",
                    "summary",
                    "description",
                    "externalDocs",
                    "operationId",
                    "deprecated",
                    "security",
                ],
            );
            if operation.get("schemes").is_some() {
                op.insert("servers".into(), servers(input, operation, &p)?);
            }
            let consumes = strings(
                operation.get("consumes").or_else(|| input.get("consumes")),
                &format!("{p}/consumes"),
            )?;
            let produces = strings(
                operation.get("produces").or_else(|| input.get("produces")),
                &format!("{p}/produces"),
            )?;
            let mut parameters: Vec<(Value, String)> = vec![];
            for (source, loc) in [(item, pointer.as_str()), (operation, p.as_str())] {
                if let Some(list) = source.get("parameters") {
                    let Some(list) = list.as_array() else {
                        return fail(&format!("{loc}/parameters"), "expected parameter array");
                    };
                    for (i, raw) in list.iter().enumerate() {
                        let site = format!("{loc}/parameters/{i}");
                        let param = resolve(input, raw, "parameters", &site)?;
                        known(
                            param,
                            &site,
                            &[
                                "name",
                                "in",
                                "description",
                                "required",
                                "schema",
                                "type",
                                "format",
                                "allowEmptyValue",
                                "items",
                                "collectionFormat",
                                "default",
                                "maximum",
                                "exclusiveMaximum",
                                "minimum",
                                "exclusiveMinimum",
                                "maxLength",
                                "minLength",
                                "pattern",
                                "maxItems",
                                "minItems",
                                "uniqueItems",
                                "enum",
                                "multipleOf",
                            ],
                        )?;
                        let name = text(param, "name", &site)?;
                        let location = text(param, "in", &site)?;
                        if let Some(index) = parameters.iter().position(|(v, _)| {
                            v.get("name").and_then(Value::as_str) == Some(name)
                                && v.get("in").and_then(Value::as_str) == Some(location)
                        }) {
                            parameters.remove(index);
                        }
                        parameters.push((param.clone(), site));
                    }
                }
            }
            let mut normal = vec![];
            let mut body = None;
            let mut form = Map::new();
            let mut required = vec![];
            let mut encoding = Map::new();
            for (param, site) in parameters {
                match text(&param, "in", &site)? {
                    "body" => {
                        if body.is_some() {
                            return fail(&site, "multiple body parameters");
                        }
                        let Some(s) = param.get("schema") else {
                            return fail(&site, "body parameter requires schema");
                        };
                        if consumes.is_empty() {
                            return fail(
                                &site,
                                "body conversion requires explicit consumes media types",
                            );
                        }
                        let mut b = fields(&param, &["description", "required"]);
                        let s = schema(s, &format!("{site}/schema"))?;
                        b.insert(
                            "content".into(),
                            Value::Object(
                                consumes
                                    .iter()
                                    .map(|m| (m.clone(), json!({"schema":s})))
                                    .collect(),
                            ),
                        );
                        body = Some(Value::Object(b));
                    }
                    "formData" => {
                        let name = text(&param, "name", &site)?.to_owned();
                        let s = parameter_schema(&param, &site)?;
                        if param.get("required") == Some(&Value::Bool(true)) {
                            required.push(name.clone());
                        }
                        if param.get("type").and_then(Value::as_str) == Some("array") {
                            let (style, explode) = serialization(&param, "query", &site)?;
                            encoding.insert(name.clone(), json!({"style":style,"explode":explode}));
                        }
                        form.insert(name, s);
                    }
                    "query" | "header" | "path" => {
                        normal.push(parameter(&param, &site)?);
                    }
                    _ => return fail(&site, "unsupported parameter location"),
                }
            }
            if !form.is_empty() {
                if body.is_some() {
                    return fail(&p, "body and formData cannot be combined");
                }
                if consumes.is_empty()
                    || consumes.iter().any(|m| {
                        !["multipart/form-data", "application/x-www-form-urlencoded"]
                            .contains(&m.as_str())
                    })
                {
                    return fail(
                        &p,
                        "formData requires multipart/form-data or application/x-www-form-urlencoded consumes",
                    );
                }
                if consumes
                    .iter()
                    .any(|m| m == "application/x-www-form-urlencoded")
                    && form
                        .values()
                        .any(|s| s.get("format").and_then(Value::as_str) == Some("binary"))
                {
                    return fail(&p, "file upload requires multipart/form-data");
                }
                let mut form_schema = json!({"type":"object","properties":form});
                if !required.is_empty() {
                    form_schema["required"] = json!(required);
                }
                let content: Map<String, Value> = consumes
                    .iter()
                    .map(|m| (m.clone(), json!({"schema":form_schema,"encoding":encoding})))
                    .collect();
                body = Some(json!({"required":!required.is_empty(),"content":content}));
            }
            if !normal.is_empty() {
                op.insert("parameters".into(), json!(normal));
            }
            if let Some(body) = body {
                op.insert("requestBody".into(), body);
            }
            let Some(responses) = operation.get("responses") else {
                return fail(&p, "operation requires responses");
            };
            let mut response_map = Map::new();
            for (status, raw) in object(responses, &format!("{p}/responses"))? {
                if status.starts_with("x-") {
                    continue;
                }
                let site = format!("{p}/responses/{}", esc(status));
                let response = resolve(input, raw, "responses", &site)?;
                known(
                    response,
                    &site,
                    &["description", "schema", "headers", "examples"],
                )?;
                let mut r = fields(response, &["description"]);
                if let Some(s) = response.get("schema") {
                    if produces.is_empty() {
                        return fail(
                            &site,
                            "response schema conversion requires explicit produces media types",
                        );
                    }
                    let s = schema(s, &format!("{site}/schema"))?;
                    if let Some(examples) = response.get("examples")
                        && object(examples, &format!("{site}/examples"))?
                            .keys()
                            .any(|m| !produces.contains(m))
                    {
                        return fail(&site, "response example media type is absent from produces");
                    }
                    let mut content = Map::new();
                    for media in &produces {
                        let mut m = json!({"schema":s});
                        if let Some(example) = response.get("examples").and_then(|v| v.get(media)) {
                            m["example"] = example.clone();
                        }
                        content.insert(media.clone(), m);
                    }
                    r.insert("content".into(), Value::Object(content));
                } else if response.get("examples").is_some() {
                    return fail(
                        &site,
                        "response examples require a declared schema and media types",
                    );
                }
                if let Some(headers) = response.get("headers") {
                    let mut h = Map::new();
                    for (name, v) in object(headers, &format!("{site}/headers"))? {
                        let hp = format!("{site}/headers/{}", esc(name));
                        let mut hv = fields(v, &["description"]);
                        hv.insert("schema".into(), parameter_schema(v, &hp)?);
                        if v.get("type").and_then(Value::as_str) == Some("array") {
                            let (style, explode) = serialization(v, "header", &hp)?;
                            hv.insert("style".into(), json!(style));
                            hv.insert("explode".into(), json!(explode));
                        }
                        h.insert(name.clone(), Value::Object(hv));
                    }
                    r.insert("headers".into(), Value::Object(h));
                }
                response_map.insert(status.clone(), Value::Object(r));
            }
            op.insert("responses".into(), Value::Object(response_map));
            for (key, value) in object(operation, &p)? {
                if key.starts_with("x-") {
                    op.insert(key.clone(), value.clone());
                }
            }
            target.insert(method.clone(), Value::Object(op));
        }
        converted.insert(path.clone(), Value::Object(target));
    }
    out.insert("paths".into(), Value::Object(converted));
    Ok(Value::Object(out))
}

fn bounded(v: &Value, p: &str, depth: usize, count: &mut usize) -> Result<()> {
    *count += 1;
    if depth > 128 || *count > 200_000 {
        return fail(p, "Swagger conversion exceeds nesting or node limit");
    }
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                bounded(v, &format!("{p}/{}", esc(k)), depth + 1, count)?;
            }
        }
        Value::Array(a) => {
            for (i, v) in a.iter().enumerate() {
                bounded(v, &format!("{p}/{i}"), depth + 1, count)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn resolve<'a>(root: &'a Value, v: &'a Value, section: &str, p: &str) -> Result<&'a Value> {
    object(v, p)?;
    if let Some(reference) = v.get("$ref") {
        let Some(reference) = reference.as_str() else {
            return fail(p, "reference must be a string");
        };
        if !reference.starts_with(&format!("#/{section}/"))
            || v.as_object().is_some_and(|m| m.len() != 1)
        {
            return fail(
                p,
                "only bare local parameter/response references are supported",
            );
        }
        let Some(target) = root.pointer(&reference[1..]) else {
            return fail(p, "local reference target does not exist");
        };
        if target.get("$ref").is_some() {
            return fail(p, "chained parameter/response references are unsupported");
        }
        object(target, p)?;
        Ok(target)
    } else {
        Ok(v)
    }
}
fn schema(v: &Value, p: &str) -> Result<Value> {
    known(
        v,
        p,
        &[
            "$ref",
            "title",
            "description",
            "type",
            "format",
            "default",
            "maximum",
            "exclusiveMaximum",
            "minimum",
            "exclusiveMinimum",
            "maxLength",
            "minLength",
            "pattern",
            "maxItems",
            "minItems",
            "uniqueItems",
            "maxProperties",
            "minProperties",
            "required",
            "enum",
            "multipleOf",
            "items",
            "allOf",
            "properties",
            "additionalProperties",
            "discriminator",
            "readOnly",
            "xml",
            "externalDocs",
            "example",
        ],
    )?;
    let mut out = object(v, p)?.clone();
    if let Some(nullable) = out.remove("x-nullable") {
        if !nullable.is_boolean() {
            return fail(p, "x-nullable must be a boolean");
        }
        out.insert("nullable".into(), nullable);
    }
    if let Some(reference) = out.get("$ref") {
        let Some(reference) = reference.as_str() else {
            return fail(p, "reference must be a string");
        };
        let Some(rest) = reference.strip_prefix("#/definitions/") else {
            return fail(p, "only local definition schema references are supported");
        };
        if out.len() != 1 {
            return fail(p, "schema reference siblings are unsupported");
        }
        return Ok(json!({"$ref":format!("#/components/schemas/{rest}")}));
    }
    if out.get("type").and_then(Value::as_str) == Some("file") {
        out.insert("type".into(), json!("string"));
        out.insert("format".into(), json!("binary"));
    }
    if let Some(discriminator) = out.get("discriminator") {
        let Some(name) = discriminator.as_str() else {
            return fail(p, "discriminator must be a property name");
        };
        out.insert("discriminator".into(), json!({"propertyName":name}));
    }
    for key in ["properties"] {
        if let Some(props) = out.get_mut(key) {
            let Some(props) = props.as_object_mut() else {
                return fail(p, "properties must be an object");
            };
            for (name, v) in props {
                *v = schema(v, &format!("{p}/{key}/{}", esc(name)))?;
            }
        }
    }
    for key in ["items", "additionalProperties"] {
        if let Some(v) = out.get_mut(key) {
            if v.is_object() {
                *v = schema(v, &format!("{p}/{key}"))?;
            } else if key == "items" || !v.is_boolean() {
                return fail(p, "unsupported nested schema");
            }
        }
    }
    if let Some(all) = out.get_mut("allOf") {
        let Some(all) = all.as_array_mut() else {
            return fail(p, "allOf must be an array");
        };
        for (i, v) in all.iter_mut().enumerate() {
            *v = schema(v, &format!("{p}/allOf/{i}"))?;
        }
    }
    Ok(Value::Object(out))
}
fn parameter_schema(v: &Value, p: &str) -> Result<Value> {
    let keys = [
        "type",
        "format",
        "items",
        "default",
        "maximum",
        "exclusiveMaximum",
        "minimum",
        "exclusiveMinimum",
        "maxLength",
        "minLength",
        "pattern",
        "maxItems",
        "minItems",
        "uniqueItems",
        "enum",
        "multipleOf",
        "description",
    ];
    if v.get("type").and_then(Value::as_str).is_none() {
        return fail(p, "non-body parameters and headers require a type");
    }
    schema(&Value::Object(fields(v, &keys)), p)
}
fn serialization(v: &Value, location: &str, p: &str) -> Result<(&'static str, bool)> {
    let format = v
        .get("collectionFormat")
        .and_then(Value::as_str)
        .unwrap_or("csv");
    match (location, format) {
        ("query", "csv") => Ok(("form", false)),
        ("query", "multi") => Ok(("form", true)),
        ("query", "ssv") => Ok(("spaceDelimited", false)),
        ("query", "pipes") => Ok(("pipeDelimited", false)),
        ("header" | "path", "csv") => Ok(("simple", false)),
        _ => fail(
            p,
            "collectionFormat cannot be faithfully represented for this parameter location",
        ),
    }
}
fn parameter(v: &Value, p: &str) -> Result<Value> {
    let mut out = fields(
        v,
        &["name", "in", "description", "required", "allowEmptyValue"],
    );
    let location = text(v, "in", p)?;
    if location == "path" && v.get("required") != Some(&Value::Bool(true)) {
        return fail(p, "path parameters must be required");
    }
    if v.get("type").and_then(Value::as_str) == Some("file") {
        return fail(p, "file is only supported in formData");
    }
    out.insert("schema".into(), parameter_schema(v, p)?);
    if v.get("type").and_then(Value::as_str) == Some("array") {
        let (style, explode) = serialization(v, location, p)?;
        out.insert("style".into(), json!(style));
        out.insert("explode".into(), json!(explode));
    }
    Ok(Value::Object(out))
}
fn servers(root: &Value, op: &Value, p: &str) -> Result<Value> {
    let schemes = strings(
        op.get("schemes").or_else(|| root.get("schemes")),
        &format!("{p}/schemes"),
    )?;
    if schemes
        .iter()
        .any(|s| !["http", "https"].contains(&s.as_str()))
    {
        return fail(p, "only HTTP and HTTPS server schemes are supported");
    }
    let base = root.get("basePath").and_then(Value::as_str).unwrap_or("/");
    if !base.starts_with('/') || base.contains(['?', '#']) {
        return fail("/basePath", "invalid basePath");
    }
    let host = root.get("host").and_then(Value::as_str);
    if let Some(host) = host {
        if host.is_empty() || host.contains(['/', '?', '#', '@']) {
            return fail(
                "/host",
                "host must contain only a hostname and optional port",
            );
        }
        if schemes.is_empty() {
            return fail(
                p,
                "host conversion requires explicit schemes to avoid guessing transport",
            );
        }
        Ok(json!(
            schemes
                .iter()
                .map(|s| json!({"url":format!("{s}://{host}{base}")}))
                .collect::<Vec<_>>()
        ))
    } else {
        if !schemes.is_empty() {
            return fail(p, "schemes without a host cannot be resolved offline");
        }
        Ok(json!([{"url":base}]))
    }
}
fn security_scheme(v: &Value, p: &str) -> Result<Value> {
    known(
        v,
        p,
        &[
            "type",
            "description",
            "name",
            "in",
            "flow",
            "authorizationUrl",
            "tokenUrl",
            "scopes",
        ],
    )?;
    let mut out = fields(v, &["description"]);
    match text(v, "type", p)? {
        "basic" => {
            out.insert("type".into(), json!("http"));
            out.insert("scheme".into(), json!("basic"));
        }
        "apiKey" => {
            let location = text(v, "in", p)?;
            if !["query", "header"].contains(&location) {
                return fail(p, "unsupported API key location");
            }
            out.insert("type".into(), json!("apiKey"));
            out.insert("name".into(), json!(text(v, "name", p)?));
            out.insert("in".into(), json!(location));
        }
        "oauth2" => {
            let flow = text(v, "flow", p)?;
            let name = match flow {
                "implicit" => "implicit",
                "password" => "password",
                "application" => "clientCredentials",
                "accessCode" => "authorizationCode",
                _ => return fail(p, "unsupported OAuth flow"),
            };
            let mut f = Map::new();
            if matches!(flow, "implicit" | "accessCode") {
                f.insert(
                    "authorizationUrl".into(),
                    json!(text(v, "authorizationUrl", p)?),
                );
            }
            if flow != "implicit" {
                f.insert("tokenUrl".into(), json!(text(v, "tokenUrl", p)?));
            }
            let scopes = v.get("scopes").ok_or_else(|| ConversionError {
                pointer: p.into(),
                message: "OAuth requires scopes".into(),
            })?;
            object(scopes, p)?;
            f.insert("scopes".into(), scopes.clone());
            out.insert("type".into(), json!("oauth2"));
            out.insert("flows".into(), json!({name:f}));
        }
        _ => return fail(p, "unsupported security scheme"),
    }
    Ok(Value::Object(out))
}

fn known(v: &Value, p: &str, keys: &[&str]) -> Result<()> {
    for key in object(v, p)?.keys() {
        if !keys.contains(&key.as_str()) && !key.starts_with("x-") {
            return fail(
                &format!("{p}/{}", esc(key)),
                "unsupported field; conversion would lose its semantics",
            );
        }
    }
    Ok(())
}
