// SPDX-License-Identifier: AGPL-3.0-only
//! Browser package options. SDK inference and npm validation are shared with MCP.

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::TargetConfig;
use tungsten_ir::Ir;

/// Resolved npm package configuration for the browser adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub package: String,
    pub version: String,
    pub sdk_package: String,
    pub sdk: String,
    pub runtime: String,
    /// Dependency specifier of the SDK's browser client runtime.
    pub sdk_runtime: String,
}

impl Options {
    /// Resolve `package`, `version`, `sdk`, `sdk_path`, `runtime` and
    /// `runtime_path`, `sdk_runtime` and `sdk_runtime_path`. The CLI injects
    /// the sibling `typescript_target` options.
    pub fn resolve(ir: &Ir, cfg: &TargetConfig) -> (Self, Diagnostics) {
        let (resolved, mut diagnostics) = tungsten_emit::tool_catalog::package::Options::resolve(
            ir,
            cfg,
            "webmcp",
            "TG0790",
            "TG0792",
            "WebMCP browser adapter",
        );
        let package = resolved.package;
        let mut read_runtime = |key: &str| {
            cfg.options.get(key).and_then(|value| match value.as_str() {
                Some(text) if !text.trim().is_empty() => Some(text.to_string()),
                _ => {
                    diagnostics.push(
                        Diagnostic::warning("TG0790", format!(
                            "target option `webmcp.{key}` must be a non-empty string; the default is used"
                        )).with_help(format!("got {value}")),
                    );
                    None
                }
            })
        };
        let sdk_runtime_range = read_runtime("sdk_runtime");
        let sdk_runtime_path = read_runtime("sdk_runtime_path");
        let sdk_runtime = sdk_runtime_path.map_or_else(
            || sdk_runtime_range.unwrap_or_else(|| "^0.1.0".into()),
            |path| format!("file:{path}"),
        );
        (
            Self {
                package,
                version: resolved.version,
                sdk_package: resolved.sdk_package,
                sdk: resolved.sdk,
                runtime: resolved.runtime,
                sdk_runtime,
            },
            diagnostics,
        )
    }
}
