// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten-emit-markdown`: a complete external emitter for tungsten
//! (protocol 1). It writes one Markdown page per resource and an index.
//!
//! The protocol in short: tungsten starts this program with no arguments,
//! writes one JSON request (the IR plus the target's options) to standard
//! input and reads one JSON response from standard output. With
//! `--describe` the program prints who it is instead.

use std::io::{Read, Write};
use std::process::ExitCode;

use serde_json::{Map, Value, json};

const PROTOCOL: u64 = 1;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => run(),
        [flag] if flag == "--describe" => describe(),
        _ => {
            eprintln!(
                "usage: tungsten-emit-markdown [--describe]  (the request is read from stdin)"
            );
            ExitCode::from(2)
        }
    }
}

fn describe() -> ExitCode {
    let description = json!({
        "protocol": PROTOCOL,
        "name": "markdown",
        "version": env!("CARGO_PKG_VERSION"),
        "options": {
            "type": "object",
            "properties": {
                "index": {
                    "type": "boolean",
                    "default": true,
                    "description": "Also write index.md, which links every page."
                }
            },
            "additionalProperties": false
        }
    });
    print_json(&description)
}

fn run() -> ExitCode {
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("cannot read the request: {e}");
        return ExitCode::FAILURE;
    }
    let request: Value = match serde_json::from_str(&input) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("the request is not JSON: {e}");
            return ExitCode::FAILURE;
        }
    };
    if request["protocol"].as_u64() != Some(PROTOCOL) {
        eprintln!(
            "this emitter speaks protocol {PROTOCOL}, the request says {}",
            request["protocol"]
        );
        return ExitCode::FAILURE;
    }
    print_json(&emit(&request))
}

fn print_json(value: &Value) -> ExitCode {
    let mut out = std::io::stdout().lock();
    let written = serde_json::to_writer(&mut out, value)
        .map_err(std::io::Error::other)
        .and_then(|()| out.write_all(b"\n"))
        .and_then(|()| out.flush());
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cannot write the response: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The response for a request.
fn emit(request: &Value) -> Value {
    let ir = &request["ir"];
    let title = ir["api"]["title"].as_str().unwrap_or("API");
    let version = ir["api"]["version"].as_str().unwrap_or("");
    let want_index = request["options"]["index"].as_bool().unwrap_or(true);
    let mut files: Vec<Value> = vec![];
    let mut diagnostics: Vec<Value> = vec![];
    let mut pages: Vec<(String, String, usize)> = vec![];
    for namespace in array(&ir["namespaces"]) {
        let ns = name_of(&namespace["name"]);
        for resource in array(&namespace["resources"]) {
            walk(&[], resource, &mut |path, resource| {
                let file = format!("{ns}/{}.md", path.join("-"));
                let heading = format!("{ns}.{}", path.join("."));
                let operations = array(&resource["operations"]);
                for op in operations {
                    if summary(op).is_empty() {
                        diagnostics.push(json!({
                            "code": "MD001",
                            "severity": "warning",
                            "message": format!("operation `{}` has no summary", op["id"].as_str().unwrap_or("?")),
                            "help": "add a `summary` to the operation in the OpenAPI document",
                            "span": { "file": namespace["source"]["file"].as_str().unwrap_or(&ns), "pointer": "" }
                        }));
                    }
                }
                pages.push((file.clone(), heading.clone(), operations.len()));
                files.push(json!({
                    "path": file,
                    "content": page(title, version, &heading, operations)
                }));
            });
        }
    }
    if want_index {
        files.push(json!({ "path": "index.md", "content": index(title, version, &pages) }));
    }
    json!({ "protocol": PROTOCOL, "files": files, "diagnostics": diagnostics })
}

/// Visit `resource` and its children depth first; `path` holds the names of
/// the ancestors.
fn walk(path: &[String], resource: &Value, visit: &mut dyn FnMut(&[String], &Value)) {
    let mut here = path.to_vec();
    here.push(name_of(&resource["name"]));
    if !array(&resource["operations"]).is_empty() {
        visit(&here, resource);
    }
    for child in array(&resource["children"]) {
        walk(&here, child, visit);
    }
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

/// The wire name of a `{ wire, words }` name, safe to use in a file name.
fn name_of(name: &Value) -> String {
    let wire = name["wire"].as_str().unwrap_or("unnamed");
    let safe: String = wire
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        "unnamed".into()
    } else {
        safe
    }
}

fn summary(op: &Value) -> &str {
    op["doc"]["summary"].as_str().unwrap_or("").trim()
}

/// Text for a table cell: one line, pipes escaped.
fn cell(text: &str) -> String {
    text.replace(['\n', '\r'], " ").replace('|', "\\|")
}

fn page(title: &str, version: &str, heading: &str, operations: &[Value]) -> String {
    let mut out = format!(
        "# {heading}\n\nPart of {title} {version}. Generated by tungsten; do not edit.\n\n"
    );
    out.push_str("| Operation | Method | Path | Status | Summary |\n|---|---|---|---|---|\n");
    for op in operations {
        out.push_str(&format!(
            "| `{}` | {} | `{}` | {} | {} |\n",
            cell(op["name"]["wire"].as_str().unwrap_or("")),
            op["method"].as_str().unwrap_or(""),
            cell(op["path"]["raw"].as_str().unwrap_or("")),
            op["status"]["kind"].as_str().unwrap_or("implemented"),
            cell(summary(op)),
        ));
    }
    for op in operations {
        out.push_str(&format!(
            "\n## {}\n\n",
            op["id"].as_str().unwrap_or("operation")
        ));
        if op["deprecated"].as_bool() == Some(true) {
            out.push_str("Deprecated.\n\n");
        }
        let doc = op["doc"]["description"].as_str().unwrap_or("").trim();
        if !doc.is_empty() {
            out.push_str(doc);
            out.push_str("\n\n");
        }
        let params = parameters(&op["params"]);
        if params.is_empty() {
            out.push_str("No parameters.\n");
        } else {
            out.push_str("| Parameter | In | Required |\n|---|---|---|\n");
            for (name, location, required) in params {
                out.push_str(&format!(
                    "| `{}` | {location} | {} |\n",
                    cell(&name),
                    if required { "yes" } else { "no" }
                ));
            }
        }
    }
    out
}

/// `(name, location, required)` for every parameter of an operation, in
/// path, query, header, cookie order.
fn parameters(params: &Value) -> Vec<(String, &'static str, bool)> {
    let empty = Map::new();
    let params = params.as_object().unwrap_or(&empty);
    ["path", "query", "header", "cookie"]
        .into_iter()
        .flat_map(|location| {
            array(params.get(location).unwrap_or(&Value::Null))
                .iter()
                .map(move |p| {
                    (
                        p["wire_name"].as_str().unwrap_or("").to_string(),
                        location,
                        p["required"].as_bool().unwrap_or(false),
                    )
                })
        })
        .collect()
}

fn index(title: &str, version: &str, pages: &[(String, String, usize)]) -> String {
    let mut out = format!("# {title} {version}\n\nGenerated by tungsten; do not edit.\n\n");
    for (file, heading, operations) in pages {
        let noun = if *operations == 1 {
            "operation"
        } else {
            "operations"
        };
        out.push_str(&format!("- [{heading}]({file}) ({operations} {noun})\n"));
    }
    out
}
