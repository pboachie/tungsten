// SPDX-License-Identifier: AGPL-3.0-only
//! `tungsten doctor`: installed tools and local project prerequisites.
//! Bare doctor is informational; project strict mode enforces required tools.
//! No package registry request, installation, or authentication is performed.

use std::fmt::Write as _;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use tungsten_config::capabilities::ToolRequirement;
use tungsten_emit::external::find_executable;

use crate::args::DoctorArgs;
use crate::output::{CliError, CommandName, CommandResult, DoctorResult, ErrorKind, ToolReport};
use crate::{CliEnv, Report, exit, input};

/// Tools in report order, with what tungsten uses them for.
const TOOLS: [(&str, &str); 14] = [
    ("cargo", "Rust SDK and CLI targets"),
    ("node", "TypeScript SDK and MCP server"),
    ("python3", "Python SDK"),
    ("deno", "sandboxed run_script in the MCP server (opt-in)"),
    ("uv", "Python SDK packaging"),
    ("prettier", "formatting TypeScript output (--format)"),
    ("ruff", "formatting Python output (--format)"),
    ("rustfmt", "formatting Rust output (--format)"),
    ("java", "Java SDK (JDK 17 or later)"),
    ("mvn", "Java SDK build (Maven)"),
    ("dotnet", "C# SDK (.NET 10 or later)"),
    ("ruby", "Ruby SDK (Ruby 3.3 or later)"),
    ("bundle", "Ruby SDK packaging (Bundler)"),
    (
        "go",
        "Go SDK from the external go target (Go 1.22 or later)",
    ),
];
/// How long a tool may take to print its version.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) fn run(args: &DoctorArgs, env: &CliEnv) -> Report {
    let mut report = Report::new(CommandName::Doctor);
    let project = args
        .path
        .as_deref()
        .or_else(|| args.strict.then_some(Path::new(".")));
    // Resolve and compose before any version subprocess. Invalid project or
    // profile content must never cause discovered tools to execute.
    let configured = if let Some(path) = project {
        let manifest = match input::resolve(path) {
            Ok(input::Input::Project(manifest)) => manifest,
            _ => {
                return report.failed(
                    exit::FAILED,
                    CliError::new(
                        ErrorKind::Usage,
                        "doctor requires a project directory or tungsten manifest",
                    ),
                );
            }
        };
        let parsed = tungsten_build::driver::load_project_config(
            &manifest,
            &tungsten_build::CompileOptions::default(),
        );
        let Some(config) = parsed.config else {
            report.diagnostics = parsed.diagnostics.0;
            report.exit = exit::FAILED;
            return report;
        };
        Some((manifest, config))
    } else {
        None
    };
    let search_path = env
        .search_path
        .clone()
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    let mut tools: Vec<ToolReport> = TOOLS
        .iter()
        .map(|&(name, purpose)| {
            let exe = find_executable(name, &search_path);
            ToolReport {
                name: name.to_string(),
                found: exe.is_some(),
                version: exe
                    .as_deref()
                    .and_then(|exe| tool_version(exe, version_arg(name))),
                purpose: purpose.to_string(),
            }
        })
        .collect();
    let mut failures = vec![];
    if let Some((manifest, config)) = configured {
        let has = |name: &str| {
            config.targets.get(name).is_some_and(|options| {
                tungsten_config::external_target(options)
                    .ok()
                    .flatten()
                    .is_none()
            })
        };
        let javascript = has("typescript") || has("mcp") || has("webmcp");
        if javascript {
            let exe = find_executable("npm", &search_path);
            tools.push(ToolReport {
                name: "npm".into(),
                found: exe.is_some(),
                version: exe
                    .as_deref()
                    .and_then(|exe| tool_version(exe, "--version")),
                purpose: "JavaScript package installation".into(),
            });
        }
        if has("rust") {
            let exe = find_executable("rustc", &search_path);
            tools.push(ToolReport {
                name: "rustc".into(),
                found: exe.is_some(),
                version: exe
                    .as_deref()
                    .and_then(|exe| tool_version(exe, "--version")),
                purpose: "Rust compiler".into(),
            });
        }
        // The Go target is an external emitter: it needs `tungsten-emit-go`
        // on PATH unless the manifest names an explicit command.
        let go = config
            .targets
            .get("go")
            .and_then(|options| tungsten_config::external_target(options).ok().flatten());
        let go_emitter = go
            .as_ref()
            .is_some_and(|external| external.command.is_none());
        if go_emitter {
            tools.push(ToolReport {
                name: "tungsten-emit-go".into(),
                found: find_executable("tungsten-emit-go", &search_path).is_some(),
                version: None,
                purpose: "Go SDK emitter".into(),
            });
        }
        let mut requirements = Vec::new();
        for (name, options) in &config.targets {
            if has(name)
                && let Some(target) = tungsten_config::capabilities::target(name)
            {
                for requirement in target.prerequisites {
                    requirements.push(requirement.for_options(options));
                }
            }
        }
        // The conventional external Go emitter retains its existing special
        // case; arbitrary external targets never inherit built-in requirements.
        for (name, required, minimum) in [
            ("go", go.is_some(), Some([1, 22])),
            ("tungsten-emit-go", go_emitter, None),
        ] {
            if required {
                requirements.push(ToolRequirement {
                    tool: name,
                    minimum,
                    maximum_exclusive: None,
                    minimum_option: None,
                    excluded_versions: &[],
                });
            }
        }
        let requirements = match aggregate_requirements(requirements) {
            Ok(requirements) => requirements,
            Err(message) => {
                report.exit = exit::FAILED;
                report.human = format!("{message}\n");
                report.error = Some(CliError::new(ErrorKind::Usage, message).with_help(
                    "choose configured targets and options with overlapping tool requirements",
                ));
                return report;
            }
        };
        for (name, prerequisite) in requirements {
            if !tools.iter().any(|tool| tool.name == name) {
                let exe = find_executable(name, &search_path);
                tools.push(ToolReport {
                    name: name.into(),
                    found: exe.is_some(),
                    version: exe
                        .as_deref()
                        .and_then(|exe| tool_version(exe, version_arg(name))),
                    purpose: "configured target prerequisite".into(),
                });
            }
            let tool = tools.iter_mut().find(|tool| tool.name == name).unwrap();
            let supported = tool.found
                && ((prerequisite.bounds.minimum.is_none()
                    && prerequisite.bounds.maximum_exclusive.is_none()
                    && prerequisite.excluded_versions.is_empty())
                    || {
                        tool.version
                            .as_deref()
                            .and_then(version_pair)
                            .is_some_and(|(major, minor)| prerequisite.accepts([major, minor]))
                    });
            let requirement = prerequisite
                .bounds
                .description_with_exclusions(&prerequisite.excluded_versions);
            tool.purpose = format!(
                "required {requirement} for configured targets; {}",
                if supported {
                    "ready"
                } else {
                    "unavailable or unsupported"
                }
            );
            if !supported {
                failures.push(format!("{name} {requirement}"));
            }
        }
        let base = manifest.parent().unwrap_or(Path::new("."));
        for target in ["typescript", "mcp", "webmcp"] {
            let Some(options) = config.targets.get(target) else {
                continue;
            };
            for (key, package) in [
                ("sdk_path", "SDK"),
                ("runtime_path", "runtime"),
                ("sdk_runtime_path", "SDK runtime"),
            ] {
                if let Some(relative) = options.get(key).and_then(serde_json::Value::as_str) {
                    let out = options
                        .get("out")
                        .and_then(serde_json::Value::as_str)
                        .map(|p| base.join(p))
                        .unwrap_or_else(|| base.join("generated").join(target));
                    let local = out.join(relative).join("package.json");
                    let found = local.is_file();
                    tools.push(ToolReport { name: format!("{target}.{key}"), found, version: None,
                        purpose: format!("local {package} package {} (informational; generate/install may be pending)", local.display()) });
                }
            }
        }
    }
    report.human = human(&tools);
    if project.is_some() {
        report.human = report.human.replace(
            "all tools are optional; a missing tool only disables what it is listed for\n",
            "project prerequisites checked locally; package registry access was not checked\n",
        );
        if !failures.is_empty() {
            let _ = writeln!(
                report.human,
                "missing or unsupported prerequisites: {}",
                failures.join(", ")
            );
        }
    }
    let openapi_inputs = tungsten_config::capabilities::OPENAPI_INPUTS.to_vec();
    report.human.push_str(&openapi_summary(&openapi_inputs));
    report.result = Some(CommandResult::Doctor(DoctorResult {
        tools,
        openapi_inputs,
    }));
    if args.strict && !failures.is_empty() {
        report.exit = exit::FAILED;
        report.error = Some(
            CliError::new(
                ErrorKind::Usage,
                format!("project prerequisites unavailable: {}", failures.join(", ")),
            )
            .with_help("install compatible tools, then run doctor --strict again"),
        );
    }
    report
}

/// One line per OpenAPI line; `tungsten capabilities` lists every diagnosed construct.
fn openapi_summary(inputs: &[tungsten_config::capabilities::OpenApiVersionCapability]) -> String {
    let mut out = String::from("OpenAPI input (see `tungsten capabilities`):\n");
    for input in inputs {
        let _ = match input.unsupported_features.len() {
            0 => writeln!(out, "  {}.x supported", input.line),
            n => writeln!(
                out,
                "  {}.x {:?}: constructs equivalent to 3.1 are read; {n} 3.2-only constructs get diagnostics TG0140-TG0156",
                input.line, input.support
            ),
        };
    }
    out
}

/// Intersect already option-resolved bounds, independent of target order.
pub(crate) struct CombinedRequirement {
    pub bounds: ToolRequirement,
    pub excluded_versions: Vec<[u32; 2]>,
}

impl CombinedRequirement {
    fn accepts(&self, version: [u32; 2]) -> bool {
        self.bounds.accepts(version) && !self.excluded_versions.contains(&version)
    }

    fn has_available_version(&self) -> bool {
        let mut first = self.bounds.minimum.unwrap_or([0, 0]);
        while self.excluded_versions.contains(&first) {
            if let Some(minor) = first[1].checked_add(1) {
                first[1] = minor;
            } else if let Some(major) = first[0].checked_add(1) {
                first = [major, 0];
            } else {
                return false;
            }
        }
        self.bounds
            .maximum_exclusive
            .is_none_or(|maximum| first < maximum)
    }
}

pub(crate) fn aggregate_requirements(
    requirements: impl IntoIterator<Item = ToolRequirement>,
) -> Result<std::collections::BTreeMap<&'static str, CombinedRequirement>, String> {
    let mut combined = std::collections::BTreeMap::<&str, CombinedRequirement>::new();
    for mut requirement in requirements {
        // The caller resolved each target's options before combining the bounds.
        requirement.minimum_option = None;
        let exclusions = requirement.excluded_versions;
        requirement.excluded_versions = &[];
        combined
            .entry(requirement.tool)
            .and_modify(|existing| {
                existing.bounds.minimum = existing
                    .bounds
                    .minimum
                    .into_iter()
                    .chain(requirement.minimum)
                    .max();
                existing.bounds.maximum_exclusive = existing
                    .bounds
                    .maximum_exclusive
                    .into_iter()
                    .chain(requirement.maximum_exclusive)
                    .min();
                existing.excluded_versions.extend(exclusions);
                existing.excluded_versions.sort_unstable();
                existing.excluded_versions.dedup();
            })
            .or_insert_with(|| {
                let mut excluded_versions = exclusions.to_vec();
                excluded_versions.sort_unstable();
                excluded_versions.dedup();
                CombinedRequirement {
                    bounds: requirement,
                    excluded_versions,
                }
            });
    }
    for (tool, requirement) in &combined {
        if !requirement.has_available_version() {
            return Err(format!(
                "configured targets have conflicting {tool} requirements: {} (no version satisfies all bounds)",
                requirement
                    .bounds
                    .description_with_exclusions(&requirement.excluded_versions)
            ));
        }
    }
    Ok(combined)
}

fn human(tools: &[ToolReport]) -> String {
    let mut out = format!("tungsten {} · doctor\n", tungsten_build::TUNGSTEN_VERSION);
    for t in tools {
        let status = match (&t.version, t.found) {
            (Some(v), _) => v.clone(),
            (None, true) => "found".to_string(),
            (None, false) => "missing".to_string(),
        };
        let _ = writeln!(out, "  {:<10} {:<14} {}", t.name, status, t.purpose);
    }
    out.push_str("all tools are optional; a missing tool only disables what it is listed for\n");
    out
}

/// The argument that makes a tool print its version (`go` has no flag).
fn version_arg(name: &str) -> &'static str {
    if name == "go" { "version" } else { "--version" }
}

/// Run `<exe> <arg>` with a timeout and parse its output.
fn tool_version(exe: &Path, arg: &str) -> Option<String> {
    let mut child = Command::new(exe)
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + VERSION_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let read = |rx: mpsc::Receiver<String>| rx.recv_timeout(Duration::from_secs(1)).ok();
    let out = read(stdout).unwrap_or_default();
    let err = read(stderr).unwrap_or_default();
    // Some tools (older Python) print their version on stderr.
    parse_version(&out).or_else(|| parse_version(&err))
}

/// Read a pipe to the end on a thread so a chatty child cannot block on a
/// full pipe while we wait for it.
fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
        });
    }
    rx
}

/// The first version-like token on the first non-empty line:
/// `cargo 1.97.0 (abc)` → `1.97.0`, `v20.1.0` → `20.1.0`.
pub(crate) fn parse_version(text: &str) -> Option<String> {
    let line = text.lines().find(|l| !l.trim().is_empty())?;
    line.split_whitespace().find_map(|token| {
        let token = token.strip_prefix('v').unwrap_or(token);
        // `go version go1.22.1 linux/amd64`
        let token = token
            .strip_prefix("go")
            .filter(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
            .unwrap_or(token);
        let token = token.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
        let version_like = token.starts_with(|c: char| c.is_ascii_digit())
            && token.contains('.')
            && token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
        version_like.then(|| token.to_string())
    })
}

fn version_pair(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}
