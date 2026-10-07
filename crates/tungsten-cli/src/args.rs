// SPDX-License-Identifier: AGPL-3.0-only
//! Command-line grammar.

use std::path::PathBuf;

use clap::{Args, ColorChoice, Parser, Subcommand, ValueEnum};
use serde::Serialize;

use crate::output::CommandName;

const EXIT_CODES: &str = "\
Exit codes:
  0  success
  1  the input has errors (or warnings with --strict), or the command refused
     to act (init without --force, explain target not found)
  2  usage error: the command line could not be parsed
  3  I/O or internal failure (for example --out is not writable)

Output:
  Without --json, results go to stdout and diagnostics to stderr. With
  --json, stdout carries exactly one JSON document described by
  `tungsten schema cli-output`. Color is used only on a terminal and never
  when NO_COLOR is set.";

#[derive(Debug, Parser)]
#[command(
    name = "tungsten",
    version,
    about = "Agent-native SDK generator and MCP compiler",
    after_long_help = EXIT_CODES,
    arg_required_else_help = true,
    color = ColorChoice::Never
)]
pub(crate) struct Cli {
    /// Print exactly one JSON document on stdout.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// `check --ci` never colors its output.
    pub fn forces_plain(&self) -> bool {
        matches!(&self.command, Command::Check(args) if args.ci)
    }

    pub fn command_name(&self) -> CommandName {
        match self.command {
            Command::Check(_) => CommandName::Check,
            Command::Ir {
                command: IrCommand::Dump(_),
            } => CommandName::IrDump,
            Command::Explain(_) => CommandName::Explain,
            Command::Schema(_) => CommandName::Schema,
            Command::Init(_) => CommandName::Init,
            Command::Doctor => CommandName::Doctor,
        }
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Validate the manifest and specs and report diagnostics.
    Check(CheckArgs),
    /// Intermediate representation commands.
    Ir {
        #[command(subcommand)]
        command: IrCommand,
    },
    /// Explain a diagnostic code, an operation or a type.
    Explain(ExplainArgs),
    /// Print a JSON Schema published by tungsten.
    Schema(SchemaArgs),
    /// Write tungsten.yml and agent.yml for a new project.
    Init(InitArgs),
    /// Report which optional external tools are installed.
    Doctor,
}

#[derive(Debug, Subcommand)]
pub(crate) enum IrCommand {
    /// Write the IR as JSON.
    Dump(DumpArgs),
}

/// Input selection shared by `check` and `ir dump`.
#[derive(Debug, Args)]
pub(crate) struct InputArgs {
    /// A directory with a tungsten.yml, a tungsten*.yml manifest, or a
    /// single OpenAPI document.
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct CheckArgs {
    #[command(flatten)]
    pub input: InputArgs,
    /// CI mode: never prompt, never color.
    #[arg(long)]
    pub ci: bool,
    /// Treat warnings as errors.
    #[arg(long)]
    pub strict: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DumpArgs {
    #[command(flatten)]
    pub input: InputArgs,
    /// Write the IR to this file instead of stdout.
    #[arg(long, value_name = "FILE")]
    pub out: Option<PathBuf>,
    /// One line instead of indented JSON.
    #[arg(long)]
    pub compact: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ExplainArgs {
    /// A diagnostic code (TG0201), an operation id (public.listPets) or a
    /// type id (public.Pet). A bare operationId or type name is accepted
    /// when it is unambiguous.
    pub target: String,
    /// Project used to resolve operation and type ids.
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub project: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct SchemaArgs {
    /// Which schema to print.
    pub name: SchemaName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SchemaName {
    /// The tungsten.yml manifest.
    Tungsten,
    /// The intermediate representation written by `ir dump`.
    Ir,
    /// The `--json` output of this CLI.
    CliOutput,
}

#[derive(Debug, Args)]
pub(crate) struct InitArgs {
    /// OpenAPI document to start from. Default: openapi.json, openapi.yaml
    /// or openapi.yml in the target directory.
    #[arg(long, value_name = "SPEC")]
    pub from: Option<PathBuf>,
    /// Directory to write the manifests into (created if missing).
    #[arg(long, value_name = "DIR", default_value = ".")]
    pub dir: PathBuf,
    /// API machine name: lowercase letters, digits and underscores, starting
    /// with a letter. Default: derived from the spec title.
    #[arg(long, value_parser = parse_api_name)]
    pub name: Option<String>,
    /// Overwrite existing tungsten.yml and agent.yml.
    #[arg(long)]
    pub force: bool,
}

/// Longest accepted API machine name.
const MAX_NAME_LEN: usize = 64;

fn parse_api_name(s: &str) -> Result<String, String> {
    let valid = s.len() <= MAX_NAME_LEN
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if valid {
        Ok(s.to_string())
    } else {
        Err(format!(
            "expected lowercase letters, digits and underscores starting with a letter \
             (at most {MAX_NAME_LEN} characters)"
        ))
    }
}
