// SPDX-License-Identifier: AGPL-3.0-only
//! Shared foundations for the tungsten compiler: source files and spans,
//! diagnostics with stable `TGxxxx` codes, and content digests.
//!
//! Every other tungsten crate builds on these types, and the crate has no
//! knowledge of OpenAPI, the IR or any emitter.
//!
//! # Contents
//!
//! - [`source`]: a [`SourceMap`] owns the text of every input file
//!   ([`SourceFile`], addressed by [`SourceId`]); a [`Span`] is a byte range
//!   in one of them, and the map turns it into `file:line:column`.
//! - [`diagnostic`]: a [`Diagnostic`] carries a stable code (`TG0101`), a
//!   [`Severity`], a message, optional [`Label`]s that point into the inputs
//!   and an optional help text. [`Diagnostics`] collects them in a
//!   deterministic order, and [`diagnostic::codes`] is the registry of every
//!   code the compiler can emit, with a one-line meaning.
//! - [`digest`]: a [`Digest`] is the hex BLAKE3 hash of some bytes, used in
//!   generated-file headers and to detect stale output.
//!
//! # Usage
//!
//! Compiler stages never print or panic on bad input; they push diagnostics
//! and carry on where they can:
//!
//! ```text
//! let mut sources = SourceMap::new();
//! let id = sources.add("openapi.yaml", text);
//! let mut diags = Diagnostics::new();
//! diags.push(Diagnostic::error("TG0101", "input file could not be read"));
//! if diags.has_errors() { /* report and stop */ }
//! ```

pub mod diagnostic;
pub mod digest;
pub mod source;

pub use diagnostic::{Diagnostic, Diagnostics, Label, Severity};
pub use digest::Digest;
pub use source::{SourceFile, SourceId, SourceMap, Span};
