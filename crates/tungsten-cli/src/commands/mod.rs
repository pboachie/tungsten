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
mod ir;
pub(crate) mod lsp;
pub(crate) mod mock;
pub(crate) mod overlay;
pub(crate) mod report;
mod schema;

use crate::args::{Cli, Command, IrCommand, OverlayCommand};
use crate::{CliEnv, Report};

pub(crate) fn dispatch(cli: &Cli, env: &CliEnv) -> Report {
    match &cli.command {
        Command::Check(args) => check::run(args, env),
        Command::Ir {
            command: IrCommand::Dump(args),
        } => ir::dump(args, cli.json),
        Command::Explain(args) => explain::run(args),
        Command::Schema(args) => schema::run(args),
        Command::Init(args) => init::run(args),
        Command::Doctor => doctor::run(env),
        Command::Emitters(args) => emitters::run(args, env),
        Command::Generate(args) => generate::run(args, env),
        Command::Mock(args) => mock::start(args),
        Command::Report(args) => report::run(args, env),
        Command::Diff(args) => diff::run(args, env),
        Command::Overlay {
            command: OverlayCommand::Suggest(args),
        } => overlay::suggest(args),
        Command::Lsp(_) => lsp::refuse(),
    }
}
