// SPDX-License-Identifier: AGPL-3.0-only
//! Identifier helpers shared by the operation steps: collision handling
//! across every target language and synthesized operation ids.

use tungsten_ir::naming::{self, Role, Target};
use tungsten_ir::{HttpMethod, Ident};

/// Every target whose rendered names must be unique.
const TARGETS: [Target; 3] = [Target::TypeScript, Target::Python, Target::Rust];

/// Make the rendered names of one scope unique in every target language
/// ([`naming::disambiguate`] applied per target, in a fixed target order,
/// so a suffix chosen for one target is seen by the next). Entries keep
/// their input order; the first entry with a name keeps it. Returns the
/// indices of the renamed entries, ascending.
pub(crate) fn disambiguate_all(idents: &mut [Ident], role: Role) -> Vec<usize> {
    let before: Vec<Vec<String>> = idents.iter().map(|i| i.words.clone()).collect();
    for target in TARGETS {
        naming::disambiguate(idents, target, role);
    }
    idents
        .iter()
        .zip(before)
        .enumerate()
        .filter(|(_, (ident, words))| ident.words != *words)
        .map(|(i, _)| i)
        .collect()
}

/// The lowercase name of an HTTP method (`get`).
pub(crate) fn method_word(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "get",
        HttpMethod::Put => "put",
        HttpMethod::Post => "post",
        HttpMethod::Delete => "delete",
        HttpMethod::Options => "options",
        HttpMethod::Head => "head",
        HttpMethod::Patch => "patch",
        HttpMethod::Trace => "trace",
    }
}

/// An operation id for an operation without `operationId`: the method,
/// then the words of every literal segment, then `by` and the words of
/// every path parameter, camelCased (`GET /v1/pets/{pet_id}` →
/// `getV1PetsByPetId`).
pub(crate) fn synthesize_operation_id(method: HttpMethod, path: &str) -> String {
    let mut words = vec![method_word(method).to_string()];
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        match segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            Some(param) => {
                words.push("by".into());
                words.extend(naming::split_words(param));
            }
            None => words.extend(naming::split_words(segment)),
        }
    }
    naming::to_case(&words, naming::Case::Camel)
}

/// PascalCase words of a name, for type name hints
/// (`createWebhookEndpoint` → `CreateWebhookEndpoint`).
pub(crate) fn pascal(name: &str) -> String {
    Ident::new(name).pascal()
}
