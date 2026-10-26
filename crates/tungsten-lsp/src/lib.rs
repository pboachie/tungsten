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
//!   JSON/YAML spans the frontend keeps; manifests, specs, overlays and local
//!   references use unsaved buffers over the same bounded disk reader;
//! - completion offers the keys of each manifest from its published JSON
//!   Schema, enum values, and from the compiled project the operation ids,
//!   gates, clusters and security schemes the manifest can refer to;
//! - hover documents a field, a value, an operation (method, path,
//!   summary) or a diagnostic code;
//! - go-to-definition on an operation id opens the operation in its
//!   OpenAPI document;
//! - document symbols outline the manifest;
//! - quick fixes replace an unknown operation id or manifest value named
//!   by a diagnostic with the closest known name, as a versioned edit the
//!   editor previews and can undo;
//! - TG0401/TG0403 on an operation id get the overlay action `tungsten
//!   overlay suggest` derives, as one workspace edit that adds it to the
//!   input's suggested overlay (creating and listing that file in
//!   `tungsten.yml` when needed), previewed before it is applied.
//!
//! The server keeps no compilation: after each run it keeps the mapped
//! diagnostics and an index with one small entry per operation, so memory
//! does not grow with the size of the specs. Edits are debounced, one
//! compilation runs at a time, and a result that an edit has overtaken is
//! cancelled cooperatively and discarded instead of published. The current
//! read, parse or indivisible pass finishes before its next checkpoint.
//!
//! The README of the repository shows how to start it from Visual Studio Code
//! and Neovim.

mod actions;
mod analysis;
mod complete;
mod docs;
mod hover;
mod overlay_actions;
mod position;
mod scan;
mod schema;
mod server;
mod symbols;
mod uri;

#[cfg(feature = "testing")]
#[doc(hidden)]
pub mod __testing;

pub use lsp_server;
pub use lsp_types;
pub use server::{Options, run_stdio, serve};
