// SPDX-License-Identifier: AGPL-3.0-only
//! Human rendering of diagnostics with source excerpts.
//!
//! ```text
//! error TG0201: unresolvable $ref
//!  --> openapi.json:7:18
//!   |
//! 7 |   "items": { "$ref": "#/components/schemas/Missing" }
//!   |                      ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ no such schema
//!   |
//!   = help: define the schema or fix the pointer
//! ```
//!
//! A label whose span cannot be mapped (no span, or a span outside the
//! source map) is shown as `file#pointer`. Multi-line spans are underlined
//! on their first line only. Very long lines (minified JSON) are cut to a
//! window around the span.

use std::fmt::Write as _;

use tungsten_core::{Diagnostic, Label, Severity, SourceFile, SourceMap};

use crate::output::CliError;

/// Lines longer than this many characters are windowed.
const MAX_EXCERPT_CHARS: usize = 120;
/// Characters kept before the span start when windowing.
const WINDOW_LEAD: usize = 40;
/// Display width of a tab in excerpts.
const TAB_WIDTH: usize = 4;
const ELLIPSIS: &str = "...";

/// A label resolved to a position in a loaded source file.
#[derive(Debug, Clone)]
pub struct Location<'a> {
    pub file: &'a SourceFile,
    /// Byte offsets into `file.text`, clamped and on character boundaries.
    pub start: usize,
    pub end: usize,
    /// 1-based line of `start`.
    pub line: u32,
    /// 1-based column of `start`, counted in characters.
    pub column: u32,
}

/// Resolve a label's span. Returns `None` when the label has no span, the
/// span's source is not in `sources`, or the source's name differs from the
/// label's file (a span from another source map).
pub fn locate<'a>(label: &Label, sources: &'a SourceMap) -> Option<Location<'a>> {
    let span = label.span?;
    let file = sources.files().get(usize::try_from(span.source.0).ok()?)?;
    if file.name != label.file {
        return None;
    }
    let text = &file.text;
    let start = floor_boundary(text, span.start as usize);
    let end = floor_boundary(text, span.end as usize).max(start);
    let (line, byte_col) = file.line_col(u32::try_from(start).ok()?);
    let line_start = start + 1 - byte_col as usize;
    let column = text[line_start..start].chars().count() + 1;
    Some(Location {
        file,
        start,
        end,
        line,
        column: u32::try_from(column).unwrap_or(u32::MAX),
    })
}

fn floor_boundary(text: &str, offset: usize) -> usize {
    let mut i = offset.min(text.len());
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// ANSI styling, or none.
#[derive(Debug, Clone, Copy)]
struct Paint {
    color: bool,
}

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[1;31m";
const YELLOW: &str = "\x1b[1;33m";
const CYAN: &str = "\x1b[1;36m";
const BLUE: &str = "\x1b[1;34m";

impl Paint {
    fn paint(self, style: &str, text: &str) -> String {
        if self.color && !text.is_empty() {
            format!("{style}{text}{RESET}")
        } else {
            text.to_string()
        }
    }
}

fn severity_style(severity: Severity) -> (&'static str, &'static str) {
    match severity {
        Severity::Error => ("error", RED),
        Severity::Warning => ("warning", YELLOW),
        Severity::Info => ("info", CYAN),
    }
}

/// Render one diagnostic. The result ends with a newline; callers separate
/// consecutive diagnostics with a blank line.
pub fn render_diagnostic(d: &Diagnostic, sources: &SourceMap, color: bool) -> String {
    let p = Paint { color };
    let (sev, sev_style) = severity_style(d.severity);
    let mut out = format!(
        "{}{}\n",
        p.paint(sev_style, &format!("{sev} {}", d.code)),
        p.paint(BOLD, &format!(": {}", d.message))
    );
    let located: Vec<Option<Location<'_>>> = d.labels.iter().map(|l| locate(l, sources)).collect();
    let width = located
        .iter()
        .flatten()
        .map(|l| l.line.to_string().len())
        .max()
        .unwrap_or(1);
    let pad = " ".repeat(width);
    let bar = p.paint(BLUE, "|");
    for (i, (label, loc)) in d.labels.iter().zip(&located).enumerate() {
        let primary = i == 0;
        let arrow = p.paint(BLUE, if primary { "-->" } else { ":::" });
        match loc {
            Some(loc) => {
                let _ = writeln!(
                    out,
                    "{pad}{arrow} {}:{}:{}",
                    loc.file.name, loc.line, loc.column
                );
                let _ = writeln!(out, "{pad} {bar}");
                let excerpt = Excerpt::new(loc);
                let lineno = p.paint(BLUE, &format!("{:>width$}", loc.line));
                push_trimmed(&mut out, &format!("{lineno} {bar} {}", excerpt.text));
                let (marker, style) = if primary {
                    ('^', sev_style)
                } else {
                    ('-', BLUE)
                };
                let mut underline = marker.to_string().repeat(excerpt.marker_len);
                if let Some(msg) = &label.message {
                    underline.push(' ');
                    underline.push_str(msg);
                }
                push_trimmed(
                    &mut out,
                    &format!(
                        "{pad} {bar} {}{}",
                        " ".repeat(excerpt.marker_offset),
                        p.paint(style, &underline)
                    ),
                );
                let _ = writeln!(out, "{pad} {bar}");
            }
            None => {
                let _ = writeln!(out, "{pad}{arrow} {}", file_pointer(label));
                if let Some(msg) = &label.message {
                    let _ = writeln!(out, "{pad} {} {msg}", p.paint(BLUE, "="));
                }
            }
        }
    }
    if let Some(help) = &d.help {
        let _ = writeln!(
            out,
            "{pad} {} {}: {help}",
            p.paint(BLUE, "="),
            p.paint(BOLD, "help")
        );
    }
    out
}

/// `file#pointer`, or just `file` for the document root.
fn file_pointer(label: &Label) -> String {
    if label.pointer.is_empty() {
        label.file.clone()
    } else {
        format!("{}#{}", label.file, label.pointer)
    }
}

fn push_trimmed(out: &mut String, line: &str) {
    out.push_str(line.trim_end());
    out.push('\n');
}

/// The displayed part of the span's first line and where to underline.
struct Excerpt {
    text: String,
    marker_offset: usize,
    marker_len: usize,
}

impl Excerpt {
    fn new(loc: &Location<'_>) -> Self {
        let line = loc.file.line_text(loc.line);
        let prefix_chars = loc.column as usize - 1;
        let span_text = loc.file.text.get(loc.start..loc.end).unwrap_or("");
        let span_chars = span_text
            .split(['\n', '\r'])
            .next()
            .unwrap_or("")
            .chars()
            .count();
        let chars: Vec<char> = line.chars().collect();
        let (from, to) = if chars.len() > MAX_EXCERPT_CHARS {
            let from = prefix_chars.saturating_sub(WINDOW_LEAD);
            (from, (from + MAX_EXCERPT_CHARS).min(chars.len()))
        } else {
            (0, chars.len())
        };
        let lead = if from > 0 { ELLIPSIS } else { "" };
        let tail = if to < chars.len() { ELLIPSIS } else { "" };
        let shown = &chars[from..to];
        let width = |cs: &[char]| cs.iter().map(|&c| char_width(c)).sum::<usize>();
        let mut text = String::from(lead);
        for &c in shown {
            if c == '\t' {
                text.push_str(&" ".repeat(TAB_WIDTH));
            } else {
                text.push(c);
            }
        }
        text.push_str(tail);
        let start = prefix_chars.clamp(from, to);
        let end = (prefix_chars + span_chars).clamp(start, to);
        Self {
            text,
            marker_offset: lead.len() + width(&chars[from..start]),
            marker_len: width(&chars[start..end]).max(1),
        }
    }
}

fn char_width(c: char) -> usize {
    if c == '\t' { TAB_WIDTH } else { 1 }
}

/// Render an invocation-level error for stderr.
pub fn render_error(err: &CliError, color: bool) -> String {
    let p = Paint { color };
    let mut out = format!(
        "{}{}\n",
        p.paint(RED, "error"),
        p.paint(BOLD, &format!(": {}", err.message))
    );
    if let Some(help) = &err.help {
        let _ = writeln!(
            out,
            "  {} {}: {help}",
            p.paint(BLUE, "="),
            p.paint(BOLD, "help")
        );
    }
    out
}
