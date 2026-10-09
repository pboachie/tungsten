// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten ir dump`: write the IR as JSON.

use tungsten_ir::Ir;

use crate::args::DumpArgs;
use crate::output::{CliError, CommandName, CommandResult, ErrorKind, IrDumpResult};
use crate::stats::{describe, ir_stats, plural};
use crate::{Report, exit, input};

pub(crate) fn dump(args: &DumpArgs, json: bool) -> Report {
    let compiled = input::compile(&args.input.path);
    let mut report = Report::new(CommandName::IrDump);
    report.diagnostics = compiled.diagnostics.0;
    report.sources = compiled.workspace.sources;
    let Some(ir) = compiled.ir else {
        report.exit = exit::FAILED;
        return report;
    };
    let stats = ir_stats(&ir);

    // JSON mode without --out embeds the IR in the output document.
    if json && args.out.is_none() {
        return match serde_json::to_value(&ir) {
            Ok(value) => {
                report.result = Some(CommandResult::IrDump(IrDumpResult {
                    ir: Some(value),
                    out: None,
                    bytes: None,
                    stats,
                }));
                report
            }
            Err(err) => serialize_failed(report, &err),
        };
    }

    let text = match ir_text(&ir, args.compact) {
        Ok(text) => text,
        Err(err) => return serialize_failed(report, &err),
    };
    let Some(path) = &args.out else {
        report.human = text;
        return report;
    };
    let shown = path.display().to_string();
    if let Err(err) = std::fs::write(path, &text) {
        return report.failed(
            exit::INTERNAL,
            CliError::new(ErrorKind::Io, format!("cannot write {shown}: {err}")),
        );
    }
    report.human = format!(
        "wrote {shown} · {} · {}\n",
        plural(text.len(), "byte", "bytes"),
        describe(&stats)
    );
    report.result = Some(CommandResult::IrDump(IrDumpResult {
        ir: None,
        out: Some(shown),
        bytes: Some(text.len()),
        stats,
    }));
    report
}

/// The IR as JSON text with a trailing newline.
fn ir_text(ir: &Ir, compact: bool) -> serde_json::Result<String> {
    let mut text = if compact {
        serde_json::to_string(ir)?
    } else {
        serde_json::to_string_pretty(ir)?
    };
    text.push('\n');
    Ok(text)
}

fn serialize_failed(report: Report, err: &serde_json::Error) -> Report {
    report.failed(
        exit::INTERNAL,
        CliError::new(
            ErrorKind::Internal,
            format!("the IR could not be serialized: {err}"),
        ),
    )
}
