// SPDX-License-Identifier: AGPL-3.0-only
//! MCP server emitter.
//!
//! Compiles the IR into an `McpManifest` (the JSON contract in
//! `runtimes/mcp/src/types.ts`): one tool entry per callable operation and
//! macro with compact schemas, annotations from safety tiers, clusters, and a
//! precomputed BM25 index for `search_tools`; plus a small TypeScript server
//! package that serves it through `@tungsten/mcp` on top of the generated
//! TypeScript SDK:
//!
//! - `manifest.json`: the manifest;
//! - `src/server.ts`: builds `ClientOptions` from environment variables
//!   (one per secret) and starts the stdio server; `src/custom/index.ts`
//!   is the hand-editable hook it calls;
//! - `package.json`, `tsconfig.json`, `README.md` (client configuration,
//!   environment, modes, safety rules, token budgets) and `.env.example`.
//!
//! [`McpEmitter::supports`] reports the budgets (TG0721 per tool over
//! `schema_budget_tokens`, TG0722 for a progressive listing over 2,000
//! tokens), macros the SDK does not emit (TG0723) and tools whose names
//! collide (TG0724); `emit` reports only invalid target options (TG0720)
//! and file errors, so a caller running both sees each problem once. Output is deterministic: tools in IR order,
//! sorted index terms, fixed float rounding, no timestamps or paths.

mod budget;
mod env;
mod index;
mod json;
mod manifest;
mod names;
mod options;
mod package;

use std::collections::BTreeMap;

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::{Emitter, FileSet, TargetConfig};
use tungsten_ir::Ir;

pub use budget::{INDEX_BUDGET, instructions, listing_tokens, tools_list};
pub use manifest::Mode;
pub use options::Options;

/// Emits the `mcp` target.
#[derive(Debug, Clone, Copy, Default)]
pub struct McpEmitter;

impl Emitter for McpEmitter {
    fn id(&self) -> &'static str {
        "mcp"
    }

    fn supports(&self, ir: &Ir) -> Diagnostics {
        let (manifest, mut diags) = manifest::build(ir);
        let budget = budget::measure(&manifest, schema_budget(ir));
        diags.extend(budget::warnings(&manifest, &budget));
        diags
    }

    fn emit(&self, ir: &Ir, cfg: &TargetConfig, out: &mut FileSet) -> Diagnostics {
        let (opts, mut diags) = Options::resolve(ir, cfg);
        for (path, text) in generate(ir, &opts) {
            if let Err(e) = out.add(path, text) {
                diags.push(Diagnostic::error("TG0701", format!("mcp target: {e}")));
            }
        }
        diags
    }
}

fn schema_budget(ir: &Ir) -> usize {
    ir.agent.disclosure.schema_budget_tokens as usize
}

/// The tools the server of `ir` serves: tool name → operation id or macro
/// name (recorded in the target's API surface snapshot, so `tungsten diff
/// --semver` sees renamed, removed and reassigned tool names).
pub fn tool_names(ir: &Ir) -> BTreeMap<String, String> {
    let (manifest, _) = manifest::build(ir);
    manifest
        .tools
        .into_iter()
        .map(|t| (t.name, t.target))
        .collect()
}

/// Every file of the server package: (path relative to the target
/// directory, contents), in a fixed order.
pub fn generate(ir: &Ir, opts: &Options) -> Vec<(String, String)> {
    let (manifest, _) = manifest::build(ir);
    let budget = budget::measure(&manifest, schema_budget(ir));
    let manifest_json = json::pretty(&serde_json::to_value(&manifest).unwrap_or_default());
    vec![
        (".env.example".into(), package::env_example(ir)),
        (
            "README.md".into(),
            package::readme(ir, opts, &manifest, &budget),
        ),
        ("manifest.json".into(), manifest_json),
        ("package.json".into(), package::package_json(ir, opts)),
        ("src/custom/index.ts".into(), package::custom_template()),
        (
            "src/server.ts".into(),
            package::server_ts(ir, opts, &manifest),
        ),
        ("tsconfig.json".into(), package::tsconfig_json()),
    ]
}

/// Internals exposed to the private test harness.
#[cfg(feature = "testing")]
pub mod __testing {
    use tungsten_core::Diagnostics;
    use tungsten_ir::Ir;

    pub use crate::budget::{
        Budget, INDEX_BUDGET, REDACTED_REPEAT, instructions, listing_tokens, tools_list,
    };
    pub use crate::index::{B, K1, STOP_WORDS, SearchIndex, build as build_index, tokenize};
    pub use crate::manifest::{
        BINARY_BODY_NOTE, COUNTER_NAME, ClusterEntry, InstructionsByMode, McpManifest, Mode,
        Reserved, ToolAnnotations, ToolEntry, ToolKind, tokens,
    };
    pub use crate::names::{MAX_TOOL_NAME, sanitize, shorten};

    /// The manifest of `ir` and its diagnostics.
    pub fn manifest(ir: &Ir) -> (McpManifest, Diagnostics) {
        crate::manifest::build(ir)
    }

    /// The budgets of `ir`'s manifest.
    pub fn budget(ir: &Ir) -> Budget {
        let (m, _) = crate::manifest::build(ir);
        crate::budget::measure(&m, super::schema_budget(ir))
    }

    /// Tool names assigned to `(base, key)` entries (key: operation id or
    /// macro name), in order.
    pub fn assign_names(entries: &[(&str, &str)]) -> Vec<String> {
        let owned: Vec<(String, String)> = entries
            .iter()
            .map(|(b, k)| (b.to_string(), k.to_string()))
            .collect();
        crate::names::assign(&owned).0
    }

    /// The environment variable names of `ir`'s server: base URL, every
    /// credential, mode.
    pub fn env_names(ir: &Ir) -> Vec<String> {
        let mut out = vec![crate::env::base_url(ir)];
        for s in crate::env::schemes(ir) {
            match s.credentials {
                crate::env::Credentials::Secret { env, .. } => out.push(env),
                crate::env::Credentials::Parts(parts) => {
                    out.extend(parts.into_iter().map(|(_, env, _)| env))
                }
            }
        }
        out.push(crate::env::mode(ir));
        out
    }
}
