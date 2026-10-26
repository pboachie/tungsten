// SPDX-License-Identifier: AGPL-3.0-only
//! Named starting points for `tungsten init`.
//!
//! A preset only fills in what the invocation left unset: explicit
//! `--targets` and `--strict` always win, and a preset never selects inputs,
//! approves operations or changes agent safety defaults. The library preset
//! also writes a local profile v1 the manifest selects, so later projects can
//! reuse the same defaults.

use tungsten_config::capabilities::INIT;

use crate::args::{InitArgs, InitPreset};

/// The profile the library preset writes, relative to the project.
pub(super) const LIBRARY_PROFILE: &str = "profiles/library.yml";
/// The include root the manifest selects for it.
pub(super) const LIBRARY_PROFILE_ROOT: &str = "profiles";

/// Names and descriptions accepted where the guided setup asks for
/// outputs, from the capability catalog.
pub(super) fn names() -> impl Iterator<Item = (&'static str, &'static str)> {
    INIT.presets
        .iter()
        .map(|preset| (preset.name, preset.description))
}

pub(crate) fn name(preset: InitPreset) -> &'static str {
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

/// CLI presets without a catalog declaration, as conflicts reported by
/// `tungsten capabilities`.
pub(crate) fn undeclared() -> Vec<String> {
    use clap::ValueEnum;
    InitPreset::value_variants()
        .iter()
        .map(|preset| name(*preset))
        .filter(|name| INIT.preset(name).is_none())
        .map(|name| format!("init preset `{name}` is not declared in the capability catalog"))
        .collect()
}

/// The catalog declaration of a CLI preset; every variant is declared.
fn declaration(preset: InitPreset) -> &'static tungsten_config::capabilities::InitPreset {
    INIT.preset(name(preset))
        .expect("every init preset is declared in the capability catalog")
}

/// Fill the preset's defaults into `args` where the invocation did not
/// choose.
pub(super) fn apply(args: &mut InitArgs) {
    let Some(preset) = args.preset else {
        return;
    };
    if args.targets.is_empty() && args.add_targets.is_empty() {
        args.targets = declaration(preset)
            .targets
            .iter()
            .map(|t| (*t).to_string())
            .collect();
    }
    if declaration(preset).strict {
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
