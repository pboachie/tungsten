// SPDX-License-Identifier: AGPL-3.0-only
//! The `tungsten` command-line interface (planning/07).
//!
//! PHASE-1 STUB: `check` and `ir dump` with plain output. The CLI work
//! package adds source-excerpt rendering, `--json` output schema, `explain`,
//! `schema` and exit-code handling.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tungsten_build::{CompileOptions, Compiled, compile_project, compile_spec};

#[derive(Debug, Parser)]
#[command(
    name = "tungsten",
    version,
    about = "Agent-native SDK generator and MCP compiler"
)]
struct Cli {
    /// Print exactly one JSON document on stdout.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate the manifest and specs and print diagnostics.
    Check {
        /// A tungsten.yml, a directory containing one, or a single OpenAPI file.
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Intermediate representation commands.
    Ir {
        #[command(subcommand)]
        command: IrCommand,
    },
}

#[derive(Debug, Subcommand)]
enum IrCommand {
    /// Write the IR as JSON to stdout.
    Dump {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}

fn compile(path: &std::path::Path) -> Compiled {
    let opts = CompileOptions::default();
    if path.is_dir() {
        compile_project(&path.join("tungsten.yml"), &opts)
    } else if path.extension().is_some_and(|e| e == "yml" || e == "yaml")
        && path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("tungsten"))
    {
        compile_project(path, &opts)
    } else {
        compile_spec(path, &opts)
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Check { path } => {
            let c = compile(&path);
            for d in c.diagnostics.iter() {
                eprintln!("{}", d.render_short(Some(&c.workspace.sources)));
            }
            if c.has_errors() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Command::Ir {
            command: IrCommand::Dump { path },
        } => {
            let c = compile(&path);
            for d in c.diagnostics.iter() {
                eprintln!("{}", d.render_short(Some(&c.workspace.sources)));
            }
            match c.ir {
                Some(ir) => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&ir).expect("IR serializes")
                    );
                    ExitCode::SUCCESS
                }
                None => ExitCode::from(1),
            }
        }
    }
}
