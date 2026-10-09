// SPDX-License-Identifier: AGPL-3.0-only
//! The arguments object of an operation as generated SDKs take it
//! (the TypeScript runtime's
//! `ParamDescriptor` and `BodyDescriptor` in `runtimes/ts/src/types.ts` are
//! its contract).
//!
//! This layout is the single source of truth for every emitter: the
//! TypeScript SDK's args types, request schemas and `BodyDescriptor`s, and
//! the tool manifests (`tools.json`, `llms-full.txt`, the MCP tools), so an
//! agent that follows a manifest builds exactly the object the SDK
//! validates.
//!
//! - Parameters are keyed by their TypeScript parameter name (`endpointId`;
//!   a reserved word is escaped, `class` → `class_`), in path, query,
//!   header, cookie order, made unique within the operation (`id`, `id2`).
//!   Parameters with the roles `idempotency_key`, `origin`, `auth` and
//!   `constant` are not arguments: the call options, the auth profile and
//!   the runtime (a constant's value) supply them, so credentials never
//!   travel through an agent's arguments. They are named
//!   after the arguments, for descriptors only.
//! - The body content is the first JSON one, else the first one.
//! - A JSON body whose type is a record without typed extras is merged:
//!   each field that is not read-only is a key of the arguments object
//!   under its wire name, when the body is required or every such field is
//!   optional (an optional body with a required field stays whole, so
//!   leaving it out stays possible) and no field name equals a parameter
//!   key. Any other body (bytes, text, form, unions, arrays, maps) is one
//!   argument, `body`, or `body2`, ... when a parameter takes that name.
//!
use tungsten_ir::naming::{self, Role, Target};
use tungsten_ir::{
    Additional, BodyContent, BodyEncoding, Field, Ident, Ir, Operation, Param, ParamRole, Presence,
    Shape, TypeRef,
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
    /// object, under their wire names (read-only fields are not).
    Merged {
        content: &'a BodyContent,
        fields: Vec<&'a Field>,
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
    /// Parameters supplied by the call options or the auth profile, named
    /// after the arguments (informational, for descriptors).
    pub supplied: Vec<ArgParam<'a>>,
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
        ParamRole::IdempotencyKey | ParamRole::Origin | ParamRole::Auth | ParamRole::Constant
    )
}

/// The header text a `constant` parameter carries (strings as they are,
/// numbers and booleans as JSON text); `None` for any other parameter.
pub fn constant_text(param: &Param) -> Option<String> {
    if param.role != ParamRole::Constant {
        return None;
    }
    match param.constant.as_ref()? {
        serde_json::Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// The arguments object of `op`.
pub fn args_layout<'a>(ir: &'a Ir, op: &'a Operation) -> ArgsLayout<'a> {
    let groups = [
        (ParamLocation::Path, &op.params.path),
        (ParamLocation::Query, &op.params.query),
        (ParamLocation::Header, &op.params.header),
        (ParamLocation::Cookie, &op.params.cookie),
    ];
    let located: Vec<(ParamLocation, &'a Param)> = groups
        .into_iter()
        .flat_map(|(location, list)| list.iter().map(move |p| (location, p)))
        .collect();
    // Arguments first, so they keep their names; supplied ones follow.
    let ordered: Vec<(ParamLocation, &'a Param)> = located
        .iter()
        .filter(|(_, p)| !is_supplied_param(p))
        .chain(located.iter().filter(|(_, p)| is_supplied_param(p)))
        .copied()
        .collect();
    let mut idents: Vec<Ident> = ordered
        .iter()
        .map(|(_, p)| Ident {
            wire: p.name.words.join(" "),
            words: p.name.words.clone(),
        })
        .collect();
    naming::disambiguate(&mut idents, Target::TypeScript, Role::Param);
    let (params, supplied): (Vec<ArgParam<'a>>, Vec<ArgParam<'a>>) = ordered
        .iter()
        .zip(&idents)
        .map(|(&(location, param), ident)| ArgParam {
            key: naming::render(ident, Target::TypeScript, Role::Param),
            location,
            param,
        })
        .partition(|a| !is_supplied_param(a.param));
    let keys: Vec<&str> = params.iter().map(|p| p.key.as_str()).collect();

    let body = op.body.as_ref().and_then(|b| {
        let content = b
            .content
            .iter()
            .find(|c| c.encoding == BodyEncoding::Json)
            .or_else(|| b.content.first())?;
        let merged = match (content.encoding, resolve(ir, &content.ty)) {
            (BodyEncoding::Json, Some(Shape::Record { fields, additional }))
                if !matches!(additional, Additional::Typed { .. }) =>
            {
                let sent: Vec<&'a Field> = fields.iter().filter(|f| !f.read_only).collect();
                let optional = |f: &&Field| {
                    matches!(f.presence, Presence::Optional | Presence::OptionalNullable)
                };
                ((b.required || sent.iter().all(optional))
                    && sent.iter().all(|f| !keys.contains(&f.wire_name.as_str())))
                .then_some((sent, additional))
            }
            _ => None,
        };
        Some(match merged {
            Some((fields, additional)) => BodyArg::Merged {
                content,
                fields,
                additional,
            },
            None => BodyArg::Arg {
                key: free_key(&keys, "body"),
                content,
            },
        })
    });
    ArgsLayout {
        params,
        supplied,
        body,
        body_required: op.body.as_ref().is_some_and(|b| b.required),
    }
}

/// `base` rendered as a TypeScript parameter name, with the smallest
/// numeric suffix (`body2`, `body3`) that no key in `taken` uses.
fn free_key(taken: &[&str], base: &str) -> String {
    let mut idents: Vec<Ident> = taken.iter().map(|t| Ident::new(*t)).collect();
    idents.push(Ident::new(base));
    naming::disambiguate(&mut idents, Target::TypeScript, Role::Param);
    idents
        .last()
        .map(|i| naming::render(i, Target::TypeScript, Role::Param))
        .unwrap_or_else(|| base.to_string())
}
