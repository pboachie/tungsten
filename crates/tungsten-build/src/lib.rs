// SPDX-License-Identifier: AGPL-3.0-only
//! Builds the IR from loaded OpenAPI documents and `tungsten.yml`
//! (planning/03).
//!
//! Ownership inside this crate:
//! - `types/`   schema → IR types (nullability, unions, allOf, enums, cycles,
//!   type naming).
//! - everything else except `driver.rs`: operations, resources, params,
//!   bodies, responses, auth, pagination, rpc unflattening, include filters,
//!   error model.
//! - `driver.rs`: file-level glue used by the CLI and the test harness.
//!
//! PHASE-1 STUB: `build` produces namespaces without operations.

pub mod driver;
pub mod types;

use std::collections::BTreeMap;

use tungsten_config::TungstenConfig;
use tungsten_core::{Diagnostics, Digest};
use tungsten_ir::{
    ApiInfo, ErrorModel, GeneratorStamp, IR_VERSION, Ident, Ir, Namespace, SourceRef,
};
use tungsten_openapi::{DocId, Workspace};

pub use driver::{CompileOptions, Compiled, compile_config, compile_project, compile_spec};

/// The tungsten version stamped into the IR.
pub const TUNGSTEN_VERSION: &str = env!("CARGO_PKG_VERSION");

/// One input namespace: its index in `config.inputs` and its loaded entry
/// document.
#[derive(Debug, Clone)]
pub struct NamespaceInput {
    pub config_index: usize,
    pub doc: DocId,
}

#[derive(Debug)]
pub struct BuildInput<'a> {
    pub config: &'a TungstenConfig,
    pub workspace: &'a Workspace,
    pub namespaces: Vec<NamespaceInput>,
    pub tungsten_version: String,
    /// Display name → digest of every input file (specs, overlays, manifest).
    pub input_digests: BTreeMap<String, Digest>,
}

#[derive(Debug)]
pub struct BuildOutput {
    pub ir: Ir,
    pub diagnostics: Diagnostics,
}

/// Build the IR. Never panics on bad input; problems become diagnostics.
pub fn build(input: &BuildInput<'_>) -> BuildOutput {
    let ws = input.workspace;
    let cfg = input.config;
    let mut diagnostics = Diagnostics::new();
    let mut tb = types::TypeBuilder::new(ws, &cfg.types.break_cycles);
    let mut namespaces = vec![];
    for ns in &input.namespaces {
        let ic = &cfg.inputs[ns.config_index];
        tb.add_components(&ic.namespace, ns.doc);
        let doc = &ws.documents[ns.doc];
        let info = doc.root.get("info");
        let s = |k: &str| {
            info.and_then(|i| i.get(k))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        namespaces.push(Namespace {
            name: Ident::new(&ic.namespace),
            source: SourceRef {
                file: doc.name.clone(),
                pointer: String::new(),
            },
            digest: doc.digest.clone(),
            title: s("title"),
            version: s("version"),
            resources: vec![],
            planned: vec![],
        });
    }
    let (types, tdiags) = tb.finish();
    diagnostics.extend(tdiags);
    let first_info = input
        .namespaces
        .first()
        .and_then(|n| ws.documents[n.doc].root.get("info"));
    let ir = Ir {
        ir_version: IR_VERSION.to_string(),
        generator: GeneratorStamp {
            tungsten_version: input.tungsten_version.clone(),
            inputs: input.input_digests.clone(),
        },
        api: ApiInfo {
            name: Ident::new(&cfg.api.name),
            title: cfg
                .api
                .title
                .clone()
                .unwrap_or_else(|| cfg.api.name.clone()),
            description: cfg.api.description.clone(),
            version: first_info
                .and_then(|i| i.get("version"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            servers: vec![],
        },
        namespaces,
        types,
        auth: vec![],
        errors: ErrorModel::default(),
        agent: Default::default(),
        diagnostics: vec![],
    };
    BuildOutput { ir, diagnostics }
}
