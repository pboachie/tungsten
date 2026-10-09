// SPDX-License-Identifier: AGPL-3.0-only
//! Operations of one namespace (planning/03 "Operations").
//!
//! Paths are visited in the order written, and the methods of a path item
//! in the order get, put, post, delete, options, head, patch, trace. Before
//! anything is built, each operation gets its disposition (include /
//! planned_from), the `rpc_unflatten` envelope is replaced by one entry per
//! method, and ids are made unique within the namespace. Names inside
//! resources are assigned later by the resource tree.

use std::collections::BTreeSet;

use serde_json::Value;
use tungsten_config::RpcUnflatten;
use tungsten_core::Diagnostic;
use tungsten_ir::{
    Body, BodyContent, BodyEncoding, HttpMethod, Ident, Operation, OperationAgentMeta, OperationId,
    OperationStatus, RpcBinding, SourceRef,
};
use tungsten_openapi::RefTarget;

use crate::auth::AuthTable;
use crate::ctx::{Ctx, child, doc_of, flag, str_of};
use crate::filter::{self, Disposition};
use crate::names::{pascal, synthesize_operation_id};
use crate::responses::RawResponse;
use crate::rpc::{self, RpcVariant};
use crate::{NamespaceInput, bodies, params, responses, streams};

/// Path item members that are operations, in visiting order.
pub(crate) const METHODS: [(&str, HttpMethod); 8] = [
    ("get", HttpMethod::Get),
    ("put", HttpMethod::Put),
    ("post", HttpMethod::Post),
    ("delete", HttpMethod::Delete),
    ("options", HttpMethod::Options),
    ("head", HttpMethod::Head),
    ("patch", HttpMethod::Patch),
    ("trace", HttpMethod::Trace),
];

/// The operation being built, as the part builders see it.
#[derive(Debug)]
pub(crate) struct OpScope<'s> {
    pub ns: &'s str,
    pub ns_index: usize,
    /// Full operation id (`public.listPets`), for messages.
    pub id: &'s str,
    /// PascalCase hint for names of inline types (`ListPets`).
    pub hint: String,
}

/// A built operation plus the raw facts later steps read.
#[derive(Debug, Clone)]
pub(crate) struct BuiltOp {
    pub op: Operation,
    pub responses: Vec<RawResponse>,
    /// Where diagnostics about the operation point.
    pub at: RefTarget,
}

/// The operations of one namespace, in spec order.
#[derive(Debug, Default)]
pub(crate) struct NamespaceOps {
    pub callable: Vec<BuiltOp>,
    pub planned: Vec<BuiltOp>,
}

/// One operation before it is built.
#[derive(Debug, Clone)]
struct Skeleton {
    path: String,
    method: HttpMethod,
    path_item: RefTarget,
    target: RefTarget,
    operation_id: Option<String>,
    /// Id without the namespace.
    local_id: String,
    disposition: Disposition,
    rpc: Option<RpcVariant>,
}

/// Build every operation of a namespace.
pub(crate) fn build_namespace(
    cx: &mut Ctx<'_>,
    auth: &AuthTable,
    ns_index: usize,
    input: &NamespaceInput,
) -> NamespaceOps {
    let cfg = cx.cfg;
    let config = &cfg.inputs[input.config_index];
    let ns = config.namespace.as_str();
    let mut skeletons = enumerate(cx, input);
    for sk in &skeletons {
        filter::report_excluded(
            cx,
            &format!("{ns}.{}", sk.local_id),
            &sk.disposition,
            &sk.target,
        );
    }
    skeletons.retain(|sk| !matches!(sk.disposition, Disposition::Drop(_)));
    if let Some(rpc) = &config.rpc_unflatten {
        expand_rpc(cx, rpc, input.config_index, &mut skeletons);
    }
    unique_ids(cx, ns, &mut skeletons);

    let mut out = NamespaceOps::default();
    // The envelope of rpc variants is built once and shared.
    let mut envelope: Option<(RefTarget, BuiltOp)> = None;
    for sk in &skeletons {
        let built = match (&sk.rpc, &config.rpc_unflatten) {
            (Some(variant), Some(rpc)) => {
                let base = match &envelope {
                    Some((target, base)) if *target == sk.target => base.clone(),
                    _ => {
                        let base = build_op(cx, auth, ns, ns_index, sk);
                        envelope = Some((sk.target.clone(), base.clone()));
                        base
                    }
                };
                rpc_op(cx, ns, rpc, base, sk, variant)
            }
            _ => build_op(cx, auth, ns, ns_index, sk),
        };
        match sk.disposition {
            Disposition::Planned(_) => out.planned.push(built),
            _ => out.callable.push(built),
        }
    }
    out
}

/// Every operation of the document, with its disposition. Operations
/// without `operationId` get a synthesized id (TG0403) unless omitted.
fn enumerate(cx: &mut Ctx<'_>, input: &NamespaceInput) -> Vec<Skeleton> {
    let cfg = cx.cfg;
    let config = &cfg.inputs[input.config_index];
    let paths_at = RefTarget {
        doc: input.doc,
        pointer: "/paths".into(),
    };
    let Some(Value::Object(paths)) = cx.get(&paths_at) else {
        return vec![];
    };
    let mut out = vec![];
    for path in paths.keys() {
        let Some((path_item, item)) = cx.deref_value(&child(&paths_at, path)) else {
            continue;
        };
        if !item.is_object() {
            cx.report(
                Diagnostic::warning(
                    "TG0508",
                    format!("path item {path} must be an object; ignored"),
                ),
                &path_item,
            );
            continue;
        }
        for (word, method) in METHODS {
            let Some(op) = item.get(word) else {
                continue;
            };
            let target = child(&path_item, word);
            if !op.is_object() {
                cx.report(
                    Diagnostic::warning(
                        "TG0508",
                        format!(
                            "operation {} {path} must be an object; ignored",
                            word.to_ascii_uppercase()
                        ),
                    ),
                    &target,
                );
                continue;
            }
            let disposition = filter::disposition(config, op);
            let operation_id = str_of(op, "operationId")
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string);
            let local_id = match &operation_id {
                Some(id) => id.clone(),
                None => {
                    let id = synthesize_operation_id(method, path);
                    if !matches!(disposition, Disposition::Drop(_)) {
                        cx.report(
                            Diagnostic::warning(
                                "TG0403",
                                format!(
                                    "operation {} {path} has no operationId; using `{id}`",
                                    word.to_ascii_uppercase()
                                ),
                            )
                            .with_help(
                                "add an operationId so the id does not change with the path",
                            ),
                            &target,
                        );
                    }
                    id
                }
            };
            out.push(Skeleton {
                path: path.clone(),
                method,
                path_item: path_item.clone(),
                target,
                operation_id,
                local_id,
                disposition,
                rpc: None,
            });
        }
    }
    out
}

/// Replace the envelope operation named by `rpc_unflatten` by one entry
/// per method. Problems are TG0506 errors and leave the envelope as is.
fn expand_rpc(
    cx: &mut Ctx<'_>,
    rpc: &RpcUnflatten,
    input_index: usize,
    skeletons: &mut Vec<Skeleton>,
) {
    let method = rpc.method.to_ascii_lowercase();
    let Some(index) = skeletons
        .iter()
        .position(|sk| sk.path == rpc.path && crate::names::method_word(sk.method) == method)
    else {
        rpc::report_missing(cx, rpc, input_index);
        return;
    };
    let envelope = skeletons[index].clone();
    let Some(variants) = rpc::variants(cx, rpc, input_index, &envelope.target) else {
        return;
    };
    let expanded = variants.into_iter().map(|v| Skeleton {
        local_id: v.local_id.clone(),
        rpc: Some(v),
        ..envelope.clone()
    });
    skeletons.splice(index..=index, expanded);
}

/// Make local ids unique in spec order: a repeated id gets the smallest
/// numeric suffix `n >= 2` that is free (TG0401).
fn unique_ids(cx: &mut Ctx<'_>, ns: &str, skeletons: &mut [Skeleton]) {
    let mut used: BTreeSet<String> = BTreeSet::new();
    for sk in skeletons.iter_mut() {
        if used.insert(sk.local_id.clone()) {
            continue;
        }
        let mut n = 2u64;
        while used.contains(&format!("{}{n}", sk.local_id)) {
            n += 1;
        }
        let renamed = format!("{}{n}", sk.local_id);
        let at = sk
            .rpc
            .as_ref()
            .map_or_else(|| sk.target.clone(), |v| v.variant.clone());
        cx.report(
            Diagnostic::warning(
                "TG0401",
                format!(
                    "operation id `{ns}.{}` is used more than once; this one is renamed to `{ns}.{renamed}`",
                    sk.local_id
                ),
            ),
            &at,
        );
        used.insert(renamed.clone());
        sk.local_id = renamed;
    }
}

fn build_op(
    cx: &mut Ctx<'_>,
    auth: &AuthTable,
    ns: &str,
    ns_index: usize,
    sk: &Skeleton,
) -> BuiltOp {
    let value = cx.get(&sk.target).unwrap_or(&Value::Null);
    // The shared parts of rpc methods are built under the envelope's id.
    let local = match (&sk.rpc, &sk.operation_id) {
        (Some(_), Some(id)) => id.clone(),
        (Some(_), None) => synthesize_operation_id(sk.method, &sk.path),
        (None, _) => sk.local_id.clone(),
    };
    let id = format!("{ns}.{local}");
    let scope = OpScope {
        ns,
        ns_index,
        id: &id,
        hint: pascal(&local),
    };
    let template = params::path_template(&sk.path);
    let params = params::build(cx, auth, &scope, &sk.path_item, &sk.target, &template);
    // An rpc envelope's body is replaced by each method's parameters, so
    // it is never converted: no type and no diagnostic for a schema that
    // is not in the IR.
    let body = if sk.rpc.is_some() {
        None
    } else {
        bodies::request_body(cx, &scope, &sk.target)
    };
    let (responses, raw) = responses::build(cx, &scope, &sk.target);
    let security = auth.requirements(cx, ns_index, &sk.target);
    let status = match &sk.disposition {
        Disposition::Planned(reason) => OperationStatus::Planned {
            reason: reason.clone(),
        },
        _ => filter::status(cx, &id, value, &sk.target),
    };
    let tags = value
        .get("tags")
        .and_then(Value::as_array)
        .map(|t| {
            t.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mut op = Operation {
        id: OperationId(id.clone()),
        name: Ident::new(&local),
        operation_id: sk.operation_id.clone(),
        method: sk.method,
        path: template,
        doc: doc_of(value),
        tags,
        params,
        body,
        responses,
        security,
        pagination: None,
        streaming: None,
        stream: None,
        deprecated: flag(value, "deprecated"),
        status,
        rpc: None,
        extensions: filter::extensions(value),
        agent: OperationAgentMeta::default_for(sk.method),
        source: SourceRef {
            file: cx.ws.name(sk.target.doc).to_string(),
            pointer: sk.target.pointer.clone(),
        },
    };
    streams::apply(cx, &mut op, &sk.target);
    BuiltOp {
        op,
        responses: raw,
        at: sk.target.clone(),
    }
}

/// One rpc method from the built envelope: its own id, body (the
/// variant's parameters), binding and source; everything else is shared.
fn rpc_op(
    cx: &mut Ctx<'_>,
    ns: &str,
    rpc: &RpcUnflatten,
    base: BuiltOp,
    sk: &Skeleton,
    variant: &RpcVariant,
) -> BuiltOp {
    let BuiltOp {
        mut op, responses, ..
    } = base;
    let id = format!("{ns}.{}", sk.local_id);
    let required = bodies::is_required(cx, &sk.target) && variant.params_required;
    op.body = variant.params.as_ref().map(|params| {
        let hint = [pascal(&variant.local_id), "Params".to_string()];
        let hint: Vec<&str> = hint.iter().map(String::as_str).collect();
        Body {
            content: vec![BodyContent {
                media_type: variant.media_type.clone(),
                ty: cx.tb.type_ref(ns, params, &hint),
                encoding: BodyEncoding::Json,
            }],
            required,
            doc: None,
        }
    });
    op.id = OperationId(id);
    op.name = Ident::new(
        variant
            .local_id
            .rsplit('.')
            .next()
            .unwrap_or(&variant.local_id),
    );
    op.operation_id = None;
    op.doc = variant.doc.clone();
    op.rpc = Some(RpcBinding {
        discriminator_field: rpc.discriminator.clone(),
        discriminator_value: variant.value.clone(),
        params_field: rpc.params.clone(),
        constants: variant.constants.clone(),
    });
    op.source = SourceRef {
        file: cx.ws.name(variant.variant.doc).to_string(),
        pointer: variant.variant.pointer.clone(),
    };
    BuiltOp {
        op,
        responses,
        at: variant.variant.clone(),
    }
}
