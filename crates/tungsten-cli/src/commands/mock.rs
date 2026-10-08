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

/// Environment variables of the runtime gates the API knows, sorted: those
/// of gated operations and those agent.yml documents.
fn known_gates(ir: &tungsten_ir::Ir) -> Vec<String> {
    let mut gates: Vec<String> = ir
        .operations()
        .into_iter()
        .filter_map(|op| match &op.status {
            tungsten_ir::OperationStatus::Gated { gate } => Some(gate.env_var.clone()),
            _ => None,
        })
        .chain(ir.agent.gates.keys().cloned())
        .collect();
    gates.sort();
    gates.dedup();
    gates
}

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
    let mut enabled_gates: Vec<String> = vec![];
    for gate in &args.gates {
        if !enabled_gates.contains(gate) {
            enabled_gates.push(gate.clone());
        }
    }
    let known = known_gates(&ir);
    if let Some(unknown) = enabled_gates.iter().find(|g| !known.contains(g)) {
        let listed = if known.is_empty() {
            "the API has no runtime gates".to_string()
        } else {
            format!("its gates: {}", known.join(", "))
        };
        return report.failed(
            exit::USAGE,
            CliError::new(
                ErrorKind::Usage,
                format!("--gate {unknown}: the API has no runtime gate of that name"),
            )
            .with_help(listed),
        );
    }
    let opts = MockOptions {
        addr,
        seed: args.seed,
        enabled_gates: enabled_gates.clone(),
        max_recorded_calls: args.max_recorded_calls,
        max_idempotent_responses: args.max_idempotent_responses,
        ..MockOptions::default()
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
    let gates_line = if enabled_gates.is_empty() {
        String::new()
    } else {
        format!("  gates on {}\n", enabled_gates.join(", "))
    };
    let caps = format!(
        "  keeps {} and {}\n",
        plural(args.max_recorded_calls, "recorded call", "recorded calls"),
        plural(
            args.max_idempotent_responses,
            "idempotent response",
            "idempotent responses"
        )
    );
    report.human = format!(
        "mock server listening on {base_url}\n  serving {} · {} · seed {}\n{gates_line}{caps}  stop with Ctrl-C or by closing stdin\n",
        stats.api,
        plural(
            stats.operations.total - stats.operations.planned,
            "operation",
            "operations"
        ),
        args.seed,
    );
    report.result = Some(CommandResult::Mock(MockResult {
        base_url,
        enabled_gates,
        max_recorded_calls: args.max_recorded_calls,
        max_idempotent_responses: args.max_idempotent_responses,
    }));
    report.serving = Some(server);
    report
}
