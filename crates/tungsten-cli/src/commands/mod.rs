// SPDX-License-Identifier: AGPL-3.0-only
//! One module per command. Each returns a [`Report`]; none writes to the
//! output streams.

pub(crate) mod check;
pub(crate) mod diff;
pub(crate) mod doctor;
pub(crate) mod explain;
pub(crate) mod generate;
pub(crate) mod init;
mod ir;
pub(crate) mod mock;
pub(crate) mod overlay;
pub(crate) mod report;
mod schema;

use crate::args::{Cli, Command, IrCommand, OverlayCommand};
use crate::{CliEnv, Report};

pub(crate) fn dispatch(cli: &Cli, env: &CliEnv) -> Report {
    match &cli.command {
        Command::Check(args) => check::run(args),
        Command::Ir {
            command: IrCommand::Dump(args),
        } => ir::dump(args, cli.json),
        Command::Explain(args) => explain::run(args),
        Command::Schema(args) => schema::run(args),
        Command::Init(args) => init::run(args),
        Command::Doctor => doctor::run(env),
        Command::Generate(args) => generate::run(args),
        Command::Mock(args) => mock::start(args),
        Command::Report(args) => report::run(args),
        Command::Diff(args) => diff::run(args),
        Command::Overlay {
            command: OverlayCommand::Suggest(args),
        } => overlay::suggest(args),
    }
}
