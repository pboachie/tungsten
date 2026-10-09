// SPDX-License-Identifier: AGPL-3.0-only
//! Names of registered types and collision handling.
//!
//! Components keep their key as written. Other `$ref` targets are named
//! from their pointer (`Node/properties/children` → `NodeChildren`), inline
//! schemas from the hint words of their position (`ErrorCode` for the
//! inline enum of `Error.code`). Rendered collisions inside one scope are
//! resolved with [`tungsten_ir::naming::disambiguate`] for every target.

use tungsten_ir::Ident;
use tungsten_ir::naming::{Case, Role, Target, disambiguate, split_words, to_case};

const TARGETS: [Target; 3] = [Target::TypeScript, Target::Python, Target::Rust];

/// Lowercase words of hint parts (each part is split like a wire name).
pub(super) fn hint_words<S: AsRef<str>>(parts: &[S]) -> Vec<String> {
    parts.iter().flat_map(|p| split_words(p.as_ref())).collect()
}

/// `hint` followed by the words of `part`.
pub(super) fn extend(hint: &[String], part: &str) -> Vec<String> {
    let mut out = hint.to_vec();
    out.extend(split_words(part));
    out
}

/// `hint` followed by `words`.
pub(super) fn concat(hint: &[String], words: &[String]) -> Vec<String> {
    let mut out = hint.to_vec();
    out.extend_from_slice(words);
    out
}

/// The identifier of a type named by words (wire name in PascalCase).
pub(super) fn type_ident(words: &[String]) -> Ident {
    Ident::new(to_case(words, Case::Pascal))
}

/// Words naming a schema from its pointer tokens. A leading
/// `components/schemas`, `definitions` or `$defs` container is dropped;
/// `properties`/`$defs`/`definitions` tokens give way to the name after
/// them; `items` reads `item`, `additionalProperties` reads `value`, and a
/// composition member reads `variant <n>` (`part <n>` for `allOf`).
pub(super) fn pointer_words(tokens: &[String]) -> Vec<String> {
    let start = match tokens {
        [a, b, ..] if a == "components" && b == "schemas" => 2,
        [a, ..] if a == "definitions" || a == "$defs" => 1,
        _ => 0,
    };
    let mut out = vec![];
    let mut rest = tokens[start..].iter().peekable();
    while let Some(token) = rest.next() {
        match token.as_str() {
            "properties" | "patternProperties" | "$defs" | "definitions" => {}
            "items" => out.push("item".to_string()),
            "additionalProperties" => out.push("value".to_string()),
            "oneOf" | "anyOf" | "allOf" | "prefixItems" => {
                out.push(if token == "allOf" { "part" } else { "variant" }.to_string());
                if let Some(n) = rest.next_if(|t| t.parse::<usize>().is_ok())
                    && let Ok(n) = n.parse::<usize>()
                {
                    out.push((n + 1).to_string());
                }
            }
            other => out.extend(split_words(other)),
        }
    }
    out
}

/// Make the rendered names of `idents` unique in one scope for `role` in
/// every target. Entries are visited sorted by wire name (ties keep input
/// order), so the result does not depend on how the caller ordered them
/// beyond that. Returns the input indices whose words changed.
pub(super) fn disambiguate_all(idents: &mut [Ident], role: Role) -> Vec<usize> {
    let mut order: Vec<usize> = (0..idents.len()).collect();
    order.sort_by(|&a, &b| idents[a].wire.cmp(&idents[b].wire).then(a.cmp(&b)));
    disambiguate_in_order(idents, &order, role)
}

/// Like [`disambiguate_all`], visiting entries in the given order.
pub(super) fn disambiguate_in_order(
    idents: &mut [Ident],
    order: &[usize],
    role: Role,
) -> Vec<usize> {
    let mut sorted: Vec<Ident> = order.iter().map(|&i| idents[i].clone()).collect();
    for target in TARGETS {
        disambiguate(&mut sorted, target, role);
    }
    let mut changed = vec![];
    for (ident, &i) in sorted.into_iter().zip(order) {
        if ident.words != idents[i].words {
            idents[i] = ident;
            changed.push(i);
        }
    }
    changed.sort_unstable();
    changed
}
