// SPDX-License-Identifier: AGPL-3.0-only
//! The arguments object of an operation as generated SDKs take it
//! (planning/05 "Method shape", `runtimes/ts/src/types.ts`
//! `ParamDescriptor` and `BodyDescriptor`).
//!
//! - Parameters are keyed by their camelCase name (`endpointId`), in path,
//!   query, header, cookie order. Parameters with the roles
//!   `idempotency_key`, `origin` and `auth` are not arguments: the call
//!   options and the auth profile supply them, so credentials never travel
//!   through an agent's arguments.
//! - A JSON body whose type is a record without typed extras is merged:
//!   each field is a key of the arguments object under its wire name. Any
//!   other body (bytes, text, form, unions, arrays, maps, or a record with
//!   a field whose name collides with a parameter key) is one argument,
//!   `body` (or the first free name of `requestBody`, `payload`).
//!
//! Emitters that describe arguments (tool manifests, MCP tools, SDK
//! methods) use [`args_layout`] so they agree on the same object.

use std::collections::BTreeSet;

use tungsten_ir::naming::{Case, Role, Target, to_case};
use tungsten_ir::{
    Additional, BodyContent, BodyEncoding, Field, Ir, Operation, Param, ParamRole, Shape, TypeRef,
};

/// Where a parameter goes on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ParamLocation {
    Path,
    Query,
    Header,
    Cookie,
}

impl ParamLocation {
    pub fn as_str(self) -> &'static str {
        match self {
            ParamLocation::Path => "path",
            ParamLocation::Query => "query",
            ParamLocation::Header => "header",
            ParamLocation::Cookie => "cookie",
        }
    }
}

/// One parameter that is a key of the arguments object.
#[derive(Debug, Clone, PartialEq)]
pub struct ArgParam<'a> {
    /// Key in the arguments object.
    pub key: String,
    pub location: ParamLocation,
    pub param: &'a Param,
}

/// How the request body appears in the arguments object.
#[derive(Debug, Clone, PartialEq)]
pub enum BodyArg<'a> {
    /// The body is a JSON object whose fields are keys of the arguments
    /// object, under their wire names.
    Merged {
        content: &'a BodyContent,
        fields: &'a [Field],
        additional: &'a Additional,
    },
    /// The whole body is the argument `key`.
    Arg {
        key: String,
        content: &'a BodyContent,
    },
}

/// The arguments object of one operation.
#[derive(Debug, Clone, PartialEq)]
pub struct ArgsLayout<'a> {
    pub params: Vec<ArgParam<'a>>,
    pub body: Option<BodyArg<'a>>,
    /// Whether the body must be given.
    pub body_required: bool,
}

/// Follow named types to the shape a reference denotes. A dangling or
/// circular alias resolves to `None`.
pub fn resolve<'a>(ir: &'a Ir, ty: &'a TypeRef) -> Option<&'a Shape> {
    let mut cur = ty;
    for _ in 0..=ir.types.types.len() {
        match cur {
            TypeRef::Inline(shape) => return Some(shape),
            TypeRef::Named(id) => match &ir.types.get(id)?.shape {
                Shape::Nullable { inner } => cur = inner,
                shape => return Some(shape),
            },
        }
    }
    None
}

/// Whether a parameter is supplied by the call options or the auth profile
/// instead of the arguments object.
pub fn is_supplied_param(param: &Param) -> bool {
    matches!(
        param.role,
        ParamRole::IdempotencyKey | ParamRole::Origin | ParamRole::Auth
    )
}

/// The arguments object of `op`.
pub fn args_layout<'a>(ir: &'a Ir, op: &'a Operation) -> ArgsLayout<'a> {
    let groups = [
        (ParamLocation::Path, &op.params.path),
        (ParamLocation::Query, &op.params.query),
        (ParamLocation::Header, &op.params.header),
        (ParamLocation::Cookie, &op.params.cookie),
    ];
    let mut used = BTreeSet::new();
    let mut params = vec![];
    for (location, list) in groups {
        for param in list.iter().filter(|p| !is_supplied_param(p)) {
            let base = param.name.render(Target::TypeScript, Role::Field);
            let qualified = {
                let mut words = vec![location.as_str().to_string()];
                words.extend(param.name.words.iter().cloned());
                to_case(&words, Case::Camel)
            };
            let key = free_key(&used, [base, qualified]);
            used.insert(key.clone());
            params.push(ArgParam {
                key,
                location,
                param,
            });
        }
    }
    let body = op
        .body
        .as_ref()
        .and_then(|b| b.content.first())
        .map(|content| {
            let record = match (content.encoding, resolve(ir, &content.ty)) {
                (BodyEncoding::Json, Some(Shape::Record { fields, additional }))
                    if !matches!(additional, Additional::Typed { .. })
                        && fields.iter().all(|f| !used.contains(&f.wire_name)) =>
                {
                    Some((fields.as_slice(), additional))
                }
                _ => None,
            };
            match record {
                Some((fields, additional)) => BodyArg::Merged {
                    content,
                    fields,
                    additional,
                },
                None => BodyArg::Arg {
                    key: free_key(&used, ["body", "requestBody", "payload"].map(String::from)),
                    content,
                },
            }
        });
    ArgsLayout {
        params,
        body,
        body_required: op.body.as_ref().is_some_and(|b| b.required),
    }
}

/// The first candidate not in `used`, else the last one with the smallest
/// free numeric suffix.
fn free_key<const N: usize>(used: &BTreeSet<String>, candidates: [String; N]) -> String {
    if let Some(free) = candidates.iter().find(|c| !used.contains(*c)) {
        return free.clone();
    }
    let last = candidates.last().cloned().unwrap_or_default();
    (2..)
        .map(|n| format!("{last}{n}"))
        .find(|k| !used.contains(k))
        .unwrap_or(last)
}
