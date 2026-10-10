// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten schema`: print a published JSON Schema.

use serde_json::Value;
use tungsten_config::TungstenConfig;
use tungsten_ir::Ir;

use crate::args::{SchemaArgs, SchemaName};
use crate::output::{
    CliError, CommandName, CommandResult, ErrorKind, SchemaResult, cli_output_schema,
};
use crate::{Report, exit};

pub(crate) fn run(args: &SchemaArgs) -> Report {
    let schema = schema_for(args.name);
    let mut report = Report::new(CommandName::Schema);
    match serde_json::to_string_pretty(&schema) {
        Ok(text) => report.human = text + "\n",
        Err(err) => {
            return report.failed(
                exit::INTERNAL,
                CliError::new(
                    ErrorKind::Internal,
                    format!("schema could not be serialized: {err}"),
                ),
            );
        }
    }
    report.result = Some(CommandResult::Schema(SchemaResult {
        name: args.name,
        schema,
    }));
    report
}

/// The JSON Schema for a published document kind.
fn schema_for(name: SchemaName) -> Value {
    match name {
        SchemaName::Tungsten => TungstenConfig::json_schema(),
        SchemaName::Agent => tungsten_agent::json_schema(),
        SchemaName::Ir => Ir::json_schema(),
        SchemaName::CliOutput => cli_output_schema(),
        SchemaName::ExternalEmitter => tungsten_emit::external::json_schema(),
    }
}
