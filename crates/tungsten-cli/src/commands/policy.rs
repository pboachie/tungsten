// SPDX-License-Identifier: AGPL-3.0-only
//! Explicit pure pin creation aid; never returns captured authority.

use crate::args::PolicyDigestArgs;
use crate::output::{CommandName, CommandResult};
use crate::{Report, exit};

pub(crate) fn digest(args: &PolicyDigestArgs) -> Report {
    let mut report = Report::new(CommandName::PolicyDigest);
    match crate::policy::read(&args.path)
        .and_then(|(bytes, _)| tungsten_core::policy::digest_document(&bytes))
    {
        Ok(result) => {
            report.human = format!("{}\n", result.digest.0);
            report.result = Some(CommandResult::PolicyDigest(result));
        }
        Err(diagnostic) => {
            report.exit = exit::FAILED;
            report
                .diagnostics
                .push(diagnostic.at("policy[0]", "", None));
        }
    }
    report
}
