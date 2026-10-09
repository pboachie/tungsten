// SPDX-License-Identifier: AGPL-3.0-only
//! Python SDK emitter.
//!
//! Generated packages are mostly data for `tungsten-runtime`
//! (`runtimes/python/src/tungsten_runtime/types.py` is the contract):
//!
//! - `<module>/_descriptors.py`: one `OperationDescriptor` per callable
//!   operation (with a request validator over its keyword arguments and a
//!   response validator built on `pydantic.TypeAdapter`) and the
//!   `ApiDescriptor`;
//! - `<module>/models/<namespace>.py`: a Pydantic v2 model per record and a
//!   PEP 695 `type` alias per other named type (presence:
//!   `T`, `T | None`, `T | Unset = UNSET`, `T | None | Unset = UNSET`);
//! - `<module>/resources/<resource>.py`: a sync and an async class per
//!   resource; each operation is a method with keyword-only arguments
//!   returning `Result[T]`, with `preview_<method>` and `<method>_pages`
//!   where they apply;
//! - `<module>/client.py` (`<Api>Client`, `Async<Api>Client`),
//!   `<module>/macros.py` (`client.macros`), `<module>/_internal.py`,
//!   `<module>/__init__.py`, `py.typed`;
//! - `pyproject.toml` (hatchling), `README.md`, and the hand-editable
//!   `<module>/custom/__init__.py`, which output writing never overwrites.
//!
//! Naming: types, fields, methods and arguments come from
//! `tungsten_ir::naming` for Python. A field whose wire name differs from
//! its attribute carries `Field(validation_alias=..., serialization_alias=...)`
//! (both rather than `alias`, so type checkers name the `__init__`
//! parameter after the attribute) and models accept either name. A leading
//! digit gets a word in front instead of `_` (`2fa` → `field_2fa`; Pydantic
//! treats `_` attributes as private), and a field named like a
//! `BaseModel` attribute or a builtin the annotations use gets `_` appended
//! (`json_`, `list_`). Enums are `Literal` unions, which round-trip any wire
//! value exactly. The `models: dataclasses` option (stdlib-only output) is
//! not implemented yet; TG0731 reports it and Pydantic models are emitted.
//!
//! Output is deterministic: names come from `tungsten_ir::naming`, every
//! collection is iterated in IR or sorted order, and files carry the
//! `tungsten_emit::header` stamp. [`PythonEmitter::supports`] reports what
//! cannot be emitted (TG0730 macros) or is emitted with a fallback (TG0732
//! OpenID Connect, TG0733 inline records); `emit` reports only invalid
//! target options (TG0731) and file errors.

mod macros;
mod ops;
pub mod options;
mod package;
mod plan;
mod py;
mod resources;
mod types;

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::{CommentStyle, Emitter, FileSet, TargetConfig, header};
use tungsten_ir::{Additional, Ir, Shape, TypeRef};

pub use options::{Options, RuntimeDep};

/// Emits the `python` target.
#[derive(Debug, Clone, Copy, Default)]
pub struct PythonEmitter;

impl Emitter for PythonEmitter {
    fn id(&self) -> &'static str {
        "python"
    }

    fn supports(&self, ir: &Ir) -> Diagnostics {
        let plan = plan::Plan::new(ir);
        let shapes: Vec<ops::OpShape<'_>> =
            plan.ops.iter().map(|o| ops::op_shape(&plan, o)).collect();
        let mut diags = macros::plan_macros(&plan, &shapes).1;
        diags.extend(ops::auth_notes(ir));
        diags.extend(inline_record_notes(ir));
        diags
    }

    fn emit(&self, ir: &Ir, cfg: &TargetConfig, out: &mut FileSet) -> Diagnostics {
        let (opts, mut diags) = Options::resolve(ir, cfg);
        for (path, text) in generate(ir, &opts) {
            if let Err(e) = out.add(path, text) {
                diags.push(Diagnostic::error("TG0701", format!("python target: {e}")));
            }
        }
        diags
    }
}

/// TG0733 for each inline record with fields (named records are classes):
/// the Python SDK types it as `dict[str, Any]`.
fn inline_record_notes(ir: &Ir) -> Diagnostics {
    fn walk_ref(r: &TypeRef, at: &str, out: &mut Vec<String>) {
        if let TypeRef::Inline(s) = r {
            if let Shape::Record { fields, additional } = s.as_ref()
                && !(fields.is_empty() && matches!(additional, Additional::Typed { .. }))
            {
                out.push(at.to_string());
            }
            walk(s, at, out);
        }
    }
    fn walk(s: &Shape, at: &str, out: &mut Vec<String>) {
        match s {
            Shape::Array { items, .. } => walk_ref(items, at, out),
            Shape::Map { values } => walk_ref(values, at, out),
            Shape::Nullable { inner } => walk_ref(inner, at, out),
            Shape::Record { fields, additional } => {
                for f in fields {
                    walk_ref(&f.ty, &format!("{at}.{}", f.wire_name), out);
                }
                if let Additional::Typed { values } = additional {
                    walk_ref(values, at, out);
                }
            }
            Shape::Union(u) => {
                for v in &u.variants {
                    walk_ref(&v.ty, at, out);
                }
            }
            Shape::Intersection { members } => {
                for m in members {
                    walk_ref(m, at, out);
                }
            }
            Shape::Primitive { .. }
            | Shape::Enum { .. }
            | Shape::Const { .. }
            | Shape::Any
            | Shape::Never => {}
        }
    }
    let mut at: Vec<String> = vec![];
    for t in &ir.types.types {
        walk(&t.shape, &t.id.0, &mut at);
    }
    for op in ir.operations() {
        let params = op
            .params
            .path
            .iter()
            .chain(&op.params.query)
            .chain(&op.params.header)
            .chain(&op.params.cookie);
        for p in params {
            walk_ref(
                &p.ty,
                &format!("{} parameter {}", op.id.0, p.wire_name),
                &mut at,
            );
        }
        if let Some(b) = &op.body {
            for c in &b.content {
                walk_ref(&c.ty, &format!("{} body", op.id.0), &mut at);
            }
        }
        for r in &op.responses {
            for c in &r.content {
                walk_ref(&c.ty, &format!("{} response", op.id.0), &mut at);
            }
        }
    }
    let mut diags = Diagnostics::new();
    for place in at {
        diags.push(Diagnostic::info(
            "TG0733",
            format!("the inline record at {place} is typed `dict[str, Any]` in the Python SDK; name the schema to get a model"),
        ));
    }
    diags
}

/// Every file of the package: (path relative to the target directory,
/// contents), in a deterministic order.
pub fn generate(ir: &Ir, opts: &Options) -> Vec<(String, String)> {
    let plan = plan::Plan::new(ir);
    let header = header(CommentStyle::Hash, ir);
    let shapes: Vec<ops::OpShape<'_>> = plan.ops.iter().map(|o| ops::op_shape(&plan, o)).collect();
    let (macro_plans, _) = macros::plan_macros(&plan, &shapes);
    let has_macros = !macro_plans.is_empty();
    let m = &opts.module;
    let mut files: Vec<(String, String)> = vec![
        (
            "pyproject.toml".into(),
            package::pyproject(&plan, opts, &header),
        ),
        (
            "README.md".into(),
            package::readme(&plan, &shapes, opts, &header, has_macros),
        ),
        (
            format!("{m}/custom/__init__.py"),
            package::custom_template(),
        ),
        (format!("{m}/py.typed"), String::new()),
        (
            format!("{m}/__init__.py"),
            resources::init_file(&plan, has_macros, &header),
        ),
        (
            format!("{m}/client.py"),
            resources::client_file(&plan, has_macros, &header),
        ),
        (
            format!("{m}/_descriptors.py"),
            ops::descriptors_file(&plan, &shapes, opts, &header),
        ),
        (format!("{m}/_internal.py"), package::internal_file(&header)),
        (
            format!("{m}/models/__init__.py"),
            types::models_init(&plan, &header),
        ),
        (
            format!("{m}/resources/__init__.py"),
            format!("{header}\n\"\"\"Resource classes, one module per resource.\"\"\"\n"),
        ),
    ];
    if has_macros {
        files.push((
            format!("{m}/macros.py"),
            macros::macros_file(&plan, &shapes, &macro_plans, &header),
        ));
    }
    for ns in &plan.models {
        files.push((
            format!("{m}/models/{}.py", ns.file),
            types::models_file(&plan, &ns.name, &header),
        ));
    }
    for (i, r) in plan.resources.iter().enumerate() {
        files.push((
            format!("{m}/resources/{}.py", r.module),
            resources::resource_file(&plan, &shapes, i, &header),
        ));
    }
    files
}

/// Internals exposed to the private test harness.
#[cfg(feature = "testing")]
pub mod __testing {
    use tungsten_ir::{Ident, Ir, TypeRef};

    pub use crate::plan::{FIELD_RESERVED, MODELS_RESERVED};

    /// Python attribute names of a record's fields, in order.
    pub fn field_names(idents: &[Ident]) -> Vec<String> {
        crate::plan::field_names(idents)
    }

    /// One keyword argument of an operation.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Arg {
        pub key: String,
        /// Wire name of a parameter or merged body field.
        pub wire: Option<String>,
        pub optional: bool,
        /// Signature annotation.
        pub hint: String,
        /// Validated type.
        pub schema: String,
    }

    /// The keyword arguments of an operation and its body shape.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct OpArgs {
        pub args: Vec<Arg>,
        /// (argument, wire name) pairs of a merged body.
        pub merged: Option<Vec<(String, String)>>,
        /// The argument carrying a whole body.
        pub body_arg: Option<String>,
        /// Descriptor constant name.
        pub key: String,
        /// `Result[T]`'s `T`.
        pub success: String,
    }

    /// The arguments of the callable operation `id`, if any.
    pub fn op_args(ir: &Ir, id: &str) -> Option<OpArgs> {
        let plan = crate::plan::Plan::new(ir);
        let i = *plan.op_by_id.get(id)?;
        let info = &plan.ops[i];
        let shape = crate::ops::op_shape(&plan, info);
        let (merged, body_arg) = match shape.body.as_ref().map(|b| &b.shape) {
            Some(crate::ops::BodyShape::Merged(pairs)) => (Some(pairs.clone()), None),
            Some(crate::ops::BodyShape::Arg(a)) => (None, Some(a.clone())),
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
                    hint: f.hint.clone(),
                    schema: f.schema.clone(),
                })
                .collect(),
            merged,
            body_arg,
            key: info.key.clone(),
            success: shape.success.text(),
        })
    }

    /// The `OperationDescriptor` of the callable operation `id` as JSON:
    /// Python literals as their values, code (validators) as strings.
    pub fn descriptor_json(ir: &Ir, id: &str) -> Option<serde_json::Value> {
        let plan = crate::plan::Plan::new(ir);
        let info = &plan.ops[*plan.op_by_id.get(id)?];
        let shape = crate::ops::op_shape(&plan, info);
        Some(crate::ops::descriptor_py(&plan, info, &shape).to_json())
    }

    /// The `ApiDescriptor` as JSON.
    pub fn api_json(ir: &Ir, opts: &crate::Options) -> serde_json::Value {
        let plan = crate::plan::Plan::new(ir);
        crate::ops::api_py(&plan, opts).to_json()
    }

    /// `(hint, schema)` renderings of a type outside the models package.
    pub fn render(ir: &Ir, ty: &TypeRef) -> (String, String) {
        let plan = crate::plan::Plan::new(ir);
        let cx = crate::types::Cx::new(&plan, None);
        (
            cx.ty(ty, crate::types::Flavor::Hint).text(),
            cx.ty(ty, crate::types::Flavor::Schema).text(),
        )
    }

    /// The Python names of every named type: (type id, module, name).
    pub fn type_names(ir: &Ir) -> Vec<(String, String, String)> {
        let plan = crate::plan::Plan::new(ir);
        plan.types
            .iter()
            .map(|(id, t)| {
                let module = plan
                    .model_ns(&t.ns)
                    .map(|m| m.file.clone())
                    .unwrap_or_default();
                (id.0.clone(), module, t.name.clone())
            })
            .collect()
    }
}
