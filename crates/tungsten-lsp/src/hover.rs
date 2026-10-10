// SPDX-License-Identifier: AGPL-3.0-only
//! Hover and go-to-definition.

use lsp_types::{Hover, HoverContents, Location, MarkupContent, MarkupKind};

use crate::analysis::Index;
use crate::complete::{Inputs, op_markdown};
use crate::docs;
use crate::scan::{self, Seg};

fn markup(text: String) -> HoverContents {
    HoverContents::Markup(MarkupContent {
        kind: MarkupKind::Markdown,
        value: text,
    })
}

/// A diagnostic code: `TG` and four digits.
fn is_code(word: &str) -> bool {
    word.len() == 6 && word.starts_with("TG") && word[2..].bytes().all(|b| b.is_ascii_digit())
}

/// Documentation of the key, value, operation or diagnostic code at
/// `offset`.
pub fn hover(inputs: &Inputs<'_>, offset: usize) -> Option<Hover> {
    let scan = scan::scan(inputs.text, inputs.lines);
    let word = scan::word_at(inputs.text, inputs.lines, offset);
    let word_text = word.map(|(s, e)| &inputs.text[s..e]);
    let range = |(s, e): (usize, usize)| Some(inputs.lines.range(s, e, inputs.text));
    if let (Some(w), Some(text)) = (word, word_text)
        && is_code(text)
        && let Some(desc) = tungsten_core::diagnostic::codes::describe(text)
    {
        return Some(Hover {
            contents: markup(format!(
                "**{text}**\n\n{desc}\n\n`tungsten explain {text}` has the details."
            )),
            range: range(w),
        });
    }
    if let Some(entry) = scan.key_at(offset) {
        let (s, e) = entry.key_range?;
        // A map key that names an operation documents the operation.
        if let Some(op) = entry.key.as_deref().and_then(|k| inputs.index?.op(k)) {
            return Some(Hover {
                contents: markup(op_markdown(op)),
                range: range((s, e)),
            });
        }
        let text = field_doc(inputs, &entry.path)?;
        return Some(Hover {
            contents: markup(format!("`{}`\n\n{text}", scan::shape(&entry.path))),
            range: range((s, e)),
        });
    }
    let entry = scan.value_at(offset)?;
    let (w, text) = (word?, word_text?);
    if let Some(op) = inputs.index.and_then(|i| i.op(text)) {
        return Some(Hover {
            contents: markup(op_markdown(op)),
            range: range(w),
        });
    }
    let doc = docs::value(inputs.kind, &entry.path, text)?;
    Some(Hover {
        contents: markup(format!("`{text}`\n\n{doc}")),
        range: range(w),
    })
}

fn field_doc(inputs: &Inputs<'_>, path: &[Seg]) -> Option<String> {
    if let Some(text) = docs::field(inputs.kind, path) {
        return Some(text.to_string());
    }
    match inputs.kind {
        docs::Kind::Manifest => inputs.manifest.description(path),
        docs::Kind::Agent => inputs.agent.description(path),
        docs::Kind::Overlay => None,
    }
}

/// The OpenAPI operation named by the word at `offset`.
pub fn definition(inputs: &Inputs<'_>, index: &Index, offset: usize) -> Option<Location> {
    let (s, e) = scan::word_at(inputs.text, inputs.lines, offset)?;
    index.op(&inputs.text[s..e])?.location.clone()
}
