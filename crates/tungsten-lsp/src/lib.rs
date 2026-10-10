// SPDX-License-Identifier: AGPL-3.0-only
//! Language server for tungsten's manifests.
//!
//! `tungsten lsp` serves the Language Server Protocol over stdio for
//! `tungsten.yml`, `agent.yml` and the overlay files a project lists. The
//! workspace is the directory of the `tungsten.yml` (found from the
//! workspace folder or given with `--config`), and it is compiled once for
//! all the files in it:
//!
//! - diagnostics are the compiler's own, with their `TG` codes, placed on
//!   the YAML spans the frontend keeps; unsaved buffers of the two
//!   manifests are what gets checked, while specs and overlays are read
//!   from disk (so they update on save);
//! - completion offers the keys of each manifest from its published JSON
//!   Schema, enum values, and from the compiled project the operation ids,
//!   gates, clusters and security schemes the manifest can refer to;
//! - hover documents a field, a value, an operation (method, path,
//!   summary) or a diagnostic code;
//! - go-to-definition on an operation id opens the operation in its
//!   OpenAPI document;
//! - document symbols outline the manifest.
//!
//! The server keeps no compilation: after each run it keeps the mapped
//! diagnostics and an index with one small entry per operation, so memory
//! does not grow with the size of the specs. Edits are debounced, one
//! compilation runs at a time, and a result that an edit has overtaken is
//! discarded instead of published.
//!
//! The README of the repository shows how to start it from Visual Studio Code
//! and Neovim.

mod analysis;
mod complete;
mod docs;
mod hover;
mod position;
mod scan;
mod schema;
mod server;
mod symbols;
mod uri;

pub use lsp_server;
pub use lsp_types;
pub use server::{Options, run_stdio, serve};
