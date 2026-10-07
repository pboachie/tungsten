// SPDX-License-Identifier: AGPL-3.0-only
//! Word splitting, casing, keyword escaping and collision disambiguation
//! (planning/03 "Identifiers").
//!
//! PHASE-1 STUB: owned by the naming work package. The signatures are the
//! contract; implementations below are deliberately minimal.

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

/// Split a wire name into lowercase words on `_ - . space /`, case
/// boundaries (`userID` → `user`, `id`; `HTTPServer` → `http`, `server`) and
/// letter/digit boundaries (`v1beta` → `v1`, `beta`; digits stay attached to
/// the preceding word). Characters outside `[A-Za-z0-9]` are separators.
pub fn split_words(wire: &str) -> Vec<String> {
    wire.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect()
}

/// Join words in a casing. An empty word list yields `"_"`.
pub fn to_case(words: &[String], case: Case) -> String {
    if words.is_empty() {
        return "_".into();
    }
    let cap = |w: &str| {
        let mut c = w.chars();
        c.next()
            .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
            .unwrap_or_default()
    };
    match case {
        Case::Snake => words.join("_"),
        Case::ScreamingSnake => words.join("_").to_ascii_uppercase(),
        Case::Kebab => words.join("-"),
        Case::Pascal => words.iter().map(|w| cap(w)).collect(),
        Case::Camel => {
            let mut s = words[0].clone();
            s.extend(words[1..].iter().map(|w| cap(w)));
            s
        }
    }
}

/// The identifier as it must appear in `target` source for `role`, with
/// casing applied and reserved words / invalid starts escaped.
pub fn render(ident: &crate::Ident, target: Target, role: Role) -> String {
    let case = match (target, role) {
        (_, Role::Type) => Case::Pascal,
        (Target::TypeScript, Role::EnumVariant) => Case::Pascal,
        (Target::Rust, Role::EnumVariant) => Case::Pascal,
        (Target::Python, Role::EnumVariant) => Case::ScreamingSnake,
        (Target::TypeScript, _) => Case::Camel,
        (_, _) => Case::Snake,
    };
    to_case(&ident.words, case)
}

/// Make every identifier in one scope unique for `target`/`role`, in place.
/// Input order must already be deterministic (sorted by wire name). Returns
/// the wire names that were disambiguated, for TG0401.
pub fn disambiguate(idents: &mut [crate::Ident], _target: Target, _role: Role) -> Vec<String> {
    let _ = idents;
    Vec::new()
}
