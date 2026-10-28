// SPDX-License-Identifier: AGPL-3.0-only
//! Opt-in, cached update notices for interactive use.
//!
//! Ordinary commands never touch the network for this. `tungsten update check`
//! records its verified answer in a small cache file when notices are enabled;
//! after an interactive human-mode command the binary reads that file (bounded)
//! and, at most once per interval, prints one line to stderr naming a newer
//! release whose archive and `SHA256SUMS` the check confirmed. Every failure
//! (missing, corrupt, stale or unwritable cache, malformed settings) is silent
//! and never changes a command's output or exit code.
//!
//! Notices stay off unless enabled by `TUNGSTEN_UPDATE_NOTICE` or the user
//! settings file, and are suppressed by `TUNGSTEN_NO_UPDATE_CHECK`, CI,
//! `--offline`, `--json`, `--policy`, a non-terminal stderr and protocol
//! commands (`lsp`, `mock` serving).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use std::fmt::Write as _;

use clap::Args;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::output::{CliError, CommandName, CommandResult, ErrorKind};
use crate::{Report, exit};

use super::update::{UpdateCheckResult, UpdateStatus, UpgradeKind, newer_than};

/// Cached answers older than this are ignored; run `update check` again.
pub const CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Default minimum time between two notices.
pub const DEFAULT_INTERVAL_HOURS: u64 = 24;
const MAX_INTERVAL_HOURS: u64 = 24 * 30;
/// Largest cache or settings file read.
const MAX_FILE_BYTES: u64 = 16 * 1024;
const CACHE_FORMAT: u32 = 1;
const CACHE_FILE: &str = "update-check.json";
const SETTINGS_FILE: &str = "settings.json";

/// Which releases a notice may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Stable,
    Prerelease,
}

/// Resolved notice settings; `channel: None` means notices are off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub channel: Option<Channel>,
    pub interval: Duration,
}

/// Process facts the notice reads, gathered once so the logic is pure.
#[derive(Debug, Clone, Default)]
pub struct NoticeEnv {
    /// Environment variable lookup results by name.
    pub vars: Vec<(String, String)>,
    pub windows: bool,
    pub macos: bool,
}

impl NoticeEnv {
    pub fn from_process() -> Self {
        let names = [
            "TUNGSTEN_NO_UPDATE_CHECK",
            "TUNGSTEN_UPDATE_NOTICE",
            "TUNGSTEN_UPDATE_NOTICE_INTERVAL_HOURS",
            "TUNGSTEN_CACHE_DIR",
            "TUNGSTEN_CONFIG_DIR",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "HOME",
            "USERPROFILE",
            "LOCALAPPDATA",
            "APPDATA",
            "CI",
            "CONTINUOUS_INTEGRATION",
            "BUILD_NUMBER",
            "GITHUB_ACTIONS",
            "GITLAB_CI",
            "BUILDKITE",
            "CIRCLECI",
            "TF_BUILD",
            "JENKINS_URL",
            "TEAMCITY_VERSION",
        ];
        Self {
            vars: names
                .iter()
                .filter_map(|name| {
                    let value = std::env::var(name).ok()?;
                    Some(((*name).to_string(), value))
                })
                .collect(),
            windows: cfg!(windows),
            macos: cfg!(target_os = "macos"),
        }
    }

    fn var(&self, name: &str) -> Option<&str> {
        self.vars
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    }

    fn truthy(&self, name: &str) -> bool {
        self.var(name).is_some_and(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
    }

    /// Conservative: any common CI marker suppresses notices.
    pub fn is_ci(&self) -> bool {
        [
            "CI",
            "CONTINUOUS_INTEGRATION",
            "GITHUB_ACTIONS",
            "GITLAB_CI",
            "BUILDKITE",
            "CIRCLECI",
            "TF_BUILD",
        ]
        .iter()
        .any(|name| self.truthy(name))
            || ["BUILD_NUMBER", "JENKINS_URL", "TEAMCITY_VERSION"]
                .iter()
                .any(|name| self.var(name).is_some())
    }

    fn home(&self) -> Option<PathBuf> {
        self.var("HOME")
            .or_else(|| self.var("USERPROFILE"))
            .map(PathBuf::from)
    }

    /// `TUNGSTEN_CACHE_DIR`, else the platform's per-user cache directory.
    pub fn cache_dir(&self) -> Option<PathBuf> {
        if let Some(dir) = self.var("TUNGSTEN_CACHE_DIR") {
            return Some(PathBuf::from(dir));
        }
        let base = if self.windows {
            self.var("LOCALAPPDATA").map(PathBuf::from)
        } else if let Some(xdg) = self.var("XDG_CACHE_HOME") {
            Some(PathBuf::from(xdg))
        } else if self.macos {
            self.home().map(|h| h.join("Library").join("Caches"))
        } else {
            self.home().map(|h| h.join(".cache"))
        }?;
        Some(if self.windows {
            base.join("tungsten").join("cache")
        } else {
            base.join("tungsten")
        })
    }

    /// `TUNGSTEN_CONFIG_DIR`, else the platform's per-user config directory.
    pub fn config_dir(&self) -> Option<PathBuf> {
        if let Some(dir) = self.var("TUNGSTEN_CONFIG_DIR") {
            return Some(PathBuf::from(dir));
        }
        let base = if self.windows {
            self.var("APPDATA").map(PathBuf::from)
        } else if let Some(xdg) = self.var("XDG_CONFIG_HOME") {
            Some(PathBuf::from(xdg))
        } else if self.macos {
            self.home()
                .map(|h| h.join("Library").join("Application Support"))
        } else {
            self.home().map(|h| h.join(".config"))
        }?;
        Some(base.join("tungsten"))
    }
}

#[derive(Debug, Default, Deserialize)]
struct SettingsFile {
    #[serde(default)]
    update_notice: Option<SettingsEntry>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsEntry {
    /// `off`, `stable` or `prerelease`.
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    interval_hours: Option<u64>,
}

fn parse_channel(text: &str) -> Result<Option<Channel>, ()> {
    match text.trim().to_ascii_lowercase().as_str() {
        "off" | "0" | "false" | "no" | "never" => Ok(None),
        "stable" | "1" | "true" | "yes" | "on" => Ok(Some(Channel::Stable)),
        "prerelease" => Ok(Some(Channel::Prerelease)),
        _ => Err(()),
    }
}

fn clamp_interval(hours: u64) -> Duration {
    Duration::from_secs(hours.clamp(1, MAX_INTERVAL_HOURS) * 60 * 60)
}

/// Environment over settings file over the default (off). An invalid value
/// turns notices off rather than guessing. `TUNGSTEN_NO_UPDATE_CHECK` wins.
pub fn settings(env: &NoticeEnv, file: Option<&[u8]>) -> Settings {
    let off = Settings {
        channel: None,
        interval: clamp_interval(DEFAULT_INTERVAL_HOURS),
    };
    if env.truthy("TUNGSTEN_NO_UPDATE_CHECK") {
        return off;
    }
    let entry = match file {
        None => SettingsEntry::default(),
        Some(bytes) => match serde_json::from_slice::<SettingsFile>(bytes) {
            Ok(file) => file.update_notice.unwrap_or_default(),
            Err(_) => return off,
        },
    };
    let channel = match env.var("TUNGSTEN_UPDATE_NOTICE") {
        Some(value) => parse_channel(value),
        None => entry.channel.as_deref().map_or(Ok(None), parse_channel),
    };
    let Ok(channel) = channel else {
        return off;
    };
    let hours = match env.var("TUNGSTEN_UPDATE_NOTICE_INTERVAL_HOURS") {
        Some(value) => match value.trim().parse::<u64>() {
            Ok(hours) => hours,
            Err(_) => return off,
        },
        None => entry.interval_hours.unwrap_or(DEFAULT_INTERVAL_HOURS),
    };
    Settings {
        channel,
        interval: clamp_interval(hours),
    }
}

fn read_bounded(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX_FILE_BYTES).then_some(bytes)
}

/// Settings for this process; a missing file is the default, an unreadable or
/// oversized one turns notices off.
pub fn load_settings(env: &NoticeEnv) -> Settings {
    let path = env.config_dir().map(|d| d.join(SETTINGS_FILE));
    let file = match path {
        Some(path) if path.exists() => match read_bounded(&path) {
            Some(bytes) => Some(bytes),
            None => return settings(env, Some(b"not json")),
        },
        _ => None,
    };
    settings(env, file.as_deref())
}

/// The cached outcome of a verified `update check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cache {
    pub format: u32,
    /// Seconds since the Unix epoch.
    pub checked_at: u64,
    pub installed: String,
    pub channel: Channel,
    pub latest: String,
    pub status: CachedStatus,
    /// Expected upgrade effect; absent in entries written before it existed.
    #[serde(default)]
    pub upgrade: Option<UpgradeKind>,
    /// When a notice was last shown, seconds since the Unix epoch.
    #[serde(default)]
    pub noticed_at: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CachedStatus {
    UpToDate,
    Ahead,
    UpdateAvailable,
    ArtifactsUnavailable,
}

fn now_secs(now: SystemTime) -> Option<u64> {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// The cache entry a check result produces; `None` when the check reached no
/// verified answer (an inconclusive check leaves the previous entry alone).
pub fn cache_entry(result: &UpdateCheckResult, now: SystemTime) -> Option<Cache> {
    let status = match result.status {
        UpdateStatus::UpToDate => CachedStatus::UpToDate,
        UpdateStatus::Ahead => CachedStatus::Ahead,
        UpdateStatus::UpdateAvailable => CachedStatus::UpdateAvailable,
        UpdateStatus::ArtifactsUnavailable => CachedStatus::ArtifactsUnavailable,
        UpdateStatus::UnableToCheck => return None,
    };
    Some(Cache {
        format: CACHE_FORMAT,
        checked_at: now_secs(now)?,
        installed: result.current_version.clone(),
        channel: if result.include_prerelease {
            Channel::Prerelease
        } else {
            Channel::Stable
        },
        latest: result.latest_version.clone()?,
        status,
        upgrade: result.upgrade,
        noticed_at: None,
    })
}

/// The notice line to print, if any. Pure: decides from the cache, settings,
/// running version and clock only.
pub fn notice(
    cache: &Cache,
    settings: &Settings,
    current: &str,
    now: SystemTime,
) -> Option<String> {
    let channel = settings.channel?;
    let now = now_secs(now)?;
    if cache.format != CACHE_FORMAT
        || cache.channel != channel
        || cache.status != CachedStatus::UpdateAvailable
    {
        return None;
    }
    // A clock that moved backwards or a future timestamp is not trusted.
    let age = now.checked_sub(cache.checked_at)?;
    if age > CACHE_MAX_AGE.as_secs() {
        return None;
    }
    if let Some(noticed) = cache.noticed_at {
        let since = now.checked_sub(noticed)?;
        if since < settings.interval.as_secs() {
            return None;
        }
    }
    // The cached version is re-validated; an upgrade since the check (or a
    // tampered entry) suppresses the notice.
    if !newer_than(&cache.latest, current)? {
        return None;
    }
    let migration = if cache.upgrade == Some(UpgradeKind::Compatible) {
        ""
    } else {
        "; it may require project migration"
    };
    Some(format!(
        "note: tungsten {} is available (installed {current}{migration}). Run `tungsten update check` for verified upgrade guidance; set TUNGSTEN_UPDATE_NOTICE=off to silence.\n",
        cache.latest
    ))
}

fn write_atomic(dir: &Path, cache: &Cache) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(cache).map_err(std::io::Error::other)?;
    write_file_atomic(dir, CACHE_FILE, &bytes)
}

fn write_file_atomic(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, dir.join(name))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn read_cache(dir: &Path) -> Option<Cache> {
    serde_json::from_slice(&read_bounded(&dir.join(CACHE_FILE))?).ok()
}

/// Record a check result for later notices when notices are enabled. Returns
/// the cache path on success; errors are swallowed.
pub fn record(result: &UpdateCheckResult, env: &NoticeEnv, now: SystemTime) -> Option<PathBuf> {
    load_settings(env).channel?;
    let entry = cache_entry(result, now)?;
    let dir = env.cache_dir()?;
    write_atomic(&dir, &entry).ok()?;
    Some(dir.join(CACHE_FILE))
}

/// After an interactive command: print at most one notice line to `stderr`.
/// No network; at most two bounded file reads and one small write.
pub fn after_command(stderr: &mut dyn Write, env: &NoticeEnv, current: &str, now: SystemTime) {
    if env.is_ci() {
        return;
    }
    let settings = load_settings(env);
    if settings.channel.is_none() {
        return;
    }
    let Some(dir) = env.cache_dir() else {
        return;
    };
    let Some(mut cache) = read_cache(&dir) else {
        return;
    };
    let Some(line) = notice(&cache, &settings, current, now) else {
        return;
    };
    // Show the notice only if its time can be recorded, so an unwritable
    // cache never turns into a notice on every command.
    cache.noticed_at = now_secs(now);
    if write_atomic(&dir, &cache).is_ok() {
        let _ = stderr.write_all(line.as_bytes());
    }
}

// ── `tungsten update notice` ───────────────────────────────────────────────

#[derive(Debug, Args)]
pub(crate) struct UpdateNoticeArgs {
    /// Turn notices on for this channel (default stable) in the user
    /// settings file.
    #[arg(
        long,
        value_name = "CHANNEL",
        num_args = 0..=1,
        default_missing_value = "stable",
        value_parser = ["stable", "prerelease"],
        conflicts_with = "disable"
    )]
    pub enable: Option<String>,
    /// Turn notices off in the user settings file.
    #[arg(long)]
    pub disable: bool,
    /// Minimum hours between two notices (1 to 720), saved in the user
    /// settings file.
    #[arg(long, value_name = "HOURS", value_parser = clap::value_parser!(u64).range(1..=MAX_INTERVAL_HOURS))]
    pub interval_hours: Option<u64>,
}

/// Where the effective channel came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettingSource {
    /// Nothing configured: notices are off.
    Default,
    /// `TUNGSTEN_UPDATE_NOTICE` or `TUNGSTEN_UPDATE_NOTICE_INTERVAL_HOURS`.
    Environment,
    SettingsFile,
    /// `TUNGSTEN_NO_UPDATE_CHECK` turns notices off whatever else is set.
    OptedOut,
    /// A setting is invalid, so notices are off.
    Invalid,
}

/// The cached answer of the last verified `update check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct CachedAnswer {
    pub latest_version: String,
    pub status: CachedStatus,
    pub channel: Channel,
    pub upgrade: Option<UpgradeKind>,
    /// Seconds since the Unix epoch.
    pub checked_at: u64,
    /// Older than seven days, so no notice uses it.
    pub expired: bool,
}

/// `tungsten update notice`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct UpdateNoticeResult {
    pub notices_enabled: bool,
    /// Null when notices are off.
    pub channel: Option<Channel>,
    pub interval_hours: u64,
    pub source: SettingSource,
    /// The user settings file; null when no per-user directory is known.
    pub settings_path: Option<String>,
    /// Whether this invocation wrote the settings file.
    pub settings_written: bool,
    /// The cache `update check` refreshes; null when no per-user directory
    /// is known.
    pub cache_path: Option<String>,
    pub cached: Option<CachedAnswer>,
    /// Why notices may still not appear here, and overrides to know about.
    pub remarks: Vec<String>,
}

fn source(env: &NoticeEnv, file: Option<&[u8]>, settings: &Settings) -> SettingSource {
    if env.truthy("TUNGSTEN_NO_UPDATE_CHECK") {
        return SettingSource::OptedOut;
    }
    let entry = match file.map(serde_json::from_slice::<SettingsFile>) {
        Some(Err(_)) => return SettingSource::Invalid,
        Some(Ok(file)) => file.update_notice.unwrap_or_default(),
        None => SettingsEntry::default(),
    };
    let env_channel = env.var("TUNGSTEN_UPDATE_NOTICE");
    let env_interval = env.var("TUNGSTEN_UPDATE_NOTICE_INTERVAL_HOURS");
    if env_channel.is_some_and(|v| parse_channel(v).is_err())
        || env_interval.is_some_and(|v| v.trim().parse::<u64>().is_err())
        || (env_channel.is_none()
            && entry
                .channel
                .as_deref()
                .is_some_and(|v| parse_channel(v).is_err()))
    {
        return SettingSource::Invalid;
    }
    if env_channel.is_some() || (env_interval.is_some() && settings.channel.is_some()) {
        SettingSource::Environment
    } else if entry.channel.is_some() {
        SettingSource::SettingsFile
    } else {
        SettingSource::Default
    }
}

fn io_error(message: String, help: &str) -> CliError {
    CliError::new(ErrorKind::Io, message).with_help(help)
}

/// Merge the change into the settings file, keeping every other key. A file
/// that cannot be read or is not a JSON object is never overwritten.
fn write_settings(path: &Path, args: &UpdateNoticeArgs) -> Result<(), CliError> {
    let help = "fix or remove the settings file, then run the command again";
    let mut root = if path.exists() {
        let bytes = read_bounded(path).ok_or_else(|| {
            io_error(
                format!(
                    "cannot read {} (missing permission, not a file, or larger than 16 KiB)",
                    path.display()
                ),
                help,
            )
        })?;
        match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(serde_json::Value::Object(map)) => map,
            _ => {
                return Err(io_error(
                    format!("{} is not a JSON object", path.display()),
                    help,
                ));
            }
        }
    } else {
        serde_json::Map::new()
    };
    let entry = root
        .entry("update_notice")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(entry) = entry.as_object_mut() else {
        return Err(io_error(
            format!("`update_notice` in {} is not an object", path.display()),
            help,
        ));
    };
    if let Some(channel) = &args.enable {
        entry.insert("channel".into(), channel.clone().into());
    }
    if args.disable {
        entry.insert("channel".into(), "off".into());
    }
    if let Some(hours) = args.interval_hours {
        entry.insert("interval_hours".into(), hours.into());
    }
    let mut bytes = serde_json::to_vec_pretty(&root)
        .map_err(|e| CliError::new(ErrorKind::Internal, format!("cannot encode settings: {e}")))?;
    bytes.push(b'\n');
    if serde_json::from_slice::<SettingsFile>(&bytes).is_err() {
        return Err(io_error(
            format!("`update_notice` in {} has unknown keys", path.display()),
            help,
        ));
    }
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return Err(io_error(
            format!("invalid settings path {}", path.display()),
            help,
        ));
    };
    write_file_atomic(dir, name, &bytes)
        .map_err(|e| io_error(format!("cannot write {}: {e}", path.display()), help))
}

pub(crate) fn run(args: &UpdateNoticeArgs) -> Report {
    let report = Report::new(CommandName::UpdateNotice);
    let env = NoticeEnv::from_process();
    let settings_path = env.config_dir().map(|d| d.join(SETTINGS_FILE));
    let writes = args.enable.is_some() || args.disable || args.interval_hours.is_some();
    if writes {
        let Some(path) = &settings_path else {
            return report.failed(
                exit::INTERNAL,
                io_error(
                    "no per-user configuration directory is known".into(),
                    "set TUNGSTEN_CONFIG_DIR, or use TUNGSTEN_UPDATE_NOTICE instead",
                ),
            );
        };
        if let Err(error) = write_settings(path, args) {
            return report.failed(exit::INTERNAL, error);
        }
    }
    let file = settings_path
        .as_ref()
        .filter(|p| p.exists())
        .map(|p| read_bounded(p).unwrap_or_else(|| b"not json".to_vec()));
    let settings = settings(&env, file.as_deref());
    let cache_dir = env.cache_dir();
    let now = now_secs(SystemTime::now()).unwrap_or(0);
    let cached = cache_dir
        .as_deref()
        .and_then(read_cache)
        .map(|c| CachedAnswer {
            expired: now.saturating_sub(c.checked_at) > CACHE_MAX_AGE.as_secs()
                || c.checked_at > now,
            latest_version: c.latest,
            status: c.status,
            channel: c.channel,
            upgrade: c.upgrade,
            checked_at: c.checked_at,
        });
    let mut remarks = vec![];
    if writes && env.var("TUNGSTEN_UPDATE_NOTICE").is_some() {
        remarks.push("TUNGSTEN_UPDATE_NOTICE is set and overrides the settings file".into());
    }
    if env.is_ci() {
        remarks.push("a CI environment is detected, so notices are not shown here".into());
    }
    if settings.channel.is_some() {
        match &cached {
            None => remarks.push("no cached answer yet; run `tungsten update check` to refresh it".into()),
            Some(c) if c.expired => remarks.push("the cached answer is older than 7 days; run `tungsten update check` to refresh it".into()),
            Some(c) if Some(c.channel) != settings.channel => remarks.push("the cached answer is for the other channel; run `tungsten update check` (with --include-prerelease for prerelease) to refresh it".into()),
            Some(_) => {}
        }
    }
    let result = UpdateNoticeResult {
        notices_enabled: settings.channel.is_some(),
        channel: settings.channel,
        interval_hours: settings.interval.as_secs() / 3600,
        source: source(&env, file.as_deref(), &settings),
        settings_path: settings_path.map(|p| p.display().to_string()),
        settings_written: writes,
        cache_path: cache_dir.map(|d| d.join(CACHE_FILE).display().to_string()),
        cached,
        remarks,
    };
    let mut report = report;
    report.human = render(&result);
    report.result = Some(CommandResult::UpdateNotice(result));
    report
}

fn render(result: &UpdateNoticeResult) -> String {
    let mut out = String::new();
    if result.settings_written {
        let _ = writeln!(
            out,
            "Saved {}",
            result.settings_path.as_deref().unwrap_or("settings")
        );
    }
    let source = match result.source {
        SettingSource::Default => "default",
        SettingSource::Environment => "environment",
        SettingSource::SettingsFile => "settings file",
        SettingSource::OptedOut => "TUNGSTEN_NO_UPDATE_CHECK",
        SettingSource::Invalid => "an invalid setting",
    };
    match result.channel {
        Some(channel) => {
            let _ = writeln!(
                out,
                "Update notices: on ({} releases, at most every {} h; from {source})",
                match channel {
                    Channel::Stable => "stable",
                    Channel::Prerelease => "prerelease",
                },
                result.interval_hours
            );
        }
        None => {
            let _ = writeln!(out, "Update notices: off (from {source})");
        }
    }
    if let Some(path) = &result.settings_path {
        let _ = writeln!(out, "Settings file: {path}");
    }
    if let Some(path) = &result.cache_path {
        let _ = writeln!(out, "Cache: {path}");
    }
    if let Some(c) = &result.cached {
        let _ = writeln!(
            out,
            "Last verified check: latest {} ({:?}){}",
            c.latest_version,
            c.status,
            if c.expired { ", expired" } else { "" }
        );
    }
    for remark in &result.remarks {
        let _ = writeln!(out, "Note: {remark}");
    }
    out.push_str(
        "Notices never make a network request; `tungsten update check` refreshes the cache.\n",
    );
    out
}
