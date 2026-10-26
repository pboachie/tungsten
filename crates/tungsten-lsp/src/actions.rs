// SPDX-License-Identifier: AGPL-3.0-only
//! Quick fixes for the compiler's own diagnostics.
//!
//! A fix is offered only when the diagnostic already names the wrong value
//! and the right one is a known name close to it: an operation id of the
//! compiled project for `TG0603` (unknown operation references), or one of
//! the values the manifest schema accepts for `TG0602` (unknown variants
//! and fields). There are no editor-only rules: the server reads the
//! diagnostic's code and message and the last index, nothing else.
//!
//! A fix replaces one occurrence of the wrong value in the diagnostic's
//! lines of the current buffer. When the buffer no longer holds exactly one
//! such occurrence there, the diagnostic is stale and no fix is offered.
//! Every edit carries the buffer version it was computed for, so the editor
//! refuses it after a later edit; applying it is an ordinary, undoable text
//! edit. A fix never compiles, emits, fetches or writes anything itself.

use std::collections::HashMap;

use lsp_types::{
    CodeAction, CodeActionDisabled, CodeActionKind, CodeActionOrCommand, Diagnostic,
    NumberOrString, OptionalVersionedTextDocumentIdentifier, TextDocumentEdit, TextEdit, Uri,
    WorkspaceEdit,
};

use crate::analysis::Index;
use crate::position::LineIndex;

/// Most replacements offered for one diagnostic.
const MAX_FIXES: usize = 3;
/// Most diagnostics of one request that get fixes.
const MAX_DIAGNOSTICS: usize = 50;

/// The buffer a code action request is about.
pub struct Buffer<'a> {
    pub uri: &'a Uri,
    pub version: i32,
    /// Whether the client takes versioned `documentChanges`; without them
    /// the edit is a plain `changes` map, still checked against the
    /// current buffer text.
    pub document_changes: bool,
    pub text: &'a str,
    pub lines: &'a LineIndex,
}

/// The quick fixes for `diagnostics` (as the client sent them back) in
/// `buffer`.
pub fn actions(
    buffer: &Buffer<'_>,
    diagnostics: &[Diagnostic],
    index: Option<&Index>,
) -> Vec<CodeActionOrCommand> {
    let mut out = vec![];
    for d in diagnostics
        .iter()
        .filter(|d| d.source.as_deref() == Some("tungsten"))
        .take(MAX_DIAGNOSTICS)
    {
        let Some((wrong, candidates)) = wanted(d, index) else {
            continue;
        };
        let Some((start, end)) = locate(buffer, d, &wrong) else {
            continue;
        };
        let fixes = closest(&wrong, &candidates);
        let preferred = fixes.len() == 1;
        for fix in fixes {
            let edit = TextEdit {
                range: buffer.lines.range(start, end, buffer.text),
                new_text: fix.clone(),
            };
            out.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Replace `{wrong}` with `{fix}`"),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![d.clone()]),
                edit: Some(versioned(buffer, edit)),
                is_preferred: Some(preferred),
                ..CodeAction::default()
            }));
        }
    }
    out
}

/// An edit of one buffer that the client applies only at `buffer.version`.
fn versioned(buffer: &Buffer<'_>, edit: TextEdit) -> WorkspaceEdit {
    if !buffer.document_changes {
        return WorkspaceEdit {
            changes: Some(HashMap::from([(buffer.uri.clone(), vec![edit])])),
            ..WorkspaceEdit::default()
        };
    }
    WorkspaceEdit {
        document_changes: Some(lsp_types::DocumentChanges::Edits(vec![TextDocumentEdit {
            text_document: OptionalVersionedTextDocumentIdentifier {
                uri: buffer.uri.clone(),
                version: Some(buffer.version),
            },
            edits: vec![lsp_types::OneOf::Left(edit)],
        }])),
        changes: None::<HashMap<Uri, Vec<TextEdit>>>,
        change_annotations: None,
    }
}

/// Whether an overlay action of `tungsten overlay suggest` can fix `d`:
/// TG0403, or TG0401 on a repeated operation id.
pub fn overlay_fixable(d: &Diagnostic) -> bool {
    d.source.as_deref() == Some("tungsten")
        && match &d.code {
            Some(NumberOrString::String(code)) => {
                code == "TG0403" || (code == "TG0401" && d.message.starts_with("operation id `"))
            }
            _ => false,
        }
}

/// A disabled action explaining why the overlay-fixable `diagnostics` that
/// no action in `offered` covers get no fix, for editors that show
/// disabled actions.
pub fn unavailable_overlay(
    diagnostics: &[Diagnostic],
    offered: &[CodeActionOrCommand],
    reason: impl FnOnce() -> String,
) -> Option<CodeActionOrCommand> {
    let covered = |d: &Diagnostic| {
        offered.iter().any(|a| match a {
            CodeActionOrCommand::CodeAction(a) => a.diagnostics.iter().flatten().any(|c| c == d),
            CodeActionOrCommand::Command(_) => false,
        })
    };
    let left: Vec<Diagnostic> = diagnostics
        .iter()
        .filter(|d| overlay_fixable(d) && !covered(d))
        .take(MAX_DIAGNOSTICS)
        .cloned()
        .collect();
    if left.is_empty() {
        return None;
    }
    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: "Add the suggested overlay for this operation id".to_string(),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(left),
        disabled: Some(CodeActionDisabled { reason: reason() }),
        ..CodeAction::default()
    }))
}

/// Disabled actions explaining, from the capability catalog, why a
/// diagnostic about an input construct tungsten does not compile has no
/// fix: one per diagnostic code, for editors that show disabled actions.
/// The catalog's version lines declare each construct with its stable
/// code; nothing here is an editor-only rule.
pub fn unsupported_inputs(diagnostics: &[Diagnostic]) -> Vec<CodeActionOrCommand> {
    use tungsten_config::capabilities::{InputFeaturePolicy, OPENAPI_INPUTS};
    let mut by_code: Vec<(String, Vec<Diagnostic>)> = vec![];
    for d in diagnostics
        .iter()
        .filter(|d| d.source.as_deref() == Some("tungsten"))
        .take(MAX_DIAGNOSTICS)
    {
        let Some(NumberOrString::String(code)) = &d.code else {
            continue;
        };
        match by_code.iter_mut().find(|(c, _)| c == code) {
            Some((_, list)) => list.push(d.clone()),
            None => by_code.push((code.clone(), vec![d.clone()])),
        }
    }
    let mut out = vec![];
    for (code, list) in by_code {
        let declared = OPENAPI_INPUTS.iter().find_map(|line| {
            line.unsupported_features
                .iter()
                .find(|f| f.diagnostic == code)
                .map(|f| (line.line, f))
        });
        let Some((line, feature)) = declared else {
            continue;
        };
        let effect = match feature.policy {
            InputFeaturePolicy::Rejected => "is refused, so nothing is generated from the document",
            InputFeaturePolicy::Ignored => "is ignored and has no effect on the output",
        };
        let reason = format!(
            "OpenAPI {line} `{}` ({}) is not compiled by tungsten and {effect}; no edit \
             derives an equivalent. Remove or rewrite it in the spec or with an overlay. \
             `tungsten capabilities` lists the supported constructs and `tungsten explain \
             {code}` has the details",
            feature.member, feature.id,
        );
        out.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: format!("Fix `{}` ({code})", feature.member),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(list),
            disabled: Some(CodeActionDisabled { reason }),
            ..CodeAction::default()
        }));
    }
    out
}

/// The wrong value a diagnostic names and the names that could replace it.
fn wanted(d: &Diagnostic, index: Option<&Index>) -> Option<(String, Vec<String>)> {
    let Some(NumberOrString::String(code)) = &d.code else {
        return None;
    };
    let head = d.message.lines().next().unwrap_or_default();
    match code.as_str() {
        "TG0603" => {
            let index = index?;
            let wrong = if let Some(rest) = head.strip_prefix("unknown operation or macro ") {
                quoted(rest).into_iter().next()?
            } else if let Some(rest) = head.strip_prefix("unknown operation ") {
                quoted(rest).into_iter().next()?
            } else if head.ends_with("which is not an operation of the inputs") {
                quoted(head).into_iter().next()?
            } else {
                return None;
            };
            let mut names: Vec<String> = index.ops.iter().map(|o| o.id.clone()).collect();
            if head.starts_with("unknown operation or macro ") {
                names.extend(index.macros.iter().cloned());
            }
            Some((wrong, names))
        }
        "TG0602" => {
            let rest = head
                .strip_prefix("unknown variant ")
                .or_else(|| head.strip_prefix("unknown field "))?;
            let (wrong, expected) = rest.split_once(", expected ")?;
            let wrong = quoted(wrong).into_iter().next()?;
            Some((wrong, quoted(expected)))
        }
        _ => None,
    }
}

/// The backtick-quoted words of `text`, in order.
fn quoted(text: &str) -> Vec<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// The byte range of the one occurrence of `wrong` as a whole word in the
/// lines of the diagnostic in the current buffer.
fn locate(buffer: &Buffer<'_>, d: &Diagnostic, wrong: &str) -> Option<(usize, usize)> {
    let text = buffer.text;
    let first = d.range.start.line as usize;
    let last = (d.range.end.line as usize).max(first);
    if first >= buffer.lines.lines() {
        return None;
    }
    let from = buffer.lines.line_start(first, text);
    let to = buffer.lines.line_range(last, text).1;
    let region = text.get(from..to)?;
    let word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '.' | '-');
    let mut found = region.match_indices(wrong).filter(|(i, _)| {
        let before = region[..*i].chars().next_back();
        let after = region[i + wrong.len()..].chars().next();
        !before.is_some_and(word) && !after.is_some_and(word)
    });
    let (at, _) = found.next()?;
    if found.next().is_some() {
        return None;
    }
    Some((from + at, from + at + wrong.len()))
}

/// Names within a small edit distance of `wrong`, closest first; only the
/// closest distance is kept so a fix is never a guess between unequal
/// matches.
fn closest(wrong: &str, names: &[String]) -> Vec<String> {
    let limit = (wrong.chars().count() / 4).max(2);
    let mut scored: Vec<(usize, &String)> = names
        .iter()
        .filter(|n| n.as_str() != wrong)
        .map(|n| (edit_distance(wrong, n), n))
        .filter(|(d, _)| *d <= limit)
        .collect();
    scored.sort();
    scored.dedup_by(|a, b| a.1 == b.1);
    let best = scored.first().map(|(d, _)| *d);
    scored
        .into_iter()
        .take_while(|(d, _)| Some(*d) == best)
        .take(MAX_FIXES)
        .map(|(_, n)| n.clone())
        .collect()
}

/// Levenshtein distance over characters.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, &cb) in b.iter().enumerate() {
            let substitute = prev[j] + usize::from(ca != cb);
            cur[j + 1] = substitute.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}
