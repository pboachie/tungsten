// SPDX-License-Identifier: AGPL-3.0-only
//! The API surface snapshot (`.tungsten/surface.json`) and semantic
//! versioning of its changes (`tungsten diff --semver`).
//!
//! [`ApiSurface::of`] reduces the IR to what callers of a generated SDK
//! depend on: every callable operation with its accessor (namespace,
//! resource path and method name, which every SDK names it by), its
//! arguments (the shared [`crate::args`] layout: names, presence, types),
//! whether it needs a caller-owned idempotency key, its safety tier, its
//! success response shape; every named type (union discriminators and
//! variant tags included); every macro's signature. A target that serves
//! agent tools adds their names ([`ApiSurface::with_tools`]). Documentation
//! is kept only as digests, so a docs-only change is visible without
//! storing text.
//! [`write_output`](crate::write_output) stores it next to the output
//! manifest when [`WriteOptions::ir`](crate::WriteOptions::ir) is set.
//!
//! [`compare`] classifies the differences between two snapshots. A type
//! change is judged by where the type is used: a request type (arguments)
//! may widen but not narrow, a response type may narrow but not widen.
//!
//! Major (callers can break):
//! - an operation, macro or named type is removed; an operation's accessor
//!   changes (a method renamed or moved to another resource); an agent tool
//!   name disappears or names another operation;
//! - a required argument is added (also a newly required caller-owned
//!   idempotency key), an optional argument becomes required, an argument
//!   is removed, the arguments object stops accepting extra keys;
//! - a request type narrows (a format, a tighter or exclusive bound, a
//!   `multipleOf` that the old one does not divide, `uniqueItems`, `number`
//!   → `integer`, nullability, a union variant or a discriminator tag
//!   removed, any → a type) or changes incompatibly (`string` →
//!   `integer`, another discriminator property); an enum value is removed
//!   from a request; a required field is added to a request object;
//! - a response field is removed or becomes optional or nullable, a
//!   response type widens (a union variant or tag added, any) or changes
//!   incompatibly, a response body disappears;
//! - an operation starts to need confirmation (its tier becomes
//!   `destructive` or `irreversible`), or becomes `read_only` (its preview
//!   and its literal `safety` go away);
//! - a macro's input or output changes.
//!
//! Minor (additive): an operation, macro, named type or tool is added; an
//! optional argument or request field is added; a requirement is relaxed;
//! a request type widens; a response type narrows; an enum value is added
//! (in a request or a response: clients must tolerate unknown values); a
//! response field or body is added; another safety tier change.
//!
//! Patch: documentation, the HTTP method or path, a runtime gate, or the
//! steps of a macro change, with the same signatures.
//!
//! None: the snapshots are equal.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tungsten_core::Digest;
use tungsten_ir::{
    Additional, BodyEncoding, Constraints, IdempotencyKind, Ir, Operation, OperationStatus,
    Presence, Primitive, Resource, ResponseKind, Safety, Shape, StringFormat, TypeRef,
};

use crate::args::{BodyArg, args_layout};

/// Where the snapshot lives, relative to the output directory.
pub const SURFACE_PATH: &str = ".tungsten/surface.json";

/// Version of the [`ApiSurface`] format.
pub const SURFACE_FORMAT: u32 = 2;

/// What callers of the generated code depend on. Maps are sorted by key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiSurface {
    /// [`SURFACE_FORMAT`].
    pub format: u32,
    /// API machine name.
    pub api: String,
    /// Callable operations by IR id.
    pub operations: BTreeMap<String, OperationSurface>,
    /// Macros by name.
    #[serde(default)]
    pub macros: BTreeMap<String, MacroSurface>,
    /// Named types by IR id.
    pub types: BTreeMap<String, TypeSurface>,
    /// Agent tool names (MCP) and the operation id or macro name each one
    /// calls, for targets that serve tools; empty otherwise.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationSurface {
    /// How SDKs reach the operation: namespace, resource path and method
    /// name (`pets.items.list`), as snake_case identifiers.
    pub accessor: String,
    pub method: String,
    pub path: String,
    /// Callable only when a runtime gate is on.
    #[serde(default, skip_serializing_if = "is_false")]
    pub gated: bool,
    pub safety: Safety,
    /// A caller-owned idempotency key is required.
    #[serde(default, skip_serializing_if = "is_false")]
    pub key_required: bool,
    /// Keys of the arguments object.
    pub args: BTreeMap<String, ArgSurface>,
    /// The arguments object accepts keys beyond `args` (an open merged
    /// body).
    #[serde(default, skip_serializing_if = "is_false")]
    pub extra_args: bool,
    /// The request body type when its fields are merged into `args`: the
    /// fields are compared as arguments, the type tells which named types
    /// are sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Ty>,
    /// The JSON success body, when the operation declares one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Ty>,
    /// Digest of the operation's documentation.
    pub docs: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArgSurface {
    /// `path`, `query`, `header`, `cookie` or `body`.
    #[serde(rename = "in")]
    pub location: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    pub ty: Ty,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MacroSurface {
    pub safety: Safety,
    /// The macro's input declaration (canonical form).
    pub input: Value,
    /// The macro's output expression.
    pub output: Value,
    /// Digest of the steps.
    pub steps: String,
    /// Digest of the summary.
    pub docs: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeSurface {
    pub ty: Ty,
    /// Digest of the type's documentation (type, fields, enum values).
    pub docs: String,
}

/// A type as callers see it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Ty {
    /// A named type (see [`ApiSurface::types`]).
    Ref {
        id: String,
    },
    String {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        #[serde(default, skip_serializing_if = "Bounds::is_empty")]
        bounds: Bounds,
    },
    Integer {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        #[serde(default, skip_serializing_if = "Bounds::is_empty")]
        bounds: Bounds,
    },
    Number {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        #[serde(default, skip_serializing_if = "Bounds::is_empty")]
        bounds: Bounds,
    },
    Bool,
    Bytes,
    Enum {
        values: Vec<Value>,
    },
    Const {
        value: Value,
    },
    Array {
        items: Box<Ty>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<u64>,
        #[serde(default, skip_serializing_if = "is_false")]
        unique: bool,
    },
    Map {
        values: Box<Ty>,
    },
    Object {
        fields: BTreeMap<String, FieldSurface>,
        /// `closed`, `open`, or the type of extra values.
        extra: Extra,
    },
    Union {
        variants: Vec<Ty>,
        /// The discriminator property of a tagged union.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        discriminator: Option<String>,
        /// The tag of each variant (same order), when any is tagged.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tags: Vec<Option<String>>,
    },
    Intersection {
        members: Vec<Ty>,
    },
    Nullable {
        inner: Box<Ty>,
    },
    Any,
    Never,
}

/// Extra keys of an object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Extra {
    Closed,
    Open,
    Typed(Box<Ty>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldSurface {
    pub ty: Ty,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub nullable: bool,
    /// Sent only in responses.
    #[serde(default, skip_serializing_if = "is_false")]
    pub read_only: bool,
    /// Sent only in requests.
    #[serde(default, skip_serializing_if = "is_false")]
    pub write_only: bool,
}

/// Value bounds of a scalar.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bounds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive_minimum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive_maximum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiple_of: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

impl Bounds {
    fn is_empty(&self) -> bool {
        *self == Bounds::default()
    }

    fn of(c: &Constraints) -> Bounds {
        let num = |n: &Option<serde_json::Number>| n.as_ref().and_then(serde_json::Number::as_f64);
        Bounds {
            min_length: c.min_length,
            max_length: c.max_length,
            minimum: num(&c.minimum),
            maximum: num(&c.maximum),
            exclusive_minimum: num(&c.exclusive_minimum),
            exclusive_maximum: num(&c.exclusive_maximum),
            multiple_of: num(&c.multiple_of),
            pattern: c.pattern.clone(),
        }
    }

    /// The effective lower bound: its value and whether it is exclusive
    /// (the tighter of `minimum` and `exclusiveMinimum`).
    fn lower(&self) -> Option<(f64, bool)> {
        match (self.minimum, self.exclusive_minimum) {
            (Some(m), Some(e)) if e >= m => Some((e, true)),
            (Some(m), _) => Some((m, false)),
            (None, Some(e)) => Some((e, true)),
            (None, None) => None,
        }
    }

    /// The effective upper bound: its value and whether it is exclusive.
    fn upper(&self) -> Option<(f64, bool)> {
        match (self.maximum, self.exclusive_maximum) {
            (Some(m), Some(e)) if e <= m => Some((e, true)),
            (Some(m), _) => Some((m, false)),
            (None, Some(e)) => Some((e, true)),
            (None, None) => None,
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn digest_of(value: &Value) -> String {
    if value.is_null() {
        return String::new();
    }
    Digest::of(value.to_string().as_bytes()).short().to_string()
}

impl ApiSurface {
    /// The surface of `ir`.
    pub fn of(ir: &Ir) -> ApiSurface {
        let accessors = accessors(ir);
        let operations = ir
            .operations()
            .into_iter()
            .filter(|op| !matches!(op.status, OperationStatus::Planned { .. }))
            .map(|op| {
                let accessor = accessors.get(op.id.0.as_str()).cloned().unwrap_or_default();
                (op.id.0.clone(), operation(ir, op, accessor))
            })
            .collect();
        let macros = ir
            .agent
            .macros
            .iter()
            .map(|m| {
                (
                    m.name.0.clone(),
                    MacroSurface {
                        safety: m.safety,
                        input: m.input.clone(),
                        output: m.output.clone(),
                        steps: digest_of(&m.steps),
                        docs: digest_of(&Value::String(m.summary.clone())),
                    },
                )
            })
            .collect();
        let types = ir
            .types
            .types
            .iter()
            .map(|t| {
                let mut docs = vec![serde_json::to_value(&t.doc).unwrap_or_default()];
                match &t.shape {
                    Shape::Record { fields, .. } => docs.extend(
                        fields
                            .iter()
                            .map(|f| serde_json::to_value(&f.doc).unwrap_or_default()),
                    ),
                    Shape::Enum { values, .. } => docs.extend(
                        values
                            .iter()
                            .map(|v| serde_json::to_value(&v.doc).unwrap_or_default()),
                    ),
                    _ => {}
                }
                (
                    t.id.0.clone(),
                    TypeSurface {
                        ty: shape_ty(&t.shape),
                        docs: digest_of(&Value::Array(docs)),
                    },
                )
            })
            .collect();
        ApiSurface {
            format: SURFACE_FORMAT,
            api: ir.api.name.wire.clone(),
            operations,
            macros,
            types,
            tools: BTreeMap::new(),
        }
    }

    /// The surface with the agent tool names a target serves (tool name →
    /// operation id or macro name).
    pub fn with_tools(mut self, tools: BTreeMap<String, String>) -> ApiSurface {
        self.tools = tools;
        self
    }

    /// Compact JSON with a trailing newline.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(self).unwrap_or_default();
        bytes.push(b'\n');
        bytes
    }

    /// The snapshot the last generation stored in `out_dir`; `Ok(None)`
    /// when there is none. Fails when it cannot be read or parsed.
    pub fn read(out_dir: &Path) -> Result<Option<ApiSurface>, String> {
        let path = out_dir.join(SURFACE_PATH);
        match std::fs::read(&path) {
            Ok(bytes) => ApiSurface::parse(&bytes)
                .map(Some)
                .map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }

    /// Parse a snapshot. Fails on malformed JSON or an unknown format.
    pub fn parse(bytes: &[u8]) -> Result<ApiSurface, String> {
        let surface: ApiSurface = serde_json::from_slice(bytes)
            .map_err(|e| format!("not a tungsten API surface: {e}"))?;
        if surface.format != SURFACE_FORMAT {
            return Err(format!(
                "surface format {} is not supported (expected {SURFACE_FORMAT})",
                surface.format
            ));
        }
        Ok(surface)
    }
}

/// `namespace.resource….method` of every operation, by IR id.
fn accessors(ir: &Ir) -> BTreeMap<&str, String> {
    fn walk<'a>(r: &'a Resource, path: &mut Vec<String>, out: &mut BTreeMap<&'a str, String>) {
        path.push(r.name.snake());
        for op in &r.operations {
            out.insert(
                op.id.0.as_str(),
                format!("{}.{}", path.join("."), op.name.snake()),
            );
        }
        for c in &r.children {
            walk(c, path, out);
        }
        path.pop();
    }
    let mut out = BTreeMap::new();
    for ns in &ir.namespaces {
        for r in &ns.resources {
            walk(r, &mut vec![ns.name.snake()], &mut out);
        }
    }
    out
}

fn operation(ir: &Ir, op: &Operation, accessor: String) -> OperationSurface {
    let layout = args_layout(ir, op);
    let mut args = BTreeMap::new();
    for p in &layout.params {
        args.insert(
            p.key.clone(),
            ArgSurface {
                location: p.location.as_str().to_string(),
                required: p.param.required,
                ty: ty_of(&p.param.ty),
            },
        );
    }
    let mut extra_args = false;
    let mut body = None;
    match &layout.body {
        Some(BodyArg::Merged {
            content,
            fields,
            additional,
        }) => {
            body = Some(ty_of(&content.ty));
            for f in fields {
                let required = layout.body_required
                    && matches!(f.presence, Presence::Required | Presence::RequiredNullable);
                args.insert(
                    f.wire_name.clone(),
                    ArgSurface {
                        location: "body".into(),
                        required,
                        ty: with_null(ty_of(&f.ty), f.presence),
                    },
                );
            }
            extra_args = !matches!(additional, Additional::Closed);
        }
        Some(BodyArg::Arg { key, content }) => {
            args.insert(
                key.clone(),
                ArgSurface {
                    location: "body".into(),
                    required: layout.body_required,
                    ty: ty_of(&content.ty),
                },
            );
        }
        None => {}
    }
    let idem = &op.agent.idempotency;
    let response = op
        .responses
        .iter()
        .filter(|r| matches!(r.kind, ResponseKind::Success))
        .flat_map(|r| r.content.iter())
        .find(|c| matches!(c.encoding, BodyEncoding::Json | BodyEncoding::Jsonl))
        .map(|c| ty_of(&c.value_type()));
    let docs = serde_json::json!([op.doc, op.agent.compact_doc]);
    OperationSurface {
        accessor,
        method: serde_json::to_value(op.method)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default(),
        path: op.path.raw.clone(),
        gated: matches!(op.status, OperationStatus::Gated { .. }),
        safety: op.agent.safety,
        key_required: idem.policy == IdempotencyKind::CallerOwned && idem.persist_required,
        args,
        extra_args,
        body,
        response,
        docs: digest_of(&docs),
    }
}

fn with_null(ty: Ty, presence: Presence) -> Ty {
    match presence {
        Presence::RequiredNullable | Presence::OptionalNullable => Ty::Nullable {
            inner: Box::new(ty),
        },
        Presence::Required | Presence::Optional => ty,
    }
}

fn ty_of(ty: &TypeRef) -> Ty {
    match ty {
        TypeRef::Named(id) => Ty::Ref { id: id.0.clone() },
        TypeRef::Inline(shape) => shape_ty(shape),
    }
}

fn format_name(format: &StringFormat) -> String {
    match format {
        StringFormat::Other(other) => other.clone(),
        known => serde_json::to_value(known)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default(),
    }
}

fn shape_ty(shape: &Shape) -> Ty {
    match shape {
        Shape::Primitive {
            primitive,
            constraints,
        } => {
            let bounds = Bounds::of(constraints);
            let named = |s: &str| Some(s.to_string());
            match primitive {
                Primitive::String { format } => Ty::String {
                    format: format.as_ref().map(format_name),
                    bounds,
                },
                Primitive::Int32 => Ty::Integer {
                    format: named("int32"),
                    bounds,
                },
                Primitive::Int64 => Ty::Integer {
                    format: named("int64"),
                    bounds,
                },
                Primitive::Integer => Ty::Integer {
                    format: None,
                    bounds,
                },
                Primitive::Float => Ty::Number {
                    format: named("float"),
                    bounds,
                },
                Primitive::Double => Ty::Number {
                    format: named("double"),
                    bounds,
                },
                Primitive::Number => Ty::Number {
                    format: None,
                    bounds,
                },
                Primitive::Bool => Ty::Bool,
                Primitive::Bytes => Ty::Bytes,
            }
        }
        Shape::Enum { values, .. } => Ty::Enum {
            values: values.iter().map(|v| v.value.clone()).collect(),
        },
        Shape::Const { value } => Ty::Const {
            value: value.clone(),
        },
        Shape::Array {
            items,
            min,
            max,
            unique,
        } => Ty::Array {
            items: Box::new(ty_of(items)),
            min: *min,
            max: *max,
            unique: *unique,
        },
        Shape::Map { values } => Ty::Map {
            values: Box::new(ty_of(values)),
        },
        Shape::Record { fields, additional } => Ty::Object {
            fields: fields
                .iter()
                .map(|f| {
                    (
                        f.wire_name.clone(),
                        FieldSurface {
                            ty: ty_of(&f.ty),
                            required: matches!(
                                f.presence,
                                Presence::Required | Presence::RequiredNullable
                            ),
                            nullable: matches!(
                                f.presence,
                                Presence::RequiredNullable | Presence::OptionalNullable
                            ),
                            read_only: f.read_only,
                            write_only: f.write_only,
                        },
                    )
                })
                .collect(),
            extra: match additional {
                Additional::Closed => Extra::Closed,
                Additional::Open => Extra::Open,
                Additional::Typed { values } => Extra::Typed(Box::new(ty_of(values))),
            },
        },
        Shape::Union(u) => Ty::Union {
            variants: u.variants.iter().map(|v| ty_of(&v.ty)).collect(),
            discriminator: u.discriminator.as_ref().map(|d| d.property.clone()),
            tags: if u.variants.iter().any(|v| v.tag.is_some()) {
                u.variants.iter().map(|v| v.tag.clone()).collect()
            } else {
                Vec::new()
            },
        },
        Shape::Intersection { members } => Ty::Intersection {
            members: members.iter().map(ty_of).collect(),
        },
        Shape::Nullable { inner } => Ty::Nullable {
            inner: Box::new(ty_of(inner)),
        },
        Shape::Any => Ty::Any,
        Shape::Never => Ty::Never,
    }
}

// ── comparison ─────────────────────────────────────────────────────────────

/// How much a change matters to callers (semantic versioning).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    None,
    Patch,
    Minor,
    Major,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::None => "none",
            Level::Patch => "patch",
            Level::Minor => "minor",
            Level::Major => "major",
        }
    }
}

/// One classified difference between two snapshots.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SurfaceChange {
    pub level: Level,
    /// Stable rule id (`operation_removed`, `required_arg_added`, ...).
    pub rule: String,
    /// What changed: an operation id, `operation(arg)`, a type id, with a
    /// path into the type (`.field`, `[]`, `{}`).
    pub subject: String,
    pub detail: String,
}

/// The overall level of `changes`: the highest one, `none` when empty.
pub fn classify(changes: &[SurfaceChange]) -> Level {
    changes.iter().map(|c| c.level).max().unwrap_or(Level::None)
}

/// Where a type is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Polarity {
    /// Sent by the caller: may widen.
    Request,
    /// Received by the caller: may narrow.
    Response,
}

/// Every difference from `old` to `new`, most severe first, then by
/// subject.
pub fn compare(old: &ApiSurface, new: &ApiSurface) -> Vec<SurfaceChange> {
    let mut cx = Cx {
        old,
        new,
        out: BTreeSet::new(),
        seen: BTreeSet::new(),
    };
    cx.operations();
    cx.macros();
    cx.types();
    cx.tools();
    let mut changes: Vec<SurfaceChange> = cx.out.into_iter().collect();
    changes.sort_by(|a, b| {
        b.level
            .cmp(&a.level)
            .then_with(|| a.subject.cmp(&b.subject))
            .then_with(|| a.rule.cmp(&b.rule))
            .then_with(|| a.detail.cmp(&b.detail))
    });
    changes
}

struct Cx<'a> {
    old: &'a ApiSurface,
    new: &'a ApiSurface,
    out: BTreeSet<SurfaceChange>,
    /// Named type pairs already compared at a use site, per polarity.
    seen: BTreeSet<(String, String, Polarity)>,
}

fn needs_confirmation(safety: Safety) -> bool {
    matches!(safety, Safety::Destructive | Safety::Irreversible)
}

fn safety_name(safety: Safety) -> String {
    serde_json::to_value(safety)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

impl Cx<'_> {
    fn push(&mut self, level: Level, rule: &str, subject: &str, detail: impl Into<String>) {
        self.out.insert(SurfaceChange {
            level,
            rule: rule.to_string(),
            subject: subject.to_string(),
            detail: detail.into(),
        });
    }

    fn safety(&mut self, subject: &str, old: Safety, new: Safety) {
        if old == new {
            return;
        }
        let detail = format!("{} → {}", safety_name(old), safety_name(new));
        if needs_confirmation(new) && !needs_confirmation(old) {
            self.push(Level::Major, "confirmation_required", subject, detail);
        } else if new == Safety::ReadOnly {
            // SDKs offer `preview()` and a literal `safety` only on
            // non-read-only operations, and MCP previews only those tools.
            self.push(Level::Major, "preview_removed", subject, detail);
        } else {
            self.push(Level::Minor, "safety_changed", subject, detail);
        }
    }

    fn operations(&mut self) {
        let (old, new) = (self.old, self.new);
        for (id, o) in &old.operations {
            let Some(n) = new.operations.get(id) else {
                self.push(Level::Major, "operation_removed", id, "operation removed");
                continue;
            };
            if o.accessor != n.accessor {
                self.push(
                    Level::Major,
                    "operation_renamed",
                    id,
                    format!("{} → {}", o.accessor, n.accessor),
                );
            }
            if (&o.method, &o.path) != (&n.method, &n.path) {
                self.push(
                    Level::Patch,
                    "wire_binding_changed",
                    id,
                    format!("{} {} → {} {}", o.method, o.path, n.method, n.path),
                );
            }
            if o.gated != n.gated {
                let detail = if n.gated {
                    "now behind a runtime gate"
                } else {
                    "no longer behind a runtime gate"
                };
                self.push(Level::Patch, "gate_changed", id, detail);
            }
            if o.docs != n.docs {
                self.push(Level::Patch, "docs_changed", id, "documentation changed");
            }
            self.safety(id, o.safety, n.safety);
            match (o.key_required, n.key_required) {
                (false, true) => self.push(
                    Level::Major,
                    "required_arg_added",
                    id,
                    "a caller-owned idempotency key is now required",
                ),
                (true, false) => self.push(
                    Level::Minor,
                    "requirement_relaxed",
                    id,
                    "the idempotency key is no longer required",
                ),
                _ => {}
            }
            self.args(id, o, n);
            let subject = format!("{id} response");
            match (&o.response, &n.response) {
                (Some(_), None) => self.push(
                    Level::Major,
                    "response_removed",
                    &subject,
                    "the success body is no longer declared",
                ),
                (None, Some(_)) => self.push(
                    Level::Minor,
                    "response_added",
                    &subject,
                    "a success body is now declared",
                ),
                (Some(a), Some(b)) => self.ty(&subject, a, b, Polarity::Response),
                (None, None) => {}
            }
        }
        for id in new.operations.keys() {
            if !old.operations.contains_key(id) {
                self.push(Level::Minor, "operation_added", id, "operation added");
            }
        }
    }

    fn args(&mut self, id: &str, o: &OperationSurface, n: &OperationSurface) {
        for (key, a) in &o.args {
            let subject = format!("{id}({key})");
            let Some(b) = n.args.get(key) else {
                self.push(Level::Major, "arg_removed", &subject, "argument removed");
                continue;
            };
            if a.location != b.location {
                self.push(
                    Level::Patch,
                    "wire_binding_changed",
                    &subject,
                    format!("sent in {} → {}", a.location, b.location),
                );
            }
            match (a.required, b.required) {
                (false, true) => self.push(
                    Level::Major,
                    "arg_became_required",
                    &subject,
                    "optional → required",
                ),
                (true, false) => self.push(
                    Level::Minor,
                    "requirement_relaxed",
                    &subject,
                    "required → optional",
                ),
                _ => {}
            }
            self.ty(&subject, &a.ty, &b.ty, Polarity::Request);
        }
        for (key, b) in &n.args {
            if o.args.contains_key(key) {
                continue;
            }
            let subject = format!("{id}({key})");
            if b.required {
                self.push(
                    Level::Major,
                    "required_arg_added",
                    &subject,
                    "required argument added",
                );
            } else {
                self.push(
                    Level::Minor,
                    "optional_arg_added",
                    &subject,
                    "optional argument added",
                );
            }
        }
        match (o.extra_args, n.extra_args) {
            (true, false) => self.push(
                Level::Major,
                "arg_type_narrowed",
                id,
                "extra argument keys are no longer accepted",
            ),
            (false, true) => self.push(
                Level::Minor,
                "arg_type_widened",
                id,
                "extra argument keys are now accepted",
            ),
            _ => {}
        }
    }

    fn macros(&mut self) {
        let (old, new) = (self.old, self.new);
        for (name, o) in &old.macros {
            let Some(n) = new.macros.get(name) else {
                self.push(Level::Major, "macro_removed", name, "macro removed");
                continue;
            };
            self.safety(name, o.safety, n.safety);
            if o.input != n.input {
                self.push(
                    Level::Major,
                    "macro_signature_changed",
                    name,
                    "macro input changed",
                );
            }
            if o.output != n.output {
                self.push(
                    Level::Major,
                    "macro_signature_changed",
                    name,
                    "macro output changed",
                );
            }
            if o.steps != n.steps {
                self.push(
                    Level::Patch,
                    "macro_steps_changed",
                    name,
                    "macro steps changed",
                );
            }
            if o.docs != n.docs {
                self.push(Level::Patch, "docs_changed", name, "documentation changed");
            }
        }
        for name in new.macros.keys() {
            if !old.macros.contains_key(name) {
                self.push(Level::Minor, "macro_added", name, "macro added");
            }
        }
    }

    /// Agent tool names: a name that disappears or calls another operation
    /// breaks every client and prompt that uses it.
    fn tools(&mut self) {
        let (old, new) = (self.old, self.new);
        for (name, target) in &old.tools {
            match new.tools.get(name) {
                None => self.push(
                    Level::Major,
                    "tool_removed",
                    name,
                    format!("tool removed (it called {target})"),
                ),
                Some(now) if now != target => self.push(
                    Level::Major,
                    "tool_retargeted",
                    name,
                    format!("now calls {now} instead of {target}"),
                ),
                Some(_) => {}
            }
        }
        for (name, target) in &new.tools {
            if !old.tools.contains_key(name) {
                self.push(
                    Level::Minor,
                    "tool_added",
                    name,
                    format!("tool added (calls {target})"),
                );
            }
        }
    }

    /// Named types: removal, addition, documentation, and their structure
    /// under every polarity they are used with (responses when unused).
    fn types(&mut self) {
        let (old, new) = (self.old, self.new);
        let usage = polarities(old);
        for (id, o) in &old.types {
            let Some(n) = new.types.get(id) else {
                self.push(Level::Major, "type_removed", id, "type removed");
                continue;
            };
            if o.docs != n.docs {
                self.push(Level::Patch, "docs_changed", id, "documentation changed");
            }
            let used = usage.get(id.as_str()).cloned().unwrap_or_default();
            let used = if used.is_empty() {
                BTreeSet::from([Polarity::Response])
            } else {
                used
            };
            for polarity in used {
                self.ty(id, &o.ty, &n.ty, polarity);
            }
        }
        for id in new.types.keys() {
            if !old.types.contains_key(id) {
                self.push(Level::Minor, "type_added", id, "type added");
            }
        }
    }

    fn narrowed(&mut self, subject: &str, polarity: Polarity, detail: String) {
        match polarity {
            Polarity::Request => self.push(Level::Major, "arg_type_narrowed", subject, detail),
            Polarity::Response => {
                self.push(Level::Minor, "response_type_narrowed", subject, detail)
            }
        }
    }

    fn widened(&mut self, subject: &str, polarity: Polarity, detail: String) {
        match polarity {
            Polarity::Request => self.push(Level::Minor, "arg_type_widened", subject, detail),
            Polarity::Response => self.push(Level::Major, "response_type_widened", subject, detail),
        }
    }

    fn changed(&mut self, subject: &str, polarity: Polarity, old: &Ty, new: &Ty) {
        let rule = match polarity {
            Polarity::Request => "arg_type_changed",
            Polarity::Response => "response_type_changed",
        };
        self.push(
            Level::Major,
            rule,
            subject,
            format!("{} → {}", describe(old), describe(new)),
        );
    }

    fn resolve<'s>(&self, side: &'s ApiSurface, id: &str) -> Option<&'s Ty> {
        side.types.get(id).map(|t| &t.ty)
    }

    /// Compare one use of a type.
    fn ty(&mut self, subject: &str, old: &Ty, new: &Ty, polarity: Polarity) {
        if old == new {
            return;
        }
        match (old, new) {
            // The same named type is compared once, as a type.
            (Ty::Ref { id: a }, Ty::Ref { id: b }) if a == b => {}
            (Ty::Nullable { inner: a }, Ty::Nullable { inner: b }) => {
                self.ty(subject, a, b, polarity)
            }
            (Ty::Nullable { inner }, other) => {
                self.narrowed(subject, polarity, "no longer nullable".into());
                self.ty(subject, inner, other, polarity);
            }
            (other, Ty::Nullable { inner }) => {
                self.widened(subject, polarity, "now nullable".into());
                self.ty(subject, other, inner, polarity);
            }
            (Ty::Ref { id: a }, Ty::Ref { id: b }) => {
                if !self.seen.insert((a.clone(), b.clone(), polarity)) {
                    return;
                }
                match (self.resolve(self.old, a), self.resolve(self.new, b)) {
                    (Some(x), Some(y)) => self.ty(subject, x, y, polarity),
                    _ => self.changed(subject, polarity, old, new),
                }
            }
            (Ty::Ref { id }, other) => match self.resolve(self.old, id) {
                Some(x) => self.ty(subject, x, other, polarity),
                None => self.changed(subject, polarity, old, new),
            },
            (other, Ty::Ref { id }) => match self.resolve(self.new, id) {
                Some(y) => self.ty(subject, other, y, polarity),
                None => self.changed(subject, polarity, old, new),
            },
            (Ty::Any, _) => self.narrowed(subject, polarity, format!("any → {}", describe(new))),
            (_, Ty::Any) => self.widened(subject, polarity, format!("{} → any", describe(old))),
            (
                Ty::String {
                    format: fa,
                    bounds: ba,
                },
                Ty::String {
                    format: fb,
                    bounds: bb,
                },
            ) => {
                self.format(subject, polarity, fa, fb);
                self.bounds(subject, polarity, ba, bb);
            }
            (
                Ty::Integer {
                    format: fa,
                    bounds: ba,
                },
                Ty::Integer {
                    format: fb,
                    bounds: bb,
                },
            )
            | (
                Ty::Number {
                    format: fa,
                    bounds: ba,
                },
                Ty::Number {
                    format: fb,
                    bounds: bb,
                },
            ) => {
                self.number_format(subject, polarity, fa, fb);
                self.bounds(subject, polarity, ba, bb);
            }
            (Ty::Number { .. }, Ty::Integer { .. }) => {
                self.narrowed(subject, polarity, "number → integer".into())
            }
            (Ty::Integer { .. }, Ty::Number { .. }) => {
                self.widened(subject, polarity, "integer → number".into())
            }
            (Ty::Enum { values: a }, Ty::Enum { values: b }) => self.enums(subject, polarity, a, b),
            (Ty::Const { value }, Ty::Enum { values }) => {
                self.enums(subject, polarity, std::slice::from_ref(value), values)
            }
            (Ty::Enum { values }, Ty::Const { value }) => {
                self.enums(subject, polarity, values, std::slice::from_ref(value))
            }
            (Ty::Const { value: a }, Ty::Const { value: b }) => self.enums(
                subject,
                polarity,
                std::slice::from_ref(a),
                std::slice::from_ref(b),
            ),
            (Ty::String { .. }, Ty::Enum { .. } | Ty::Const { .. }) => {
                self.narrowed(subject, polarity, format!("string → {}", describe(new)))
            }
            (Ty::Enum { .. } | Ty::Const { .. }, Ty::String { .. }) => {
                self.widened(subject, polarity, format!("{} → string", describe(old)))
            }
            (
                Ty::Array {
                    items: a,
                    min: mina,
                    max: maxa,
                    unique: ua,
                },
                Ty::Array {
                    items: b,
                    min: minb,
                    max: maxb,
                    unique: ub,
                },
            ) => {
                self.ty(&format!("{subject}[]"), a, b, polarity);
                match (ua, ub) {
                    (false, true) => {
                        self.narrowed(subject, polarity, "items must be unique".into())
                    }
                    (true, false) => {
                        self.widened(subject, polarity, "items need not be unique".into())
                    }
                    _ => {}
                }
                let ba = Bounds {
                    min_length: *mina,
                    max_length: *maxa,
                    ..Bounds::default()
                };
                let bb = Bounds {
                    min_length: *minb,
                    max_length: *maxb,
                    ..Bounds::default()
                };
                self.bounds(subject, polarity, &ba, &bb);
            }
            (Ty::Map { values: a }, Ty::Map { values: b }) => {
                self.ty(&format!("{subject}{{}}"), a, b, polarity)
            }
            (
                Ty::Object {
                    fields: fa,
                    extra: ea,
                },
                Ty::Object {
                    fields: fb,
                    extra: eb,
                },
            ) => self.object(subject, polarity, (fa, ea), (fb, eb)),
            (
                Ty::Union {
                    variants: a,
                    discriminator: da,
                    tags: ta,
                },
                Ty::Union {
                    variants: b,
                    discriminator: db,
                    tags: tb,
                },
            ) => {
                self.union(subject, polarity, a, b);
                self.tags(subject, polarity, (da, ta), (db, tb));
            }
            (Ty::Union { variants, .. }, other) if variants.contains(other) => self.narrowed(
                subject,
                polarity,
                format!("union → its variant {}", describe(other)),
            ),
            (other, Ty::Union { variants, .. }) if variants.contains(other) => self.widened(
                subject,
                polarity,
                format!("{} → a union including it", describe(other)),
            ),
            _ => self.changed(subject, polarity, old, new),
        }
    }

    fn format(
        &mut self,
        subject: &str,
        polarity: Polarity,
        a: &Option<String>,
        b: &Option<String>,
    ) {
        match (a, b) {
            (None, Some(f)) => self.narrowed(subject, polarity, format!("format {f} added")),
            (Some(f), None) => self.widened(subject, polarity, format!("format {f} removed")),
            (Some(x), Some(y)) if x != y => {
                let rule = match polarity {
                    Polarity::Request => "arg_type_changed",
                    Polarity::Response => "response_type_changed",
                };
                self.push(Level::Major, rule, subject, format!("format {x} → {y}"));
            }
            _ => {}
        }
    }

    /// `int64` → `int32` and `double` → `float` narrow; the reverse widens.
    fn number_format(
        &mut self,
        subject: &str,
        polarity: Polarity,
        a: &Option<String>,
        b: &Option<String>,
    ) {
        let width = |f: &Option<String>| match f.as_deref() {
            Some("int32" | "float") => 1,
            Some("int64" | "double") => 2,
            _ => 3,
        };
        let (wa, wb) = (width(a), width(b));
        let show = |f: &Option<String>| f.clone().unwrap_or_else(|| "unbounded".into());
        let detail = format!("format {} → {}", show(a), show(b));
        if wb < wa {
            self.narrowed(subject, polarity, detail);
        } else if wb > wa {
            self.widened(subject, polarity, detail);
        }
    }

    fn bounds(&mut self, subject: &str, polarity: Polarity, a: &Bounds, b: &Bounds) {
        // A lower bound narrows when it rises or appears; an upper bound
        // when it falls or appears.
        let lower = |x: Option<f64>, y: Option<f64>| match (x, y) {
            (None, Some(_)) => Some(true),
            (Some(_), None) => Some(false),
            (Some(x), Some(y)) if y > x => Some(true),
            (Some(x), Some(y)) if y < x => Some(false),
            _ => None,
        };
        let upper = |x: Option<f64>, y: Option<f64>| match (x, y) {
            (None, Some(_)) => Some(true),
            (Some(_), None) => Some(false),
            (Some(x), Some(y)) if y < x => Some(true),
            (Some(x), Some(y)) if y > x => Some(false),
            _ => None,
        };
        // An exclusive bound at the same value is the tighter one.
        let lower_value = |x: Option<(f64, bool)>, y: Option<(f64, bool)>| match (x, y) {
            (Some((x, ex)), Some((y, ey))) if x == y && ex != ey => Some(ey),
            _ => lower(x.map(|b| b.0), y.map(|b| b.0)),
        };
        let upper_value = |x: Option<(f64, bool)>, y: Option<(f64, bool)>| match (x, y) {
            (Some((x, ex)), Some((y, ey))) if x == y && ex != ey => Some(ey),
            _ => upper(x.map(|b| b.0), y.map(|b| b.0)),
        };
        let as_f = |n: Option<u64>| n.map(|n| n as f64);
        let checks = [
            (
                "minimum length",
                lower(as_f(a.min_length), as_f(b.min_length)),
            ),
            (
                "maximum length",
                upper(as_f(a.max_length), as_f(b.max_length)),
            ),
            ("minimum", lower_value(a.lower(), b.lower())),
            ("maximum", upper_value(a.upper(), b.upper())),
        ];
        for (what, verdict) in checks {
            match verdict {
                Some(true) => self.narrowed(subject, polarity, format!("{what} tightened")),
                Some(false) => self.widened(subject, polarity, format!("{what} relaxed")),
                None => {}
            }
        }
        // Values must be multiples of the step: a new step narrows unless it
        // divides the old one.
        let divides =
            |step: f64, of: f64| step != 0.0 && ((of / step) - (of / step).round()).abs() < 1e-9;
        match (a.multiple_of, b.multiple_of) {
            (None, Some(m)) => self.narrowed(subject, polarity, format!("multipleOf {m} added")),
            (Some(m), None) => self.widened(subject, polarity, format!("multipleOf {m} removed")),
            (Some(x), Some(y)) if x != y => {
                let detail = format!("multipleOf {x} → {y}");
                if divides(y, x) {
                    self.widened(subject, polarity, detail);
                } else if divides(x, y) {
                    self.narrowed(subject, polarity, detail);
                } else {
                    let rule = match polarity {
                        Polarity::Request => "arg_type_changed",
                        Polarity::Response => "response_type_changed",
                    };
                    self.push(Level::Major, rule, subject, detail);
                }
            }
            _ => {}
        }
        match (&a.pattern, &b.pattern) {
            (None, Some(_)) => self.narrowed(subject, polarity, "pattern added".into()),
            (Some(_), None) => self.widened(subject, polarity, "pattern removed".into()),
            (Some(x), Some(y)) if x != y => {
                let rule = match polarity {
                    Polarity::Request => "arg_type_changed",
                    Polarity::Response => "response_type_changed",
                };
                self.push(Level::Major, rule, subject, "pattern changed");
            }
            _ => {}
        }
    }

    fn enums(&mut self, subject: &str, polarity: Polarity, a: &[Value], b: &[Value]) {
        for v in a.iter().filter(|v| !b.contains(v)) {
            let level = match polarity {
                Polarity::Request => Level::Major,
                Polarity::Response => Level::Minor,
            };
            self.push(
                level,
                "enum_value_removed",
                subject,
                format!("value {v} removed"),
            );
        }
        for v in b.iter().filter(|v| !a.contains(v)) {
            self.push(
                Level::Minor,
                "enum_value_added",
                subject,
                format!("value {v} added"),
            );
        }
    }

    fn union(&mut self, subject: &str, polarity: Polarity, a: &[Ty], b: &[Ty]) {
        let removed = a.iter().filter(|v| !b.contains(v)).count();
        let added = b.iter().filter(|v| !a.contains(v)).count();
        if removed > 0 {
            self.narrowed(
                subject,
                polarity,
                format!("{removed} union variant(s) removed"),
            );
        }
        if added > 0 {
            self.widened(subject, polarity, format!("{added} union variant(s) added"));
        }
    }

    /// Discriminator tags of a tagged union: the property name is part of
    /// every tagged value; a removed tag narrows, an added one widens.
    fn tags(
        &mut self,
        subject: &str,
        polarity: Polarity,
        (da, ta): (&Option<String>, &[Option<String>]),
        (db, tb): (&Option<String>, &[Option<String>]),
    ) {
        if da != db {
            let show = |d: &Option<String>| d.clone().unwrap_or_else(|| "none".into());
            let rule = match polarity {
                Polarity::Request => "arg_type_changed",
                Polarity::Response => "response_type_changed",
            };
            self.push(
                Level::Major,
                rule,
                subject,
                format!("discriminator {} → {}", show(da), show(db)),
            );
        }
        let set = |t: &[Option<String>]| t.iter().flatten().cloned().collect::<BTreeSet<String>>();
        let (old, new) = (set(ta), set(tb));
        for tag in old.difference(&new) {
            self.narrowed(subject, polarity, format!("tag {tag:?} removed"));
        }
        for tag in new.difference(&old) {
            self.widened(subject, polarity, format!("tag {tag:?} added"));
        }
    }

    fn object(
        &mut self,
        subject: &str,
        polarity: Polarity,
        (fa, ea): (&BTreeMap<String, FieldSurface>, &Extra),
        (fb, eb): (&BTreeMap<String, FieldSurface>, &Extra),
    ) {
        // Read-only fields are never sent, write-only fields never received.
        let visible = |f: &FieldSurface| match polarity {
            Polarity::Request => !f.read_only,
            Polarity::Response => !f.write_only,
        };
        for (name, a) in fa.iter().filter(|(_, f)| visible(f)) {
            let sub = format!("{subject}.{name}");
            let Some(b) = fb.get(name).filter(|f| visible(f)) else {
                let rule = match polarity {
                    Polarity::Request => "field_removed",
                    Polarity::Response => "response_field_removed",
                };
                self.push(Level::Major, rule, &sub, "field removed");
                continue;
            };
            match (polarity, a.required, b.required) {
                (Polarity::Request, false, true) => self.push(
                    Level::Major,
                    "arg_became_required",
                    &sub,
                    "optional → required",
                ),
                (Polarity::Request, true, false) => self.push(
                    Level::Minor,
                    "requirement_relaxed",
                    &sub,
                    "required → optional",
                ),
                (Polarity::Response, true, false) => self.push(
                    Level::Major,
                    "response_field_became_optional",
                    &sub,
                    "required → optional",
                ),
                (Polarity::Response, false, true) => self.push(
                    Level::Minor,
                    "response_type_narrowed",
                    &sub,
                    "optional → required",
                ),
                _ => {}
            }
            match (a.nullable, b.nullable) {
                (true, false) => self.narrowed(&sub, polarity, "no longer nullable".into()),
                (false, true) => self.widened(&sub, polarity, "now nullable".into()),
                _ => {}
            }
            self.ty(&sub, &a.ty, &b.ty, polarity);
        }
        for (name, b) in fb.iter().filter(|(_, f)| visible(f)) {
            if fa.get(name).is_some_and(&visible) {
                continue;
            }
            let sub = format!("{subject}.{name}");
            match (polarity, b.required) {
                (Polarity::Request, true) => self.push(
                    Level::Major,
                    "required_arg_added",
                    &sub,
                    "required field added",
                ),
                (Polarity::Request, false) => self.push(
                    Level::Minor,
                    "optional_arg_added",
                    &sub,
                    "optional field added",
                ),
                (Polarity::Response, _) => {
                    self.push(Level::Minor, "response_field_added", &sub, "field added")
                }
            }
        }
        match (ea, eb) {
            (Extra::Typed(a), Extra::Typed(b)) => {
                self.ty(&format!("{subject}{{}}"), a, b, polarity)
            }
            (a, b) if a == b => {}
            (_, Extra::Closed) => match polarity {
                Polarity::Request => {
                    self.narrowed(subject, polarity, "extra keys no longer accepted".into())
                }
                Polarity::Response => self.push(
                    Level::Minor,
                    "response_type_narrowed",
                    subject,
                    "extra keys no longer returned",
                ),
            },
            (Extra::Closed, _) => match polarity {
                Polarity::Request => {
                    self.widened(subject, polarity, "extra keys now accepted".into())
                }
                // Clients tolerate unknown response keys.
                Polarity::Response => self.push(
                    Level::Minor,
                    "response_field_added",
                    subject,
                    "extra keys may now be returned",
                ),
            },
            (Extra::Open, _) => {
                self.narrowed(subject, polarity, "extra values now have a type".into())
            }
            (_, Extra::Open) => {
                self.widened(subject, polarity, "extra values may now be anything".into())
            }
        }
    }
}

/// A short name of a type for change details.
fn describe(ty: &Ty) -> String {
    match ty {
        Ty::Ref { id } => id.clone(),
        Ty::String {
            format: Some(f), ..
        } => format!("string({f})"),
        Ty::String { .. } => "string".into(),
        Ty::Integer { .. } => "integer".into(),
        Ty::Number { .. } => "number".into(),
        Ty::Bool => "boolean".into(),
        Ty::Bytes => "bytes".into(),
        Ty::Enum { .. } => "enum".into(),
        Ty::Const { value } => format!("const {value}"),
        Ty::Array { items, .. } => format!("array of {}", describe(items)),
        Ty::Map { values } => format!("map of {}", describe(values)),
        Ty::Object { .. } => "object".into(),
        Ty::Union { .. } => "union".into(),
        Ty::Intersection { .. } => "intersection".into(),
        Ty::Nullable { inner } => format!("nullable {}", describe(inner)),
        Ty::Any => "any".into(),
        Ty::Never => "never".into(),
    }
}

/// The polarities each named type of `surface` is used with, following
/// references from operation arguments (requests) and responses.
fn polarities(surface: &ApiSurface) -> BTreeMap<&str, BTreeSet<Polarity>> {
    fn walk<'a>(
        surface: &'a ApiSurface,
        ty: &'a Ty,
        polarity: Polarity,
        out: &mut BTreeMap<&'a str, BTreeSet<Polarity>>,
    ) {
        match ty {
            Ty::Ref { id } => {
                let fresh = out.entry(id.as_str()).or_default().insert(polarity);
                if let Some(t) = surface.types.get(id).filter(|_| fresh) {
                    walk(surface, &t.ty, polarity, out);
                }
            }
            Ty::Array { items: inner, .. } | Ty::Map { values: inner } | Ty::Nullable { inner } => {
                walk(surface, inner, polarity, out)
            }
            Ty::Object { fields, extra } => {
                for f in fields.values() {
                    walk(surface, &f.ty, polarity, out);
                }
                if let Extra::Typed(t) = extra {
                    walk(surface, t, polarity, out);
                }
            }
            Ty::Union { variants: list, .. } | Ty::Intersection { members: list } => {
                for t in list {
                    walk(surface, t, polarity, out);
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeMap::new();
    for op in surface.operations.values() {
        for ty in op.args.values().map(|a| &a.ty).chain(&op.body) {
            walk(surface, ty, Polarity::Request, &mut out);
        }
        if let Some(r) = &op.response {
            walk(surface, r, Polarity::Response, &mut out);
        }
    }
    out
}
