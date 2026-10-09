// SPDX-License-Identifier: AGPL-3.0-only
//! Rust SDK and CLI emitter.
//!
//! The `rust` target writes one Cargo workspace:
//!
//! ```text
//! Cargo.toml                       workspace: <package>, <cli_package>
//! README.md
//! .github/workflows/release.yml    release build matrix of the CLI (cli module)
//! <package>/                       the SDK crate, `<api>-sdk` by default (sdk module)
//!   Cargo.toml  src/lib.rs  src/client.rs  src/descriptors.rs  src/dispatch.rs
//!   src/models/*.rs  src/resources/*.rs  src/macros.rs  src/support.rs
//!   src/custom/mod.rs     (hand-written; created once, never overwritten)
//! <cli_package>/                   the CLI crate, `<api>-cli` by default (cli module)
//!   Cargo.toml  src/main.rs  src/table.rs
//! ```
//!
//! What the two halves agree on (the `sdk` module implements the left side,
//! the `cli` module the right side; both are written against the contract
//! files, not against each other):
//!
//! - The SDK crate depends on `tungsten-runtime` (a version or a path, as
//!   the python target's `runtime` / `runtime_path` options), exposes
//!   `<Api>Client` (the client type is named like the TypeScript and Python
//!   SDKs name it), implementing `tungsten_runtime::Dispatch`, and
//!   `<Api>Client::new(options: ClientOptions) -> Result<Self, ConfigError>`.
//! - Arguments objects use the argument names of `tungsten_emit::args`
//!   rendered for Rust (`naming::render(Target::Rust, Role::Field)`), which
//!   are the field names of the generated request structs and the `name` /
//!   `arg` members of the descriptors. The CLI table's flags use the same
//!   names.
//! - The CLI crate depends on the SDK crate and `tungsten-cli-kit` (a version
//!   or a path); its `main` builds the `CliSpec` from `table.rs` and calls
//!   `tungsten_cli_kit::run(&spec, argv, <Api>Client::new)`.
//!
//! Stability: the workspace layout, the options and the runtime contract
//! files these halves share are stable. Changes are additive.
//!
//! Diagnostics: TG0740–TG0749 belong to the SDK half, TG0750–TG0759 to the
//! CLI half.

mod cli;
pub mod options;
mod sdk;

pub use options::{CliOptions, Dep, Options};
#[cfg(feature = "testing")]
pub use sdk::testing as __testing;

use tungsten_core::Diagnostics;
use tungsten_emit::{Emitter, FileSet, TargetConfig};
use tungsten_ir::Ir;

/// Emits the `rust` target.
#[derive(Debug, Clone, Copy, Default)]
pub struct RustEmitter;

impl Emitter for RustEmitter {
    fn id(&self) -> &'static str {
        "rust"
    }

    fn supports(&self, ir: &Ir) -> Diagnostics {
        let mut diags = sdk::supports(ir);
        diags.extend(cli::supports(ir));
        diags
    }

    fn emit(&self, ir: &Ir, cfg: &TargetConfig, out: &mut FileSet) -> Diagnostics {
        let mut diags = sdk::emit(ir, cfg, out);
        diags.extend(cli::emit(ir, cfg, out));
        diags
    }
}
