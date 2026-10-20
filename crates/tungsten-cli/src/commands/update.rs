// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten update check`: explicit, bounded release discovery.
//!
//! One GET of the public GitHub releases listing for pboachie/tungsten, with a
//! strict timeout and response cap. Nothing about the project, its
//! specification, credentials or usage is sent. The command never installs,
//! rewrites project pins or generated clients, or runs remote content; remote
//! text is untrusted display data and is sanitized before it is shown.
//!
//! The network sits behind [`Fetcher`] and release parsing behind
//! [`UpdateSource`], so [`check`] is pure and tests never touch the network.

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Subcommand};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::output::{CommandName, CommandResult};
use crate::{Report, exit};

/// Public release metadata. `per_page` bounds the listing independently of
/// the byte cap.
pub const RELEASES_URL: &str =
    "https://api.github.com/repos/pboachie/tungsten/releases?per_page=20";
/// Largest response body accepted.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Wall-clock bound for the whole request, connection and body included.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REPOSITORY_URL: &str = "https://github.com/pboachie/tungsten";
const NOTES_MAX_CHARS: usize = 1200;
const NOTES_MAX_LINES: usize = 12;

pub const EXIT_UPDATE_AVAILABLE: i32 = exit::UPDATE_AVAILABLE;
pub const EXIT_UNCONFIRMED: i32 = exit::UPDATE_UNCONFIRMED;

#[derive(Debug, Subcommand)]
pub(crate) enum UpdateCommand {
    /// Ask GitHub once whether a newer release exists; never installs.
    ///
    /// Prints installation-specific upgrade guidance.
    /// Exit codes: 0 up to date, 5 newer release available, 6 could not
    /// confirm (offline, opted out, network or metadata failure, or the
    /// release lacks this platform's archive).
    Check(UpdateCheckArgs),
}

#[derive(Debug, Args)]
pub(crate) struct UpdateCheckArgs {
    /// Also consider prereleases; by default only stable releases count.
    #[arg(long)]
    pub include_prerelease: bool,
    /// Opt out: make no update request (exit 6). Also set by
    /// TUNGSTEN_NO_UPDATE_CHECK=1.
    #[arg(
        long,
        env = "TUNGSTEN_NO_UPDATE_CHECK",
        value_parser = clap::builder::BoolishValueParser::new()
    )]
    pub no_update_check: bool,
}

pub(crate) fn run(command: &UpdateCommand, offline: bool) -> Report {
    let UpdateCommand::Check(args) = command;
    let ctx = Context {
        current: tungsten_build::TUNGSTEN_VERSION.to_string(),
        include_prerelease: args.include_prerelease,
        offline,
        opted_out: args.no_update_check,
        platform: Platform::current(),
        install: InstallEnv::from_process(),
    };
    let fetcher = HttpFetcher;
    let result = check(&GithubReleases { fetcher: &fetcher }, &ctx);
    finish(result)
}

fn finish(result: UpdateCheckResult) -> Report {
    let mut report = Report::new(CommandName::UpdateCheck);
    report.exit = exit_code(&result);
    report.human = render(&result);
    report.result = Some(CommandResult::UpdateCheck(result));
    report
}

// ── result model ───────────────────────────────────────────────────────────

/// `tungsten update check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct UpdateCheckResult {
    pub status: UpdateStatus,
    /// Why the check was inconclusive; null unless `status` is
    /// `unable_to_check` or `artifacts_unavailable`.
    pub reason: Option<UnavailableReason>,
    /// The running tungsten version.
    pub current_version: String,
    /// Newest release considered (stable unless `include_prerelease`); null
    /// when none was determined.
    pub latest_version: Option<String>,
    pub include_prerelease: bool,
    /// Canonical release page for `latest_version`, built from the validated
    /// tag, never copied from remote metadata.
    pub release_url: Option<String>,
    /// Sanitized, truncated release notes. Untrusted remote text.
    pub notes: Option<String>,
    /// Archive the installers fetch for this platform and release; null when
    /// this platform has no release archive.
    pub expected_archive: Option<String>,
    /// Whether the release lists that archive and `SHA256SUMS`; null when
    /// unknown (platform unsupported or no release determined).
    pub artifacts_available: Option<bool>,
    pub install: InstallReport,
    /// Upgrade guidance. Commands appear only for a detected installation
    /// route; they are printed, never run.
    pub advice: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStatus {
    /// Running the newest release (exit 0).
    UpToDate,
    /// Running a version newer than the newest release considered (exit 0).
    Ahead,
    /// A newer release with this platform's archive exists (exit 5).
    UpdateAvailable,
    /// A newer release exists but lacks this platform's archive or checksums,
    /// so it is not recommended yet (exit 6).
    ArtifactsUnavailable,
    /// No usable answer; see `reason` (exit 6).
    UnableToCheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    /// TUNGSTEN_NO_UPDATE_CHECK or --no-update-check; no request was made.
    OptedOut,
    /// `--offline`; no request was made.
    Offline,
    Timeout,
    Network,
    RateLimited,
    HttpError,
    /// The response was not a usable release listing.
    Malformed,
    /// The response exceeded the size cap.
    Oversized,
    /// The listing holds no release matching the stable/prerelease choice.
    NoMatchingRelease,
    /// The newest release lacks this platform's archive or `SHA256SUMS`.
    ArtifactsMissing,
}

impl UnavailableReason {
    fn text(self) -> &'static str {
        match self {
            Self::OptedOut => {
                "update checks are disabled (TUNGSTEN_NO_UPDATE_CHECK or --no-update-check); no request was made"
            }
            Self::Offline => "--offline was given; no request was made",
            Self::Timeout => "the request timed out",
            Self::Network => "the release metadata could not be fetched (DNS, connection or TLS)",
            Self::RateLimited => "GitHub rate-limited the request",
            Self::HttpError => "GitHub returned an unexpected HTTP status",
            Self::Malformed => "the release metadata was not valid",
            Self::Oversized => "the release metadata exceeded the size limit",
            Self::NoMatchingRelease => "no matching release was found in the newest releases",
            Self::ArtifactsMissing => {
                "the newest release does not list this platform's archive and SHA256SUMS yet"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct InstallReport {
    pub route: InstallRoute,
    /// What the route was inferred from, or why it is unknown.
    pub evidence: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstallRoute {
    /// The binary is in the cargo bin directory.
    Cargo,
    /// The binary lives under a `node_modules` directory.
    Npm,
    /// The binary is in the release installer's target directory. The
    /// installer leaves no marker, so this is inferred from the location.
    Installer,
    /// Not determined; no command is suggested.
    Unknown,
}

// ── release lookup ─────────────────────────────────────────────────────────

/// Why a fetch produced no body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchError {
    Timeout,
    Network,
    RateLimited,
    Status(u16),
    TooLarge,
}

/// The only network seam: one bounded GET.
pub trait Fetcher {
    fn get(&self, url: &str, max_bytes: usize, timeout: Duration) -> Result<Vec<u8>, FetchError>;
}

struct HttpFetcher;

impl Fetcher for HttpFetcher {
    fn get(&self, url: &str, max_bytes: usize, timeout: Duration) -> Result<Vec<u8>, FetchError> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .timeout_connect(Some(CONNECT_TIMEOUT.min(timeout)))
            .max_redirects(0)
            .http_status_as_error(false)
            .max_response_header_size(16 * 1024)
            .build()
            .new_agent();
        let mut response = agent
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header(
                "User-Agent",
                format!("tungsten/{}", tungsten_build::TUNGSTEN_VERSION),
            )
            .call()
            .map_err(|error| match error {
                ureq::Error::Timeout(_) => FetchError::Timeout,
                _ => FetchError::Network,
            })?;
        let status = response.status().as_u16();
        if status == 429
            || (status == 403
                && response
                    .headers()
                    .get("x-ratelimit-remaining")
                    .is_some_and(|v| v.as_bytes() == b"0"))
        {
            return Err(FetchError::RateLimited);
        }
        if status != 200 {
            return Err(FetchError::Status(status));
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::TimedOut {
                    FetchError::Timeout
                } else {
                    FetchError::Network
                }
            })?;
        if bytes.len() > max_bytes {
            return Err(FetchError::TooLarge);
        }
        Ok(bytes)
    }
}

/// A usable release: a non-draft entry whose tag is a strict semantic version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    version: Version,
    tag: String,
    prerelease: bool,
    notes: String,
    /// Names of uploaded assets.
    assets: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceError {
    Fetch(FetchError),
    Malformed,
    Oversized,
}

/// Where releases come from. Implementations must be bounded and side-effect
/// free apart from their single read.
pub trait UpdateSource {
    fn releases(&self) -> Result<Vec<Release>, SourceError>;
}

pub struct GithubReleases<'a> {
    pub fetcher: &'a dyn Fetcher,
}

impl UpdateSource for GithubReleases<'_> {
    fn releases(&self) -> Result<Vec<Release>, SourceError> {
        let body = self
            .fetcher
            .get(RELEASES_URL, MAX_RESPONSE_BYTES, REQUEST_TIMEOUT)
            .map_err(|error| match error {
                FetchError::TooLarge => SourceError::Oversized,
                other => SourceError::Fetch(other),
            })?;
        // Enforced again so a fetcher cannot widen the bound.
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(SourceError::Oversized);
        }
        parse_releases(&body)
    }
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    #[serde(default)]
    state: Option<String>,
}

fn parse_releases(body: &[u8]) -> Result<Vec<Release>, SourceError> {
    let entries: Vec<serde_json::Value> =
        serde_json::from_slice(body).map_err(|_| SourceError::Malformed)?;
    let mut releases = vec![];
    let mut recognized = 0usize;
    for entry in entries.iter() {
        // One bad entry never hides the others.
        let Ok(raw) = serde_json::from_value::<RawRelease>(entry.clone()) else {
            continue;
        };
        recognized += 1;
        let Some(version) = raw.tag_name.strip_prefix('v').and_then(Version::parse) else {
            continue;
        };
        if raw.draft {
            continue;
        }
        releases.push(Release {
            tag: raw.tag_name,
            prerelease: raw.prerelease || !version.pre.is_empty(),
            version,
            notes: sanitize(raw.body.as_deref().unwrap_or(""), NOTES_MAX_CHARS),
            assets: raw
                .assets
                .into_iter()
                .filter(|a| a.state.as_deref() == Some("uploaded"))
                .map(|a| a.name)
                .collect(),
        });
    }
    if !entries.is_empty() && recognized == 0 {
        return Err(SourceError::Malformed);
    }
    Ok(releases)
}

/// Remote text becomes display data: control characters (escape sequences),
/// bidirectional and zero-width formatting characters are dropped, and the
/// length is bounded.
fn sanitize(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    for c in text.replace("\r\n", "\n").chars() {
        let c = match c {
            '\n' => '\n',
            '\t' => ' ',
            c if c.is_control() => continue,
            '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}' => continue,
            c => c,
        };
        if count == max_chars {
            out.push_str("...");
            break;
        }
        out.push(c);
        count += 1;
    }
    out.trim().to_string()
}

// ── versions ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum Ident {
    Numeric(u64),
    Alpha(String),
}

impl Ord for Ident {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Numeric(a), Self::Numeric(b)) => a.cmp(b),
            (Self::Numeric(_), Self::Alpha(_)) => Ordering::Less,
            (Self::Alpha(_), Self::Numeric(_)) => Ordering::Greater,
            (Self::Alpha(a), Self::Alpha(b)) => a.cmp(b),
        }
    }
}
impl PartialOrd for Ident {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Semantic Versioning 2.0.0; build metadata is validated and ignored for
/// precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Version {
    core: [u64; 3],
    pre: Vec<Ident>,
    /// The validated text without a leading `v`.
    text: String,
}

impl Version {
    fn parse(text: &str) -> Option<Self> {
        if text.is_empty() || text.len() > 64 {
            return None;
        }
        let (rest, build) = match text.split_once('+') {
            Some((rest, build)) => (rest, Some(build)),
            None => (text, None),
        };
        if let Some(build) = build
            && !build.split('.').all(|id| {
                !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            return None;
        }
        let (core, pre) = match rest.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (rest, None),
        };
        let numeric = |s: &str| -> Option<u64> {
            let ok = !s.is_empty()
                && s.len() <= 18
                && s.bytes().all(|b| b.is_ascii_digit())
                && (s == "0" || !s.starts_with('0'));
            ok.then(|| s.parse().ok()).flatten()
        };
        let mut parts = core.split('.');
        let core = [
            numeric(parts.next()?)?,
            numeric(parts.next()?)?,
            numeric(parts.next()?)?,
        ];
        if parts.next().is_some() {
            return None;
        }
        let mut idents = vec![];
        if let Some(pre) = pre {
            for id in pre.split('.') {
                if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                    return None;
                }
                idents.push(if id.bytes().all(|b| b.is_ascii_digit()) {
                    Ident::Numeric(numeric(id)?)
                } else {
                    Ident::Alpha(id.to_string())
                });
            }
        }
        Some(Self {
            core,
            pre: idents,
            text: text.to_string(),
        })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core
            .cmp(&other.core)
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}
impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// ── platform and installation route ────────────────────────────────────────

/// Release archive naming used by `install.sh` and `install.ps1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    triple: String,
    extension: &'static str,
}

impl Platform {
    /// `None` for platforms the releases do not cover.
    pub fn for_target(arch: &str, os: &str, env: &str) -> Option<Self> {
        let arch = match arch {
            "x86_64" | "aarch64" => arch,
            _ => return None,
        };
        let (suffix, extension) = match (os, env) {
            ("linux", "musl") => ("unknown-linux-musl", "tar.gz"),
            ("linux", "gnu") => ("unknown-linux-gnu", "tar.gz"),
            ("macos", _) => ("apple-darwin", "tar.gz"),
            ("windows", "msvc") => ("pc-windows-msvc", "zip"),
            _ => return None,
        };
        Some(Self {
            triple: format!("{arch}-{suffix}"),
            extension,
        })
    }

    fn current() -> Option<Self> {
        Self::for_target(
            std::env::consts::ARCH,
            std::env::consts::OS,
            if cfg!(target_env = "musl") {
                "musl"
            } else if cfg!(target_env = "gnu") {
                "gnu"
            } else if cfg!(target_env = "msvc") {
                "msvc"
            } else {
                ""
            },
        )
    }

    fn archive(&self, version: &str) -> String {
        format!("tungsten-{version}-{}.{}", self.triple, self.extension)
    }
}

/// The facts route detection reads, gathered once so detection is pure.
#[derive(Debug, Clone, Default)]
pub struct InstallEnv {
    pub exe: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub cargo_home: Option<PathBuf>,
    /// `TUNGSTEN_INSTALL_DIR`, which the installers honor.
    pub installer_dir: Option<PathBuf>,
    pub local_app_data: Option<PathBuf>,
    pub windows: bool,
}

impl InstallEnv {
    fn from_process() -> Self {
        let var = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Self {
            exe: std::env::current_exe().ok(),
            home: var("HOME").or_else(|| var("USERPROFILE")),
            cargo_home: var("CARGO_HOME"),
            installer_dir: var("TUNGSTEN_INSTALL_DIR"),
            local_app_data: var("LOCALAPPDATA"),
            windows: cfg!(windows),
        }
    }
}

/// Conservative: a route is named only when exactly one signal matches.
pub fn detect_route(env: &InstallEnv) -> InstallReport {
    let unknown = |evidence: &str| InstallReport {
        route: InstallRoute::Unknown,
        evidence: evidence.to_string(),
    };
    let Some(exe) = env.exe.as_deref() else {
        return unknown("the path of the running executable is unavailable");
    };
    let Some(dir) = exe.parent() else {
        return unknown("the running executable has no parent directory");
    };
    let same = |other: Option<PathBuf>| other.is_some_and(|p| p.components().eq(dir.components()));
    let mut matches: Vec<(InstallRoute, &str)> = vec![];
    if exe
        .components()
        .any(|c| c.as_os_str() == std::ffi::OsStr::new("node_modules"))
    {
        matches.push((
            InstallRoute::Npm,
            "the binary is under a node_modules directory",
        ));
    }
    let cargo_bin = env
        .cargo_home
        .clone()
        .or_else(|| env.home.as_ref().map(|h| h.join(".cargo")))
        .map(|p| p.join("bin"));
    if same(cargo_bin) {
        matches.push((
            InstallRoute::Cargo,
            "the binary is in the cargo bin directory",
        ));
    }
    let installer_default = if env.windows {
        env.local_app_data
            .as_ref()
            .map(|p| p.join("Programs").join("tungsten").join("bin"))
    } else {
        env.home.as_ref().map(|h| h.join(".local").join("bin"))
    };
    if same(env.installer_dir.clone()) {
        matches.push((
            InstallRoute::Installer,
            "the binary is in TUNGSTEN_INSTALL_DIR; the installer leaves no marker, so this is inferred from the location",
        ));
    } else if same(installer_default) {
        matches.push((
            InstallRoute::Installer,
            "the binary is in the installer's default directory; the installer leaves no marker, so this is inferred from the location",
        ));
    }
    match matches.as_slice() {
        [(route, evidence)] => InstallReport {
            route: *route,
            evidence: (*evidence).to_string(),
        },
        [] => {
            let text = exe.to_string_lossy();
            if [
                "/nix/store/",
                "/Cellar/",
                "/opt/homebrew/",
                "/snap/",
                "/usr/",
            ]
            .iter()
            .any(|p| text.starts_with(p) || text.contains(p))
            {
                unknown("the binary is in a location usually owned by a system package manager")
            } else if text.contains("/target/release/") || text.contains("/target/debug/") {
                unknown("the binary looks like a source build tree output")
            } else {
                unknown("the binary location matches no known installation route")
            }
        }
        _ => unknown("the binary location matches more than one installation route"),
    }
}

fn advice(route: InstallRoute, tag: &str, windows: bool) -> Vec<String> {
    match route {
        InstallRoute::Installer => {
            let raw = format!("https://raw.githubusercontent.com/pboachie/tungsten/{tag}");
            vec![
                "Re-run the installer pinned to this release; it verifies SHA256SUMS before replacing the binary:".into(),
                if windows {
                    format!("  & ([scriptblock]::Create((irm {raw}/install.ps1))) -Version {tag}")
                } else {
                    format!("  curl -fsSL {raw}/install.sh | sh -s -- --version {tag}")
                },
                "If you placed the binary there another way, use that route instead.".into(),
            ]
        }
        InstallRoute::Cargo => vec![
            "Reinstall from the release tag with cargo:".into(),
            format!("  cargo install --locked --git {REPOSITORY_URL} --tag {tag} tungsten-cli"),
        ],
        InstallRoute::Npm => vec![
            "This binary came from a node_modules package; upgrade it with the package manager and package that provided it.".into(),
            "tungsten does not publish its compiler on npm, so verify that package's source first.".into(),
        ],
        InstallRoute::Unknown => vec![
            "tungsten could not tell how it was installed, so no replacement command is suggested.".into(),
            "Use the tool that installed it, or choose a route from the install section:".into(),
            format!("  {REPOSITORY_URL}#install"),
            "The release archives are verified against the release's SHA256SUMS.".into(),
        ],
    }
}

// ── the check ──────────────────────────────────────────────────────────────

/// Everything a check depends on besides the source.
#[derive(Debug, Clone)]
pub struct Context {
    pub current: String,
    pub include_prerelease: bool,
    pub offline: bool,
    pub opted_out: bool,
    pub platform: Option<Platform>,
    pub install: InstallEnv,
}

pub fn check(source: &dyn UpdateSource, ctx: &Context) -> UpdateCheckResult {
    let install = detect_route(&ctx.install);
    let mut result = UpdateCheckResult {
        status: UpdateStatus::UnableToCheck,
        reason: None,
        current_version: ctx.current.clone(),
        latest_version: None,
        include_prerelease: ctx.include_prerelease,
        release_url: None,
        notes: None,
        expected_archive: None,
        artifacts_available: None,
        install,
        advice: vec![],
    };
    let unable = |mut result: UpdateCheckResult, reason| {
        result.status = UpdateStatus::UnableToCheck;
        result.reason = Some(reason);
        result.advice = vec![format!("Check manually: {REPOSITORY_URL}/releases")];
        result
    };
    // Opt-out and offline return before the source is touched.
    if ctx.opted_out {
        return unable(result, UnavailableReason::OptedOut);
    }
    if ctx.offline {
        return unable(result, UnavailableReason::Offline);
    }
    let Some(current) = Version::parse(&ctx.current) else {
        return unable(result, UnavailableReason::Malformed);
    };
    let releases = match source.releases() {
        Ok(releases) => releases,
        Err(error) => {
            let reason = match error {
                SourceError::Fetch(FetchError::Timeout) => UnavailableReason::Timeout,
                SourceError::Fetch(FetchError::Network) => UnavailableReason::Network,
                SourceError::Fetch(FetchError::RateLimited) => UnavailableReason::RateLimited,
                SourceError::Fetch(FetchError::Status(_)) => UnavailableReason::HttpError,
                SourceError::Fetch(FetchError::TooLarge) | SourceError::Oversized => {
                    UnavailableReason::Oversized
                }
                SourceError::Malformed => UnavailableReason::Malformed,
            };
            return unable(result, reason);
        }
    };
    let Some(latest) = releases
        .iter()
        .filter(|r| ctx.include_prerelease || !r.prerelease)
        .max_by(|a, b| a.version.cmp(&b.version))
    else {
        return unable(result, UnavailableReason::NoMatchingRelease);
    };
    result.latest_version = Some(latest.version.text.clone());
    result.release_url = Some(format!("{REPOSITORY_URL}/releases/tag/{}", latest.tag));
    if !latest.notes.is_empty() {
        result.notes = Some(latest.notes.clone());
    }
    if let Some(platform) = &ctx.platform {
        let archive = platform.archive(&latest.version.text);
        let present = |name: &str| latest.assets.iter().any(|a| a == name);
        result.artifacts_available = Some(present(&archive) && present("SHA256SUMS"));
        result.expected_archive = Some(archive);
    }
    result.status = match latest.version.cmp(&current) {
        Ordering::Equal => UpdateStatus::UpToDate,
        Ordering::Less => UpdateStatus::Ahead,
        Ordering::Greater if result.artifacts_available != Some(true) => {
            result.reason = Some(UnavailableReason::ArtifactsMissing);
            UpdateStatus::ArtifactsUnavailable
        }
        Ordering::Greater => UpdateStatus::UpdateAvailable,
    };
    result.advice = match result.status {
        UpdateStatus::UpdateAvailable => advice(
            result.install.route,
            &latest.tag,
            ctx.install.windows,
        ),
        UpdateStatus::ArtifactsUnavailable if result.expected_archive.is_none() => vec![
            "Release archives are not declared for this platform; no replacement is recommended.".into(),
        ],
        UpdateStatus::ArtifactsUnavailable => vec![
            "The release is published without this platform's archive; wait for the release to complete rather than building a guess.".into(),
        ],
        _ => vec![],
    };
    result
}

pub fn exit_code(result: &UpdateCheckResult) -> i32 {
    match result.status {
        UpdateStatus::UpToDate | UpdateStatus::Ahead => exit::OK,
        UpdateStatus::UpdateAvailable => EXIT_UPDATE_AVAILABLE,
        UpdateStatus::ArtifactsUnavailable | UpdateStatus::UnableToCheck => EXIT_UNCONFIRMED,
    }
}

pub fn render(result: &UpdateCheckResult) -> String {
    let mut out = format!("tungsten {} (installed)\n", result.current_version);
    if let Some(latest) = &result.latest_version {
        let kind = if result.include_prerelease {
            "newest release"
        } else {
            "latest stable release"
        };
        let _ = writeln!(out, "{kind}: {latest}");
    }
    let summary = match (result.status, result.reason) {
        (UpdateStatus::UpToDate, _) => "Up to date.".to_string(),
        (UpdateStatus::Ahead, _) => {
            "This build is newer than the newest release considered.".to_string()
        }
        (UpdateStatus::UpdateAvailable, _) => "A newer release is available.".to_string(),
        (UpdateStatus::ArtifactsUnavailable, _) if result.expected_archive.is_none() => {
            "A newer release exists, but no release archive is declared for this platform; not recommended."
                .to_string()
        }
        (UpdateStatus::ArtifactsUnavailable, _) => {
            "A newer release exists, but it does not list this platform's archive and SHA256SUMS yet; not recommended."
                .to_string()
        }
        (UpdateStatus::UnableToCheck, reason) => format!(
            "Could not check for updates: {}.",
            reason.map_or("unknown", UnavailableReason::text)
        ),
    };
    let _ = writeln!(out, "{summary}");
    if let Some(url) = &result.release_url {
        let _ = writeln!(out, "Release page: {url}");
    }
    if matches!(
        result.status,
        UpdateStatus::UpdateAvailable | UpdateStatus::ArtifactsUnavailable
    ) {
        let _ = writeln!(
            out,
            "Installation: {:?} ({})",
            result.install.route, result.install.evidence
        );
    }
    for line in &result.advice {
        let _ = writeln!(out, "{line}");
    }
    if let Some(notes) = &result.notes
        && result.status != UpdateStatus::UnableToCheck
    {
        out.push_str("Release notes (excerpt of remote text, not verified):\n");
        for line in notes.lines().take(NOTES_MAX_LINES) {
            let _ = writeln!(out, "  {line}");
        }
        if notes.lines().count() > NOTES_MAX_LINES {
            out.push_str("  ...\n");
        }
    }
    out.push_str(
        "Nothing was changed: this check never modifies the installed binary, project pins or generated clients.\n",
    );
    out
}

/// Entry points for the private harness.
#[cfg(feature = "testing")]
pub mod testing {
    use super::*;

    /// The scripted fetch: url, byte cap and timeout in, body or failure out.
    pub type FetchFn<'a> = &'a dyn Fn(&str, usize, Duration) -> Result<Vec<u8>, FetchError>;

    /// A fetcher backed by a closure; the harness counts and scripts calls.
    pub struct ClosureFetcher<'a>(pub FetchFn<'a>);
    impl std::fmt::Debug for ClosureFetcher<'_> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("ClosureFetcher(..)")
        }
    }
    impl Fetcher for ClosureFetcher<'_> {
        fn get(&self, url: &str, max: usize, timeout: Duration) -> Result<Vec<u8>, FetchError> {
            (self.0)(url, max, timeout)
        }
    }

    pub fn context(
        current: &str,
        include_prerelease: bool,
        offline: bool,
        opted_out: bool,
        target: Option<(&str, &str, &str)>,
        install: InstallEnv,
    ) -> Context {
        Context {
            current: current.to_string(),
            include_prerelease,
            offline,
            opted_out,
            platform: target.and_then(|(arch, os, env)| Platform::for_target(arch, os, env)),
            install,
        }
    }

    /// Run the pure check and render it exactly as the command does:
    /// (exit code, result JSON, human text).
    pub fn run_check(fetcher: &dyn Fetcher, ctx: &Context) -> (i32, serde_json::Value, String) {
        let result = check(&GithubReleases { fetcher }, ctx);
        (
            exit_code(&result),
            serde_json::to_value(&result).expect("result serializes"),
            render(&result),
        )
    }

    pub fn route_of(env: &InstallEnv) -> serde_json::Value {
        serde_json::to_value(detect_route(env)).expect("report serializes")
    }

    pub fn compare(a: &str, b: &str) -> Option<Ordering> {
        Some(Version::parse(a)?.cmp(&Version::parse(b)?))
    }

    pub fn sanitize_text(text: &str, max: usize) -> String {
        sanitize(text, max)
    }

    pub use super::{
        Context, EXIT_UNCONFIRMED, EXIT_UPDATE_AVAILABLE, FetchError, Fetcher, InstallEnv,
        MAX_RESPONSE_BYTES, RELEASES_URL, REQUEST_TIMEOUT,
    };
}
