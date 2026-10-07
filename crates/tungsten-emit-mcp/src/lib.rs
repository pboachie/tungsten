// SPDX-License-Identifier: AGPL-3.0-only
//! MCP server emitter (planning/05 "MCP server", planning/07 "MCP tool
//! surface").
//!
//! Compiles the IR into an `McpManifest` (the JSON contract in
//! `runtimes/mcp/src/types.ts`): one tool entry per callable operation and
//! macro with compact schemas, annotations from safety tiers, clusters, and a
//! precomputed BM25 index for `search_tools`; plus a small TypeScript server
//! package that serves it through `@tungsten/mcp` on top of the generated
//! TypeScript SDK.
//!
//! PHASE-3 STUB: emits nothing. The mcp-emitter work package implements it.

use tungsten_core::Diagnostics;
use tungsten_emit::{Emitter, FileSet, TargetConfig};
use tungsten_ir::Ir;

/// Emits the `mcp` target.
#[derive(Debug, Clone, Copy, Default)]
pub struct McpEmitter;

impl Emitter for McpEmitter {
    fn id(&self) -> &'static str {
        "mcp"
    }
    fn supports(&self, _ir: &Ir) -> Diagnostics {
        Diagnostics::new()
    }
    fn emit(&self, _ir: &Ir, _cfg: &TargetConfig, _out: &mut FileSet) -> Diagnostics {
        Diagnostics::new()
    }
}
