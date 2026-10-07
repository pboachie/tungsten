// SPDX-License-Identifier: AGPL-3.0-only
//! Word splitting, casing, keyword escaping and collision disambiguation
//! (planning/03 "Identifiers").
//!
//! The pipeline is: [`split_words`] turns a wire name into lowercase words
//! (stored in [`crate::Ident::words`]); [`render`] cases the words for a
//! target and role and escapes the result; [`disambiguate`] makes every
//! rendered name in one scope unique by appending a numeric word. All
//! functions are pure: the same input always yields the same output.

use std::collections::{BTreeMap, BTreeSet};

use crate::Ident;

/// Target casings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    Snake,
    Camel,
    Pascal,
    ScreamingSnake,
    Kebab,
}

/// Target languages whose keyword tables are known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Target {
    TypeScript,
    Python,
    Rust,
}

/// What an identifier is used as; keyword rules differ per role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Type,
    Field,
    Method,
    Param,
    EnumVariant,
    Module,
}

/// Split a wire name into lowercase ASCII words.
///
/// Rules, applied left to right:
///
/// 1. Every character outside `[A-Za-z0-9]` is a separator, including all
///    non-ASCII characters (`__Host-zrotext_session` → `host`, `zrotext`,
///    `session`). Separators never appear in words; runs of separators
///    produce no empty words.
/// 2. A lowercase letter followed by an uppercase letter starts a new word
///    (`userName` → `user`, `name`).
/// 3. Inside a run of uppercase letters, the last uppercase letter starts a
///    new word when a lowercase letter follows it (`HTTPServer` → `http`,
///    `server`; `getHTTPSUrl` → `get`, `https`, `url`). Exception (plural
///    acronyms): when that lowercase letter is a single `s` that ends the
///    word (it is followed by the end of input, a separator, an uppercase
///    letter or a digit), the `s` joins the acronym (`IDs` → `ids`,
///    `userIDs` → `user`, `ids`).
/// 4. Digits attach to the word before them (`user2Name` → `user2`, `name`;
///    `e164` → `e164`). A letter after a digit starts a new word only when
///    the current word already contains a letter (`v1beta` → `v1`, `beta`),
///    so a word may start with digits (`2fa` → `2fa`, `2FAToken` → `2fa`,
///    `token`).
/// 5. All words are lowercased.
///
/// A name with no ASCII letters or digits yields no words.
pub fn split_words(wire: &str) -> Vec<String> {
    let mut words = vec![];
    for chunk in wire.split(|c: char| !c.is_ascii_alphanumeric()) {
        split_chunk(chunk.as_bytes(), &mut words);
    }
    words
}

/// Split one run of ASCII alphanumerics (rules 2 to 5 of [`split_words`]).
fn split_chunk(chunk: &[u8], out: &mut Vec<String>) {
    let mut current = String::new();
    let mut has_letter = false;
    for (i, &c) in chunk.iter().enumerate() {
        if i > 0 && starts_word(chunk, i, has_letter) {
            out.push(std::mem::take(&mut current));
            has_letter = false;
        }
        has_letter |= c.is_ascii_alphabetic();
        current.push(c.to_ascii_lowercase() as char);
    }
    if !current.is_empty() {
        out.push(current);
    }
}

/// Whether `chunk[i]` (with `i > 0`) begins a new word.
fn starts_word(chunk: &[u8], i: usize, word_has_letter: bool) -> bool {
    let prev = chunk[i - 1];
    let cur = chunk[i];
    let next = chunk.get(i + 1).copied();
    if prev.is_ascii_digit() {
        return cur.is_ascii_alphabetic() && word_has_letter;
    }
    if prev.is_ascii_lowercase() {
        return cur.is_ascii_uppercase();
    }
    // prev is uppercase.
    if !cur.is_ascii_uppercase() {
        return false;
    }
    match next {
        Some(b's') => {
            let after = chunk.get(i + 2).copied();
            after.is_some_and(|a| a.is_ascii_lowercase())
        }
        Some(n) => n.is_ascii_lowercase(),
        None => false,
    }
}

/// Join words in a casing.
///
/// Words are expected to come from [`split_words`] (lowercase ASCII
/// alphanumerics); they are lowercased again here and empty words are
/// skipped, so any input yields a well-formed result. Camel and Pascal case
/// capitalize the first character of each word (a no-op for a leading
/// digit) and insert `_` between two words when the first ends with a digit
/// and the second starts with one, so the boundary is not lost
/// (`["x86", "64"]` → `x86_64`). An empty word list yields `"_"`.
pub fn to_case(words: &[String], case: Case) -> String {
    let words: Vec<String> = words
        .iter()
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect();
    if words.is_empty() {
        return "_".into();
    }
    match case {
        Case::Snake => words.join("_"),
        Case::ScreamingSnake => words.join("_").to_ascii_uppercase(),
        Case::Kebab => words.join("-"),
        Case::Pascal => join_capitalized(&words, true),
        Case::Camel => join_capitalized(&words, false),
    }
}

fn join_capitalized(words: &[String], capitalize_first: bool) -> String {
    let mut out = String::new();
    for (i, word) in words.iter().enumerate() {
        let digit_boundary = out.ends_with(|c: char| c.is_ascii_digit())
            && word.starts_with(|c: char| c.is_ascii_digit());
        if digit_boundary {
            out.push('_');
        }
        if i == 0 && !capitalize_first {
            out.push_str(word);
        } else {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                out.push(first.to_ascii_uppercase());
                out.push_str(chars.as_str());
            }
        }
    }
    out
}

/// The casing [`render`] uses for a target and role.
///
/// | Role | TypeScript | Python | Rust |
/// |---|---|---|---|
/// | Type | Pascal | Pascal | Pascal |
/// | EnumVariant | Pascal | ScreamingSnake | Pascal |
/// | Field, Method, Param, Module | Camel | Snake | Snake |
pub fn case_for(target: Target, role: Role) -> Case {
    match (target, role) {
        (_, Role::Type) => Case::Pascal,
        (Target::Python, Role::EnumVariant) => Case::ScreamingSnake,
        (_, Role::EnumVariant) => Case::Pascal,
        (Target::TypeScript, _) => Case::Camel,
        (_, _) => Case::Snake,
    }
}

/// The identifier as it must appear in `target` source for `role`.
///
/// Steps, in order:
///
/// 1. Case the words with [`case_for`]. No words gives `_`.
/// 2. Leading digit: prefix `_` (`2fa` → `_2fa`).
/// 3. Builtin shadowing, role `Type` only: when the name equals a builtin
///    the generated code relies on ([`builtin_types`]), the word `model` is
///    appended and the name is cased again (`Error` → `ErrorModel`).
/// 4. Reserved words ([`is_reserved`]): append `_` (`type` → `type_` in
///    Python and Rust, `Self` → `Self_` in Rust, `_` → `__` in Python and
///    Rust). Comparison is case-sensitive on the final name, so a PascalCase
///    type never collides with a lowercase keyword.
///
/// The result can still collide with another identifier of the same scope;
/// [`disambiguate`] resolves that.
pub fn render(ident: &Ident, target: Target, role: Role) -> String {
    render_words(&ident.words, target, role)
}

fn render_words(words: &[String], target: Target, role: Role) -> String {
    let case = case_for(target, role);
    let mut name = with_digit_prefix(to_case(words, case));
    if role == Role::Type && builtin_types(target).contains(&name.as_str()) {
        let mut suffixed = words.to_vec();
        suffixed.push("model".into());
        name = with_digit_prefix(to_case(&suffixed, case));
    }
    if is_reserved(&name, target, role) {
        name.push('_');
    }
    name
}

fn with_digit_prefix(name: String) -> String {
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{name}")
    } else {
        name
    }
}

/// Whether `name` (already cased) is reserved in `target` for `role`, so
/// [`render`] appends `_`.
///
/// - Python: every hard keyword plus the soft keywords `match`, `case`,
///   `type` and `_`, for all roles.
/// - Rust: strict and reserved keywords of the 2024 edition (including
///   `self`, `Self`, `crate`, `super`, `gen`) plus `_`, for all roles.
/// - TypeScript: ECMAScript reserved words, strict-mode reserved words,
///   `arguments`, `eval`, and the TypeScript names that cannot name a
///   declaration (`any`, `boolean`, `number`, `string`, `symbol`, `type`,
///   ...), for roles `Type`, `Param`, `EnumVariant` and `Module`. Roles
///   `Field` and `Method` are member names, which may be any identifier in
///   TypeScript (`client.pets.delete()`), so only `constructor` is reserved
///   for `Method` and nothing for `Field`.
pub fn is_reserved(name: &str, target: Target, role: Role) -> bool {
    match (target, role) {
        (Target::TypeScript, Role::Field) => false,
        (Target::TypeScript, Role::Method) => name == "constructor",
        (Target::TypeScript, _) => TS_RESERVED.contains(&name),
        (Target::Python, _) => PY_RESERVED.contains(&name),
        (Target::Rust, _) => RS_RESERVED.contains(&name),
    }
}

/// Type names that would shadow a builtin the generated code of `target`
/// refers to without qualification. A type rendering to one of these gets
/// the word `model` appended (see [`render`]).
///
/// - TypeScript: the ECMAScript global constructors and the utility types
///   generated code uses (`Array`, `Error`, `Promise`, `Record`, ...).
/// - Python: the PascalCase builtins (`Exception`, `BaseException`, ...);
///   lowercase builtins such as `list` or `str` cannot collide with a
///   PascalCase type name.
/// - Rust: every type and trait of the standard prelude, its variants
///   (`Some`, `None`, `Ok`, `Err`) and `Error`.
pub fn builtin_types(target: Target) -> &'static [&'static str] {
    match target {
        Target::TypeScript => TS_BUILTIN_TYPES,
        Target::Python => PY_BUILTIN_TYPES,
        Target::Rust => RS_BUILTIN_TYPES,
    }
}

/// Make every identifier in one scope unique for `target`/`role`, in place.
///
/// Collisions are decided on the rendered name ([`render`]). Entries are
/// visited in input order (callers pass a deterministic order, normally
/// sorted by wire name). The first entry with a given rendered name keeps
/// it. Every later entry gets a numeric word appended to
/// [`Ident::words`] (its `wire` is never changed): the smallest `n >= 2`
/// whose rendering is free in the whole scope, meaning it is neither the
/// original rendering of any entry nor a name already assigned by this
/// call. Returns the wire names of the entries that changed, in input
/// order, for TG0401.
///
/// The search for `n` resumes, per word list, after the last number it
/// assigned: names only ever become taken, so every smaller number is
/// still taken, and `k` entries that render alike cost `O(k)` renders
/// instead of `O(k²)`.
pub fn disambiguate(idents: &mut [Ident], target: Target, role: Role) -> Vec<String> {
    let rendered: Vec<String> = idents.iter().map(|i| render(i, target, role)).collect();
    let mut taken: BTreeSet<String> = rendered.iter().cloned().collect();
    let mut claimed: BTreeSet<&str> = BTreeSet::new();
    let mut next: BTreeMap<Vec<String>, u64> = BTreeMap::new();
    let mut changed = vec![];
    for (ident, name) in idents.iter_mut().zip(&rendered) {
        if claimed.insert(name.as_str()) {
            continue;
        }
        let mut n: u64 = next.get(&ident.words).copied().unwrap_or(2);
        loop {
            let mut words = ident.words.clone();
            words.push(n.to_string());
            let candidate = render_words(&words, target, role);
            if taken.insert(candidate) {
                next.insert(ident.words.clone(), n + 1);
                ident.words = words;
                break;
            }
            n += 1;
        }
        changed.push(ident.wire.clone());
    }
    changed
}

const PY_RESERVED: &[&str] = &[
    "False", "None", "True", "_", "and", "as", "assert", "async", "await", "break", "case",
    "class", "continue", "def", "del", "elif", "else", "except", "finally", "for", "from",
    "global", "if", "import", "in", "is", "lambda", "match", "nonlocal", "not", "or", "pass",
    "raise", "return", "try", "type", "while", "with", "yield",
];

const RS_RESERVED: &[&str] = &[
    "Self", "_", "abstract", "as", "async", "await", "become", "box", "break", "const", "continue",
    "crate", "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if",
    "impl", "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub",
    "ref", "return", "self", "static", "struct", "super", "trait", "true", "try", "type", "typeof",
    "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

const TS_RESERVED: &[&str] = &[
    "any",
    "arguments",
    "await",
    "bigint",
    "boolean",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "eval",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "never",
    "new",
    "null",
    "number",
    "object",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "string",
    "super",
    "switch",
    "symbol",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "unknown",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

const TS_BUILTIN_TYPES: &[&str] = &[
    "Array", "BigInt", "Boolean", "Date", "Error", "Function", "Map", "Number", "Object",
    "Partial", "Promise", "Readonly", "Record", "RegExp", "Required", "Set", "String", "Symbol",
];

const PY_BUILTIN_TYPES: &[&str] = &["BaseException", "Ellipsis", "Exception", "NotImplemented"];

const RS_BUILTIN_TYPES: &[&str] = &[
    "Box",
    "Clone",
    "Copy",
    "Default",
    "Drop",
    "Eq",
    "Err",
    "Error",
    "Fn",
    "FnMut",
    "FnOnce",
    "From",
    "Into",
    "Iterator",
    "None",
    "Ok",
    "Option",
    "Ord",
    "PartialEq",
    "PartialOrd",
    "Result",
    "Send",
    "Sized",
    "Some",
    "String",
    "Sync",
    "ToOwned",
    "ToString",
    "Vec",
];
