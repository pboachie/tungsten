// SPDX-License-Identifier: AGPL-3.0-only
//! Builds the IR from loaded OpenAPI documents and `tungsten.yml`.
//!
//! Ownership inside this crate:
//! - `types/`   schema → IR types (nullability, unions, allOf, enums, cycles,
//!   type naming).
//! - everything else except `driver.rs`: operations, resources, params,
//!   bodies, responses, auth, pagination, rpc unflattening, include filters,
//!   error model.
//! - `driver.rs`: file-level glue used by the CLI and the test harness.
//!
//! Build order: component types of every namespace, the security scheme
//! table, then per namespace its operations (include filters, rpc
//! unflattening, unique ids, parameters, bodies, responses, security,
//! gates), pagination, its error model, and the resource tree with method
//! names; finally the API-wide error model, servers and checks of manifest
//! references. Decisions
//! that depend on schema structure (pagination, rpc, error envelope) read
//! the normalized documents, never type shapes.

pub mod driver;
pub mod types;

mod auth;
mod bodies;
mod ctx;
mod errors;
mod filter;
mod names;
mod operations;
mod pagination;
mod params;
mod prune;
mod resources;
mod responses;
mod rpc;
mod streams;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use tungsten_config::TungstenConfig;
use tungsten_core::{Diagnostic, Diagnostics, Digest};
use tungsten_ir::{ApiInfo, GeneratorStamp, IR_VERSION, Ident, Ir, Namespace, Server, SourceRef};
use tungsten_openapi::{DocId, Workspace};

use crate::ctx::{Ctx, pointer, str_of};

pub use driver::{CompileOptions, Compiled, compile_config, compile_project, compile_spec};

/// The tungsten version stamped into the IR.
pub const TUNGSTEN_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How [`build`] names the manifest in diagnostics when no name is given.
pub const DEFAULT_MANIFEST_NAME: &str = "tungsten.yml";

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
/// Diagnostics about the manifest name it [`DEFAULT_MANIFEST_NAME`].
pub fn build(input: &BuildInput<'_>) -> BuildOutput {
    build_with_manifest(input, DEFAULT_MANIFEST_NAME)
}

/// [`build`], naming the manifest `manifest` in the labels of diagnostics
/// about it (the name the manifest was loaded under, so spans attach).
pub fn build_with_manifest(input: &BuildInput<'_>, manifest: &str) -> BuildOutput {
    let ws = input.workspace;
    let cfg = input.config;
    let mut cx = Ctx::new(ws, cfg, manifest);
    for ns in &input.namespaces {
        cx.tb
            .add_components(&cfg.inputs[ns.config_index].namespace, ns.doc);
    }
    let auth = auth::AuthTable::build(&mut cx, &input.namespaces);

    let mut namespaces = vec![];
    let mut known_ids: BTreeSet<String> = BTreeSet::new();
    for (ns_index, ns) in input.namespaces.iter().enumerate() {
        let name = cfg.inputs[ns.config_index].namespace.as_str();
        let mut ops = operations::build_namespace(&mut cx, &auth, ns_index, ns);
        pagination::apply(&mut cx, &mut ops.callable, &mut ops.planned);
        for built in ops.callable.iter().chain(&ops.planned) {
            known_ids.insert(built.op.id.0.clone());
        }
        let responses: Vec<&[responses::RawResponse]> = ops
            .callable
            .iter()
            .map(|b| b.responses.as_slice())
            .collect();
        let errors = errors::build(&mut cx, name, &responses);
        let resources = resources::build(&mut cx, name, ns.doc, ops.callable, &mut ops.planned);
        let doc = &ws.documents[ns.doc];
        let info = |k: &str| {
            doc.root
                .get("info")
                .and_then(|i| str_of(i, k))
                .unwrap_or("")
                .to_string()
        };
        namespaces.push(Namespace {
            name: Ident::new(name),
            source: SourceRef {
                file: doc.name.clone(),
                pointer: String::new(),
            },
            digest: doc.digest.clone(),
            title: info("title"),
            version: version_of(info("version")),
            resources,
            planned: ops.planned.into_iter().map(|b| b.op).collect(),
            errors,
        });
    }
    check_operation_refs(&mut cx, &known_ids);
    let errors = errors::merge(namespaces.iter().map(|n| &n.errors));

    let Ctx {
        tb,
        diags: mut diagnostics,
        ..
    } = cx;
    let (types, type_diags) = tb.finish();
    diagnostics.extend(type_diags);
    let first_doc = input.namespaces.first().map(|n| &ws.documents[n.doc]);
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
            version: version_of(
                first_doc
                    .and_then(|d| d.root.get("info"))
                    .and_then(|i| str_of(i, "version"))
                    .unwrap_or("")
                    .to_string(),
            ),
            servers: servers(cfg, first_doc.map(|d| &d.root)),
        },
        namespaces,
        types,
        auth: auth.schemes,
        errors,
        agent: Default::default(),
        diagnostics: vec![],
    };
    BuildOutput { ir, diagnostics }
}

/// The version recorded for a document without `info.version` (TG0110).
const DEFAULT_VERSION: &str = "0.0.0";

fn version_of(declared: String) -> String {
    if declared.trim().is_empty() {
        DEFAULT_VERSION.to_string()
    } else {
        declared
    }
}

/// `servers.default` from the manifest, else the first document's servers.
fn servers(cfg: &TungstenConfig, first: Option<&Value>) -> Vec<Server> {
    if let Some(url) = cfg.servers.as_ref().and_then(|s| s.default.clone()) {
        return vec![Server {
            url,
            description: None,
        }];
    }
    first
        .and_then(|root| root.get("servers"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| {
            Some(Server {
                url: str_of(s, "url")?.to_string(),
                description: str_of(s, "description").map(str::to_string),
            })
        })
        .collect()
}

/// Manifest keys naming operations that do not exist (TG0603):
/// `naming.operations` and `pagination`. `known` holds every operation id
/// of every namespace, planned ones included.
fn check_operation_refs(cx: &mut Ctx<'_>, known: &BTreeSet<String>) {
    let cfg = cx.cfg;
    let refs = cfg
        .naming
        .operations
        .keys()
        .map(|k| ("naming.operations", pointer(["naming", "operations", k]), k))
        .chain(
            cfg.pagination
                .keys()
                .map(|k| ("pagination", pointer(["pagination", k]), k)),
        );
    for (section, at, key) in refs {
        if !known.contains(key) {
            cx.report_manifest(
                Diagnostic::warning(
                    "TG0603",
                    format!("{section} names `{key}`, which is not an operation of the inputs"),
                ),
                &at,
            );
        }
    }
}

/// Internals exposed to the private test harness.
#[cfg(feature = "testing")]
#[doc(hidden)]
pub mod __testing {
    use tungsten_ir::{BodyEncoding, HttpMethod, PathTemplate, StatusMatch};

    /// The body encoding of a media type.
    pub fn encoding_for(media_type: &str) -> BodyEncoding {
        crate::bodies::encoding_for(media_type)
    }

    /// The id given to an operation without `operationId`.
    pub fn synthesize_operation_id(method: HttpMethod, path: &str) -> String {
        crate::names::synthesize_operation_id(method, path)
    }

    /// A Responses Object key as a status.
    pub fn parse_status(key: &str) -> Option<StatusMatch> {
        crate::responses::parse_status(key)
    }

    /// Whether a response description names an error code.
    pub fn mentions_code(description: &str, code: &str) -> bool {
        crate::errors::mentions(description, code)
    }

    /// A path template and its segments.
    pub fn path_template(raw: &str) -> PathTemplate {
        crate::params::path_template(raw)
    }

    /// Whether a leading path segment is an API version (`v1`, `v2beta`).
    pub fn is_version_segment(segment: &str) -> bool {
        crate::resources::is_version(segment)
    }

    /// Whether a trailing POST segment reads as a verb.
    pub fn reads_as_verb(segment: &str) -> bool {
        crate::resources::reads_as_verb(segment)
    }
}
