// SPDX-License-Identifier: AGPL-3.0-only
//! Deterministic, offline inference from explicitly routed HTTP evidence.

mod engine;
pub mod har;
mod model;
pub use engine::{infer, parameter_schema_pointer, parse_evidence, stable_operation_id};
pub use model::*;
