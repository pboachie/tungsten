// SPDX-License-Identifier: AGPL-3.0-only
//! Prepare no-schema structural evidence using the shared offline engine.
use crate::args::InitArgs;
use std::io::{Read, Write};
use std::path::Path;
use tungsten_contract::{
    EvidenceDocument, Frontend, HttpEvidence, Limits, MediaEvidence, ResponseEvidence,
    ReviewedOverrides, ShapeEvidence, SourceLocation, UnknownFact,
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
    let operation = HttpEvidence {
        method: args.method.clone().ok_or("HTTP method is required")?,
        path,
        source: SourceLocation {
            document: "manual".into(),
            line: None,
            column: None,
        },
        parameters: vec![],
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
