// SPDX-License-Identifier: AGPL-3.0-only
//! Narrow adapters for the private regression harness.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Map an already captured compiler result through the production LSP mapper.
pub fn diagnostics(
    compiled: &tungsten_build::Compiled,
    manifest: &Path,
) -> BTreeMap<PathBuf, Vec<lsp_types::Diagnostic>> {
    let base = manifest
        .parent()
        .map(crate::uri::normalize)
        .unwrap_or_default();
    let name = manifest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    crate::analysis::map_diagnostics(compiled, &base, &name, &crate::uri::normalize(manifest))
}

/// The server's file URI encoder, for protocol-correct private clients.
pub fn file_uri(path: &Path) -> Option<lsp_types::Uri> {
    crate::uri::path_to_uri(path)
}
