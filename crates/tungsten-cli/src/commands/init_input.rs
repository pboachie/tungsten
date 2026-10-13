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
    .map_err(|e| e.to_string())?;
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

/// Detect a Swagger document and persist its bounded conversion for the ordinary
/// compiler. OpenAPI input is left in place so relative references retain meaning.
pub(super) fn import(args: &mut InitArgs) -> Result<Vec<String>, String> {
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
        return Ok(vec![]);
    };
    let mut bytes = Vec::new();
    std::fs::File::open(&source)
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
    let root = tungsten_config::parse_yaml("input", text)
        .0
        .unwrap_or(serde_json::Value::Null);
    let is_curl = args.input_format == "curl"
        || (args.input_format == "auto"
            && (text.trim_start().starts_with("curl ")
                || text.trim_start().starts_with("curl.exe ")
                || source.extension().is_some_and(|ext| ext == "curl")));
    let is_http = args.input_format == "http"
        || (args.input_format == "auto" && source.extension().is_some_and(|ext| ext == "http"));
    let mut import_report = None;
    let mut converted = if is_curl || is_http {
        let limits = Limits::default();
        let evidence = if is_curl {
            if args.route.len() != 1 {
                return Err(
                    "saved cURL requires exactly one explicit --route path template".into(),
                );
            }
            let dialect = match args.shell_dialect.as_str() {
                "cmd" => tungsten_contract::request_import::ShellDialect::WindowsCmd,
                "powershell" => tungsten_contract::request_import::ShellDialect::PowerShell,
                "posix" => tungsten_contract::request_import::ShellDialect::Posix,
                _ => return Err("shell dialect must be posix, cmd or powershell".into()),
            };
            tungsten_contract::request_import::curl(text, dialect, &args.route[0], &limits)
        } else {
            tungsten_contract::request_import::http(
                text,
                &args.route,
                &std::collections::BTreeMap::new(),
                &limits,
            )
        }
        .map_err(|error| error.to_string())?;
        let output = tungsten_contract::infer(&evidence, &ReviewedOverrides::default(), &limits)
            .map_err(|error| error.to_string())?;
        import_report = Some(serde_json::json!({"contract": output.report}));
        output.openapi
    } else if root.get("log").is_some() {
        let limits = Limits::default();
        let options = tungsten_contract::har::HarOptions {
            origin: args.har_origin.clone(),
            include_prefixes: args.include_prefix.clone(),
            exclude_prefixes: args.exclude_prefix.clone(),
            redact_properties: args.redact_property.iter().cloned().collect(),
            ..Default::default()
        };
        let imported = tungsten_contract::har::import_har(&bytes, &options, &limits)
            .map_err(|error| error.to_string())?;
        let output = imported
            .infer(&ReviewedOverrides::default(), &limits)
            .map_err(|error| error.to_string())?;
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
        .map_err(|error| error.to_string())?;
        let output = imported
            .infer(&ReviewedOverrides::default(), &limits)
            .map_err(|error| error.to_string())?;
        import_report =
            Some(serde_json::json!({"source": imported.report, "contract": output.report}));
        output.openapi
    } else if root.get("endpoint_manifest").is_some() {
        let canonical =
            std::fs::canonicalize(&source).map_err(|_| "cannot resolve endpoint manifest")?;
        let allowed = canonical
            .parent()
            .ok_or("endpoint manifest needs a parent directory")?
            .to_path_buf();
        tungsten_openapi::endpoint_manifest::load(
            &canonical,
            &[allowed],
            &tungsten_openapi::LoadOptions::default(),
        )
        .map_err(|error| format!("endpoint manifest import failed: {}", error.message))?
    } else if root.get("swagger").is_some() {
        tungsten_openapi::swagger::convert(&root)
            .map_err(|error| format!("Swagger conversion failed: {}", error.message))?
    } else {
        return Ok(vec![]);
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
        serde_json::to_string_pretty(&converted)
            .map_err(|_| "cannot encode converted input")?
            .as_bytes(),
    )
    .map_err(|_| "cannot write converted input; inspect partial files before retrying")?;
    let mut files = vec!["imported.openapi.json".to_owned()];
    if let Some(report) = import_report {
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
