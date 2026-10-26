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
        "GO001",
        "A macro of agent.yml is not in the canonical form the SDKs run (an unknown step kind \
         or operation, a reference to no earlier step, an invalid `as`, `until` or budget). The \
         Go SDK leaves it out; the other operations are generated as usual. The Go emitter \
         reports it through TG0806.",
        "Fix the macro in agent.yml; the message names the offending member.",
    ),
    e(
        "GO002",
        "The API declares an OpenID Connect security scheme. The Go SDK sends the configured \
         credential as a bearer token and does not run discovery or obtain tokens itself.",
        "Obtain the token yourself and pass it to the client under the scheme's name.",
    ),
    e(
        "GO003",
        "An operation answers different JSON bodies depending on the status. The Go SDK's \
         typed result for it is `json.RawMessage`; the decoded body also stays in `Response.Raw`.",
        "Nothing to fix; decode the raw message according to the status that arrived.",
    ),
    e(
        "GO004",
        "An option of the `go` target in tungsten.yml is unknown or has the wrong type, \
         so the Go emitter refused the target and wrote nothing.",
        "Fix or remove the option under `targets.go` in tungsten.yml; the message names it \
         and the type it must have.",
    ),
    e(
        "GO005",
        "The Go source the emitter generated does not parse, so nothing was written. This is a \
         fault in the emitter or in a name taken from the API description.",
        "Check the API description for names that cannot become Go identifiers and report \
         the diagnostic with the offending schema if it persists.",
    ),
    e(
        "GO006",
        "The pagination items path of an operation (from the spec or inferred) does not \
         resolve to an array in the success schema, so the Go SDK cannot type the page items.",
        "Correct the pagination items path of the operation to name an array field of the \
         success response, or remove the pagination declaration.",
    ),
    e(
        "HR001",
        "The HAR capture is larger than the import limits: its byte size, nesting depth or \
         node count is over the budget. Nothing was imported.",
        "Export a smaller capture, or narrow it with `--include-prefix` and `--exclude-prefix`.",
    ),
    e(
        "HR002",
        "The input is not a HAR 1.2 capture tungsten can read: invalid JSON, a missing `log`, \
         entries, request, method, header name or query parameter name, an unsupported HAR \
         version, or a response without a completed HTTP status.",
        "Export the capture again as HAR 1.2 and exclude failed or pending requests.",
    ),
    e(
        "HR003",
        "The origin given with `--har-origin` is not one of the origins in the capture, or is \
         not written in canonical form (scheme, host and port only).",
        "Pass exactly one HTTP(S) origin that occurs in the capture, for example \
         `https://api.example.com`.",
    ),
    e(
        "HR006",
        "Reviewed route templates (`--route` or `--har-paths`) are ambiguous or malformed: a \
         captured path matches several equally specific templates, or a template replaces \
         part of a segment instead of a whole parameter segment.",
        "Make the templates unambiguous and let each replace whole segments only, such as \
         `/items/{id}`; keep literal routing segments unchanged.",
    ),
    e(
        "PM001",
        "The Postman collection is over an import limit: variable expansion and parsed samples \
         exceed the import-wide byte or node budget, or folders nest deeper than the limit.",
        "Reduce large or repeated variables and examples, or flatten the folder nesting.",
    ),
    e(
        "PM002",
        "The input is not a Postman Collection v2.1 item tree: the `item` array is missing or \
         not an array.",
        "Export the collection as Postman Collection v2.1 JSON and import that file.",
    ),
    e(
        "PM003",
        "A request uses API key authentication without an explicit key name, or with a \
         location other than header or query.",
        "Give the API key an explicit name and set its location to header or query in the \
         collection.",
    ),
    e(
        "PM004",
        "A request uses a Postman dynamic variable (`{{$...}}`). tungsten never runs scripts \
         or generates dynamic values, so the request shape cannot be known.",
        "Replace the dynamic variable with an explicit example value or a declared parameter.",
    ),
    e(
        "PM005",
        "A request URL is not HTTP(S) with an explicit path, or a variable that decides the \
         request shape (host, path or query) cannot be resolved from the collection.",
        "Provide the variable in the collection or an environment override, or replace it with \
         a declared path parameter; use an absolute HTTP(S) URL.",
    ),
    e(
        "PM006",
        "A raw request body is not valid JSON after variable resolution, or a saved response \
         body is not JSON, so no schema can be inferred from it.",
        "Provide explicit JSON examples, remove the unsupported example, or supply a reviewed \
         media schema.",
    ),
    e(
        "PM007",
        "Several requests describe the same operation, and some declare authentication while \
         others leave it unknown.",
        "Declare one consistent authentication policy for the duplicated requests.",
    ),
    e(
        "PM008",
        "Every request in the Postman collection was omitted as unsupported, so nothing could \
         be imported.",
        "Remove or fix the unsupported constructs, or supply a reviewed contract.",
    ),
    e(
        "TC001",
        "The structural request evidence is larger than the import byte limit.",
        "Split the import into smaller captures or collections.",
    ),
    e(
        "TC002",
        "The structural request evidence is not valid JSON, or is nested too deeply.",
        "Fix the JSON syntax and nesting of the evidence file.",
    ),
    e(
        "TC003",
        "The evidence model or the reviewed overrides do not match the evidence schema.",
        "Regenerate the evidence with tungsten, or correct the overrides to match the \
         documented shape.",
    ),
    e(
        "TC004",
        "An operation in the evidence cannot become an OpenAPI operation: its HTTP method is \
         not supported, or its path template repeats a placeholder name.",
        "Use an OpenAPI HTTP method and make the path placeholder names unique.",
    ),
    e(
        "TC005",
        "An evidence source id is not an opaque identifier; it looks like a file name or a URL.",
        "Use an opaque alphanumeric source id, never a file name or URL.",
    ),
    e(
        "TC006",
        "A structural schema in the evidence uses a keyword the import does not support. \
         References and value-bearing annotations are not imported.",
        "Use structural fields only, or keep the declaration in the original OpenAPI document.",
    ),
    e(
        "TC007",
        "A reviewed override points into a body schema or parameter with a numeric parameter \
         index, which changes when parameters are added or reordered.",
        "Use `/paths/` body schema pointers or stable `parameter_schema_pointer` keys.",
    ),
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
         compile. tungsten reads OpenAPI 3.0.x and 3.1.x and a documented subset of 3.2.x; \
         Swagger 2.0 documents declare `swagger: \"2.0\"` instead.",
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
        "TG0110",
        "The document has no `info.version`. OpenAPI requires it, but published specs \
         sometimes omit it. tungsten records version 0.0.0 and continues; the diagnostic is a \
         warning, and the document is still refused when something else required is missing.",
        "Add `info.version` to the document, or supply it with an overlay.",
    ),
    e(
        "TG0111",
        "A key of `paths` carries a query string, such as `/v1/messages?beta=true`. OpenAPI \
         forbids it, but some APIs use it to select a variant of an operation. tungsten \
         names the operation and places it in resources from the path alone, keeps the \
         query pairs as constant query parameters of the path template (`path.query` in the \
         IR), sends them on every call (they are not arguments), and the mock routes a \
         request only to the operation whose constants it carries.",
        "No change is needed. To make the document valid OpenAPI, declare the pair as a \
         query parameter with a `const` or `enum` schema.",
    ),
    e(
        "TG0112",
        "The document defines no security scheme and no `security`, but declares a header \
         that carries an API key (`x-api-key`, `api-key` or `apikey`) as a parameter. \
         tungsten inferred an `apiKey` header scheme named `apiKey` and required it by every \
         operation, so the credential is configured once on the client and the header is no \
         longer an argument of each call.",
        "Define the scheme under `components/securitySchemes` and list it in `security` (an \
         overlay can add both) to make the intent explicit and silence the diagnostic.",
    ),
    e(
        "TG0120",
        "A remote input URL contains credentials, a query or fragment, or its acquisition policy is invalid. Authentication belongs in headers, never source identifiers.",
        "Supply an HTTP(S) document URL and --input-header or newline-separated TUNGSTEN_INPUT_HEADERS. Choose either --offline or --refresh-inputs.",
    ),
    e(
        "TG0121",
        "Fetching an input failed, returned a non-success HTTP status, exceeded 8 MiB or did not finish within 30 seconds. Redirects are refused so credentials cannot cross origins.",
        "Check network access, trusted TLS certificates and authentication. Pass the final document URL explicitly. Use --offline to reuse a verified pin.",
    ),
    e(
        "TG0122",
        "Downloaded or cached bytes differ from their SHA256 pin, or a converted import no longer matches its original remote source.",
        "Review upstream changes before --refresh-inputs. Converted inputs are refreshed with `tungsten refresh-import`, which previews the conversion and requires renewed review of changed approvals. Use --offline for the existing pin; restore corrupted cache bytes from a trusted copy.",
    ),
    e(
        "TG0123",
        "The .tungsten-inputs cache has no usable pin, contains a symlink or invalid file, or cannot be written safely.",
        "Fetch once online and retain both source.lock.json and its content file for offline use. Inspect permissions and unexpected cache entries before retrying. Partial *.pending writes from an interrupted fetch are discarded by the next online run; a held acquire.lock means another run is fetching the same source.",
    ),
    e(
        "TG0124",
        "`tungsten refresh-import --apply` was interrupted: its journal under .tungsten/refresh-import/ remains, so the converted input, import report, source_sha256, pin and lock may be partly refreshed. `check` and `generate` refuse the project until it is recovered.",
        "Run `tungsten refresh-import --resume` to finish the reviewed refresh from its staged content, or `tungsten refresh-import --rollback` to restore the backed-up files and refresh again.",
    ),
    e(
        "TG0128",
        "A locked run (`generate --locked` or `check --locked`) found that the project does not match its generation lock: tungsten.lock.json is missing or corrupt, or a recorded source, reference, overlay, profile, target option, compiler or emitter identity changed. Nothing is generated.",
        "Review the change and accept it explicitly with `tungsten lock --update`; `tungsten lock --check` lists every difference. Locked runs never fetch or refresh inputs.",
    ),
    e(
        "TG0129",
        "A locked run (`generate --locked`) over a lock that records generated outputs found that a built-in target no longer generates the recorded bytes, although every locked input, option, compiler and emitter identity matches. This usually means a platform-dependent difference. Nothing is generated.",
        "Compare the platform and toolchain with the environment that recorded the lock. After reviewing the new output, record it explicitly with `tungsten lock --outputs --update`.",
    ),
    e(
        "TG0130",
        "The Git input reference has an unsupported transport, URL credentials, query, invalid ref or unsafe subpath. Git authentication stays in the credential helper or environment.",
        "Use git+https://host/owner/repository@ref#path. Local hermetic inputs may use git+file:///absolute/repository@ref#path. Do not put secrets in the reference.",
    ),
    e(
        "TG0131",
        "Git could not start, acquire the repository or resolve the ref to one commit. Raw Git stderr is discarded so credential helpers and servers cannot leak credentials into diagnostics.",
        "Install Git, verify access and credentials, and use a full refs/heads/... or refs/tags/... name when branch/tag names conflict. Use --offline with a previously verified pin.",
    ),
    e(
        "TG0132",
        "Git acquisition exceeded its time, file, byte or process-output budget, or encountered a symlink, submodule or unsafe repository path. Only regular files are materialized and nothing is checked out or executed.",
        "Narrow #subpath to the contract directory, select a smaller repository, or acquire a single document over HTTP(S).",
    ),
    e(
        "TG0133",
        "The Git source lock or its materialized files are missing, invalid, modified, symlinked or not writable. Every reused content file is SHA256-verified.",
        "Restore the pinned cache from a trusted copy, or refresh a converted input with `tungsten refresh-import`. Retain .tungsten-inputs with the same access controls as the original source for offline use. Partial *.pending entries from an interrupted fetch are discarded by the next online run; a held acquire.lock means another run is fetching the same source.",
    ),
    e(
        "TG0134",
        "Discovery found zero or multiple supported documents under the selected repository subpath. Tungsten lists candidates and does not choose one implicitly.",
        "Append #path to select exactly one OpenAPI, Swagger, Postman, HAR, saved request or JSON Schema endpoint manifest document.",
    ),
    e(
        "TG0140",
        "An OpenAPI 3.2 document declares a `$self` that tungsten cannot honour. `$self` is the document's own URI and the base for its relative references; tungsten resolves references against the file location, so it accepts `$self` only when that gives the same result: the document has no relative reference to another document, or `$self` is a bare file name in the document's directory. This code reports a `$self` that would move the base of a relative `$ref`, one that is not a string or has a fragment, and a `$ref` that names the document by its absolute `$self` URI.",
        "Remove `$self`, or keep references that resolve the same against the file location.",
    ),
    e(
        "TG0141",
        "Retired. Earlier releases refused the OpenAPI 3.2 `query` operation (the HTTP QUERY method). tungsten now compiles it as a safe operation that sends its request body, and no longer reports this code.",
        "Nothing to do; regenerate to pick up QUERY operations.",
    ),
    e(
        "TG0142",
        "An OpenAPI 3.2 `additionalOperations` entry cannot be sent: `additionalOperations` is not a map, or its key is not an RFC 9110 method token, is a case variant of a method the Path Item has a fixed field for (`get` to `query`), or is `CONNECT` or `TRACK`. Other entries compile to operations sent with their method token as written.",
        "Use the method token the server expects, or describe the call under the matching fixed field such as `post`.",
    ),
    e(
        "TG0143",
        "An OpenAPI 3.2 `in: querystring` parameter (the whole query string as one value described by `content`) cannot be compiled. tungsten compiles one whose `content` is a single `application/x-www-form-urlencoded` media type with an object schema, sent as one `key=value` pair per property, when it is the operation's only query parameter. Other media types, a schema that is not an object, or `query` parameters beside it are refused, and the parameter is left out.",
        "Describe the query string as one form-encoded object, or as separate `in: query` parameters.",
    ),
    e(
        "TG0144",
        "A parameter that is not `in: cookie` uses `style: cookie`, the OpenAPI 3.2 style that follows Cookie header syntax. tungsten compiles `style: cookie` on cookie parameters (values as written, exploded arrays and objects as `n=v; n2=v2` pairs) but it has no meaning in another location.",
        "Use `style: cookie` only on `in: cookie` parameters, or the default style of the parameter's location.",
    ),
    e(
        "TG0145",
        "A Media Type Object uses OpenAPI 3.2 `itemSchema`, which types each item of a sequential media type, in a way tungsten does not compile. tungsten compiles `itemSchema` only for responses, without `schema` beside it, for JSON Lines (`application/jsonl`, `application/x-jsonl`, `application/ndjson`, `application/x-ndjson`; the type of one line) and for `text/event-stream` when the item is an event object whose `data` is a string with a JSON `contentMediaType` and a `contentSchema` (the event type), or a plain string without `contentMediaType` or with a `text/*` one (string events delivered as written). Request bodies, parameters, headers, other media types and event `data` of any other kind would be typed without their items.",
        "Describe the whole payload with `schema`, or reshape the item into one of the compiled forms.",
    ),
    e(
        "TG0146",
        "A Media Type Object uses OpenAPI 3.2 `prefixEncoding` or `itemEncoding`, which encode multipart parts by position. tungsten encodes parts by name only.",
        "Name the parts and describe them with `encoding`.",
    ),
    e(
        "TG0147",
        "An Encoding Object uses OpenAPI 3.2 `encoding`, `prefixEncoding` or `itemEncoding` to encode a nested multipart or form value. tungsten does not model nested encodings.",
        "Describe the part as one value with a `contentType`.",
    ),
    e(
        "TG0148",
        "Retired. Earlier releases refused OpenAPI 3.2 `components.mediaTypes` and media type objects given by `$ref`. tungsten now follows the reference and reads the media type as if written in place, and no longer reports this code.",
        "Nothing to do; regenerate to pick up the referenced media types.",
    ),
    e(
        "TG0149",
        "Retired. Earlier releases refused the OpenAPI 3.2 discriminator `defaultMapping`. tungsten now decodes a value whose tag is missing or maps to no variant as the default variant (TypeScript, Python and Rust SDKs and the mock), and no longer reports this code. A `defaultMapping` that is not a variant of the union is reported as TG0306 and ignored.",
        "Nothing to do; regenerate to pick up the default variant.",
    ),
    e(
        "TG0150",
        "Retired. Earlier releases refused a security scheme with `flows.deviceAuthorization`, the OpenAPI 3.2 device flow. tungsten now keeps the flow and its `deviceAuthorizationUrl` in the IR, like the `password` and `implicit` flows: generated clients accept an access token obtained through it as the scheme's credential, but do not run the device flow themselves. This code is no longer reported.",
        "Nothing to do; configure the scheme with an access token obtained through the device flow.",
    ),
    e(
        "TG0151",
        "An OpenAPI 3.2 security requirement names a scheme by URI reference, and the URI does not resolve to an entry under `components.securitySchemes` of an input document. tungsten resolves a URI such as `#/components/securitySchemes/apiKey` (or one into another input document) to that scheme, but not a scheme in a file that is not an input, nor a URI to anything else.",
        "Reference a scheme under `components.securitySchemes` of an input document, or name it by its component name.",
    ),
    e(
        "TG0152",
        "An OAuth 2.0 security scheme declares the OpenAPI 3.2 `oauth2MetadataUrl`. tungsten does not fetch server metadata; the declared flows are used as written.",
        "Nothing to fix; remove the field to silence the warning.",
    ),
    e(
        "TG0153",
        "A tag uses the OpenAPI 3.2 `parent` or `kind` field. tungsten groups operations by tag name and ignores the hierarchy.",
        "Nothing to fix; remove the field to silence the warning.",
    ),
    e(
        "TG0154",
        "An Example Object uses OpenAPI 3.2 `dataValue` or `serializedValue`. tungsten reads `value` and `externalValue` only.",
        "Add `value` when the example should be used, or remove the field.",
    ),
    e(
        "TG0155",
        "An XML Object uses the OpenAPI 3.2 `nodeType`. tungsten does not use XML Object details.",
        "Nothing to fix; remove the field to silence the warning.",
    ),
    e(
        "TG0156",
        "A document uses an OpenAPI 3.2 descriptive field without effect on a client: tag `summary`, response `summary`, server `name` or security scheme `deprecated`.",
        "Nothing to fix; remove the field to silence the warning.",
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
         candidates overlap. A union of exactly two candidates where one is a plain string, \
         array or map (`X` or its id, `X` or a list of `X`) is reported as info instead of a \
         warning: the two are told apart by JSON kind, so `--strict` does not fail on it.",
        "Add a `discriminator` with a property every variant declares as a `const` or single \
         enum value. `tungsten check` prints a repeated code a few times and then a count; \
         `--all-warnings` prints all of them and `--json` always lists every one.",
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
        "TG0510",
        "A Path Item combines `$ref` with fields written beside it, and the referenced item \
         defines the same field: an operation, `summary`, `description`, `servers`, or a \
         parameter with the same `name` and `in`. Fields only one side defines are merged. \
         OpenAPI leaves the conflicting ones undefined; tungsten lets the field beside the \
         `$ref` win, and the message names both pointers.",
        "Define the field on one side only, or drop it from the shared item.",
    ),
    e(
        "TG0530",
        "A success response declares the `text/event-stream` media type without a schema, so \
         the SDK cannot type or check the events of the stream. Each event's `data` is still \
         decoded as JSON and handed to the caller as an untyped value.",
        "Declare the schema of one event under the media type. Use a discriminator only if \
         it exists inside the JSON data; the SSE event name is separate. A raw data sentinel \
         can be declared with `x-tungsten-stream-done`. See the typed Python SSE recipe at \
         https://github.com/pboachie/tungsten/tree/release/0.1.3/examples/typed-sse.",
    ),
    e(
        "TG0531",
        "More than one success status of an operation declares `text/event-stream`. The SDK \
         streams the lowest status; the others are read as plain responses.",
        "Declare the stream under one status, normally 200.",
    ),
    e(
        "TG0570",
        "`targets.mcp.search.embeddings` in tungsten.yml is not valid: an unknown key, a \
         `provider` that is not `command` or `http`, a missing `command`, `url` or `model`, a \
         `dimensions`, `batch_size` or `timeout_ms` out of range, or an `api_key_env` that is \
         not an environment variable name. The embedding index is off and the MCP server \
         searches with BM25 only; with `required: true` this is an error instead.",
        "Fix the option: `search: { embeddings: { provider: command, command: \"node embed.mjs\", \
         model: my-model } }` or `provider: http` with `url`. See the MCP server guide \
         for embedding provider examples.",
    ),
    e(
        "TG0571",
        "The embedding provider of the MCP target could not be used: the command was not \
         found, could not be started, exited with a failure, or neither it nor the HTTP \
         endpoint answered within `timeout_ms`. Generation went on without \
         `index.embeddings.bin` and `index.embeddings.json`, so the server searches with \
         BM25 only. With `required: true` this is an error and nothing is written.",
        "Run the command by hand with a request on standard input (`{\"protocol\":1,\"kind\":\
         \"document\",\"model\":\"m\",\"dimensions\":null,\"inputs\":[\"x\"]}`), raise \
         `timeout_ms`, or lower `batch_size`.",
    ),
    e(
        "TG0572",
        "The embedding provider answered, but not with what the protocol says: not JSON, no \
         `embeddings` array (command) or `data` array (HTTP), a different number of vectors \
         than inputs, vectors of different or unexpected lengths, or values that are not \
         finite numbers. Generation went on with BM25 only (an error with `required: true`).",
        "Return one vector per input, in order, all of the length the model produces; set \
         `dimensions` to that length so a mismatch is caught.",
    ),
    e(
        "TG0573",
        "The HTTP embedding provider could not be called: the endpoint answered with an error \
         status, the environment variable named by `api_key_env` is not set, or the URL is \
         `https://`, which the compiler cannot call (it links no TLS stack). Generation went \
         on with BM25 only (an error with `required: true`). The key is only ever read from \
         the environment and never written to the output.",
        "Export the API key, check the URL and model, or use `provider: command` with a \
         program that calls the endpoint (for example `curl`) or a local http:// gateway.",
    ),
    e(
        "TG0574",
        "The MCP target wrote the embedding index: `index.embeddings.json` (model, \
         dimensions, tool names and content hashes, checksum) and `index.embeddings.bin` \
         (normalized little-endian f32 vectors). Vectors of tools whose text did not change \
         since the previous run are reused, so only new or edited tools reach the provider.",
        "Nothing to fix. Serve the index with `search.embeddings` in the server options \
         (the generated server.ts sets it from tungsten.yml).",
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
        "TG0614",
        "A `gates` entry of agent.yml is used by no operation: the spec carries no \
         `x-runtime-gate` for that environment variable and no `tools` entry names it in \
         `gate`. The entry's text is never shown and a 404 from the routes it was written for \
         is reported as NOT_FOUND (\"check the identifiers\") instead of GATE_DISABLED. \
         Live runs against ZROtext showed it: SEALED_ADMISSION_ENABLED was declared but none \
         of the sealed operations named it.",
        "Add `gate: <ENV_VAR>` to the `tools` entry of every operation the deployment setting \
         switches off (one entry per operation), or delete the `gates` entry.",
    ),
    e(
        "TG0615",
        "An ordinary OpenAPI operation's `x-agent-safety` is less cautious than its \
         built-in HTTP-method tier. The compiler uses the method tier and reports a \
         warning, which fails `check` and `generate` under `--strict`.",
        "Review the operation's behavior. Remove or raise the spec annotation, or \
         explicitly set `safety` in that operation's `agent.yml` tools entry to authorize \
         a lower tier. Imported operations retain their separate reviewed-policy floor.",
    ),
    e(
        "TG0616",
        "An operation with side effects sets `x-agent-preview: none` in the spec. \
         The compiler keeps a configured safe preview or uses local preview instead. \
         This warning fails `check` and `generate` under `--strict`.",
        "Keep a preview, or explicitly set `preview: none` in that operation's \
         `agent.yml` tools entry after reviewing its behavior. A safety override \
         that still has side effects does not also approve removing preview.",
    ),
    e(
        "TG0640",
        "Workspace discovery (`tungsten workspace`, `--project`, the language server's \
         workspace folders) is bounded: it reads at most 4,096 directories and loads at \
         most 64 projects and manifests of at most 1 MiB, searching 8 levels deep. A tree \
         that exceeds a bound stops discovery with this error rather than return a partial \
         list, so a project is never silently missed.",
        "Run the command in the project's own directory, or move large unrelated trees \
         (dependencies, fixtures, build output) out of the workspace. Hidden directories, \
         `node_modules`, `target`, `vendor` and the output directories of discovered \
         projects are never searched.",
    ),
    e(
        "TG0641",
        "One directory holds both `tungsten.yml` and `tungsten.yaml`. A project is owned by \
         exactly one manifest, so discovery loads neither instead of guessing.",
        "Keep one manifest per directory; move the other project into its own directory.",
    ),
    e(
        "TG0642",
        "Two projects of one workspace write to the same output directory, or one \
         project's target directory is inside another project's. Each target removes \
         files it no longer generates from its own directory, so the projects would \
         delete each other's output.",
        "Give every target of every project its own directory that is not inside \
         another's, for example `out: generated/billing-ts` and `out: generated/identity-ts`.",
    ),
    e(
        "TG0643",
        "A `tungsten.yml` found by workspace discovery is a symbolic link. Discovery never \
         follows links, so a project cannot be reached twice or from outside the \
         workspace; the linked manifest is not a project of this workspace.",
        "Replace the link with the manifest, or run the command in the directory the link \
         points to.",
    ),
    e(
        "TG0644",
        "`--project` named a project that workspace discovery did not find. A project id is \
         its directory relative to the workspace root, with `/` separators (`.` for the \
         root); its manifest path is accepted too.",
        "Run `tungsten workspace` in the same directory to list the project ids.",
    ),
    e(
        "TG0701",
        "A generated file could not be written to the target's output directory, for example \
         because the directory is read-only or a path component is a file.",
        "Check the target's `out` path and its permissions, then run `tungsten generate` again.",
    ),
    e(
        "TG0702",
        "tungsten.yml configures a target (such as mock) for which this version \
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
        "TG0705",
        "The project was imported from a Postman collection or another source whose operations \
         carry an enforced import policy (the runtime must apply it to every call), and a \
         configured target cannot apply it. Only the typescript, python, rust, java, kotlin, ruby, \
         csharp, fsharp, dart, mcp, mcp-python, webmcp and docs targets declare that support, and an \
         external emitter declares it with `import_policy` in its `--describe` response (the Go \
         emitter in `emitters/go` does); generating any other target would \
         discard the policy. During input conversion, init refuses unsupported built-in \
         targets before saving the converted input or project manifests. During generation \
         of an existing project, tungsten refuses that target alone; other targets are \
         still written, unless `--strict` asks for all or none.",
        "Generate only supported targets: remove the unsupported target from tungsten.yml, or \
         add a supported one with `tungsten init --resume --add-targets typescript,python` \
         (any built-in SDK target, mcp, mcp-python, webmcp or docs), then run `tungsten generate` \
         again.",
    ),
    e(
        "TG0706",
        "The compiled IR binds a namespace to a protocol other than HTTP (its `protocol` \
         field), and a configured target does not declare that protocol in the capability \
         catalog. Operations of another protocol are never reshaped into REST methods and \
         paths, so the target is refused before anything is written for it. External \
         emitters cannot declare protocols in this version and are refused too. Other \
         targets are still processed, unless `--strict` asks for all or none.",
        "Remove the target from tungsten.yml, or select a target that declares the protocol: \
         `tungsten capabilities --json` lists each protocol's targets under `protocols`. A \
         reserved protocol has no generating target yet.",
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
         not an npm package name or a `version` that is not a semantic version). Invalid \
         scalar options use their defaults. Invalid `dependencies` entries fail generation: \
         package names must be valid npm names and version specifiers must be non-empty \
         strings. Emitter-managed runtime, zod and typescript packages cannot be replaced.",
        "Fix the option under `targets.typescript` in tungsten.yml. Configure the runtime \
         with `runtime` or `runtime_path`; remove zod and typescript from `dependencies`.",
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
         cluster index. Together they exceed the 2,000-token budget of the progressive index, usually because of \
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
        "TG0725",
        "`defaults.disclosure.mode: auto` in agent.yml chooses between the discrete MCP tool \
         list (every tool with its schema) and progressive disclosure (search_tools, \
         describe_tool, invoke) by measuring the discrete `tools/list` plus its instructions \
         with the tungsten token estimate. Progressive is chosen when that is above \
         `defaults.disclosure.list_budget_tokens` (default 10000), whatever the number of \
         tools. This note states the measured tokens, the budget and the decision.",
        "Set `defaults.disclosure.mode` to `discrete` or `progressive` to decide yourself, or \
         change `defaults.disclosure.list_budget_tokens`.",
    ),
    e(
        "TG0726",
        "The `mcp` target has no `sdk` option, so the server depends on the package the \
         `typescript` target of the same tungsten.yml generates (its `package` and `version`) \
         instead of the default `<api>-sdk`.",
        "Set `targets.mcp.sdk` (`<package>` or `<package>@<range>`) or `targets.mcp.sdk_path` to \
         depend on something else.",
    ),
    e(
        "TG0727",
        "The output schema of an MCP tool is larger than four times the per-tool schema budget \
         even though every reference to another type was already replaced by `{}` (the \
         emitter keeps only the types one reference away from the response body, and none \
         when that is still over the limit). The response type itself is large: many fields \
         or long descriptions.",
        "Shorten the descriptions in the spec, or raise \
         `defaults.disclosure.schema_budget_tokens` in agent.yml.",
    ),
    e(
        "TG0728",
        "An option of the experimental `mcp-python` target is not valid (a `package` or `sdk` \
         that is not a distribution name, a `module` or `sdk_module` that is not a lowercase \
         module name, a `version` or `sdk_version` that is not a version, an empty path or \
         specifier), so its default is used.",
        "Fix the option under `targets.mcp-python` in tungsten.yml.",
    ),
    e(
        "TG0729",
        "The experimental `mcp-python` server (the `tungsten-mcp` Python runtime) runs macros \
         through the generated Python SDK, and that SDK does not emit some macros of this API \
         (see the python target's TG0730). Their tools answer a misconfiguration envelope and \
         send nothing; nothing is weakened silently.",
        "Fix the macros the python target reports with TG0730, or use the `mcp` target (the \
         TypeScript server) for them.",
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
         that is not a PEP 440 version, or a `models` that is neither `pydantic` nor \
         `dataclasses`), so its default is used.",
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
        "A schema has no Rust type of its own: a union without variants, an `allOf` that could \
         not be merged into one record, a schema no value satisfies, or an inline enum, union \
         or record without a name. The Rust SDK types it `serde_json::Value` (the generated \
         type says so in its documentation) and checks its constraints (enum membership, \
         `allOf` members) when the request or response is validated. Enums of booleans, of \
         numbers with fractions and of integers above `i64` are not in this group: they are \
         untagged enums with one variant per JSON type, and enums of several types are \
         unions of constants.",
        "Move the schema to `components/schemas` (or name it with an overlay) so it gets a \
         Rust type; for an `allOf`, make the members compatible so that they merge.",
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
        "An operation's success responses have different bodies, and they are not all JSON \
         bodies under exact statuses (a body is text, binary or a form, or a status is a \
         range such as `2XX`). The Rust SDK types an operation whose exact statuses answer \
         with different JSON bodies as a response enum; for the shapes it cannot represent \
         it returns `serde_json::Value` and does not validate the response.",
        "Declare each success status exactly and describe its body as JSON, or call the \
         operation through `Dispatch::invoke`, which returns JSON.",
    ),
    e(
        "TG0745",
        "A union has a discriminator, but a variant has no tag value: the mapping does not name \
         it and it is not a component with a name or a constant to take the tag from. A union \
         whose variants all have tags is decoded by the tag, whatever the variants are (records, \
         other unions, maps), like the TypeScript and Python SDKs; this one cannot be, so the \
         Rust SDK tries its variants in order like an untagged union.",
        "Name every variant under the discriminator's `mapping`, or give every variant a \
         component of its own.",
    ),
    e(
        "TG0746",
        "The output of a macro is not an object or a reference whose members the Rust SDK can \
         type: a member is a `null`, a list, a nested object or a reference into a step the \
         API description gives no single JSON response type for. The macro's method returns \
         `serde_json::Value`, the output expression evaluated by the runtime.",
        "Read the members you need from the value, or flatten the output expression to \
         references into steps with a documented response (`$step.field`) and boolean \
         expressions.",
    ),
    e(
        "TG0750",
        "An argument of an operation has no plain flag form in the generated CLI: a parameter \
         whose type is an object, a map, a union or a list of those, or a multipart body whose \
         parts are files. An object, map or union takes JSON text (`--filter '{\"a\":1}'`, \
         `--filter @filter.json` or `-` for standard input) or its members one at a time \
         (`--filter.a 1`, `--filter.created.after 5`; a value is typed by the operation's \
         schema, and both forms can be mixed, the members applied on top of the JSON text). A \
         list of those repeats the flag, one JSON value each. A multipart file cannot be given \
         from the command line at all.",
        "Use the JSON or dotted form, or the SDK for multipart uploads. Nested fields of a \
         request body are JSON-text flags by design and are not reported.",
    ),
    e(
        "TG0751",
        "A resource, or a namespace, has the name of a command the generated CLI provides itself \
         (`auth`, `help`, `operations`, `schema`). Its commands are under `<name>-resource` so \
         that both exist.",
        "Rename the resource under `resources` in tungsten.yml.",
    ),
    e(
        "TG0752",
        "The flag derived from an argument has the name of a flag the CLI adds to every command \
         (`--json`, `--body`, `--yes`, `--dry-run`, `--idempotency-key`, `--timeout`, ...) or of \
         another flag of the same command. The argument keeps its name in the SDK; its flag gets \
         the suffix `-arg`.",
        "Use the renamed flag, or rename the parameter in the spec or an overlay.",
    ),
    e(
        "TG0753",
        "Two operations or macros end up with the same command path in the generated CLI (an \
         operation and a macro with the same name, or a resource renamed onto another). The \
         later one gets a numeric suffix so that both stay reachable.",
        "Give the operations distinct names with `naming.operations` in tungsten.yml.",
    ),
    e(
        "TG0760",
        "Some `components/schemas` entries are not reachable from any operation (typically the \
         schemas only the operations excluded by an `include` predicate use), so they were not \
         generated: no type, no validator, and none of the warnings their schemas would have \
         raised. The note counts them and lists the first names. Operations that are only \
         planned (`planned_from`) and the error envelope still keep the types they use.",
        "Nothing to fix. To generate every schema anyway, set `types.prune_unreferenced: false` \
         in tungsten.yml.",
    ),
    e(
        "TG0790",
        "An option of the `webmcp` target is invalid, such as a malformed npm package, SDK \
         dependency, runtime dependency or version. The emitter uses its documented default for that option.",
        "Fix the option under `targets.webmcp` in tungsten.yml.",
    ),
    e(
        "TG0791",
        "The WebMCP emitter could not add a generated file to its output. The diagnostic \
         identifies the file and the reason.",
        "Check the target configuration. If the generated path is invalid or duplicated, \
         report an emitter bug.",
    ),
    e(
        "TG0792",
        "The `webmcp` target has no explicit SDK dependency, so the browser adapter depends \
         on the package and version the `typescript` target generates.",
        "Generate and make that TypeScript package available, or set `targets.webmcp.sdk` \
         (`<package>` or `<package>@<range>`) or `targets.webmcp.sdk_path`.",
    ),
    e(
        "TG0793",
        "A confirmation-bearing WebMCP tool's generated `__preview` name is already used \
         by another invocation tool. The browser runtime rejects a registration whose \
         selected tools contain that collision.",
        "Rename the colliding operation or macro so invocation and preview tool names \
         are distinct.",
    ),
    e(
        "TG0801",
        "A target in tungsten.yml declares `external`, but its emitter could not be run: \
         with `external: true` there is no executable `tungsten-emit-<name>` on PATH; with a \
         command, its program does not exist, is not executable, or the operating system \
         refused to start it. Nothing is generated for the target.",
        "Install the emitter and put it on PATH, or point `external` at it (a path with a \
         slash is relative to tungsten.yml, a bare name is looked up on PATH). `tungsten \
         emitters` lists what tungsten finds.",
    ),
    e(
        "TG0802",
        "The external emitter answered with a protocol version other than 1, the only \
         version this tungsten speaks. The response is not read.",
        "Use an emitter release built for protocol 1, or a tungsten release that speaks the \
         emitter's protocol. `tungsten emitters` shows the protocol each emitter reports.",
    ),
    e(
        "TG0803",
        "The external emitter ran but did not finish normally: it exited with a non-zero \
         status or was killed by a signal (the end of its standard error is quoted), it did \
         not finish within `timeout_ms` (default 60 s), or it wrote more than \
         `max_output_bytes` (default 64 MiB) to standard output. Nothing is generated for \
         the target.",
        "Run the emitter by hand with a request on its standard input to see why it fails. \
         Raise `timeout_ms` or `max_output_bytes` on the target if the limit is too tight \
         for a large API.",
    ),
    e(
        "TG0804",
        "The external emitter's standard output is not a valid response of protocol 1: it is \
         not JSON, a field has the wrong type, a file has both or neither of `content` and \
         `content_base64`, a base64 value is malformed, a path is listed twice, or there are \
         more than 10000 files. The same code reports \
         a `--describe` document that is not valid. Nothing is generated for the target.",
        "Check the emitter against the schema printed by `tungsten schema external-emitter` \
         (validate the response against `#/$defs/EmitResponse`).",
    ),
    e(
        "TG0805",
        "The external emitter named a file path that is absolute, has a `..`, `.` or empty \
         segment, a backslash, a NUL byte or a drive prefix, or starts with `.tungsten/`. \
         Generated files are written only inside the target's `out` directory, so the \
         whole response is refused and nothing is written.",
        "Make the emitter return paths relative to the output directory with `/` separators. \
         It never needs the absolute output path.",
    ),
    e(
        "TG0806",
        "A diagnostic reported by an external emitter, kept with its severity. The message \
         reads `external emitter <name>: <code>: <text>`, where `<code>` is the emitter's \
         own code. An error fails the target; under `--strict` a warning does too.",
        "Fix the cause the emitter describes; `tungsten explain` knows only tungsten's own \
         codes, so look the emitter's code up in the emitter's documentation.",
    ),
    e(
        "TG0807",
        "A target in tungsten.yml declares `external`, and tungsten was told not to start \
         external emitters (`--no-external` or `TUNGSTEN_NO_EXTERNAL=1`), so its command was \
         not run and the target is skipped. Nothing is generated, compared or reported for it; \
         the diagnostic is informational and does not fail the run. `emitters` discovery \
         is also disabled, including descriptor probes of programs on PATH; an empty \
         inspection result does not mean no emitters are installed.",
        "This is the intended result for CI that validates a manifest it does not trust. Run \
         without the option, on a manifest you trust, to emit or check the target.",
    ),
    e(
        "TG0808",
        "An external target was selected for a project with imported-operation policy, \
         but its protocol-1 --describe response omitted `import_policy` or set it to false. \
         The emitter is not run and no files are written for that target. Other supported \
         targets are still processed. Built-in targets without this capability use TG0705.",
        "Use an emitter that declares `import_policy: true` and enforces the imported \
         metadata (the Go emitter in `emitters/go` does), or select a supported TypeScript, \
         Python, Rust, Java, Kotlin, Ruby, C#, F#, Dart, MCP, WebMCP or docs target. Do not \
         remove policy metadata merely to bypass this check. Protocol-1 \
         emitters remain compatible with projects that have no imported-operation policy.",
    ),
    e(
        "TG0809",
        "A deliberately selected host policy could not be captured, exceeded the strict byte/document/nesting bounds, was not valid strict JSON, or did not match its exact lowercase BLAKE3 pin. Physical paths and authored values are omitted. No external emitter is started.",
        "Use a regular local non-symlink policy and separately review `tungsten policy digest PATH`. Execution never updates the pin automatically. Policy limits do not grant executable approval or isolation.",
    ),
    e(
        "TG0810",
        "An explicit external target timeout/output value exceeds a protected ceiling, or cannot be represented on this platform. Missing/null target values keep existing defaults and are clamped. All tying protected sources remain in provenance.",
        "Lower the project declaration to the host ceiling. A project/profile cannot weaken host restrictions. Descriptor negotiation is subject to the same preflight.",
    ),
    e(
        "TG0811",
        "The invocation's selected host policy denies external emitter execution. Runs, imported-policy descriptor negotiation and PATH descriptor discovery are refused before execution. This is a failure, distinct from the unconditional informational --no-external skip.",
        "Select built-in targets or obtain a deliberately reviewed host policy. A permissive policy is not executable approval, environment isolation or a sandbox.",
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
    e(
        "TG0920",
        "`tungsten overlay suggest` found a diagnostic that an overlay action could fix, but \
         the value the action needs cannot be derived without guessing: the operation has no \
         single id in the IR (its document is shared by several namespaces, or the operation \
         is expanded from an rpc envelope), or its position cannot be written as an Overlay \
         target. The overlay suggestion leaves it out; it never writes placeholder text.",
        "Fix the diagnostic by hand in the spec (add or change the operationId), or in the \
         overlay you maintain yourself.",
    ),
    e(
        "TG0921",
        "`tungsten overlay suggest` only writes actions whose fix the compiler has already \
         decided: the id of an operation without operationId, and the unique id of an \
         operation whose operationId is used more than once. Warnings of any other code, \
         such as an inferred pagination (TG0501) or a renamed field or type (TG0401), need a \
         decision or live in tungsten.yml; the count is reported once per code.",
        "Use `tungsten explain` on the code for its usual fix, for example declare the \
         pagination under `pagination` in tungsten.yml.",
    ),
    e(
        "TG0930",
        "`tungsten check --audit-contracts` found a success response whose JSON media type \
         declares no `schema`, so any JSON value is accepted and the generated SDK types \
         the value as any. The message names the operation, the status and the media type; \
         the second location is the operation that answers it. Default runs are unchanged: \
         the audit runs only with the flag or `--require-contracts`, which fails on audit \
         findings alone. `--strict` fails on these and every other warning.",
        "Declare a schema for the media type, in the spec or in a reviewed overlay listed \
         under `overlays` in tungsten.yml, or acknowledge the gap with \
         `x-tungsten-reviewed-contract` on the media-type entry when any value really is \
         accepted.",
    ),
    e(
        "TG0931",
        "`tungsten check --audit-contracts` found a success response schema that is a \
         free-form object: `type: object` with no properties and an open or unconstrained \
         `additionalProperties`, so the generated type is a plain mapping of any values (a \
         `dict[str, Any]`). A typed map, a record with properties, a response without a \
         JSON body and a non-JSON media type are not findings; a schema marked \
         `x-tungsten-reviewed-contract` counts as reviewed. See TG0930 for how the audit \
         is run and enforced.",
        "Declare the properties the server really answers with, named response models or \
         their union, in the spec or a reviewed overlay, or acknowledge the free-form \
         shape with `x-tungsten-reviewed-contract` when it is intentional.",
    ),
    e(
        "TG0932",
        "The opt-in response-contract audit reached its schema traversal budget. Its \
         findings are incomplete. This warning fails --require-contracts or --strict, so an \
         incomplete audit cannot silently satisfy that quality gate.",
        "Split the input into smaller projects and audit each one. Reuse named schemas \
         for shared contracts. The audit follows references and nested shapes iteratively \
         and reports each weak source node only once.",
    ),
    e(
        "TG1010",
        "An option of the `java` target in tungsten.yml is not valid: `group_id` and `package` \
         must be dotted Java package names (no keywords), `artifact_id` a Maven artifactId, \
         `version` and `runtime` versions such as 1.2.3, `runtime_path` a non-empty path. The \
         generator uses the option's default instead.",
        "Fix the option under `targets.java` in tungsten.yml, or remove it to keep the default.",
    ),
    e(
        "TG1011",
        "The Java SDK has no native type for this schema (an allOf whose members cannot be \
         merged into one record, or an enum whose values mix JSON types), so the typed value \
         is the runtime's JSON value (`Object`: JsonObject, List, String, Long, Double, \
         Boolean or null). The runtime still validates it against the schema.",
        "Merge the allOf members into one object, or give the enum values a single type, if a \
         typed Java model is wanted.",
    ),
    e(
        "TG1012",
        "A macro of the agent manifest is not in the canonical form the SDK runtimes run \
         (steps over callable operations, references to $input or earlier steps, a known \
         step kind), so the Java SDK does not include it; the TypeScript SDK reports the same \
         macro as TG0710.",
        "Fix the macro in agent.yml at the pointer the diagnostic names.",
    ),
    e(
        "TG1013",
        "The Java runtime sends an OpenID Connect scheme as a bearer token. It does not run \
         discovery or obtain tokens itself.",
        "Configure the scheme with `AuthCredential.secret(token)` holding an access token you \
         obtained from the provider.",
    ),
    e(
        "TG1014",
        "The operation answers different JSON bodies depending on the exact status, so the \
         typed method's result is the runtime's JSON value; the result's metadata carries \
         the status that tells them apart.",
        "Read `meta().status()` of the result and decode the body with the matching model's \
         `fromJson`.",
    ),
    e(
        "TG1015",
        "The union declares a discriminator, but at least one variant has no tag value or is \
         not a named type, so the Java SDK cannot pick the variant by its tag: it decodes the \
         variants in the plan's order and takes the first that fits.",
        "Give every variant a named schema and a discriminator mapping value.",
    ),
    e(
        "TG1020",
        "An option of the `csharp` target in tungsten.yml is not valid: `package_id` must be a \
         NuGet package id, `namespace` a dotted C# namespace (no keywords), `version` and \
         `runtime` versions such as 1.2.3, `runtime_path` a non-empty path. The generator uses \
         the option's default instead.",
        "Fix the option under `targets.csharp` in tungsten.yml, or remove it to keep the default.",
    ),
    e(
        "TG1021",
        "The C# SDK has no native type for this schema: an `allOf` without members has nothing \
         to type, so its typed value is the runtime's `JsonNode?`, which the runtime still \
         validates against the schema. Two shapes that used to fall back to `JsonNode?` are \
         typed since 0.2.0: an `allOf` whose members cannot be merged into one record (a member \
         is not a record, has typed extra members, or two members share a wire name) is a \
         sealed record with one typed property per member (each decodes the whole value, and \
         `ToJson()` merges the properties' JSON), and an enum of mixed JSON types (booleans, \
         fractional numbers, integers beyond 64 bits) is a readonly record struct of its known \
         JSON values (`Value` is the wire value, compared as JSON).",
        "Give the `allOf` at least one member, or use the `JsonNode?` value.",
    ),
    e(
        "TG1022",
        "A macro of the agent manifest is not in the canonical form the SDK runtimes run \
         (steps over callable operations, references to $input or earlier steps, a known step \
         kind), so the C# SDK does not include it; the TypeScript SDK reports the same macro as \
         TG0710.",
        "Fix the macro in agent.yml at the pointer the diagnostic names.",
    ),
    e(
        "TG1023",
        "The .NET runtime sends an OpenID Connect scheme as a bearer token. It does not run \
         discovery or obtain tokens itself.",
        "Configure the scheme with `Credential.Secret(token)` holding an access token you \
         obtained from the provider.",
    ),
    e(
        "TG1024",
        "Reserved; not reported since 0.2.0. An operation that answers different JSON bodies \
         depending on the exact status returns a nested abstract record of its answers \
         (`<Operation>Response`, one `Status<code>` record per status holding the typed body, \
         or none for an empty one). `Status` is the HTTP status of the response; a success with \
         a status the operation does not declare fails as an unexpected response.",
        "Match on the answer's `Status<code>` records (or read `Status`).",
    ),
    e(
        "TG1025",
        "The union declares a discriminator, but at least one variant has no tag value or is not \
         a named type, so the C# SDK cannot pick the variant by its tag: it decodes the variants \
         in the plan's order and takes the first that fits.",
        "Give every variant a named schema and a discriminator mapping value.",
    ),
    e(
        "TG1030",
        "An option of the `kotlin` target in tungsten.yml is not valid: `group` and `package` \
         must be dotted package names usable in Kotlin and Java (no keywords), `artifact` an \
         artifact name, `version` and `runtime` versions such as 1.2.3, `runtime_path` and \
         `java_runtime_path` and `coroutines_runtime_path` non-empty paths, `coroutines` true or false. The generator uses the option's default instead.",
        "Fix the option under `targets.kotlin` in tungsten.yml, or remove it to keep the default.",
    ),
    e(
        "TG1031",
        "The Kotlin SDK has no native type for this schema (an allOf whose members cannot be \
         merged into one data class, or an enum whose values mix JSON types), so the typed \
         value is the Java runtime's JSON value (`Any?`: JsonObject, List, String, Long, \
         Double, Boolean or null). The runtime still validates it against the schema.",
        "Merge the allOf members into one object, or give the enum values a single type, if a \
         typed Kotlin model is wanted.",
    ),
    e(
        "TG1032",
        "A macro of the agent manifest is not in the canonical form the SDK runtimes run \
         (steps over callable operations, references to $input or earlier steps, a known step \
         kind), so the Kotlin SDK does not include it; the TypeScript SDK reports the same \
         macro as TG0710.",
        "Fix the macro in agent.yml at the pointer the diagnostic names.",
    ),
    e(
        "TG1033",
        "The Kotlin SDK runs on the Java runtime, which sends an OpenID Connect scheme as a \
         bearer token. It does not run discovery or obtain tokens itself.",
        "Configure the scheme with `AuthCredential.secret(token)` holding an access token you \
         obtained from the provider.",
    ),
    e(
        "TG1034",
        "The operation answers different JSON bodies depending on the exact status, so the \
         typed function's result is the runtime's JSON value; the result's metadata carries \
         the status that tells them apart.",
        "Read `meta().status()` of the result and decode the body with the matching model's \
         `fromJson`.",
    ),
    e(
        "TG1035",
        "The union declares a discriminator, but at least one variant has no tag value or is \
         not a named type, so the Kotlin SDK cannot pick the variant by its tag: it decodes \
         the variants in the plan's order and takes the first that fits.",
        "Give every variant a named schema and a discriminator mapping value.",
    ),
    e(
        "TG1060",
        "An option of the `ruby` target is not valid (a `gem` that is not a lowercase gem name, \
         a `module` that is not a Ruby constant path or names a core class or the runtime's \
         `Tungsten`, a `version` that is not a gem version, a `runtime` that is not a gem \
         requirement, or an empty `runtime_path`), so its default is used.",
        "Fix the option under `targets.ruby` in tungsten.yml.",
    ),
    e(
        "TG1061",
        "An `allOf` could not be merged into one record (a member is not a record, has typed \
         extra members, or two members share a wire name). The Ruby SDK has no model for it: \
         its typed value is the JSON value, which the runtime still validates against every \
         member.",
        "Restructure the schema so its members are records with distinct properties, or \
         accept the JSON value.",
    ),
    e(
        "TG1062",
        "A macro of agent.yml is not in the canonical form the SDKs run (an unknown step kind \
         or operation, a reference to no earlier step, an invalid `as`, `until` or budget). The \
         Ruby SDK leaves it out; the other operations are generated as usual.",
        "Fix the macro in agent.yml; the message names the offending member.",
    ),
    e(
        "TG1063",
        "The API declares an OpenID Connect security scheme. The Ruby SDK sends the configured \
         credential as a bearer token and does not run discovery or obtain tokens itself.",
        "Obtain the token yourself and pass it in `auth:` under the scheme's name.",
    ),
    e(
        "TG1064",
        "An operation answers different JSON bodies depending on the status. The Ruby SDK's \
         typed result for it is the decoded JSON value; the runtime validates it against the \
         body of the status that arrived.",
        "Nothing to fix; read `result.meta.status` to tell the bodies apart.",
    ),
    e(
        "TG1065",
        "A union has a discriminator, but one of its variants has no tag value or is not a \
         named type, so the Ruby SDK cannot pick the variant by the tag. It tries the variants \
         in order and decodes the first that accepts the value.",
        "Give every variant a `discriminator.mapping` entry or a `const` tag property.",
    ),
    e(
        "TG1070",
        "An option of the `dart` target is not valid (a `package` that is not a pub package \
         name or is a Dart keyword or `tungsten_runtime`, a `version` that is not a semantic \
         version, a `runtime` that is not a pub version constraint, or an empty \
         `runtime_path`), so its default is used.",
        "Fix the option under `targets.dart` in tungsten.yml.",
    ),
    e(
        "TG1071",
        "A schema has no Dart type: an `allOf` without members has nothing to type, and an \
         enum with an integer beyond 64 bits has no Dart constant (a Dart `int` is 64 bits, and \
         a `double` would send another number). The Dart SDK types it as its JSON value \
         (`Object?`), which the runtime still validates against the schema. Two shapes that \
         used to fall back to `Object?` are typed since 0.2.0: an `allOf` whose members cannot \
         be merged into one record (a member is not a record, has typed extra members, or two \
         members share a wire name) is a final class with one typed part per member (each \
         decodes the whole value, and `toJson()` merges the parts' JSON), and an enum of mixed \
         JSON types (booleans, fractional numbers) is an enhanced enum whose `wire` is the JSON \
         value, compared as JSON.",
        "Give the `allOf` at least one member, keep the integers of the enum within 64 bits, or \
         accept the JSON value.",
    ),
    e(
        "TG1072",
        "A macro of agent.yml is not in the canonical form the SDKs run (an unknown step kind \
         or operation, a reference to no earlier step, an invalid `as`, `until` or budget). The \
         Dart SDK leaves it out; the other operations are generated as usual.",
        "Fix the macro in agent.yml; the message names the offending member.",
    ),
    e(
        "TG1073",
        "The API declares an OpenID Connect security scheme. The Dart SDK sends the configured \
         credential as a bearer token and does not run discovery or obtain tokens itself.",
        "Obtain the token yourself and pass it in `ClientOptions.auth` under the scheme's name.",
    ),
    e(
        "TG1074",
        "Reserved; not reported since 0.2.0. An operation that answers different JSON bodies \
         depending on the exact status returns a sealed class of its answers \
         (`<Resource><Operation>Response`, one `...Status<code>` subclass per status; `value` \
         is the typed body, absent for an empty one, and `status` the HTTP status). A success \
         with a status the operation does not declare fails as an unexpected response.",
        "Switch over the answer's subclasses (or read `status`).",
    ),
    e(
        "TG1075",
        "A union has a discriminator, but one of its variants has no tag value or is not a \
         named type, so the Dart SDK cannot pick the variant by the tag. It tries the variants \
         in order and decodes the first that accepts the value.",
        "Give every variant a `discriminator.mapping` entry or a `const` tag property.",
    ),
    e(
        "TG1080",
        "An option of the `fsharp` target in tungsten.yml is not valid: `package_id` must be a \
         NuGet package id, `namespace` a dotted F# namespace (no F# or C# keywords), `version` \
         and `runtime` versions such as 1.2.3, `runtime_path` and `csharp_runtime_path` \
         non-empty paths. The generator uses the option's default instead.",
        "Fix the option under `targets.fsharp` in tungsten.yml, or remove it to keep the default.",
    ),
    e(
        "TG1081",
        "The F# SDK has no native type for this schema: an `allOf` without members has nothing \
         to type, so its typed value is the .NET runtime's `JsonNode`, which the runtime still \
         validates against the schema. Two shapes that used to fall back to `JsonNode` are \
         typed since 0.2.0: an `allOf` whose members cannot be merged into one record (a member \
         is not a record, has typed extra members, or two members share a wire name) is a \
         record with one typed field per member (each decodes the whole value, and `ToJson()` \
         merges the fields' JSON), and an enum of mixed JSON types (booleans, fractional \
         numbers, integers beyond 64 bits) is a union of its known JSON values plus `Unknown \
         of JsonNode` (`Value` is the wire value, compared as JSON).",
        "Give the `allOf` at least one member, or use the `JsonNode` value.",
    ),
    e(
        "TG1082",
        "A macro of the agent manifest is not in the canonical form the SDK runtimes run \
         (steps over callable operations, references to $input or earlier steps, a known step \
         kind), so the F# SDK does not include it; the TypeScript SDK reports the same macro as \
         TG0710.",
        "Fix the macro in agent.yml at the pointer the diagnostic names.",
    ),
    e(
        "TG1083",
        "The F# SDK runs on the .NET runtime, which sends an OpenID Connect scheme as a bearer \
         token. It does not run discovery or obtain tokens itself.",
        "Configure the scheme with `Credential.Secret(token)` holding an access token you \
         obtained from the provider.",
    ),
    e(
        "TG1084",
        "Reserved; not reported since 0.2.0. An operation that answers different JSON bodies \
         depending on the exact status returns a union of its answers \
         (`<Class><Member>Response`, one `Status<code>` case per status holding the typed body, \
         none for an empty one). `Status` is the HTTP status of the response; a success with a \
         status the operation does not declare fails as an unexpected response.",
        "Match on the answer's `Status<code>` cases (or read `Status`).",
    ),
    e(
        "TG1085",
        "The union declares a discriminator, but at least one variant has no tag value or is not \
         a named type, so the F# SDK cannot pick the variant by its tag: it decodes the variants \
         in the plan's order and takes the first that fits.",
        "Give every variant a named schema and a discriminator mapping value.",
    ),
    e(
        "TG1201",
        "A saved cURL command or `.http` request given to `tungsten init` cannot be imported. \
         Requests are parsed as data and never executed, so anything that needs a shell, a \
         file or an environment is refused: compound commands, expansions, unsupported flags, \
         file bodies or headers, unbound `{{variables}}`, credentials in the URL, headers or \
         query, a non-JSON body, an input over the size limit, or a missing or mismatched \
         `--route` template. The message names the first problem.",
        "Save exactly one request. For cURL use `curl` (or `curl.exe` in PowerShell) with \
         --request, --header, --url and inline JSON data only, and pass the trusted path \
         template with `--route`. Bind every `{{variable}}` of a `.http` file explicitly and \
         describe authentication in an endpoint manifest instead of the request.",
    ),
];

// The table stays sorted by code for readers: an entry out of order or a
// duplicate code fails the build.
const _: () = assert!(
    strictly_sorted(EXPLANATIONS),
    "EXPLANATIONS must stay sorted by code"
);

const fn strictly_sorted(table: &[Explanation]) -> bool {
    let mut i = 1;
    while i < table.len() {
        if !less(table[i - 1].code.as_bytes(), table[i].code.as_bytes()) {
            return false;
        }
        i += 1;
    }
    true
}

/// `a < b` byte-wise, as `str::cmp` orders them.
const fn less(a: &[u8], b: &[u8]) -> bool {
    let mut i = 0;
    while i < a.len() && i < b.len() {
        if a[i] != b[i] {
            return a[i] < b[i];
        }
        i += 1;
    }
    a.len() < b.len()
}

/// The extended explanation of a code, if one exists. A linear scan, so a
/// lookup never depends on the table staying sorted (it is kept sorted by code
/// for readers, but an out-of-order entry once hid TG0705 from a binary search).
pub(crate) fn explanation(code: &str) -> Option<&'static Explanation> {
    EXPLANATIONS.iter().find(|e| e.code == code)
}

/// The code range a diagnostic belongs to.
pub(crate) fn area(code: &str) -> &'static str {
    match code.get(..2) {
        Some("GO") => return "Go emitter (GOxxx)",
        Some("HR") => return "HAR import (HRxxx)",
        Some("PM") => return "Postman import (PMxxx)",
        Some("TC") => return "request-contract checks (TCxxx)",
        _ => {}
    }
    match code.get(..4) {
        Some("TG01") => "parsing and input limits (TG01xx)",
        Some("TG02") => "references and cycles (TG02xx)",
        Some("TG03") => "type normalization (TG03xx)",
        Some("TG04") => "naming (TG04xx)",
        Some("TG05") => "operations, pagination and auth inference (TG05xx)",
        Some("TG06") => "manifests (TG06xx)",
        Some("TG07") => "emitter limits (TG07xx)",
        Some("TG08") => "external emitters (TG08xx)",
        Some("TG09") => "staleness and CI (TG09xx)",
        Some("TG10") => "SDK emitters: Java, C#, Kotlin, Swift, PHP, Ruby, Dart, F# (TG10xx)",
        Some("TG12") => "request import: saved cURL and .http (TG12xx)",
        _ => "unassigned range",
    }
}
