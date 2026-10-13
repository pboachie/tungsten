// SPDX-License-Identifier: AGPL-3.0-only
//! Saved requests are parsed as data. This module has no process, filesystem,
//! environment or network APIs. Explicit route templates keep URL path values
//! out of generated contracts.

use crate::{
    Diagnostic, EVIDENCE_VERSION, EvidenceDocument, Frontend, HttpEvidence, Limits, MediaEvidence,
    ParameterEvidence, ParameterLocation, ShapeEvidence, SourceLocation, UnknownFact,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellDialect {
    Posix,
    WindowsCmd,
    PowerShell,
}
fn err(message: &str) -> Diagnostic {
    Diagnostic {
        code: "TG1201".into(),
        message: message.into(),
    }
}

/// Parse one cURL invocation. `route` is a separately supplied, trusted path
/// template, never inferred from a possibly credential-bearing URL path.
pub fn curl(
    input: &str,
    dialect: ShellDialect,
    route: &str,
    limits: &Limits,
) -> Result<EvidenceDocument, Diagnostic> {
    bounded(input, limits)?;
    let words = words(input, dialect)?;
    let Some(command) = words.first() else {
        return Err(err("provide one saved curl invocation"));
    };
    if !matches!(command.as_str(), "curl" | "curl.exe")
        || (dialect == ShellDialect::PowerShell && command != "curl.exe")
    {
        return Err(err(
            "use curl (POSIX/CMD) or curl.exe (PowerShell); aliases and arbitrary commands are unsupported",
        ));
    }
    let mut method = None;
    let mut url = None;
    let mut headers = vec![];
    let mut body = None;
    let mut json_flag = false;
    let mut i = 1;
    while i < words.len() {
        let word = &words[i];
        match word.as_str() {
            "-X" | "--request" | "-H" | "--header" | "-d" | "--data" | "--data-raw"
            | "--data-binary" | "--json" | "--url" => {
                i += 1;
                let Some(value) = words.get(i) else {
                    return Err(err("saved curl flag is missing its value"));
                };
                match word.as_str() {
                    "-X" | "--request" => {
                        if method.replace(value.clone()).is_some() {
                            return Err(err("multiple method flags are ambiguous"));
                        }
                    }
                    "-H" | "--header" => {
                        headers.push(value.clone());
                    }
                    "--url" => {
                        if url.replace(value.clone()).is_some() {
                            return Err(err("only one URL per curl invocation is supported"));
                        }
                    }
                    _ => {
                        if value.starts_with('@') {
                            return Err(err(
                                "body/upload file references are never read; provide inline JSON",
                            ));
                        }
                        if body.replace(value.clone()).is_some() {
                            return Err(err(
                                "multiple body flags are unsupported; provide one inline JSON body",
                            ));
                        }
                        json_flag = word == "--json";
                    }
                }
            }
            _ if word.starts_with('-') => {
                return Err(err(
                    "unsupported curl option; supported flags are --request, --header, --url and inline JSON data flags",
                ));
            }
            _ => {
                if url.replace(word.clone()).is_some() {
                    return Err(err("compound commands and multiple URLs are unsupported"));
                }
            }
        }
        i += 1;
    }
    let url = url.ok_or_else(|| err("saved curl requires one URL"))?;
    if json_flag
        && !headers.iter().any(|h| {
            h.split_once(':')
                .is_some_and(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        })
    {
        headers.push("Content-Type: application/json".into());
    }
    let method = method.unwrap_or_else(|| {
        if body.is_some() {
            "POST".into()
        } else {
            "GET".into()
        }
    });
    Ok(EvidenceDocument {
        version: EVIDENCE_VERSION,
        frontend: Frontend::Http,
        operations: vec![request(
            &method,
            &url,
            headers,
            body.as_deref(),
            route,
            1,
            limits,
        )?],
    })
}

/// Parse the HTTP-file subset: `###` separators, METHOD URL, headers and an
/// optional inline JSON body. Variables are explicit literal caller bindings;
/// inline definitions, environment access and executable blocks are rejected.
pub fn http(
    input: &str,
    routes: &[String],
    bindings: &BTreeMap<String, String>,
    limits: &Limits,
) -> Result<EvidenceDocument, Diagnostic> {
    bounded(input, limits)?;
    if input.contains("< {%")
        || input.contains("> {%")
        || input.contains("{{$")
        || input.contains("{%")
    {
        return Err(err(
            "HTTP scripts and dynamic/environment variables are unsupported",
        ));
    }
    let mut blocks = vec![];
    let mut current = String::new();
    for line in input.lines() {
        if line.trim_start().starts_with("###") {
            if !current.trim().is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
        } else {
            current.push_str(line);
            current.push('\n');
        }
    }
    if !current.trim().is_empty() {
        blocks.push(current);
    }
    if blocks.is_empty() || blocks.len() > limits.max_operations || blocks.len() != routes.len() {
        return Err(err(
            "provide one explicit route template per HTTP request within the operation limit",
        ));
    }
    let mut operations = vec![];
    for (index, block) in blocks.iter().enumerate() {
        let expanded = expand(block, bindings, limits)?;
        let mut lines = expanded.trim().lines();
        let first = lines.next().ok_or_else(|| err("empty HTTP request"))?;
        let parts: Vec<_> = first.split_whitespace().collect();
        if !(parts.len() == 2 || parts.len() == 3 && parts[2] == "HTTP/1.1") {
            return Err(err(
                "request line must be METHOD URL with optional HTTP/1.1",
            ));
        }
        let mut headers = vec![];
        let mut body = String::new();
        let mut in_body = false;
        for line in lines {
            if !in_body && line.trim().is_empty() {
                in_body = true;
                continue;
            }
            if in_body {
                body.push_str(line);
                body.push('\n');
            } else {
                headers.push(line.to_string());
            }
        }
        operations.push(request(
            parts[0],
            parts[1],
            headers,
            (!body.trim().is_empty()).then_some(body.trim()),
            &routes[index],
            index as u32 + 1,
            limits,
        )?);
    }
    Ok(EvidenceDocument {
        version: EVIDENCE_VERSION,
        frontend: Frontend::Http,
        operations,
    })
}

fn bounded(input: &str, limits: &Limits) -> Result<(), Diagnostic> {
    if input.len() > limits.max_bytes || input.contains('\0') {
        Err(err("saved request exceeds byte limit or contains a NUL"))
    } else {
        Ok(())
    }
}
fn expand(
    input: &str,
    bindings: &BTreeMap<String, String>,
    limits: &Limits,
) -> Result<String, Diagnostic> {
    let mut out = String::new();
    let mut rest = input;
    while let Some((before, after)) = rest.split_once("{{") {
        out.push_str(before);
        let Some((key, tail)) = after.split_once("}}") else {
            return Err(err("unterminated explicit variable"));
        };
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(err("only named literal variables are supported"));
        }
        let value = bindings.get(key).ok_or_else(|| {
            err("provide an explicit binding for every variable; no environment is read")
        })?;
        if value.contains(['\r', '\n']) || value.contains("{{") {
            return Err(err(
                "variable bindings must be single-line literal values without nested expansion",
            ));
        }
        if out
            .len()
            .saturating_add(value.len())
            .saturating_add(tail.len())
            > limits.max_bytes
        {
            return Err(err("expanded request exceeds byte limit"));
        }
        out.push_str(value);
        rest = tail;
    }
    out.push_str(rest);
    bounded(&out, limits)?;
    Ok(out)
}

fn request(
    method: &str,
    url_text: &str,
    headers: Vec<String>,
    body: Option<&str>,
    route: &str,
    line: u32,
    limits: &Limits,
) -> Result<HttpEvidence, Diagnostic> {
    let method = method.to_ascii_uppercase();
    if ![
        "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE",
    ]
    .contains(&method.as_str())
    {
        return Err(err("unsupported HTTP method"));
    }
    let url = url::Url::parse(url_text).map_err(|_| err("provide an absolute HTTP(S) URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(err(
            "URL credentials, fragments and non-HTTP protocols are unsupported; configure authentication through an endpoint manifest",
        ));
    }
    if route.is_empty()
        || !route.starts_with('/')
        || route.contains(['?', '#', '\\'])
        || route.chars().any(char::is_control)
    {
        return Err(err(
            "provide an explicit credential-free path template separately from the request URL",
        ));
    }
    let mut parameters = vec![];
    let mut names = BTreeSet::new();
    let actual: Vec<_> = url.path().split('/').collect();
    let declared: Vec<_> = route.split('/').collect();
    if actual.len() != declared.len()
        || actual.iter().zip(&declared).any(|(actual, declared)| {
            !(declared.starts_with('{') && declared.ends_with('}')) && actual != declared
        })
    {
        return Err(err(
            "route template must match the captured URL's literal path segments",
        ));
    }
    for part in route.split('/') {
        if part.contains(['{', '}']) {
            let Some(name) = part.strip_prefix('{').and_then(|s| s.strip_suffix('}')) else {
                return Err(err("route templates must occupy entire path segments"));
            };
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return Err(err("invalid route template name"));
            }
            if names.insert(("path".to_string(), name.to_string())) {
                parameters.push(parameter(
                    name,
                    ParameterLocation::Path,
                    Value::String(String::new()),
                ));
            }
        }
    }
    for (name, value) in url.query_pairs() {
        if !names.insert(("query".into(), name.to_string())) {
            return Err(err(
                "repeated query keys need an explicit endpoint manifest serialization contract",
            ));
        }
        if sensitive(&name) {
            return Err(err(
                "credential query parameters require an explicit endpoint manifest auth reference",
            ));
        }
        parameters.push(parameter(
            &name,
            ParameterLocation::Query,
            Value::String(value.into_owned()),
        ));
    }
    let mut content_type = None;
    for header in headers {
        let Some((name, value)) = header.split_once(':') else {
            return Err(err(
                "header must be a name/value pair; file headers are unsupported",
            ));
        };
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            || value.contains(['\r', '\n'])
        {
            return Err(err("invalid header syntax"));
        }
        let lower = name.to_ascii_lowercase();
        if sensitive(&lower)
            || matches!(
                lower.as_str(),
                "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
            )
        {
            return Err(err(
                "credential headers require an explicit endpoint manifest auth reference; captured credentials are never exported",
            ));
        }
        if !names.insert(("header".into(), lower.clone())) {
            return Err(err(
                "duplicate headers require an explicit endpoint description",
            ));
        }
        match lower.as_str() {
            "content-type" => content_type = Some(value.trim().to_ascii_lowercase()),
            "host" | "content-length" | "transfer-encoding" => {
                return Err(err("transport override headers are unsupported"));
            }
            _ => parameters.push(parameter(
                name,
                ParameterLocation::Header,
                Value::String(value.trim().into()),
            )),
        }
    }
    let mut request = vec![];
    if let Some(body) = body {
        let media = content_type.as_deref().ok_or_else(|| {
            err("inline JSON data requires explicit Content-Type: application/json (or --json)")
        })?;
        if media != "application/json" && !media.ends_with("+json") {
            return Err(err("only inline JSON request bodies are supported"));
        }
        let sample: Value = serde_json::from_str(body).map_err(|_| {
            err("inline body must be JSON; scripts and file includes are unsupported")
        })?;
        request.push(MediaEvidence {
            media_type: media.into(),
            shape: ShapeEvidence {
                samples: vec![sample],
                declared_schema: None,
            },
        });
    }
    let evidence = HttpEvidence {
        method,
        path: route.into(),
        source: SourceLocation {
            document: "saved_request".into(),
            line: Some(line),
            column: None,
        },
        parameters,
        request,
        responses: vec![],
        unknown: BTreeSet::from([
            UnknownFact::Authentication,
            UnknownFact::Authorization,
            UnknownFact::Safety,
            UnknownFact::Completeness,
            UnknownFact::Requiredness,
        ]),
    };
    // Run engine validation before returning evidence; no raw samples survive
    // the eventual inferred contract or report.
    crate::infer(
        &EvidenceDocument {
            version: EVIDENCE_VERSION,
            frontend: Frontend::Http,
            operations: vec![evidence.clone()],
        },
        &Default::default(),
        limits,
    )?;
    Ok(evidence)
}
fn parameter(name: &str, location: ParameterLocation, sample: Value) -> ParameterEvidence {
    ParameterEvidence {
        name: name.into(),
        location,
        shape: ShapeEvidence {
            samples: vec![sample],
            declared_schema: None,
        },
    }
}
fn sensitive(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "api_key",
        "apikey",
        "api-key",
        "credential",
    ]
    .iter()
    .any(|s| n.contains(s))
}

fn words(input: &str, dialect: ShellDialect) -> Result<Vec<String>, Diagnostic> {
    let mut words = vec![];
    let mut word = String::new();
    let mut quote = None;
    let mut active = false;
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            if dialect == ShellDialect::WindowsCmd && c == '\\' && chars.get(i + 1) == Some(&'"') {
                word.push('"');
                i += 2;
                continue;
            }
            if c == q {
                if dialect == ShellDialect::PowerShell
                    && q == '\''
                    && chars.get(i + 1) == Some(&'\'')
                {
                    word.push('\'');
                    i += 2;
                    continue;
                }
                quote = None;
                i += 1;
                continue;
            }
            if q == '"' && (c == '$' || c == '`' && dialect != ShellDialect::PowerShell) {
                return Err(err(
                    "shell expansion and command substitution are unsupported",
                ));
            }
            if dialect == ShellDialect::WindowsCmd && matches!(c, '%' | '!') {
                return Err(err("CMD variable expansion is unsupported"));
            }
            let escape = match dialect {
                ShellDialect::Posix => Some('\\'),
                ShellDialect::PowerShell => Some('`'),
                ShellDialect::WindowsCmd => None,
            };
            if q == '"' && Some(c) == escape {
                let Some(next) = chars.get(i + 1) else {
                    return Err(err("trailing quote escape"));
                };
                if !matches!(next, '"' | '\\' | '`') {
                    return Err(err("unsupported quoted escape"));
                }
                word.push(*next);
                i += 2;
                continue;
            }
            word.push(c);
            i += 1;
            continue;
        }
        if c == '"' || c == '\'' && dialect != ShellDialect::WindowsCmd {
            quote = Some(c);
            active = true;
            i += 1;
            continue;
        }
        let escape = match dialect {
            ShellDialect::Posix => '\\',
            ShellDialect::WindowsCmd => '^',
            ShellDialect::PowerShell => '`',
        };
        if c == escape {
            let Some(next) = chars.get(i + 1) else {
                return Err(err("trailing escape"));
            };
            if *next == '\n' {
                i += 2;
                continue;
            }
            if *next == '\r' && chars.get(i + 2) == Some(&'\n') {
                i += 3;
                continue;
            }
            if dialect != ShellDialect::Posix {
                return Err(err(
                    "only line continuations are supported outside quotes in Windows dialects",
                ));
            }
            word.push(*next);
            active = true;
            i += 2;
            continue;
        }
        if c.is_whitespace() {
            if active {
                words.push(std::mem::take(&mut word));
                active = false;
            }
            i += 1;
            continue;
        }
        if matches!(c, '|' | '&' | ';' | '<' | '>' | '$' | '`' | '(' | ')')
            || dialect == ShellDialect::WindowsCmd && matches!(c, '%' | '!')
        {
            return Err(err(
                "shell operators, expansion and compound commands are unsupported; quote literal URLs",
            ));
        }
        word.push(c);
        active = true;
        i += 1;
    }
    if quote.is_some() {
        return Err(err("unterminated shell quote"));
    }
    if active {
        words.push(word);
    }
    Ok(words)
}
