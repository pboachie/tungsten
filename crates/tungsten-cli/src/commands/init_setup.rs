// SPDX-License-Identifier: AGPL-3.0-only
//! Scoped first-use checks for freshly generated projects.
use crate::args::InitArgs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tungsten_config::capabilities::ToolRequirement;

pub(super) fn preflight(args: &InitArgs) -> Result<(), String> {
    let init = &tungsten_config::capabilities::INIT;
    // Docs alone builds nothing to set up.
    if args.targets.is_empty()
        || args.targets.iter().all(|t| t == "docs")
        || args.targets.iter().any(|t| !init.local_setup(t))
    {
        return Err(
            "local setup supports TypeScript, Python, Rust/CLI, MCP, WebMCP and docs".into(),
        );
    }
    if ["tungsten.yml", "agent.yml", "generated"]
        .iter()
        .any(|name| std::fs::symlink_metadata(args.dir.join(name)).is_ok())
    {
        return Err(
            "local setup requires a new project to avoid executing existing custom code".into(),
        );
    }
    if args
        .targets
        .iter()
        .any(|t| matches!(t.as_str(), "rust" | "cli"))
    {
        super::init_setup_rust::preflight()?;
    }
    if args.targets.iter().any(|t| t == "python") {
        super::init_setup_python::interpreter()?;
    }
    // Every selected target's declared `node` requirement must hold.
    let node: Vec<ToolRequirement> = args
        .targets
        .iter()
        .filter_map(|t| tungsten_config::capabilities::target(t))
        .flat_map(|target| target.prerequisites.iter().copied())
        .filter(|requirement| requirement.tool == "node")
        .collect();
    if node.is_empty() {
        return Ok(());
    }
    let required = node
        .iter()
        .map(ToolRequirement::description)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(" and ");
    execute(
        "node",
        &["-e", &node_check(&node)],
        Path::new("."),
        Duration::from_secs(5),
    )
    .map_err(|_| {
        format!("Node {required} is required for local setup (toolchain check timed out or failed)")
    })?;
    execute(
        npm(),
        &["--version"],
        Path::new("."),
        Duration::from_secs(5),
    )
    .map_err(|_| "npm is required for local setup (toolchain check timed out or failed)")?;
    Ok(())
}

/// A Node script exiting 0 exactly when the running version meets every
/// requirement, compared on major.minor as [`ToolRequirement::accepts`] does.
fn node_check(requirements: &[ToolRequirement]) -> String {
    let mut conditions = vec![];
    for requirement in requirements {
        if let Some([major, minor]) = requirement.minimum {
            conditions.push(format!("c({major},{minor})>=0"));
        }
        if let Some([major, minor]) = requirement.maximum_exclusive {
            conditions.push(format!("c({major},{minor})<0"));
        }
        for [major, minor] in requirement.excluded_versions {
            conditions.push(format!("c({major},{minor})!==0"));
        }
    }
    if conditions.is_empty() {
        conditions.push("true".into());
    }
    format!(
        "const v=process.versions.node.split('.').map(Number);\
         const c=(a,b)=>v[0]-a||v[1]-b;process.exit(({})?0:1)",
        conditions.join("&&")
    )
}

fn npm() -> &'static str {
    if cfg!(windows) { "npm.cmd" } else { "npm" }
}

pub(super) fn execute(
    program: &str,
    args: &[&str],
    dir: &Path,
    timeout: Duration,
) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| format!("could not start {program}; verify the required toolchain"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => {
                return Err(format!(
                    "{program} failed; inspect dependencies and toolchain in the generated project"
                ));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("cannot wait for {program}"));
            }
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{program} timed out; project files have been preserved"
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(super) fn run(args: &InitArgs) -> (Vec<crate::output::InitSetupTarget>, Result<(), String>) {
    let mut results = vec![];
    if args
        .targets
        .iter()
        .any(|t| matches!(t.as_str(), "typescript" | "mcp" | "webmcp"))
    {
        results.push(crate::output::InitSetupTarget {
            target: "typescript".into(),
            status: "not_attempted".into(),
        });
    }
    for target in ["python", "rust", "mcp", "webmcp", "docs"] {
        if args
            .targets
            .iter()
            .any(|t| t == target || (target == "rust" && t == "cli"))
        {
            results.push(crate::output::InitSetupTarget {
                target: target.into(),
                status: if target == "docs" {
                    "generated"
                } else {
                    "not_attempted"
                }
                .into(),
            });
        }
    }
    let result = setup(args, &mut results);
    (results, result)
}

fn setup(args: &InitArgs, results: &mut [crate::output::InitSetupTarget]) -> Result<(), String> {
    let compiled = crate::input::compile(&args.dir);
    let ir = compiled
        .ir
        .filter(|_| !compiled.diagnostics.has_errors())
        .ok_or("cannot compile project for the local example")?;
    let native = args
        .targets
        .iter()
        .any(|t| matches!(t.as_str(), "python" | "rust" | "cli"));
    let simple = |op: &&tungsten_ir::Operation| {
        op.method == tungsten_ir::HttpMethod::Get
            && op.body.is_none()
            && op.params.path.is_empty()
            && !op
                .params
                .query
                .iter()
                .chain(&op.params.header)
                .chain(&op.params.cookie)
                .any(|p| p.required)
    };
    let mut candidates: Vec<_> = ir
        .operations()
        .into_iter()
        .filter(|op| {
            op.security.is_empty()
                && op.streaming.is_none()
                && matches!(op.status, tungsten_ir::OperationStatus::Implemented)
                && !op.agent.hidden
        })
        .collect();
    candidates.sort_by_key(|op| !simple(op));
    let (operation, example) = candidates.into_iter().filter(|op| !native || simple(op)).find_map(|op| super::init_example::arguments(&ir, op).ok().map(|example| (op, example))).ok_or("local example needs an implemented unauthenticated operation with a synthesizable JSON request; Python/Rust checks currently require a GET without required arguments")?;
    let operation_id = operation.id.0.clone();
    let method = format!("{:?}", operation.method).to_ascii_uppercase();
    let example = serde_json::to_string(&example).map_err(|_| "cannot encode local arguments")?;
    let rust_binary =
        tungsten_ir::naming::to_case(&ir.api.name.words, tungsten_ir::naming::Case::Kebab);
    for result in results.iter_mut().filter(|r| r.target != "docs") {
        let directory = args.dir.join("generated").join(&result.target);
        result.status = "failed".into();
        if result.target == "rust" {
            super::init_setup_rust::build(&directory)?;
            result.status = "built".into();
            continue;
        }
        if result.target == "python" {
            super::init_setup_python::build(&directory, args.python_runtime.as_deref())?;
            result.status = "built".into();
            continue;
        }
        execute(
            npm(),
            &["install", "--ignore-scripts", "--no-audit", "--no-fund"],
            &directory,
            Duration::from_secs(180),
        )?;
        execute(
            "node",
            &["node_modules/typescript/bin/tsc", "-p", "tsconfig.json"],
            &directory,
            Duration::from_secs(60),
        )?;
        result.status = "built".into();
    }
    let server = tungsten_mock::MockServer::start(ir, tungsten_mock::MockOptions::default())
        .map_err(|_| "cannot start the loopback mock")?;
    let base_url = server.base_url();
    for target in results
        .iter()
        .filter(|r| matches!(r.target.as_str(), "typescript" | "python" | "rust" | "mcp"))
    {
        let directory = args.dir.join("generated").join(&target.target);
        let before = server.calls().len();
        let result = if target.target == "python" {
            super::init_setup_python::run(&directory, &base_url, &operation_id)
        } else if target.target == "rust" {
            super::init_setup_rust::run(&directory, &rust_binary, &base_url, &operation_id)
        } else if target.target == "mcp" {
            super::init_setup_mcp::run(&directory, &base_url, &operation_id, &method, &example)
        } else {
            execute(
                "node",
                &[
                    "--input-type=module",
                    "-e",
                    LOCAL_CALL,
                    &base_url,
                    &operation_id,
                    &method,
                    &example,
                ],
                &directory,
                Duration::from_secs(20),
            )
        };
        let observed = server.calls().iter().skip(before).any(|call| {
            call.operation.as_deref() == Some(&operation_id)
                && (200..300).contains(&call.response_status)
        });
        if let Err(error) = result {
            server.shutdown();
            return Err(error);
        }
        if !observed {
            server.shutdown();
            return Err(format!(
                "generated {} client did not produce a successful mock receipt",
                target.target
            ));
        }
    }
    server.shutdown();
    Ok(())
}

const LOCAL_CALL: &str = r#"
import * as sdk from './dist/index.js';
const [baseUrl, operationId, method, encodedArgs] = process.argv.slice(1);
const args = JSON.parse(encodedArgs);
const Client = Object.values(sdk).find(v => typeof v === 'function' && /Client$/.test(v.name));
if (!Client) throw new Error('generated client not found');
const guardedFetch = (input, options) => {
  const url = new URL(typeof input === 'string' || input instanceof URL ? input : input.url);
  if (url.origin !== new URL(baseUrl).origin || (options?.method ?? 'GET').toUpperCase() !== method) throw new Error('non-local request blocked');
  return fetch(input, {...options, redirect: 'error'});
};
const client = new Client({baseUrl, fetch: guardedFetch, retries: {max: 0}, timeoutMs: 5000});
const seen = new Set();
function find(object, depth = 0) {
  if (!object || depth > 5 || seen.has(object)) return;
  seen.add(object);
  for (const [key, value] of Object.entries(object)) {
    if (key === 'core' || key === 'macros') continue;
    if (typeof value === 'function' && value.descriptor?.id === operationId) return value;
    if (value && typeof value === 'object') { const found = find(value, depth + 1); if (found) return found; }
  }
}
const call = find(client);
if (!call) throw new Error('generated operation not found');
let options = {};
if (typeof call.preview === 'function') {
  const preview = await call.preview(args);
  if (!preview.ok) throw new Error('local preview failed');
  if (preview.value.confirmation_token) options.confirm = preview.value.confirmation_token;
}
const result = await call(args, options);
if (!result.ok) throw new Error('local generated call failed');
"#;
