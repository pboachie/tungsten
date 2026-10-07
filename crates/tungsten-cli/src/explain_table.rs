// SPDX-License-Identifier: AGPL-3.0-only
//! Extended explanations for `tungsten explain TGxxxx`.
//!
//! The one-line summary of each code lives in
//! `tungsten_core::diagnostic::codes::REGISTRY`; this table adds what the
//! diagnostic means and how it is usually fixed.

/// An extended explanation of one diagnostic code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Explanation {
    pub code: &'static str,
    pub meaning: &'static str,
    pub fix: &'static str,
}

const fn e(code: &'static str, meaning: &'static str, fix: &'static str) -> Explanation {
    Explanation { code, meaning, fix }
}

/// Sorted by code. An entry may precede its code's registration in
/// `REGISTRY`; `explain` only offers codes that are registered.
pub(crate) const EXPLANATIONS: &[Explanation] = &[
    e(
        "TG0101",
        "An input named in tungsten.yml or on the command line could not be read: the file \
         does not exist, is a directory, or is not readable by the current user.",
        "Check the path. Paths in tungsten.yml are relative to the directory of tungsten.yml, \
         not to the working directory.",
    ),
    e(
        "TG0102",
        "The input is neither valid JSON nor valid YAML, so no OpenAPI document could be read \
         from it. The message names the first syntax error.",
        "Fix the syntax error at the reported position. Run the file through a JSON or YAML \
         linter if the position is not obvious.",
    ),
    e(
        "TG0103",
        "The document's `openapi` field is missing or names a version tungsten does not \
         compile. tungsten reads OpenAPI 3.0.x and 3.1.x; Swagger 2.0 documents declare \
         `swagger: \"2.0\"` instead.",
        "Set `openapi` to the version the document follows, or convert a Swagger 2.0 document \
         to OpenAPI 3 (for example with swagger2openapi) before compiling.",
    ),
    e(
        "TG0104",
        "A field the OpenAPI specification requires is absent (for example `info`, \
         `info.title`, `info.version`, or a parameter's `name` or `in`).",
        "Add the field at the reported pointer.",
    ),
    e(
        "TG0105",
        "The input is larger than the configured byte limit or nests deeper than the depth \
         limit. The limits protect the compiler from pathological or hostile inputs.",
        "Split very large documents into files joined by `$ref`, or flatten deeply nested \
         inline schemas into named components.",
    ),
    e(
        "TG0201",
        "A `$ref` points at a location that does not exist: the JSON Pointer names a missing \
         member, or the referenced file has no such path.",
        "Correct the pointer (pointers are case-sensitive and escape `/` as `~1` and `~` as \
         `~0`), or add the missing definition.",
    ),
    e(
        "TG0202",
        "A `$ref` names a remote URL. Remote references are fetched only from allowlisted URL \
         prefixes so that compiling never depends on, or leaks to, arbitrary hosts.",
        "Vendor the remote document next to the spec and reference it by relative path, or \
         allowlist its URL prefix.",
    ),
    e(
        "TG0203",
        "Named schemas reference each other in a cycle. This is legal; the cycle is kept and \
         the participating types are marked recursive so emitters box or lazily evaluate the \
         recursive edges.",
        "Nothing to fix. The note exists so recursive types are visible in review.",
    ),
    e(
        "TG0204",
        "A `$ref` points at a file outside the directory tree of the input, which tungsten \
         refuses to read.",
        "Move the referenced file under the input root, or point the manifest at a common \
         parent directory.",
    ),
    e(
        "TG0205",
        "A reference cycle runs through inline schemas only, so there is no named type at \
         which generated code could break the recursion.",
        "Move the recursive schema under `components/schemas` and reference it by name, or \
         list the edge in tungsten.yml `types.break_cycles` (for example `Node.children`).",
    ),
    e(
        "TG0206",
        "An overlay action's `target` JSONPath matched no node in the document, so the action \
         had no effect.",
        "Fix the JSONPath expression, or remove the action if the spec no longer contains the \
         node it was written for.",
    ),
    e(
        "TG0207",
        "An overlay file is not an OpenAPI Overlay 1.x document, has no `actions` array, or \
         contains an action without a valid `target` JSONPath, so it could not be applied.",
        "Start the file with `overlay: 1.0.0` and an `info` block, list the changes under \
         `actions`, and give each action a `target` JSONPath plus `update` or `remove`.",
    ),
    e(
        "TG0301",
        "A `oneOf` or `anyOf` has no discriminator, so a value's variant can only be found by \
         trying the candidates in order at runtime. The order is deterministic (most \
         constrained first), but sniffing is slower and can pick the wrong variant when \
         candidates overlap.",
        "Add a `discriminator` with a property every variant declares as a `const` or single \
         enum value.",
    ),
    e(
        "TG0302",
        "The members of an `allOf` declare the same property with incompatible types, or mix \
         records with non-records, so they cannot be merged into one record. The type is kept \
         as an intersection, which some targets express poorly.",
        "Make the overlapping property types agree, or restructure the schema so `allOf` \
         only combines compatible records.",
    ),
    e(
        "TG0303",
        "A schema uses a keyword tungsten does not model (for example `not`, \
         `if`/`then`/`else` or `patternProperties`). The keyword is ignored, so generated \
         validation is looser than the spec.",
        "Nothing is required. If the constraint matters to clients, express it with supported \
         keywords or document it in the description.",
    ),
    e(
        "TG0304",
        "A string schema uses a `format` tungsten does not recognize. The value is treated as \
         a plain string and the format name is preserved in the IR.",
        "Use a standard format (`uuid`, `date-time`, `email`, ...) if one fits; otherwise the \
         warning can be accepted.",
    ),
    e(
        "TG0305",
        "A schema has no `type` and nothing else (enum, const, properties, items, \
         composition) from which a type could be inferred, so it accepts any value.",
        "Add a `type`, or an explicit empty schema `{}` if any value really is allowed.",
    ),
    e(
        "TG0401",
        "Two names in one scope (fields of a record, operations of a resource, types of a \
         namespace) map to the same identifier in some target language. The later one in \
         sorted order was renamed deterministically.",
        "Rename one of them in the spec, or give an explicit name in tungsten.yml `naming`, so \
         the generated name is chosen rather than derived.",
    ),
    e(
        "TG0402",
        "A name is a reserved word in a target language or does not start with a letter, so \
         it was escaped (for example `type` becomes `type_` in Python and Rust). The wire name \
         is unchanged.",
        "Nothing is required. Rename in tungsten.yml `naming` if the escaped form reads badly.",
    ),
    e(
        "TG0403",
        "An operation has no `operationId`, so its id and method name were synthesized from \
         the HTTP method and path. Synthesized names change when the path changes.",
        "Add an `operationId` to the operation so its id stays stable.",
    ),
    e(
        "TG0501",
        "No explicit pagination was configured and the operation matched the cursor \
         heuristic (a closed response record with one array field and one nullable `next_*` \
         field, and a query parameter of the same name). Generated SDKs will iterate pages.",
        "Confirm the inference, or declare the operation under tungsten.yml `pagination` (use \
         `none: true` to switch inference off for it).",
    ),
    e(
        "TG0502",
        "An operation's `security` requirement names a scheme that is not defined under \
         `components/securitySchemes`, so tungsten cannot tell how to authenticate it.",
        "Define the scheme under `components/securitySchemes`, or fix the name in the \
         requirement.",
    ),
    e(
        "TG0503",
        "A composite auth profile in tungsten.yml claims to satisfy a security requirement \
         but does not provide every scheme the requirement combines.",
        "List every scheme of the requirement under the profile's `satisfies`, and add parts \
         that supply them.",
    ),
    e(
        "TG0504",
        "The operation does not match the input's `include` predicate, so it is not callable. \
         When it matches `planned_from` it is recorded as planned for documentation.",
        "Nothing is required. Change the predicate or the operation's extension value if the \
         operation should be callable.",
    ),
    e(
        "TG0505",
        "The operation carries `x-runtime-gate`, so it only works when the server enables the \
         named gate. Generated clients report the gate instead of a bare 404.",
        "Nothing is required. Describe the gate under agent.yml `gates` so agents get a useful \
         explanation.",
    ),
    e(
        "TG0506",
        "The input's `rpc_unflatten` names a path and method that do not exist, or whose \
         request body is not a `oneOf` discriminated by a `const` method field, so it cannot \
         be split into one operation per method.",
        "Check `rpc_unflatten.path`, `method`, `discriminator` and `params` against the spec.",
    ),
    e(
        "TG0601",
        "A manifest (tungsten.yml or agent.yml) is not valid YAML.",
        "Fix the YAML syntax at the reported position. Indentation must use spaces.",
    ),
    e(
        "TG0602",
        "A manifest is valid YAML but does not match its schema: a key is unknown, a required \
         key is missing, or a value has the wrong type. The pointer names the offending node.",
        "Correct the value at the pointer. Editors with the YAML language server validate \
         against the schema named in the file's first-line comment (`tungsten schema \
         tungsten` prints it).",
    ),
    e(
        "TG0603",
        "A manifest refers to an operation id that does not exist in the compiled API.",
        "Use `namespace.operationId`; `tungsten explain <id>` shows whether an id exists.",
    ),
    e(
        "TG0604",
        "A manifest refers to a namespace, resource or path that does not exist in the \
         inputs.",
        "Check the name against `inputs[].namespace` in tungsten.yml and the paths of the \
         spec.",
    ),
    e(
        "TG0610",
        "The spec uses an `x-agent-*` extension tungsten does not know, often a misspelling. \
         It is ignored.",
        "Fix the extension name (for example `x-agent-safety`), or rename it outside the \
         `x-agent-` prefix if it is meant for another tool.",
    ),
    e(
        "TG0901",
        "Generated output was produced from inputs whose digests differ from the current \
         inputs, so it no longer matches the spec and manifests.",
        "Regenerate and commit the output.",
    ),
];

/// The extended explanation of a code, if one exists.
pub(crate) fn explanation(code: &str) -> Option<&'static Explanation> {
    EXPLANATIONS
        .binary_search_by(|e| e.code.cmp(code))
        .ok()
        .map(|i| &EXPLANATIONS[i])
}

/// The code range a diagnostic belongs to (planning/03).
pub(crate) fn area(code: &str) -> &'static str {
    match code.get(..4) {
        Some("TG01") => "parsing and input limits (TG01xx)",
        Some("TG02") => "references and cycles (TG02xx)",
        Some("TG03") => "type normalization (TG03xx)",
        Some("TG04") => "naming (TG04xx)",
        Some("TG05") => "operations, pagination and auth inference (TG05xx)",
        Some("TG06") => "manifests (TG06xx)",
        Some("TG07") => "emitter limits (TG07xx)",
        Some("TG09") => "staleness and CI (TG09xx)",
        _ => "unassigned range",
    }
}
