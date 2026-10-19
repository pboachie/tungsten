// SPDX-License-Identifier: AGPL-3.0-only
//! Target options (`tungsten.yml` `targets.typescript`).

use std::collections::BTreeMap;

use tungsten_core::Diagnostics;
use tungsten_emit::TargetConfig;
use tungsten_ir::Ir;
use tungsten_ir::naming::{self, Case};

/// Default `@elysra/tungsten-runtime` version range.
pub const DEFAULT_RUNTIME_RANGE: &str = "^0.1.3";
/// Zod version range of generated packages: `exactOptional()`, which keeps
/// optional properties exact, exists since 4.3.0.
pub const ZOD_RANGE: &str = "^4.3.0";
/// TypeScript version pinned in generated packages.
pub const TYPESCRIPT_VERSION: &str = "5.9.3";

/// Resolved target options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// npm package name.
    pub package: String,
    /// Package version.
    pub version: String,
    /// `@elysra/tungsten-runtime` dependency specifier.
    pub runtime: String,
    /// Additional npm dependencies of hand-written `src/custom` extensions.
    pub dependencies: BTreeMap<String, String>,
}

impl Options {
    /// Read the options of `cfg`, falling back to defaults with a TG0711
    /// warning for each invalid scalar value. Invalid custom dependencies
    /// produce TG0711 errors so generation cannot silently omit required packages.
    pub fn resolve(ir: &Ir, cfg: &TargetConfig) -> (Options, Diagnostics) {
        let (raw, diags) = tungsten_config::target_options::TypeScriptOptions::parse(&cfg.options);
        let package = raw
            .package
            .unwrap_or_else(|| format!("{}-sdk", naming::to_case(&ir.api.name.words, Case::Kebab)));
        let version = raw.version.unwrap_or_else(|| "0.1.0".to_string());
        let runtime = match raw.runtime_path {
            Some(path) => format!("file:{path}"),
            None => raw
                .runtime
                .unwrap_or_else(|| DEFAULT_RUNTIME_RANGE.to_string()),
        };
        let dependencies = raw.dependencies.unwrap_or_default();
        (
            Options {
                package,
                version,
                runtime,
                dependencies,
            },
            diags,
        )
    }
}
