// SPDX-License-Identifier: AGPL-3.0-only
//! Host-only capture and central command support gate. Never uses editor sources.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tungsten_core::policy::{self, CapturedPolicy, InvocationPolicy, MAX_DOCUMENT_BYTES};
use tungsten_core::{Diagnostic, Digest};

use crate::args::{Cli, Command};
use crate::output::{CliError, CommandName, ErrorKind};
use crate::{CliEnv, Report, exit};

pub(crate) const SUPPORT: &str = "Execution policy is supported by generate (all modes), check (including --ci), report, diff, emitters and pure config inspection. policy digest rejects execution policy. All other commands, including doctor, init/wizard, scenario, lsp, mock, ir, overlay, explain, capabilities and schema, reject supplied policy before reads or effects. Policies constrain external emitters only; they do not approve executables or isolate acquisition, tools, files, environment or network.";

/// Runs before acquisition scopes, signal capture, command effects or policy
/// reads. An embedding selection is subject to exactly the same command matrix.
pub(crate) fn select(cli: &Cli, env: &CliEnv) -> Result<CliEnv, Box<Report>> {
    if cli.policy.is_some() != cli.policy_digest.is_some() {
        return Err(Box::new(
            Report::new(cli.command_name().unwrap_or(CommandName::Check)).failed(
                exit::USAGE,
                CliError::new(
                    ErrorKind::Usage,
                    "--policy and --policy-digest must be selected together",
                ),
            ),
        ));
    }
    let supported = matches!(
        cli.command,
        Command::Generate(_)
            | Command::Check(_)
            | Command::Report(_)
            | Command::Diff(_)
            | Command::Emitters(_)
            | Command::Config(_)
    );
    let supplied = cli.policy.is_some() || cli.policy_digest.is_some() || env.policy.is_some();
    if supplied && !supported {
        return Err(Box::new(
            Report::new(cli.command_name().unwrap_or(CommandName::Check)).failed(
                exit::USAGE,
                CliError::new(
                    ErrorKind::Usage,
                    "this command does not support a supplied execution policy",
                )
                .with_help(SUPPORT),
            ),
        ));
    }
    let mut selected = env.clone();
    if !supported {
        // Ordinary init/wizard may internally generate built-in output. Select
        // absence explicitly AFTER rejecting any host/CLI supplied context;
        // this does not broaden the supported policy command matrix.
        selected.policy = Some(Arc::new(InvocationPolicy::absent()));
        return Ok(selected);
    }
    let mut policy = env
        .policy
        .clone()
        .unwrap_or_else(|| Arc::new(InvocationPolicy::absent()));
    if let (Some(path), Some(expected)) = (&cli.policy, &cli.policy_digest) {
        let ordinal = policy.document_count();
        let result = capture(path, &Digest(expected.clone()))
            .and_then(|document| policy.intersect(document));
        policy = Arc::new(result.map_err(|mut diagnostic| {
            for label in &mut diagnostic.labels {
                label.file = format!("policy[{ordinal}]");
            }
            if diagnostic.labels.is_empty() {
                diagnostic = diagnostic.at(format!("policy[{ordinal}]"), "", None);
            }
            let mut report = Report::new(cli.command_name().unwrap_or(CommandName::Check));
            report.exit = exit::FAILED;
            report.diagnostics.push(diagnostic);
            Box::new(report)
        })?);
    }
    selected.policy = Some(policy);
    Ok(selected)
}

pub(crate) fn capture(path: &Path, expected: &Digest) -> Result<CapturedPolicy, Diagnostic> {
    let (bytes, identity) = read(path)?;
    let digest = Digest::of(&bytes);
    CapturedPolicy::from_bytes(&bytes, expected)
        .map(|document| document.with_local_identity(identity))
        .map_err(|diagnostic| diagnostic.at("policy[0]", "", None).with_help(format!("captured policy digest {}; select the reviewed exact pin; authored values and paths are omitted", digest.0)))
}

/// One regular local handle, one bounded read. A canonical path is local
/// provenance only; the pin is checked on the exact bytes from this handle.
pub(crate) fn read(path: &Path) -> Result<(Vec<u8>, PathBuf), Diagnostic> {
    let fail = |_| policy::invalid();
    if path.to_string_lossy().contains("://") {
        return Err(policy::invalid());
    }
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        // UNC, device and verbatim namespaces are outside this local adapter.
        if path.components().any(|part| matches!(part, Component::Prefix(prefix) if !matches!(prefix.kind(), Prefix::Disk(_))))
            || (path.has_root() && !path.is_absolute())
            || path.components().any(|part| matches!(part, Component::Prefix(_))) && !path.is_absolute()
        { return Err(policy::invalid()); }
        for part in path.components().filter_map(|part| {
            if let Component::Normal(value) = part {
                value.to_str()
            } else {
                None
            }
        }) {
            let base = part
                .split('.')
                .next()
                .unwrap_or("")
                .trim_end_matches(' ')
                .to_ascii_uppercase();
            if part.contains(':')
                || part.ends_with([' ', '.'])
                || matches!(
                    base.as_str(),
                    "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
                )
                || base
                    .strip_prefix("COM")
                    .or_else(|| base.strip_prefix("LPT"))
                    .is_some_and(|suffix| {
                        matches!(
                            suffix,
                            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                        )
                    })
            {
                return Err(policy::invalid());
            }
        }
    }
    let selected = fs::symlink_metadata(path).map_err(fail)?;
    if selected.file_type().is_symlink() || !selected.is_file() || reparse(&selected) {
        return Err(policy::invalid());
    }
    let identity = fs::canonicalize(path).map_err(fail)?;
    let file = open(path)?;
    let opened = file.metadata().map_err(fail)?;
    if !opened.is_file() || reparse(&opened) || opened.len() > MAX_DOCUMENT_BYTES as u64 {
        return Err(policy::invalid());
    }
    let mut bytes = Vec::new();
    file.take((MAX_DOCUMENT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(fail)?;
    if bytes.len() > MAX_DOCUMENT_BYTES || std::str::from_utf8(&bytes).is_err() {
        return Err(policy::invalid());
    }
    Ok((bytes, identity))
}

#[cfg(unix)]
fn open(path: &Path) -> Result<File, Diagnostic> {
    use nix::fcntl::{OFlag, open};
    use nix::sys::stat::Mode;
    open(
        path,
        OFlag::O_RDONLY | OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| policy::invalid())
}

#[cfg(windows)]
fn open(path: &Path) -> Result<File, Diagnostic> {
    use std::os::windows::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x0020_0000) // FILE_FLAG_OPEN_REPARSE_POINT
        .open(path)
        .map_err(|_| policy::invalid())
}

#[cfg(not(any(unix, windows)))]
fn open(_: &Path) -> Result<File, Diagnostic> {
    Err(policy::invalid())
}

fn reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}
