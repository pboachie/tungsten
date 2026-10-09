// SPDX-License-Identifier: Apache-2.0
//! Exit codes of the output contract (see the crate documentation).

use tungsten_runtime::{Category, Diagnostic, Retryable};

pub(crate) const OK: u8 = 0;
pub(crate) const INTERNAL: u8 = 1;
pub(crate) const USAGE: u8 = 2;
pub(crate) const API_ERROR: u8 = 3;
pub(crate) const API_RETRYABLE: u8 = 4;
pub(crate) const OUTCOME_UNKNOWN: u8 = 5;
pub(crate) const CONFIRMATION_REQUIRED: u8 = 6;
pub(crate) const AUTH_MISSING: u8 = 7;

/// The exit code of a failed call. Pre-flight failures have no HTTP status:
/// a validation or malformed request is a usage error (2), a missing or
/// malformed credential is 7; everything else is decided by what a retry
/// could do.
pub(crate) fn for_diagnostic(d: &Diagnostic) -> u8 {
    for_parts(d.category, d.http_status.is_some(), d.retryable)
}

/// [`for_diagnostic`] from the three members that decide it.
pub(crate) fn for_parts(category: Category, has_status: bool, retryable: Retryable) -> u8 {
    match category {
        Category::OutcomeUnknown => OUTCOME_UNKNOWN,
        Category::ConfirmationRequired => CONFIRMATION_REQUIRED,
        Category::ValidationFailed | Category::MalformedRequest if !has_status => USAGE,
        Category::AuthFailed if !has_status => AUTH_MISSING,
        _ if retryable != Retryable::Never => API_RETRYABLE,
        _ => API_ERROR,
    }
}
