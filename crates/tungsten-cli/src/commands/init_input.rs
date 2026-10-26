// SPDX-License-Identifier: AGPL-3.0-only
//! Prepare no-schema structural evidence using the shared offline engine.
use crate::args::InitArgs;
use std::io::{Read, Write};
use std::path::Path;
use tungsten_contract::{
    EvidenceDocument, Frontend, HttpEvidence, Limits, MediaEvidence, ParameterEvidence,
    ParameterLocation, ResponseEvidence, ReviewedOverrides, ShapeEvidence, SourceLocation,
    UnknownFact,
};

/// An importer diagnostic as a message that names `tungsten explain`.
fn hinted(d: &tungsten_contract::Diagnostic) -> String {
    format!("{d} (see `tungsten explain {}`)", d.code)
}

fn har_paths(
    args: &ConvertOptions<'_>,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let Some(path) = args.har_paths else {
        return Ok(args.har_path_map.cloned().unwrap_or_default());
    };
    if !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        return Err("HAR path mapping must be a regular JSON file".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| "cannot open HAR path mapping")?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read HAR path mapping")?;
    if bytes.len() > 1024 * 1024 {
        return Err("HAR path mapping exceeds 1 MiB; split the capture and mapping".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        "HAR path mapping must be a JSON object of captured URL paths to reviewed template strings"
            .into()
    })
}

fn example(path: &Path) -> Result<Vec<MediaEvidence>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| "cannot open example file")?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read example file")?;
    if bytes.len() > 1024 * 1024 {
        return Err("example exceeds 1 MiB; provide a smaller structural example".into());
    }
    let value = serde_json::from_slice(&bytes).map_err(|_| "example must contain valid JSON")?;
    Ok(vec![MediaEvidence {
        media_type: "application/json".into(),
        shape: ShapeEvidence {
            samples: vec![value],
            declared_schema: None,
        },
    }])
}

pub(super) fn prepare(args: &mut InitArgs) -> Result<(), String> {
    let path = args
        .endpoint
        .clone()
        .ok_or("endpoint path template is required")?;
    if !path.starts_with('/') || path.starts_with("//") || path.contains(['?', '#', '@']) {
        return Err(
            "endpoint must be a path template without origin, credentials, query or fragment"
                .into(),
        );
    }
    for name in [
        "tungsten.yml",
        "agent.yml",
        "inferred.openapi.json",
        "inference-report.json",
    ] {
        if std::fs::symlink_metadata(args.dir.join(name)).is_ok() {
            return Err(
                "no-schema setup requires a new project; resume the existing configuration instead"
                    .into(),
            );
        }
    }
    // The supplied template declares string path parameters; requiredness is
    // intrinsic to path parameters. The engine validates syntax and uniqueness.
    let parameters = path
        .split('/')
        .filter_map(|segment| {
            segment
                .strip_prefix('{')
                .and_then(|s| s.strip_suffix('}'))
                .map(|name| ParameterEvidence {
                    name: name.into(),
                    location: ParameterLocation::Path,
                    shape: ShapeEvidence {
                        samples: vec![],
                        declared_schema: Some(serde_json::json!({"type":"string"})),
                    },
                })
        })
        .collect();
    let operation = HttpEvidence {
        method: args.method.clone().ok_or("HTTP method is required")?,
        path,
        source: SourceLocation {
            document: "manual".into(),
            line: None,
            column: None,
        },
        parameters,
        request: args
            .request_example
            .as_deref()
            .map(example)
            .transpose()?
            .unwrap_or_default(),
        responses: vec![ResponseEvidence {
            status: 200,
            content: args
                .response_example
                .as_deref()
                .map(example)
                .transpose()?
                .unwrap_or_default(),
        }],
        unknown: [
            UnknownFact::Authentication,
            UnknownFact::Authorization,
            UnknownFact::Safety,
            UnknownFact::Requiredness,
            UnknownFact::Completeness,
        ]
        .into_iter()
        .collect(),
    };
    let output = tungsten_contract::infer(
        &EvidenceDocument {
            version: 1,
            frontend: Frontend::Http,
            operations: vec![operation],
        },
        &ReviewedOverrides::default(),
        &Limits::default(),
    )
    .map_err(|e| hinted(&e))?;
    std::fs::create_dir_all(&args.dir).map_err(|_| "cannot create project directory")?;
    // Exclusive opens refuse symlinks and races. On failure keep the created evidence
    // for inspection; never claim completed initialization or overwrite a conflict.
    for (name, value) in [
        ("inferred.openapi.json", output.openapi),
        (
            "inference-report.json",
            serde_json::to_value(output.report).map_err(|_| "cannot encode inference report")?,
        ),
    ] {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(args.dir.join(name))
            .map_err(
                |_| "cannot create evidence file; inspect the project and choose a fresh directory",
            )?;
        file.write_all(
            serde_json::to_string_pretty(&value)
                .map_err(|_| "cannot encode inferred contract")?
                .as_bytes(),
        )
        .map_err(|_| "cannot write evidence; inspect partial files before retrying")?;
    }
    args.from = Some(args.dir.join("inferred.openapi.json"));
    Ok(())
}

/// Parse the input for format detection. JSON input is read with serde_json, whose
/// recursion limit fails cleanly, so a deep Postman or Swagger document is never
/// mistaken for OpenAPI because the YAML reader stopped early; its error is surfaced.
fn detect_root(text: &str) -> Result<serde_json::Value, String> {
    let body = text.trim_start_matches('\u{feff}').trim_start();
    if body.starts_with('{') || body.starts_with('[') {
        let error = match serde_json::from_str(body) {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        // A YAML flow mapping that is not strict JSON is still accepted as YAML.
        if let Ok(value) = tungsten_config::parse_yaml("input", text).0 {
            return Ok(value);
        }
        return Err(format!(
            "input looks like JSON but cannot be read (invalid syntax or nesting deeper than 128 levels): {error}"
        ));
    }
    Ok(tungsten_config::parse_yaml("input", text)
        .0
        .unwrap_or(serde_json::Value::Null))
}

/// Number of items a Postman import omitted, from the saved `import-report.json`.
pub(super) fn omitted_count(dir: &std::path::Path) -> usize {
    std::fs::read(dir.join("import-report.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|report| report.pointer("/source/omitted")?.as_array().map(Vec::len))
        .unwrap_or(0)
}

/// A conversion held in memory until setup has checked the selected targets.
pub(super) struct PreparedImport {
    document: serde_json::Value,
    report: Option<serde_json::Value>,
}

/// How a source is converted: the importer settings of `init` that
/// `refresh-import` repeats for a new revision of the same source.
pub(super) struct ConvertOptions<'a> {
    pub input_format: &'a str,
    pub route: &'a [String],
    pub shell_dialect: &'a str,
    pub har_origin: Option<&'a str>,
    pub har_paths: Option<&'a Path>,
    /// Reviewed HAR path templates recorded by an earlier conversion; used
    /// when `har_paths` names no file.
    pub har_path_map: Option<&'a std::collections::BTreeMap<String, String>>,
    pub include_prefix: &'a [String],
    pub exclude_prefix: &'a [String],
    pub redact_property: &'a [String],
}

impl PreparedImport {
    pub(super) fn document(&self) -> &serde_json::Value {
        &self.document
    }
    pub(super) fn report(&self) -> Option<&serde_json::Value> {
        self.report.as_ref()
    }
}

/// Detect and convert an input without writing project files. OpenAPI input is
/// left in place so relative references retain meaning.
pub(super) fn import(args: &InitArgs) -> Result<Option<PreparedImport>, String> {
    let source = args.from.clone().or_else(|| {
        [
            "openapi.json",
            "openapi.yaml",
            "openapi.yml",
            "swagger.json",
            "swagger.yaml",
        ]
        .iter()
        .map(|name| args.dir.join(name))
        .find(|path| path.is_file())
    });
    let Some(source) = source else {
        return Ok(None);
    };
    convert(
        &source,
        &ConvertOptions {
            input_format: &args.input_format,
            route: &args.route,
            shell_dialect: &args.shell_dialect,
            har_origin: args.har_origin.as_deref(),
            har_paths: args.har_paths.as_deref(),
            har_path_map: None,
            include_prefix: &args.include_prefix,
            exclude_prefix: &args.exclude_prefix,
            redact_property: &args.redact_property,
        },
    )
}

/// Convert one source file; `None` when it is OpenAPI and needs no conversion.
pub(super) fn convert(
    source: &Path,
    args: &ConvertOptions<'_>,
) -> Result<Option<PreparedImport>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(source)
        .map_err(|_| "cannot open input file")?
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read input file")?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(
            "setup input exceeds 8 MiB; use the explicit compiler flow for larger OpenAPI files"
                .into(),
        );
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| "input must be UTF-8 JSON or YAML")?;
    let root = detect_root(text)?;
    let is_curl = args.input_format == "curl"
        || (args.input_format == "auto"
            && (text.trim_start().starts_with("curl ")
                || text.trim_start().starts_with("curl.exe ")
                || source.extension().is_some_and(|ext| ext == "curl")));
    let is_http = args.input_format == "http"
        || (args.input_format == "auto"
            && source
                .extension()
                .is_some_and(|ext| matches!(ext.to_str(), Some("http" | "rest"))));
    let mut import_report = None;
    // Reviewed importer settings, saved in the report so `refresh-import`
    // converts a new revision of the source the same way.
    let mut settings = serde_json::Map::new();
    let mut converted = if is_curl || is_http {
        let limits = Limits::default();
        let evidence = if is_curl {
            if args.route.len() != 1 {
                return Err(
                    "saved cURL requires exactly one explicit --route path template".into(),
                );
            }
            let dialect = match args.shell_dialect {
                "cmd" => tungsten_contract::request_import::ShellDialect::WindowsCmd,
                "powershell" => tungsten_contract::request_import::ShellDialect::PowerShell,
                "posix" => tungsten_contract::request_import::ShellDialect::Posix,
                _ => return Err("shell dialect must be posix, cmd or powershell".into()),
            };
            tungsten_contract::request_import::curl(text, dialect, &args.route[0], &limits)
        } else {
            tungsten_contract::request_import::http(
                text,
                args.route,
                &std::collections::BTreeMap::new(),
                &limits,
            )
        }
        .map_err(|error| hinted(&error))?;
        let output = tungsten_contract::infer(&evidence, &ReviewedOverrides::default(), &limits)
            .map_err(|error| hinted(&error))?;
        import_report = Some(serde_json::json!({"contract": output.report}));
        settings.insert(
            "input_format".into(),
            (if is_curl { "curl" } else { "http" }).into(),
        );
        settings.insert("route".into(), args.route.into());
        if is_curl {
            settings.insert("shell_dialect".into(), args.shell_dialect.into());
        }
        output.openapi
    } else if root.get("log").is_some() {
        let limits = Limits::default();
        let path_templates = har_paths(args)?;
        for (key, value) in [
            ("route", args.route),
            ("include_prefix", args.include_prefix),
            ("exclude_prefix", args.exclude_prefix),
            ("redact_property", args.redact_property),
        ] {
            if !value.is_empty() {
                settings.insert(key.into(), value.into());
            }
        }
        if let Some(origin) = args.har_origin {
            settings.insert("har_origin".into(), origin.into());
        }
        if !path_templates.is_empty() {
            settings.insert("har_paths".into(), serde_json::json!(path_templates));
        }
        let options = tungsten_contract::har::HarOptions {
            origin: args.har_origin.map(str::to_owned),
            path_templates,
            include_prefixes: args.include_prefix.to_vec(),
            exclude_prefixes: args.exclude_prefix.to_vec(),
            redact_properties: args.redact_property.iter().cloned().collect(),
            route_templates: args.route.to_vec(),
            ..Default::default()
        };
        let imported = tungsten_contract::har::import_har(&bytes, &options, &limits)
            .map_err(|error| hinted(&error))?;
        // Captured paths may hold identifiers or secrets; never persist one unreviewed.
        let literal = imported
            .report
            .observations
            .values()
            .filter(|notes| {
                notes.contains(&tungsten_contract::har::CaptureNote::LiteralPathNeedsReview)
            })
            .count();
        if literal > 0 {
            return Err(format!(
                "{literal} captured HAR path(s) need review before saving: pass --route templates (for example --route '/items/{{id}}', or --route /health for a literal route) or --har-paths <JSON> mapping each captured path to a reviewed template, or narrow the capture with --include-prefix/--exclude-prefix"
            ));
        }
        let output = imported
            .infer(&ReviewedOverrides::default(), &limits)
            .map_err(|error| hinted(&error))?;
        import_report =
            Some(serde_json::json!({"source": output.report, "contract": output.contract.report}));
        output.contract.openapi
    } else if root.get("item").is_some() && root.get("info").is_some() {
        let limits = Limits::default();
        let imported = tungsten_contract::postman::import_collection(
            &bytes,
            &tungsten_contract::postman::PostmanOptions::default(),
            &limits,
        )
        .map_err(|error| hinted(&error))?;
        let output = imported
            .infer(&ReviewedOverrides::default(), &limits)
            .map_err(|error| hinted(&error))?;
        import_report =
            Some(serde_json::json!({"source": imported.report, "contract": output.report}));
        output.openapi
    } else if root.get("endpoint_manifest").is_some() {
        let canonical =
            std::fs::canonicalize(source).map_err(|_| "cannot resolve endpoint manifest")?;
        let allowed = canonical
            .parent()
            .ok_or("endpoint manifest needs a parent directory")?
            .to_path_buf();
        tungsten_openapi::endpoint_manifest::load(
            &canonical,
            &[allowed],
            &tungsten_openapi::LoadOptions::default(),
        )
        .map_err(|error| {
            format!(
                "endpoint manifest import failed: {}",
                error.render_short(None)
            )
        })?
    } else if root.get("swagger").is_some() {
        tungsten_openapi::swagger::convert(&root).map_err(|error| {
            format!(
                "Swagger conversion failed at {}#{}: {}",
                source.display(),
                error.pointer,
                error.message
            )
        })?
    } else {
        return Ok(None);
    };
    if let Some(paths) = converted
        .get_mut("paths")
        .and_then(serde_json::Value::as_object_mut)
    {
        for item in paths
            .values_mut()
            .filter_map(serde_json::Value::as_object_mut)
        {
            for method in [
                "get", "post", "put", "patch", "delete", "head", "options", "trace",
            ] {
                if let Some(op) = item
                    .get_mut(method)
                    .and_then(serde_json::Value::as_object_mut)
                {
                    op.insert("x-tungsten-imported".into(), serde_json::Value::Bool(true));
                    // Imported annotations cannot review their own safety or origins.
                    op.remove("x-tungsten-reviewed-safety");
                    op.remove("x-tungsten-reviewed-origins");
                }
            }
        }
    }
    if let Some(report) = import_report.as_mut().filter(|_| !settings.is_empty()) {
        report["settings"] = serde_json::Value::Object(settings);
    }
    Ok(Some(PreparedImport {
        document: converted,
        report: import_report,
    }))
}

/// Persist a checked conversion using the same exclusive writes as other setup
/// artifacts. Target capability refusal happens before this function is called.
pub(super) fn save_import(
    args: &mut InitArgs,
    prepared: PreparedImport,
) -> Result<Vec<String>, String> {
    for name in [
        "tungsten.yml",
        "agent.yml",
        "imported.openapi.json",
        "import-report.json",
    ] {
        if std::fs::symlink_metadata(args.dir.join(name)).is_ok() {
            return Err("input conversion requires a new project; resume the existing converted configuration".into());
        }
    }
    std::fs::create_dir_all(&args.dir).map_err(|_| "cannot create project directory")?;
    let destination = args.dir.join("imported.openapi.json");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .map_err(|_| "cannot create converted input; choose a fresh project directory")?;
    file.write_all(
        serde_json::to_string_pretty(&prepared.document)
            .map_err(|_| "cannot encode converted input")?
            .as_bytes(),
    )
    .map_err(|_| "cannot write converted input; inspect partial files before retrying")?;
    let mut files = vec!["imported.openapi.json".to_owned()];
    if let Some(report) = prepared.report {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(args.dir.join("import-report.json"))
            .map_err(
                |_| "cannot create import report; inspect partial setup files before retrying",
            )?;
        file.write_all(
            serde_json::to_string_pretty(&report)
                .map_err(|_| "cannot encode import report")?
                .as_bytes(),
        )
        .map_err(|_| "cannot write import report")?;
        files.push("import-report.json".to_owned());
    }
    args.from = Some(destination);
    Ok(files)
}
