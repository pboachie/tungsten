// SPDX-License-Identifier: AGPL-3.0-only
//! The experimental `mcp-python` target: a native Python MCP server package
//! that serves the same manifest as the `mcp` target through the
//! `tungsten-mcp` Python runtime (`runtimes/mcp-python`) on top of the
//! generated Python SDK, over stdio or Streamable HTTP, in discrete or
//! progressive mode.
//!
//! Files: `pyproject.toml`, `README.md`, `.env.example` and the package
//! `<module>/` (`__init__.py`, `__main__.py`, `server.py`,
//! `manifest.json` and the hand-editable `custom/__init__.py`).
//!
//! | Option | Meaning | Default |
//! |---|---|---|
//! | `package` | distribution name of the server | `<api>-mcp` |
//! | `module` | import name of the server | `<api>_mcp` |
//! | `version` | package version | `0.1.0` |
//! | `sdk` | distribution name of the generated Python SDK | the `python` target's `package` |
//! | `sdk_module` | import name of the generated Python SDK | the `python` target's `module` |
//! | `sdk_version` | the SDK version required (`==`) | the `python` target's `version`, else `0.1.0` |
//! | `sdk_path` | the SDK from a local path (a uv path source) | |
//! | `runtime` | `tungsten-mcp` version specifier | `>=0.1.3,<0.2.0` |
//! | `runtime_path` | `tungsten-mcp` from a local path (a uv path source) | |
//! | `sdk_runtime_path` | `tungsten-runtime` from a local path (a uv path source) | |
//!
//! The command line passes the sibling `python` target's options as the
//! reserved option `python_target` (`{package, module, version}`). An invalid
//! value is a TG0728 warning and the default is used. What the runtime does
//! not serve yet is reported by [`McpPythonEmitter::supports`] as TG0729.

use serde_json::Value;
use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::{CommentStyle, Emitter, FileSet, TargetConfig, header};
use tungsten_ir::Ir;

use crate::env::{self, Credentials};
use crate::manifest::{self, McpManifest, ToolKind};

/// Default version specifier of the `tungsten-mcp` runtime.
pub const DEFAULT_RUNTIME: &str = ">=0.1.3,<0.2.0";

/// Emits the experimental `mcp-python` target.
#[derive(Debug, Clone, Copy, Default)]
pub struct McpPythonEmitter;

impl Emitter for McpPythonEmitter {
    fn id(&self) -> &'static str {
        "mcp-python"
    }

    fn supports(&self, ir: &Ir) -> Diagnostics {
        let (manifest, mut diags) = manifest::build(ir);
        diags.extend(gaps(ir, &manifest));
        diags
    }

    fn emit(&self, ir: &Ir, cfg: &TargetConfig, out: &mut FileSet) -> Diagnostics {
        let (opts, mut diags) = PythonOptions::resolve(ir, cfg);
        for (path, text) in generate(ir, &opts) {
            if let Err(e) = out.add(path, text) {
                diags.push(Diagnostic::error(
                    "TG0701",
                    format!("mcp-python target: {e}"),
                ));
            }
        }
        diags
    }
}

/// What the Python runtime does not serve for this API (TG0729).
fn gaps(ir: &Ir, manifest: &McpManifest) -> Diagnostics {
    let mut diags = Diagnostics::new();
    let streamed: Vec<&str> = ir
        .operations()
        .into_iter()
        .filter(|op| op.stream.is_some())
        .map(|op| op.id.0.as_str())
        .filter(|id| manifest.tools.iter().any(|t| t.target == *id))
        .collect();
    if !streamed.is_empty() {
        diags.push(
            Diagnostic::warning(
                "TG0729",
                format!(
                    "mcp-python does not collect event streams yet: streamed calls of {} return an envelope \
                     and send nothing",
                    streamed.join(", ")
                ),
            )
            .with_help("use the `mcp` target, or the SDK's stream(), for these operations"),
        );
    }
    let python = tungsten_emit_py::PythonEmitter.supports(ir);
    let macros: Vec<String> = python
        .iter()
        .filter(|d| d.code == "TG0730")
        .filter_map(|d| {
            let rest = d.message.split_once('`')?.1;
            Some(rest.split_once('`')?.0.to_string())
        })
        .filter(|name| {
            manifest
                .tools
                .iter()
                .any(|t| t.kind == ToolKind::Macro && t.target == *name)
        })
        .collect();
    if !macros.is_empty() {
        diags.push(
            Diagnostic::warning(
                "TG0729",
                format!(
                    "the Python SDK does not emit macro(s) {}, so mcp-python answers their tools with a \
                     misconfiguration envelope and sends nothing",
                    macros.join(", ")
                ),
            )
            .with_help("see the python target's TG0730 diagnostics, or use the `mcp` target"),
        );
    }
    diags
}

/// Resolved options of the `mcp-python` target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonOptions {
    pub package: String,
    pub module: String,
    pub version: String,
    pub sdk: String,
    pub sdk_module: String,
    pub sdk_version: String,
    pub sdk_path: Option<String>,
    pub runtime: String,
    pub runtime_path: Option<String>,
    pub sdk_runtime_path: Option<String>,
}

fn is_module(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_distribution(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.ends_with(|c: char| c.is_ascii_alphanumeric())
}

fn is_version(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | '!'))
}

fn is_text(s: &str) -> bool {
    !s.trim().is_empty() && !s.contains(['\n', '\r', '"', '\\'])
}

impl PythonOptions {
    /// Read the options of `cfg`, falling back to defaults with a TG0728
    /// warning for each invalid value.
    pub fn resolve(ir: &Ir, cfg: &TargetConfig) -> (PythonOptions, Diagnostics) {
        let mut diags = Diagnostics::new();
        let (py, _) = tungsten_emit_py::Options::resolve(
            ir,
            &TargetConfig {
                name: "python".into(),
                out_dir: cfg.out_dir.clone(),
                options: cfg
                    .options
                    .get("python_target")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({})),
            },
        );
        let mut read = |key: &str, valid: &dyn Fn(&str) -> bool, expected: &str| {
            let value = cfg.options.get(key)?;
            match value.as_str() {
                Some(s) if valid(s) => Some(s.to_string()),
                _ => {
                    diags.push(
                        Diagnostic::warning(
                            "TG0728",
                            format!(
                                "target option `mcp-python.{key}` must be {expected}; the default is used"
                            ),
                        )
                        .with_help(format!("got {value}")),
                    );
                    None
                }
            }
        };
        let api = &ir.api.name;
        let base = if api.words.is_empty() {
            "api".to_string()
        } else {
            api.snake()
        };
        let opts = PythonOptions {
            package: read("package", &is_distribution, "a distribution name")
                .unwrap_or_else(|| format!("{}-mcp", base.replace('_', "-"))),
            module: read("module", &is_module, "a lowercase module name")
                .unwrap_or_else(|| format!("{base}_mcp")),
            version: read("version", &is_version, "a version").unwrap_or_else(|| "0.1.0".into()),
            sdk: read("sdk", &is_distribution, "a distribution name").unwrap_or(py.package),
            sdk_module: read("sdk_module", &is_module, "a lowercase module name")
                .unwrap_or(py.module),
            sdk_version: read("sdk_version", &is_version, "a version").unwrap_or(py.version),
            sdk_path: read("sdk_path", &is_text, "a non-empty path"),
            runtime: read("runtime", &is_text, "a version specifier")
                .unwrap_or_else(|| DEFAULT_RUNTIME.into()),
            runtime_path: read("runtime_path", &is_text, "a non-empty path"),
            sdk_runtime_path: read("sdk_runtime_path", &is_text, "a non-empty path"),
        };
        (opts, diags)
    }
}

/// The generated-file header and a blank line.
fn head(ir: &Ir) -> String {
    let mut out = header(CommentStyle::Hash, ir);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// A Python (and TOML basic) string literal: JSON escapes are valid in both.
fn lit(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// Every file of the server package: (path relative to the target
/// directory, contents), in a fixed order.
pub fn generate(ir: &Ir, opts: &PythonOptions) -> Vec<(String, String)> {
    let (manifest, _) = manifest::build(ir);
    let manifest_json =
        crate::json::pretty(&serde_json::to_value(&manifest).unwrap_or(Value::Null));
    let m = &opts.module;
    vec![
        (".env.example".into(), env_example(ir)),
        ("README.md".into(), readme(ir, opts, &manifest)),
        ("pyproject.toml".into(), pyproject(ir, opts)),
        (format!("{m}/__init__.py"), init_py(ir)),
        (format!("{m}/__main__.py"), main_py(ir)),
        (format!("{m}/custom/__init__.py"), custom_template()),
        (format!("{m}/manifest.json"), manifest_json),
        (format!("{m}/server.py"), server_py(ir, opts)),
    ]
}

fn pyproject(ir: &Ir, opts: &PythonOptions) -> String {
    let mut out = head(ir);
    out.push_str(&format!(
        "\n[build-system]\nrequires = [\"hatchling>=1.25\"]\nbuild-backend = \"hatchling.build\"\n\n\
         [project]\nname = {}\nversion = {}\ndescription = {}\nreadme = \"README.md\"\nrequires-python = \">=3.12\"\n\
         dependencies = [\n  {},\n  {},\n]\n\n[project.scripts]\n{} = {}\n\n\
         [tool.hatch.build.targets.wheel]\npackages = [{}]\n",
        lit(&opts.package),
        lit(&opts.version),
        lit(&format!(
            "Experimental MCP server for the {} API, generated by tungsten.",
            tungsten_ir::title_stem(&ir.api.title)
        )),
        lit(&format!("tungsten-mcp{}", opts.runtime)),
        lit(&format!("{}=={}", opts.sdk, opts.sdk_version)),
        opts.package,
        lit(&format!("{}.server:main", opts.module)),
        lit(&opts.module),
    ));
    let sources: Vec<String> = [
        ("tungsten-mcp", &opts.runtime_path),
        (opts.sdk.as_str(), &opts.sdk_path),
        ("tungsten-runtime", &opts.sdk_runtime_path),
    ]
    .iter()
    .filter_map(|(name, path)| {
        path.as_ref()
            .map(|p| format!("{name} = {{ path = {} }}", lit(p)))
    })
    .collect();
    if !sources.is_empty() {
        out.push_str(
            "\n# Local checkouts (uv; pip installs the published packages).\n[tool.uv.sources]\n",
        );
        for line in sources {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

fn init_py(ir: &Ir) -> String {
    let mut out = head(ir);
    out.push_str(&format!(
        "\n\"\"\"Experimental MCP server for the {} API (`python -m` this package, or its command).\"\"\"\n",
        tungsten_ir::title_stem(&ir.api.title)
    ));
    out
}

fn main_py(ir: &Ir) -> String {
    let mut out = head(ir);
    out.push_str("\nfrom .server import main\n\nraise SystemExit(main())\n");
    out
}

fn custom_template() -> String {
    "# Hand-written extensions of this MCP server. tungsten creates this file once\n\
     # and never overwrites or deletes it; server.py calls `customize` before the\n\
     # server starts.\n\
     \n\
     from tungsten_mcp import ServerOptions\n\
     \n\
     \n\
     def customize(options: ServerOptions) -> ServerOptions:\n\
     \x20   \"\"\"Adjust the server options (wrap `create_core`, change limits or the name);\n\
     \x20   return them unchanged to keep the defaults.\"\"\"\n\
     \x20   return options\n"
        .to_string()
}

/// `AUTH_ENV`: the environment variable of each credential, by scheme.
fn auth_env(ir: &Ir) -> String {
    let schemes = env::schemes(ir);
    if schemes.is_empty() {
        return "{}".into();
    }
    let mut out = String::from("{\n");
    for s in schemes {
        match &s.credentials {
            Credentials::Secret { env, .. } => {
                out.push_str(&format!("    {}: {},\n", lit(&s.scheme), lit(env)));
            }
            Credentials::Parts(parts) => {
                let entries: Vec<String> = parts
                    .iter()
                    .map(|(key, env, _)| format!("{}: {}", lit(key), lit(env)))
                    .collect();
                out.push_str(&format!(
                    "    {}: {{{}}},\n",
                    lit(&s.scheme),
                    entries.join(", ")
                ));
            }
        }
    }
    out.push('}');
    out
}

fn server_py(ir: &Ir, opts: &PythonOptions) -> String {
    let client = tungsten_emit_py::async_client_class(ir);
    let mut out = head(ir);
    out.push_str(&format!(
        "\n\"\"\"MCP server for the {title} API: serves manifest.json through tungsten-mcp on top of\n\
         the generated Python SDK, over stdio or Streamable HTTP. Configuration comes from the\n\
         environment only\n\
         (README.md, .env.example).\"\"\"\n\
         \n\
         from __future__ import annotations\n\
         \n\
         import os\n\
         import sys\n\
         from collections.abc import Sequence\n\
         \n\
         from {sdk} import {client}\n\
         from tungsten_mcp import ServeError, ServerOptions, check_mode, load_manifest, run\n\
         from tungsten_runtime import ClientOptions\n\
         \n\
         from .custom import customize\n\
         \n\
         #: Environment variable of each credential, by auth scheme name; a composite\n\
         #: profile maps each of its credential keys to a variable.\n\
         AUTH_ENV: dict[str, str | dict[str, str]] = {auth}\n\
         BASE_URL_ENV = {base}\n\
         MODE_ENV = {mode}\n\
         DEADLINE_ENV = {deadline}\n\
         \n\
         \n\
         def _env(name: str) -> str | None:\n\
         \x20   \"\"\"A variable's value; unset and empty are the same.\"\"\"\n\
         \x20   value = os.environ.get(name)\n\
         \x20   return value or None\n\
         \n\
         \n\
         def client_options() -> ClientOptions:\n\
         \x20   \"\"\"Client options from the environment. Schemes without credentials are left\n\
         \x20   out; the runtime names what is missing when an operation needs them.\"\"\"\n\
         \x20   auth: dict[str, str | dict[str, str]] = {{}}\n\
         \x20   for scheme, source in AUTH_ENV.items():\n\
         \x20       if isinstance(source, str):\n\
         \x20           secret = _env(source)\n\
         \x20           if secret is not None:\n\
         \x20               auth[scheme] = secret\n\
         \x20           continue\n\
         \x20       parts = {{key: value for key, name in source.items() if (value := _env(name)) is not None}}\n\
         \x20       if parts:\n\
         \x20           auth[scheme] = parts\n\
         \x20   return ClientOptions(base_url=_env(BASE_URL_ENV), auth=auth)\n\
         \n\
         \n\
         def deadline_ms() -> int | None:\n\
         \x20   \"\"\"The total deadline of one tool call set by the environment, if any.\"\"\"\n\
         \x20   text = _env(DEADLINE_ENV)\n\
         \x20   if text is None:\n\
         \x20       return None\n\
         \x20   if not text.isdigit():\n\
         \x20       raise ServeError(f\"{{DEADLINE_ENV}} must be a whole number of milliseconds, not {{text!r}}\")\n\
         \x20   return int(text) or None\n\
         \n\
         \n\
         def main(argv: Sequence[str] | None = None) -> int:\n\
         \x20   \"\"\"Serve over stdio, or Streamable HTTP with `--http <port>`; the exit status.\"\"\"\n\
         \x20   manifest = load_manifest(__package__ or {module})\n\
         \x20   try:\n\
         \x20       mode = check_mode(manifest, _env(MODE_ENV), MODE_ENV)\n\
         \x20       deadline = deadline_ms()\n\
         \x20   except ServeError as error:\n\
         \x20       print(f\"error: {{error}}\", file=sys.stderr)\n\
         \x20       return 1\n\
         \x20   options = ServerOptions(\n\
         \x20       manifest=manifest,\n\
         \x20       # One core per MCP session: confirmation tokens and shown-once state are per session.\n\
         \x20       create_core=lambda: {client}(client_options()).core,\n\
         \x20       name={name},\n\
         \x20       version={version},\n\
         \x20       tool_deadline_ms=deadline,\n\
         \x20       mode=mode,\n\
         \x20   )\n\
         \x20   return run(customize(options), argv)\n",
        title = tungsten_ir::title_stem(&ir.api.title),
        sdk = opts.sdk_module,
        auth = auth_env(ir),
        base = lit(&env::base_url(ir)),
        mode = lit(&env::mode(ir)),
        deadline = lit(&env::deadline(ir)),
        module = lit(&opts.module),
        name = lit(&opts.package),
        version = lit(&opts.version),
    ));
    out
}

/// The variables of `.env.example` and the README: the `mcp` target's.
fn variables(ir: &Ir) -> Vec<(String, String)> {
    crate::package::variables(ir)
}

fn env_example(ir: &Ir) -> String {
    let mut out = head(ir);
    out.push_str(&format!(
        "\n#\n# Environment of the {} MCP server (Python). Your MCP client passes these to\n\
         # the server process; set only what your operations need and never commit secrets.\n",
        tungsten_ir::title_stem(&ir.api.title)
    ));
    for (name, what) in variables(ir) {
        out.push_str(&format!("\n# {}\n{name}=\n", what.replace('`', "")));
    }
    out
}

fn readme(ir: &Ir, opts: &PythonOptions, manifest: &McpManifest) -> String {
    let title = tungsten_ir::title_stem(&ir.api.title);
    let mut out = String::from("<!--\n");
    for l in header(CommentStyle::Hash, ir).lines() {
        out.push_str(l.trim_start_matches('#').trim_start());
        out.push('\n');
    }
    out.push_str("-->\n\n");
    let macros = manifest
        .tools
        .iter()
        .filter(|t| t.kind == ToolKind::Macro)
        .count();
    let previewable = manifest.tools.iter().any(|t| t.safety != "read_only");
    out.push_str(&format!(
        "# {package}\n\n\
         **Experimental.** MCP server for the {title} API, generated by tungsten. It serves {tools} \
         tools ({ops} operations, {macros} macros{preview}) over stdio or Streamable HTTP through the `tungsten-mcp` \
         Python runtime, calling the API with the generated Python SDK `{sdk}`. It needs neither \
         Node nor the TypeScript SDK. The `mcp` target remains the supported MCP server; see Limits.\n\n",
        package = opts.package,
        tools = manifest.tools.len(),
        ops = manifest.tools.len() - macros,
        preview = if previewable { ", plus `preview`" } else { "" },
        sdk = opts.sdk,
    ));
    out.push_str(&format!(
        "## Install and run\n\n\
         Install the generated SDK, the runtimes and this package into one environment (Python 3.12 or \
         newer), for example from local paths until they are published:\n\n\
         ```sh\npip install <tungsten>/runtimes/python <tungsten>/runtimes/mcp-python <sdk dir> .\n```\n\n\
         Then configure your MCP client to launch `{package}` (or `python -m {module}`) with the \
         environment below. It speaks MCP on standard input and output; its own errors go to standard \
         error.\n\n\
         ```json\n{{\n  \"mcpServers\": {{\n    {key}: {{\n      \"command\": {command},\n      \"env\": {{ {base}: \"https://...\" }}\n    }}\n  }}\n}}\n```\n\n\
         `{package} --http 3000` serves Streamable HTTP at `http://127.0.0.1:3000/mcp` instead: one MCP \
         session per `Mcp-Session-Id`, each with its own API client, loopback-bound with Host and Origin \
         checks. Add `--token-env NAME` to require `Authorization: Bearer <the value of NAME>`; binding \
         another interface with `--host` needs it (or your own access control).\n\n",
        package = opts.package,
        module = opts.module,
        key = lit(&opts.package),
        command = lit(&opts.package),
        base = lit(&env::base_url(ir)),
    ));
    out.push_str("## Environment\n\n| Variable | Meaning |\n|---|---|\n");
    for (name, what) in variables(ir) {
        out.push_str(&format!("| `{name}` | {} |\n", what.replace('|', "\\|")));
    }
    out.push_str(
        "\n## Safety\n\n\
         - Every call goes through the Python runtime: argument validation, safety tiers, idempotency \
         keys, safe retries, verification of irreversible operations and response bounds.\n\
         - Destructive and irreversible tools need the `confirmation_token` that `preview` returns for \
         the same arguments, in the same session; `true` never confirms.\n\
         - Failures are tool results with the diagnostic envelope as `structuredContent`. \
         `DEADLINE_EXCEEDED` means nothing more was sent; `OUTCOME_UNKNOWN` means a mutation may have \
         taken effect: check it, and retry only with the same idempotency key.\n\
         - Sensitive response fields and shown-once results are returned once per session.\n\n\
         ## Limits\n\n\
         - Streamed (server-sent event) results are not collected; such calls return an envelope and \
         send nothing.\n\
         - No `run_script`, embedding search, resources or prompts.\n\
         - See the `tungsten-mcp` runtime README for the full list.\n\n\
         `custom/__init__.py` is yours: tungsten creates it once and never overwrites it.\n",
    );
    out
}
