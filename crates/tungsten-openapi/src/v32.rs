// SPDX-License-Identifier: AGPL-3.0-only
//! OpenAPI 3.2 constructs that have no 3.1 equivalent.
//!
//! A 3.2 document that uses only 3.1 constructs goes through the unchanged
//! 3.1 path. The `query` method (as `QUERY`) and `additionalOperations`
//! (custom methods, sent as written) are compiled by the build, and
//! reusable media types (`components.mediaTypes`, `$ref` in a `content` map)
//! are followed like any other reference. Every other construct 3.2 added is
//! either refused with an error (when ignoring it would produce a client that
//! behaves differently from the API) or reported as a warning (when it only
//! adds metadata the compiler has no use for). Nothing is dropped silently. Codes `TG0140`-`TG0156` form one
//! family, one code per feature, each located at the member that introduces
//! it.

use std::collections::{BTreeSet, HashSet};

use serde_json::{Map, Value};
use tungsten_core::Diagnostic;

use crate::join_pointer;
use crate::spans::SpanIndex;
use crate::walk::{Kind, Node, walk};

/// One 3.2-only construct found in a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    pub code: &'static str,
    pub warning: bool,
    pub pointer: String,
    pub message: String,
    pub help: &'static str,
}

impl Finding {
    pub(crate) fn into_diagnostic(self, file: &str, spans: &SpanIndex) -> Diagnostic {
        let diagnostic = if self.warning {
            Diagnostic::warning(self.code, self.message)
        } else {
            Diagnostic::error(self.code, self.message)
        };
        diagnostic
            .with_help(self.help)
            .at(file, &self.pointer, spans.nearest(&self.pointer))
    }
}

/// JSON Lines media types: one JSON value per line.
const JSON_LINES: &[&str] = &[
    "application/jsonl",
    "application/x-jsonl",
    "application/ndjson",
    "application/x-ndjson",
];

/// The fields of one server-sent event (`data`, `event`, `id`, `retry`).
const EVENT_FIELDS: &[&str] = &["data", "event", "id", "retry"];

/// Keywords an event `itemSchema` may use besides `properties`.
const EVENT_KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "title",
    "description",
    "$comment",
];

fn essence(media_type: &str) -> String {
    media_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn is_json(media_type: &str) -> bool {
    let essence = essence(media_type);
    essence == "application/json" || essence.ends_with("+json")
}

/// Where a response media type's OpenAPI 3.2 `itemSchema` keeps the type of
/// one item, relative to the Media Type Object, when the compiler supports
/// it:
///
/// - JSON Lines (`application/jsonl` and its aliases): `/itemSchema` is the
///   type of one line, as `schema` is for these media types in 3.1.
/// - `text/event-stream` whose item is an event object with a `data` string
///   of JSON (`contentMediaType` JSON plus `contentSchema`):
///   `/itemSchema/properties/data/contentSchema` is the event type. The
///   other event fields (`event`, `id`, `retry`) carry no type.
///
/// `None` for every other use, including a media type that declares
/// `schema` too; TG0145 reports those.
pub fn response_item_schema(media_type: &str, media: &Map<String, Value>) -> Option<&'static str> {
    let item = media.get("itemSchema")?;
    if media.get("schema").is_some() || !item.is_object() {
        return None;
    }
    let essence = essence(media_type);
    if JSON_LINES.contains(&essence.as_str()) {
        return Some("/itemSchema");
    }
    if essence != "text/event-stream" {
        return None;
    }
    let item = item.as_object()?;
    let shaped = item.keys().all(|k| EVENT_KEYWORDS.contains(&k.as_str()))
        && item
            .get("type")
            .is_none_or(|t| t.as_str() == Some("object"));
    let properties = item.get("properties")?.as_object()?;
    if !shaped
        || !properties
            .keys()
            .all(|k| EVENT_FIELDS.contains(&k.as_str()))
    {
        return None;
    }
    let data = properties.get("data")?.as_object()?;
    let json = data
        .get("contentMediaType")
        .and_then(Value::as_str)
        .is_some_and(is_json);
    let string = data
        .get("type")
        .is_none_or(|t| t.as_str() == Some("string"));
    (json && string && data.contains_key("contentSchema"))
        .then_some("/itemSchema/properties/data/contentSchema")
}

const REMOVE: &str = "use only OpenAPI 3.1 constructs, or declare the document as OpenAPI 3.1";
const IGNORED: &str = "the compiler does not use it; remove it to silence this warning";

fn error(code: &'static str, pointer: String, message: &str, help: &'static str) -> Finding {
    Finding {
        code,
        warning: false,
        pointer,
        message: message.to_string(),
        help,
    }
}

fn warning(code: &'static str, pointer: String, message: &str) -> Finding {
    Finding {
        code,
        warning: true,
        pointer,
        message: message.to_string(),
        help: IGNORED,
    }
}

/// Findings of a whole 3.2 document, in document order. `root` is the
/// document after overlays.
pub(crate) fn scan_document(root: &Value) -> Vec<Finding> {
    let mut out = vec![];
    let Value::Object(doc) = root else {
        return out;
    };
    if doc.contains_key("$self") {
        out.push(error(
            "TG0140",
            "/$self".into(),
            "the OpenAPI 3.2 `$self` document identity is not supported",
            "remove `$self`; relative references resolve against the file location",
        ));
    }
    let schemes = scheme_names(doc);
    if let Some(Value::Array(tags)) = doc.get("tags") {
        for (i, tag) in tags.iter().enumerate() {
            if let Value::Object(tag) = tag {
                tag_findings(&join_pointer("/tags", &i.to_string()), tag, &mut out);
            }
        }
    }
    server_findings("/servers", doc.get("servers"), &mut out);
    requirements(doc.get("security"), "/security", &schemes, &mut out);
    walk(root, "", Kind::Document, |node| {
        node_findings(node, &schemes, &mut out);
        true
    });
    out
}

/// Findings of the subtree of a fragment at `pointer`, read as `kind`.
/// Security names use the input entry's component names, as in the builder;
/// a fragment's own members cannot introduce a different namespace. Whole
/// document-only members are checked separately by `scan_document`.
pub(crate) fn scan_subtree(
    value: &Value,
    pointer: &str,
    kind: Kind,
    entry: &Value,
) -> Vec<Finding> {
    let mut out = vec![];
    let schemes = entry.as_object().map(scheme_names).unwrap_or_default();
    walk(value, pointer, kind, |node| {
        node_findings(node, &schemes, &mut out);
        true
    });
    out
}

/// Remember which findings were already reported, so that a subtree reached
/// by several references reports each construct once.
#[derive(Debug, Default)]
pub(crate) struct Reported(HashSet<(usize, String, &'static str)>);

impl Reported {
    pub(crate) fn first(&mut self, doc: usize, finding: &Finding) -> bool {
        self.0.insert((doc, finding.pointer.clone(), finding.code))
    }
}

fn scheme_names(doc: &Map<String, Value>) -> BTreeSet<String> {
    doc.get("components")
        .and_then(|c| c.get("securitySchemes"))
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

fn tag_findings(pointer: &str, tag: &Map<String, Value>, out: &mut Vec<Finding>) {
    for key in ["parent", "kind"] {
        if tag.contains_key(key) {
            out.push(warning(
                "TG0153",
                join_pointer(pointer, key),
                &format!("the OpenAPI 3.2 tag `{key}` field (tag hierarchy) is ignored"),
            ));
        }
    }
    if tag.contains_key("summary") {
        out.push(warning(
            "TG0156",
            join_pointer(pointer, "summary"),
            "the OpenAPI 3.2 tag `summary` field is ignored",
        ));
    }
}

fn server_findings(pointer: &str, servers: Option<&Value>, out: &mut Vec<Finding>) {
    let Some(Value::Array(servers)) = servers else {
        return;
    };
    for (i, server) in servers.iter().enumerate() {
        if server.get("name").is_some() {
            out.push(warning(
                "TG0156",
                join_pointer(&join_pointer(pointer, &i.to_string()), "name"),
                "the OpenAPI 3.2 server `name` field is ignored",
            ));
        }
    }
}

/// A security requirement key that is not a component name but looks like a
/// URI reference names a Security Scheme Object by location (3.2).
fn requirements(
    value: Option<&Value>,
    pointer: &str,
    schemes: &BTreeSet<String>,
    out: &mut Vec<Finding>,
) {
    let Some(Value::Array(list)) = value else {
        return;
    };
    for (i, requirement) in list.iter().enumerate() {
        let Value::Object(requirement) = requirement else {
            continue;
        };
        for key in requirement.keys() {
            let by_uri = !schemes.contains(key)
                && !key.starts_with("x-")
                && (key.starts_with('.') || key.contains([':', '/', '#']));
            if by_uri {
                out.push(error(
                    "TG0151",
                    join_pointer(&join_pointer(pointer, &i.to_string()), key),
                    &format!(
                        "OpenAPI 3.2 security requirement `{key}` names a security scheme by URI, which is not supported"
                    ),
                    "name a scheme declared under `components.securitySchemes`",
                ));
            }
        }
    }
}

fn node_findings(node: &Node<'_>, schemes: &BTreeSet<String>, out: &mut Vec<Finding>) {
    let map = node.value;
    let at = |key: &str| join_pointer(&node.pointer, key);
    match node.kind {
        Kind::Document => {}
        Kind::PathItem => {
            if let Some(extra) = map.get("additionalOperations") {
                additional_operations(&at("additionalOperations"), extra, out);
            }
            server_findings(&at("servers"), map.get("servers"), out);
        }
        Kind::Operation => {
            server_findings(&at("servers"), map.get("servers"), out);
            requirements(map.get("security"), &at("security"), schemes, out);
        }
        Kind::Parameter => {
            if map.get("in").and_then(Value::as_str) == Some("querystring") {
                out.push(error(
                    "TG0143",
                    at("in"),
                    "the OpenAPI 3.2 `querystring` parameter location is not supported",
                    "describe the query as separate `query` parameters",
                ));
            }
            if map.get("style").and_then(Value::as_str) == Some("cookie") {
                out.push(error(
                    "TG0144",
                    at("style"),
                    "the OpenAPI 3.2 `cookie` parameter style is not supported",
                    "use `style: form`, which 3.1 defines for cookie parameters",
                ));
            }
        }
        Kind::MediaType => {
            let supported = node.parent == Some(Kind::Response)
                && node.pointer.rsplit_once('/').is_some_and(|(_, key)| {
                    let media_type = key.replace("~1", "/").replace("~0", "~");
                    response_item_schema(&media_type, map).is_some()
                });
            if map.contains_key("itemSchema") && !supported {
                out.push(error(
                    "TG0145",
                    at("itemSchema"),
                    "this use of OpenAPI 3.2 `itemSchema` (sequential and streaming media types) is not supported",
                    "only response JSON Lines items and server-sent events whose `data` has a JSON `contentSchema`, written in the response's `content` map, are compiled; otherwise describe the whole payload with `schema`",
                ));
            }
            for key in ["prefixEncoding", "itemEncoding"] {
                if map.contains_key(key) {
                    out.push(error(
                        "TG0146",
                        at(key),
                        &format!(
                            "OpenAPI 3.2 positional multipart encoding (`{key}`) is not supported"
                        ),
                        "name the parts with `encoding`, which 3.1 defines",
                    ));
                }
            }
        }
        Kind::Encoding => {
            for key in ["encoding", "prefixEncoding", "itemEncoding"] {
                if map.contains_key(key) {
                    out.push(error(
                        "TG0147",
                        at(key),
                        &format!(
                            "OpenAPI 3.2 nested encoding (`{key}` in an Encoding Object) is not supported"
                        ),
                        "describe the part as one opaque value with a `contentType`",
                    ));
                }
            }
        }
        Kind::Response => {
            if map.contains_key("summary") && !map.contains_key("$ref") {
                out.push(warning(
                    "TG0156",
                    at("summary"),
                    "the OpenAPI 3.2 response `summary` field is ignored",
                ));
            }
        }
        Kind::Example => {
            for key in ["dataValue", "serializedValue"] {
                if map.contains_key(key) {
                    out.push(warning(
                        "TG0154",
                        at(key),
                        &format!("the OpenAPI 3.2 example `{key}` field is ignored"),
                    ));
                }
            }
        }
        Kind::Link => server_findings_single(&node.pointer, map.get("server"), out),
        Kind::SecurityScheme => {
            if let Some(Value::Object(flows)) = map.get("flows")
                && flows.contains_key("deviceAuthorization")
            {
                out.push(error(
                    "TG0150",
                    join_pointer(&at("flows"), "deviceAuthorization"),
                    "the OpenAPI 3.2 OAuth 2.0 device authorization flow is not supported",
                    "declare a flow tungsten supports, or obtain the token outside the client",
                ));
            }
            if map.contains_key("oauth2MetadataUrl") {
                out.push(warning(
                    "TG0152",
                    at("oauth2MetadataUrl"),
                    "the OpenAPI 3.2 `oauth2MetadataUrl` is ignored; no server metadata is discovered",
                ));
            }
            if map.contains_key("deprecated") {
                out.push(warning(
                    "TG0156",
                    at("deprecated"),
                    "the OpenAPI 3.2 security scheme `deprecated` field is ignored",
                ));
            }
        }
        Kind::Schema => {
            if let Some(Value::Object(discriminator)) = map.get("discriminator")
                && discriminator.contains_key("defaultMapping")
            {
                out.push(error(
                    "TG0149",
                    join_pointer(&at("discriminator"), "defaultMapping"),
                    "the OpenAPI 3.2 discriminator `defaultMapping` is not supported",
                    REMOVE,
                ));
            }
            if let Some(Value::Object(xml)) = map.get("xml")
                && xml.contains_key("nodeType")
            {
                out.push(warning(
                    "TG0155",
                    join_pointer(&at("xml"), "nodeType"),
                    "the OpenAPI 3.2 XML `nodeType` field is ignored",
                ));
            }
        }
        Kind::Header | Kind::RequestBody | Kind::Callback => {}
    }
}

/// Path item members that are operations; `additionalOperations` must not
/// repeat them.
const FIXED_METHODS: [&str; 9] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace", "query",
];

/// Why `token` cannot be compiled as an `additionalOperations` method, or
/// `None` when it is sent as written.
pub fn custom_method_problem(token: &str) -> Option<String> {
    let tchar = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c);
    if token.is_empty() || !token.chars().all(tchar) {
        return Some(format!(
            "`{token}` is not an HTTP method token (RFC 9110 `tchar` characters only)"
        ));
    }
    if let Some(fixed) = FIXED_METHODS.iter().find(|m| m.eq_ignore_ascii_case(token)) {
        return Some(format!(
            "`{token}` must be described by the path item's `{fixed}` field, not by `additionalOperations`"
        ));
    }
    if token.eq_ignore_ascii_case("CONNECT") || token.eq_ignore_ascii_case("TRACK") {
        return Some(format!(
            "`{token}` is a forbidden method (it opens a tunnel or traces the connection, not a resource); generated clients cannot send it"
        ));
    }
    None
}

/// Each `additionalOperations` entry is compiled with its key as the wire
/// method; only keys that cannot be sent are refused.
fn additional_operations(pointer: &str, extra: &Value, out: &mut Vec<Finding>) {
    let Value::Object(extra) = extra else {
        out.push(error(
            "TG0142",
            pointer.to_string(),
            "OpenAPI 3.2 `additionalOperations` must be a map from HTTP method to operation",
            "write a map such as `additionalOperations: { COPY: { ... } }`",
        ));
        return;
    };
    for token in extra.keys() {
        if let Some(problem) = custom_method_problem(token) {
            out.push(error(
                "TG0142",
                join_pointer(pointer, token),
                &format!("OpenAPI 3.2 `additionalOperations` method {problem}; the operation would be left out"),
                "use the method token the server expects, or the matching fixed path item field",
            ));
        }
    }
}

fn server_findings_single(pointer: &str, server: Option<&Value>, out: &mut Vec<Finding>) {
    if server.is_some_and(|s| s.get("name").is_some()) {
        out.push(warning(
            "TG0156",
            join_pointer(&join_pointer(pointer, "server"), "name"),
            "the OpenAPI 3.2 server `name` field is ignored",
        ));
    }
}
