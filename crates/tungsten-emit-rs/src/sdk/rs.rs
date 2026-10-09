// SPDX-License-Identifier: AGPL-3.0-only
//! Rust lexical helpers: string literals, doc comments, JSON as raw
//! strings, and a small expression printer that lays out descriptor data
//! the way rustfmt does (so generated files pass `cargo fmt --check`
//! without being formatted).

use serde_json::Value;
use tungsten_emit::Writer;
use tungsten_ir::Doc;

/// rustfmt's `max_width`.
pub(crate) const MAX_WIDTH: usize = 100;
/// rustfmt's `fn_call_width` and `array_width`.
pub(crate) const CALL_WIDTH: usize = 60;
/// rustfmt's `struct_lit_width`.
const STRUCT_WIDTH: usize = 18;
/// rustfmt's `short_array_element_width_threshold`.
const SHORT_ELEMENT: usize = 10;

/// A double-quoted Rust string literal.
pub(crate) fn string_lit(s: &str) -> String {
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
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The text of a doc: the full description, or the summary when there is
/// no description, or both when the description does not repeat the
/// summary.
pub(crate) fn doc_text(doc: Option<&Doc>) -> String {
    let Some(doc) = doc else {
        return String::new();
    };
    let summary = doc.summary.as_deref().map(str::trim).unwrap_or("");
    let description = doc.description.as_deref().map(str::trim).unwrap_or("");
    match (summary.is_empty(), description.is_empty()) {
        (_, true) => summary.to_string(),
        (true, false) => description.to_string(),
        (false, false) if description.starts_with(summary) => description.to_string(),
        (false, false) => format!("{summary}\n\n{description}"),
    }
}

/// The summary line of a doc (summary, else the description's first line).
pub(crate) fn doc_summary(doc: Option<&Doc>) -> String {
    let Some(doc) = doc else {
        return String::new();
    };
    match doc.summary.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => doc
            .description
            .as_deref()
            .and_then(|d| d.trim().lines().next())
            .unwrap_or("")
            .to_string(),
    }
}

/// Join doc paragraphs, skipping empty ones.
pub(crate) fn paragraphs<I, S>(parts: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    parts
        .into_iter()
        .map(|p| p.as_ref().trim().to_string())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Text that is safe inside `///` lines: tabs become spaces and carriage
/// returns vanish.
fn doc_clean(text: &str) -> String {
    text.replace('\r', "").replace('\t', "    ")
}

/// Write `text` as `///` lines at the current indentation (nothing for
/// empty text).
pub(crate) fn doc(w: &mut Writer, text: &str) {
    let text = doc_clean(text);
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    for line in text.lines() {
        if line.trim().is_empty() {
            w.line("///");
        } else {
            w.line(format!("/// {}", line.trim_end()));
        }
    }
}

/// A raw string literal holding `text` (with enough `#` to be safe).
pub(crate) fn raw_string(text: &str) -> String {
    let mut hashes = 1;
    let mut run = 0;
    let mut after_quote = false;
    for c in text.chars() {
        if c == '"' {
            after_quote = true;
            run = 0;
        } else if c == '#' && after_quote {
            run += 1;
            hashes = hashes.max(run + 1);
        } else {
            after_quote = false;
            run = 0;
        }
    }
    let h = "#".repeat(hashes);
    format!("r{h}\"{text}\"{h}")
}

/// A Rust expression tree printed in rustfmt's layout.
#[derive(Debug, Clone)]
pub(crate) enum Rx {
    /// Verbatim single-line text. `simple` marks literals and paths, which
    /// rustfmt packs several to a line in a long array.
    Atom { text: String, simple: bool },
    /// `callee(args)`.
    Call { callee: String, args: Vec<Rx> },
    /// `Name { field: value, .. }`.
    Struct {
        name: String,
        fields: Vec<(String, Rx)>,
    },
    /// `vec![items]`, or `[items]` (`open` is `vec![` or `[`).
    Vec { open: &'static str, items: Vec<Rx> },
}

impl Rx {
    /// Verbatim text; literals, paths and field accesses count as simple.
    pub(crate) fn atom(text: impl Into<String>) -> Rx {
        let text = text.into();
        let simple = is_simple(&text);
        Rx::Atom { text, simple }
    }

    /// `s("text")`: an owned `String`.
    pub(crate) fn string(text: &str) -> Rx {
        Rx::call("s", vec![Rx::atom(string_lit(text))])
    }

    pub(crate) fn call(callee: impl Into<String>, args: Vec<Rx>) -> Rx {
        Rx::Call {
            callee: callee.into(),
            args,
        }
    }

    pub(crate) fn some(inner: Rx) -> Rx {
        Rx::call("Some", vec![inner])
    }

    pub(crate) fn none() -> Rx {
        Rx::atom("None")
    }

    pub(crate) fn opt_string(text: Option<&str>) -> Rx {
        text.map_or_else(Rx::none, |t| Rx::some(Rx::string(t)))
    }

    pub(crate) fn boolean(b: bool) -> Rx {
        Rx::atom(if b { "true" } else { "false" })
    }

    pub(crate) fn strings<S: AsRef<str>>(items: impl IntoIterator<Item = S>) -> Rx {
        Rx::list(items.into_iter().map(|i| Rx::string(i.as_ref())).collect())
    }

    pub(crate) fn list(items: Vec<Rx>) -> Rx {
        if items.is_empty() {
            Rx::atom("Vec::new()")
        } else {
            Rx::Vec {
                open: "vec![",
                items,
            }
        }
    }

    pub(crate) fn record(name: impl Into<String>, fields: Vec<(&str, Rx)>) -> Rx {
        Rx::Struct {
            name: name.into(),
            fields: fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        }
    }

    /// A JSON value: `Value::Null` for null, else `json(r#"..."#)`.
    pub(crate) fn json(v: &Value) -> Rx {
        match v {
            Value::Null => Rx::atom("Value::Null"),
            Value::Bool(b) => Rx::call("Value::Bool", vec![Rx::boolean(*b)]),
            _ => {
                let text = serde_json::to_string(v).unwrap_or_else(|_| "null".into());
                Rx::call("json", vec![Rx::atom(raw_string(&text))])
            }
        }
    }

    fn is_expandable(&self) -> bool {
        !matches!(self, Rx::Atom { .. })
    }

    /// The one-line rendering.
    pub(crate) fn flat(&self) -> String {
        match self {
            Rx::Atom { text, .. } => text.clone(),
            Rx::Call { callee, args } => format!("{callee}({})", flat_args(args)),
            Rx::Struct { name, fields } => {
                if fields.is_empty() {
                    format!("{name} {{}}")
                } else {
                    format!("{name} {{ {} }}", flat_fields(fields))
                }
            }
            Rx::Vec { open, items } => format!("{open}{}]", flat_args(items)),
        }
    }

    /// Whether the one-line rendering obeys rustfmt's width limits at every
    /// level.
    pub(crate) fn flat_valid(&self) -> bool {
        match self {
            Rx::Atom { .. } => true,
            Rx::Call { args, .. } => {
                // A call of one literal or path has no width limit of its own.
                (flat_args(args).len() <= CALL_WIDTH || is_single_atom(args))
                    && args.iter().all(Rx::flat_valid)
            }
            Rx::Struct { fields, .. } => {
                flat_fields(fields).len() <= STRUCT_WIDTH
                    && fields.iter().all(|(_, v)| v.flat_valid())
            }
            Rx::Vec { items, .. } => {
                (flat_args(items).len() <= CALL_WIDTH || is_single_atom(items))
                    && items.iter().all(Rx::flat_valid)
            }
        }
    }

    /// Render starting at column `col` of a line whose block is indented by
    /// `indent`; `tail` is the width of what follows on the last line (`,`
    /// or `;`).
    pub(crate) fn render(&self, indent: usize, col: usize, tail: usize) -> String {
        self.render_with(indent, col, tail, false)
    }

    /// [`Rx::render`]; `force` lays out an expandable value over several
    /// lines even when it would fit on one (the last argument of a call
    /// that did not fit on one line).
    fn render_with(&self, indent: usize, col: usize, tail: usize, force: bool) -> String {
        let flat = self.flat();
        if !(force && self.is_expandable())
            && self.flat_valid()
            && col + flat.len() + tail <= MAX_WIDTH
        {
            return flat;
        }
        let pad = " ".repeat(indent + 4);
        let close_pad = " ".repeat(indent);
        match self {
            Rx::Atom { text, .. } => text.clone(),
            Rx::Call { callee, args } => {
                if let [only] = args.as_slice()
                    && only.is_expandable()
                {
                    let head = format!("{callee}(");
                    let inner = only.render_with(indent, col + head.len(), tail + 1, true);
                    return format!("{head}{inner})");
                }
                let mut out = format!("{callee}(\n");
                if mixed_items(args) {
                    pack_items(&mut out, args, indent);
                } else {
                    for a in args {
                        out.push_str(&pad);
                        out.push_str(&a.render(indent + 4, indent + 4, 1));
                        out.push_str(",\n");
                    }
                }
                out.push_str(&close_pad);
                out.push(')');
                out
            }
            Rx::Struct { name, fields } => {
                let mut out = format!("{name} {{\n");
                for (k, v) in fields {
                    out.push_str(&pad);
                    if matches!(v, Rx::Atom { text, .. } if text == k) {
                        out.push_str(k);
                    } else {
                        out.push_str(k);
                        out.push_str(": ");
                        out.push_str(&v.render(indent + 4, indent + 4 + k.len() + 2, 1));
                    }
                    out.push_str(",\n");
                }
                out.push_str(&close_pad);
                out.push('}');
                out
            }
            Rx::Vec { open, items } => {
                if let [only] = items.as_slice()
                    && only.is_expandable()
                {
                    let inner = only.render_with(indent, col + open.len(), tail + 1, true);
                    return format!("{open}{inner}]");
                }
                if mixed_items(items) {
                    let mut out = format!("{open}\n");
                    pack_items(&mut out, items, indent);
                    out.push_str(&close_pad);
                    out.push(']');
                    return out;
                }
                let mut out = format!("{open}\n");
                for i in items {
                    out.push_str(&pad);
                    out.push_str(&i.render(indent + 4, indent + 4, 1));
                    out.push_str(",\n");
                }
                out.push_str(&close_pad);
                out.push(']');
                out
            }
        }
    }
}

/// A literal, a path, a field access, or a reference to one.
fn is_simple(text: &str) -> bool {
    let t = text.strip_prefix('&').unwrap_or(text);
    if t.starts_with('"') {
        return t.len() >= 2 && t.ends_with('"') && !t[1..t.len() - 1].contains("\"");
    }
    !t.is_empty()
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.'))
}

fn is_single_atom(args: &[Rx]) -> bool {
    matches!(args, [Rx::Atom { .. }])
}

/// Whether rustfmt packs the items several to a line: every one is a
/// simple expression of at most ten columns.
fn mixed_items(items: &[Rx]) -> bool {
    !items.is_empty()
        && items
            .iter()
            .all(|i| matches!(i, Rx::Atom { simple: true, .. }) && i.flat().len() <= SHORT_ELEMENT)
}

/// Append `items` packed into lines of at most 99 columns, each line
/// ending with a comma, indented four columns past `indent`.
fn pack_items(out: &mut String, items: &[Rx], indent: usize) {
    let pad = " ".repeat(indent + 4);
    let all = flat_args(items);
    // All on one line: up to the full width. Over several lines: 99 columns.
    if indent + 4 + all.len() < MAX_WIDTH {
        out.push_str(&format!("{pad}{all},\n"));
        return;
    }
    let mut line = String::new();
    for item in items {
        let text = item.flat();
        if !line.is_empty() && indent + 4 + line.len() + 2 + text.len() + 1 > MAX_WIDTH - 1 {
            out.push_str(&pad);
            out.push_str(&line);
            out.push_str(",\n");
            line.clear();
        }
        if !line.is_empty() {
            line.push_str(", ");
        }
        line.push_str(&text);
    }
    out.push_str(&pad);
    out.push_str(&line);
    out.push_str(",\n");
}

fn flat_args(args: &[Rx]) -> String {
    args.iter().map(Rx::flat).collect::<Vec<_>>().join(", ")
}

fn flat_fields(fields: &[(String, Rx)]) -> String {
    fields
        .iter()
        .map(|(k, v)| field_text(k, &v.flat()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `name: value`, or `name` when the value is the variable of that name
/// (clippy's `redundant_field_names`).
fn field_text(name: &str, value: &str) -> String {
    if name == value {
        name.to_string()
    } else {
        format!("{name}: {value}")
    }
}

/// Write `prefix` followed by `value` rendered at the writer's current
/// indentation (`indent` columns), then `suffix` (a `;` or `,`).
pub(crate) fn put(w: &mut Writer, indent: usize, prefix: &str, value: &Rx, suffix: &str) {
    let text = value.render(indent, indent + prefix.len(), suffix.len());
    let mut lines = text.lines();
    let mut out = format!("{prefix}{}", lines.next().unwrap_or(""));
    for l in lines {
        out.push('\n');
        // The writer indents every line by `indent` again.
        out.push_str(l.get(indent..).unwrap_or(l));
    }
    out.push_str(suffix);
    w.line(out);
}

// ---------------------------------------------------------------- imports

/// Names a generated file can use unqualified, and where they come from.
const TOKENS: &[(&str, &str)] = &[
    ("AgentMeta", "tungsten_runtime"),
    ("ApiDescriptor", "tungsten_runtime"),
    ("ApiKeyLocation", "tungsten_runtime"),
    ("Arc", "std::sync"),
    ("AuthSchemeDescriptor", "tungsten_runtime"),
    ("BTreeMap", "std::collections"),
    ("BodyDescriptor", "tungsten_runtime"),
    ("BodyEncoding", "tungsten_runtime"),
    ("BodyShape", "tungsten_runtime"),
    ("CallOptions", "tungsten_runtime"),
    ("Category", "tungsten_runtime"),
    ("Checker", "crate::support"),
    ("ClientCore", "tungsten_runtime"),
    ("ClientOptions", "tungsten_runtime"),
    ("CompositePart", "tungsten_runtime"),
    ("ConfigError", "tungsten_runtime"),
    ("ConfirmationMeta", "tungsten_runtime"),
    ("Deserialize", "serde"),
    ("Deserializer", "serde"),
    ("Dispatch", "tungsten_runtime"),
    ("Duration", "std::time"),
    ("HttpMethod", "tungsten_runtime"),
    ("IdempotencyKind", "tungsten_runtime"),
    ("IdempotencyMeta", "tungsten_runtime"),
    ("Jitter", "tungsten_runtime"),
    ("LazyLock", "std::sync"),
    ("MacroDescriptor", "tungsten_runtime"),
    ("MacroInput", "tungsten_runtime"),
    ("MacroStep", "tungsten_runtime"),
    ("MacroStepKind", "tungsten_runtime"),
    ("MergedBodyField", "tungsten_runtime"),
    ("NonJsonError", "tungsten_runtime"),
    ("OnceLock", "std::sync"),
    ("OperationDescriptor", "tungsten_runtime"),
    ("OperationStatus", "tungsten_runtime"),
    ("Outcome", "tungsten_runtime"),
    ("Page", "tungsten_runtime"),
    ("PaginationDescriptor", "tungsten_runtime"),
    ("ParamDescriptor", "tungsten_runtime"),
    ("ParamLocation", "tungsten_runtime"),
    ("ParamRole", "tungsten_runtime"),
    ("ParamStyle", "tungsten_runtime"),
    ("PartialRetryOptions", "tungsten_runtime"),
    ("Patch", "tungsten_runtime"),
    ("PreviewMode", "tungsten_runtime"),
    ("PreviewResult", "tungsten_runtime"),
    ("Regex", "regex"),
    ("RemediationEntry", "tungsten_runtime"),
    ("ResponseDescriptor", "tungsten_runtime"),
    ("ResponseKind", "tungsten_runtime"),
    ("Retryable", "tungsten_runtime"),
    ("RpcDescriptor", "tungsten_runtime"),
    ("Safety", "tungsten_runtime"),
    ("Serialize", "serde"),
    ("Serializer", "serde"),
    ("StatusMatch", "tungsten_runtime"),
    ("TierRetries", "tungsten_runtime"),
    ("Typed", "crate::support"),
    ("TypedPages", "tungsten_runtime"),
    ("Value", "serde_json"),
    ("VerifyDescriptor", "tungsten_runtime"),
    ("decode", "tungsten_runtime"),
    ("decode_page", "tungsten_runtime"),
    ("json", "crate::support"),
    ("no_check", "crate::support"),
    ("s", "crate::support"),
];

/// Names generated modules must not define themselves: everything
/// [`TOKENS`] can import, so a model or request type never shadows one.
pub(crate) fn reserved_names() -> Vec<&'static str> {
    TOKENS
        .iter()
        .map(|(t, _)| *t)
        .filter(|t| t.chars().next().is_some_and(char::is_uppercase))
        .collect()
}

/// `code` without the contents of its string literals and comments.
pub(crate) fn strip_strings(code: &str) -> String {
    let chars: Vec<char> = code.chars().collect();
    let mut out = String::with_capacity(code.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == 'r'
            && matches!(chars.get(i + 1), Some('#' | '"'))
            && (i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_'))
        {
            let mut j = i + 1;
            let mut hashes = 0;
            while chars.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if chars.get(j) != Some(&'"') {
                out.push(c);
                i += 1;
                continue;
            }
            j += 1;
            loop {
                if j >= chars.len() {
                    break;
                }
                if chars[j] == '"' && (0..hashes).all(|k| chars.get(j + 1 + k) == Some(&'#')) {
                    j += 1 + hashes;
                    break;
                }
                j += 1;
            }
            out.push_str("\"\"");
            i = j;
        } else if c == '"' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '"' {
                j += if chars[j] == '\\' { 2 } else { 1 };
            }
            out.push_str("\"\"");
            i = j + 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether `token` occurs in `code` as a whole word that is not part of a
/// path (`a::token`) and not a method or field (`.token`).
fn uses_token(code: &str, token: &str) -> bool {
    let bytes = code.as_bytes();
    let mut from = 0;
    while let Some(i) = code[from..].find(token) {
        let start = from + i;
        let end = start + token.len();
        let before_ok = start == 0
            || !(is_word_byte(bytes[start - 1])
                || bytes[start - 1] == b'.'
                || (bytes[start - 1] == b':' && start >= 2 && bytes[start - 2] == b':'));
        let after_ok = end >= bytes.len() || !is_word_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

/// The `use` block of a file whose code is `code`: every token of
/// [`TOKENS`] it uses, the `extra` (module, name) imports, plus
/// `use crate::support as s;` when it calls `s::...` helpers. `skip` names
/// tokens the file defines itself; `only` limits the tokens imported.
pub(crate) fn imports_for(
    code: &str,
    skip: &[&str],
    only: Option<&[&str]>,
    extra: &[(String, String)],
) -> Vec<String> {
    let stripped = strip_strings(code);
    let mut by_module: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for (token, module) in TOKENS {
        if skip.contains(token) || only.is_some_and(|o| !o.contains(token)) {
            continue;
        }
        // `s` is the helper function (`s("..")`); `s::` is the module alias.
        let used = if *token == "s" {
            has_call(&stripped, "s")
        } else {
            uses_token(&stripped, token)
        };
        if used {
            by_module
                .entry((*module).to_string())
                .or_default()
                .insert((*token).to_string());
        }
    }
    for (module, name) in extra {
        by_module
            .entry(module.clone())
            .or_default()
            .insert(name.clone());
    }
    let alias = has_alias_use(&stripped)
        || code
            .lines()
            .any(|l| !l.trim_start().starts_with("//") && l.contains("\"s::"));
    let mut lines: Vec<(String, String)> = vec![];
    for (module, names) in by_module {
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let key = match names.as_slice() {
            [one] => format!("{module}::{one}"),
            _ => format!("{module}::{{"),
        };
        lines.push((key, use_line(&module, &names)));
    }
    if alias {
        lines.push(("crate::support".into(), "use crate::support as s;".into()));
    }
    lines.sort_by_key(|a| module_rank(&a.0));
    lines.into_iter().map(|(_, l)| l).collect()
}

fn module_rank(m: &str) -> (u8, String) {
    let rank = if m.starts_with("super") {
        0
    } else if m.starts_with("crate") {
        1
    } else {
        2
    };
    (rank, m.to_string())
}

/// Whether `s(` is a call of the helper `s` (not part of another word).
fn has_call(code: &str, name: &str) -> bool {
    let bytes = code.as_bytes();
    let pat = format!("{name}(");
    let mut from = 0;
    while let Some(i) = code[from..].find(&pat) {
        let start = from + i;
        if start == 0
            || !(is_word_byte(bytes[start - 1])
                || bytes[start - 1] == b'.'
                || bytes[start - 1] == b':')
        {
            return true;
        }
        from = start + pat.len();
    }
    false
}

/// Whether `s::` appears as a path start (module alias use).
fn has_alias_use(code: &str) -> bool {
    let bytes = code.as_bytes();
    let mut from = 0;
    while let Some(i) = code[from..].find("s::") {
        let start = from + i;
        if start == 0 || !(is_word_byte(bytes[start - 1]) || bytes[start - 1] == b':') {
            return true;
        }
        from = start + 3;
    }
    false
}

/// One `use module::{names};` statement in rustfmt's layout.
pub(crate) fn use_line(module: &str, names: &[&str]) -> String {
    let mut sorted: Vec<&str> = names.to_vec();
    sorted.sort_unstable();
    if let [one] = sorted.as_slice() {
        return format!("use {module}::{one};");
    }
    let flat = format!("use {module}::{{{}}};", sorted.join(", "));
    // rustfmt breaks a use of 100 columns, but not one of 99.
    if flat.len() < MAX_WIDTH {
        return flat;
    }
    let mut out = format!("use {module}::{{\n");
    let mut line = String::new();
    // All on one line: up to the full width. Over several lines: 99 columns.
    let one_line = format!("    {},", sorted.join(", "));
    if one_line.len() <= MAX_WIDTH {
        return format!("{out}{one_line}\n}};");
    }
    for n in sorted {
        if !line.is_empty() && 4 + line.len() + 2 + n.len() + 1 > MAX_WIDTH - 1 {
            out.push_str(&format!("    {line},\n"));
            line.clear();
        }
        if !line.is_empty() {
            line.push_str(", ");
        }
        line.push_str(n);
    }
    out.push_str(&format!("    {line},\n}};"));
    out
}

/// A method chain as a statement in rustfmt's layout. `prefix` is what
/// precedes it on the first line (`let x = `), `root` the chain root and
/// `parts` its children (`.a()`, `.await`). A chain of at most 60 columns
/// that fits stays on one line; else every child gets its own line, and a
/// root of at most four columns at the start of the line keeps the first
/// child.
pub(crate) fn chain_stmt(
    w: &mut Writer,
    indent: usize,
    prefix: &str,
    root: &str,
    parts: &[&str],
    suffix: &str,
) {
    let chain = format!("{root}{}", parts.concat());
    if chain.len() <= CALL_WIDTH && indent + prefix.len() + chain.len() + suffix.len() <= MAX_WIDTH
    {
        w.line(format!("{prefix}{chain}{suffix}"));
        return;
    }
    let mut rest: &[&str] = parts;
    let mut first = root.to_string();
    if prefix.is_empty() && root.len() <= 4 && !rest.is_empty() {
        first.push_str(rest[0]);
        rest = &rest[1..];
    }
    w.line(format!("{prefix}{first}"));
    w.indent();
    for (i, p) in rest.iter().enumerate() {
        let end = if i + 1 == rest.len() { suffix } else { "" };
        w.line(format!("{p}{end}"));
    }
    w.dedent();
}
