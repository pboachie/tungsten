// SPDX-License-Identifier: AGPL-3.0-only
//! F# types of planned type references, where every model lives (its F#
//! namespace, its file and its compile group), and the namer setup of the
//! target.
//!
//! Types ([`Ft`]): `string`, `bool`, `int`, `int64`, `float`, `byte[]`
//! (base64 in JSON), the runtime's `Binary` (a body or multipart file), the
//! .NET runtime's `JsonNode` for anything without an F# type (TG1081: an
//! `allOf` without members; inline records and unions; `any`), generated models, `'T list` for arrays and
//! `Map<string, 'T>` for maps. JSON null inside a type is `'T option`
//! (`JsonNode` holds null itself). A record field's type follows its
//! presence ([`field_ty`]): `'T` when required, `'T option` when optional
//! or when null is one of the values of a required field, and the F#
//! layer's `Patch<'T>` (absent, null or a value) when the field is optional
//! and may be null.
//!
//! Models ([`Ctx::model_groups`]): one F# namespace per model namespace
//! (`<namespace>.Models`, or `<namespace>.Models.<Module>` when the API has
//! several), one file per strongly connected component of the type graph
//! that has a model, in the plan's component order (dependencies first, as
//! F# requires). A recursive component is one group of `type ... and ...`
//! declarations in one file, named after its first type, in that type's
//! namespace (a cycle across model namespaces is declared in the namespace
//! of its first type; a model of that cycle whose name is taken there is
//! prefixed with its own module, [`Ctx::placed_names`]).

use std::collections::{BTreeMap, BTreeSet};

use tungsten_emit::sdk::{AllOfPlan, EnumPlan, NamerOptions, Scope, TypeKind, UnionPlan};
use tungsten_ir::{Presence, Primitive, Shape, TypeId, TypeRef};

use crate::Ctx;
use crate::fs;

/// The kind of an F# type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fk {
    Str,
    Bool,
    I32,
    I64,
    F64,
    /// Base64 text in JSON (`byte[]`).
    Bytes,
    /// A binary body or multipart field (the runtime's `Binary`).
    Binary,
    /// The runtime's JSON value (`JsonNode`, null for JSON null).
    Any,
    /// A generated model, by its name as written from the referencing
    /// namespace.
    Named(String),
    List(Box<Ft>),
    Map(Box<Ft>),
}

/// An F# type: its kind, and whether JSON null is one of its values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ft {
    pub kind: Fk,
    pub nullable: bool,
}

impl Ft {
    pub fn of(kind: Fk) -> Self {
        let nullable = kind == Fk::Any;
        Self { kind, nullable }
    }

    /// The runtime's JSON value.
    pub fn any() -> Self {
        Self::of(Fk::Any)
    }

    /// The type without its null.
    pub fn base(&self) -> String {
        match &self.kind {
            Fk::Str => "string".into(),
            Fk::Bool => "bool".into(),
            Fk::I32 => "int".into(),
            Fk::I64 => "int64".into(),
            Fk::F64 => "float".into(),
            Fk::Bytes => "byte[]".into(),
            Fk::Binary => "Binary".into(),
            Fk::Any => "JsonNode".into(),
            Fk::Named(name) => name.clone(),
            Fk::List(item) => format!("{} list", item.ty()),
            Fk::Map(value) => format!("Map<string, {}>", value.ty()),
        }
    }

    /// The type as written: `'T option` when null is a value (except for
    /// `JsonNode`, which holds JSON null itself).
    pub fn ty(&self) -> String {
        if self.nullable && self.kind != Fk::Any {
            format!("{} option", self.base())
        } else {
            self.base()
        }
    }

    /// The same type without its null.
    pub fn non_null(&self) -> Ft {
        Ft {
            kind: self.kind.clone(),
            nullable: self.kind == Fk::Any,
        }
    }

    /// The same type with null as a value.
    pub fn with_null(&self) -> Ft {
        Ft {
            kind: self.kind.clone(),
            nullable: true,
        }
    }
}

/// The type of a record field (or request member) of type `ft` with this
/// presence; see the module documentation.
pub fn field_ty(ft: &Ft, presence: Presence) -> String {
    let optional = matches!(presence, Presence::Optional | Presence::OptionalNullable);
    let null_allowed = ft.nullable
        || matches!(
            presence,
            Presence::RequiredNullable | Presence::OptionalNullable
        );
    let inner = ft.non_null();
    match (optional, null_allowed) {
        (false, false) => inner.ty(),
        (false, true) if inner.kind == Fk::Any => "JsonNode".into(),
        (false, true) => format!("{} option", inner.base()),
        (true, false) => format!("{} option", inner.ty()),
        (true, true) => format!("Patch<{}>", inner.base()),
    }
}

/// The F# type of a primitive.
pub fn primitive(p: &Primitive) -> Fk {
    match p {
        Primitive::String { .. } => Fk::Str,
        Primitive::Bytes => Fk::Bytes,
        Primitive::Int32 => Fk::I32,
        Primitive::Int64 | Primitive::Integer => Fk::I64,
        Primitive::Float | Primitive::Double | Primitive::Number => Fk::F64,
        Primitive::Bool => Fk::Bool,
    }
}

/// One model file: the models of one strongly connected component of the
/// type graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelGroup {
    /// The types with a model, in plan order; never empty.
    pub types: Vec<TypeId>,
    /// The F# namespace the group is declared in.
    pub namespace: String,
    /// The file (`Models/<Name>.fs`, or `Models/<Module>/<Name>.fs`).
    pub path: String,
    /// The component is a reference cycle: one `type ... and ...` group.
    pub recursive: bool,
}

impl Ctx<'_> {
    /// The F# namespace of a model namespace's types.
    pub fn model_ns(&self, ns: &str) -> String {
        if self.single_model_namespace {
            format!("{}.Models", self.opts.namespace)
        } else {
            format!("{}.Models.{}", self.opts.namespace, self.module(ns))
        }
    }

    /// The module name of a model namespace (multi-namespace APIs).
    fn module(&self, ns: &str) -> String {
        self.names
            .modules
            .get(ns)
            .cloned()
            .unwrap_or_else(|| "Shared".into())
    }

    /// Whether a named type has a model (a record, an `allOf` (merged into
    /// one record, or a record of one part per member), an enum, or a union
    /// with variants).
    pub fn has_model(&self, id: &TypeId) -> bool {
        let Some(t) = self.plan.types.get(id) else {
            return false;
        };
        match &t.kind {
            TypeKind::Record { .. } => true,
            TypeKind::Enum(e) => matches!(
                e,
                EnumPlan::Strings | EnumPlan::Integers | EnumPlan::Mixed { .. }
            ),
            TypeKind::Union(u) => !matches!(u, UnionPlan::Empty),
            TypeKind::AllOf(AllOfPlan::Merged { .. }) => true,
            TypeKind::AllOf(AllOfPlan::Intersection) => matches!(
                self.ir.types.get(id).map(|t| &t.shape),
                Some(Shape::Intersection { members }) if !members.is_empty()
            ),
            TypeKind::Alias => false,
        }
    }

    /// The F# name of every model, unique in the F# namespace it is
    /// declared in. A model keeps the Namer's name unless it belongs to a
    /// cycle declared in another model namespace's F# namespace and that
    /// name is taken there: then it is prefixed with its own module
    /// (`SealedError`), numbered if that is taken too.
    pub fn placed_names(&self) -> BTreeMap<TypeId, String> {
        let mut out = BTreeMap::new();
        let mut taken: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut moved = vec![];
        for id in self.plan.namespaces.iter().flat_map(|n| &n.types) {
            let Some(t) = self.plan.types.get(id) else {
                continue;
            };
            if !self.has_model(id) {
                continue;
            }
            let name = self.names.types.get(id).cloned().unwrap_or_default();
            let ns = self.type_ns(id);
            if ns == self.model_ns(&t.namespace) {
                taken.entry(ns).or_default().push(name.clone());
                out.insert(id.clone(), name);
            } else {
                moved.push((id.clone(), name, ns, t.namespace.clone()));
            }
        }
        for (id, name, ns, own) in moved {
            let names = taken.entry(ns).or_default();
            let placed = if names.contains(&name) {
                crate::fs::unique(format!("{}{name}", self.module(&own)), names)
            } else {
                names.push(name.clone());
                name
            };
            out.insert(id, placed);
        }
        out
    }

    /// The F# name of a model, as declared ([`Ctx::placed_names`]).
    pub fn model_name(&self, id: &TypeId) -> String {
        self.model_names
            .get(id)
            .or_else(|| self.names.types.get(id))
            .cloned()
            .unwrap_or_default()
    }

    /// Every model file in compile order (dependencies first).
    pub fn model_groups(&self) -> Vec<ModelGroup> {
        // Plan order: namespace order, then each namespace's declaration
        // order.
        let order: BTreeMap<&TypeId, usize> = self
            .plan
            .namespaces
            .iter()
            .flat_map(|n| &n.types)
            .enumerate()
            .map(|(i, id)| (id, i))
            .collect();
        let mut components: BTreeMap<usize, Vec<&TypeId>> = BTreeMap::new();
        for (id, t) in &self.plan.types {
            if self.has_model(id) {
                components.entry(t.scc).or_default().push(id);
            }
        }
        let mut groups = vec![];
        for (scc, mut ids) in components {
            ids.sort_by_key(|id| (order.get(id).copied().unwrap_or(usize::MAX), (*id).clone()));
            let lead = ids[0];
            let Some(plan) = self.plan.types.get(lead) else {
                continue;
            };
            let name = self.model_name(lead);
            let path = if self.single_model_namespace {
                format!("Models/{name}.fs")
            } else {
                format!("Models/{}/{name}.fs", self.module(&plan.namespace))
            };
            let recursive = self.plan.types.values().any(|t| t.scc == scc && t.cyclic);
            groups.push(ModelGroup {
                types: ids.into_iter().cloned().collect(),
                namespace: self.model_ns(&plan.namespace),
                path,
                recursive,
            });
        }
        groups
    }

    /// The F# namespace a model is declared in (its group's).
    pub fn type_ns(&self, id: &TypeId) -> String {
        let Some(t) = self.plan.types.get(id) else {
            return self.model_ns("");
        };
        if !t.cyclic || self.single_model_namespace {
            return self.model_ns(&t.namespace);
        }
        // The first type of the cycle in plan order names the namespace.
        let lead = self
            .plan
            .namespaces
            .iter()
            .flat_map(|n| &n.types)
            .filter_map(|other| self.plan.types.get(other))
            .find(|other| other.scc == t.scc && self.has_model(&other.id))
            .map_or(t.namespace.as_str(), |other| other.namespace.as_str());
        self.model_ns(lead)
    }

    /// The name of a model as written from the F# namespace `from`: bare
    /// in its own namespace (the types of one recursive group refer to one
    /// another), else fully qualified with `global.`.
    pub fn model_ref(&self, id: &TypeId, from: Option<&str>) -> String {
        let name = self.model_name(id);
        let ns = self.type_ns(id);
        if from == Some(ns.as_str()) {
            name
        } else {
            format!("global.{ns}.{name}")
        }
    }

    /// The F# type of a type reference written in the F# namespace `from`.
    pub fn ft(&self, ty: &TypeRef, from: Option<&str>) -> Ft {
        let mut active = BTreeSet::new();
        self.ft_in(ty, from, &mut active)
    }

    fn ft_in(&self, ty: &TypeRef, from: Option<&str>, active: &mut BTreeSet<TypeId>) -> Ft {
        match ty {
            TypeRef::Inline(shape) => self.shape_ft(shape, from, active),
            TypeRef::Named(id) => {
                let Some(t) = self.plan.types.get(id) else {
                    return Ft::any();
                };
                match &t.kind {
                    TypeKind::Alias => {
                        if !active.insert(id.clone()) {
                            return Ft::any();
                        }
                        let out = match self.ir.types.get(id) {
                            Some(named) => self.shape_ft(&named.shape, from, active),
                            None => Ft::any(),
                        };
                        active.remove(id);
                        out
                    }
                    _ if self.has_model(id) => Ft::of(Fk::Named(self.model_ref(id, from))),
                    _ => Ft::any(),
                }
            }
        }
    }

    fn shape_ft(&self, shape: &Shape, from: Option<&str>, active: &mut BTreeSet<TypeId>) -> Ft {
        match shape {
            Shape::Primitive { primitive: p, .. } => Ft::of(primitive(p)),
            Shape::Enum { base, .. } => Ft::of(primitive(base)),
            Shape::Const { value } => Ft::of(match value {
                serde_json::Value::String(_) => Fk::Str,
                serde_json::Value::Bool(_) => Fk::Bool,
                serde_json::Value::Number(n) if n.is_i64() => Fk::I64,
                serde_json::Value::Number(_) => Fk::F64,
                _ => Fk::Any,
            }),
            Shape::Array { items, .. } => {
                Ft::of(Fk::List(Box::new(self.ft_in(items, from, active))))
            }
            Shape::Map { values } => Ft::of(Fk::Map(Box::new(self.ft_in(values, from, active)))),
            Shape::Nullable { inner } => self.ft_in(inner, from, active).with_null(),
            Shape::Record { .. }
            | Shape::Union(_)
            | Shape::Intersection { .. }
            | Shape::Any
            | Shape::Never => Ft::any(),
        }
    }
}

/// The runtime's, the F# layer's and the base library's type and module
/// names the generated code uses without qualification: no model or class
/// may take one. Sorted.
pub const TAKEN_TYPES: &[&str] = &[
    "Action",
    "Array",
    "Async",
    "Binary",
    "CallOptions",
    "CancellationToken",
    "Choice",
    "ClientCore",
    "ClientOptions",
    "Codec",
    "Confirm",
    "Credential",
    "DecodeException",
    "Decoder",
    "Diagnostic",
    "Dictionary",
    "Encode",
    "EventStream",
    "Exception",
    "Func",
    "IAsyncEnumerable",
    "IDispatch",
    "IReadOnlyDictionary",
    "IReadOnlyList",
    "Identity",
    "Json",
    "JsonArray",
    "JsonKind",
    "JsonNode",
    "JsonObject",
    "JsonValue",
    "Lazy",
    "List",
    "MacroDescriptor",
    "Map",
    "Models",
    "NoContent",
    "OAuthFlow",
    "OperationDescriptor",
    "Ops",
    "Option",
    "Page",
    "Patch",
    "PreviewResult",
    "Result",
    "Results",
    "SdkDescriptors",
    "Seq",
    "Set",
    "StreamItem",
    "String",
    "System",
    "Task",
    "Tasks",
    "Tungsten",
    "TungstenError",
    "Typed",
    "Unit",
    "ValueOption",
];

/// Members every .NET object already has.
pub const OBJECT_MEMBERS: &[&str] = &[
    "Equals",
    "Finalize",
    "GetHashCode",
    "GetType",
    "MemberwiseClone",
    "ToString",
];

/// Union and enum case names F# refuses or the generated code defines:
/// `Tag` and `Tags` (every union has them), the codec members, and
/// `Unknown`, the enum case that keeps a value the SDK does not know.
pub const CASE_NAMES: &[&str] = &["FromJson", "Tag", "Tags", "ToJson", "Unknown"];

/// The namer options of the F# target.
///
/// F# keeps the C# naming profile ([`tungsten_ir::naming::Target::CSharp`]):
/// the SDK is a .NET assembly that C# code also calls, so its names avoid
/// the C# keywords as well, and PascalCase members never meet an F#
/// keyword. The camelCase names (arguments, locals) that are F# keywords
/// are written escaped (``` ``type`` ```, [`fs::ident`]) where they are
/// declared, and namespaces refuse them (TG1080).
pub fn namer_options() -> NamerOptions {
    let mut reserved: BTreeMap<Scope, Vec<String>> = BTreeMap::new();
    let types: Vec<String> = TAKEN_TYPES.iter().map(|s| s.to_string()).collect();
    reserved.insert(Scope::Types, types.clone());
    reserved.insert(Scope::Classes, types);
    let cases: Vec<String> = CASE_NAMES.iter().map(|s| s.to_string()).collect();
    reserved.insert(Scope::UnionVariants, cases.clone());
    reserved.insert(Scope::EnumVariants, cases);
    reserved.insert(
        Scope::Fields,
        fs::RECORD_MEMBERS.iter().map(|s| s.to_string()).collect(),
    );
    let mut members: Vec<String> = OBJECT_MEMBERS.iter().map(|s| s.to_string()).collect();
    members.push("Core".into());
    reserved.insert(Scope::Members, members.clone());
    let mut client = members;
    client.extend(["Dispatch", "Macros", "OAuth"].map(String::from));
    reserved.insert(Scope::Client, client);
    reserved.insert(Scope::Operations, vec!["Document".into()]);
    reserved.insert(
        Scope::Macros,
        OBJECT_MEMBERS.iter().map(|s| s.to_string()).collect(),
    );
    NamerOptions {
        reserved,
        preview_members: true,
        pages_members: true,
        stream_members: true,
    }
}
