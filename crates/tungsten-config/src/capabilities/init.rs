// SPDX-License-Identifier: AGPL-3.0-only
//! How `tungsten init` and the terminal wizard select built-in targets. The
//! CLI derives its default selections, local-setup scope and companion
//! targets from these declarations; setup itself stays in the CLI.

use schemars::JsonSchema;
use serde::Serialize;

use super::{SourceSupport, TargetCapability};

/// A target `init` writes next to another one that builds on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct CompanionTarget {
    pub target: &'static str,
    /// Added before `target` when the project does not already have it.
    pub requires: &'static str,
}

/// Source declaration of `init` target selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct InitCapability {
    /// Written by `init` when `--targets` is omitted, in this order.
    pub default_targets: &'static [&'static str],
    /// Offered as the wizard's default outputs.
    pub wizard_default_targets: &'static [&'static str],
    /// Targets `init --local-example` and the wizard can build and call locally;
    /// a selection with any other target is source generation only.
    pub local_setup_targets: &'static [&'static str],
    pub companions: &'static [CompanionTarget],
    /// The `init`-only output name expanding to the `rust` target with its CLI.
    pub cli_alias: &'static str,
}

pub const INIT: InitCapability = InitCapability {
    default_targets: &["typescript", "python", "docs"],
    wizard_default_targets: &["typescript", "docs"],
    local_setup_targets: &["typescript", "python", "rust", "mcp", "webmcp", "docs"],
    companions: &[
        CompanionTarget {
            target: "mcp",
            requires: "typescript",
        },
        CompanionTarget {
            target: "webmcp",
            requires: "typescript",
        },
    ],
    cli_alias: "cli",
};

impl InitCapability {
    /// Whether local setup covers `target` (an id, or the CLI alias).
    pub fn local_setup(&self, target: &str) -> bool {
        let target = if target == self.cli_alias {
            "rust"
        } else {
            target
        };
        self.local_setup_targets.contains(&target)
    }

    /// The targets `init` must add for `selected`, in insertion order;
    /// `present` lists targets the project already has.
    pub fn required_companions(&self, selected: &[&str], present: &[&str]) -> Vec<&'static str> {
        let mut out = Vec::new();
        for companion in self.companions {
            if selected.contains(&companion.target)
                && !selected.contains(&companion.requires)
                && !present.contains(&companion.requires)
                && !out.contains(&companion.requires)
            {
                out.push(companion.requires);
            }
        }
        out
    }

    /// Conflicts: unknown or reserved targets, or the alias shadowing an id.
    pub fn validate(&self, targets: &[TargetCapability]) -> Vec<String> {
        let mut errors = vec![];
        let emitter = |id: &str| {
            targets
                .iter()
                .find(|target| target.id == id)
                .is_some_and(|target| target.source_support == SourceSupport::BuiltInEmitter)
        };
        let named = self
            .default_targets
            .iter()
            .chain(self.wizard_default_targets)
            .chain(self.local_setup_targets)
            .chain(
                self.companions
                    .iter()
                    .flat_map(|c| [&c.target, &c.requires]),
            );
        for id in named {
            if !emitter(id) {
                errors.push(format!(
                    "init declares `{id}`, which is not a built-in emitter target"
                ));
            }
        }
        if targets
            .iter()
            .any(|t| t.id == self.cli_alias || t.aliases.contains(&self.cli_alias))
        {
            errors.push(format!(
                "init output alias `{}` shadows a target name",
                self.cli_alias
            ));
        }
        errors
    }
}
