// SPDX-License-Identifier: AGPL-3.0-only
//! One module per command. Each returns a [`Report`]; none writes to the
//! output streams.

pub(crate) mod bundle;
mod capabilities;
pub(crate) mod check;
mod config;
pub(crate) mod diff;
pub(crate) mod doctor;
pub(crate) mod emitters;
pub(crate) mod explain;
pub(crate) mod generate;
pub(crate) mod init;
mod init_advanced;
mod init_edit;
mod init_example;
mod init_input;
mod init_preset;
mod init_setup;
mod init_setup_mcp;
mod init_setup_python;
mod init_setup_rust;
mod ir;
pub(crate) mod lock;
pub(crate) mod lsp;
pub(crate) mod mock;
pub(crate) mod overlay;
mod policy;
pub(crate) mod refresh_import;
pub(crate) mod report;
pub(crate) mod scenario;
mod schema;
pub(crate) mod support_bundle;
pub(crate) mod support_bundle_attach;
mod support_bundle_file;
pub(crate) mod support_bundle_inspect;
mod support_bundle_rehearsal;
pub(crate) mod update;
pub(crate) mod update_notice;
pub(crate) mod upgrade;
pub(crate) mod upgrade_apply;
mod wizard;
pub(crate) mod workspace;

use crate::args::{
    BundleCommand, Cli, Command, IrCommand, OverlayCommand, PolicyCommand, UpgradeCommand,
};
use crate::{CliEnv, Report};

pub(crate) fn dispatch(
    cli: &Cli,
    env: &CliEnv,
    cancellation: &tungsten_mock::RunCancellation,
) -> Report {
    let source = match &cli.command {
        Command::Check(args) => Some(args.input.path.as_path()),
        Command::Ir {
            command: IrCommand::Dump(args),
        } => Some(args.input.path.as_path()),
        Command::Init(args) => args.from.as_deref(),
        Command::RefreshImport(args) => args.from.as_deref(),
        Command::Generate(args) => Some(args.input.path.as_path()),
        Command::Mock(args) => Some(args.input.path.as_path()),
        Command::Scenario { command } => Some(scenario::input(command)),
        Command::Report(args) => Some(args.input.path.as_path()),
        Command::Diff(args) => Some(args.input.path.as_path()),
        Command::Overlay {
            command: OverlayCommand::Suggest(args),
        } => Some(args.input.path.as_path()),
        _ => None,
    };
    // `--locked` (or `locked: true` in the manifest) reproduces from pinned
    // inputs only: acquisition is offline and refreshing a pin is a
    // separate, explicit step.
    let locked = match &cli.command {
        Command::Check(args) => lock::required(&args.input.path, args.locked),
        Command::Generate(args) => lock::required(&args.input.path, args.locked),
        _ => false,
    };
    if matches!(cli.command, Command::Upgrade { .. }) && cli.refresh_inputs {
        return crate::Report::new(cli.command_name().unwrap_or(crate::output::CommandName::Check))
            .failed(
                crate::exit::USAGE,
                crate::output::CliError::new(
                    crate::output::ErrorKind::Usage,
                    "an upgrade never refreshes inputs; --refresh-inputs cannot be combined with upgrade",
                )
                .with_help("refresh and review inputs as a separate step, then run `tungsten lock --update` before rehearsing"),
            );
    }
    if locked && cli.refresh_inputs {
        return crate::Report::new(cli.command_name().unwrap_or(crate::output::CommandName::Check))
            .failed(
                crate::exit::USAGE,
                crate::output::CliError::new(
                    crate::output::ErrorKind::Usage,
                    "locked mode (--locked or `locked: true`) cannot be combined with --refresh-inputs",
                )
                .with_help("refresh inputs and review them first, then run `tungsten lock --update` before a locked run"),
            );
    }
    tungsten_openapi::acquire::with_options(
        tungsten_openapi::acquire::Options {
            offline: cli.offline || locked,
            refresh: cli.refresh_inputs,
            headers: cli.input_header.clone(),
            header_sources: source
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
        },
        || dispatch_with_inputs(cli, env, cancellation),
    )
}

fn dispatch_with_inputs(
    cli: &Cli,
    env: &CliEnv,
    cancellation: &tungsten_mock::RunCancellation,
) -> Report {
    match &cli.command {
        Command::Check(args) => check::run(args, env, cli.no_external),
        Command::Ir {
            command: IrCommand::Dump(args),
        } => ir::dump(args, cli.json),
        Command::Explain(args) => explain::run(args),
        Command::Schema(args) => schema::run(args),
        Command::Init(args) => wizard::run(args, cli.json, env),
        Command::Doctor(args) => doctor::run(args, env),
        Command::Capabilities(args) => capabilities::run(args),
        Command::Config(args) => config::run(args, env),
        Command::Workspace(args) => workspace::run(args),
        Command::Lock(args) => lock::run(args, env),
        Command::RefreshImport(args) => refresh_import::run(args, env, cli.no_external),
        Command::Bundle {
            command: BundleCommand::Export(args),
        } => bundle::export(args),
        Command::Bundle {
            command: BundleCommand::Import(args),
        } => bundle::import(args),
        Command::Policy {
            command: PolicyCommand::Digest(args),
        } => policy::digest(args),
        Command::Emitters(args) => emitters::run(args, env, cli.no_external),
        Command::Generate(args) => generate::run(args, env, cli.no_external),
        Command::Mock(args) => mock::start(args),
        Command::Scenario { command } => scenario::run(command, cancellation),
        Command::Report(args) => report::run(args, env, cli.no_external),
        Command::Diff(args) => diff::run(args, env, cli.no_external),
        Command::Overlay {
            command: OverlayCommand::Suggest(args),
        } => overlay::suggest(args),
        Command::SupportBundle(args) => support_bundle::run(args, env, cli, cancellation),
        Command::Update { command } => update::run(command, cli.offline),
        Command::Upgrade {
            command: UpgradeCommand::Rehearse(args),
        } => upgrade::rehearse(args, env),
        Command::Upgrade {
            command: UpgradeCommand::Apply(args),
        } => upgrade_apply::run(args),
        Command::Lsp(_) => lsp::refuse(),
    }
}
