// SPDX-License-Identifier: AGPL-3.0-only
#[allow(unused_imports)]
pub use tungsten_emit::tool_catalog::compiler::{MAX_TOOL_NAME, sanitize, shorten};
#[cfg(feature = "testing")]
pub(crate) fn assign(entries: &[(String, String)]) -> (Vec<String>, Vec<(String, Vec<String>)>) {
    tungsten_emit::tool_catalog::assign_names(entries)
}
