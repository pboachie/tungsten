// SPDX-License-Identifier: AGPL-3.0-only
//! The language-neutral layer of the SDK emitters for Java, C#, Kotlin,
//! Swift, PHP, Ruby and Dart (planning/05 "SDK emitters for more
//! languages").
//!
//! - [`SdkPlan`] holds the structural decisions every SDK makes, without
//!   spellings: named types per namespace in declaration order with their
//!   reference cycles and reachability, how unions, enums and `allOf` types
//!   are represented, the callable operations with their arguments layout,
//!   supplied parameters, success value, paging and streaming, the resource
//!   tree, the client and the macros ([`MacroPlan`], parsed once; a macro
//!   outside the canonical form is a [`MacroIssue`]).
//! - [`Namer`] renders every word list of a plan for one target into a
//!   [`NameMap`], unique per scope, with the language's own reserved names.
//! - [`descriptors`] builds the SDK descriptor document v1 from a plan and
//!   its names: the data a runtime calls operations and runs macros with,
//!   schemas in the runtime schema form ([`RuntimeSchema`]). Its module
//!   documentation is the normative specification of the document and of
//!   the validation rules every runtime implements.
//!
//! The TypeScript, Python and Rust emitters keep their own plans; this layer
//! reproduces their decisions where they agree (the arguments layout, the
//! TypeScript descriptors, TG0710's macro checks), and the harness pins it
//! against them.
//!
//! Stability: additive only, like the rest of the crate.

pub mod descriptors;
mod macros;
mod namer;
mod plan;
mod schema_form;

pub use descriptors::{DocumentOptions, SdkDescriptors, document, document_value};
pub use macros::{
    InputSource, MacroExpr, MacroInputField, MacroIssue, MacroPlan, MacroStep, RefRoot, StepKind,
};
pub use namer::{
    MacroNames, Member, MemberKind, NameMap, Namer, NamerOptions, OperationNames, Rename,
    ResourceNames, Scope,
};
pub use plan::{
    AllOfPlan, ArgPlan, ArgSource, ClientNamespace, ClientPlan, EnumPlan, LiteralKey, MemberPlan,
    ModelNamespace, OpPlan, PagePlan, ResourcePlan, SdkPlan, StatusBody, StreamPlan, SuccessPlan,
    SuppliedParam, TypeKind, TypePlan, UnionPlan, has_preview, json_type, primitive_json_type,
    shape_refs,
};
pub use schema_form::{
    AdditionalMembers, RuntimeSchema, SchemaField, SchemaFormBuilder, SchemaKind, format_name,
    limits,
};
