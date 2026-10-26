// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten workspace` and `--project`: the projects under one directory.
//!
//! Discovery is `tungsten_config::workspace`: bounded, deterministic and
//! never following links. `workspace` lists what it found; `--project ID`
//! on a command that takes a project turns the workspace root and the id
//! into that project's manifest before the command runs, so the command
//! itself is unchanged and single-project use needs no flag at all.
//! `workspace --run` runs `check`, `generate` or `lock` on each selected
//! project, one at a time in discovery order, and reports each outcome.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;
use tungsten_config::workspace::{self, DEFAULT_LIMITS, DiscoveryLimits, Project};
use tungsten_core::Diagnostic;

use std::ffi::OsString;

use clap::Parser;
use tungsten_build::{CompileOptions, ProjectIdentity, compile_project};

use crate::args::{
    Cli, Command, IrCommand, OverlayCommand, ScenarioCommand, WorkspaceArgs, WorkspaceRun,
};
use crate::output::{
    CliError, CommandName, CommandResult, ErrorKind, JsonDiagnostic, json_diagnostic,
};
use crate::{CliEnv, Report, exit};

/// The `workspace` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct WorkspaceResult {
    /// The workspace root as given on the command line.
    pub root: String,
    /// The bounds discovery ran with.
    pub limits: DiscoveryLimits,
    /// Every project found (or those selected with `--project` and
    /// `--affected`), in discovery order: breadth first, directory entries
    /// in byte order.
    pub projects: Vec<Project>,
    /// Pairs of project ids whose output directories are shared or nested
    /// (TG0642), in discovery order.
    pub collisions: Vec<[String; 2]>,
    /// With `--run`: one outcome per selected project, in the order of
    /// `projects`. Projects after an interruption are not run.
    pub runs: Vec<ProjectRun>,
}

/// The outcome of `--run` on one project.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ProjectRun {
    /// The project id.
    pub project: String,
    /// `check`, `generate` or `lock`.
    pub command: String,
    /// The exit code the command would have returned for this project.
    pub exit_code: i32,
    pub ok: bool,
    /// The run's diagnostics, located in the project's sources.
    pub diagnostics: Vec<JsonDiagnostic>,
    /// The run's invocation error, if any.
    pub error: Option<CliError>,
}

pub(crate) fn run(
    args: &WorkspaceArgs,
    cli: &Cli,
    env: &CliEnv,
    cancellation: &tungsten_mock::RunCancellation,
) -> Report {
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
    let mut projects: Vec<Project> = if args.project.is_empty() {
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
    if !args.affected.is_empty() {
        let files: Vec<PathBuf> = args.affected.iter().map(|f| absolute(f)).collect();
        projects.retain(|p| depends_on(p, &files));
    }
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
    let mut runs = vec![];
    if let Some(command) = args.run {
        match run_each(
            command,
            args,
            cli,
            env,
            cancellation,
            &projects,
            &collisions,
        ) {
            Ok((outcomes, human, rendered, code)) => {
                runs = outcomes;
                report.human.push_str(&human);
                report.rendered = rendered;
                report.exit = report.exit.max(code);
            }
            Err(error) => return report.failed(exit::USAGE, error),
        }
    }
    report.result = Some(CommandResult::Workspace(WorkspaceResult {
        root: args.path.display().to_string(),
        limits: DEFAULT_LIMITS,
        projects,
        collisions,
        runs,
    }));
    report
}

/// Whether `project` depends on any of `files` (absolute): its manifest, a
/// declared input, or a file its offline compilation read or looked for.
/// A project without a compilation identity (it has errors, or reads a
/// remote document) is assumed to depend on everything.
fn depends_on(project: &Project, files: &[PathBuf]) -> bool {
    let declared = files
        .iter()
        .any(|f| *f == project.manifest_path || project.input_paths.contains(f));
    if declared {
        return true;
    }
    let mut opts = CompileOptions::default();
    opts.load.acquisition = tungsten_openapi::acquire::current_options();
    opts.load.acquisition.offline = true;
    opts.load.acquisition.refresh = false;
    let compiled = compile_project(&project.manifest_path, &opts);
    match ProjectIdentity::of(&compiled, &opts, &project.manifest_path, &[]) {
        Some(identity) => files.iter().any(|f| identity.reads(f)),
        None => true,
    }
}

type Runs = (Vec<ProjectRun>, String, String, i32);

/// Run `command` on each project, serially, in order. Before anything runs,
/// arguments are validated and a writing `generate` is refused (TG0642)
/// when a selected project's outputs collide with another project's.
fn run_each(
    command: WorkspaceRun,
    args: &WorkspaceArgs,
    cli: &Cli,
    env: &CliEnv,
    cancellation: &tungsten_mock::RunCancellation,
    projects: &[Project],
    collisions: &[[String; 2]],
) -> Result<Runs, CliError> {
    let parse = |manifest: &Path| -> Result<Cli, CliError> {
        let mut argv: Vec<OsString> = vec!["tungsten".into()];
        if cli.offline {
            argv.push("--offline".into());
        }
        if cli.refresh_inputs {
            argv.push("--refresh-inputs".into());
        }
        if cli.no_external {
            argv.push("--no-external".into());
        }
        argv.push(command.name().into());
        argv.extend(args.args.iter().map(OsString::from));
        argv.push(manifest.as_os_str().to_owned());
        let mut sub = Cli::try_parse_from(argv).map_err(|e| {
            let text = e.to_string();
            let first = text
                .lines()
                .next()
                .unwrap_or_default()
                .trim_start_matches("error: ");
            CliError::new(
                ErrorKind::Usage,
                format!("invalid arguments for `{}`: {first}", command.name()),
            )
            .with_help(format!(
                "pass the arguments `tungsten {} --help` lists after `--`, without PATH",
                command.name()
            ))
        })?;
        sub.input_header = cli.input_header.clone();
        Ok(sub)
    };
    // Validate once, before any project runs.
    let probe = parse(Path::new("."))?;
    let writes = matches!(&probe.command, Command::Generate(g) if !g.dry_run && !g.check);
    let mut outcomes = vec![];
    let mut human = String::new();
    let mut rendered = String::new();
    let mut code = exit::OK;
    if writes && !collisions.is_empty() {
        let pairs: Vec<String> = collisions
            .iter()
            .map(|[a, b]| format!("{a} and {b}"))
            .collect();
        return Err(CliError::new(
            ErrorKind::Refused,
            format!(
                "generate would write output directories that projects share ({}); nothing was generated",
                pairs.join("; ")
            ),
        )
        .with_help("fix the TG0642 diagnostics, or run with --project for projects that do not collide"));
    }
    for project in projects {
        if (cancellation.0)() {
            break;
        }
        let manifest = crate::input::join(&args.path, &project.manifest);
        let sub = parse(&manifest)?;
        let result = super::dispatch(&sub, env, cancellation);
        let _ = writeln!(
            human,
            "\n== {} ({}) exit {}",
            project.id,
            command.name(),
            result.exit
        );
        human.push_str(&result.human);
        for d in &result.diagnostics {
            let _ = writeln!(
                rendered,
                "{}",
                crate::render::render_diagnostic(d, &result.sources, env.color())
            );
        }
        if let Some(error) = &result.error {
            rendered.push_str(&crate::render::render_error(error, env.color()));
        }
        code = code.max(result.exit);
        outcomes.push(ProjectRun {
            project: project.id.clone(),
            command: command.name().to_string(),
            exit_code: result.exit,
            ok: result.exit == exit::OK,
            diagnostics: result
                .diagnostics
                .iter()
                .map(|d| json_diagnostic(d, &result.sources))
                .collect(),
            error: result.error.clone(),
        });
    }
    Ok((outcomes, human, rendered, code))
}

fn absolute(path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    workspace::normalize(&path)
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
