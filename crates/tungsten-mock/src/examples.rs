// SPDX-License-Identifier: AGPL-3.0-only
//! Bounded deterministic request examples for local onboarding only.
use crate::{
    MockOptions,
    generate::Generator,
    model::Model,
    validate::{Context, Validator},
};
use serde_json::Value;
use tungsten_ir::{Ir, TypeRef};

/// Produces synthetic values and rejects schemas whose constraints cannot be
/// satisfied by the bounded mock generator. Never fetches data or executes input.
#[derive(Debug)]
pub struct RequestExamples {
    model: Model,
}

impl RequestExamples {
    pub fn new(ir: Ir) -> Self {
        Self {
            model: Model::new(ir, &MockOptions::default()),
        }
    }

    pub fn value(&self, ty: &TypeRef, operation: &str, path: &str) -> Result<Value, String> {
        let value = Generator::new(&self.model, Context::Request).value(ty, operation, path);
        Validator::new(&self.model, Context::Request).check(ty, &value).map_err(|_| "cannot synthesize a valid local example for this schema; provide a simpler local operation".to_owned())?;
        Ok(value)
    }
}
