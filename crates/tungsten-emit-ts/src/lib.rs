// SPDX-License-Identifier: AGPL-3.0-only
//! TypeScript SDK emitter (planning/05 "TypeScript SDK").
//!
//! Generated packages are mostly data for `@tungsten/runtime`
//! (`runtimes/ts/src/types.ts` is the contract):
//!
//! - `src/descriptors.ts`: one `OperationDescriptor` per callable
//!   operation (with Zod schemas for its args object and success body) and
//!   the `ApiDescriptor`;
//! - `src/models/<namespace>.ts`: a type and a Zod schema per IR type;
//! - `src/resources/...`: classes whose operation properties are callable,
//!   with `.preview()`, `.pages()`, `.descriptor` and `.safety` attached;
//! - `src/client.ts`, `src/macros.ts`, `src/index.ts`, `src/internal.ts`;
//! - `package.json`, `tsconfig.json`, `README.md`, and the hand-editable
//!   `src/custom/index.ts`, which output writing never overwrites.
//!
//! Output is deterministic: names come from `tungsten_ir::naming`, every
//! collection is iterated in IR or sorted order, and files carry the
//! `tungsten_emit::header` stamp, never timestamps or absolute paths.
//! [`TypeScriptEmitter::supports`] reports what cannot be emitted (TG0710
//! macros) or is emitted with a fallback (TG0712 OpenID Connect); `emit`
//! reports only invalid target options (TG0711) and file errors, so a
//! caller running both sees each problem once.

mod macros;
mod models;
mod ops;
pub mod options;
mod package;
mod plan;
mod resources;
mod ts;

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::{CommentStyle, Emitter, FileSet, TargetConfig, header};
use tungsten_ir::Ir;

pub use options::Options;

/// Emits the `typescript` target.
#[derive(Debug, Clone, Copy, Default)]
pub struct TypeScriptEmitter;

impl Emitter for TypeScriptEmitter {
    fn id(&self) -> &'static str {
        "typescript"
    }

    fn supports(&self, ir: &Ir) -> Diagnostics {
        let plan = plan::Plan::new(ir);
        let mut diags = macros::plan_macros(&plan).1;
        diags.extend(ops::auth_notes(ir));
        diags
    }

    fn emit(&self, ir: &Ir, cfg: &TargetConfig, out: &mut FileSet) -> Diagnostics {
        let (opts, mut diags) = Options::resolve(ir, cfg);
        for (path, text) in generate(ir, &opts) {
            if let Err(e) = out.add(path, text) {
                diags.push(Diagnostic::error(
                    "TG0701",
                    format!("typescript target: {e}"),
                ));
            }
        }
        diags
    }
}

/// Every file of the package: (path relative to the target directory,
/// contents), in a deterministic order.
pub fn generate(ir: &Ir, opts: &Options) -> Vec<(String, String)> {
    let plan = plan::Plan::new(ir);
    let header = header(CommentStyle::DoubleSlash, ir);
    let shapes: Vec<ops::OpShape<'_>> = plan.ops.iter().map(|o| ops::op_shape(&plan, o)).collect();
    let (macro_plans, _) = macros::plan_macros(&plan);
    let has_macros = !macro_plans.is_empty();
    let mut files: Vec<(String, String)> = vec![
        ("package.json".into(), package::package_json(&plan, opts)),
        ("tsconfig.json".into(), package::tsconfig_json()),
        (
            "README.md".into(),
            package::readme(&plan, &shapes, opts, &header, has_macros),
        ),
        ("src/custom/index.ts".into(), package::custom_template()),
        (
            "src/index.ts".into(),
            resources::index_file(&plan, has_macros, &header),
        ),
        (
            "src/client.ts".into(),
            resources::client_file(&plan, has_macros, &header),
        ),
        (
            "src/descriptors.ts".into(),
            ops::descriptors_file(&plan, &shapes, opts, &header),
        ),
    ];
    if has_macros {
        files.push((
            "src/macros.ts".into(),
            macros::macros_file(&plan, &shapes, &macro_plans, &header),
        ));
    }
    files.push(("src/internal.ts".into(), package::internal_file(&header)));
    for m in &plan.models {
        files.push((
            format!("src/models/{}.ts", m.file),
            models::models_file(&plan, &m.name, &header),
        ));
    }
    for i in 0..plan.resources.len() {
        files.push((
            plan.resources[i].file.clone(),
            resources::resource_file(&plan, &shapes, i, &header),
        ));
    }
    files
}
