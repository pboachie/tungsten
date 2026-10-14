// SPDX-License-Identifier: AGPL-3.0-only
//! `lib/src/descriptors.dart`: the SDK descriptor document as string
//! constants.
//!
//! The document is the compact JSON text, cut into adjacent string
//! literals of at most [`LINE`] columns (one per line, as `dart format`
//! lays adjacent strings out) and into constants of at most
//! [`PART_LINES`] lines, joined by `+` into `descriptorDocument`. A piece
//! is a raw string (`r'...'`: `$` and `\` are taken as they are, so the
//! JSON escapes stay intact) unless it holds a `'`, a control or line
//! separator character, which a raw string cannot carry; such a piece is
//! an escaped string literal. `'''` can never occur, since no piece is a
//! triple-quoted string.

use tungsten_emit::sdk::{DocumentOptions, NameMap, SdkPlan, document_value};

use crate::Gen;
use crate::Library;
use crate::dart::{Code, str_lit};

use super::LibraryText;

/// The widest rendered piece (with its quotes), so that a continuation
/// line (`    r'...'` plus `;`) stays within 80 columns.
const LINE: usize = 72;

/// The widest first piece, after `const String _partN = `.
const FIRST: usize = 52;

/// The most pieces in one constant (more when the document would need
/// more than eight constants).
const PART_LINES: usize = 400;

/// The names of a plan for the document: the Namer's, with every argument
/// and macro input field named by its arguments layout key (the Dart SDK
/// keeps the TypeScript layout).
pub(crate) fn document_names(names: &NameMap, plan: &SdkPlan<'_>) -> NameMap {
    let mut doc = names.clone();
    for (i, op) in plan.operations.iter().enumerate() {
        if let Some(o) = doc.operations.get_mut(i) {
            o.arguments = op.arguments.iter().map(|a| a.key.clone()).collect();
        }
    }
    for (k, m) in plan.macros.iter().enumerate() {
        if let Some(n) = doc.macros.get_mut(k) {
            n.input = m.input.iter().map(|f| f.key.clone()).collect();
        }
    }
    doc
}

/// The document as compact JSON text.
pub(crate) fn document_text(g: &Gen<'_, '_>) -> String {
    let doc = document_value(
        g.plan,
        &document_names(&g.names, g.plan),
        &DocumentOptions {
            version: g.opts.version.clone(),
        },
    );
    serde_json::to_string(&doc).unwrap_or_else(|_| "{}".into())
}

/// Whether a raw single-quoted Dart string can carry `c`.
fn raw_ok(c: char) -> bool {
    c != '\'' && (c as u32) >= 0x20 && c != '\u{7f}' && c != '\u{2028}' && c != '\u{2029}'
}

/// The length of `c` inside an escaped string literal ([`str_lit`]).
fn escaped_len(c: char) -> usize {
    str_lit(&c.to_string()).chars().count() - 2
}

/// One piece as a Dart string literal.
fn literal(piece: &str) -> String {
    if piece.chars().all(raw_ok) {
        format!("r'{piece}'")
    } else {
        str_lit(piece)
    }
}

/// `text` cut into string literals: the first at most [`FIRST`] columns
/// wide, the others at most [`LINE`]; concatenated, they are `text`.
pub(crate) fn pieces(text: &str) -> Vec<String> {
    spans(text).into_iter().map(|t| literal(&t)).collect()
}

/// The texts of [`pieces`].
fn spans(text: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut cur = String::new();
    let mut chars = 0usize;
    let mut escaped = 0usize;
    let mut raw = true;
    for c in text.chars() {
        let limit = if out.is_empty() { FIRST } else { LINE };
        let width = if raw && raw_ok(c) {
            chars + 1 + 3
        } else {
            escaped + escaped_len(c) + 2
        };
        if width > limit && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            chars = 0;
            escaped = 0;
            raw = true;
        }
        cur.push(c);
        chars += 1;
        escaped += escaped_len(c);
        raw = raw && raw_ok(c);
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(cur);
    }
    out
}

/// `lib/src/descriptors.dart`.
pub(crate) fn library(g: &Gen<'_, '_>) -> Library {
    let text = document_text(g);
    let all = spans(&text);
    // At most eight constants, so that their sum fits on one line.
    let size = PART_LINES.max(all.len().div_ceil(8));
    // Each constant's first piece is narrower (it shares its line with the
    // declaration), so each part is cut again from its own text.
    let parts: Vec<Vec<String>> = all
        .chunks(size)
        .map(|chunk| pieces(&chunk.concat()))
        .collect();
    let mut w = Code::new();
    w.doc("The parsed document, built once on first use.");
    w.line("final SdkDescriptors descriptors = SdkDescriptors.parse(descriptorDocument);");
    w.blank();
    w.doc("The SDK descriptor document as JSON text.");
    let names: Vec<String> = (0..parts.len()).map(|i| format!("_part{i}")).collect();
    let joined = names.join(" + ");
    let flat = format!("const String descriptorDocument = {joined};");
    if w.fits(&flat) {
        w.line(flat);
    } else {
        w.line("const String descriptorDocument =");
        w.indent();
        w.indent();
        w.line(format!("{joined};"));
        w.dedent();
        w.dedent();
    }
    for (i, part) in parts.iter().enumerate() {
        w.blank();
        let head = format!("const String _part{i} = ");
        if part.len() == 1 {
            let line = format!("{head}{};", part[0]);
            if w.fits(&line) {
                w.line(line);
            } else {
                w.line(head.trim_end());
                w.indent();
                w.indent();
                w.line(format!("{};", part[0]));
                w.dedent();
                w.dedent();
            }
            continue;
        }
        for (j, piece) in part.iter().enumerate() {
            let last = j + 1 == part.len();
            let end = if last { ";" } else { "" };
            if j == 0 {
                w.line(format!("{head}{piece}"));
                w.indent();
                w.indent();
            } else {
                w.line(format!("{piece}{end}"));
            }
        }
        w.dedent();
        w.dedent();
    }
    let body = w.finish();
    let lib = LibraryText {
        doc: Some(
            "The SDK descriptor document of this package (format\n`tungsten-sdk-descriptors/1`): the data the runtime calls operations and\nruns macros with. It is a string constant, so nothing is read from a\nfile at run time (AOT and Flutter builds included).",
        ),
        locals: vec![],
        wire: "wire.dart",
        models: "models/",
        runtime: Some(&["SdkDescriptors"]),
    };
    Library {
        path: "src/descriptors.dart".into(),
        text: lib.finish(g, &body),
        exports: Some(vec!["descriptorDocument".into(), "descriptors".into()]),
    }
}
