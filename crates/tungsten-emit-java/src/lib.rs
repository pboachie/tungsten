// SPDX-License-Identifier: AGPL-3.0-only
//! The `java` target: a Java SDK over the Java runtime
//! (`io.github.pboachie.tungsten:tungsten-runtime`), built on the shared SDK
//! plan, names and descriptor document of [`tungsten_emit::sdk`].
//!
//! Output (package `<package>` from the options):
//!
//! - `pom.xml`, `README.md`;
//! - `<package>/<Api>Client.java` (resource or namespace accessors,
//!   `dispatch()`, `macros()`, `oauth()` when a scheme has an
//!   authorization-code flow), `Ops.java` (the descriptor document and one
//!   constant per operation), `Dispatch.java` (the runtime's `Dispatch`:
//!   operations by id and macros by name with JSON arguments, through the
//!   typed methods), `Macros.java`;
//! - `<package>/models/...`: records (four-state presence with `Patch<T>`,
//!   additional members kept), enums with a strict decoder, sealed interfaces
//!   with record variants for unions (by tag, else the first variant that
//!   decodes, in plan order);
//! - `<package>/resources/...`: one class per resource with a request record
//!   and a typed method per operation, `preview<Op>`, `<op>Pages`,
//!   `<op>Stream`; one class per namespace in multi-namespace APIs;
//! - `src/main/resources/<package path>/tungsten-descriptors.json`;
//! - `<package>/custom/`, created once.
//!
//! Arguments objects use the TypeScript arguments layout, so
//! `failed_parameter` and contract parity match the TypeScript SDK.
//! Diagnostics: TG1010 to TG1015 (planning/05 "Java SDK").

mod java;
pub mod options;
mod package;
mod resources;
mod types;

use std::collections::{BTreeMap, BTreeSet};

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::sdk::{
    self, DocumentOptions, NameMap, Namer, NamerOptions, Scope, SdkPlan, SuccessPlan, TypeKind,
    UnionPlan,
};
use tungsten_emit::{Emitter, FileSet, TargetConfig, header_text};
use tungsten_ir::naming::Target;
use tungsten_ir::{AuthScheme, Ir};

pub use options::Options;

/// Internals for the private test harness (`testing` feature).
#[cfg(feature = "testing")]
pub mod __testing {
    use super::*;

    /// The client class of the SDK of `ir`.
    pub fn client_class(ir: &Ir) -> String {
        let (plan, _) = SdkPlan::new(ir);
        Namer::new(&plan)
            .names(Target::Java, &namer_options())
            .client_class
    }

    /// (type id, fully qualified class) of every model class of the SDK of
    /// `ir` in `package`.
    pub fn model_classes(ir: &Ir, package: &str) -> Vec<(String, String)> {
        let opts = Options {
            group_id: "gate".into(),
            artifact_id: "gate".into(),
            package: package.into(),
            version: "0.1.0".into(),
            runtime: options::DEFAULT_RUNTIME.into(),
            runtime_path: None,
        };
        let (plan, _) = SdkPlan::new(ir);
        let names = Namer::new(&plan).names(Target::Java, &namer_options());
        let ctx = context(ir, &plan, &names, &opts);
        ir.types
            .types
            .iter()
            .filter(|t| ctx.has_class(&t.id))
            .map(|t| (t.id.0.clone(), ctx.class_ref(&t.id, types::From::Elsewhere)))
            .collect()
    }
}

/// Emits the `java` target.
#[derive(Debug, Clone, Copy, Default)]
pub struct JavaEmitter;

impl Emitter for JavaEmitter {
    fn id(&self) -> &'static str {
        "java"
    }

    fn supports(&self, ir: &Ir) -> Diagnostics {
        let (plan, _) = SdkPlan::new(ir);
        notes(ir, &plan)
    }

    fn emit(&self, ir: &Ir, cfg: &TargetConfig, out: &mut FileSet) -> Diagnostics {
        let (opts, mut diags) = Options::resolve(ir, cfg);
        let (files, more) = generate(ir, &opts);
        diags.extend(more);
        for (path, text) in files {
            if let Err(e) = out.add(path, text) {
                diags.push(Diagnostic::error("TG0701", format!("java target: {e}")));
            }
        }
        diags
    }
}

/// The names of the runtime's public types, java.util's types generated code
/// imports, and the nested `Builder`: no model may take one, since files
/// import both the runtime and their models.
const TAKEN_TYPES: &[&str] = &[
    "ApiDescriptor",
    "AuthCredential",
    "Binary",
    "Builder",
    "CallOptions",
    "Category",
    "ClientCore",
    "ClientOptions",
    "Confirm",
    "Decode",
    "Descriptors",
    "Diagnostic",
    "Dispatch",
    "EventStream",
    "Folded",
    "IdempotencyStore",
    "Json",
    "JsonObject",
    "List",
    "MacroDescriptor",
    "Macros",
    "Map",
    "Middleware",
    "OAuthFlow",
    "OperationDescriptor",
    "Ops",
    "Page",
    "Pages",
    "Patch",
    "Polled",
    "PreviewResult",
    "ResponseMeta",
    "Result",
    "RetryOptions",
    "Retryable",
    "RuntimeIdentity",
    "Safety",
    "Schema",
    "StreamEvent",
    "TokenStore",
    "Typed",
    "TungstenException",
    "Verification",
];

/// The `java.lang` types: a model of the same name would be ambiguous in
/// files that import the models with a wildcard.
const JAVA_LANG: &[&str] = &[
    "AutoCloseable",
    "Boolean",
    "Byte",
    "CharSequence",
    "Character",
    "Class",
    "ClassLoader",
    "Cloneable",
    "Comparable",
    "Deprecated",
    "Double",
    "Enum",
    "Error",
    "Exception",
    "Float",
    "FunctionalInterface",
    "Integer",
    "Iterable",
    "Long",
    "Math",
    "Module",
    "Number",
    "Object",
    "Override",
    "Package",
    "Process",
    "Record",
    "Runnable",
    "Runtime",
    "RuntimeException",
    "SafeVarargs",
    "Short",
    "StrictMath",
    "String",
    "StringBuilder",
    "SuppressWarnings",
    "System",
    "Thread",
    "Throwable",
    "Void",
];

/// Member names every Java object (or record) already has.
const OBJECT_MEMBERS: &[&str] = &[
    "clone",
    "equals",
    "finalize",
    "getClass",
    "hashCode",
    "notify",
    "notifyAll",
    "toString",
    "wait",
];

/// The namer options of the Java target.
pub fn namer_options() -> NamerOptions {
    let mut reserved: BTreeMap<Scope, Vec<String>> = BTreeMap::new();
    let types: Vec<String> = TAKEN_TYPES
        .iter()
        .chain(JAVA_LANG)
        .map(|s| s.to_string())
        .collect();
    reserved.insert(Scope::Types, types.clone());
    reserved.insert(Scope::UnionVariants, vec!["Builder".into()]);
    reserved.insert(Scope::Classes, types);
    let mut record_members: Vec<String> = OBJECT_MEMBERS.iter().map(|s| s.to_string()).collect();
    record_members
        .extend(["additionalProperties", "builder", "fromJson", "toJson"].map(String::from));
    reserved.insert(Scope::Fields, record_members.clone());
    reserved.insert(Scope::Arguments, record_members);
    let mut members: Vec<String> = OBJECT_MEMBERS.iter().map(|s| s.to_string()).collect();
    members.push("core".into());
    reserved.insert(Scope::Members, members.clone());
    let mut client = members;
    client.extend(["dispatch", "macros", "oauth"].map(String::from));
    reserved.insert(Scope::Client, client);
    reserved.insert(
        Scope::Macros,
        OBJECT_MEMBERS.iter().map(|s| s.to_string()).collect(),
    );
    NamerOptions {
        reserved,
        preview_members: true,
        pages_members: true,
        stream_members: true,
    }
}

/// What every part of the generator reads.
pub(crate) struct Ctx<'a> {
    pub ir: &'a Ir,
    pub plan: &'a SdkPlan<'a>,
    pub names: &'a NameMap,
    pub opts: &'a Options,
    /// One model package (`<package>.models`) for every type.
    pub single_model_package: bool,
    /// Simple names a resources file cannot use for a model: the resource and
    /// namespace classes of its own package.
    pub resources_taken: BTreeSet<String>,
}

/// The first line of the generated header, for files whose comments are not
/// `//` (the pom).
pub(crate) fn header_line(ir: &Ir) -> String {
    header_text(ir).replace("--", "- -").replace('\n', " ")
}

/// The names of a plan for Java: the Namer's, with every argument and macro
/// input field named by its arguments layout key in the descriptor document
/// (the Java SDK keeps the TypeScript layout).
fn document_names(names: &NameMap, plan: &SdkPlan<'_>) -> NameMap {
    let mut doc = names.clone();
    for (i, op) in plan.operations.iter().enumerate() {
        if let Some(o) = doc.operations.get_mut(i) {
            o.arguments = op.arguments.iter().map(|a| a.key.clone()).collect();
        }
    }
    for (k, m) in plan.macros.iter().enumerate() {
        if let Some(n) = doc.macros.get_mut(k) {
            n.input = m.input.iter().map(|f| f.key.clone()).collect();
        }
    }
    doc
}

fn context<'a>(
    ir: &'a Ir,
    plan: &'a SdkPlan<'a>,
    names: &'a NameMap,
    opts: &'a Options,
) -> Ctx<'a> {
    let mut resources_taken: BTreeSet<String> =
        names.resources.iter().map(|r| r.class.clone()).collect();
    resources_taken.extend(names.namespace_classes.iter().cloned());
    let model_namespaces: BTreeSet<&str> =
        plan.types.values().map(|t| t.namespace.as_str()).collect();
    Ctx {
        ir,
        plan,
        names,
        opts,
        single_model_package: model_namespaces.len() <= 1,
        resources_taken,
    }
}

/// Every file of the SDK, and the generator's diagnostics.
pub fn generate(ir: &Ir, opts: &Options) -> (Vec<(String, String)>, Diagnostics) {
    let (plan, _) = SdkPlan::new(ir);
    let names = Namer::new(&plan).names(Target::Java, &namer_options());
    let ctx = context(ir, &plan, &names, opts);
    let mut files: Vec<(String, String)> = vec![];
    files.push(("pom.xml".into(), ctx.pom()));
    files.push(("README.md".into(), ctx.readme()));
    files.extend(ctx.models());
    let infos = ctx.op_infos();
    files.extend(ctx.resources(&infos));
    files.extend(ctx.root(&infos));
    let document = sdk::document_value(
        &plan,
        &document_names(&names, &plan),
        &DocumentOptions {
            version: opts.version.clone(),
        },
    );
    let mut text = serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".into());
    text.push('\n');
    files.push((
        format!(
            "src/main/resources/{}/tungsten-descriptors.json",
            opts.package.replace('.', "/")
        ),
        text,
    ));
    files.push((
        format!(
            "src/main/java/{}/custom/README.md",
            opts.package.replace('.', "/")
        ),
        ctx.custom_readme(),
    ));
    files.sort();
    let diags = notes(ir, ctx.plan);
    (files, diags)
}

/// TG1011 to TG1015 for the IR.
fn notes(ir: &Ir, plan: &SdkPlan<'_>) -> Diagnostics {
    let mut diags = Diagnostics::new();
    for issue in &plan.macro_issues {
        diags.push(
            Diagnostic::warning(
                "TG1012",
                format!(
                    "macro `{}` is not in the canonical form ({}); the Java SDK leaves it out",
                    issue.name, issue.reason
                ),
            )
            .at("agent.yml", issue.pointer.clone(), None),
        );
    }
    for s in &ir.auth {
        if let AuthScheme::OpenIdConnect { name, .. } = s {
            diags.push(Diagnostic::info(
                "TG1013",
                format!("auth scheme `{name}` (OpenID Connect) is sent as a bearer token; discovery is not supported"),
            ));
        }
    }
    for op in &plan.operations {
        if let SuccessPlan::ByStatus(_) = op.success {
            diags.push(Diagnostic::info(
                "TG1014",
                format!(
                    "operation `{}` answers different bodies by status: its typed result is the runtime's JSON value",
                    op.op.id.0
                ),
            ));
        }
    }
    for t in plan.types.values() {
        match &t.kind {
            TypeKind::Union(UnionPlan::Untagged { .. }) => {
                if let Some(tungsten_ir::Shape::Union(u)) = ir.types.get(&t.id).map(|n| &n.shape)
                    && u.discriminator.is_some()
                {
                    diags.push(Diagnostic::warning(
                        "TG1015",
                        format!(
                            "union `{}` has a discriminator, but a variant has no tag value or is not a named type: its variants are tried in order",
                            t.id.0
                        ),
                    ));
                }
            }
            TypeKind::AllOf(sdk::AllOfPlan::Intersection) => diags.push(Diagnostic::warning(
                "TG1011",
                format!("type `{}` (an allOf that cannot be merged) has no Java type: the runtime's JSON value is used", t.id.0),
            )),
            TypeKind::Enum(sdk::EnumPlan::Mixed { .. }) => diags.push(Diagnostic::warning(
                "TG1011",
                format!("enum `{}` mixes JSON types and has no Java type: the runtime's JSON value is used", t.id.0),
            )),
            _ => {}
        }
    }
    diags
}
