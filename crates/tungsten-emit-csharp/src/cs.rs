// SPDX-License-Identifier: AGPL-3.0-only
//! C# source helpers: literals, XML documentation comments, identifiers.

use tungsten_emit::Writer;
use tungsten_ir::naming::{self, Role, Target};

/// A C# string literal.
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
            '\0' => out.push_str("\\0"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' || c == '\u{85}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Text made safe for an XML documentation comment.
pub fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A `/// <summary>` comment; empty text writes nothing.
pub fn summary(w: &mut Writer, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    w.line("/// <summary>");
    for l in text.lines() {
        let l = l.trim_end();
        if l.is_empty() {
            w.line("///");
        } else {
            w.line(format!("/// {}", xml(l)));
        }
    }
    w.line("/// </summary>");
}

/// A one-line `/// <summary>` comment.
pub fn summary_line(w: &mut Writer, text: &str) {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !text.is_empty() {
        w.line(format!("/// <summary>{}</summary>", xml(&text)));
    }
}

/// The PascalCase member name of a word list (a property or method).
pub fn member(words: &[String]) -> String {
    naming::render(
        &tungsten_ir::Ident {
            wire: words.join(" "),
            words: words.to_vec(),
        },
        Target::CSharp,
        Role::Field,
    )
}

/// `name`, or `name2`, `name3`... so it is not in `taken`; the result is
/// added to `taken`.
pub fn unique(name: String, taken: &mut Vec<String>) -> String {
    let mut out = name.clone();
    let mut n = 2;
    while taken.contains(&out) {
        out = format!("{name}{n}");
        n += 1;
    }
    taken.push(out.clone());
    out
}

/// Members every record has, which no generated property may take.
pub const RECORD_MEMBERS: &[&str] = &[
    "AdditionalProperties",
    "Deconstruct",
    "EqualityContract",
    "Equals",
    "Finalize",
    "FromJson",
    "GetHashCode",
    "GetType",
    "MemberwiseClone",
    "PrintMembers",
    "ToJson",
    "ToString",
];
