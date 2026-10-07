// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten mock`: compile the project and serve its mock. The report is
//! printed once the server listens; [`crate::run`] then keeps serving until
//! stdin closes (or the process is interrupted) and shuts the server down.

use std::net::SocketAddr;

use tungsten_mock::{MockOptions, MockServer};

use crate::args::MockArgs;
use crate::output::{CliError, CommandName, CommandResult, ErrorKind, MockResult};
use crate::stats::{ir_stats, plural};
use crate::{Report, exit, input};

pub(crate) fn start(args: &MockArgs) -> Report {
    let mut compiled = input::compile(&args.input.path);
    let mut report = Report::new(CommandName::Mock);
    report.diagnostics = compiled.diagnostics.0.clone();
    report.sources = std::mem::take(&mut compiled.workspace.sources);
    let Some(ir) = compiled.ir.filter(|_| !compiled.diagnostics.has_errors()) else {
        report.exit = exit::FAILED;
        return report;
    };
    let stats = ir_stats(&ir);
    let addr = SocketAddr::from(([127, 0, 0, 1], args.port));
    let opts = MockOptions {
        addr,
        seed: args.seed,
    };
    let server = match MockServer::start(ir, opts) {
        Ok(server) => server,
        Err(e) => {
            return report.failed(
                exit::INTERNAL,
                CliError::new(
                    ErrorKind::Io,
                    format!("cannot start the mock server on {addr}: {e}"),
                )
                .with_help("check that the port is free; --port 0 picks a free one"),
            );
        }
    };
    let base_url = server.base_url();
    report.human = format!(
        "mock server listening on {base_url}\n  serving {} · {} · seed {}\n  stop with Ctrl-C or by closing stdin\n",
        stats.api,
        plural(
            stats.operations.total - stats.operations.planned,
            "operation",
            "operations"
        ),
        args.seed,
    );
    report.result = Some(CommandResult::Mock(MockResult { base_url }));
    report.serving = Some(server);
    report
}
