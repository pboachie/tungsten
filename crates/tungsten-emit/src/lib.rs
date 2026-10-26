// SPDX-License-Identifier: AGPL-3.0-only
//! Shared emitter infrastructure.
//!
//! Every emitter implements [`Emitter`] and writes into a [`FileSet`] using a
//! [`Writer`] (blocks, indentation, doc comments) and an [`Imports`]
//! collector. [`write_output`] puts a file set on disk together with
//! `.tungsten/manifest.json`; [`check_stale`] compares it without writing.
//! [`schema`] renders IR types as JSON Schema, [`args`] says how an
//! operation's arguments object is laid out and [`compact`] builds the
//! compact tool schemas, for every emitter that describes operations to
//! agents. [`surface`] snapshots the API surface (`.tungsten/surface.json`)
//! and classifies changes between snapshots. [`external`] runs emitters
//! that are separate executables and speak the JSON protocol documented
//! there. [`sdk`] is the language-neutral SDK plan, naming and descriptor
//! document the further SDK languages are built on.
//!
//! Stability: the public signatures in this crate are shared by every
//! emitter. Changes are additive.

pub mod args;
pub mod compact;
pub mod external;
mod fileset;
mod imports;
mod output;
mod prepared;
pub mod schema;
pub mod sdk;
mod staging;
pub mod surface;
pub mod tool_catalog;
pub mod webhooks;
mod writer;

use std::path::PathBuf;

use tungsten_core::Diagnostics;
use tungsten_ir::Ir;

pub use fileset::{FileSet, FileSetError};
pub use imports::Imports;
pub use output::{
    MANIFEST_FORMAT, MANIFEST_PATH, ManifestFile, OutputManifest, StaleFile, StaleReason,
    WriteOptions, WriteReport, check_stale, header, header_text, is_custom_path, stale_files,
    write_output, write_output_with_cancel,
};
pub use prepared::PreparedGeneration;
pub use staging::{OutputStaging, StagedFile, StagedOutput, StagingLimits};
pub use writer::{CommentStyle, Writer};

/// Options for one target, from `tungsten.yml` `targets.<name>`.
#[derive(Debug, Clone)]
pub struct TargetConfig {
    /// Target name (`typescript`, `docs`, ...).
    pub name: String,
    /// Output directory, resolved against the manifest's directory.
    pub out_dir: PathBuf,
    /// The raw target options object from the manifest.
    pub options: serde_json::Value,
}

impl TargetConfig {
    /// A string option, if present.
    pub fn option_str(&self, key: &str) -> Option<&str> {
        self.options.get(key).and_then(|v| v.as_str())
    }
}

/// One code generator.
pub trait Emitter {
    /// Stable target id matching the `tungsten.yml` targets key.
    fn id(&self) -> &'static str;
    /// Feature gaps for this IR (TG07xx). Never fails.
    fn supports(&self, ir: &Ir) -> Diagnostics;
    /// Write the target's files into `out` (paths relative to the target's
    /// output directory, forward slashes). Problems become diagnostics; an
    /// emitter never panics on a valid IR.
    fn emit(&self, ir: &Ir, cfg: &TargetConfig, out: &mut FileSet) -> Diagnostics;
    /// Whether preparation may share the language-neutral SDK plan. Native
    /// target naming and lowering still belong to this emitter.
    fn uses_sdk_plan(&self) -> bool {
        false
    }
    /// Support checks using a previously prepared SDK plan. The default is
    /// the legacy adapter for emitters with their own planning conventions.
    fn supports_prepared(&self, ir: &Ir, _plan: &sdk::SdkPlan<'_>) -> Diagnostics {
        self.supports(ir)
    }
    /// Emission using the same immutable plan as support checks.
    fn emit_prepared(
        &self,
        ir: &Ir,
        _plan: &sdk::SdkPlan<'_>,
        cfg: &TargetConfig,
        out: &mut FileSet,
    ) -> Diagnostics {
        self.emit(ir, cfg, out)
    }
}
