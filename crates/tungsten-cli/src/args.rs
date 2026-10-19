// SPDX-License-Identifier: AGPL-3.0-only
//! Command-line grammar.

use std::path::PathBuf;

use clap::{Args, ColorChoice, Parser, Subcommand, ValueEnum};
use serde::Serialize;

use crate::output::CommandName;

macro_rules! examples_text {
    () => {
        "\
Examples:
  tungsten init --from openapi.yaml    Write tungsten.yml and agent.yml
  tungsten check .                     Validate the project and print diagnostics
  tungsten config .                    Explain supported effective configuration
  tungsten generate --target ts,py     Generate the TypeScript and Python SDKs
  tungsten generate --target webmcp    Generate a browser WebMCP adapter
  tungsten mock --port 4010            Serve a mock of the API from the IR
  tungsten report --html report.html   Write a static coverage and safety report
  tungsten diff --semver               Show how the API surface changed
  tungsten overlay suggest --write fixes.overlay.yaml
                                       Suggest an overlay that fixes diagnostics
  tungsten lsp                         Serve diagnostics, completion and hover for
                                       manifests to an editor over stdio

Repository: https://github.com/pboachie/tungsten

"
    };
}

macro_rules! exit_codes_text {
    () => {
        "\
Exit codes:
  0  success (also `diff` when regeneration would change files, and
     `report` with warnings)
  1  the input has errors (or warnings with --strict), generated output is
     stale (generate --check, check --ci), or the item asked for does not
     exist (explain target not found, unknown target)
  2  usage error: the command line could not be parsed
  3  I/O or internal failure (for example --out is not writable, or the
     mock server cannot start)
  4  the command refused to act without confirmation (init over existing
     files, generate into a directory tungsten did not write: pass --force)

Output:
  Without --json, results go to stdout and diagnostics to stderr. With
  --json, stdout carries exactly one JSON document described by
  `tungsten schema cli-output`, help and version included. Color is used
  only on a terminal and never when NO_COLOR is set."
    };
}

const SHORT_HELP: &str = examples_text!();

const LONG_HELP: &str = concat!(examples_text!(), exit_codes_text!());

#[derive(Debug, Parser)]
#[command(
    name = "tungsten",
    version,
    about = "Agent-native SDK generator and MCP/WebMCP compiler",
    after_help = SHORT_HELP,
    after_long_help = LONG_HELP,
    arg_required_else_help = true,
    color = ColorChoice::Never
)]
pub(crate) struct Cli {
    /// Reuse verified HTTP/Git input pins and cached files without network access.
    #[arg(long, global = true, conflicts_with = "refresh_inputs")]
    pub offline: bool,
    /// Refresh an HTTP content pin or resolve a Git ref again after review.
    /// Converted imports require init into a fresh project to change sources.
    #[arg(long, global = true)]
    pub refresh_inputs: bool,
    /// Header for a URL typed on the command line; for manifest URLs use
    /// [https://api.example.com] Name: value to restrict it to that origin. Not Git authentication.
    /// Environment values are newline-separated. Prefer the environment for secrets.
    #[arg(
        long,
        global = true,
        env = "TUNGSTEN_INPUT_HEADERS",
        hide_env_values = true,
        value_delimiter = '\n',
        value_name = "[ORIGIN] NAME: VALUE"
    )]
    pub input_header: Vec<String>,
    /// Print exactly one JSON document on stdout.
    #[arg(long, global = true)]
    pub json: bool,
    /// Never start the commands of `external` targets. Without it, an
    /// `external` target's command runs on `generate` (any mode), `check
    /// --ci`, `diff` and `report`, with your privileges and with whatever
    /// the manifest says; pass it (or set TUNGSTEN_NO_EXTERNAL=1) when the
    /// manifest is not trusted, for example on a pull request in CI. Such
    /// targets are reported as skipped (TG0807). Also disables `emitters`
    /// discovery, including descriptor probes of programs on PATH.
    #[arg(
        long,
        global = true,
        env = "TUNGSTEN_NO_EXTERNAL",
        value_parser = clap::builder::BoolishValueParser::new()
    )]
    pub no_external: bool,
    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// CI checks and pure configuration inspection never color their output.
    pub fn forces_plain(&self) -> bool {
        matches!(&self.command, Command::Config(_))
            || matches!(&self.command, Command::Check(args) if args.ci)
    }

    /// The command's name in `--json` output; `None` for `lsp`, which
    /// speaks the language server protocol instead.
    pub fn command_name(&self) -> Option<CommandName> {
        Some(match self.command {
            Command::Check(_) => CommandName::Check,
            Command::Ir {
                command: IrCommand::Dump(_),
            } => CommandName::IrDump,
            Command::Explain(_) => CommandName::Explain,
            Command::Schema(_) => CommandName::Schema,
            Command::Init(_) => CommandName::Init,
            Command::Doctor(_) => CommandName::Doctor,
            Command::Capabilities(_) => CommandName::Capabilities,
            Command::Config(_) => CommandName::Config,
            Command::Emitters(_) => CommandName::Emitters,
            Command::Generate(_) => CommandName::Generate,
            Command::Mock(_) => CommandName::Mock,
            Command::Scenario { .. } => CommandName::Scenario,
            Command::Report(_) => CommandName::Report,
            Command::Diff(_) => CommandName::Diff,
            Command::Overlay {
                command: OverlayCommand::Suggest(_),
            } => CommandName::OverlaySuggest,
            Command::Lsp(_) => return None,
        })
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Validate the manifest and specs and report diagnostics.
    Check(CheckArgs),
    /// Intermediate representation commands.
    Ir {
        #[command(subcommand)]
        command: IrCommand,
    },
    /// Explain a diagnostic code, an operation or a type.
    Explain(ExplainArgs),
    /// Print a JSON Schema published by tungsten.
    Schema(SchemaArgs),
    /// Write tungsten.yml and agent.yml for a new project.
    Init(Box<InitArgs>),
    /// Report installed tools and optionally check project prerequisites.
    Doctor(DoctorArgs),
    /// Query built-in target declarations without executing tools or emitters.
    Capabilities(CapabilitiesArgs),
    /// Explain supported effective configuration without compiling or running tools.
    Config(ConfigArgs),
    /// List the external emitters (`tungsten-emit-<name>`) that can be
    /// run, with the protocol and version each reports.
    Emitters(EmittersArgs),
    /// Generate every configured target (SDKs, docs) from the project.
    Generate(GenerateArgs),
    /// Serve a mock of the API from the compiled IR.
    Mock(MockArgs),
    /// Validate a saved mock scenario or run an explicitly selected integration.
    Scenario {
        #[command(subcommand)]
        command: ScenarioCommand,
    },
    /// Coverage, safety matrix, token budgets, diagnostics and changes
    /// since the last generation, as text, one static HTML file or JSON.
    Report(ReportArgs),
    /// Show what regeneration would add, change or remove in each target,
    /// and with --semver how the API surface changed.
    Diff(DiffArgs),
    /// OpenAPI Overlay authoring helpers.
    Overlay {
        #[command(subcommand)]
        command: OverlayCommand,
    },
    /// Serve a Language Server Protocol server on stdin and stdout for
    /// tungsten.yml, agent.yml and overlays: diagnostics, completion, hover,
    /// go-to-definition and symbols. `--json` has no effect.
    Lsp(LspArgs),
}

#[derive(Debug, Args)]
pub(crate) struct LspArgs {
    /// The manifest to serve: a tungsten.yml, or a directory holding one.
    /// Default: the tungsten.yml of the editor's workspace folder.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct CapabilitiesArgs {
    /// Limit the catalog to one target id or CLI alias.
    pub target: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ConfigArgs {
    /// Local project directory or tungsten manifest.
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Fail on supported-option warnings as well as configuration errors.
    #[arg(long)]
    pub strict: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum OverlayCommand {
    /// Suggest an Overlay 1.0 document whose actions fix diagnostics
    /// (a missing or repeated operationId) without changing the IR.
    Suggest(SuggestArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum IrCommand {
    /// Write the IR as JSON.
    Dump(DumpArgs),
}

/// Input selection shared by the commands that compile a project.
#[derive(Debug, Args)]
pub(crate) struct InputArgs {
    /// A directory with a tungsten.yml, a tungsten*.yml manifest, or a
    /// single OpenAPI document (local path, HTTP(S) URL or git+ URL).
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct CheckArgs {
    #[command(flatten)]
    pub input: InputArgs,
    /// CI mode: never prompt, never color, and fail (TG0901) when the
    /// generated output of a target differs from what `generate` would
    /// write. This runs every configured target's emitter in memory,
    /// including the commands of `external` targets (see --no-external).
    #[arg(long)]
    pub ci: bool,
    /// Treat warnings as errors.
    #[arg(long)]
    pub strict: bool,
    /// Audit the success response contracts after compiling (opt-in): a
    /// JSON response without a schema (TG0930), a schema that constrains
    /// nothing (TG0305) and a free-form object response (TG0931) become
    /// warnings naming the operation, status and media type. Nothing is
    /// rejected and the default exit behavior is unchanged; combine with
    /// --strict to fail when the audit finds weak contracts. Typed maps,
    /// no-content responses and non-JSON media types are not findings; a
    /// schema (or media-type entry) with a nonempty string review note in
    /// `x-tungsten-reviewed-contract`, usually through an overlay, counts
    /// as reviewed. TG0932 reports an incomplete audit when budgets are hit.
    #[arg(long)]
    pub audit_contracts: bool,
    /// Run the response-contract audit and treat its findings as errors.
    /// Other warnings keep their usual severity (unlike --strict).
    #[arg(long)]
    pub require_contracts: bool,
    /// Print every warning and info diagnostic in full. By default a code
    /// that repeats is shown a few times, then summarized with a count;
    /// `--json` always lists every diagnostic.
    #[arg(long)]
    pub all_warnings: bool,
}

#[derive(Debug, Args)]
pub(crate) struct GenerateArgs {
    #[command(flatten)]
    pub input: InputArgs,
    /// Only these targets, comma-separated (`typescript,docs`; `ts`, `py`,
    /// `rs` and `rb` are accepted). Default: every target in tungsten.yml.
    #[arg(long, value_name = "TARGETS", value_delimiter = ',')]
    pub target: Vec<String>,
    /// Report what would be written or removed without touching the disk.
    #[arg(long, conflicts_with = "check")]
    pub dry_run: bool,
    /// Compare the output directories with what would be generated; write
    /// nothing and exit 1 when they differ.
    #[arg(long)]
    pub check: bool,
    /// Write into a non-empty output directory that tungsten did not
    /// generate (one without .tungsten/manifest.json).
    #[arg(long, conflicts_with = "check")]
    pub force: bool,
    /// Treat warnings (the compiler's and the emitters') as errors: nothing
    /// is written when there are any, in any target.
    #[arg(long)]
    pub strict: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ReportArgs {
    #[command(flatten)]
    pub input: InputArgs,
    /// Write the report as one static HTML file (no external assets, no
    /// scripts).
    #[arg(long, value_name = "FILE")]
    pub html: Option<PathBuf>,
    /// Also run the emitter of every target this version knows, not only
    /// the configured ones, to fill the coverage table and the token
    /// budgets of targets that are not enabled yet. Slow on a large API.
    /// Without it those targets are listed as `not_configured`. Configured
    /// `external` targets run either way (see --no-external).
    #[arg(long)]
    pub all_targets: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DiffArgs {
    #[command(flatten)]
    pub input: InputArgs,
    /// Only these targets, comma-separated. Default: every target in
    /// tungsten.yml.
    #[arg(long, value_name = "TARGETS", value_delimiter = ',')]
    pub target: Vec<String>,
    /// Classify the API surface change since the last generation (from
    /// .tungsten/surface.json) as major, minor, patch or none.
    #[arg(long)]
    pub semver: bool,
    /// Include actionable migration guidance (implies --semver).
    #[arg(long)]
    pub migration: bool,
}

#[derive(Debug, Args)]
pub(crate) struct MockArgs {
    #[command(flatten)]
    pub input: InputArgs,
    /// Load a versioned JSON scenario before serving; no commands are executed.
    #[arg(long, value_name = "FILE")]
    pub scenario: Option<PathBuf>,
    /// Port on 127.0.0.1 to listen on; 0 picks a free port.
    #[arg(long, default_value_t = 0)]
    pub port: u16,
    /// Seed for generated response values; the same seed gives the same
    /// responses.
    #[arg(long, default_value_t = 0)]
    pub seed: u64,
    /// Treat this runtime gate (its environment variable, e.g.
    /// SEALED_WEBHOOK_DELIVERY_ENABLED) as on; repeat for several. Gates
    /// not listed keep their default.
    #[arg(long = "gate", value_name = "ENV")]
    pub gates: Vec<String>,
    /// Recorded calls kept for `/__tungsten/calls`; past it the oldest is
    /// dropped. 0 records nothing.
    #[arg(long, value_name = "N", default_value_t = tungsten_mock::DEFAULT_MAX_RECORDED_CALLS)]
    pub max_recorded_calls: usize,
    /// Stored idempotent responses kept for replays; past it the oldest is
    /// dropped and a later request with its key is served as new. 0 stores
    /// nothing.
    #[arg(long, value_name = "N", default_value_t = tungsten_mock::DEFAULT_MAX_IDEMPOTENT_RESPONSES)]
    pub max_idempotent_responses: usize,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ScenarioCommand {
    /// Validate a saved JSON scenario without binding or executing commands.
    Validate(ScenarioValidateArgs),
    /// Load isolated mock state, execute explicit argv, and assert counts/exit.
    Run(ScenarioRunArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ScenarioValidateArgs {
    #[command(flatten)]
    pub input: InputArgs,
    #[arg(long, value_name = "FILE")]
    pub scenario: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct ScenarioRunArgs {
    #[command(flatten)]
    pub input: InputArgs,
    #[arg(long, value_name = "FILE")]
    pub scenario: PathBuf,
    /// Run-phase budget including cleanup; compilation/validation precede it.
    #[arg(long, default_value_t = 30_000, value_parser = clap::value_parser!(u64).range(20..=600_000))]
    pub timeout_ms: u64,
    #[arg(long, default_value_t = 0)]
    pub expect_exit: u8,
    #[arg(long, default_value_t = 10_000, value_parser = clap::value_parser!(u64).range(1..=10_000))]
    pub max_calls: u64,
    /// Executable and arguments after --. No shell interpolation is performed.
    #[arg(last = true, required = true, num_args = 1.., value_name = "COMMAND")]
    pub command: Vec<std::ffi::OsString>,
}

#[derive(Debug, Args)]
pub(crate) struct SuggestArgs {
    #[command(flatten)]
    pub input: InputArgs,
    /// Write the overlay to this file instead of stdout.
    #[arg(long, value_name = "FILE")]
    pub write: Option<PathBuf>,
    /// Overwrite the --write file if it exists.
    #[arg(long, requires = "write")]
    pub force: bool,
    /// Only suggest fixes for these diagnostic codes, comma-separated.
    #[arg(long, value_name = "CODES", value_delimiter = ',')]
    pub only: Vec<String>,
    /// The input (namespace) to suggest for. Needed when more than one
    /// input has suggestions: an overlay's targets only fit its own spec.
    #[arg(long, value_name = "NAME")]
    pub namespace: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct DumpArgs {
    #[command(flatten)]
    pub input: InputArgs,
    /// Write the IR to this file instead of stdout.
    #[arg(long, value_name = "FILE")]
    pub out: Option<PathBuf>,
    /// One line instead of indented JSON.
    #[arg(long, conflicts_with = "pretty")]
    pub compact: bool,
    /// Indented JSON (the default).
    #[arg(long)]
    pub pretty: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ExplainArgs {
    /// A diagnostic code (TG0201), an operation id (public.listPets) or a
    /// type id (public.Pet). A bare operationId or type name is accepted
    /// when it is unambiguous.
    pub target: String,
    /// Project used to resolve operation and type ids.
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub project: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct EmittersArgs {
    /// A project (as for `check`) whose `external` targets are listed too.
    pub path: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct SchemaArgs {
    /// Which schema to print.
    pub name: SchemaName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SchemaName {
    /// The tungsten.yml manifest.
    Tungsten,
    /// The agent.yml manifest.
    Agent,
    /// The intermediate representation written by `ir dump`.
    Ir,
    /// The `--json` output of this CLI.
    CliOutput,
    /// The documents of the external emitter protocol.
    ExternalEmitter,
    /// The SDK descriptor document (tungsten_emit::sdk::descriptors).
    SdkDescriptors,
    /// Transport-neutral tools and scoped groups.
    ToolCatalog,
    /// Built-in target capability declarations, independent of qualification.
    Capabilities,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct InitArgs {
    /// Acquisition provenance for a converted remote import, set internally.
    #[arg(skip)]
    pub source_url: Option<String>,
    #[arg(skip)]
    pub source_sha256: Option<String>,
    /// Advanced: default server URL for later live use; no credentials/query/fragment.
    #[arg(long, conflicts_with = "resume")]
    pub server_url: Option<String>,
    /// Advanced: generated Rust CLI environment profile NAME=URL; repeatable.
    /// Writes tungsten-profiles.toml with no credentials.
    #[arg(long, conflicts_with = "resume")]
    pub cli_profile: Vec<String>,
    /// Advanced: promote generation warnings to errors and withhold all outputs.
    #[arg(long)]
    pub strict: bool,
    /// Hide an exact namespace.operationId from agent surfaces; repeatable.
    #[arg(long, conflicts_with = "resume")]
    pub hide_operation: Vec<String>,
    /// Require argument-bound confirmation on an exact operation; repeatable.
    /// Never counts as imported-operation review or grants live authority.
    #[arg(long, conflicts_with = "resume")]
    pub require_confirmation: Vec<String>,
    /// Advanced: local Python runtime checkout (used only with explicit setup consent).
    #[arg(long, conflicts_with = "resume")]
    pub python_runtime: Option<PathBuf>,
    /// Advanced: local Rust runtime crate.
    #[arg(long, conflicts_with = "resume")]
    pub rust_runtime: Option<PathBuf>,
    /// Advanced: local Rust CLI kit crate.
    #[arg(long, conflicts_with = "resume")]
    pub cli_kit_runtime: Option<PathBuf>,
    /// Exact HTTP(S) origin to select from a multi-origin HAR (no credentials).
    #[arg(long, conflicts_with = "method")]
    pub har_origin: Option<String>,
    /// Reviewed HAR paths: JSON object mapping each captured URL path to a template.
    /// Required for every imported path before any converted output is saved.
    #[arg(long, conflicts_with = "method", value_name = "JSON")]
    pub har_paths: Option<PathBuf>,
    /// HAR path prefixes to include; repeatable, exclusions take precedence.
    #[arg(long)]
    pub include_prefix: Vec<String>,
    /// HAR path prefixes to exclude; repeatable.
    #[arg(long)]
    pub exclude_prefix: Vec<String>,
    /// Additional HAR property names to redact from structural observations.
    #[arg(long)]
    pub redact_property: Vec<String>,
    /// Advanced: use an explicitly supplied local WebMCP runtime package.
    #[arg(long, conflicts_with = "resume")]
    pub webmcp_runtime: Option<PathBuf>,
    /// Advanced: use an explicitly supplied local MCP runtime package.
    #[arg(long, conflicts_with = "resume")]
    pub mcp_runtime: Option<PathBuf>,
    /// Advanced input detection override for saved requests.
    #[arg(long, default_value = "auto", value_parser = ["auto", "curl", "http"])]
    pub input_format: String,
    /// Trusted path template for each saved request, in input order; repeat as needed.
    /// HAR input requires a template for every captured path; each path uses the most
    /// literal matching template.
    #[arg(long, conflicts_with = "method")]
    pub route: Vec<String>,
    /// Shell quoting syntax of a saved cURL command; never executes a shell.
    #[arg(long, default_value = "posix", value_parser = ["posix", "cmd", "powershell"])]
    pub shell_dialect: String,
    /// Advanced: use an explicitly supplied local TypeScript runtime package.
    #[arg(long, conflicts_with = "resume")]
    pub typescript_runtime: Option<PathBuf>,
    /// Add outputs to an existing project while retaining all existing settings.
    /// Edits only the `targets` section and saves the original manifest as
    /// tungsten.yml.init-backup (or the next free `.init-backup.N`) first.
    #[arg(long, requires = "resume", value_delimiter = ',', value_parser = parse_init_target)]
    pub add_targets: Vec<String>,
    /// Acquire project dependencies, build selected outputs and call a loopback mock.
    /// Requires the selected Node, Python or Rust toolchains; only new projects.
    #[arg(long, requires = "generate", conflicts_with_all = ["resume", "force"])]
    pub local_example: bool,
    /// No-schema HTTP method; use a path template with --endpoint, never credentials.
    #[arg(long, requires = "endpoint", conflicts_with_all = ["from", "resume"], value_parser = ["get", "post", "put", "patch", "delete", "head", "options"])]
    pub method: Option<String>,
    /// No-schema path template, for example /pets/{id}; not a live URL.
    #[arg(long, requires = "method")]
    pub endpoint: Option<String>,
    /// JSON request example file; only its structure is persisted.
    #[arg(long, requires = "method")]
    pub request_example: Option<PathBuf>,
    /// JSON response example file; only its structure is persisted (status 200).
    #[arg(long, requires = "method")]
    pub response_example: Option<PathBuf>,
    /// Start the guided terminal setup (requires a terminal, incompatible with JSON).
    #[arg(long, conflicts_with = "no_interactive")]
    pub interactive: bool,
    /// Never prompt, including when attached to a terminal.
    #[arg(long)]
    pub no_interactive: bool,
    /// Generate configured outputs after initializing, without acquiring dependencies.
    #[arg(long)]
    pub generate: bool,
    /// Generate from existing manifests without overwriting reviewed settings.
    #[arg(long, requires = "generate", conflicts_with_all = ["force", "from", "targets", "name"])]
    pub resume: bool,
    /// API description to start from: an OpenAPI 3.0 or 3.1 document, a
    /// Swagger 2.0 document, a Postman Collection v2.1 JSON file, a saved
    /// cURL command, an HTTP request file (.http/.rest), a HAR 1.2 capture
    /// or an endpoint manifest. Saved requests and captures need explicit
    /// --route templates. Accepts a local path, HTTP(S) URL or
    /// git+https://host/repository@ref#path (also git+file://).
    /// Default: openapi.json, openapi.yaml, openapi.yml,
    /// swagger.json or swagger.yaml in the target directory.
    #[arg(long, value_name = "SPEC")]
    pub from: Option<PathBuf>,
    /// Directory to write the manifests into (created if missing).
    #[arg(long, value_name = "DIR", default_value = ".")]
    pub dir: PathBuf,
    /// API machine name: lowercase letters, digits and underscores, starting
    /// with a letter. Default: derived from the spec title.
    #[arg(long, value_parser = parse_api_name)]
    pub name: Option<String>,
    /// Targets to configure, comma-separated or repeated (ts, py, rs aliases).
    /// Default: typescript,python,docs. WebMCP also configures TypeScript.
    #[arg(long, value_name = "TARGETS", value_delimiter = ',', value_parser = parse_init_target)]
    pub targets: Vec<String>,
    /// Overwrite existing tungsten.yml and agent.yml.
    #[arg(long)]
    pub force: bool,
}

/// Longest accepted API machine name.
const MAX_NAME_LEN: usize = 64;

fn parse_api_name(s: &str) -> Result<String, String> {
    let valid = s.len() <= MAX_NAME_LEN
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if valid {
        Ok(s.to_string())
    } else {
        Err(format!(
            "expected lowercase letters, digits and underscores starting with a letter \
             (at most {MAX_NAME_LEN} characters)"
        ))
    }
}

/// Optional project prerequisite checks; bare doctor remains informational.
#[derive(Debug, Args)]
pub(crate) struct DoctorArgs {
    /// Project directory or tungsten manifest to inspect without network access.
    pub path: Option<PathBuf>,
    /// Fail when the project's required tools are missing or unsupported.
    /// Uses the current directory when PATH is omitted.
    #[arg(long)]
    pub strict: bool,
}

fn parse_init_target(s: &str) -> Result<String, String> {
    let target = crate::targets::canonical(s.trim());
    if target == "cli" || tungsten_config::KNOWN_TARGETS.contains(&target) {
        Ok(target.to_string())
    } else {
        Err(format!(
            "unknown target `{s}`; choose {}",
            tungsten_config::KNOWN_TARGETS.join(", ")
        ))
    }
}
