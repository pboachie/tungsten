// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten workspace` and `--project`: the projects under one directory.
//!
//! Discovery is `tungsten_config::workspace`: bounded, deterministic and
//! never following links. `workspace` lists what it found; `--project ID`
//! on a command that takes a project turns the workspace root and the id
//! into that project's manifest before the command runs, so the command
//! itself is unchanged and single-project use needs no flag at all.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;
use tungsten_config::workspace::{self, DEFAULT_LIMITS, DiscoveryLimits, Project};
use tungsten_core::Diagnostic;

use crate::args::{Cli, Command, IrCommand, OverlayCommand, ScenarioCommand, WorkspaceArgs};
use crate::output::{CliError, CommandName, CommandResult, ErrorKind};
use crate::{Report, exit};

/// The `workspace` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct WorkspaceResult {
    /// The workspace root as given on the command line.
    pub root: String,
    /// The bounds discovery ran with.
    pub limits: DiscoveryLimits,
    /// Every project found (or those selected with `--project`), in
    /// discovery order: breadth first, directory entries in byte order.
    pub projects: Vec<Project>,
    /// Pairs of project ids whose output directories are shared or nested
    /// (TG0642), in discovery order.
    pub collisions: Vec<[String; 2]>,
}

pub(crate) fn run(args: &WorkspaceArgs) -> Report {
    let mut report = Report::new(CommandName::Workspace);
    if !args.path.is_dir() {
        return report.failed(
            exit::FAILED,
            CliError::new(
                ErrorKind::NotFound,
                format!("{} is not a directory", args.path.display()),
            )
            .with_help("pass the directory that holds the projects"),
        );
    }
    let catalog = workspace::discover(&args.path, DEFAULT_LIMITS);
    report.diagnostics = catalog.diagnostics.0.clone();
    let projects: Vec<Project> = if args.project.is_empty() {
        catalog.projects.clone()
    } else {
        match catalog.select(&args.project) {
            Ok(selected) => selected.into_iter().cloned().collect(),
            Err(diagnostic) => {
                report.diagnostics.push(diagnostic);
                report.exit = exit::FAILED;
                return report;
            }
        }
    };
    let ids: Vec<&str> = projects.iter().map(|p| p.id.as_str()).collect();
    let collisions: Vec<[String; 2]> = catalog
        .collisions
        .iter()
        .filter(|c| c.projects.iter().any(|id| ids.contains(&id.as_str())))
        .map(|c| c.projects.clone())
        .collect();
    if catalog.diagnostics.has_errors() {
        report.exit = exit::FAILED;
    }
    report.human = human(&args.path, &projects);
    report.result = Some(CommandResult::Workspace(WorkspaceResult {
        root: args.path.display().to_string(),
        limits: DEFAULT_LIMITS,
        projects,
        collisions,
    }));
    report
}

fn human(root: &Path, projects: &[Project]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} project{} under {}",
        projects.len(),
        if projects.len() == 1 { "" } else { "s" },
        root.display()
    );
    for project in projects {
        let name = project.name.as_deref().unwrap_or("(invalid manifest)");
        let _ = writeln!(out, "  {:<24} {name}  {}", project.id, project.manifest);
        for output in &project.outputs {
            let _ = writeln!(out, "    {:<12} -> {}", output.target, output.out);
        }
    }
    out
}

/// Resolve `--project ID` of the command in place: its PATH becomes the
/// selected project's manifest. A command without `--project` is left as
/// it is. Fails with TG0644 for an unknown id (with any TG0640/TG0641 that
/// may explain it), TG0640 when discovery could not complete, and TG0642 before `generate` writes into
/// a directory another project also writes.
pub(crate) fn select(cli: &mut Cli) -> Result<(), Box<Report>> {
    let name = cli.command_name().unwrap_or(CommandName::Check);
    let writes = matches!(&cli.command, Command::Generate(g) if !g.dry_run && !g.check);
    let Some((path, id)) = selection(&mut cli.command) else {
        return Ok(());
    };
    let root = path.clone();
    let fail = |diagnostics: Vec<Diagnostic>, error: Option<CliError>| {
        let mut report = Report::new(name);
        report.diagnostics = diagnostics;
        report.exit = exit::FAILED;
        report.error = error;
        Err(Box::new(report))
    };
    if !root.is_dir() {
        return fail(
            vec![],
            Some(
                CliError::new(
                    ErrorKind::Usage,
                    format!(
                        "--project needs PATH to be the workspace directory; {} is not a directory",
                        root.display()
                    ),
                )
                .with_help("pass the directory that holds the projects, or drop --project and pass the manifest"),
            ),
        );
    }
    let catalog = workspace::discover(&root, DEFAULT_LIMITS);
    let project = match catalog.select(std::slice::from_ref(&id)) {
        Ok(selected) => selected[0].clone(),
        Err(diagnostic) => {
            // Ambiguous or unsearched directories may explain the miss.
            let mut diagnostics: Vec<Diagnostic> = catalog
                .diagnostics
                .iter()
                .filter(|d| d.code == "TG0640" || d.code == "TG0641")
                .cloned()
                .collect();
            diagnostics.push(diagnostic);
            return fail(diagnostics, None);
        }
    };
    // A reached limit makes the collision check incomplete.
    let mut diagnostics: Vec<Diagnostic> = catalog
        .diagnostics
        .iter()
        .filter(|d| d.code == "TG0640")
        .cloned()
        .collect();
    if writes {
        diagnostics.extend(
            catalog
                .diagnostics_for(&project)
                .into_iter()
                .filter(|d| d.code == "TG0642"),
        );
    }
    if !diagnostics.is_empty() {
        return fail(diagnostics, None);
    }
    *path = crate::input::join(&root, &project.manifest);
    Ok(())
}

/// The PATH and `--project` of the command, when it was given one.
fn selection(command: &mut Command) -> Option<(&mut PathBuf, String)> {
    let (path, project) = match command {
        Command::Check(a) => (&mut a.input.path, &a.input.project),
        Command::Generate(a) => (&mut a.input.path, &a.input.project),
        Command::Report(a) => (&mut a.input.path, &a.input.project),
        Command::Diff(a) => (&mut a.input.path, &a.input.project),
        Command::Mock(a) => (&mut a.input.path, &a.input.project),
        Command::Ir {
            command: IrCommand::Dump(a),
        } => (&mut a.input.path, &a.input.project),
        Command::Overlay {
            command: OverlayCommand::Suggest(a),
        } => (&mut a.input.path, &a.input.project),
        Command::Scenario {
            command: ScenarioCommand::Validate(a),
        } => (&mut a.input.path, &a.input.project),
        Command::Scenario {
            command: ScenarioCommand::Run(a),
        } => (&mut a.input.path, &a.input.project),
        Command::Lock(a) => (&mut a.path, &a.project),
        _ => return None,
    };
    let id = project.clone()?;
    Some((path, id))
}
