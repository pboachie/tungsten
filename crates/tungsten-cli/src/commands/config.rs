// SPDX-License-Identifier: AGPL-3.0-only
//! Configuration inspection is unconditional no-execution, including without flags.

use std::fmt::Write as _;

use tungsten_config::inspection::{ConfigurationInspection, InspectionValueState};
use tungsten_core::Severity;

use crate::args::ConfigArgs;
use crate::output::{CliError, CommandName, CommandResult, ErrorKind};
use crate::{Report, exit, input};

pub(crate) fn run(args: &ConfigArgs) -> Report {
    let mut report = Report::new(CommandName::Config);
    let manifest =
        match input::resolve(&args.path) {
            Ok(input::Input::Project(manifest)) => manifest,
            _ => return report.failed(
                exit::FAILED,
                CliError::new(
                    ErrorKind::Usage,
                    "config requires a local project directory or tungsten manifest",
                )
                .with_help(
                    "pass a directory containing tungsten.yml or a local tungsten*.yml manifest",
                ),
            ),
        };
    let mut inspected = tungsten_build::inspection::inspect_project(
        &manifest,
        &tungsten_build::CompileOptions::default(),
    );
    if args.strict {
        for diagnostic in &mut inspected.diagnostics.0 {
            if diagnostic.severity == Severity::Warning {
                diagnostic.severity = Severity::Error;
            }
        }
    }
    report.exit = if inspected.diagnostics.has_errors() || inspected.configuration.is_none() {
        exit::FAILED
    } else {
        exit::OK
    };
    if let Some(configuration) = inspected.configuration {
        report.human = human(&configuration);
        report.result = Some(CommandResult::Config(configuration));
    }
    report.diagnostics = inspected.diagnostics.0;
    report.sources = inspected.sources;
    report
}

fn human(configuration: &ConfigurationInspection) -> String {
    let mut text = format!(
        "tungsten effective configuration (contract {})\n",
        configuration.configuration_version
    );
    for profile in &configuration.profiles {
        let _ = writeln!(text, "  {} digest {}", profile.source, profile.digest);
    }
    for target in &configuration.targets {
        let _ = writeln!(text, "  {}: {:?}", target.target, target.status);
        for field in &target.fields {
            let value = match field.state {
                InspectionValueState::Redacted => "[redacted]".into(),
                _ => field.value.as_ref().map_or_else(
                    || "[unresolved or absent]".into(),
                    |value| value.to_string(),
                ),
            };
            let _ = writeln!(text, "    {} = {} ({:?})", field.path, value, field.state);
            if let Some(winner) = &field.winner {
                let _ = writeln!(text, "      winner: {}#{}", winner.source, winner.pointer);
            }
            for origin in &field.shadowed {
                let _ = writeln!(text, "      shadowed: {}#{}", origin.source, origin.pointer);
            }
            if let Some(overridden) = &field.overridden_by {
                let _ = writeln!(text, "      inactive: {overridden} takes precedence");
            }
            let _ = writeln!(
                text,
                "      default: {}; invalid: {}",
                field.default_policy, field.invalid_policy
            );
            if let Some(value) = &field.default_value {
                let _ = writeln!(text, "      schema default value: {value}");
            }
        }
    }
    text.push_str("Authored declarations and supported schema defaults only. Compiler-dependent defaults remain unresolved.\nSensitive values, physical paths, unknown options and external capabilities are omitted. This is not a replayable manifest or approval.\n");
    text
}
