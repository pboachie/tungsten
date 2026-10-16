// SPDX-License-Identifier: AGPL-3.0-only
//! One module per command. Each returns a [`Report`]; none writes to the
//! output streams.

pub(crate) mod check;
pub(crate) mod diff;
pub(crate) mod doctor;
pub(crate) mod emitters;
pub(crate) mod explain;
pub(crate) mod generate;
pub(crate) mod init;
mod init_advanced;
mod init_edit;
mod init_example;
mod init_input;
mod init_setup;
mod init_setup_mcp;
mod init_setup_python;
mod init_setup_rust;
mod ir;
pub(crate) mod lsp;
pub(crate) mod mock;
pub(crate) mod overlay;
pub(crate) mod report;
mod schema;
mod wizard;

use crate::args::{Cli, Command, IrCommand, OverlayCommand};
use crate::{CliEnv, Report};

pub(crate) fn dispatch(cli: &Cli, env: &CliEnv) -> Report {
    let source = match &cli.command {
        Command::Check(args) => Some(args.input.path.as_path()),
        Command::Ir {
            command: IrCommand::Dump(args),
        } => Some(args.input.path.as_path()),
        Command::Init(args) => args.from.as_deref(),
        Command::Generate(args) => Some(args.input.path.as_path()),
        Command::Mock(args) => Some(args.input.path.as_path()),
        Command::Report(args) => Some(args.input.path.as_path()),
        Command::Diff(args) => Some(args.input.path.as_path()),
        Command::Overlay {
            command: OverlayCommand::Suggest(args),
        } => Some(args.input.path.as_path()),
        _ => None,
    };
    tungsten_openapi::acquire::with_options(
        tungsten_openapi::acquire::Options {
            offline: cli.offline,
            refresh: cli.refresh_inputs,
            headers: cli.input_header.clone(),
            header_sources: source
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
        },
        || dispatch_with_inputs(cli, env),
    )
}

fn dispatch_with_inputs(cli: &Cli, env: &CliEnv) -> Report {
    match &cli.command {
        Command::Check(args) => check::run(args, env, cli.no_external),
        Command::Ir {
            command: IrCommand::Dump(args),
        } => ir::dump(args, cli.json),
        Command::Explain(args) => explain::run(args),
        Command::Schema(args) => schema::run(args),
        Command::Init(args) => wizard::run(args, cli.json, env),
        Command::Doctor(args) => doctor::run(args, env),
        Command::Emitters(args) => emitters::run(args, env, cli.no_external),
        Command::Generate(args) => generate::run(args, env, cli.no_external),
        Command::Mock(args) => mock::start(args),
        Command::Report(args) => report::run(args, env, cli.no_external),
        Command::Diff(args) => diff::run(args, env, cli.no_external),
        Command::Overlay {
            command: OverlayCommand::Suggest(args),
        } => overlay::suggest(args),
        Command::Lsp(_) => lsp::refuse(),
    }
}
