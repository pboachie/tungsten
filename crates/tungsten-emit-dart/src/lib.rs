// SPDX-License-Identifier: AGPL-3.0-only
//! Dart SDK emitter (`dart` target).
//!
//! Built on the language-neutral SDK layer of `tungsten-emit`
//! ([`tungsten_emit::sdk`]): the [`SdkPlan`] decides the structure, the
//! [`Namer`] the Dart names, and the SDK descriptor document v1 carries
//! everything the runtime needs to call operations and run macros. A
//! generated package is that document plus thin, documented Dart over the
//! `tungsten_runtime` package (`runtimes/dart`, dart:io only).
//!
//! The file layout (paths relative to the target directory):
//!
//! - `pubspec.yaml` (SDK `>=3.6.0 <4.0.0`, one dependency:
//!   `tungsten_runtime`, a `path:` dependency with `runtime_path`),
//!   `analysis_options.yaml` (strict casts, inference and raw types, and the
//!   lint rules generated code passes), `README.md`;
//! - `lib/<package>.dart`: the entry library, exporting every library of
//!   the package (a name two libraries declare is exported from the first
//!   only) and `lib/src/custom/extensions.dart`;
//! - `lib/src/wire.dart`: the JSON decoders and encoders models use;
//! - `lib/src/models/<namespace>.dart`: one final class per record (and per
//!   `allOf` merged into one) with `fromJson` and `toJson`, four-state
//!   presence (a required member is `T`, a required nullable one `T?`, an
//!   optional one `T?` that is left out when null, an optional nullable one
//!   `Patch<T>`), members the schema does not declare kept in
//!   `additionalProperties`, value equality; an enhanced enum with its wire
//!   values per string or integer enum; a sealed class with one final
//!   subclass per variant per union (decoded by tag, by value, or by the
//!   first variant that accepts the value); `Uint8List` for bytes;
//! - `lib/src/custom/extensions.dart`, created once and never overwritten.
//!
//! The SDK surface (`lib/src/client.dart`, `lib/src/resources/*.dart`,
//! `lib/src/dispatch.dart`, `lib/src/macros.dart` and
//! `lib/src/descriptors.dart`, with the notes TG1072 to TG1075) comes from
//! the `sdk` module: [`generate`] adds the [`Library`] values it returns to
//! the model libraries before the entry library is written, so the entry
//! library exports them too.
//!
//! Output is deterministic: names come from the [`Namer`], every
//! collection is iterated in plan or sorted order, files carry the
//! `tungsten_emit::header` stamp, and the Dart is laid out as
//! `dart format` (short style, language 3.6) lays it out. Diagnostics:
//! TG1070 (an invalid target option, from `emit`) and TG1071 (a schema
//! without a Dart type: an `allOf` that cannot be merged or an enum that
//! is not of strings or integers is the JSON value `Object?`, from
//! `supports`).

mod dart;
mod models;
pub mod options;
mod package;
mod sdk;

use std::collections::BTreeMap;

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::sdk::{
    AllOfPlan, EnumPlan, NameMap, Namer, NamerOptions, Scope, SdkPlan, TypeKind,
};
use tungsten_emit::{CommentStyle, Emitter, FileSet, TargetConfig, header};
use tungsten_ir::Ir;
use tungsten_ir::naming::Target;

pub use options::Options;

/// Emits the `dart` target.
#[derive(Debug, Clone, Copy, Default)]
pub struct DartEmitter;

impl Emitter for DartEmitter {
    fn id(&self) -> &'static str {
        "dart"
    }

    fn supports(&self, ir: &Ir) -> Diagnostics {
        let (plan, _) = SdkPlan::new(ir);
        let mut diags = notes(&plan);
        diags.extend(sdk::notes(&plan));
        diags
    }

    fn emit(&self, ir: &Ir, cfg: &TargetConfig, out: &mut FileSet) -> Diagnostics {
        let (opts, mut diags) = Options::resolve(ir, cfg);
        for (path, text) in generate(ir, &opts) {
            if let Err(e) = out.add(path, text) {
                diags.push(Diagnostic::error("TG0701", format!("dart target: {e}")));
            }
        }
        diags
    }
}

/// Every file of the package: (path relative to the target directory,
/// contents), sorted by path.
pub fn generate(ir: &Ir, opts: &Options) -> Vec<(String, String)> {
    let (plan, _) = SdkPlan::new(ir);
    let g = Gen::new(&plan, opts);
    let mut libraries = models::libraries(&g);
    libraries.extend(sdk::libraries(&g));
    let mut files = package::files(&g, &libraries);
    files.extend(
        libraries
            .into_iter()
            .map(|l| (format!("lib/{}", l.path), l.text)),
    );
    files.sort();
    files
}

/// One generated Dart library under `lib/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Library {
    /// Path relative to `lib/` (`src/models/public.dart`).
    pub path: String,
    pub text: String,
    /// The public top-level names it declares, for the entry library's
    /// exports; `None` keeps it out of the entry library.
    pub exports: Option<Vec<String>>,
}

/// The public names of `tungsten_runtime` (with `EventStream` of its
/// streams), the names generated libraries import and the wire library's:
/// no model may take one, so files that import the
/// runtime and the models (and users who import both packages) never see
/// two declarations of a name.
const TAKEN_TYPES: &[&str] = &[
    "Binary",
    "BodyDescriptor",
    "CallOptions",
    "ClientConfigException",
    "ClientCore",
    "ClientOptions",
    "Confirm",
    "Credential",
    "Decoder",
    "Diagnostic",
    "Dispatch",
    "DispatchEntry",
    "Dispatcher",
    "Encoder",
    "Err",
    "EventStream",
    "Expr",
    "FormatException",
    "IdempotencyStore",
    "Issue",
    "Jitter",
    "Json",
    "JsonEncodable",
    "JsonParseException",
    "MacroDescriptor",
    "MacroStepPreview",
    "MemoryIdempotencyStore",
    "MemoryTokenStore",
    "Middleware",
    "Models",
    "OAuthFlow",
    "Ok",
    "OperationDescriptor",
    "Page",
    "ParamDescriptor",
    "Patch",
    "PatchAbsent",
    "PatchNull",
    "PatchValue",
    "Pkce",
    "PollResult",
    "PreviewResult",
    "RedirectPolicy",
    "RenderedRequest",
    "RequestContext",
    "ResponseContext",
    "ResponseDescriptor",
    "ResponseMeta",
    "Result",
    "RetryOptions",
    "Schema",
    "SdkDescriptors",
    "StoredToken",
    "StreamDescriptor",
    "StreamEvent",
    "TokenInfo",
    "TokenStore",
    "Typed",
    "Uint8List",
    "Unset",
    "ValidateResponses",
    "Verification",
    "Wire",
];

/// Members every Dart object has.
const OBJECT_MEMBERS: &[&str] = &["hashCode", "noSuchMethod", "runtimeType", "toString"];

/// The namer options of the Dart target: the names generated code defines
/// itself in each scope.
pub fn namer_options() -> NamerOptions {
    let object: Vec<String> = OBJECT_MEMBERS.iter().map(|s| s.to_string()).collect();
    let mut reserved: BTreeMap<Scope, Vec<String>> = BTreeMap::new();
    let types: Vec<String> = TAKEN_TYPES.iter().map(|s| s.to_string()).collect();
    reserved.insert(Scope::Types, types.clone());
    reserved.insert(Scope::Classes, types);
    let mut fields = object.clone();
    fields.extend(["additionalProperties", "fromJson", "toJson"].map(String::from));
    reserved.insert(Scope::Fields, fields);
    let mut variants = object.clone();
    variants.extend(["fromJson", "index", "name", "toJson", "values", "wire"].map(String::from));
    reserved.insert(Scope::EnumVariants, variants);
    let mut members = object.clone();
    members.push("core".into());
    reserved.insert(Scope::Members, members.clone());
    let mut client = members;
    client.extend(["close", "dispatch", "macros", "oauth"].map(String::from));
    reserved.insert(Scope::Client, client);
    reserved.insert(Scope::Macros, object);
    reserved.insert(Scope::Arguments, vec!["options".into()]);
    reserved.insert(Scope::MacroInput, vec!["options".into()]);
    NamerOptions {
        reserved,
        preview_members: true,
        pages_members: true,
        stream_members: true,
    }
}

/// Everything a file writer needs.
pub(crate) struct Gen<'p, 'a> {
    pub plan: &'p SdkPlan<'a>,
    pub names: NameMap,
    pub opts: &'p Options,
    /// The `// Generated by tungsten ...` comment lines.
    pub header: String,
}

impl<'p, 'a> Gen<'p, 'a> {
    pub fn new(plan: &'p SdkPlan<'a>, opts: &'p Options) -> Self {
        let names = Namer::new(plan).names(Target::Dart, &namer_options());
        Self {
            plan,
            names,
            opts,
            header: header(CommentStyle::DoubleSlash, plan.ir),
        }
    }

    /// The library name of a model namespace (`public`), also its file name.
    pub fn ns_module(&self, ns: &str) -> String {
        self.names
            .modules
            .get(ns)
            .cloned()
            .unwrap_or_else(|| "models".to_string())
    }

    /// The import prefix of a model namespace in other libraries
    /// (`$public`): `$` keeps it apart from every member name.
    pub fn ns_prefix(&self, ns: &str) -> String {
        format!("${}", self.ns_module(ns))
    }

    /// The path of a namespace's models under `lib/`.
    pub fn ns_path(&self, ns: &str) -> String {
        format!("src/models/{}.dart", self.ns_module(ns))
    }
}

/// TG1071 for an IR (see the crate documentation).
fn notes(plan: &SdkPlan<'_>) -> Diagnostics {
    let mut diags = Diagnostics::new();
    for t in plan.types.values() {
        match &t.kind {
            TypeKind::AllOf(AllOfPlan::Intersection) => diags.push(Diagnostic::warning(
                "TG1071",
                format!(
                    "type `{}` (an allOf that cannot be merged) has no Dart type: the Dart SDK uses its JSON value (`Object?`)",
                    t.id.0
                ),
            )),
            TypeKind::Enum(EnumPlan::Mixed { .. }) => diags.push(Diagnostic::warning(
                "TG1071",
                format!(
                    "enum `{}` is not a string or integer enum and has no Dart type: the Dart SDK uses its JSON value (`Object?`)",
                    t.id.0
                ),
            )),
            _ => {}
        }
    }
    diags
}
