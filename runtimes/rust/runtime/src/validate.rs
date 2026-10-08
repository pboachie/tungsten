// SPDX-License-Identifier: Apache-2.0
//! Running generated validators without letting a failure of the validator
//! itself escape: a panic is reported as a validation issue.

use std::panic::{AssertUnwindSafe, catch_unwind};

use serde_json::Value;

use crate::types::{Issue, Validation, Validator};

/// Run `validator` on `value`.
pub(crate) fn run(validator: &dyn Validator, value: &Value) -> Validation {
    catch_unwind(AssertUnwindSafe(|| validator.validate(value))).unwrap_or_else(|_| {
        Validation::Invalid(vec![Issue {
            path: Vec::new(),
            message: "the validator failed".to_owned(),
        }])
    })
}
