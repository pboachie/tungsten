// SPDX-License-Identifier: AGPL-3.0-only
//! Ruby source helpers: literals and comments.

use tungsten_emit::Writer;

/// A double-quoted Ruby string literal of `text`: `\`, `"` and `#` (which
/// could start an interpolation) are escaped, control characters are
/// written as escapes, everything else as is.
pub fn str_lit(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '#' => out.push_str("\\#"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Comment lines (`# ...`) of a text, at the writer's indentation. Blank
/// lines of the text become `#`.
pub fn comment(w: &mut Writer, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            w.line("#");
        } else {
            w.line(format!("# {line}"));
        }
    }
}

/// Text collapsed to one line.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
