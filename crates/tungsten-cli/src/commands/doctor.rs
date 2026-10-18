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
    let mut report = Report::new(CommandName::Doctor);
    let project = args
        .path
        .as_deref()
        .or_else(|| args.strict.then_some(Path::new(".")));
    let mut failures = vec![];
    if let Some(path) = project {
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
        let (config, diagnostics) = tungsten_config::load(&manifest);
        if diagnostics.has_errors() || config.is_none() {
            report.diagnostics = diagnostics.0;
            report.exit = exit::FAILED;
            return report;
        }
        let config = config.unwrap();
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
        let mut requirements = std::collections::BTreeMap::new();
        for (name, options) in &config.targets {
            if has(name)
                && let Some(target) = tungsten_config::capabilities::target(name)
            {
                for requirement in target.prerequisites {
                    requirements.insert(requirement.tool, requirement.for_options(options));
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
                requirements.insert(
                    name,
                    tungsten_config::capabilities::ToolRequirement {
                        tool: name,
                        minimum,
                        maximum_exclusive: None,
                        minimum_option: None,
                    },
                );
            }
        }
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
                && ((prerequisite.minimum.is_none() && prerequisite.maximum_exclusive.is_none())
                    || {
                        tool.version
                            .as_deref()
                            .and_then(version_pair)
                            .is_some_and(|(major, minor)| prerequisite.accepts([major, minor]))
                    });
            let requirement = prerequisite.description();
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
    report.result = Some(CommandResult::Doctor(DoctorResult { tools }));
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
