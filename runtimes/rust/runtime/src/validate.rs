// SPDX-License-Identifier: Apache-2.0
//! Running generated validators without letting a failure of the validator
//! itself escape: a panic is reported to the caller as `Err`.

use std::panic::{AssertUnwindSafe, catch_unwind};

use serde_json::Value;

use crate::types::{Issue, Validation, Validator};

/// Run `validator` on `value`; `Err` when the validator panicked.
pub(crate) fn run(validator: &dyn Validator, value: &Value) -> Result<Validation, ()> {
    catch_unwind(AssertUnwindSafe(|| validator.validate(value))).map_err(|_| ())
}

/// [`run`] for validators that only judge (responses, page items): a panic is
/// a validation issue at the root.
pub(crate) fn judge(validator: &dyn Validator, value: &Value) -> Validation {
    run(validator, value).unwrap_or_else(|()| {
        Validation::Invalid(vec![Issue {
            path: Vec::new(),
            message: "the response schema failed (the validator panicked)".to_owned(),
        }])
    })
}
