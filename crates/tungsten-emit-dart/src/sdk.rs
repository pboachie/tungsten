// SPDX-License-Identifier: AGPL-3.0-only
//! The SDK surface of a Dart package, over the models (`models`) and the
//! SDK descriptor document of [`tungsten_emit::sdk`]:
//!
//! - `lib/src/client.dart`: the client class (`<Api>Client`) over
//!   `ClientCore` with `ClientOptions`, one `late final` member per
//!   top-level resource (or per namespace class in a multi-namespace API),
//!   `macros`, `dispatch` and, with an OAuth2 authorization-code scheme,
//!   `oauth(scheme)`;
//! - `lib/src/resources/<resource>.dart`: one final class per resource,
//!   with one `Future<Result<T>>` method per operation taking named
//!   parameters (`required` for required arguments, `T?` for optional ones,
//!   `Patch<T>` for optional ones that may be null, and `CallOptions
//!   options`), `preview<Op>` for operations with a preview, `<op>Pages`
//!   (`Stream<Result<Page<T>>>`) for paginated operations and `<op>Stream`
//!   (`EventStream<T>`) for event streams, plus a getter per child
//!   resource;
//! - `lib/src/dispatch.dart`: `Dispatch`, the runtime's `Dispatcher` over
//!   the typed methods (operations by id, macros by name, JSON arguments
//!   keyed by the arguments layout keys);
//! - `lib/src/macros.dart`: one method per macro in the canonical form
//!   (`ClientCore.runMacro`) and its preview (`ClientCore.previewMacro`);
//! - `lib/src/descriptors.dart`: the descriptor document as string
//!   constants (no file is read at run time, so AOT and Flutter builds
//!   need no asset), parsed once into `descriptors`.
//!
//! Arguments objects use the TypeScript arguments layout: the keys of the
//! map every method hands to the runtime, and the `name` of every argument
//! in the document, are the arguments layout keys, so `failed_parameter`
//! and contract parity match the TypeScript SDK; the Dart parameter names
//! come from the [`tungsten_emit::sdk::Namer`].
//!
//! Notes ([`notes`]): TG1072 (a macro outside the canonical form is left
//! out), TG1073 (OpenID Connect is sent as a bearer token), TG1074
//! (different success bodies by status: the typed result is `Object?`) and
//! TG1075 (a discriminated union with a variant without a tag value: its
//! variants are tried in order).

mod client;
mod descriptors;
mod dispatch;
mod macros;
mod resources;

use std::collections::BTreeSet;

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::sdk::{
    ArgSource, ClientPlan, InputSource, MemberKind, SdkPlan, SuccessPlan, TypeKind, UnionPlan,
};
use tungsten_ir::{AuthScheme, BodyEncoding, Presence, Shape};

use crate::dart::{Code, Expr, contains_word, directive, one_line, str_lit};
use crate::models::Dt;
use crate::options::RUNTIME_PACKAGE;
use crate::{Gen, Library};

/// The class name of the generated dispatcher (reserved for it among the
/// type and class names).
const DISPATCH_CLASS: &str = "Dispatch";

/// The names of `tungsten_runtime` the SDK libraries may use; a library
/// imports those its code mentions.
const RUNTIME_NAMES: &[&str] = &[
    "CallOptions",
    "ClientCore",
    "ClientOptions",
    "DispatchEntry",
    "Dispatcher",
    "EventStream",
    "Models",
    "OAuthFlow",
    "Page",
    "Patch",
    "PreviewResult",
    "Result",
    "SdkDescriptors",
    "Typed",
];

/// The SDK libraries of a package, in the order the entry library exports
/// them: the client, the resources, the dispatcher, the macros and the
/// descriptor document.
pub(crate) fn libraries(g: &Gen<'_, '_>) -> Vec<Library> {
    let mut out = vec![client::library(g)];
    out.extend(resources::libraries(g));
    out.push(dispatch::library(g));
    if !g.plan.macros.is_empty() {
        out.push(macros::library(g));
    }
    out.push(descriptors::library(g));
    out
}

/// TG1072 to TG1075 for a plan (see the module documentation).
pub(crate) fn notes(plan: &SdkPlan<'_>) -> Diagnostics {
    let ir = plan.ir;
    let mut diags = Diagnostics::new();
    for issue in &plan.macro_issues {
        diags.push(
            Diagnostic::warning(
                "TG1072",
                format!(
                    "macro `{}` is not in the canonical form ({}); the Dart SDK leaves it out",
                    issue.name, issue.reason
                ),
            )
            .at("agent.yml", issue.pointer.clone(), None),
        );
    }
    for s in &ir.auth {
        if let AuthScheme::OpenIdConnect { name, .. } = s {
            diags.push(Diagnostic::info(
                "TG1073",
                format!(
                    "auth scheme `{name}` (OpenID Connect) is sent as a bearer token by the Dart SDK; discovery is not supported"
                ),
            ));
        }
    }
    for op in &plan.operations {
        if let SuccessPlan::ByStatus(_) = op.success {
            diags.push(Diagnostic::info(
                "TG1074",
                format!(
                    "operation `{}` answers different bodies by status: its Dart typed result is the JSON value (`Object?`)",
                    op.op.id.0
                ),
            ));
        }
    }
    for t in plan.types.values() {
        if let TypeKind::Union(UnionPlan::Untagged { .. }) = &t.kind
            && let Some(Shape::Union(u)) = ir.types.get(&t.id).map(|n| &n.shape)
            && u.discriminator.is_some()
        {
            diags.push(Diagnostic::warning(
                "TG1075",
                format!(
                    "union `{}` has a discriminator, but a variant has no tag value or is not a named type: the Dart SDK tries its variants in order",
                    t.id.0
                ),
            ));
        }
    }
    diags
}

// ------------------------------------------------------------- arguments

/// One named parameter of a generated method: an operation argument or a
/// macro input field.
pub(crate) struct Arg {
    /// The Dart parameter name.
    pub name: String,
    /// The key in the arguments object (the arguments layout key).
    pub key: String,
    pub dt: Dt,
    pub presence: Presence,
    pub doc: String,
}

impl Arg {
    /// Whether the key must be given.
    pub fn required(&self) -> bool {
        matches!(
            self.presence,
            Presence::Required | Presence::RequiredNullable
        )
    }
}

/// The parameters of operation `o`, in arguments layout order.
pub(crate) fn op_args(g: &Gen<'_, '_>, o: usize) -> Vec<Arg> {
    let op = &g.plan.operations[o];
    let names = &g.names.operations[o].arguments;
    op.arguments
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let dt = match (&a.ty, &a.source) {
                (Some(t), _) => g.dt(t),
                (
                    None,
                    ArgSource::Body {
                        encoding: BodyEncoding::Text,
                        ..
                    },
                ) => Dt::Str,
                (None, ArgSource::Body { .. }) => Dt::Bytes { base64: false },
                (None, _) => Dt::Json,
            };
            let doc = match &a.source {
                ArgSource::Param { location, param } => {
                    let text = param
                        .doc
                        .as_ref()
                        .and_then(|d| d.summary.clone().or_else(|| d.description.clone()))
                        .map(|s| one_line(&s))
                        .unwrap_or_default();
                    let at = location.as_str();
                    if text.is_empty() {
                        format!("The {at} parameter `{}`.", a.wire)
                    } else {
                        format!("{text} (the {at} parameter `{}`)", a.wire)
                    }
                }
                ArgSource::Field(f) => {
                    let text = f
                        .doc
                        .as_ref()
                        .and_then(|d| d.summary.clone().or_else(|| d.description.clone()))
                        .map(|s| one_line(&s))
                        .unwrap_or_default();
                    if text.is_empty() {
                        format!("The body member `{}`.", a.wire)
                    } else {
                        format!("{text} (the body member `{}`)", a.wire)
                    }
                }
                ArgSource::Body { media_type, .. } => format!("The request body (`{media_type}`)."),
            };
            Arg {
                name: names.get(i).cloned().unwrap_or_else(|| a.key.clone()),
                key: a.key.clone(),
                dt,
                presence: a.presence,
                doc,
            }
        })
        .collect()
}

/// The parameters of macro `k`: the extended operation's arguments with
/// their types, the added fields with the type of their JSON Schema.
pub(crate) fn macro_args(g: &Gen<'_, '_>, k: usize) -> Vec<Arg> {
    let m = &g.plan.macros[k];
    let names = &g.names.macros[k].input;
    let base = m.base.map(|b| op_args(g, b));
    m.input
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let (dt, doc) = match (&f.source, &base) {
                (InputSource::Argument(a), Some(args)) => match args.get(*a) {
                    Some(arg) => (arg.dt.clone(), arg.doc.clone()),
                    None => (Dt::Json, String::new()),
                },
                (InputSource::Added { schema, .. }, _) => {
                    let doc = schema
                        .get("description")
                        .and_then(|d| d.as_str())
                        .map(one_line)
                        .unwrap_or_else(|| format!("The input field `{}`.", f.key));
                    (schema_dt(schema), doc)
                }
                _ => (Dt::Json, String::new()),
            };
            Arg {
                name: names.get(i).cloned().unwrap_or_else(|| f.key.clone()),
                key: f.key.clone(),
                dt,
                presence: f.presence,
                doc,
            }
        })
        .collect()
}

/// The Dart representation of an added macro input field's JSON Schema.
fn schema_dt(schema: &serde_json::Value) -> Dt {
    match schema.get("type").and_then(|t| t.as_str()) {
        Some("string") => Dt::Str,
        Some("integer") => Dt::Int,
        Some("number") => Dt::Double,
        Some("boolean") => Dt::Bool,
        Some("array") => Dt::List(Box::new(Dt::Json)),
        Some("object") => Dt::Object,
        _ => Dt::Json,
    }
}

/// The declaration of a named parameter.
pub(crate) fn param_decl(g: &Gen<'_, '_>, a: &Arg) -> String {
    let ty = g.dart_type(&a.dt, None);
    let nullable = if a.dt.nullable() {
        ty.clone()
    } else {
        format!("{ty}?")
    };
    match a.presence {
        Presence::Required => format!("required {ty} {}", a.name),
        Presence::RequiredNullable => format!("required {nullable} {}", a.name),
        Presence::Optional => format!("{nullable} {}", a.name),
        Presence::OptionalNullable => format!("Patch<{ty}> {} = const Patch.absent()", a.name),
    }
}

/// Whether a Dart value of `dt` is its own JSON value for the runtime.
fn is_plain(dt: &Dt) -> bool {
    match dt {
        Dt::Str | Dt::Int | Dt::Double | Dt::Bool => true,
        Dt::Nullable(i) => is_plain(i),
        _ => false,
    }
}

/// The runtime value of `value` (a Dart value of `dt`): itself for
/// strings, numbers and booleans, else `Models.dump` (models as JSON, bytes
/// as `Binary`, `Patch`es and `unset` members resolved).
fn value_out(dt: &Dt, value: &str) -> String {
    if is_plain(dt) {
        value.to_string()
    } else {
        format!("Models.dump({value})")
    }
}

/// `final $args = <String, Object?>{...};`: the arguments object, keyed by
/// arguments layout key, without the optional arguments that are not
/// given.
pub(crate) fn args_map(w: &mut Code, args: &[Arg]) {
    if args.is_empty() {
        w.line("final $args = <String, Object?>{};");
        return;
    }
    w.line("final $args = <String, Object?>{");
    w.indent();
    for a in args {
        let key = str_lit(&a.key);
        match a.presence {
            Presence::Required | Presence::RequiredNullable => {
                let v = value_out(&a.dt, &a.name);
                w.line(format!("{key}: {v},"));
            }
            Presence::Optional => {
                let v = value_out(&a.dt, &a.name);
                w.if_element(
                    &format!("{} != null", a.name),
                    &format!("{key}: "),
                    &Expr::raw(v),
                    ",",
                );
            }
            Presence::OptionalNullable => {
                let v = value_out(&a.dt, &format!("{}.valueOrNull", a.name));
                w.if_element(
                    &format!("{}.isPresent", a.name),
                    &format!("{key}: "),
                    &Expr::raw(v),
                    ",",
                );
            }
        }
    }
    w.dedent();
    w.line("};");
}

/// The expression that reads argument `a` from the JSON arguments object
/// `args` (in a dispatcher); throws a `FormatException` when the value
/// does not fit, so the call goes to the core as it is.
pub(crate) fn arg_in(g: &Gen<'_, '_>, a: &Arg) -> Expr {
    let decoder = match (&a.dt, a.presence) {
        // An optional `T?` reads a `T`: null is not a value it sends.
        (Dt::Nullable(inner), Presence::Optional) => g.decoder(inner, None),
        (dt, _) => g.decoder(dt, None),
    };
    let key = Expr::raw(str_lit(&a.key));
    let args = Expr::raw("args");
    let path = Expr::raw("r'$'");
    match a.presence {
        Presence::Required => Expr::call("Wire.required", vec![args, key, path, decoder]),
        Presence::RequiredNullable => {
            Expr::call("Wire.requiredNullable", vec![args, key, path, decoder])
        }
        Presence::Optional => Expr::call("_optional", vec![args, key, decoder]),
        Presence::OptionalNullable => Expr::call("Wire.patch", vec![args, key, path, decoder]),
    }
}

/// Method parameters `{ ..., CallOptions options = const CallOptions(), }`
/// after `head` (`Future<Result<T>> name`), always split one per line.
pub(crate) fn method_head(w: &mut Code, g: &Gen<'_, '_>, head: &str, args: &[Arg]) {
    w.line(format!("{head}({{"));
    w.indent();
    for a in args {
        let decl = format!("{},", param_decl(g, a));
        if w.fits(&decl) {
            w.line(decl);
        } else if let Some((head, name)) = decl
            .rsplit_once(' ')
            .filter(|_| !matches!(a.presence, Presence::OptionalNullable))
        {
            // `dart format` puts the name under a type it cannot split.
            w.line(head);
            w.indent();
            w.indent();
            w.line(name);
            w.dedent();
            w.dedent();
        } else {
            w.line(decl);
        }
    }
    w.line("CallOptions options = const CallOptions(),");
    w.dedent();
    w.line("}) {");
}

/// The documentation of the parameters of a method.
pub(crate) fn args_doc(args: &[Arg]) -> String {
    let mut text = String::new();
    for a in args {
        let presence = match a.presence {
            Presence::Required => "required",
            Presence::RequiredNullable => "required, may be null",
            Presence::Optional => "optional",
            Presence::OptionalNullable => "optional, may be null",
        };
        text.push_str(&format!("- `{}` ({presence}): {}\n", a.name, a.doc));
    }
    text.trim_end().to_string()
}

/// [`Code::expr`], except that a prefixed name that does not fit
/// (`$workflow.LongName.fromJson`) is split before its first `.`, one
/// level deeper, as `dart format` splits it.
pub(crate) fn expr(w: &mut Code, prefix: &str, e: &Expr, suffix: &str) {
    let flat = format!("{prefix}{}{suffix}", e.flat());
    if w.fits(&flat) {
        w.line(flat);
        return;
    }
    match e {
        Expr::Call(callee, args) if !args.is_empty() => {
            w.line(format!("{prefix}{callee}("));
            w.indent();
            for a in args {
                expr(w, "", a, ",");
            }
            w.dedent();
            w.line(format!("){suffix}"));
        }
        Expr::Named(name, value) => expr(w, &format!("{prefix}{name}: "), value, suffix),
        Expr::Raw(text) if text.starts_with('$') && text.contains('.') => {
            let (head, rest) = text.split_at(text.find('.').unwrap_or(text.len()));
            w.line(format!("{prefix}{head}"));
            w.indent();
            w.indent();
            w.line(format!("{rest}{suffix}"));
            w.dedent();
            w.dedent();
        }
        _ => w.line(flat),
    }
}

// --------------------------------------------------------------- results

/// The success value of operation `o`: its Dart type and the decoder of
/// the wire value (`None`: the wire value itself, `Object?`).
pub(crate) fn success_dt(g: &Gen<'_, '_>, o: usize) -> Option<Dt> {
    let op = &g.plan.operations[o];
    match &op.success {
        SuccessPlan::Json(t) => match g.dt(t) {
            Dt::Json => None,
            dt => Some(dt),
        },
        SuccessPlan::Bytes => Some(Dt::Bytes { base64: false }),
        SuccessPlan::Text => Some(Dt::Str),
        SuccessPlan::None | SuccessPlan::ByStatus(_) | SuccessPlan::Raw(_) => None,
    }
}

/// The Dart type of a result value, nullable when `optional`.
pub(crate) fn result_type(g: &Gen<'_, '_>, dt: Option<&Dt>, optional: bool) -> String {
    match dt {
        None => "Object?".into(),
        Some(dt) => {
            let ty = g.dart_type(dt, None);
            if optional && !dt.nullable() {
                format!("{ty}?")
            } else {
                ty
            }
        }
    }
}

/// A private top-level decoder function `name` of a result value.
pub(crate) fn decoder_fn(w: &mut Code, g: &Gen<'_, '_>, name: &str, dt: &Dt) {
    let ty = g.dart_type(dt, None);
    w.line(format!("{ty} {name}(Object? raw) {{"));
    w.indent();
    w.expr("return ", &g.decode_value(dt, None, "raw", "r'$'"), ";");
    w.dedent();
    w.line("}");
}

// ----------------------------------------------------------------- paths

/// The getter chain from the client to each resource
/// (`public.webhooks.endpoints`), by resource index.
pub(crate) fn resource_paths(g: &Gen<'_, '_>) -> Vec<String> {
    let mut paths = vec![String::new(); g.plan.resources.len()];
    match &g.plan.client {
        ClientPlan::Resources(top) => {
            for (r, m) in top.iter().zip(&g.names.client_members) {
                paths[*r] = m.clone();
            }
        }
        ClientPlan::Namespaces(namespaces) => {
            for (k, ns) in namespaces.iter().enumerate() {
                let member = &g.names.client_members[k];
                for (r, m) in ns.resources.iter().zip(&g.names.namespace_members[k]) {
                    paths[*r] = format!("{member}.{m}");
                }
            }
        }
    }
    // Resources come depth first: every parent before its children.
    for (r, names) in g.names.resources.iter().enumerate() {
        for m in &names.members {
            if let MemberKind::Child(c) = m.kind {
                paths[c] = format!("{}.{}", paths[r], m.name);
            }
        }
    }
    paths
}

/// The name of a resource member.
pub(crate) fn member_name(g: &Gen<'_, '_>, resource: usize, kind: MemberKind) -> Option<String> {
    g.names.resources[resource]
        .members
        .iter()
        .find(|m| m.kind == kind)
        .map(|m| m.name.clone())
}

/// The file of a resource class under `lib/src/resources/`
/// (`PetsResource` → `pets_resource.dart`).
pub(crate) fn resource_file(class: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = class.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() {
            let prev_lower =
                i > 0 && (chars[i - 1].is_ascii_lowercase() || chars[i - 1].is_ascii_digit());
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            let prev_upper = i > 0 && chars[i - 1].is_ascii_uppercase();
            if i > 0 && (prev_lower || (prev_upper && next_lower)) {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else if c.is_ascii_alphanumeric() {
            out.push(*c);
        } else {
            out.push('_');
        }
    }
    format!("{out}.dart")
}

/// The macros class: `<Client>Macros`, unique among the package's
/// classes.
pub(crate) fn macros_class(g: &Gen<'_, '_>) -> String {
    let mut taken: BTreeSet<String> = g.names.resources.iter().map(|r| r.class.clone()).collect();
    taken.extend(g.names.namespace_classes.iter().cloned());
    taken.insert(g.names.client_class.clone());
    taken.extend(g.names.types.values().cloned());
    taken.extend(crate::TAKEN_TYPES.iter().map(|s| s.to_string()));
    unique(format!("{}Macros", g.names.client_class), &taken)
}

/// `base`, else `base2`, `base3`, ... the first not in `taken`.
pub(crate) fn unique(base: String, taken: &BTreeSet<String>) -> String {
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}{n}"))
        .find(|c| !taken.contains(c))
        .unwrap_or(base)
}

/// `word` with its first letter in upper case.
pub(crate) fn upper_first(word: &str) -> String {
    let mut c = word.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Whether the API has an OAuth2 scheme with an authorization-code flow.
pub(crate) fn has_code_flow(g: &Gen<'_, '_>) -> bool {
    g.plan
        .ir
        .auth
        .iter()
        .any(|s| matches!(s, AuthScheme::OAuth2 { .. }) && s.authorization_code().is_some())
}

// --------------------------------------------------------------- imports

/// A library's text: the header, an optional doc comment with `library;`,
/// the imports the body uses (`dart:typed_data`, the runtime, the
/// relative `locals`, `wire.dart` at `wire`, the model libraries by
/// prefix, with `models` their directory), then the body.
pub(crate) struct LibraryText<'s> {
    pub doc: Option<&'s str>,
    /// Relative URIs of other generated libraries, imported as they are.
    pub locals: Vec<String>,
    /// The relative URI of `wire.dart`.
    pub wire: &'s str,
    /// The relative directory of the model libraries (`models/`).
    pub models: &'s str,
    /// The runtime names to import, instead of those the body mentions
    /// (a body that is mostly data, such as the descriptor document).
    pub runtime: Option<&'s [&'s str]>,
}

impl LibraryText<'_> {
    pub fn finish(&self, g: &Gen<'_, '_>, body: &str) -> String {
        let code: String = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut out = String::new();
        out.push_str(&g.header);
        out.push_str("\n\n");
        if let Some(doc) = self.doc {
            let mut w = Code::new();
            w.doc(doc);
            w.line("library;");
            out.push_str(&w.finish());
            out.push('\n');
        }
        let scan = self.runtime.is_none();
        if scan && contains_word(&code, "Uint8List") {
            out.push_str(&directive(
                "import",
                "dart:typed_data",
                "show",
                &["Uint8List".to_string()],
            ));
            out.push_str("\n\n");
        }
        let runtime: Vec<String> = match self.runtime {
            Some(names) => names.iter().map(|n| n.to_string()).collect(),
            None => RUNTIME_NAMES
                .iter()
                .filter(|n| contains_word(&code, n))
                .map(|n| n.to_string())
                .collect(),
        };
        if !runtime.is_empty() {
            out.push_str(&directive(
                "import",
                &format!("package:{RUNTIME_PACKAGE}/{RUNTIME_PACKAGE}.dart"),
                "show",
                &runtime,
            ));
            out.push_str("\n\n");
        }
        let mut relative: Vec<String> = vec![];
        for l in &self.locals {
            relative.push(directive("import", l, "", &[]));
        }
        if scan && (code.contains("Wire.") || contains_word(&code, "Decoder")) {
            relative.push(directive("import", self.wire, "", &[]));
        }
        for ns in &g.plan.namespaces {
            let prefix = g.ns_prefix(&ns.name);
            if scan && code.contains(&format!("{prefix}.")) {
                let uri = format!("{}{}.dart", self.models, g.ns_module(&ns.name));
                relative.push(format!(
                    "{} as {prefix};",
                    directive("import", &uri, "", &[]).trim_end_matches(';')
                ));
            }
        }
        if !relative.is_empty() {
            out.push_str(&relative.join("\n"));
            out.push_str("\n\n");
        }
        out.push_str(body.trim_start_matches('\n'));
        out
    }
}
