// SPDX-License-Identifier: AGPL-3.0-only
//! Shared foundations for the tungsten compiler: source files and spans,
//! diagnostics with stable `TGxxxx` codes, and content digests.

pub mod diagnostic;
pub mod digest;
pub mod source;

pub use diagnostic::{Diagnostic, Diagnostics, Label, Severity};
pub use digest::Digest;
pub use source::{SourceFile, SourceId, SourceMap, Span};
