// SPDX-License-Identifier: AGPL-3.0-only
//! Terminal orchestration of the same init and generation used by scripts.
use std::io::{self, IsTerminal, Write};

use crate::args::{GenerateArgs, InitArgs, InputArgs};
use crate::output::{
    CliError, CommandName, CommandResult, ErrorKind, InitAction, InitFile, InitResult,
};
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
    if let Err(message) = super::init_advanced::validate(&selected) {
        return refused(
            message,
            "correct the advanced setting; no project changes made",
        );
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
    let imported_files = if !selected.resume && selected.method.is_none() {
        match super::init_input::import(&mut selected) {
            Ok(files) => files,
            Err(error) => {
                return refused(
                    &error,
                    "correct the input or resume the existing project; no imported commands are executed",
                );
            }
        }
    } else {
        vec![]
    };
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
        if !selected.add_targets.is_empty()
            && let Err(error) = super::init_edit::add_outputs(&selected)
        {
            return refused(
                &error,
                "inspect tungsten.yml.init-backup and tungsten.yml.init-pending before retrying",
            );
        }
        // Existing reviewed values survive output additions; plain resume writes nothing.
        let manifest =
            std::fs::read_to_string(selected.dir.join("tungsten.yml")).unwrap_or_default();
        let (configuration, _) = tungsten_config::parse_str("tungsten.yml", &manifest);
        let Some(configuration) = configuration else {
            return refused(
                "existing project configuration is invalid",
                "correct tungsten.yml before resuming",
            );
        };
        let first = configuration.inputs.first();
        let mut r = Report::new(CommandName::Init);
        r.result = Some(CommandResult::Init(InitResult {
            dir: selected.dir.display().to_string(),
            api_name: configuration.api.name.clone(),
            namespace: first.map(|i| i.namespace.clone()).unwrap_or_default(),
            spec: first.map(|i| i.spec.clone()).unwrap_or_default(),
            spec_found: true,
            files: if selected.add_targets.is_empty() {
                vec![]
            } else {
                vec![
                    InitFile {
                        path: selected.dir.join("tungsten.yml").display().to_string(),
                        action: InitAction::Overwritten,
                    },
                    InitFile {
                        path: selected
                            .dir
                            .join("tungsten.yml.init-backup")
                            .display()
                            .to_string(),
                        action: InitAction::Created,
                    },
                ]
            },
            generation: None,
            local_example: None,
            setup: None,
        }));
        r
    } else {
        super::init::run(&selected)
    };
    if report.exit == exit::OK
        && selected.method.is_some()
        && let Some(CommandResult::Init(result)) = &mut report.result
    {
        for name in ["inferred.openapi.json", "inference-report.json"] {
            result.files.push(InitFile {
                path: selected.dir.join(name).display().to_string(),
                action: InitAction::Created,
            });
        }
    }
    if report.exit == exit::OK
        && let Some(CommandResult::Init(result)) = &mut report.result
    {
        for name in &imported_files {
            result.files.push(InitFile {
                path: selected.dir.join(name).display().to_string(),
                action: InitAction::Created,
            });
        }
        if imported_files
            .iter()
            .any(|name| name == "import-report.json")
        {
            report.human.push_str("Import report saved: review omitted scripts, authentication and missing origin configuration in import-report.json.\n");
        }
    }
    if report.exit != exit::OK || !selected.generate {
        return report;
    }
    if !selected.hide_operation.is_empty() || !selected.require_confirmation.is_empty() {
        let compiled = crate::input::compile(&selected.dir);
        if let Some(ir) = &compiled.ir {
            let operations = ir.operations();
            if selected.require_confirmation.iter().any(|reference| {
                operations.iter().any(|op| {
                    op.id.0 == *reference && op.agent.safety != tungsten_ir::Safety::Irreversible
                })
            }) {
                report.exit = exit::REFUSED;
                report.error = Some(CliError::new(ErrorKind::Refused, "saved operation review prevents the requested confirmation restriction").with_help("review this exact operation's existing safety settings in agent.yml or the input. Setup does not overwrite saved imported reviews; configuration is preserved and no outputs were generated."));
                return report;
            }
            if selected
                .hide_operation
                .iter()
                .chain(&selected.require_confirmation)
                .any(|reference| !operations.iter().any(|op| op.id.0 == *reference))
            {
                report.exit = exit::REFUSED;
                report.error = Some(CliError::new(ErrorKind::Refused, "an advanced operation reference does not match an exact compiled operation id").with_help("inspect `tungsten --json ir dump <project>`; correct agent.yml and resume generation. Configuration is preserved; outputs were not generated."));
                return report;
            }
        }
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
            strict: selected.strict,
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
        let (setup, success) = super::init_setup::run(&selected);
        for target in &setup {
            report
                .human
                .push_str(&format!("setup {}: {}\n", target.target, target.status));
        }
        if selected.targets.iter().any(|target| target == "webmcp") {
            report.human.push_str("Browser registration and disposal have not been exercised by this terminal setup.\n");
        }
        if let Some(CommandResult::Init(result)) = &mut report.result {
            result.local_example = Some(success.is_ok());
            result.setup = Some(setup);
        }
        match success {
            Ok(()) => report.human.push_str("Built selected outputs and completed generated client/MCP calls against the local mock. The mock has stopped.\n"),
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
            let Some(additions) = answer(
                "Optional outputs to add (comma-separated; blank preserves configuration)",
                "",
            )?
            else {
                return Ok(false);
            };
            args.add_targets = additions
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| crate::targets::canonical(s).to_string())
                .collect();
            args.resume = true;
        } else {
            eprintln!(
                "Outputs (comma-separated multi-select):\n  [ ] typescript - JavaScript/TypeScript SDK (Node/npm)\n  [ ] python - Python SDK\n  [ ] rust - Rust SDK and CLI (Cargo)\n  [ ] cli - command-line client (Rust SDK + Cargo)\n  [ ] mcp - local MCP server (Node/npm; adds TypeScript)\n  [ ] webmcp - browser adapter (Node/npm; adds TypeScript)\n  [ ] docs - documentation"
            );
            for target in tungsten_config::KNOWN_TARGETS {
                if crate::targets::emitter(target).is_some()
                    && !matches!(
                        *target,
                        "typescript" | "python" | "rust" | "mcp" | "webmcp" | "docs"
                    )
                {
                    eprintln!(
                        "  [ ] {target} - SDK source generation only; declared OpenAPI, no imported contracts or local build/setup"
                    );
                }
            }
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
                .any(|s| s != "cli" && crate::targets::emitter(s).is_none())
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
                "API input file (OpenAPI, Swagger, Postman, HAR, .curl, .http or endpoint manifest); none for no schema; back",
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
            if let Some(source) = &args.from {
                if source
                    .extension()
                    .is_some_and(|extension| extension == "har")
                {
                    let Some(origin) = answer(
                        "HAR origin (blank auto-selects only a single-origin capture)",
                        args.har_origin.as_deref().unwrap_or(""),
                    )?
                    else {
                        return Ok(false);
                    };
                    args.har_origin = (!origin.is_empty()).then_some(origin);
                }
                let request_file = source
                    .extension()
                    .is_some_and(|extension| matches!(extension.to_str(), Some("http" | "curl")));
                if request_file || args.input_format != "auto" {
                    let Some(routes) = answer(
                        "Saved request route templates, in request order (comma-separated)",
                        &args.route.join(","),
                    )?
                    else {
                        return Ok(false);
                    };
                    args.route = routes
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect();
                    if source
                        .extension()
                        .is_some_and(|extension| extension == "curl")
                    {
                        let Some(dialect) = answer(
                            "Saved cURL quoting: posix / cmd / powershell",
                            &args.shell_dialect,
                        )?
                        else {
                            return Ok(false);
                        };
                        args.shell_dialect = dialect;
                    }
                }
            }
            args.resume = false;
        }
        if !args.resume {
            let Some(advanced) = answer("Advanced options? yes / no", "no")? else {
                return Ok(false);
            };
            if advanced == "yes" {
                let Some(name) = answer(
                    "Optional API name (blank uses input title)",
                    args.name.as_deref().unwrap_or(""),
                )?
                else {
                    return Ok(false);
                };
                args.name = (!name.is_empty()).then_some(name);
                let Some(server) = answer(
                    "Default server URL for later live use (blank uses input; no credentials)",
                    args.server_url.as_deref().unwrap_or(""),
                )?
                else {
                    return Ok(false);
                };
                args.server_url = (!server.is_empty()).then_some(server);
                let Some(strict) = answer(
                    "Strict generation validation? yes / no",
                    if args.strict { "yes" } else { "no" },
                )?
                else {
                    return Ok(false);
                };
                args.strict = strict == "yes";
                for (label, operations) in [
                    (
                        "Hide exact namespace.operationId from agents (comma-separated, optional)",
                        &mut args.hide_operation,
                    ),
                    (
                        "Require confirmation for exact namespace.operationId (comma-separated, optional)",
                        &mut args.require_confirmation,
                    ),
                ] {
                    let Some(value) = answer(label, &operations.join(","))? else {
                        return Ok(false);
                    };
                    *operations = value
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect();
                }
                let has_node = args
                    .targets
                    .iter()
                    .any(|t| matches!(t.as_str(), "typescript" | "mcp" | "webmcp"));
                let has_rust = args
                    .targets
                    .iter()
                    .any(|t| matches!(t.as_str(), "rust" | "cli"));
                if has_rust {
                    let Some(profiles) = answer(
                        "Optional CLI environment profiles NAME=URL (comma-separated, no credentials)",
                        &args.cli_profile.join(","),
                    )?
                    else {
                        return Ok(false);
                    };
                    args.cli_profile = profiles
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect();
                }
                for (label, enabled, setting) in [
                    (
                        "TypeScript runtime path (blank uses registry)",
                        has_node,
                        &mut args.typescript_runtime,
                    ),
                    (
                        "WebMCP runtime path (blank uses registry)",
                        args.targets.iter().any(|t| t == "webmcp"),
                        &mut args.webmcp_runtime,
                    ),
                    (
                        "MCP runtime path (blank uses registry)",
                        args.targets.iter().any(|t| t == "mcp"),
                        &mut args.mcp_runtime,
                    ),
                    (
                        "Python runtime path (blank uses registry)",
                        args.targets.iter().any(|t| t == "python"),
                        &mut args.python_runtime,
                    ),
                    (
                        "Rust runtime crate path (blank uses registry)",
                        has_rust,
                        &mut args.rust_runtime,
                    ),
                    (
                        "CLI kit crate path (blank uses registry)",
                        has_rust,
                        &mut args.cli_kit_runtime,
                    ),
                ] {
                    if enabled {
                        let Some(value) = answer(
                            label,
                            &setting
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default(),
                        )?
                        else {
                            return Ok(false);
                        };
                        *setting = (!value.is_empty()).then(|| value.into());
                    }
                }
                eprintln!(
                    "Configuration precedence: input shapes, tungsten.yml target/server overrides, then agent.yml per-operation policy. Inspect the effective contract with tungsten --json ir dump. Imported operations retain preview confirmation until explicitly reviewed; no credentials are required for the mock."
                );
            }
            let setup_supported = args.targets.iter().all(|t| {
                matches!(
                    t.as_str(),
                    "typescript" | "python" | "rust" | "cli" | "mcp" | "webmcp" | "docs"
                )
            });
            if !setup_supported {
                eprintln!(
                    "This selection includes source-only SDK outputs. Generate and inspect those sources with their language toolchains; this combination has no automated local setup or successful first-call claim."
                );
                args.local_example = false;
            } else if args.targets.iter().any(|t| t != "docs") {
                eprintln!(
                    "Local setup prerequisites: Node >=22/npm for TypeScript/MCP/WebMCP; Python >=3.12/venv/pip for Python; Rust >=1.88/Cargo for Rust/CLI. Downloads dependencies into this project, builds selected outputs and calls only a temporary loopback mock. npm lifecycle scripts are disabled; Python uses wheel downloads and the package build backend; Cargo runs dependency build scripts. Explicit runtime paths must be trusted source."
                );
                let Some(local) =
                    answer("Local setup? yes / no (no keeps offline generation)", "yes")?
                else {
                    return Ok(false);
                };
                args.local_example = local == "yes";
            }
        }
        eprintln!(
            "Plan: {} in {}. Generate configured source outputs using the regular compiler. Local setup: {}. Shared TypeScript output is reused for MCP/WebMCP. No live API calls or application integration installation. Advanced settings remain editable in tungsten.yml and agent.yml.",
            if args.resume {
                "preserve reviewed settings; optionally add outputs with a manifest backup"
            } else {
                "create tungsten.yml and agent.yml"
            },
            args.dir.display(),
            if args.local_example {
                "download project dependencies, build selected targets and run temporary local checks"
            } else {
                "disabled; generate source only"
            }
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
