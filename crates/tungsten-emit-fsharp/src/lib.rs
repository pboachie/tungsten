// SPDX-License-Identifier: AGPL-3.0-only
//! The `fsharp` target: an F# SDK over the .NET runtime (`Tungsten.Runtime`)
//! and its F# layer (`Tungsten.Runtime.FSharp`), built on the shared SDK plan,
//! names and descriptor document of [`tungsten_emit::sdk`].
//!
//! Only the option resolution and the F# source helpers exist so far.
//! Diagnostics: TG1080 to TG1089 (planning/05 "F# SDK").

pub mod fs;
pub mod options;

pub use options::Options;
