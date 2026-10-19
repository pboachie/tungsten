// SPDX-License-Identifier: AGPL-3.0-only
//! The `csharp` target: a C# (.NET) SDK over the .NET runtime
//! (`Tungsten.Runtime`, net10.0), built on the shared SDK plan, names and
//! descriptor document of [`tungsten_emit::sdk`].
//!
//! Output (root namespace `<namespace>` from the options):
//!
//! - `<PackageId>.csproj` (net10.0, nullable, warnings as errors, the
//!   descriptor document as an embedded resource), `README.md`,
//!   `tungsten-descriptors.json`;
//! - `Client.cs` (resource or namespace properties, `Macros`, `OAuth` when a
//!   scheme has an authorization-code flow), `Dispatch.cs` (the runtime's
//!   `IDispatch`: operations by id and macros by name with JSON arguments,
//!   through the typed methods), `Macros.cs`, `Ops.cs`;
//! - `Models/...`: sealed records (four-state presence with `Patch<T>`,
//!   additional members kept), enums as readonly record structs with known
//!   values and a strict decoder, unions as abstract records with variant
//!   records (by tag, else the first variant that decodes, in plan order);
//!   every model decodes and encodes itself over the runtime's JSON tree
//!   (no reflection, trimming- and AOT-safe);
//! - `Resources/...`: one class per resource with a request record and the
//!   async methods of each operation; one class per namespace in
//!   multi-namespace APIs;
//! - `custom/`, created once.
//!
//! Arguments objects use the TypeScript arguments layout, so
//! `failed_parameter` and contract parity match the TypeScript SDK.
//! Diagnostics: TG1020 to TG1025 (planning/05 "C# SDK").

mod client;
mod cs;
pub mod options;
mod resources;
mod types;

use std::collections::BTreeMap;

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::sdk::{
    self, DocumentOptions, NameMap, Namer, NamerOptions, Scope, SdkPlan, SuccessPlan, TypeKind,
    UnionPlan,
};
use tungsten_emit::{CommentStyle, Emitter, FileSet, TargetConfig, Writer, header};
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
            .names(Target::CSharp, &namer_options())
            .client_class
    }

    /// (type id, fully qualified class) of every model class of the SDK of
    /// `ir` under the root namespace `namespace`.
    pub fn model_classes(ir: &Ir, namespace: &str) -> Vec<(String, String)> {
        let opts = Options {
            package_id: namespace.into(),
            namespace: namespace.into(),
            version: "0.1.0".into(),
            runtime: options::DEFAULT_RUNTIME.into(),
            runtime_path: None,
        };
        let (plan, _) = SdkPlan::new(ir);
        let names = Namer::new(&plan).names(Target::CSharp, &namer_options());
        let ctx = context(ir, &plan, &names, &opts);
        ir.types
            .types
            .iter()
            .filter(|t| ctx.has_class(&t.id))
            .map(|t| {
                let class = ctx.class_ref(&t.id, None);
                (
                    t.id.0.clone(),
                    class.trim_start_matches("global::").to_string(),
                )
            })
            .collect()
    }
}

/// Emits the `csharp` target.
#[derive(Debug, Clone, Copy, Default)]
pub struct CSharpEmitter;

impl Emitter for CSharpEmitter {
    fn id(&self) -> &'static str {
        "csharp"
    }

    fn supports(&self, ir: &Ir) -> Diagnostics {
        let (plan, _) = SdkPlan::new(ir);
        notes(ir, &plan)
    }

    fn emit(&self, ir: &Ir, cfg: &TargetConfig, out: &mut FileSet) -> Diagnostics {
        let (opts, mut diags) = Options::resolve(ir, cfg);
        let files = generate(ir, &opts);
        for (path, text) in files {
            if let Err(e) = out.add(path, text) {
                diags.push(Diagnostic::error("TG0701", format!("csharp target: {e}")));
            }
        }
        diags
    }

    fn uses_sdk_plan(&self) -> bool {
        true
    }

    fn supports_prepared(&self, ir: &Ir, plan: &SdkPlan<'_>) -> Diagnostics {
        notes(ir, plan)
    }

    fn emit_prepared(
        &self,
        ir: &Ir,
        plan: &SdkPlan<'_>,
        cfg: &TargetConfig,
        out: &mut FileSet,
    ) -> Diagnostics {
        let (opts, mut diags) = Options::resolve(ir, cfg);
        let files = generate_prepared(ir, plan, &opts);
        for (path, text) in files {
            if let Err(e) = out.add(path, text) {
                diags.push(Diagnostic::error("TG0701", format!("csharp target: {e}")));
            }
        }
        diags
    }
}

/// The runtime's and the base library's type names the generated code uses
/// without qualification: no model or class may take one.
const TAKEN_TYPES: &[&str] = &[
    "Action",
    "Binary",
    "CallOptions",
    "CancellationToken",
    "ClientCore",
    "ClientOptions",
    "Confirm",
    "Credential",
    "DecodeException",
    "Decoder",
    "Diagnostic",
    "Dictionary",
    "Encode",
    "EventStream",
    "Func",
    "IAsyncEnumerable",
    "IDispatch",
    "IReadOnlyDictionary",
    "IReadOnlyList",
    "Json",
    "JsonArray",
    "JsonKind",
    "JsonNode",
    "JsonObject",
    "JsonValue",
    "List",
    "MacroDescriptor",
    "Models",
    "NoContent",
    "OAuthFlow",
    "OperationDescriptor",
    "Ops",
    "Page",
    "Patch",
    "PreviewResult",
    "Result",
    "SdkDescriptors",
    "StreamItem",
    "System",
    "Task",
    "Tungsten",
    "Typed",
    "TungstenError",
];

/// Members every C# object already has.
const OBJECT_MEMBERS: &[&str] = &[
    "Equals",
    "Finalize",
    "GetHashCode",
    "GetType",
    "MemberwiseClone",
    "ToString",
];

/// The namer options of the C# target.
pub fn namer_options() -> NamerOptions {
    let mut reserved: BTreeMap<Scope, Vec<String>> = BTreeMap::new();
    let types: Vec<String> = TAKEN_TYPES.iter().map(|s| s.to_string()).collect();
    reserved.insert(Scope::Types, types.clone());
    reserved.insert(Scope::Classes, types);
    reserved.insert(
        Scope::UnionVariants,
        vec!["FromJson".into(), "ToJson".into()],
    );
    reserved.insert(
        Scope::Fields,
        cs::RECORD_MEMBERS.iter().map(|s| s.to_string()).collect(),
    );
    let mut members: Vec<String> = OBJECT_MEMBERS.iter().map(|s| s.to_string()).collect();
    members.push("Core".into());
    reserved.insert(Scope::Members, members.clone());
    let mut client = members;
    client.extend(["Dispatch", "Macros", "OAuth"].map(String::from));
    reserved.insert(Scope::Client, client);
    reserved.insert(Scope::Operations, vec!["Document".into()]);
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
    /// One model namespace (`<namespace>.Models`) for every type.
    pub single_model_namespace: bool,
}

impl Ctx<'_> {
    /// A writer for a C# file, with the generated header.
    pub fn file_writer(&self) -> Writer {
        let mut w = Writer::new("    ");
        w.line(header(CommentStyle::DoubleSlash, self.ir));
        w.blank();
        w
    }
}

/// The names of a plan for C#: the Namer's, with every argument and macro
/// input field named by its arguments layout key in the descriptor document
/// (the C# SDK keeps the TypeScript layout).
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
    let model_namespaces: std::collections::BTreeSet<&str> =
        plan.types.values().map(|t| t.namespace.as_str()).collect();
    Ctx {
        ir,
        plan,
        names,
        opts,
        single_model_namespace: model_namespaces.len() <= 1,
    }
}

/// Every file of the SDK (its notes come from `supports`).
pub fn generate(ir: &Ir, opts: &Options) -> Vec<(String, String)> {
    let (plan, _) = SdkPlan::new(ir);
    generate_prepared(ir, &plan, opts)
}

fn generate_prepared(ir: &Ir, plan: &SdkPlan<'_>, opts: &Options) -> Vec<(String, String)> {
    let names = Namer::new(plan).names(Target::CSharp, &namer_options());
    let ctx = context(ir, plan, &names, opts);
    let mut files: Vec<(String, String)> = vec![];
    files.push((format!("{}.csproj", opts.package_id), ctx.csproj()));
    files.push(("README.md".into(), ctx.readme()));
    files.extend(ctx.models());
    let infos = ctx.op_infos();
    files.extend(ctx.resources(&infos));
    files.extend(ctx.namespace_classes());
    files.push(("Ops.cs".into(), ctx.ops_file(&infos)));
    files.push(("Client.cs".into(), ctx.client_file()));
    files.push(("Dispatch.cs".into(), ctx.dispatch_file(&infos)));
    if !plan.macros.is_empty() {
        files.push(("Macros.cs".into(), ctx.macros_file()));
    }
    let document = sdk::document_value(
        plan,
        &document_names(&names, plan),
        &DocumentOptions {
            version: opts.version.clone(),
        },
    );
    let mut text = serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".into());
    text.push('\n');
    files.push(("tungsten-descriptors.json".into(), text));
    files.push(("custom/README.md".into(), ctx.custom_readme()));
    for (path, text) in &mut files {
        if path.ends_with(".cs") {
            *text = tidy(text);
        }
    }
    files.sort();
    files
}

/// Drops the blank lines that open or close a brace block.
fn tidy(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    for (i, l) in lines.iter().enumerate() {
        if l.is_empty() {
            let after_open = out.last().is_some_and(|p| p.trim_end().ends_with('{'));
            let before_close = lines
                .get(i + 1)
                .is_some_and(|n| n.trim_start().starts_with('}'));
            if after_open || before_close {
                continue;
            }
        }
        out.push(l);
    }
    indent_case_blocks(&out).join("\n")
}

/// Indents the brace block that follows a `case ...:` label one level, as
/// `dotnet format` lays it out.
fn indent_case_blocks(lines: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let mut i = 0;
    while i + 1 < lines.len() {
        let label = lines[i];
        let indent = label.len() - label.trim_start().len();
        let next = lines[i + 1];
        if label.trim_start().starts_with("case ")
            && label.ends_with(':')
            && next.trim() == "{"
            && next.len() - next.trim_start().len() == indent
        {
            let close = format!("{}}}", " ".repeat(indent));
            let mut j = i + 1;
            while j < lines.len() {
                if !lines[j].is_empty() {
                    out[j] = format!("    {}", lines[j]);
                }
                if lines[j] == close {
                    break;
                }
                j += 1;
            }
            i = j;
        }
        i += 1;
    }
    out
}

/// TG1021 to TG1025 for the IR.
fn notes(ir: &Ir, plan: &SdkPlan<'_>) -> Diagnostics {
    let mut diags = Diagnostics::new();
    for issue in &plan.macro_issues {
        diags.push(
            Diagnostic::warning(
                "TG1022",
                format!(
                    "macro `{}` is not in the canonical form ({}); the C# SDK leaves it out",
                    issue.name, issue.reason
                ),
            )
            .at("agent.yml", issue.pointer.clone(), None),
        );
    }
    for s in &ir.auth {
        if let AuthScheme::OpenIdConnect { name, .. } = s {
            diags.push(Diagnostic::info(
                "TG1023",
                format!(
                    "auth scheme `{name}` (OpenID Connect) is sent as a bearer token; discovery is not supported"
                ),
            ));
        }
    }
    for op in &plan.operations {
        if let SuccessPlan::ByStatus(_) = op.success {
            diags.push(Diagnostic::info(
                "TG1024",
                format!(
                    "operation `{}` answers different bodies by status: its typed result is the runtime's JsonNode",
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
                        "TG1025",
                        format!(
                            "union `{}` has a discriminator, but a variant has no tag value or is not a named type: its variants are tried in order",
                            t.id.0
                        ),
                    ));
                }
            }
            TypeKind::AllOf(sdk::AllOfPlan::Intersection) => diags.push(Diagnostic::warning(
                "TG1021",
                format!(
                    "type `{}` (an allOf that cannot be merged) has no C# type: the runtime's JsonNode is used",
                    t.id.0
                ),
            )),
            TypeKind::Enum(sdk::EnumPlan::Mixed { .. }) => diags.push(Diagnostic::warning(
                "TG1021",
                format!(
                    "enum `{}` mixes JSON types and has no C# type: the runtime's JsonNode is used",
                    t.id.0
                ),
            )),
            _ => {}
        }
    }
    diags
}
