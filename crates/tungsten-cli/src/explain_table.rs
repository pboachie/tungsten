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
        "A `$ref` names a remote URL. This version of tungsten does not fetch remote \
         references, so that compiling never depends on, or leaks to, other hosts.",
        "Vendor the remote document next to the spec and reference it by relative path.",
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
        "TG0306",
        "A `discriminator` next to `oneOf`/`anyOf` could not be used as written: it has no \
         `propertyName`, a variant is not an object schema, a `mapping` entry does not \
         resolve or names a schema that is not one of the variants, or a variant has no tag \
         value. Unusable mapping entries are dropped; an unusable discriminator is ignored and \
         the union is classified as if it had none.",
        "Point every `mapping` value at one of the union's variants (`#/components/schemas/X` \
         or the bare name `X`), and make every variant an object schema with the property.",
    ),
    e(
        "TG0307",
        "A schema admits no value at all: an empty `enum`, or a `oneOf`/`anyOf` with no \
         members. Its type is `never`, so no generated client can send or accept it.",
        "List the allowed values, or remove the schema if it is unused.",
    ),
    e(
        "TG0308",
        "An entry of tungsten.yml `types.break_cycles` does not name a property of a schema \
         under components/schemas (entries have the form `Type.field`), so it has no effect.",
        "Fix the entry to `Type.field` with the component key and property name as written in \
         the spec, or remove it.",
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
        "No explicit pagination was configured and the operation matched a heuristic: cursor \
         (a closed response record with one array field and one nullable `next_*` field, and \
         a query parameter of the same name), or offset/page (`offset` and `limit`, or `page` \
         and a page size, with an array of items). Generated SDKs will iterate pages.",
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
        "A composite auth profile in tungsten.yml lists under `satisfies` a name that is not a \
         security scheme of any input, so the profile cannot stand in for it.",
        "List the scheme names the operations' `security` requirements use (from \
         `components/securitySchemes`), and add parts that supply them.",
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
        "TG0507",
        "A path template names a `{param}` that no path-level or operation-level parameter \
         declares. The SDK treats it as a required string so the operation stays callable.",
        "Declare the parameter with `in: path`, `required: true` and a schema.",
    ),
    e(
        "TG0508",
        "Part of an operation does not have the shape OpenAPI requires and was skipped: a \
         parameter without `name` or `in`, a path parameter missing from the template, a \
         response key that is not a status code, `NXX` range or `default`, or a member that \
         must be an object or array but is not.",
        "Fix the element at the reported pointer; the message names what is wrong.",
    ),
    e(
        "TG0509",
        "A security scheme uses a type tungsten cannot generate authentication for (for \
         example `mutualTLS` or an HTTP scheme other than `bearer` and `basic`), or lacks a \
         field its type requires. Requirements naming it stay in the IR without a scheme.",
        "Describe the credential with a supported scheme, or authenticate those operations \
         through a custom transport.",
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
        "TG0605",
        "An agent rule points at an operation that cannot do what the rule needs: a \
         verification hook or a macro poll step names an operation that is not read_only \
         (the runtime would repeat its side effect), a paginate step names an operation \
         without pagination, or an endpoint preview names the operation itself. The rule \
         (or the macro) is dropped.",
        "Point the rule at a read_only operation (for a poll or a verification hook), at a \
         paginated one (for paginate), or declare the operation's safety or pagination if \
         the inferred one is wrong.",
    ),
    e(
        "TG0606",
        "A remediation entry in agent.yml (`errors.codes` or a tool's `remediation`) or in \
         `x-agent-remediation` is keyed by an error code that the error model of the \
         operation's namespace (or of any namespace, for `errors.codes`) does not list. The \
         entry is kept but may never match a response.",
        "Use a code from the error schema's `code` enum (`tungsten ir dump` shows \
         `namespaces[].errors.codes`), or fix the spelling.",
    ),
    e(
        "TG0607",
        "An agent rule names a field that does not exist: a confirmation summary field or \
         message placeholder that is not a request parameter or body field, a sensitive \
         field that is not in the success response, a verification argument or predicate \
         path, a macro reference such as `$accepted.message_id` into a step's response, or \
         the code field of `errors.envelope`.",
        "Use the wire names of the spec (dotted for nested fields, for example \
         `key.action_id`); `tungsten explain <operation>` lists an operation's parameters \
         and body.",
    ),
    e(
        "TG0608",
        "A tool's `gate` names neither an `x-runtime-gate` environment variable of the spec \
         nor a `gates` entry, or the operation is already gated by a different variable, or \
         a `gates` entry's `disabled_status` disagrees with the spec's gate. The spec wins.",
        "Declare the gate under `gates` (with `text` and `disabled_status`) or use the \
         spec's variable name.",
    ),
    e(
        "TG0609",
        "A macro could not be compiled as written: its name is also an operation id (the \
         macro is dropped), or it declares a safety tier weaker than its strictest step (the \
         macro gets the step's tier, because a macro can never be safer than what it calls).",
        "Rename the macro, or declare a tier at least as strict as every `call` step.",
    ),
    e(
        "TG0610",
        "The spec uses an `x-agent-*` extension tungsten does not know, often a misspelling. \
         It is ignored.",
        "Fix the extension name (for example `x-agent-safety`), or rename it outside the \
         `x-agent-` prefix if it is meant for another tool.",
    ),
    e(
        "TG0611",
        "An `x-agent-*` extension of an operation has the wrong shape (for example \
         `x-agent-safety: dangerous` or an `x-agent-idempotency` object without `policy`). \
         The extension is ignored and the operation keeps its defaults or agent.yml rules.",
        "Give the extension the shape of the matching agent.yml tools key \
         (`specs/agent-manifest.schema.json`, `ToolConfig`).",
    ),
    e(
        "TG0612",
        "Two targets in tungsten.yml write to the same output directory, or one target's \
         `out` is inside another's. Each target records its files in its own \
         `.tungsten/manifest.json` and removes files it no longer generates, so targets \
         sharing a directory delete each other's output on every run.",
        "Give every target its own directory that is not inside another target's, for \
         example `typescript: { out: generated/typescript }` and `docs: { out: generated/docs }`.",
    ),
    e(
        "TG0613",
        "agent.yml sets an option the schema accepts but this version of tungsten does not \
         apply yet (`disclosure.prune.drop_fields`, `disclosure.prune.keep_examples`). Agent \
         tool schemas describe exactly the arguments the SDK takes, so nothing is hidden \
         and no examples are added; the rest of the manifest applies.",
        "Remove the option, or keep it for a later version that applies it. To make a tool \
         smaller now, shorten descriptions or raise `defaults.disclosure.schema_budget_tokens`.",
    ),
    e(
        "TG0701",
        "A generated file could not be written to the target's output directory, for example \
         because the directory is read-only or a path component is a file.",
        "Check the target's `out` path and its permissions, then run `tungsten generate` again.",
    ),
    e(
        "TG0702",
        "tungsten.yml configures a target (python, rust, mcp or mock) for which this version \
         of tungsten has no emitter yet. `tungsten generate` skips it and `check --ci` does \
         not compare its output.",
        "Nothing to fix. Remove the target from tungsten.yml to silence the notice, or \
         generate it once a tungsten release ships its emitter.",
    ),
    e(
        "TG0703",
        "The target's output directory is not empty and has no `.tungsten/manifest.json` (or \
         an unreadable one), so tungsten cannot tell its own files from yours. Writing would \
         mix generated files into a directory it does not own, so nothing was written.",
        "Point the target's `out` at a new or empty directory, or pass `--force` to write \
         into it anyway; files tungsten did not generate are then left alone.",
    ),
    e(
        "TG0704",
        "A generated file would be written outside the target's output directory: a \
         directory on its path is a symlink leading elsewhere, the file itself is a \
         symlink, or the path is inside the reserved `.tungsten/` directory. Nothing was \
         written.",
        "Remove the symlink from the output directory. If the path is under `.tungsten/`, \
         the emitter is at fault: report it.",
    ),
    e(
        "TG0710",
        "A macro in the IR does not fit the canonical form the TypeScript SDK compiles: a \
         step names an operation that is not callable, a reference names a later or unknown \
         step, or an `{expr}` is not `<ref> in [..]`, `<ref> == x` or `<ref> != x`. The SDK \
         is generated without that macro.",
        "Fix the macro in agent.yml so every step calls a callable operation and every \
         reference names `$input` or an earlier step's `as`.",
    ),
    e(
        "TG0711",
        "An option of the `typescript` target is not valid (for example a `package` that is \
         not an npm package name or a `version` that is not a semantic version), so its \
         default is used.",
        "Fix the option under `targets.typescript` in tungsten.yml.",
    ),
    e(
        "TG0712",
        "The API declares an OpenID Connect security scheme. The TypeScript SDK sends the \
         configured credential as a bearer token and does not run discovery or obtain \
         tokens itself.",
        "Obtain the token with your OpenID Connect client and pass it in `auth` under the \
         scheme's name.",
    ),
    e(
        "TG0713",
        "A tool of tools.json (its name, description and parameters, measured as characters \
         / 4 after repeated sub-schemas were moved to `$defs`) is larger than the per-tool \
         schema budget of agent.yml (`defaults.disclosure.schema_budget_tokens`, 600 by \
         default). Agents pay this cost every time the tool is listed.",
        "Shorten the operation's and its fields' descriptions (`disclosure.prune` for the \
         operation, the spec for fields), split the operation, or raise the budget.",
    ),
    e(
        "TG0720",
        "An option of the `mcp` target is not valid (a `package` that is not an npm package \
         name, a `version` that is not a semantic version, an `sdk` that is not \
         `<package>` or `<package>@<range>`, an empty path or range, a `sandbox` that is not \
         a boolean), so its default is used.",
        "Fix the option under `targets.mcp` in tungsten.yml.",
    ),
    e(
        "TG0721",
        "An MCP tool (its description and input schema, measured with the tungsten-tokens \
         estimate after repeated sub-schemas were moved to `$defs`) is larger than the \
         per-tool schema budget of agent.yml (`defaults.disclosure.schema_budget_tokens`, \
         600 by default). Agents pay this cost whenever the tool is listed or described.",
        "Shorten the operation's and its fields' descriptions (`disclosure.prune` for the \
         operation, the spec for fields), split the operation, or raise the budget.",
    ),
    e(
        "TG0722",
        "In progressive mode an MCP client first receives the meta tools (`search_tools`, \
         `describe_tool`, `invoke`, `preview`, `list_clusters`) and the instructions with the \
         cluster index. Together they exceed the 2,000 tokens of NFR-3, usually because of \
         many clusters or long cluster summaries.",
        "Shorten the cluster summaries in agent.yml (only their first sentence is used) or \
         merge clusters.",
    ),
    e(
        "TG0723",
        "A macro of the IR is not served by the MCP server because the generated TypeScript \
         SDK, which runs it, does not emit it (see TG0710 of the `typescript` target).",
        "Fix the macro in agent.yml so every step calls a callable operation and every \
         reference names `$input` or an earlier step's `as`.",
    ),
    e(
        "TG0724",
        "Two or more operations or macros give the same MCP tool name once their namespace, \
         resource path and method are reduced to `[a-z0-9_]` (`/a-b/c` and `/a/b-c` both \
         become `..._a_b_c_get`). Numbering them in IR order would hand a name to another \
         operation when one of them is added or removed, so each is named with the first \
         eight hex digits of the digest of its operation id or macro name instead \
         (`..._a_b_c_get_1f3e9a0c`): stable, but not descriptive.",
        "Give the operations distinct method names with `naming.operations` in tungsten.yml.",
    ),
    e(
        "TG0730",
        "A macro in the IR does not fit the canonical form the Python SDK compiles: a step \
         names an operation that is not callable, a reference names a later or unknown step, \
         an `{expr}` is not `<ref> in [..]`, `<ref> == x` or `<ref> != x`, or a field the \
         macro adds to its input is not a free Python keyword argument name (not an \
         identifier, a keyword, or an argument the extended operation already takes). The \
         SDK is generated without that macro.",
        "Fix the macro in agent.yml so every step calls a callable operation, every reference \
         names `$input` or an earlier step's `as`, and added input fields have identifier names.",
    ),
    e(
        "TG0731",
        "An option of the `python` target is not valid (for example a `package` that is not a \
         PEP 508 distribution name, a `module` that is not a lowercase identifier, a `version` \
         that is not a PEP 440 version, or `models: dataclasses`, which this version does not \
         emit), so its default is used.",
        "Fix the option under `targets.python` in tungsten.yml.",
    ),
    e(
        "TG0732",
        "The API declares an OpenID Connect security scheme. The Python SDK sends the \
         configured credential as a bearer token and does not run discovery or obtain tokens \
         itself.",
        "Obtain the token with your OpenID Connect client and pass it in `auth` under the \
         scheme's name.",
    ),
    e(
        "TG0733",
        "A record schema without a name (inline, not a component) has fixed fields. The Python \
         SDK declares a model class per named record only, so this one is typed \
         `dict[str, Any]` and its fields are not validated client-side.",
        "Move the schema to `components/schemas` (or name it with an overlay) so it gets a \
         model.",
    ),
    e(
        "TG0740",
        "An option of the `rust` target is not valid (for example a `crate` that is not a \
         crate name, a `version` that is not semver, or a `cli` that is neither a boolean nor \
         an object), so its default is used.",
        "Fix the option under `targets.rust` in tungsten.yml.",
    ),
    e(
        "TG0741",
        "A schema has no Rust type of its own: an enum whose values are not all strings or all \
         integers, a union without variants, an `allOf` that could not be merged into one \
         record, a schema no value satisfies, or an inline enum, union or record without a \
         name. The Rust SDK types it `serde_json::Value` and checks its constraints (enum \
         membership, `allOf` members) when the request or response is validated.",
        "Move the schema to `components/schemas` (or name it with an overlay) so it gets a \
         Rust type; make an enum's values all strings or all integers.",
    ),
    e(
        "TG0742",
        "A macro in the IR does not fit the canonical form the Rust SDK compiles: a step names \
         an operation that is not callable, a reference names a later or unknown step, an \
         `{expr}` is not `<ref> in [..]`, `<ref> == x` or `<ref> != x`, or the input extends \
         an operation that is not callable. The SDK is generated without that macro.",
        "Fix the macro in agent.yml so every step calls a callable operation and every \
         reference names `$input` or an earlier step's `as`.",
    ),
    e(
        "TG0743",
        "The API declares an OpenID Connect security scheme. The Rust SDK sends the \
         configured credential as a bearer token and does not run discovery or obtain tokens \
         itself.",
        "Obtain the token with your OpenID Connect client and pass it in `auth` under the \
         scheme's name.",
    ),
    e(
        "TG0744",
        "An operation's success responses have different bodies (for example 200 returns a \
         record and 201 an array). The Rust SDK cannot give the typed method one return \
         type, so it returns `serde_json::Value` and does not validate the response.",
        "Describe one success body, or call the operation through `Dispatch::invoke`, which \
         returns JSON.",
    ),
    e(
        "TG0745",
        "A union has a discriminator, but not every variant is a named record the tag can be \
         read from. The Rust SDK cannot select a variant by its tag, so it tries the variants \
         in order like an untagged union.",
        "Give every variant a named object schema that carries the discriminator property.",
    ),
    e(
        "TG0901",
        "A target's output directory differs from what `tungsten generate` would write now: \
         a generated file is missing or has other content, a file of the previous \
         generation would be removed, or `.tungsten/manifest.json` is missing. Usually the \
         spec or a manifest changed after the last generation, or a generated file was \
         edited by hand.",
        "Run `tungsten generate` and commit the result. Put hand-written code in the \
         target's `custom/` files, which generation never overwrites.",
    ),
    e(
        "TG0902",
        "`tungsten diff --semver` and `tungsten report` compare the API surface recorded by \
         the last generation (`.tungsten/surface.json` in the target's output directory) with \
         the current one. The target has no snapshot (it was never generated, or generated \
         by a tungsten that did not record one), or the snapshot cannot be read or parsed, \
         so the change since that generation is not classified as major, minor or patch.",
        "Run `tungsten generate` to record a snapshot; later runs of `tungsten diff \
         --semver` classify the changes made after it. Do not edit files under `.tungsten/`.",
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
