// SPDX-License-Identifier: AGPL-3.0-only
//! Deterministic, offline inference from explicitly routed HTTP evidence.

mod engine;
mod model;
pub use engine::{infer, parse_evidence, stable_operation_id};
pub use model::*;
