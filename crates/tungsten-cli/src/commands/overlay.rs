// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten overlay suggest`: an Overlay 1.0 document that fixes
//! diagnostics.

use std::path::Path;

use tungsten_build::suggest::{
    InputSuggestions, SkipKind, Skipped, Suggestions, render_overlay, suggest as find,
};
use tungsten_core::Diagnostic;
use tungsten_core::diagnostic::codes;

use crate::args::SuggestArgs;
use crate::output::{
    CliError, CommandName, CommandResult, ErrorKind, OverlaySuggestResult, SuggestedAction,
};
use crate::stats::plural;
use crate::{Report, exit, input};

pub(crate) fn suggest(args: &SuggestArgs) -> Report {
    let mut report = Report::new(CommandName::OverlaySuggest);
    if let Some(bad) = args.only.iter().find(|c| codes::describe(c).is_none()) {
        return report.failed(
            exit::USAGE,
            CliError::new(
                ErrorKind::Usage,
                format!("--only: `{bad}` is not a diagnostic code"),
            )
            .with_help("codes look like TG0403; `tungsten explain` describes them"),
        );
    }
    let compiled = input::compile(&args.input.path);
    report.sources = compiled.workspace.sources.clone();
    if compiled.ir.is_none() || compiled.has_errors() {
        report.diagnostics = compiled.diagnostics.0;
        report.exit = exit::FAILED;
        return report;
    }
    let found = find(&compiled, &args.only);
    let chosen = match choose(&found, args.namespace.as_deref(), &compiled) {
        Ok(chosen) => chosen,
        Err(error) => return report.failed(exit::USAGE, error),
    };
    report.diagnostics = skipped_diagnostics(&found.skipped);
    let skipped = found.skipped.iter().map(|s| s.count).sum();
    let mut result = OverlaySuggestResult {
        namespace: chosen.map(|c| c.namespace.clone()),
        spec: chosen.map(|c| c.spec.clone()),
        actions: chosen.map(actions_of).unwrap_or_default(),
        skipped,
        out: None,
        overlay: None,
    };
    let Some(chosen) = chosen else {
        if args.write.is_some() {
            report.human = "nothing to suggest\n".into();
        }
        report.result = Some(CommandResult::OverlaySuggest(result));
        return report;
    };
    let text = render_overlay(chosen);
    match &args.write {
        None => {
            report.human.clone_from(&text);
            result.overlay = Some(text);
        }
        Some(path) => {
            if let Err(error) = write(path, &text, args.force) {
                return report.failed(error.0, error.1);
            }
            let shown = path.display().to_string();
            report.human = format!(
                "wrote {shown} · {} for {}\n  add it under `overlays` of input {} in tungsten.yml\n",
                plural(chosen.actions.len(), "action", "actions"),
                chosen.spec,
                chosen.namespace,
            );
            result.out = Some(shown);
        }
    }
    report.result = Some(CommandResult::OverlaySuggest(result));
    report
}

/// The input to write an overlay for: the named one, or the only one with
/// suggestions.
fn choose<'a>(
    found: &'a Suggestions,
    namespace: Option<&str>,
    compiled: &tungsten_build::Compiled,
) -> Result<Option<&'a InputSuggestions>, CliError> {
    if let Some(name) = namespace {
        let known = compiled
            .config
            .as_ref()
            .is_some_and(|c| c.inputs.iter().any(|i| i.namespace == name));
        if !known {
            return Err(CliError::new(
                ErrorKind::Usage,
                format!("--namespace: the project has no input named `{name}`"),
            ));
        }
        return Ok(found.inputs.iter().find(|i| i.namespace == name));
    }
    match found.inputs.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one)),
        many => Err(CliError::new(
            ErrorKind::Usage,
            format!(
                "several inputs have suggestions: {}",
                many.iter()
                    .map(|i| i.namespace.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
        .with_help("an overlay's targets fit one spec: pass --namespace NAME")),
    }
}

fn actions_of(input: &InputSuggestions) -> Vec<SuggestedAction> {
    input
        .actions
        .iter()
        .map(|a| SuggestedAction {
            codes: a.codes.clone(),
            target: a.target.clone(),
            pointer: a.pointer.clone(),
            update: a.update.clone(),
        })
        .collect()
}

/// TG0920 for a fix whose value cannot be derived, TG0921 for a code no
/// overlay can fix.
fn skipped_diagnostics(skipped: &[Skipped]) -> Vec<Diagnostic> {
    skipped
        .iter()
        .map(|s| {
            let d = match s.kind {
                SkipKind::Underivable => Diagnostic::info(
                    "TG0920",
                    format!("no overlay action for {}: {}", s.code, s.reason),
                ),
                SkipKind::NoOverlayFix => Diagnostic::info(
                    "TG0921",
                    format!(
                        "{} ({}): {}",
                        s.code,
                        plural(s.count, "diagnostic", "diagnostics"),
                        s.reason
                    ),
                ),
            };
            match &s.label {
                Some(l) => d.at(l.file.clone(), l.pointer.clone(), l.span),
                None => d,
            }
        })
        .collect()
}

/// Write `text` to `path`; an existing entry needs `force` and must be a
/// regular file.
fn write(path: &Path, text: &str, force: bool) -> Result<(), (i32, CliError)> {
    let shown = path.display().to_string();
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !force {
            return Err((
                exit::REFUSED,
                CliError::new(ErrorKind::Refused, format!("refusing to overwrite {shown}"))
                    .with_help("pass --force to overwrite"),
            ));
        }
        if !meta.file_type().is_file() {
            return Err((
                exit::REFUSED,
                CliError::new(
                    ErrorKind::Refused,
                    format!("refusing to write through {shown}: not a regular file"),
                ),
            ));
        }
    }
    std::fs::write(path, text).map_err(|err| {
        (
            exit::INTERNAL,
            CliError::new(ErrorKind::Io, format!("cannot write {shown}: {err}")),
        )
    })
}
