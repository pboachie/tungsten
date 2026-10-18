// SPDX-License-Identifier: AGPL-3.0-only
//! Offline declaration query; never discovers or executes external emitters.

use std::fmt::Write as _;
use tungsten_config::capabilities::{self, CapabilityCatalog};

use crate::args::CapabilitiesArgs;
use crate::output::{CliError, CommandName, CommandResult, ErrorKind};
use crate::{Report, exit};

pub(crate) fn run(args: &CapabilitiesArgs) -> Report {
    let mut report = Report::new(CommandName::Capabilities);
    let targets = match args.target.as_deref() {
        None => capabilities::TARGETS.to_vec(),
        Some(name) => match capabilities::target(name) {
            Some(target) => vec![target.clone()],
            None => return report.failed(exit::USAGE, CliError::new(
                ErrorKind::Usage, format!("unknown built-in target `{name}`"),
            ).with_help("run `tungsten capabilities` to list built-in targets; external emitters are not inspected by this command")),
        },
    };
    let result = CapabilityCatalog {
        capability_version: capabilities::VERSION,
        targets,
    };
    report.human = format!(
        "tungsten target capabilities (contract {})\n",
        result.capability_version
    );
    for target in &result.targets {
        let _ = writeln!(
            report.human,
            "  {:<12} {:?}; imported-operation policy: {}",
            target.id, target.source_support, target.imported_operation_policy
        );
        for requirement in target.prerequisites {
            let _ = writeln!(
                report.human,
                "    {} {}",
                requirement.tool,
                requirement.description()
            );
        }
    }
    report.human.push_str("Source declarations only; platform qualification and artifact availability are unknown. Use doctor for local prerequisites.\n");
    report.result = Some(CommandResult::Capabilities(result));
    report
}
