// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten lsp`: the language server for manifests.

use crate::args::LspArgs;
use crate::output::{CliError, CommandName, ErrorKind};
use crate::{Report, exit};

/// Serve on the process's stdin and stdout until the client exits.
pub(crate) fn serve(args: &LspArgs) -> i32 {
    tungsten_lsp::run_stdio(tungsten_lsp::Options {
        config: args.config.clone(),
        ..tungsten_lsp::Options::default()
    })
}

/// [`crate::run`] writes to the streams it is given, which a language
/// server cannot share; the binary starts the server before `run`.
pub(crate) fn refuse() -> Report {
    Report::new(CommandName::Check).failed(
        exit::USAGE,
        CliError::new(
            ErrorKind::Usage,
            "`lsp` serves the process's own stdin and stdout and cannot run through `run`",
        ),
    )
}
