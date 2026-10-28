// SPDX-License-Identifier: AGPL-3.0-only
//! The `fsharp` target: an F# SDK over the .NET runtime (`Tungsten.Runtime`,
//! net10.0) and its F# layer (`Tungsten.Runtime.FSharp`), built on the shared
//! SDK plan, names and descriptor document of [`tungsten_emit::sdk`].
//!
//! Output (root namespace `<namespace>` from the options):
//!
//! - `<PackageId>.fsproj`: net10.0, warnings as errors, the descriptor
//!   document as an embedded resource, `FSharp.Core` pinned to
//!   [`options::FSHARP_CORE`] (the .NET SDK's bundled library pack, so a
//!   restore needs no package source), a reference to the F# layer (the
//!   package at `runtime`, or the projects of `runtime_path` and
//!   `csharp_runtime_path`) and an explicit compile order
//!   ([`compile_order`]); `README.md`; `tungsten-descriptors.json`;
//! - `Models/...`: one file per model group ([`typeref`]);
//! - `Resources/...`, `Macros.fs`, `Dispatch.fs`, `Client.fs`;
//! - `custom/`, created once; its `.fs` files compile after the generated
//!   ones.
//!
//! F# types of type references and the namer setup live in [`typeref`].
//! Names use the C# naming profile ([`typeref::namer_options`] says why);
//! the descriptor document names `fsharp` as its target. Arguments objects
//! use the TypeScript arguments layout, as in the C# SDK, so
//! `failed_parameter` and contract parity match the TypeScript SDK.
//! Diagnostics: TG1080 to TG1085 (planning/05 "F# SDK"): TG1080 an invalid
//! option, TG1081 a schema without an F# type (an `allOf` without
//! members), TG1082 a macro outside the canonical form, TG1083 OpenID
//! Connect sent as a bearer token, TG1084 (reserved: success bodies that
//! differ by status used to be `JsonNode`; they are a union of the answers
//! now), TG1085 a discriminated variant without a tag value. TG1086 to
//! TG1089 are unused.

mod client;
pub mod fs;
pub mod options;
mod resources;
pub mod typeref;
pub mod types;

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::sdk::{
    self, ClientPlan, DocumentOptions, MemberKind, NameMap, Namer, SdkPlan, TypeKind, UnionPlan,
};
use tungsten_emit::{CommentStyle, Emitter, FileSet, TargetConfig, Writer, header, header_text};
use tungsten_ir::naming::Target;
use tungsten_ir::{AuthScheme, Ir};

pub use options::Options;
pub use typeref::namer_options;

/// Internals for the private test harness (`testing` feature).
#[cfg(feature = "testing")]
pub mod __testing {
    use super::*;
    use tungsten_ir::TypeId;

    fn with_ctx<T>(ir: &Ir, namespace: &str, f: impl FnOnce(&Ctx<'_>) -> T) -> T {
        let opts = Options {
            package_id: namespace.into(),
            namespace: namespace.into(),
            version: "0.1.0".into(),
            runtime: options::DEFAULT_RUNTIME.into(),
            runtime_path: None,
            csharp_runtime_path: None,
        };
        let (plan, _) = SdkPlan::new(ir);
        let names = Namer::new(&plan).names(Target::CSharp, &namer_options());
        let ctx = context(ir, &plan, &names, &opts);
        f(&ctx)
    }

    /// The client class of the SDK of `ir`.
    pub fn client_class(ir: &Ir) -> String {
        let (plan, _) = SdkPlan::new(ir);
        Namer::new(&plan)
            .names(Target::CSharp, &namer_options())
            .client_class
    }

    /// (type id, fully qualified F# type) of every model of the SDK of
    /// `ir` under the root namespace `namespace`, in IR order.
    pub fn model_classes(ir: &Ir, namespace: &str) -> Vec<(String, String)> {
        with_ctx(ir, namespace, |ctx| {
            ir.types
                .types
                .iter()
                .filter(|t| ctx.has_model(&t.id))
                .map(|t| {
                    let name = ctx.model_ref(&t.id, None);
                    (
                        t.id.0.clone(),
                        name.trim_start_matches("global.").to_string(),
                    )
                })
                .collect()
        })
    }

    /// Every model group of the SDK of `ir` under the root namespace
    /// `namespace`, in compile order: (file, F# namespace, type ids,
    /// recursive).
    pub fn model_groups(ir: &Ir, namespace: &str) -> Vec<(String, String, Vec<String>, bool)> {
        with_ctx(ir, namespace, |ctx| {
            ctx.model_groups()
                .into_iter()
                .map(|g| {
                    (
                        g.path,
                        g.namespace,
                        g.types.into_iter().map(|t| t.0).collect(),
                        g.recursive,
                    )
                })
                .collect()
        })
    }

    /// (wire name, F# type) of every field of the record `type_id`, as
    /// written in the record's own namespace.
    pub fn field_types(ir: &Ir, namespace: &str, type_id: &str) -> Vec<(String, String)> {
        with_ctx(ir, namespace, |ctx| {
            let id = TypeId(type_id.into());
            let from = ctx.type_ns(&id);
            let Some(tungsten_ir::Shape::Record { fields, .. }) =
                ir.types.get(&id).map(|t| &t.shape)
            else {
                return vec![];
            };
            fields
                .iter()
                .map(|f| {
                    let ft = ctx.ft(&f.ty, Some(&from));
                    (f.wire_name.clone(), typeref::field_ty(&ft, f.presence))
                })
                .collect()
        })
    }

    /// The F# compile order of these source paths for the SDK of `ir`.
    pub fn compile_order(ir: &Ir, namespace: &str, sources: &[&str]) -> Vec<String> {
        with_ctx(ir, namespace, |ctx| {
            let sources: Vec<String> = sources.iter().map(|s| s.to_string()).collect();
            super::compile_order(ctx, &sources)
        })
    }
}

/// Emits the `fsharp` target.
#[derive(Debug, Clone, Copy, Default)]
pub struct FSharpEmitter;

impl Emitter for FSharpEmitter {
    fn id(&self) -> &'static str {
        "fsharp"
    }

    fn supports(&self, ir: &Ir) -> Diagnostics {
        let (plan, _) = SdkPlan::new(ir);
        notes(ir, &plan)
    }

    fn emit(&self, ir: &Ir, cfg: &TargetConfig, out: &mut FileSet) -> Diagnostics {
        let (opts, mut diags) = Options::resolve(ir, cfg);
        for (path, text) in generate(ir, &opts) {
            if let Err(e) = out.add(path, text) {
                diags.push(Diagnostic::error("TG0701", format!("fsharp target: {e}")));
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
        for (path, text) in generate_prepared(ir, plan, &opts) {
            if let Err(e) = out.add(path, text) {
                diags.push(Diagnostic::error("TG0701", format!("fsharp target: {e}")));
            }
        }
        diags
    }
}

/// What every part of the generator reads: the IR, its plan and F# names,
/// and the resolved options. Built once per [`generate`].
#[derive(Debug)]
pub struct Ctx<'a> {
    pub ir: &'a Ir,
    pub plan: &'a SdkPlan<'a>,
    pub names: &'a NameMap,
    pub opts: &'a Options,
    /// One model namespace (`<namespace>.Models`) for every type.
    pub single_model_namespace: bool,
    /// The F# name of every model ([`Ctx::placed_names`]).
    pub model_names: std::collections::BTreeMap<tungsten_ir::TypeId, String>,
}

impl Ctx<'_> {
    /// A writer for an F# file, with the generated header.
    pub fn file_writer(&self) -> Writer {
        let mut w = Writer::new("    ");
        w.line(header(CommentStyle::DoubleSlash, self.ir));
        w.blank();
        w
    }

    /// The expression of each resource from the client, by resource index
    /// (`Pets`, `Store.Orders`).
    pub fn resource_paths(&self) -> Vec<String> {
        let mut paths = vec![String::new(); self.plan.resources.len()];
        match &self.plan.client {
            ClientPlan::Resources(top) => {
                for (r, m) in top.iter().zip(&self.names.client_members) {
                    paths[*r] = m.clone();
                }
            }
            ClientPlan::Namespaces(namespaces) => {
                for (k, ns) in namespaces.iter().enumerate() {
                    let member = &self.names.client_members[k];
                    for (r, m) in ns.resources.iter().zip(&self.names.namespace_members[k]) {
                        paths[*r] = format!("{member}.{m}");
                    }
                }
            }
        }
        // Resources come depth first: every parent before its children.
        for (r, names) in self.names.resources.iter().enumerate() {
            for m in &names.members {
                if let MemberKind::Child(c) = m.kind {
                    paths[c] = format!("{}.{}", paths[r], m.name);
                }
            }
        }
        paths
    }

    /// The file of a resource class, or of a namespace class.
    pub fn class_file(class: &str) -> String {
        format!("Resources/{class}.fs")
    }

    /// `<PackageId>.fsproj` compiling `sources` in this order.
    pub fn fsproj(&self, sources: &[String]) -> String {
        let header = header_text(self.ir).replace("--", "- -");
        let mut out = String::new();
        for line in header.lines() {
            out.push_str(&format!("<!-- {line} -->\n"));
        }
        let mut compile = String::new();
        for s in sources {
            compile.push_str(&format!("    <Compile Include=\"{s}\" />\n"));
        }
        compile.push_str("    <Compile Include=\"custom/**/*.fs\" />\n");
        let references = match &self.opts.runtime_path {
            Some(layer) => {
                let layer = layer.trim_end_matches('/');
                let csharp = self.opts.csharp_dir().unwrap_or_default();
                // The F# layer finds the .NET runtime next to itself unless
                // told otherwise; the path is made absolute (relative to this
                // project) because the layer's project resolves it.
                let properties = match &self.opts.csharp_runtime_path {
                    Some(_) => format!(
                        " AdditionalProperties=\"TungstenRuntime=$([MSBuild]::NormalizePath('$(MSBuildThisFileDirectory)', '{csharp}/Tungsten.Runtime.csproj'))\""
                    ),
                    None => String::new(),
                };
                format!(
                    "    <ProjectReference Include=\"{layer}/Tungsten.Runtime.FSharp.fsproj\"{properties} />\n    \
                     <ProjectReference Include=\"{csharp}/Tungsten.Runtime.csproj\" />\n"
                )
            }
            None => format!(
                "    <PackageReference Include=\"Tungsten.Runtime.FSharp\" Version=\"{}\" />\n",
                self.opts.runtime
            ),
        };
        out.push_str(&format!(
            r#"<Project Sdk="Microsoft.NET.Sdk">

  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <TreatWarningsAsErrors>true</TreatWarningsAsErrors>
    <GenerateDocumentationFile>true</GenerateDocumentationFile>
    <RootNamespace>{ns}</RootNamespace>
    <AssemblyName>{pkg}</AssemblyName>
    <PackageId>{pkg}</PackageId>
    <Version>{version}</Version>
    <DisableImplicitFSharpCoreReference>true</DisableImplicitFSharpCoreReference>
  </PropertyGroup>

  <!-- F# compiles in this order: models, resources, then the client. -->
  <ItemGroup>
{compile}  </ItemGroup>

  <ItemGroup>
    <EmbeddedResource Include="tungsten-descriptors.json" LogicalName="tungsten-descriptors.json" />
  </ItemGroup>

  <ItemGroup>
    <PackageReference Include="FSharp.Core" Version="{core}" />
{references}  </ItemGroup>

</Project>
"#,
            ns = self.opts.namespace,
            pkg = self.opts.package_id,
            version = self.opts.version,
            core = options::FSHARP_CORE,
        ));
        out
    }

    /// `README.md`.
    pub fn readme(&self) -> String {
        let client = &self.names.client_class;
        let title = &self.ir.api.title;
        let mut out = format!(
            "# {pkg}\n\nThe F# SDK of the {title} API, generated by tungsten {v}. Do not edit the generated files; \
             put your own code in `custom/`.\n\n## Install\n\n```sh\ndotnet add package {pkg} --version {version}\n```\n\n\
             It depends on `Tungsten.Runtime.FSharp`, the F# layer over the .NET runtime `Tungsten.Runtime` \
             (net10.0), which carries validation, auth, idempotency, confirmation, retries, pagination, \
             streams and the error envelope.\n\n## Use\n\n```fsharp\nopen {ns}\nopen Tungsten.Runtime\n\n\
             let client = {client}(ClientOptions(BaseUrl = \"https://api.example.com\"))\n```\n\n\
             Every operation is a task-based method returning F# `Result<'T, Diagnostic>`: `Ok` with the \
             typed value, or `Error` with the diagnostic envelope (category, remediation, retryable). \
             Optional members are `'T option`; optional members that may also be null are \
             `Patch<'T>` (`Absent`, `Null` or `Value`). Destructive operations need a confirmation \
             token from their `Preview...Async` method, passed as `CallOptions(Confirm = \
             Confirm.Token token)`.\n",
            pkg = self.opts.package_id,
            ns = self.opts.namespace,
            v = self.ir.generator.tungsten_version,
            version = self.opts.version,
        );
        if !self.plan.operations.is_empty() {
            out.push_str("\n## Operations\n\n| Operation | Method |\n|---|---|\n");
            let paths = self.resource_paths();
            for (i, p) in self.plan.operations.iter().enumerate() {
                let member = self.names.resources[p.resource]
                    .members
                    .iter()
                    .find(|m| m.kind == MemberKind::Operation(i))
                    .map(|m| m.name.clone())
                    .unwrap_or_default();
                out.push_str(&format!(
                    "| `{}` | `{}.{member}Async` |\n",
                    p.op.id.0, paths[p.resource]
                ));
            }
        }
        out
    }

    /// `custom/README.md`.
    pub fn custom_readme(&self) -> String {
        format!(
            "# custom\n\nFiles in this directory are yours: tungsten creates this one once and never \
             overwrites or removes anything here. The project compiles every `.fs` file in it after \
             the generated files, in path order, so helpers can extend the generated client, for \
             example with a type extension `type {} with member ...` in a module that opens the \
             `{}` namespace.\n",
            self.names.client_class, self.opts.namespace
        )
    }
}

/// The names of a plan for F#: the Namer's, with every argument and macro
/// input field named by its arguments layout key in the descriptor document
/// (the F# SDK keeps the TypeScript layout).
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
    let mut ctx = Ctx {
        ir,
        plan,
        names,
        opts,
        single_model_namespace: model_namespaces.len() <= 1,
        model_names: std::collections::BTreeMap::new(),
    };
    ctx.model_names = ctx.placed_names();
    ctx
}

/// The descriptor document of the SDK, `fsharp` as its target.
fn descriptor_document(plan: &SdkPlan<'_>, names: &NameMap, opts: &Options) -> String {
    let mut document = sdk::document_value(
        plan,
        &document_names(names, plan),
        &DocumentOptions {
            version: opts.version.clone(),
        },
    );
    if let Some(map) = document.as_object_mut() {
        map.insert("target".into(), serde_json::Value::String("fsharp".into()));
    }
    let mut text = serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".into());
    text.push('\n');
    text
}

/// Every file of the SDK (its notes come from `supports`).
pub fn generate(ir: &Ir, opts: &Options) -> Vec<(String, String)> {
    let (plan, _) = SdkPlan::new(ir);
    generate_prepared(ir, &plan, opts)
}

fn generate_prepared(ir: &Ir, plan: &SdkPlan<'_>, opts: &Options) -> Vec<(String, String)> {
    let names = Namer::new(plan).names(Target::CSharp, &namer_options());
    let ctx = context(ir, plan, &names, opts);
    // The F# sources of the SDK, one part per generator module, in any
    // order: [`compile_order`] places them.
    let parts: Vec<Vec<(String, String)>> =
        vec![ctx.models(), ctx.resource_files(), ctx.client_files()];
    let sources: Vec<(String, String)> = parts.into_iter().flatten().collect();
    let paths: Vec<String> = sources.iter().map(|(p, _)| p.clone()).collect();
    let order = compile_order(&ctx, &paths);
    let mut files = sources;
    files.push((format!("{}.fsproj", opts.package_id), ctx.fsproj(&order)));
    files.push(("README.md".into(), ctx.readme()));
    files.push((
        "tungsten-descriptors.json".into(),
        descriptor_document(plan, &names, opts),
    ));
    files.push(("custom/README.md".into(), ctx.custom_readme()));
    files.sort();
    files
}

/// The F# compile order of the generated sources (`.fs` paths; others are
/// left out). F# sees only what an earlier file declares, so:
///
/// 1. `Models/`: the model groups in [`Ctx::model_groups`] order
///    (dependencies first, a recursive group in one file), then any other
///    model file in path order;
/// 2. every other file outside `Models/` and `Resources/` that is not one
///    of the three below (support modules), in path order;
/// 3. `Resources/`: the resource classes children first (the reverse of
///    the plan's depth-first order, so a parent sees its children's
///    classes), then the namespace classes in IR order, then any other
///    resource file in path order;
/// 4. `Macros.fs`, `Dispatch.fs`, `Client.fs`.
///
/// `custom/` is never listed here: the project compiles it last.
pub fn compile_order(ctx: &Ctx<'_>, sources: &[String]) -> Vec<String> {
    const LAST: [&str; 3] = ["Macros.fs", "Dispatch.fs", "Client.fs"];
    let models: Vec<String> = ctx.model_groups().into_iter().map(|g| g.path).collect();
    let mut classes: Vec<String> = ctx
        .names
        .resources
        .iter()
        .rev()
        .map(|r| Ctx::class_file(&r.class))
        .collect();
    classes.extend(
        ctx.names
            .namespace_classes
            .iter()
            .map(|c| Ctx::class_file(c)),
    );
    let rank = |path: &str| -> (usize, usize, String) {
        let at = |list: &[String]| list.iter().position(|p| p == path).unwrap_or(usize::MAX);
        if path.starts_with("Models/") {
            (0, at(&models), path.to_string())
        } else if path.starts_with("Resources/") {
            (2, at(&classes), path.to_string())
        } else if let Some(k) = LAST.iter().position(|p| *p == path) {
            (3 + k, 0, path.to_string())
        } else {
            (1, 0, path.to_string())
        }
    };
    let mut out: Vec<String> = sources
        .iter()
        .filter(|p| p.ends_with(".fs") && !p.starts_with("custom/"))
        .cloned()
        .collect();
    out.sort_by_cached_key(|p| rank(p));
    out.dedup();
    out
}

/// TG1081 to TG1085 for the IR.
fn notes(ir: &Ir, plan: &SdkPlan<'_>) -> Diagnostics {
    let mut diags = Diagnostics::new();
    for issue in &plan.macro_issues {
        diags.push(
            Diagnostic::warning(
                "TG1082",
                format!(
                    "macro `{}` is not in the canonical form ({}); the F# SDK leaves it out",
                    issue.name, issue.reason
                ),
            )
            .at("agent.yml", issue.pointer.clone(), None),
        );
    }
    for s in &ir.auth {
        if let AuthScheme::OpenIdConnect { name, .. } = s {
            diags.push(Diagnostic::info(
                "TG1083",
                format!(
                    "auth scheme `{name}` (OpenID Connect) is sent as a bearer token; discovery is not supported"
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
                        "TG1085",
                        format!(
                            "union `{}` has a discriminator, but a variant has no tag value or is not a named type: its variants are tried in order",
                            t.id.0
                        ),
                    ));
                }
            }
            TypeKind::AllOf(sdk::AllOfPlan::Intersection)
                if matches!(
                    ir.types.get(&t.id).map(|n| &n.shape),
                    Some(tungsten_ir::Shape::Intersection { members }) if members.is_empty()
                ) =>
            {
                diags.push(Diagnostic::warning(
                    "TG1081",
                    format!(
                        "type `{}` (an allOf without members) has no F# type: the runtime's JsonNode is used",
                        t.id.0
                    ),
                ))
            }
            _ => {}
        }
    }
    diags
}
