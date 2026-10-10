// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten emitters`: the external emitters tungsten can find, and what
//! each reports about itself (`--describe`).
//!
//! The emitters are the `tungsten-emit-<name>` executables on `PATH` and,
//! when a project is given, the targets of its `tungsten.yml` that declare
//! `external`. The command always succeeds when the project loads: an
//! emitter that cannot be started or speaks another protocol is listed with
//! its status and the reason.

use std::fmt::Write as _;

use tungsten_emit::external::{self, Command};

use crate::args::EmittersArgs;
use crate::commands::generate::Project;
use crate::output::{
    CommandName, CommandResult, EmitterReport, EmitterSource, EmitterStatus, EmittersResult,
};
use crate::{CliEnv, Report, exit, input};

pub(crate) fn run(args: &EmittersArgs, env: &CliEnv) -> Report {
    let mut report = Report::new(CommandName::Emitters);
    let mut emitters: Vec<EmitterReport> = vec![];
    let search_path = env
        .search_path
        .clone()
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    let mut cwd = std::path::PathBuf::from(".");
    if let Some(path) = &args.path {
        let mut compiled = input::compile(path);
        report.diagnostics = compiled.diagnostics.0.clone();
        report.sources = std::mem::take(&mut compiled.workspace.sources);
        let Some(config) = &compiled.config else {
            report.exit = exit::FAILED;
            return report;
        };
        let project = Project::of(path, env);
        cwd = project.working_dir();
        for (name, options) in &config.targets {
            let Ok(Some(target)) = tungsten_config::external_target(options) else {
                continue;
            };
            let located = external::locate(
                name,
                target.command.as_deref(),
                &project.base_dir,
                &project.search_path,
            );
            emitters.push(inspect(
                name,
                EmitterSource::Config,
                located,
                target
                    .command
                    .unwrap_or_else(|| format!("{}{name}", external::EXECUTABLE_PREFIX)),
                &cwd,
            ));
        }
    }
    for found in external::discover(&search_path) {
        if emitters.iter().any(|e| e.name == found.name) {
            continue;
        }
        let command = Command {
            program: found.path,
            args: vec![],
            display: format!("{}{}", external::EXECUTABLE_PREFIX, found.name),
        };
        let display = command.display.clone();
        emitters.push(inspect(
            &found.name,
            EmitterSource::Path,
            Ok(command),
            display,
            &cwd,
        ));
    }
    let result = EmittersResult { emitters };
    report.human = human(&result);
    report.result = Some(CommandResult::Emitters(result));
    report
}

/// Describe one emitter, turning every failure into a status.
fn inspect(
    name: &str,
    source: EmitterSource,
    located: Result<Command, tungsten_core::Diagnostic>,
    command: String,
    cwd: &std::path::Path,
) -> EmitterReport {
    let mut entry = EmitterReport {
        name: name.to_string(),
        source,
        command,
        status: EmitterStatus::Ok,
        protocol: None,
        version: None,
        has_options_schema: false,
        message: None,
    };
    let described = located.and_then(|c| external::describe(name, &c, cwd));
    match described {
        Ok(d) => {
            entry.protocol = Some(d.protocol);
            entry.version = Some(d.version);
            entry.has_options_schema = d.options.is_some();
        }
        Err(d) => {
            entry.status = match d.code.as_str() {
                "TG0801" => EmitterStatus::NotFound,
                "TG0802" => EmitterStatus::ProtocolMismatch,
                _ => EmitterStatus::Failed,
            };
            entry.message = Some(d.message);
        }
    }
    entry
}

fn human(result: &EmittersResult) -> String {
    let mut out = format!(
        "tungsten {} · external emitters (protocol {})\n",
        tungsten_build::TUNGSTEN_VERSION,
        external::PROTOCOL
    );
    if result.emitters.is_empty() {
        out.push_str("  none found: put `tungsten-emit-<name>` on PATH, or declare `external` on a target in tungsten.yml\n");
        return out;
    }
    for e in &result.emitters {
        let source = match e.source {
            EmitterSource::Path => "PATH",
            EmitterSource::Config => "tungsten.yml",
        };
        let state = match (&e.status, &e.version) {
            (EmitterStatus::Ok, Some(v)) => format!(
                "{v} · protocol {}",
                e.protocol.map_or_else(String::new, |p| p.to_string())
            ),
            (EmitterStatus::NotFound, _) => "not found".to_string(),
            (EmitterStatus::ProtocolMismatch, _) => "protocol mismatch".to_string(),
            _ => "failed".to_string(),
        };
        let _ = writeln!(
            out,
            "  {:<12} {:<14} {:<26} {}",
            e.name, source, state, e.command
        );
        if let Some(message) = &e.message {
            let _ = writeln!(out, "    {}", message.replace('\n', "\n    "));
        }
    }
    out
}
