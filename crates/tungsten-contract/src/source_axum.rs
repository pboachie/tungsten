// SPDX-License-Identifier: AGPL-3.0-only
//! Bounded Axum declaration evidence. Parses supplied Rust text only: no file
//! reads, module loading, macro expansion, application or dependency execution.
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use proc_macro2::Span;
use serde::Serialize;
use serde_json::{Value, json};
use syn::visit::{self, Visit};
use syn::{Expr, FnArg, GenericArgument, Item, PathArguments, ReturnType, Type, spanned::Spanned};

use crate::*;
mod lex;

/// Explicit inventory selection. The caller supplies source text and the
/// reviewed mount prefix; application composition and mounts are not guessed.
pub struct SourceFile<'a> {
    pub label: &'a str,
    pub text: &'a str,
    pub router: &'a str,
    pub prefix: &'a str,
}

impl std::fmt::Debug for SourceFile<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceFile")
            .field("bytes", &self.text.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SourceNote {
    DynamicPath,
    UnsupportedPath,
    RouterFlow,
    UnsupportedMethod,
    UnsupportedHandler,
    ConditionalDeclaration,
    MacroNotExpanded,
    NestedRouter,
    UnknownExtractor,
    UnknownTypeOrSerde,
    UnknownResponse,
    UnknownMiddlewareAuth,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteCoverage {
    pub source: SourceLocation,
    pub method: Option<String>,
    pub path: Option<String>,
    pub operation_id: Option<String>,
    pub notes: BTreeSet<SourceNote>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeclaredBinding {
    pub operation_id: String,
    pub name: String,
    pub location: String,
    pub required: bool,
    pub source: SourceLocation,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SourceReport {
    pub version: u32,
    /// Explicit caller labels mapped from opaque evidence document IDs.
    pub files: BTreeMap<String, String>,
    pub routes: Vec<RouteCoverage>,
    pub bindings: Vec<DeclaredBinding>,
    pub declarations: BTreeMap<String, BTreeSet<SourceLocation>>,
    /// Always false: selected declarations cannot establish runtime coverage.
    pub runtime_complete: bool,
}

#[derive(Debug, Clone)]
pub struct AxumImport {
    pub evidence: EvidenceDocument,
    pub report: SourceReport,
}

impl AxumImport {
    /// Reuse the shared engine and caller-owned persistent review overrides.
    pub fn infer(
        &self,
        overrides: &ReviewedOverrides,
        limits: &Limits,
    ) -> Result<ContractOutput, Diagnostic> {
        let mut output = crate::infer(&self.evidence, overrides, limits)?;
        for operation in &self.evidence.operations {
            let id = stable_operation_id(&operation.method, &operation.path);
            let value = &mut output.openapi["paths"][&operation.path][&operation.method];
            value["x-tungsten-source-candidate"] = json!(true);
            for binding in self
                .report
                .bindings
                .iter()
                .filter(|binding| binding.operation_id == id)
            {
                if binding.location == "body" {
                    value["requestBody"]["required"] = json!(binding.required);
                } else if let Some(parameters) = value["parameters"].as_array_mut() {
                    for parameter in parameters.iter_mut().filter(|parameter| {
                        parameter["in"] == binding.location && parameter["name"] == binding.name
                    }) {
                        parameter["required"] = json!(binding.required);
                    }
                }
            }
        }
        Ok(output)
    }
}

fn failure(message: &str) -> Diagnostic {
    Diagnostic {
        code: "AX001".into(),
        message: message.into(),
    }
}

fn location(document: &str, span: Span) -> SourceLocation {
    let point = span.start();
    SourceLocation {
        document: document.into(),
        line: u32::try_from(point.line).ok(),
        column: u32::try_from(point.column + 1).ok(),
    }
}

fn path_text(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn imports(tree: &syn::UseTree, prefix: &str, output: &mut BTreeMap<String, String>) {
    match tree {
        syn::UseTree::Path(path) => {
            imports(&path.tree, &format!("{prefix}{}::", path.ident), output)
        }
        syn::UseTree::Name(name) => {
            output.insert(name.ident.to_string(), format!("{prefix}{}", name.ident));
        }
        syn::UseTree::Rename(name) => {
            output.insert(name.rename.to_string(), format!("{prefix}{}", name.ident));
        }
        syn::UseTree::Group(group) => {
            for item in &group.items {
                imports(item, prefix, output);
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}

struct File<'a> {
    input: &'a SourceFile<'a>,
    document: String,
    aliases: BTreeMap<String, String>,
    functions: BTreeMap<String, &'a syn::ItemFn>,
    models: BTreeMap<String, &'a syn::ItemStruct>,
    shadowed: BTreeSet<String>,
    schema_nodes: Cell<usize>,
    schema_sources: RefCell<BTreeSet<SourceLocation>>,
    max_nodes: usize,
}

impl File<'_> {
    fn canonical(&self, path: &syn::Path) -> String {
        let text = path_text(path);
        let (first, rest) = text.split_once("::").unwrap_or((&text, ""));
        if self.shadowed.contains(first) {
            return "unresolved".into();
        }
        match self.aliases.get(first) {
            Some(alias) if rest.is_empty() => alias.clone(),
            Some(alias) => format!("{alias}::{rest}"),
            None => text,
        }
    }

    fn axum_type(&self, ty: &Type, expected: &str) -> Option<Type> {
        let Type::Path(path) = ty else { return None };
        if self.canonical(&path.path) != expected {
            return None;
        }
        let PathArguments::AngleBracketed(arguments) = &path.path.segments.last()?.arguments else {
            return None;
        };
        if arguments.args.len() != 1 {
            return None;
        }
        match arguments.args.first()? {
            GenericArgument::Type(ty) => Some(ty.clone()),
            _ => None,
        }
    }

    fn router_root(&self, expr: &Expr) -> bool {
        match expr {
            Expr::MethodCall(call)
                if [
                    "route",
                    "with_state",
                    "layer",
                    "route_layer",
                    "nest",
                    "merge",
                ]
                .contains(&call.method.to_string().as_str()) =>
            {
                self.router_root(&call.receiver)
            }
            Expr::Paren(expr) => self.router_root(&expr.expr),
            Expr::Call(call) => {
                matches!(&*call.func, Expr::Path(path) if self.canonical(&path.path) == "axum::Router::new")
            }
            _ => false,
        }
    }

    fn methods<'a>(&self, expr: &'a Expr, output: &mut Vec<(String, &'a Expr)>) -> bool {
        match expr {
            Expr::Call(call) if call.args.len() == 1 => {
                let Expr::Path(path) = &*call.func else {
                    return false;
                };
                let path = self.canonical(&path.path);
                let Some(method) = path.strip_prefix("axum::routing::") else {
                    return false;
                };
                if !http_method(method) {
                    return false;
                }
                output.push((method.into(), &call.args[0]));
                true
            }
            Expr::MethodCall(call)
                if call.args.len() == 1 && http_method(&call.method.to_string()) =>
            {
                if !self.methods(&call.receiver, output) {
                    return false;
                }
                output.push((call.method.to_string(), &call.args[0]));
                true
            }
            _ => false,
        }
    }

    fn schema(
        &self,
        ty: &Type,
        response: bool,
        seen: &mut BTreeSet<String>,
        depth: usize,
        max_depth: usize,
    ) -> Option<Value> {
        self.schema_nodes
            .set(self.schema_nodes.get().saturating_add(1));
        if self.schema_nodes.get() > self.max_nodes {
            return None;
        }
        if depth > max_depth {
            return None;
        }
        let Type::Path(path) = ty else { return None };
        let name = self.canonical(&path.path);
        let arguments = &path.path.segments.last()?.arguments;
        if let PathArguments::AngleBracketed(arguments) = arguments {
            if arguments.args.len() != 1 {
                return None;
            }
            let GenericArgument::Type(inner) = &arguments.args[0] else {
                return None;
            };
            let schema = self.schema(inner, response, seen, depth + 1, max_depth)?;
            return match name.as_str() {
                "Option" | "std::option::Option" => Some(json!({"anyOf":[schema,{"type":"null"}]})),
                "Vec" | "std::vec::Vec" => Some(json!({"type":"array","items":schema})),
                _ => None,
            };
        }
        let primitive = match name.as_str() {
            "String" | "std::string::String" => Some(json!({"type":"string"})),
            "bool" => Some(json!({"type":"boolean"})),
            "i8" => Some(json!({"type":"integer","minimum":-128,"maximum":127})),
            "i16" => Some(json!({"type":"integer","minimum":-32768,"maximum":32767})),
            "i32" => Some(json!({"type":"integer","minimum":i32::MIN,"maximum":i32::MAX})),
            "i64" => Some(json!({"type":"integer","minimum":i64::MIN,"maximum":i64::MAX})),
            "u8" => Some(json!({"type":"integer","minimum":0,"maximum":255})),
            "u16" => Some(json!({"type":"integer","minimum":0,"maximum":65535})),
            "u32" => Some(json!({"type":"integer","minimum":0,"maximum":u32::MAX})),
            "u64" => Some(json!({"type":"integer","minimum":0,"maximum":u64::MAX})),
            _ => None,
        };
        if primitive.is_some() {
            return primitive;
        }
        if !seen.insert(name.clone()) {
            return None;
        }
        let model = self.models.get(&name)?;
        self.schema_sources
            .borrow_mut()
            .insert(location(&self.document, model.span()));
        if !model.generics.params.is_empty()
            || conditional(&model.attrs)
            || model.attrs.iter().any(|attribute| {
                !["serde", "doc", "derive"]
                    .iter()
                    .any(|name| attribute.path().is_ident(name))
            })
        {
            return None;
        }
        let derive = if response {
            "serde::Serialize"
        } else {
            "serde::Deserialize"
        };
        let derives = model
            .attrs
            .iter()
            .filter(|attribute| attribute.path().is_ident("derive"))
            .filter_map(|attribute| {
                attribute
                    .parse_args_with(
                        syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
                    )
                    .ok()
            })
            .flatten()
            .any(|path| self.canonical(&path) == derive);
        if !derives {
            return None;
        }
        let container = serde_options(&model.attrs)?;
        let syn::Fields::Named(fields) = &model.fields else {
            return None;
        };
        let mut properties = BTreeMap::new();
        let mut required = Vec::new();
        for field in &fields.named {
            self.schema_sources
                .borrow_mut()
                .insert(location(&self.document, field.span()));
            if conditional(&field.attrs)
                || field.attrs.iter().any(|attribute| {
                    !["serde", "doc"]
                        .iter()
                        .any(|name| attribute.path().is_ident(name))
                })
            {
                return None;
            }
            let options = serde_options(&field.attrs)?;
            let field_name = options.rename.unwrap_or(field.ident.as_ref()?.to_string());
            if properties.contains_key(&field_name) {
                return None;
            }
            let optional = option_type(&field.ty);
            let required_field = if response {
                !options.skip_none
            } else {
                !optional && !options.default && !container.default
            };
            if options.skip_none && !optional {
                return None;
            }
            if required_field {
                required.push(field_name.clone());
            }
            properties.insert(
                field_name,
                self.schema(&field.ty, response, &mut seen.clone(), depth + 1, max_depth)?,
            );
        }
        seen.remove(&name);
        Some(
            json!({"type":"object","properties":properties,"required":required,"additionalProperties":!response && !container.deny_unknown}),
        )
    }
}

#[derive(Default)]
struct SerdeOptions {
    rename: Option<String>,
    default: bool,
    deny_unknown: bool,
    skip_none: bool,
}

fn serde_options(attributes: &[syn::Attribute]) -> Option<SerdeOptions> {
    let mut options = SerdeOptions::default();
    for attribute in attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("serde"))
    {
        attribute
            .parse_nested_meta(|meta| {
                if meta.path.is_ident("deny_unknown_fields") {
                    options.deny_unknown = true;
                } else if meta.path.is_ident("default") && !meta.input.peek(syn::Token![=]) {
                    options.default = true;
                } else if meta.path.is_ident("rename") {
                    options.rename = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                } else if meta.path.is_ident("skip_serializing_if") {
                    if meta.value()?.parse::<syn::LitStr>()?.value() != "Option::is_none" {
                        return Err(meta.error("unsupported serde"));
                    }
                    options.skip_none = true;
                } else {
                    return Err(meta.error("unsupported serde"));
                }
                Ok(())
            })
            .ok()?;
    }
    Some(options)
}

fn option_type(ty: &Type) -> bool {
    matches!(ty, Type::Path(path) if ["Option", "std::option::Option"].contains(&path_text(&path.path).as_str()))
}

fn conditional(attributes: &[syn::Attribute]) -> bool {
    attributes
        .iter()
        .any(|attribute| attribute.path().is_ident("cfg") || attribute.path().is_ident("cfg_attr"))
}

fn unknown_function_attribute(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        !["doc", "cfg", "cfg_attr"]
            .iter()
            .any(|name| attribute.path().is_ident(name))
    })
}

fn http_method(method: &str) -> bool {
    [
        "get", "post", "put", "patch", "delete", "head", "options", "trace",
    ]
    .contains(&method)
}

fn local_shadowing(function: &syn::ItemFn, file: &File<'_>) -> bool {
    let protected: BTreeSet<_> = file
        .functions
        .keys()
        .chain(file.aliases.iter().filter_map(|(name, path)| {
            path.strip_prefix("axum::routing::")
                .filter(|method| http_method(method))
                .map(|_| name)
        }))
        .cloned()
        .collect();
    struct Check {
        protected: BTreeSet<String>,
        shadowed: bool,
    }
    impl<'ast> Visit<'ast> for Check {
        fn visit_item(&mut self, _: &'ast Item) {
            self.shadowed = true;
        }
        fn visit_pat_ident(&mut self, pattern: &'ast syn::PatIdent) {
            if self.protected.contains(&pattern.ident.to_string()) {
                self.shadowed = true;
            }
            visit::visit_pat_ident(self, pattern);
        }
    }
    let mut check = Check {
        protected,
        shadowed: false,
    };
    for argument in &function.sig.inputs {
        if let FnArg::Typed(argument) = argument {
            check.visit_pat(&argument.pat);
        }
    }
    check.visit_block(&function.block);
    check.shadowed
}

fn conditional_function(function: &syn::ItemFn) -> bool {
    struct Check(bool);
    impl<'ast> Visit<'ast> for Check {
        fn visit_attribute(&mut self, attribute: &'ast syn::Attribute) {
            self.0 |= attribute.path().is_ident("cfg") || attribute.path().is_ident("cfg_attr");
        }
    }
    let mut check = Check(false);
    check.visit_item_fn(function);
    check.0
}

struct Routes<'a, 'b> {
    file: &'a File<'a>,
    output: &'b mut AxumImport,
    limits: &'a Limits,
    conditional: bool,
    macro_unexpanded: bool,
    scope_shadowed: bool,
    nodes: usize,
    overflow: bool,
}

impl Routes<'_, '_> {
    fn note(&mut self, span: Span, note: SourceNote) {
        self.output.report.routes.push(RouteCoverage {
            source: location(&self.file.document, span),
            method: None,
            path: None,
            operation_id: None,
            notes: BTreeSet::from([note]),
        });
    }

    fn route(&mut self, call: &syn::ExprMethodCall) {
        let mut notes = BTreeSet::new();
        let conditional_route = self.conditional || conditional(&call.attrs);
        if conditional_route {
            notes.insert(SourceNote::ConditionalDeclaration);
        }
        if self.macro_unexpanded {
            notes.insert(SourceNote::MacroNotExpanded);
        }
        let path = call.args.first().and_then(|expr| match expr {
            Expr::Lit(expr) => match &expr.lit {
                syn::Lit::Str(value) if value.value().starts_with('/') => Some(format!(
                    "{}{}",
                    self.file.input.prefix.trim_end_matches('/'),
                    value.value()
                )),
                _ => None,
            },
            _ => None,
        });
        if path.is_none() {
            notes.insert(SourceNote::DynamicPath);
        }
        let unsupported_path = path.as_ref().is_some_and(|path| {
            path.split('/')
                .any(|segment| segment.starts_with(':') || segment.contains('*'))
        });
        if unsupported_path {
            notes.insert(SourceNote::UnsupportedPath);
        }
        let anchored = !self.scope_shadowed && self.file.router_root(&call.receiver);
        if !anchored {
            notes.insert(SourceNote::RouterFlow);
        }
        let mut methods = Vec::new();
        if call.args.len() != 2 || !self.file.methods(&call.args[1], &mut methods) {
            notes.insert(SourceNote::UnsupportedMethod);
        }
        if methods.is_empty() {
            self.output.report.routes.push(RouteCoverage {
                source: location(&self.file.document, call.method.span()),
                method: None,
                path,
                operation_id: None,
                notes,
            });
            return;
        }
        for (method, handler) in methods {
            let mut row = RouteCoverage {
                source: location(&self.file.document, call.method.span()),
                method: Some(method.clone()),
                path: path.clone(),
                operation_id: None,
                notes: notes.clone(),
            };
            if let Some(path) = path.as_ref().filter(|_| {
                anchored && !conditional_route && !self.macro_unexpanded && !unsupported_path
            }) {
                let id = stable_operation_id(&method, path);
                row.operation_id = Some(id.clone());
                let mut operation = HttpEvidence {
                    method,
                    path: path.clone(),
                    source: row.source.clone(),
                    parameters: vec![],
                    request: vec![],
                    responses: vec![],
                    unknown: BTreeSet::from([
                        UnknownFact::Authentication,
                        UnknownFact::Authorization,
                        UnknownFact::Safety,
                        UnknownFact::Completeness,
                    ]),
                };
                // Preserve known route templates even when the binding type is unknown.
                for segment in path.split('/') {
                    if let Some(name) = segment
                        .strip_prefix('{')
                        .and_then(|segment| segment.strip_suffix('}'))
                    {
                        operation.parameters.push(ParameterEvidence {
                            name: name.into(),
                            location: ParameterLocation::Path,
                            shape: ShapeEvidence {
                                samples: vec![],
                                declared_schema: None,
                            },
                        });
                    }
                }
                let function = match handler {
                    Expr::Path(path) if path.path.segments.len() == 1 => self
                        .file
                        .functions
                        .get(&path.path.segments[0].ident.to_string())
                        .copied(),
                    _ => None,
                };
                if let Some(function) = function.filter(|function| {
                    !conditional(&function.attrs) && !unknown_function_attribute(&function.attrs)
                }) {
                    self.handler(function, &id, &mut operation, &mut row.notes);
                } else {
                    row.notes.insert(SourceNote::UnsupportedHandler);
                }
                if !row.notes.is_empty() {
                    operation
                        .unknown
                        .insert(UnknownFact::UnsupportedSourceField);
                }
                self.output.evidence.operations.push(operation);
            }
            self.output.report.routes.push(row);
        }
    }

    fn handler(
        &mut self,
        function: &syn::ItemFn,
        id: &str,
        operation: &mut HttpEvidence,
        notes: &mut BTreeSet<SourceNote>,
    ) {
        self.file.schema_sources.borrow_mut().clear();
        let mut extractors = BTreeSet::new();
        let duplicate_extractors = function
            .sig
            .inputs
            .iter()
            .filter_map(|argument| match argument {
                FnArg::Typed(argument) => match &*argument.ty {
                    Type::Path(path) => Some(self.file.canonical(&path.path)),
                    _ => None,
                },
                _ => None,
            })
            .filter(|extractor| {
                ["axum::extract::Path", "axum::extract::Query", "axum::Json"]
                    .contains(&extractor.as_str())
            })
            .any(|extractor| !extractors.insert(extractor));
        self.output
            .report
            .declarations
            .entry(id.into())
            .or_default()
            .insert(location(&self.file.document, function.sig.span()));
        for argument in &function.sig.inputs {
            if duplicate_extractors {
                notes.insert(SourceNote::UnknownExtractor);
                break;
            }
            let FnArg::Typed(argument) = argument else {
                notes.insert(SourceNote::UnknownExtractor);
                continue;
            };
            if let Some(ty) = self.file.axum_type(&argument.ty, "axum::extract::State") {
                let _ = ty;
                notes.insert(SourceNote::UnknownMiddlewareAuth);
                continue;
            }
            if let Some(ty) = self.file.axum_type(&argument.ty, "axum::Json") {
                if let Some(schema) =
                    self.file
                        .schema(&ty, false, &mut BTreeSet::new(), 0, self.limits.max_depth)
                {
                    operation.request.push(MediaEvidence {
                        media_type: "application/json".into(),
                        shape: ShapeEvidence {
                            samples: vec![],
                            declared_schema: Some(schema),
                        },
                    });
                    self.binding(id, "", "body", true, argument.span());
                } else {
                    notes.insert(SourceNote::UnknownTypeOrSerde);
                }
            } else if let Some(ty) = self.file.axum_type(&argument.ty, "axum::extract::Query") {
                if let Some(schema) = self
                    .file
                    .schema(&ty, false, &mut BTreeSet::new(), 0, self.limits.max_depth)
                    .filter(|schema| schema["type"] == "object")
                {
                    if let Some(properties) = schema["properties"].as_object() {
                        for (name, value) in properties {
                            let scalar = value["type"].as_str().is_some_and(|kind| {
                                ["string", "integer", "number", "boolean"].contains(&kind)
                            }) || value["anyOf"].as_array().is_some_and(|branches| {
                                branches.len() == 2
                                    && branches.iter().all(|branch| {
                                        branch["type"].as_str().is_some_and(|kind| {
                                            ["null", "string", "integer", "number", "boolean"]
                                                .contains(&kind)
                                        })
                                    })
                            });
                            if !scalar {
                                notes.insert(SourceNote::UnknownExtractor);
                                continue;
                            }
                            operation.parameters.push(ParameterEvidence {
                                name: name.clone(),
                                location: ParameterLocation::Query,
                                shape: ShapeEvidence {
                                    samples: vec![],
                                    declared_schema: Some(value.clone()),
                                },
                            });
                            let required = schema["required"]
                                .as_array()
                                .is_some_and(|required| required.iter().any(|value| value == name));
                            self.binding(id, name, "query", required, argument.span());
                        }
                    }
                } else {
                    notes.insert(SourceNote::UnknownTypeOrSerde);
                }
            } else if let Some(ty) = self.file.axum_type(&argument.ty, "axum::extract::Path") {
                let paths: Vec<_> = operation
                    .parameters
                    .iter_mut()
                    .filter(|parameter| parameter.location == ParameterLocation::Path)
                    .collect();
                if paths.len() == 1 {
                    let parameter = paths.into_iter().next().expect("one path binding");
                    parameter.shape.declared_schema = self
                        .file
                        .schema(&ty, false, &mut BTreeSet::new(), 0, self.limits.max_depth)
                        .filter(|schema| {
                            schema["type"].as_str().is_some_and(|kind| {
                                ["string", "integer", "boolean"].contains(&kind)
                            })
                        });
                    if parameter.shape.declared_schema.is_none() {
                        notes.insert(SourceNote::UnknownTypeOrSerde);
                    }
                } else {
                    notes.insert(SourceNote::UnknownExtractor);
                }
            } else {
                notes.insert(SourceNote::UnknownExtractor);
            }
        }
        let response = match &function.sig.output {
            ReturnType::Type(_, ty) => self.file.axum_type(ty, "axum::Json"),
            _ => None,
        };
        if let Some(schema) = response.and_then(|ty| {
            self.file
                .schema(&ty, true, &mut BTreeSet::new(), 0, self.limits.max_depth)
        }) {
            operation.responses.push(ResponseEvidence {
                status: 200,
                content: vec![MediaEvidence {
                    media_type: "application/json".into(),
                    shape: ShapeEvidence {
                        samples: vec![],
                        declared_schema: Some(schema),
                    },
                }],
            });
        } else {
            notes.insert(SourceNote::UnknownResponse);
        }
        self.output
            .report
            .declarations
            .entry(id.into())
            .or_default()
            .extend(self.file.schema_sources.borrow().iter().cloned());
    }

    fn binding(&mut self, id: &str, name: &str, kind: &str, required: bool, span: Span) {
        self.output.report.bindings.push(DeclaredBinding {
            operation_id: id.into(),
            name: name.into(),
            location: kind.into(),
            required,
            source: location(&self.file.document, span),
        });
    }
}

impl<'ast> Visit<'ast> for Routes<'_, '_> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        self.nodes += 1;
        if self.nodes > self.limits.max_nodes
            || self.output.report.routes.len() > self.limits.max_operations
        {
            self.overflow = true;
            return;
        }
        visit::visit_expr(self, expr);
    }
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        match call.method.to_string().as_str() {
            "route" => self.route(call),
            "nest" | "merge" => self.note(call.span(), SourceNote::NestedRouter),
            "nest_service" => self.note(call.method.span(), SourceNote::NestedRouter),
            "route_service" | "fallback" | "fallback_service" => {
                self.note(call.method.span(), SourceNote::UnsupportedHandler)
            }
            "layer" | "route_layer" => self.note(call.span(), SourceNote::UnknownMiddlewareAuth),
            _ => {}
        }
        visit::visit_expr_method_call(self, call);
    }
    fn visit_expr_if(&mut self, expr: &'ast syn::ExprIf) {
        let prior = self.conditional;
        self.conditional = true;
        visit::visit_expr_if(self, expr);
        self.conditional = prior;
    }
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.note(mac.span(), SourceNote::MacroNotExpanded);
    }
    fn visit_expr_match(&mut self, expr: &'ast syn::ExprMatch) {
        let prior = self.conditional;
        self.conditional = true;
        visit::visit_expr_match(self, expr);
        self.conditional = prior;
    }
    fn visit_expr_for_loop(&mut self, expr: &'ast syn::ExprForLoop) {
        let prior = self.conditional;
        self.conditional = true;
        visit::visit_expr_for_loop(self, expr);
        self.conditional = prior;
    }
    fn visit_expr_while(&mut self, expr: &'ast syn::ExprWhile) {
        let prior = self.conditional;
        self.conditional = true;
        visit::visit_expr_while(self, expr);
        self.conditional = prior;
    }
    fn visit_expr_loop(&mut self, expr: &'ast syn::ExprLoop) {
        let prior = self.conditional;
        self.conditional = true;
        visit::visit_expr_loop(self, expr);
        self.conditional = prior;
    }
    fn visit_expr_closure(&mut self, expr: &'ast syn::ExprClosure) {
        let prior = self.conditional;
        self.conditional = true;
        visit::visit_expr_closure(self, expr);
        self.conditional = prior;
    }
    fn visit_local(&mut self, local: &'ast syn::Local) {
        let prior = self.conditional;
        self.conditional |= conditional(&local.attrs);
        visit::visit_local(self, local);
        self.conditional = prior;
    }
}

/// Parse selected same-file Axum router inventories. Unknown constructs are
/// retained in coverage, never executed or expanded. Use existing OpenAPI
/// exports for richer behavior, auth and error declarations when available.
pub fn import(files: &[SourceFile<'_>], limits: &Limits) -> Result<AxumImport, Diagnostic> {
    if files.len() > limits.max_sources
        || files
            .iter()
            .try_fold(0usize, |sum, file| {
                sum.checked_add(file.text.len())?
                    .checked_add(file.label.len())?
                    .checked_add(file.router.len())?
                    .checked_add(file.prefix.len())
            })
            .is_none_or(|bytes| bytes > limits.max_bytes)
    {
        return Err(failure(
            "Source exceeds limits; split the selected inventory.",
        ));
    }
    for file in files {
        if !lex::bounded(file.text, limits.max_depth.min(32), limits.max_nodes) {
            return Err(failure(
                "Source token nesting or expression budget exceeded; split the inventory.",
            ));
        }
    }
    let files: Vec<_> = files
        .iter()
        .map(|file| {
            (
                file.label.to_owned(),
                file.text.to_owned(),
                file.router.to_owned(),
                file.prefix.to_owned(),
            )
        })
        .collect();
    let limits = limits.clone();
    // syn's recursive AST construction/destruction stays on a bounded parser
    // stack; this thread runs only trusted parser code, never application code.
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            let sources: Vec<_> = files
                .iter()
                .map(|(label, text, router, prefix)| SourceFile {
                    label,
                    text,
                    router,
                    prefix,
                })
                .collect();
            import_inner(&sources, &limits)
        })
        .map_err(|_| failure("Cannot start bounded source parser."))?
        .join()
        .map_err(|_| failure("Source parser failed; reduce the selected inventory."))?
}

fn import_inner(files: &[SourceFile<'_>], limits: &Limits) -> Result<AxumImport, Diagnostic> {
    let mut output = AxumImport {
        evidence: EvidenceDocument {
            version: EVIDENCE_VERSION,
            frontend: Frontend::Http,
            operations: vec![],
        },
        report: SourceReport {
            version: 1,
            ..SourceReport::default()
        },
    };
    for input in files {
        if input.label.is_empty()
            || input.label.len() > 512
            || input.label.starts_with(['/', '\\'])
            || input.label.split('/').any(|part| part == "..")
            || input.label.contains([':', '\\', '\n', '\r'])
            || (!input.prefix.is_empty() && !input.prefix.starts_with('/'))
            || input.prefix.contains(['?', '#', '{', '}', '\\'])
            || input
                .prefix
                .chars()
                .any(|ch| ch.is_whitespace() || ch.is_control())
        {
            return Err(failure(
                "Use unique relative source labels and an explicit slash-prefixed mount prefix.",
            ));
        }
        let document = format!("axum_{}", blake3::hash(input.label.as_bytes()).to_hex());
        let document = document[..48].to_owned();
        if output
            .report
            .files
            .insert(document.clone(), input.label.into())
            .is_some()
        {
            return Err(failure("Source labels must be unique."));
        }
        let parsed = syn::parse_file(input.text).map_err(|_| {
            failure("Invalid Rust syntax; provide a complete source file without expanding macros.")
        })?;
        let mut file = File {
            input,
            document,
            aliases: BTreeMap::new(),
            functions: BTreeMap::new(),
            models: BTreeMap::new(),
            shadowed: BTreeSet::new(),
            schema_nodes: Cell::new(0),
            schema_sources: RefCell::new(BTreeSet::new()),
            max_nodes: limits.max_nodes,
        };
        for item in &parsed.items {
            match item {
                Item::Use(item) if !conditional(&item.attrs) => {
                    imports(&item.tree, "", &mut file.aliases)
                }
                Item::Fn(item) => {
                    file.functions.insert(item.sig.ident.to_string(), item);
                }
                Item::Struct(item) => {
                    file.models.insert(item.ident.to_string(), item);
                }
                _ => {}
            }
        }
        // Type/value aliases that shadow imported Axum names are not resolved.
        for item in &parsed.items {
            let name = match item {
                Item::Struct(item) => Some(item.ident.to_string()),
                Item::Enum(item) => Some(item.ident.to_string()),
                Item::Type(item) => Some(item.ident.to_string()),
                Item::Mod(item) => Some(item.ident.to_string()),
                _ => None,
            };
            if let Some(name) = name {
                file.aliases.remove(&name);
                if [
                    "String", "Vec", "Option", "std", "axum", "serde", "bool", "u8", "u16", "u32",
                    "u64", "i8", "i16", "i32", "i64",
                ]
                .contains(&name.as_str())
                {
                    file.shadowed.insert(name);
                }
            }
        }
        let function = file
            .functions
            .get(input.router)
            .ok_or_else(|| failure("Selected router function is not a same-file function."))?;
        let mut routes = Routes {
            file: &file,
            output: &mut output,
            limits,
            conditional: conditional_function(function),
            macro_unexpanded: unknown_function_attribute(&function.attrs),
            scope_shadowed: local_shadowing(function, &file),
            nodes: 0,
            overflow: false,
        };
        for item in &parsed.items {
            match item {
                Item::Macro(item) => routes.note(item.span(), SourceNote::MacroNotExpanded),
                Item::Mod(item) => routes.note(item.span(), SourceNote::NestedRouter),
                _ => {}
            }
        }
        routes.visit_block(&function.block);
        if routes.overflow
            || file.schema_nodes.get() > limits.max_nodes
            || routes.output.report.routes.len() > limits.max_operations
        {
            return Err(failure(
                "Source AST, schema or route coverage exceeds limits; split the inventory.",
            ));
        }
    }
    output
        .evidence
        .operations
        .sort_by(|a, b| (&a.path, &a.method, &a.source).cmp(&(&b.path, &b.method, &b.source)));
    output
        .report
        .routes
        .sort_by(|a, b| (&a.source, &a.path, &a.method).cmp(&(&b.source, &b.path, &b.method)));
    output.report.bindings.sort_by(|a, b| {
        (&a.operation_id, &a.location, &a.name).cmp(&(&b.operation_id, &b.location, &b.name))
    });
    if output.evidence.operations.windows(2).any(|operations| {
        operations[0].path == operations[1].path && operations[0].method == operations[1].method
    }) {
        return Err(failure(
            "Duplicate method/path declarations require an explicit router inventory selection.",
        ));
    }
    // Reuse the shared validator even when all discovered routes are unresolved.
    crate::infer(&output.evidence, &ReviewedOverrides::default(), limits)?;
    Ok(output)
}
