// SPDX-License-Identifier: AGPL-3.0-only
//! F# source helpers: literals, XML documentation comments, identifiers.

use tungsten_emit::Writer;
use tungsten_ir::naming::{self, Role, Target};

/// An F# string literal.
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
            c if (c as u32) < 0x20
                || c == '\u{7f}'
                || c == '\u{2028}'
                || c == '\u{2029}'
                || c == '\u{85}' =>
            {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// An F# array literal of string literals: `[| "a"; "b" |]`, or `[||]`.
pub fn string_array(items: &[String]) -> String {
    if items.is_empty() {
        "[||]".into()
    } else {
        format!(
            "[| {} |]",
            items.iter().map(|s| lit(s)).collect::<Vec<_>>().join("; ")
        )
    }
}

/// Text made safe for an XML documentation comment.
pub fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A `///` documentation comment; empty text writes nothing.
pub fn doc(w: &mut Writer, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    for l in text.lines() {
        let l = l.trim_end();
        if l.is_empty() {
            w.line("///");
        } else {
            w.line(format!("/// {}", xml(l)));
        }
    }
}

/// A one-line `///` documentation comment.
pub fn doc_line(w: &mut Writer, text: &str) {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !text.is_empty() {
        w.line(format!("/// {}", xml(&text)));
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

/// A name usable as a union case: F# requires an uppercase first letter.
pub fn case_name(name: String) -> String {
    match name.chars().next() {
        Some(c) if c.is_ascii_uppercase() => name,
        Some(c) if c.is_ascii_lowercase() => {
            let mut out = c.to_ascii_uppercase().to_string();
            out.push_str(&name[c.len_utf8()..]);
            out
        }
        _ => format!("V{name}"),
    }
}

/// F# keywords and reserved identifiers (lower case: the PascalCase names
/// the SDK renders never take one, but namespaces may).
const KEYWORDS: &[&str] = &[
    "abstract",
    "and",
    "as",
    "asr",
    "assert",
    "base",
    "begin",
    "break",
    "checked",
    "class",
    "component",
    "const",
    "constraint",
    "continue",
    "default",
    "delegate",
    "do",
    "done",
    "downcast",
    "downto",
    "elif",
    "else",
    "end",
    "event",
    "exception",
    "extern",
    "external",
    "false",
    "finally",
    "fixed",
    "for",
    "fun",
    "function",
    "global",
    "if",
    "in",
    "include",
    "inherit",
    "inline",
    "interface",
    "internal",
    "land",
    "lazy",
    "let",
    "lor",
    "lsl",
    "lsr",
    "lxor",
    "match",
    "member",
    "mixin",
    "mod",
    "module",
    "mutable",
    "namespace",
    "new",
    "not",
    "null",
    "of",
    "open",
    "or",
    "override",
    "parallel",
    "private",
    "process",
    "protected",
    "public",
    "pure",
    "rec",
    "return",
    "select",
    "sig",
    "static",
    "struct",
    "tailcall",
    "then",
    "to",
    "trait",
    "true",
    "try",
    "type",
    "upcast",
    "use",
    "val",
    "virtual",
    "void",
    "when",
    "while",
    "with",
    "yield",
];

/// An F# keyword or reserved identifier.
pub fn is_keyword(s: &str) -> bool {
    KEYWORDS.contains(&s)
}

/// Members every record and union has, which no generated field or case
/// may take.
pub const RECORD_MEMBERS: &[&str] = &[
    "AdditionalProperties",
    "CompareTo",
    "Empty",
    "Equals",
    "Finalize",
    "FromJson",
    "GetHashCode",
    "GetType",
    "MemberwiseClone",
    "Tag",
    "Tags",
    "ToJson",
    "ToString",
    "Value",
    "Values",
];
