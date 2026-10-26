// SPDX-License-Identifier: AGPL-3.0-only
//! Named starting points for `tungsten init`.
//!
//! A preset only fills in what the invocation left unset: explicit
//! `--targets` and `--strict` always win, and a preset never selects inputs,
//! approves operations or changes agent safety defaults. The library preset
//! also writes a local profile v1 the manifest selects, so later projects can
//! reuse the same defaults.

use crate::args::{InitArgs, InitPreset};

/// The profile the library preset writes, relative to the project.
pub(super) const LIBRARY_PROFILE: &str = "profiles/library.yml";
/// The include root the manifest selects for it.
pub(super) const LIBRARY_PROFILE_ROOT: &str = "profiles";

/// Names accepted where the guided setup asks for outputs.
pub(super) const NAMES: [(&str, &str); 3] = [
    ("app", "TypeScript SDK and docs for one application"),
    (
        "library",
        "TypeScript, Python and Rust SDKs with docs, sharing a local profile",
    ),
    (
        "agent",
        "MCP server on the TypeScript SDK, generated with --strict",
    ),
];

pub(super) fn name(preset: InitPreset) -> &'static str {
    match preset {
        InitPreset::App => "app",
        InitPreset::Library => "library",
        InitPreset::Agent => "agent",
    }
}

pub(super) fn parse(name: &str) -> Option<InitPreset> {
    match name {
        "app" => Some(InitPreset::App),
        "library" => Some(InitPreset::Library),
        "agent" => Some(InitPreset::Agent),
        _ => None,
    }
}

fn targets(preset: InitPreset) -> &'static [&'static str] {
    match preset {
        InitPreset::App => &["typescript", "docs"],
        InitPreset::Library => &["typescript", "python", "rust", "docs"],
        InitPreset::Agent => &["typescript", "mcp"],
    }
}

/// Fill the preset's defaults into `args` where the invocation did not
/// choose.
pub(super) fn apply(args: &mut InitArgs) {
    let Some(preset) = args.preset else {
        return;
    };
    if args.targets.is_empty() && args.add_targets.is_empty() {
        args.targets = targets(preset).iter().map(|t| (*t).to_string()).collect();
    }
    if preset == InitPreset::Agent {
        args.strict = true;
    }
}

/// Whether the manifest selects the library profile.
pub(super) fn writes_profile(args: &InitArgs) -> bool {
    args.preset == Some(InitPreset::Library)
}

/// The library profile: shared package versions for each SDK. Projects
/// override any of these fields in their own `targets`.
pub(super) fn library_profile() -> String {
    "\
# Local profile v1 written by `tungsten init --preset library`.
# Shared SDK defaults. A project overrides any field under its own
# `targets`; `tungsten config` shows which source each value came from.
# Select it from another project with
#   profiles: [{ path: <relative path to this file>, root: <its directory> }]
tungsten_profile: 1
targets:
  typescript:
    version: 0.1.0
  python:
    version: 0.1.0
  rust:
    version: 0.1.0
"
    .to_string()
}
