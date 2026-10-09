// SPDX-License-Identifier: AGPL-3.0-only
//! OpenAPI version detection and required top-level fields.

use serde_json::Value;
use tungsten_core::Diagnostic;

use crate::spans::SpanIndex;
use crate::{LoadOptions, SpecVersion};

/// Detect the version of an entry document and check the fields every
/// document of that version needs. Errors: TG0103 (unsupported version,
/// including Swagger 2.0) and TG0104 (missing required field). A missing
/// `info.version` is only a TG0110 warning, returned with the version when
/// nothing else is wrong; the compiler then uses `0.0.0`.
pub(crate) fn detect(
    root: &Value,
    file: &str,
    spans: &SpanIndex,
    opts: &LoadOptions,
) -> Result<(SpecVersion, Vec<Diagnostic>), Vec<Diagnostic>> {
    let at = |d: Diagnostic, pointer: &str| d.at(file, pointer, spans.nearest(pointer));
    let Value::Object(doc) = root else {
        return Err(vec![at(
            Diagnostic::error("TG0104", "an OpenAPI document must be an object"),
            "",
        )]);
    };
    if let Some(swagger) = doc.get("swagger") {
        let declared = swagger
            .as_str()
            .map_or_else(|| swagger.to_string(), str::to_string);
        let help = if opts.convert_swagger2 {
            "Swagger 2.0 conversion is not yet supported in this version; convert the document to OpenAPI 3.x first"
        } else {
            "this version of tungsten does not convert Swagger 2.0; convert the document to OpenAPI 3.x first (for example with swagger2openapi)"
        };
        return Err(vec![at(
            Diagnostic::error(
                "TG0103",
                format!("Swagger {declared} documents are not supported"),
            )
            .with_help(help),
            "/swagger",
        )]);
    }
    let mut errors = vec![];
    let mut warnings = vec![];
    let version = match doc.get("openapi") {
        None => {
            errors.push(at(
                Diagnostic::error("TG0104", "missing required field `openapi`"),
                "",
            ));
            None
        }
        Some(Value::String(v)) => match parse_version(v) {
            Some(version) => Some(version),
            None => {
                errors.push(at(
                    Diagnostic::error("TG0103", format!("unsupported OpenAPI version {v}"))
                        .with_help("supported versions are 3.0.x and 3.1.x"),
                    "/openapi",
                ));
                None
            }
        },
        Some(other) => {
            errors.push(at(
                Diagnostic::error(
                    "TG0103",
                    format!("`openapi` must be a version string, found {other}"),
                )
                .with_help("quote the version, for example `openapi: \"3.1.0\"`"),
                "/openapi",
            ));
            None
        }
    };
    match doc.get("info") {
        None => errors.push(at(
            Diagnostic::error("TG0104", "missing required field `info`"),
            "",
        )),
        Some(Value::Object(info)) => {
            if !info.get("title").is_some_and(Value::is_string) {
                errors.push(at(
                    Diagnostic::error("TG0104", "missing required string field `info.title`"),
                    "/info",
                ));
            }
            match info.get("version") {
                Some(Value::String(v)) if !v.trim().is_empty() => {}
                None | Some(Value::Null) | Some(Value::String(_)) => warnings.push(at(
                    Diagnostic::warning("TG0110", "missing `info.version`; using version 0.0.0")
                        .with_help("add `info.version` to the document or an overlay"),
                    "/info",
                )),
                Some(_) => errors.push(at(
                    Diagnostic::error("TG0104", "missing required string field `info.version`"),
                    "/info",
                )),
            }
        }
        Some(_) => errors.push(at(
            Diagnostic::error("TG0104", "`info` must be an object"),
            "/info",
        )),
    }
    match &version {
        Some(SpecVersion::V30(_)) if !doc.contains_key("paths") => errors.push(at(
            Diagnostic::error("TG0104", "missing required field `paths` (OpenAPI 3.0)"),
            "",
        )),
        Some(SpecVersion::V31(_))
            if !["paths", "webhooks", "components"]
                .iter()
                .any(|k| doc.contains_key(*k)) =>
        {
            errors.push(at(
                Diagnostic::error(
                    "TG0104",
                    "an OpenAPI 3.1 document needs at least one of `paths`, `webhooks` or `components`",
                ),
                "",
            ));
        }
        _ => {}
    }
    for key in ["paths", "webhooks", "components"] {
        if doc.get(key).is_some_and(|v| !v.is_object()) {
            errors.push(at(
                Diagnostic::error("TG0104", format!("`{key}` must be an object")),
                &format!("/{key}"),
            ));
        }
    }
    match version {
        Some(v) if errors.is_empty() => Ok((v, warnings)),
        _ => Err(errors),
    }
}

/// `3.0`, `3.0.N` or `3.0.N-suffix` (and the same for 3.1).
fn parse_version(v: &str) -> Option<SpecVersion> {
    let core = v.split_once('-').map_or(v, |(c, _)| c);
    let mut parts = core.split('.');
    let (major, minor, patch) = (parts.next()?, parts.next()?, parts.next());
    if parts.next().is_some() || major != "3" {
        return None;
    }
    if patch.is_some_and(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    match minor {
        "0" => Some(SpecVersion::V30(v.to_string())),
        "1" => Some(SpecVersion::V31(v.to_string())),
        _ => None,
    }
}
