// SPDX-License-Identifier: AGPL-3.0-only
//! The SDK half of the `rust` target.
//!
//! Generated crates are mostly data for `tungsten-runtime`
//! (`runtimes/rust/runtime/src/types.rs` is the contract):
//!
//! - `src/descriptors.rs`: one `OperationDescriptor` per callable operation
//!   (with validators that decode the arguments, a response or a page item
//!   with `serde_path_to_error` and run the type's constraint checks) and
//!   the `ApiDescriptor`, built once;
//! - `src/models/<namespace>.rs`: serde types for every IR type (presence:
//!   `T`, `Option<T>`, `Option<T>` left out when `None`,
//!   `Patch<T>`) and a `check_<name>` function per type with constraints;
//! - `src/resources/<resource>.rs`: a resource struct per resource with an
//!   async method per operation `(request, &CallOptions)`, `preview_<method>`
//!   and `<method>_pages` where they apply, and the request structs (one
//!   field per argument of `tungsten_emit::args`);
//! - `src/client.rs` (`<Api>Client`), `src/blocking.rs` (`<Api>BlockingClient`,
//!   the same calls run to completion by the runtime's blocking facade),
//!   `src/dispatch.rs` (`Dispatch`),
//!   `src/macros.rs` (`client.macros()`), `src/support.rs` (helpers),
//!   `src/lib.rs`, the hand-editable `src/custom/mod.rs`;
//! - `Cargo.toml` and `README.md` of the workspace.
//!
//! Naming: types, fields, methods and arguments come from
//! `tungsten_ir::naming` for Rust. Output is deterministic and laid out the
//! way rustfmt lays it out (descriptor data goes through a small printer
//! that follows rustfmt's width rules), so `cargo fmt --check` passes
//! without formatting.
//!
//! [`supports`] reports what is emitted with a fallback (TG0741 types typed
//! `serde_json::Value`, TG0743 OpenID Connect, TG0744 distinct success
//! bodies, TG0746 macro outputs) and what cannot be emitted (TG0742
//! macros); [`emit`] reports
//! invalid target options (TG0740) and file errors.

mod blocking;
mod dispatch;
mod graph;
mod macros;
pub(crate) mod ops;
mod package;
mod plan;
mod resources;
mod rs;
mod types;

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::{CommentStyle, FileSet, TargetConfig, header};
use tungsten_ir::{Ir, Shape, TypeRef};

use crate::options::Options;

/// Feature gaps for this IR.
pub(crate) fn supports(ir: &Ir) -> Diagnostics {
    let plan = plan::Plan::new(ir);
    let shapes: Vec<ops::OpShape<'_>> = plan.ops.iter().map(|o| ops::op_shape(&plan, o)).collect();
    let mut diags = macros::plan_macros(&plan, &shapes).1;
    diags.extend(ops::auth_notes(ir));
    diags.extend(fallback_notes(&plan, &shapes));
    diags
}

/// TG0741 for each named type typed `serde_json::Value`, TG0744 for each
/// operation whose success bodies differ.
fn fallback_notes(plan: &plan::Plan<'_>, shapes: &[ops::OpShape<'_>]) -> Diagnostics {
    let mut diags = Diagnostics::new();
    for t in &plan.ir.types.types {
        let reason = match (&t.shape, plan::emit_kind(plan.ir, t)) {
            (Shape::Enum { .. }, plan::Emit::Alias) => {
                Some("an enum whose values are not all strings or all integers")
            }
            (Shape::Union(u), plan::Emit::Alias) if u.variants.is_empty() => {
                Some("a union without variants")
            }
            (Shape::Intersection { .. }, _) => Some("an `allOf` that could not be merged"),
            (Shape::Never, _) => Some("a schema no value satisfies"),
            _ => None,
        };
        if let Some(reason) = reason {
            diags.push(Diagnostic::info(
                "TG0741",
                format!(
                    "type `{}` is {reason}; the Rust SDK types it `serde_json::Value` and checks the constraints at run time",
                    t.id.0
                ),
            ));
        }
        if let Shape::Union(u) = &t.shape
            && u.strategy == tungsten_ir::UnionStrategy::Tagged
            && plan::union_kind(plan.ir, u) != Some(plan::UnionKind::Tagged)
        {
            diags.push(Diagnostic::info(
                "TG0745",
                format!(
                    "union `{}` has a discriminator but not every variant is a named record; the Rust SDK tries its variants in order",
                    t.id.0
                ),
            ));
        }
    }
    for (info, shape) in plan.ops.iter().zip(shapes) {
        if shape.mixed_success {
            diags.push(Diagnostic::info(
                "TG0744",
                format!(
                    "operation `{}` answers with different bodies; the Rust SDK returns `serde_json::Value` and does not validate its response",
                    info.op.id.0
                ),
            ));
        }
    }
    fn walk(r: &TypeRef, at: &str, out: &mut Vec<String>) {
        if let TypeRef::Inline(s) = r {
            match s.as_ref() {
                Shape::Record { fields, .. } if !fields.is_empty() => out.push(at.to_string()),
                Shape::Enum { .. } | Shape::Union(_) => out.push(at.to_string()),
                Shape::Array { items, .. } => walk(items, at, out),
                Shape::Map { values } => walk(values, at, out),
                Shape::Nullable { inner } => walk(inner, at, out),
                _ => {}
            }
        }
    }
    let mut inline: Vec<String> = vec![];
    for t in &plan.ir.types.types {
        if let Shape::Record { fields, .. } = &t.shape {
            for f in fields {
                walk(&f.ty, &format!("{}.{}", t.id.0, f.wire_name), &mut inline);
            }
        }
    }
    for place in inline {
        diags.push(Diagnostic::info(
            "TG0741",
            format!("the inline schema at {place} has no name; the Rust SDK types it `serde_json::Value`"),
        ));
    }
    diags
}

/// Write the SDK crate and the workspace root files.
pub(crate) fn emit(ir: &Ir, cfg: &TargetConfig, out: &mut FileSet) -> Diagnostics {
    let (opts, mut diags) = Options::resolve(ir, cfg);
    for (path, text) in generate(ir, &opts) {
        if let Err(e) = out.add(path, text) {
            diags.push(Diagnostic::error("TG0701", format!("rust target: {e}")));
        }
    }
    diags
}

/// Every file of the SDK half: (path relative to the target directory,
/// contents), in a deterministic order.
fn generate(ir: &Ir, opts: &Options) -> Vec<(String, String)> {
    let plan = plan::Plan::new(ir);
    let hdr_slash = header(CommentStyle::DoubleSlash, ir);
    let hdr_hash = header(CommentStyle::Hash, ir);
    let shapes: Vec<ops::OpShape<'_>> = plan.ops.iter().map(|o| ops::op_shape(&plan, o)).collect();
    let (macro_plans, _) = macros::plan_macros(&plan, &shapes);
    let has_macros = !macro_plans.is_empty();
    let p = &opts.package;
    let mut files: Vec<(String, String)> = vec![
        (
            "Cargo.toml".into(),
            package::workspace_toml(opts, &hdr_hash),
        ),
        (
            "README.md".into(),
            package::readme(&plan, &shapes, opts, &hdr_slash, has_macros),
        ),
        (
            format!("{p}/Cargo.toml"),
            package::sdk_toml(&plan, opts, &hdr_hash),
        ),
        (
            format!("{p}/src/lib.rs"),
            package::lib_file(&plan, has_macros, &hdr_slash),
        ),
        (
            format!("{p}/src/support.rs"),
            package::support_file(&hdr_slash, plan.ops.iter().any(|o| o.op.stream.is_some())),
        ),
        (format!("{p}/src/custom/mod.rs"), package::custom_template()),
        (
            format!("{p}/src/client.rs"),
            resources::client_file(&plan, has_macros, &hdr_slash),
        ),
        (
            format!("{p}/src/descriptors.rs"),
            ops::descriptors_file(&plan, &shapes, &opts.version, has_macros, &hdr_slash),
        ),
        (
            format!("{p}/src/dispatch.rs"),
            dispatch::dispatch_file(&plan, &macro_plans, &hdr_slash),
        ),
        (
            format!("{p}/src/models/mod.rs"),
            types::models_mod(&plan, &hdr_slash),
        ),
        (
            format!("{p}/src/resources/mod.rs"),
            resources::resources_mod(&plan, &hdr_slash),
        ),
    ];
    files.push((
        format!("{p}/src/blocking.rs"),
        blocking::blocking_file(&plan, &shapes, &macro_plans, &hdr_slash),
    ));
    if has_macros {
        files.push((
            format!("{p}/src/macros.rs"),
            macros::macros_file(&plan, &shapes, &macro_plans, &hdr_slash),
        ));
    }
    for ns in &plan.models {
        files.push((
            format!("{p}/src/models/{}.rs", ns.file),
            types::models_file(&plan, &ns.name, &hdr_slash),
        ));
    }
    for (i, r) in plan.resources.iter().enumerate() {
        files.push((
            format!("{p}/src/resources/{}.rs", r.module),
            resources::resource_file(&plan, &shapes, i, &hdr_slash),
        ));
    }
    files
}

/// Internals exposed to the private test harness.
#[cfg(feature = "testing")]
pub mod testing {
    use tungsten_ir::{Ir, Presence, TypeRef};

    /// One field of an operation's request struct.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Arg {
        /// Field name (the key in the arguments object).
        pub key: String,
        /// Wire name of a parameter or merged body field.
        pub wire: Option<String>,
        pub optional: bool,
        /// The Rust type of the field.
        pub ty: String,
        /// The IR type behind it (none for raw bodies).
        pub type_ref: Option<TypeRef>,
        pub presence: Option<Presence>,
    }

    /// The request struct of an operation and its body shape.
    #[derive(Debug, Clone, PartialEq)]
    pub struct OpArgs {
        pub args: Vec<Arg>,
        /// (argument, wire name) pairs of a merged body.
        pub merged: Option<Vec<(String, String)>>,
        /// The argument carrying a whole body.
        pub body_arg: Option<String>,
        /// Index constant in `descriptors.rs`.
        pub konst: String,
        /// The request struct name.
        pub request: String,
        /// Its module under `resources`.
        pub module: String,
        /// The typed result's value type.
        pub success: String,
    }

    /// The arguments of the callable operation `id`, if any.
    pub fn op_args(ir: &Ir, id: &str) -> Option<OpArgs> {
        let plan = crate::sdk::plan::Plan::new(ir);
        let i = *plan.op_by_id.get(id)?;
        let info = &plan.ops[i];
        let shape = crate::sdk::ops::op_shape(&plan, info);
        let (merged, body_arg) = match shape.body.as_ref().map(|b| &b.shape) {
            Some(crate::sdk::ops::BodyShape::Merged(pairs)) => (Some(pairs.clone()), None),
            Some(crate::sdk::ops::BodyShape::Arg(a)) => (None, Some(a.clone())),
            None => (None, None),
        };
        let wire_of = |key: &str| {
            shape
                .params
                .iter()
                .find(|p| p.name == key)
                .map(|p| p.param.wire_name.clone())
                .or_else(|| {
                    merged
                        .as_ref()
                        .and_then(|m| m.iter().find(|(a, _)| a == key).map(|(_, w)| w.clone()))
                })
        };
        Some(OpArgs {
            args: shape
                .fields
                .iter()
                .map(|f| Arg {
                    key: f.key.clone(),
                    wire: wire_of(&f.key),
                    optional: f.optional,
                    ty: f.slot.ty.clone(),
                    type_ref: f.check.map(|c| c.ty.clone()),
                    presence: f.check.map(|c| c.presence),
                })
                .collect(),
            merged,
            body_arg,
            konst: info.konst.clone(),
            request: info.request.clone(),
            module: plan.resources[info.res].module.clone(),
            success: shape.success.clone(),
        })
    }

    /// The Rust name of a named type.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct TypeName {
        /// The IR type id.
        pub id: String,
        /// The module under `models` (a namespace's file).
        pub module: String,
        /// The Rust type name.
        pub name: String,
        /// The type's check function in the same module, if it has one.
        pub check: Option<String>,
    }

    /// The name of the blocking client.
    pub fn blocking_client_name(ir: &Ir) -> String {
        let plan = crate::sdk::plan::Plan::new(ir);
        crate::sdk::blocking::blocking_client_name(&plan)
    }

    /// The Rust type of each macro's output (`Value` when it is not
    /// typed), by macro name.
    pub fn macro_outputs(ir: &Ir) -> Vec<(String, String)> {
        let plan = crate::sdk::plan::Plan::new(ir);
        let shapes: Vec<crate::sdk::ops::OpShape<'_>> = plan
            .ops
            .iter()
            .map(|o| crate::sdk::ops::op_shape(&plan, o))
            .collect();
        crate::sdk::macros::plan_macros(&plan, &shapes)
            .0
            .iter()
            .map(|m| (m.name(), m.output_type()))
            .collect()
    }

    /// The Rust names of every named type.
    pub fn type_names(ir: &Ir) -> Vec<TypeName> {
        let plan = crate::sdk::plan::Plan::new(ir);
        plan.types
            .iter()
            .map(|(id, t)| TypeName {
                id: id.0.clone(),
                module: plan
                    .model_ns(&t.ns)
                    .map(|m| m.file.clone())
                    .unwrap_or_default(),
                name: t.name.clone(),
                check: plan
                    .graph
                    .needs_check
                    .contains(id)
                    .then(|| t.check_fn.clone()),
            })
            .collect()
    }

    /// The client's type name and the `Dispatch` accessor chain of every
    /// operation (`["public", "webhooks"]`), by operation id.
    pub fn client_info(ir: &Ir) -> (String, Vec<(String, Vec<String>)>) {
        let plan = crate::sdk::plan::Plan::new(ir);
        let paths = crate::sdk::dispatch::client_paths(&plan);
        let ops = plan
            .ops
            .iter()
            .zip(paths)
            .map(|(o, p)| (o.op.id.0.clone(), p))
            .collect();
        (plan.client_class.clone(), ops)
    }
}
