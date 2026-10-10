// SPDX-License-Identifier: Apache-2.0
//! The clap command tree, built with the builder API from the table.

use std::collections::{BTreeMap, BTreeSet};

use clap::builder::{PossibleValuesParser, ValueParser};
use clap::{Arg, ArgAction, Command};
use tungsten_runtime::Safety;

use crate::spec::{CliFlag, CliSpec, FlagKind};
use crate::values::{check_text, id_of};

/// What a command of the tree runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Leaf {
    Op(usize),
    Macro(usize),
}

pub(crate) struct Tree {
    pub command: Command,
    pub leaves: BTreeMap<Vec<String>, Leaf>,
}

struct Node {
    name: String,
    kids: Vec<Node>,
    leaf: Option<Leaf>,
}

fn insert(nodes: &mut Vec<Node>, path: &[String], leaf: Leaf) {
    let Some((head, rest)) = path.split_first() else {
        return;
    };
    let at = match nodes.iter().position(|n| n.name == *head) {
        Some(i) => i,
        None => {
            nodes.push(Node {
                name: head.clone(),
                kids: Vec::new(),
                leaf: None,
            });
            nodes.len() - 1
        }
    };
    if rest.is_empty() {
        nodes[at].leaf = Some(leaf);
    } else {
        insert(&mut nodes[at].kids, rest, leaf);
    }
}

fn first_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

fn global(name: &'static str, value: Option<&'static str>, help: &'static str) -> Arg {
    let a = Arg::new(name).long(name).global(true).help(help);
    match value {
        Some(v) => a.value_name(v).action(ArgAction::Set),
        None => a.action(ArgAction::SetTrue),
    }
}

fn exit_codes() -> &'static str {
    "Exit codes:\n  0  success\n  1  internal error\n  2  usage or validation failure \
     (nothing was sent)\n  3  API error, not retryable\n  4  API error, retryable\n  5  \
     outcome unknown (check before retrying)\n  6  confirmation required (preview printed)\n  \
     7  credentials missing or malformed (nothing was sent)"
}

fn flag_help(f: &CliFlag, prefix: &str) -> String {
    let mut help = f.help.trim().to_string();
    let hint = match &f.kind {
        FlagKind::Json => Some(format!(
            "JSON text, @file.json, or - for standard input; or one member at a time, --{}.NAME VALUE",
            f.flag
        )),
        FlagKind::File => Some("@path of a file".to_string()),
        FlagKind::Array(inner) if **inner == FlagKind::Json => {
            Some("JSON text or @file.json; repeatable".to_string())
        }
        FlagKind::Array(_) => Some("repeatable".to_string()),
        _ => None,
    };
    if let Some(h) = hint {
        help = if help.is_empty() {
            h
        } else {
            format!("{help} ({h})")
        };
    }
    if f.sensitive {
        let name = crate::config::upper_words(&f.arg);
        help.push_str(&format!(
            " [secret: pass `-` to read it from standard input or `env:NAME`; or set {prefix}_{name}]"
        ));
    }
    help
}

/// What the commands are built with.
struct Build<'a> {
    /// The environment variable prefix (named in the help of secrets).
    prefix: &'a str,
    /// Flags the command line gave dotted members to: they are not required
    /// here, the members can satisfy them (checked after).
    relaxed: &'a BTreeSet<String>,
}

fn flag_arg(f: &CliFlag, env: &Build<'_>) -> Arg {
    let (prefix, relaxed) = (env.prefix, env.relaxed);
    let (base, many) = match &f.kind {
        FlagKind::Array(inner) => (inner.as_ref(), true),
        k => (k, false),
    };
    let mut arg = Arg::new(id_of(&f.arg))
        .long(f.flag.clone())
        .help(flag_help(f, prefix))
        .action(if many {
            ArgAction::Append
        } else {
            ArgAction::Set
        })
        .value_name(f.flag.to_ascii_uppercase().replace('-', "_"))
        .required(f.required && !f.sensitive && !relaxed.contains(&f.flag));
    if f.sensitive {
        // Not validated here: clap would repeat a rejected value in its
        // message, and the value of a sensitive flag must never be echoed.
        return arg.value_name("-|env:NAME");
    }
    match base {
        FlagKind::Enum(values) => {
            arg = arg.value_parser(PossibleValuesParser::new(values.clone()));
        }
        FlagKind::Boolean => {
            arg = arg
                .num_args(0..=1)
                .default_missing_value("true")
                .value_name("BOOL")
                .value_parser(PossibleValuesParser::new(["true", "false"]));
        }
        other => {
            let kind = other.clone();
            arg = arg.value_parser(ValueParser::new(move |s: &str| {
                check_text(&kind, s).map(|()| s.to_string())
            }));
            let name = match other {
                FlagKind::Integer => Some("INT"),
                FlagKind::Number => Some("NUMBER"),
                FlagKind::Json => Some("JSON"),
                FlagKind::File => Some("@FILE"),
                _ => None,
            };
            if let Some(n) = name {
                arg = arg.value_name(n);
            }
            if matches!(other, FlagKind::Integer | FlagKind::Number) {
                arg = arg.allow_negative_numbers(true);
            }
        }
    }
    arg
}

fn leaf_command(
    env: &Build<'_>,
    name: &str,
    about: &str,
    flags: &[CliFlag],
    safety: Safety,
    body: bool,
    paginated: bool,
) -> Command {
    let mut cmd = Command::new(name.to_string()).about(first_line(about).to_string());
    if about.trim().lines().count() > 1 {
        cmd = cmd.long_about(about.trim().to_string());
    }
    for f in flags {
        cmd = cmd.arg(flag_arg(f, env));
    }
    if body {
        cmd = cmd.arg(
            Arg::new("body")
                .long("body")
                .value_name("@FILE|-")
                .action(ArgAction::Set)
                .help("The request body: @file.json, - for standard input, or JSON text"),
        );
    }
    if paginated {
        cmd = cmd.arg(
            Arg::new("all")
                .long("all")
                .action(ArgAction::SetTrue)
                .help("Fetch every page and print one array of the items"),
        );
    }
    if safety != Safety::ReadOnly {
        cmd = cmd
            .arg(
                Arg::new("dry-run")
                    .long("dry-run")
                    .action(ArgAction::SetTrue)
                    .help_heading("Safety")
                    .help("Print the request and its effects without sending anything"),
            )
            .arg(
                Arg::new("yes")
                    .long("yes")
                    .action(ArgAction::SetTrue)
                    .help_heading("Safety")
                    .help("Confirm a destructive operation"),
            )
            .arg(
                Arg::new("i-understand")
                    .long("i-understand")
                    .action(ArgAction::SetTrue)
                    .help_heading("Safety")
                    .help("With --yes, confirm an irreversible operation"),
            )
            .arg(
                Arg::new("idempotency-key")
                    .long("idempotency-key")
                    .value_name("KEY")
                    .action(ArgAction::Set)
                    .help_heading("Safety")
                    .help("Idempotency key (a UUIDv4) for operations that need one; reuse it on a retry"),
            )
            .arg(
                Arg::new("verify")
                    .long("verify")
                    .action(ArgAction::SetTrue)
                    .help_heading("Safety")
                    .help("After success, check the effect with the operation's verification hook"),
            );
    }
    let note = match safety {
        Safety::ReadOnly => None,
        Safety::Mutating => Some("Safety: mutating."),
        Safety::Destructive => {
            Some("Safety: destructive. Without --yes the command prints a preview and exits 6.")
        }
        Safety::Irreversible => Some(
            "Safety: irreversible. Without --yes --i-understand the command prints a preview \
             and exits 6.",
        ),
    };
    match note {
        Some(n) => cmd.after_help(n),
        None => cmd,
    }
}

fn group_about(name: &str) -> String {
    if name == "macros" {
        "Run multi-step workflows".to_string()
    } else {
        format!("Commands of {name}")
    }
}

fn to_command(node: &Node, spec: &CliSpec, env: &Build<'_>, path: &mut Vec<String>) -> Command {
    path.push(node.name.clone());
    let cmd = match node.leaf {
        Some(Leaf::Op(i)) => {
            let op = &spec.ops[i];
            leaf_command(
                env,
                &node.name,
                &op.about,
                &op.flags,
                op.safety,
                op.body_arg.is_some(),
                op.paginated,
            )
        }
        Some(Leaf::Macro(i)) => {
            let m = &spec.macros[i];
            leaf_command(env, &node.name, &m.about, &m.flags, m.safety, false, false)
        }
        None => {
            let mut cmd = Command::new(node.name.clone())
                .about(group_about(&node.name))
                .subcommand_required(true)
                .arg_required_else_help(true);
            for kid in &node.kids {
                cmd = cmd.subcommand(to_command(kid, spec, env, path));
            }
            cmd
        }
    };
    path.pop();
    cmd
}

/// The whole command tree of `spec`, which must have passed
/// [`crate::check::validate`].
pub(crate) fn build(spec: &CliSpec, relaxed: &BTreeSet<String>) -> Tree {
    let mut nodes: Vec<Node> = Vec::new();
    let mut leaves = BTreeMap::new();
    for (i, op) in spec.ops.iter().enumerate() {
        insert(&mut nodes, &op.path, Leaf::Op(i));
        leaves.insert(op.path.clone(), Leaf::Op(i));
    }
    for (i, m) in spec.macros.iter().enumerate() {
        insert(&mut nodes, &m.path, Leaf::Macro(i));
        leaves.insert(m.path.clone(), Leaf::Macro(i));
    }
    let mut command = Command::new(spec.bin.clone())
        .about(first_line(&spec.about).to_string())
        .version(spec.version.clone())
        .subcommand_required(true)
        .arg_required_else_help(true)
        .after_help(exit_codes())
        .arg(global(
            "json",
            None,
            "Print exactly one JSON document on standard output",
        ))
        .arg(global("base-url", Some("URL"), "Base URL of the API"))
        .arg(global(
            "profile",
            Some("NAME"),
            "Profile of the config file",
        ))
        .arg(global("config", Some("PATH"), "Path of the config file"))
        .arg(
            global(
                "timeout",
                Some("SECONDS"),
                "Timeout of each request attempt",
            )
            .value_parser(ValueParser::new(|s: &str| {
                s.parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .map(|_| s.to_string())
                    .ok_or_else(|| format!("`{s}` is not a positive number of seconds"))
            })),
        )
        .arg(global("no-color", None, "Never color the output"));
    let mut path = Vec::new();
    let env = Build {
        prefix: &spec.env_prefix,
        relaxed,
    };
    for node in &nodes {
        command = command.subcommand(to_command(node, spec, &env, &mut path));
    }
    command = command
        .subcommand(
            Command::new("schema")
                .about("Print the compact JSON Schema of an operation's arguments")
                .arg(
                    Arg::new("path")
                        .value_name("COMMAND")
                        .num_args(1..)
                        .required(true)
                        .help("The command path (`webhooks create`) or the operation id"),
                )
                .arg(
                    Arg::new("pretty")
                        .long("pretty")
                        .action(ArgAction::SetTrue)
                        .help("Indent the schema"),
                ),
        )
        .subcommand(Command::new("operations").about("List every operation and macro"))
        .subcommand(
            Command::new("explain-error")
                .about("Explain an error category, an API error code or an error envelope")
                .arg(
                    Arg::new("input")
                        .value_name("CODE|-|@FILE")
                        .required(true)
                        .help(
                            "An error category (VALIDATION_FAILED), an API error code, \
                             or an envelope as JSON: `-` reads standard input, `@file` a file",
                        ),
                ),
        )
        .subcommand(
            Command::new("auth")
                .about("Credentials")
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(
                    Command::new("status")
                        .about("Show which credentials are present, never their values"),
                ),
        );
    Tree { command, leaves }
}
