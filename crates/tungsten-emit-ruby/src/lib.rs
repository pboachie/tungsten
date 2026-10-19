// SPDX-License-Identifier: AGPL-3.0-only
//! Ruby SDK emitter (`ruby` target).
//!
//! Built on the language-neutral SDK layer of `tungsten-emit`
//! ([`tungsten_emit::sdk`]): the [`SdkPlan`] decides the structure, the
//! [`Namer`] the Ruby names, and the SDK descriptor document v1
//! (`tungsten-descriptors.json`) carries everything the runtime needs to
//! call operations and run macros. A generated gem is mostly that document
//! plus thin, documented Ruby over the `tungsten-runtime` gem
//! (`runtimes/ruby`):
//!
//! - `<gem>.gemspec`, `Gemfile`, `README.md`, `.rubocop.yml`;
//! - `lib/<lib>.rb`: the module, its `VERSION` and `DESCRIPTORS` (the loaded
//!   document), and the requires;
//! - `lib/<lib>/models/<namespace>.rb`: a `Data.define` value object per
//!   record (`from_json` and `to_h` by wire name; optional members default
//!   to `Tungsten::Runtime::UNSET`, so absent, `nil` and a value stay
//!   apart), a frozen constant module per enum with `member?`, a module per
//!   union that decodes by tag or by the first variant that accepts the
//!   value, and a module per other named type;
//! - `lib/<lib>/resources/*.rb`: one class per resource; each operation is
//!   a method with keyword arguments (`opts:` takes the call options) that
//!   returns `Tungsten::Runtime::Ok` or `Err`, with `preview_<op>`,
//!   `<op>_pages` and `<op>_stream` where they apply;
//! - `lib/<lib>/client.rb` (`<Api>Client`), `macros.rb` (`client.macros`),
//!   `dispatch.rb` (calls by operation id or macro name with JSON
//!   arguments, through the typed methods), `tungsten-descriptors.json`;
//! - `lib/<lib>/custom/extensions.rb`, created once and never overwritten.
//!
//! Arguments are snake_case: the document's `name` of each argument is its
//! Ruby keyword, and validation failures name it (`args.endpoint_id`).
//!
//! Output is deterministic: names come from the [`Namer`], every
//! collection is iterated in plan or sorted order, and files carry the
//! `tungsten_emit::header` stamp. [`RubyEmitter::supports`] reports what is
//! left out or emitted with a fallback: TG1061 (an `allOf` that cannot be
//! merged is the JSON value), TG1062 (a macro outside the canonical form),
//! TG1063 (OpenID Connect sent as a bearer token), TG1064 (different
//! success bodies by status), TG1065 (a discriminated union variant
//! without a tag value); `emit` reports invalid target options (TG1060).

mod context;
mod models;
pub mod options;
mod package;
mod rb;
mod sdk;

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::sdk::{AllOfPlan, SdkPlan, SuccessPlan, TypeKind, UnionPlan};
use tungsten_emit::{Emitter, FileSet, TargetConfig};
use tungsten_ir::{AuthScheme, Ir, Shape, UnionStrategy};

pub use options::Options;

/// Emits the `ruby` target.
#[derive(Debug, Clone, Copy, Default)]
pub struct RubyEmitter;

impl Emitter for RubyEmitter {
    fn id(&self) -> &'static str {
        "ruby"
    }

    fn supports(&self, ir: &Ir) -> Diagnostics {
        notes(ir)
    }

    fn emit(&self, ir: &Ir, cfg: &TargetConfig, out: &mut FileSet) -> Diagnostics {
        let (opts, mut diags) = Options::resolve(ir, cfg);
        for (path, text) in generate(ir, &opts) {
            if let Err(e) = out.add(path, text) {
                diags.push(Diagnostic::error("TG0701", format!("ruby target: {e}")));
            }
        }
        diags
    }

    fn uses_sdk_plan(&self) -> bool {
        true
    }

    fn supports_prepared(&self, ir: &Ir, plan: &SdkPlan<'_>) -> Diagnostics {
        notes_prepared(ir, plan)
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
                diags.push(Diagnostic::error("TG0701", format!("ruby target: {e}")));
            }
        }
        diags
    }
}

/// Every file of the gem: (path relative to the target directory,
/// contents), in a deterministic order.
pub fn generate(ir: &Ir, opts: &Options) -> Vec<(String, String)> {
    let (plan, _) = SdkPlan::new(ir);
    generate_prepared(ir, &plan, opts)
}

fn generate_prepared(_ir: &Ir, plan: &SdkPlan<'_>, opts: &Options) -> Vec<(String, String)> {
    let g = context::Gen::new(plan, opts);
    let mut files = package::files(&g);
    files.extend(models::files(&g));
    files.extend(sdk::files(&g));
    files
}

/// TG1061 to TG1065 for an IR (see the crate documentation).
fn notes(ir: &Ir) -> Diagnostics {
    let (plan, _) = SdkPlan::new(ir);
    notes_prepared(ir, &plan)
}

fn notes_prepared(ir: &Ir, plan: &SdkPlan<'_>) -> Diagnostics {
    let mut diags = Diagnostics::new();
    for t in plan.types.values() {
        match &t.kind {
            TypeKind::AllOf(AllOfPlan::Intersection) => diags.push(Diagnostic::warning(
                "TG1061",
                format!(
                    "the allOf type `{}` cannot be merged into one record; the Ruby SDK types it as its JSON value",
                    t.id.0
                ),
            )),
            TypeKind::Union(UnionPlan::Untagged { .. }) => {
                let tagged = ir.types.get(&t.id).is_some_and(|nt| {
                    matches!(&nt.shape, Shape::Union(u)
                        if u.strategy == UnionStrategy::Tagged && u.discriminator.is_some())
                });
                if tagged {
                    diags.push(Diagnostic::warning(
                        "TG1065",
                        format!(
                            "a variant of the discriminated union `{}` has no tag value or is not a named type; the Ruby SDK tries the variants in order",
                            t.id.0
                        ),
                    ));
                }
            }
            _ => {}
        }
    }
    for issue in &plan.macro_issues {
        diags.push(Diagnostic::warning(
            "TG1062",
            format!(
                "macro `{}` is not in the canonical form ({}); the Ruby SDK leaves it out",
                issue.name, issue.reason
            ),
        ));
    }
    for s in &ir.auth {
        if let AuthScheme::OpenIdConnect { name, .. } = s {
            diags.push(Diagnostic::info(
                "TG1063",
                format!("OpenID Connect scheme `{name}` is sent as a bearer token by the Ruby SDK"),
            ));
        }
    }
    for op in &plan.operations {
        if let SuccessPlan::ByStatus(_) = op.success {
            diags.push(Diagnostic::info(
                "TG1064",
                format!(
                    "`{}` answers different JSON bodies by status; its typed result in the Ruby SDK is the JSON value",
                    op.op.id.0
                ),
            ));
        }
    }
    diags
}
