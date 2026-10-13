// SPDX-License-Identifier: AGPL-3.0-only
//! Terminal orchestration of the same init and generation used by scripts.
use std::io::{self, IsTerminal, Write};

use crate::args::{GenerateArgs, InitArgs, InputArgs};
use crate::output::{CliError, CommandName, CommandResult, ErrorKind, InitResult};
use crate::{CliEnv, Report, exit};

pub(crate) fn run(args: &InitArgs, json: bool, env: &CliEnv) -> Report {
    let bare = args.method.is_none()
        && args.from.is_none()
        && args.name.is_none()
        && args.targets.is_empty()
        && args.dir == std::path::Path::new(".")
        && !args.force
        && !args.generate
        && !args.resume;
    let terminal = io::stdin().is_terminal() && env.is_tty;
    if args.interactive && (json || !terminal) {
        return refused(
            "interactive init requires terminal input and human output",
            "use --no-interactive with --from, --targets and --generate",
        );
    }
    let mut selected = args.clone();
    if args.interactive || (bare && terminal && !json && !args.no_interactive) {
        match walkthrough(&mut selected) {
            Ok(true) => {}
            Ok(false) => {
                return refused(
                    "setup cancelled; no project changes made",
                    "run tungsten init again when ready",
                );
            }
            Err(e) => {
                return Report::new(CommandName::Init).failed(
                    exit::INTERNAL,
                    CliError::new(ErrorKind::Io, format!("cannot read setup answers: {e}")),
                );
            }
        }
    }
    if selected.local_example
        && let Err(message) = super::init_setup::preflight(&selected)
    {
        return refused(
            message,
            "use --generate alone for offline source generation or select typescript,docs",
        );
    }
    if selected.method.is_some()
        && let Err(error) = super::init_input::prepare(&mut selected)
    {
        return refused(
            &error,
            "correct the no-schema input; existing files are never overwritten",
        );
    }
    let mut report = if selected.resume {
        for name in ["tungsten.yml", "agent.yml"] {
            let path = selected.dir.join(name);
            if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
                return refused(
                    "resume requires regular tungsten.yml and agent.yml files",
                    "restore both manifests; symlinks are not accepted",
                );
            }
        }
        // Generation reads the existing configuration; no configuration is rewritten.
        let mut r = Report::new(CommandName::Init);
        r.result = Some(CommandResult::Init(InitResult {
            dir: selected.dir.display().to_string(),
            api_name: String::new(),
            namespace: String::new(),
            spec: String::new(),
            spec_found: true,
            files: vec![],
            generation: None,
            local_example: None,
        }));
        r
    } else {
        super::init::run(&selected)
    };
    if report.exit != exit::OK || !selected.generate {
        return report;
    }
    let generated = super::generate::run(
        &GenerateArgs {
            input: InputArgs {
                path: selected.dir.clone(),
            },
            target: vec![],
            dry_run: false,
            check: false,
            force: false,
            strict: false,
        },
        env,
    );
    report.exit = generated.exit;
    report.error = generated.error;
    report.diagnostics.extend(generated.diagnostics);
    report.sources = generated.sources;
    report.human.push_str(&generated.human);
    if let (Some(CommandResult::Init(result)), Some(CommandResult::Generate(generation))) =
        (&mut report.result, generated.result)
    {
        result.generation = Some(generation);
    }
    if report.exit == exit::OK && selected.local_example {
        let success = super::init_setup::run(&selected);
        if let Some(CommandResult::Init(result)) = &mut report.result {
            result.local_example = Some(success.is_ok());
        }
        match success {
            Ok(()) => report.human.push_str("Built TypeScript SDK and completed a generated client call against the local mock. The mock has stopped.\n"),
            Err(message) => { report.exit = exit::FAILED; report.error = Some(CliError::new(ErrorKind::Io, message).with_help("source and manifests are preserved; inspect the project dependencies and rebuild before retrying the local example")); }
        }
        return report;
    }
    if report.exit == exit::OK {
        report.human.push_str("Source generation complete. Dependency installation, builds and a first local call have not been run.\n");
    } else {
        report.human.push_str("Generation failed. Manifests are preserved; correct the diagnostics and resume with `tungsten init --resume --generate --dir <project>`.\n");
    }
    report
}

fn refused(message: &str, help: &str) -> Report {
    Report::new(CommandName::Init).failed(
        exit::REFUSED,
        CliError::new(ErrorKind::Refused, message).with_help(help),
    )
}

fn answer(label: &str, default: &str) -> io::Result<Option<String>> {
    eprint!("{label} [{default}]: ");
    io::stderr().flush()?;
    let mut value = String::new();
    if io::stdin().read_line(&mut value)? == 0 {
        return Ok(None);
    }
    let value = value.trim();
    if value.eq_ignore_ascii_case("cancel") {
        return Ok(None);
    }
    Ok(Some(if value.is_empty() {
        default.to_owned()
    } else {
        value.to_owned()
    }))
}

fn walkthrough(args: &mut InitArgs) -> io::Result<bool> {
    eprintln!(
        "Tungsten setup. Type cancel at any prompt; back at input/summary returns to project selection."
    );
    loop {
        let Some(dir) = answer("Project directory", &args.dir.display().to_string())? else {
            return Ok(false);
        };
        args.dir = dir.into();
        if args.dir.join("tungsten.yml").exists() || args.dir.join("agent.yml").exists() {
            eprintln!(
                "Existing project: resume preserves all reviewed settings and outputs. Edit manifests separately to change advanced options."
            );
            let Some(choice) = answer("Resume generation? yes / back / cancel", "cancel")? else {
                return Ok(false);
            };
            if choice == "back" {
                continue;
            }
            if choice != "yes" {
                return Ok(false);
            }
            args.resume = true;
        } else {
            eprintln!(
                "Outputs (comma-separated multi-select):\n  [ ] typescript - JavaScript/TypeScript SDK (Node/npm)\n  [ ] python - Python SDK\n  [ ] rust - Rust SDK and CLI (Cargo)\n  [ ] cli - command-line client (Rust SDK + Cargo)\n  [ ] mcp - local MCP server (Node/npm; adds TypeScript)\n  [ ] webmcp - browser adapter (Node/npm; adds TypeScript)\n  [ ] docs - documentation"
            );
            let defaults = if args.targets.is_empty() {
                "typescript,docs".to_owned()
            } else {
                args.targets.join(",")
            };
            let Some(outputs) = answer("Outputs", &defaults)? else {
                return Ok(false);
            };
            let targets: Vec<String> = outputs
                .split(',')
                .map(|s| crate::targets::canonical(s.trim()).to_string())
                .collect();
            if targets
                .iter()
                .any(|s| s != "cli" && !tungsten_config::KNOWN_TARGETS.contains(&s.as_str()))
            {
                eprintln!("Unknown output; choose from the displayed list.");
                continue;
            }
            args.targets = targets;
            let default = args
                .from
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let Some(source) = answer(
                "OpenAPI file (blank auto-detects), none for no schema, or back",
                &default,
            )?
            else {
                return Ok(false);
            };
            if source == "back" {
                continue;
            }
            if source == "none" {
                let Some(method) = answer("HTTP method", "get")? else {
                    return Ok(false);
                };
                let Some(endpoint) = answer(
                    "Endpoint path template (no URL, query or credentials)",
                    "/ping",
                )?
                else {
                    return Ok(false);
                };
                let Some(request) = answer("Optional JSON request example file", "")? else {
                    return Ok(false);
                };
                let Some(response) = answer("Optional JSON response example file", "")? else {
                    return Ok(false);
                };
                args.method = Some(method.to_ascii_lowercase());
                args.endpoint = Some(endpoint);
                args.request_example = (!request.is_empty()).then(|| request.into());
                args.response_example = (!response.is_empty()).then(|| response.into());
                args.from = None;
            } else {
                args.method = None;
                args.endpoint = None;
                args.from = if source.is_empty() {
                    None
                } else {
                    Some(source.into())
                };
            }
            if args.method.is_none()
                && args.from.is_none()
                && !["openapi.json", "openapi.yaml", "openapi.yml"]
                    .iter()
                    .any(|s| args.dir.join(s).is_file())
            {
                eprintln!(
                    "No OpenAPI input found. Supply an existing OpenAPI file to generate outputs."
                );
                continue;
            }
            args.resume = false;
        }
        eprintln!(
            "Plan: {} in {}. Generate configured source outputs using the regular compiler. Local setup, if selected above, downloads npm packages with scripts disabled, runs the TypeScript compiler and starts a temporary loopback mock. No live API calls. Advanced settings remain editable in tungsten.yml and agent.yml.",
            if args.resume {
                "preserve manifests"
            } else {
                "create tungsten.yml and agent.yml"
            },
            args.dir.display()
        );
        let Some(consent) = answer("Proceed? yes / back / cancel", "cancel")? else {
            return Ok(false);
        };
        if consent == "back" {
            continue;
        }
        if consent != "yes" {
            return Ok(false);
        }
        args.force = false;
        args.generate = true;
        return Ok(true);
    }
}
