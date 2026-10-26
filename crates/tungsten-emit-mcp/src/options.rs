// SPDX-License-Identifier: AGPL-3.0-only
//! Target options (`tungsten.yml` `targets.mcp`).
//!
//! | Option | Meaning | Default |
//! |---|---|---|
//! | `package` | npm package name of the server | `<api>-mcp` |
//! | `version` | package version | `0.1.0` |
//! | `sdk` | the generated TypeScript SDK: `<package>` or `<package>@<range>` | the `typescript` target's `package` and `^<version>` when `tungsten.yml` configures them (TG0726), else its default package, `^0.1.0` |
//! | `sdk_path` | depend on the SDK by path (`file:<path>`) instead of a range | |
//! | `runtime` | `@elysra/tungsten-mcp` version range | `^0.1.3` |
//! | `runtime_path` | depend on `@elysra/tungsten-mcp` by path | |
//! | `sdk_runtime` | direct `@elysra/tungsten-runtime` version range | `^0.1.3` |
//! | `sdk_runtime_path` | use the same local client runtime as the linked SDK | |
//! | `search.embeddings` | the optional embedding index: `provider` (`command` or `http`), `command` or `url`, `model`, `dimensions`, `api_key_env`, `required`, `batch_size`, `timeout_ms` (TG0570 to TG0574; see `embeddings`) | off |
//! | `sandbox` | enable the opt-in `run_script` sandbox (engine `auto`: deno when installed, else the WASM isolate; set `sandbox.engine` in `customize`) | `false` |
//!
//! The command line passes the sibling `typescript` target's options as the
//! reserved option `typescript_target` (`{package, version}`); it is not a
//! `tungsten.yml` key. An invalid value is a TG0720 warning and the default
//! is used.

use serde_json::Value;
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::TargetConfig;
use tungsten_ir::Ir;

/// Default version range of `@elysra/tungsten-mcp` and `@elysra/tungsten-runtime`.
pub const DEFAULT_RANGE: &str = tungsten_config::option_fields::NPM_RUNTIME_RANGE;
/// TypeScript version pinned in the generated package.
pub const TYPESCRIPT_VERSION: &str = "5.9.3";
/// `@types/node` version pinned in the generated package.
pub const NODE_TYPES_VERSION: &str = "22.20.5";

/// Resolved target options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// npm package name of the server.
    pub package: String,
    pub version: String,
    /// Executable name: `<api>-mcp`.
    pub bin: String,
    /// npm package name of the generated SDK.
    pub sdk_package: String,
    /// Dependency specifier of the SDK.
    pub sdk: String,
    /// Dependency specifier of `@elysra/tungsten-mcp`.
    pub runtime: String,
    /// Direct dependency shared with the generated SDK and MCP runtime peer.
    pub sdk_runtime: String,
    pub sandbox: bool,
    /// `search.embeddings`: the optional embedding index.
    pub embeddings: Option<crate::embeddings::Config>,
}

impl Options {
    /// Resolve package dependencies through the shared transport adapter resolver.
    pub fn resolve(ir: &Ir, cfg: &TargetConfig) -> (Options, Diagnostics) {
        let (shared, mut diags) = tungsten_emit::tool_catalog::package::Options::resolve(
            ir,
            cfg,
            "mcp",
            "TG0720",
            "TG0726",
            "MCP server",
        );
        let sandbox = match cfg.options.get("sandbox") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(value) => {
                diags.push(
                    Diagnostic::warning(
                        "TG0720",
                        "target option `mcp.sandbox` must be true or false; the default is used",
                    )
                    .with_help(format!("got {value}")),
                );
                false
            }
        };
        let mut read_runtime = |key: &str| {
            cfg.options.get(key).and_then(|value| match value.as_str() {
                Some(text) if !text.trim().is_empty() => Some(text.to_owned()),
                _ => {
                    diags.push(Diagnostic::warning("TG0720", format!("target option `mcp.{key}` must be a non-empty string; the default is used")));
                    None
                }
            })
        };
        let sdk_runtime_range = read_runtime("sdk_runtime");
        let sdk_runtime_path = read_runtime("sdk_runtime_path");
        let sdk_runtime = sdk_runtime_path.map_or_else(
            || sdk_runtime_range.unwrap_or_else(|| DEFAULT_RANGE.into()),
            |path| format!("file:{path}"),
        );
        let embeddings = match crate::embeddings::parse(&cfg.options) {
            Ok(config) => config,
            Err(diagnostic) => {
                diags.push(diagnostic);
                None
            }
        };
        (
            Options {
                package: shared.package,
                version: shared.version,
                bin: shared.bin,
                sdk_package: shared.sdk_package,
                sdk: shared.sdk,
                runtime: shared.runtime,
                sdk_runtime,
                sandbox,
                embeddings,
            },
            diags,
        )
    }
}
