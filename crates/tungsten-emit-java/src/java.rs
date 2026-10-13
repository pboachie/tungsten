// SPDX-License-Identifier: AGPL-3.0-only
//! Java source helpers: literals, Javadoc, file headers.

use tungsten_emit::{CommentStyle, Writer, header};
use tungsten_ir::Ir;

/// The runtime package generated code imports.
pub const RUNTIME: &str = "io.github.pboachie.tungsten.runtime";

/// A Java string literal. Control characters and everything outside ASCII
/// are `\uXXXX` escapes (UTF-16 units), so the source is ASCII whatever the
/// compiler's encoding.
pub fn lit(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{u:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

/// Text made safe inside a Javadoc comment: HTML special characters as
/// entities, no comment terminator and no `\u` sequence (javac reads
/// unicode escapes before comments end), ASCII only.
pub fn doc_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '@' => out.push_str("&#64;"),
            '\\' => out.push_str("&#92;"),
            '\t' => out.push(' '),
            '\n' | ' '..='~' => out.push(c),
            '\r' => {}
            _ if (c as u32) < 0x20 => {}
            _ => out.push_str(&format!("&#{};", c as u32)),
        }
    }
    out.replace("*/", "*&#47;")
}

/// Writes a Javadoc comment, wrapped at about 96 columns; empty text writes
/// nothing.
pub fn javadoc(w: &mut Writer, text: &str) {
    let text = doc_text(text.trim());
    if text.is_empty() {
        return;
    }
    let mut lines: Vec<String> = vec![];
    for (i, para) in text.split("\n\n").enumerate() {
        if i > 0 {
            lines.push(String::new());
        }
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && line.len() + word.len() + 1 > 92 {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        if !line.is_empty() {
            lines.push(line);
        }
    }
    w.line("/**");
    for l in lines {
        if l.is_empty() {
            w.line(" *");
        } else {
            w.line(format!(" * {l}"));
        }
    }
    w.line(" */");
}

/// A new source file: the generated header, the package and the imports
/// (sorted, without repeats).
pub fn file(ir: &Ir, package: &str, imports: &[String]) -> Writer {
    let mut w = Writer::new("    ");
    w.line(header(CommentStyle::DoubleSlash, ir));
    w.line(format!("package {package};"));
    let mut sorted: Vec<&String> = imports.iter().collect();
    sorted.sort();
    sorted.dedup();
    if !sorted.is_empty() {
        w.blank();
        for i in sorted {
            w.line(format!("import {i};"));
        }
    }
    w.blank();
    w
}

/// The first letter in upper case (`create` → `Create`).
pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}
