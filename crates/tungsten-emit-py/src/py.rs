// SPDX-License-Identifier: AGPL-3.0-only
//! Python lexical helpers: string literals, JSON values as Python literals,
//! docstrings, doc text and the import block.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use tungsten_emit::Writer;
use tungsten_ir::Doc;

/// Line length the generated packages declare for ruff; import lines
/// longer than this are wrapped the way ruff's isort wraps them.
pub(crate) const LINE_LENGTH: usize = 110;

/// A double-quoted Python string literal. JSON string escapes (`\"`, `\\`,
/// `\n`, `\uXXXX`) are valid Python escapes with the same meaning.
pub(crate) fn string_lit(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// A JSON value as a Python expression (compact, one line): `None`,
/// `True`, `False`, numbers, strings, lists and dicts.
pub(crate) fn json_lit(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => string_lit(s),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(json_lit).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", string_lit(k), json_lit(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Whether `s` is an ASCII Python identifier (`[A-Za-z_][A-Za-z0-9_]*`).
pub(crate) fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Python's hard keywords and the soft keywords naming escapes
/// (`tungsten_ir::naming`).
pub(crate) fn is_keyword(s: &str) -> bool {
    tungsten_ir::naming::is_reserved(
        s,
        tungsten_ir::naming::Target::Python,
        tungsten_ir::naming::Role::Param,
    )
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

/// Write a docstring at the current indentation: `"""One line."""`, or
/// the first line after the opening quotes and the closing quotes on their
/// own line. Backslashes are escaped and quotes that would end the string
/// early are broken up. Empty text writes nothing.
pub(crate) fn docstring(w: &mut Writer, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let mut safe = text.replace('\\', "\\\\").replace("\"\"\"", "\\\"\\\"\\\"");
    if safe.ends_with('"') {
        safe.pop();
        safe.push_str("\\\"");
    }
    if safe.contains('\n') {
        w.line(format!("\"\"\"{safe}"));
        w.line("\"\"\"");
    } else {
        w.line(format!("\"\"\"{safe}\"\"\""));
    }
}

/// A Python value tree printed one entry per line when it does not fit on
/// one line. Used for descriptor literals.
#[derive(Debug, Clone)]
pub(crate) enum Py {
    /// Code written verbatim (may span lines; continuation lines are
    /// indented with the value).
    Raw(String),
    Dict(Vec<(String, Py)>),
    List(Vec<Py>),
}

impl Py {
    pub(crate) fn str(s: &str) -> Py {
        Py::Raw(string_lit(s))
    }
    pub(crate) fn opt_str(s: Option<&str>) -> Py {
        s.map_or_else(Py::none, Py::str)
    }
    pub(crate) fn none() -> Py {
        Py::Raw("None".into())
    }
    pub(crate) fn json(v: &Value) -> Py {
        match v {
            Value::Array(items) => Py::List(items.iter().map(Py::json).collect()),
            Value::Object(map) => {
                Py::Dict(map.iter().map(|(k, v)| (k.clone(), Py::json(v))).collect())
            }
            _ => Py::Raw(json_lit(v)),
        }
    }
    pub(crate) fn bool(b: bool) -> Py {
        Py::Raw(if b { "True" } else { "False" }.into())
    }
    pub(crate) fn num(n: impl std::fmt::Display) -> Py {
        Py::Raw(n.to_string())
    }
    pub(crate) fn strs<S: AsRef<str>>(items: impl IntoIterator<Item = S>) -> Py {
        Py::List(items.into_iter().map(|s| Py::str(s.as_ref())).collect())
    }
    pub(crate) fn dict(entries: Vec<(&str, Py)>) -> Py {
        Py::Dict(
            entries
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    fn inline(&self) -> String {
        match self {
            Py::Raw(s) => s.clone(),
            Py::List(items) => format!(
                "[{}]",
                items.iter().map(Py::inline).collect::<Vec<_>>().join(", ")
            ),
            Py::Dict(entries) => format!(
                "{{{}}}",
                entries
                    .iter()
                    .map(|(k, v)| format!("{}: {}", string_lit(k), v.inline()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// Render as the value of a line that starts at column 0, with the value
    /// itself at column `col`.
    pub(crate) fn render(&self, col: usize) -> String {
        self.render_in(0, col)
    }

    /// Render with four spaces per level: the line holding the value is
    /// indented by `indent` columns and the value starts at column `col`
    /// (used to decide whether it fits on one line of [`LINE_LENGTH`]
    /// columns, its trailing comma included).
    pub(crate) fn render_in(&self, indent: usize, col: usize) -> String {
        const INDENT: &str = "    ";
        if let Py::Raw(s) = self {
            return s.clone();
        }
        let flat = self.inline();
        if col + flat.len() < LINE_LENGTH && !flat.contains('\n') {
            return flat;
        }
        let inner = indent + INDENT.len();
        match self {
            Py::Raw(s) => s.clone(),
            Py::List(items) => {
                let mut out = String::from("[\n");
                for item in items {
                    out.push_str(INDENT);
                    out.push_str(&indent_rest(&item.render_in(inner, inner), INDENT));
                    out.push_str(",\n");
                }
                out.push(']');
                out
            }
            Py::Dict(entries) => {
                let mut out = String::from("{\n");
                for (k, v) in entries {
                    let key = string_lit(k);
                    out.push_str(INDENT);
                    out.push_str(&key);
                    out.push_str(": ");
                    let rendered = v.render_in(inner, inner + key.len() + 2);
                    out.push_str(&indent_rest(&rendered, INDENT));
                    out.push_str(",\n");
                }
                out.push('}');
                out
            }
        }
    }
}

impl Py {
    /// The tree as JSON (for the test harness): Python literals become
    /// their JSON values, other code a string.
    #[cfg(feature = "testing")]
    pub(crate) fn to_json(&self) -> Value {
        match self {
            Py::Raw(s) => match s.as_str() {
                "None" => Value::Null,
                "True" => Value::Bool(true),
                "False" => Value::Bool(false),
                _ => serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.clone())),
            },
            Py::List(items) => Value::Array(items.iter().map(Py::to_json).collect()),
            Py::Dict(entries) => Value::Object(
                entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_json()))
                    .collect(),
            ),
        }
    }
}

/// Indent every line after the first by `indent`.
pub(crate) fn indent_rest(text: &str, indent: &str) -> String {
    let mut lines = text.split('\n');
    let mut out = lines.next().unwrap_or("").to_string();
    for l in lines {
        out.push('\n');
        if !l.is_empty() {
            out.push_str(indent);
        }
        out.push_str(l);
    }
    out
}

/// Natural order (digit runs compare by value), like ruff's sorting.
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn chunks(s: &str) -> Vec<(bool, &str)> {
        let mut out = vec![];
        let mut start = 0;
        let bytes = s.as_bytes();
        for i in 1..=bytes.len() {
            if i == bytes.len() || bytes[i].is_ascii_digit() != bytes[start].is_ascii_digit() {
                out.push((bytes[start].is_ascii_digit(), &s[start..i]));
                start = i;
            }
        }
        out
    }
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(&cb) {
        let ord = match (x, y) {
            ((true, dx), (true, dy)) => {
                let tx = dx.trim_start_matches('0');
                let ty = dy.trim_start_matches('0');
                tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty))
            }
            ((_, sx), (_, sy)) => sx.cmp(sy),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    ca.len().cmp(&cb.len())
}

/// isort's member category: constants, then classes, then the rest.
fn member_kind(name: &str) -> u8 {
    if name.len() > 1
        && name.chars().any(|c| c.is_ascii_alphabetic())
        && name == name.to_ascii_uppercase()
    {
        0
    } else if name.starts_with(|c: char| c.is_ascii_uppercase()) {
        1
    } else {
        2
    }
}

/// The order ruff's isort gives imported names (`order-by-type`, case
/// insensitive, natural).
pub(crate) fn member_cmp(a: &str, b: &str) -> Ordering {
    member_kind(a)
        .cmp(&member_kind(b))
        .then_with(|| natural_cmp(&a.to_ascii_lowercase(), &b.to_ascii_lowercase()))
        .then_with(|| natural_cmp(a, b))
}

/// The order of ruff's `__all__` sort (RUF022): `SCREAMING_CASE`, then
/// `CamelCase`, then the rest, each in natural order.
pub(crate) fn dunder_all_cmp(a: &str, b: &str) -> Ordering {
    member_kind(a)
        .cmp(&member_kind(b))
        .then_with(|| natural_cmp(a, b))
}

/// Names `tungsten_runtime` exports itself; every other runtime name is
/// imported from the contract module `tungsten_runtime.types`, where it is
/// defined (type checkers do not treat the package's `import *` of it as a
/// re-export).
const RUNTIME_ROOT: &[&str] = &[
    "AsyncClientCore",
    "ClientCore",
    "TungstenError",
    "UNSET",
    "Unset",
    "unwrap",
];

fn runtime_module<'m>(module: &'m str, name: &str) -> &'m str {
    if module == "tungsten_runtime" && !RUNTIME_ROOT.contains(&name) {
        "tungsten_runtime.types"
    } else {
        module
    }
}

/// The import section of a module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Section {
    Future,
    Stdlib,
    ThirdParty,
    /// Relative imports.
    Local,
}

/// Imports of one module, rendered in the order ruff's isort (default
/// settings) expects: sections future, standard library, third party,
/// relative; `import x` before `from x import ...`; relative modules from
/// the furthest; names constants first, then classes, then the rest;
/// aliased names on their own line.
#[derive(Debug, Clone, Default)]
pub(crate) struct PyImports {
    /// (module, name) → alias (`None` when not aliased).
    from: BTreeMap<String, BTreeSet<(String, Option<String>)>>,
}

impl PyImports {
    /// `from module import name`.
    pub(crate) fn add(&mut self, module: &str, name: &str) {
        let module = runtime_module(module, name);
        self.from
            .entry(module.to_string())
            .or_default()
            .insert((name.to_string(), None));
    }

    /// `from module import name as alias`.
    pub(crate) fn add_as(&mut self, module: &str, name: &str, alias: &str) {
        let module = runtime_module(module, name);
        self.from
            .entry(module.to_string())
            .or_default()
            .insert((name.to_string(), Some(alias.to_string())));
    }

    fn section(module: &str) -> Section {
        if module == "__future__" {
            Section::Future
        } else if module.starts_with('.') {
            Section::Local
        } else if matches!(
            module.split('.').next().unwrap_or(""),
            "base64"
                | "binascii"
                | "collections"
                | "dataclasses"
                | "functools"
                | "operator"
                | "re"
                | "types"
                | "typing"
        ) {
            Section::Stdlib
        } else {
            Section::ThirdParty
        }
    }

    /// The import block (no trailing blank line); empty when nothing is
    /// imported.
    pub(crate) fn render(&self) -> String {
        struct Stmt {
            section: Section,
            level: usize,
            module: String,
            first: String,
            text: String,
        }
        let mut stmts: Vec<Stmt> = vec![];
        for (module, names) in &self.from {
            let section = Self::section(module);
            let level = module.chars().take_while(|c| *c == '.').count();
            let plain: Vec<&str> = {
                let mut v: Vec<&str> = names
                    .iter()
                    .filter(|(_, a)| a.is_none())
                    .map(|(n, _)| n.as_str())
                    .collect();
                v.sort_by(|a, b| member_cmp(a, b));
                v
            };
            if !plain.is_empty() {
                stmts.push(Stmt {
                    section,
                    level,
                    module: module.clone(),
                    first: plain[0].to_string(),
                    text: from_line(module, &plain),
                });
            }
            for (name, alias) in names {
                if let Some(alias) = alias {
                    stmts.push(Stmt {
                        section,
                        level,
                        module: module.clone(),
                        first: name.clone(),
                        text: format!("from {module} import {name} as {alias}"),
                    });
                }
            }
        }
        stmts.sort_by(|a, b| {
            a.section
                .cmp(&b.section)
                .then_with(|| b.level.cmp(&a.level))
                .then_with(|| {
                    let ma = a.module.trim_start_matches('.');
                    let mb = b.module.trim_start_matches('.');
                    natural_cmp(&ma.to_ascii_lowercase(), &mb.to_ascii_lowercase())
                        .then_with(|| natural_cmp(ma, mb))
                })
                .then_with(|| member_cmp(&a.first, &b.first))
        });
        let mut out: Vec<String> = vec![];
        let mut last: Option<Section> = None;
        for s in &stmts {
            if last.is_some_and(|l| l != s.section) {
                out.push(String::new());
            }
            last = Some(s.section);
            out.push(s.text.clone());
        }
        out.join("\n")
    }
}

/// `from module import a, b`, wrapped one name per line when longer than
/// [`LINE_LENGTH`].
fn from_line(module: &str, names: &[&str]) -> String {
    let flat = format!("from {module} import {}", names.join(", "));
    if flat.len() <= LINE_LENGTH {
        return flat;
    }
    let mut out = format!("from {module} import (\n");
    for n in names {
        out.push_str(&format!("    {n},\n"));
    }
    out.push(')');
    out
}

/// Two blank lines (PEP 8, between top-level definitions). At the start
/// of a writer this leaves one leading newline, which callers trim.
pub(crate) fn two_blank(w: &mut Writer) {
    w.blank();
    w.line("");
}

/// `__all__ = [...]` of `names` (already in RUF022 order), wrapped one
/// name per line when longer than [`LINE_LENGTH`].
pub(crate) fn dunder_all(names: &[String]) -> String {
    let items: Vec<String> = names.iter().map(|n| string_lit(n)).collect();
    let flat = format!("__all__ = [{}]", items.join(", "));
    if flat.len() <= LINE_LENGTH {
        return flat;
    }
    let mut out = String::from("__all__ = [\n");
    for i in items {
        out.push_str(&format!("    {i},\n"));
    }
    out.push(']');
    out
}

/// The blank lines between an import block and `next` (the code after it),
/// as ruff's isort wants them: two before a class or function, one before
/// any other statement.
pub(crate) fn after_imports(next: &str) -> &'static str {
    let first = next.trim_start();
    if ["class ", "def ", "async def ", "@"]
        .iter()
        .any(|k| first.starts_with(k))
    {
        "\n\n"
    } else {
        "\n"
    }
}
