// SPDX-License-Identifier: AGPL-3.0-only
//! The CLI half of the `rust` target: the CLI crate, its command table and
//! the release workflow (enabled by `options.cli`).
//!
//! ```text
//! <cli_package>/Cargo.toml        depends on the SDK crate (path), tungsten-cli-kit
//!                                 and tungsten-runtime, tokio and serde_json
//! <cli_package>/src/main.rs       builds the table and hands it to tungsten_cli_kit::run
//! <cli_package>/src/table.rs      the CliSpec: one command per operation and macro
//! .github/workflows/release.yml   static binaries for Linux (musl), macOS and Windows
//! ```
//!
//! Diagnostics (reported by `supports`): TG0750 a parameter or body that is
//! given as JSON text, TG0751 a resource named like a command of the CLI
//! itself, TG0752 a flag renamed because its name is taken, TG0753 a command
//! path that two commands share. `emit` reports only file errors; the target
//! options (TG0740) are reported by the SDK half.

mod kinds;
mod model;
mod release;
mod table;

use tungsten_core::{Diagnostic, Diagnostics};
use tungsten_emit::{CommentStyle, FileSet, TargetConfig, header};
use tungsten_ir::naming::{self, Case, Role, Target};
use tungsten_ir::{Ident, Ir};

use crate::options::{Dep, Options};

pub(crate) fn supports(ir: &Ir) -> Diagnostics {
    model::build(ir).1
}

/// The client type of the SDK, named as the TypeScript and Python SDKs name
/// theirs (`<Api>Client`).
fn client_type(ir: &Ir) -> String {
    let mut words = ir.api.name.words.clone();
    words.push("client".to_string());
    let ident = Ident {
        wire: words.join(" "),
        words,
    };
    naming::render(&ident, Target::Rust, Role::Type)
}

fn toml_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn dep(name: &str, dep: &Dep) -> String {
    match dep {
        Dep::Version(v) => format!("{name} = {}\n", toml_str(v)),
        Dep::Path(p) => format!("{name} = {{ path = {} }}\n", toml_str(p)),
    }
}

fn cargo_toml(ir: &Ir, opts: &Options, bin: &str, package: &str, kit: &Dep, stamp: &str) -> String {
    let mut s = format!("{stamp}\n\n[package]\n");
    s.push_str(&format!("name = {}\n", toml_str(package)));
    s.push_str(&format!("version = {}\n", toml_str(&opts.version)));
    s.push_str("edition = \"2024\"\nrust-version = \"1.85\"\n");
    s.push_str(&format!(
        "description = {}\n",
        toml_str(&format!(
            "Command-line client for the {} API.",
            ir.api.title
        ))
    ));
    s.push_str(&format!(
        "\n[[bin]]\nname = {}\npath = \"src/main.rs\"\n",
        toml_str(bin)
    ));
    s.push_str("\n[dependencies]\n");
    s.push_str("serde_json = \"1\"\n");
    s.push_str("tokio = { version = \"1\", features = [\"macros\", \"rt-multi-thread\"] }\n");
    s.push_str(&dep("tungsten-cli-kit", kit));
    s.push_str(&dep("tungsten-runtime", &opts.runtime));
    s.push_str(&dep(
        &opts.package,
        &Dep::Path(format!("../{}", opts.package)),
    ));
    s
}

fn main_rs(ir: &Ir, opts: &Options, stamp: &str) -> String {
    format!(
        "{stamp}\n\
         //! Command-line client for the {title} API: the commands are the table in\n\
         //! `table.rs`; `tungsten-cli-kit` parses the flags and runs the SDK.\n\
         \n\
         #[rustfmt::skip]\n\
         mod table;\n\
         \n\
         use std::process::ExitCode;\n\
         \n\
         use {lib}::{client};\n\
         \n\
         #[tokio::main]\n\
         async fn main() -> ExitCode {{\n\
         \x20   let argv = std::env::args_os()\n\
         \x20       .map(|a| a.to_string_lossy().into_owned())\n\
         \x20       .collect();\n\
         \x20   tungsten_cli_kit::run(&table::spec(), argv, {client}::new).await\n\
         }}\n",
        title = ir.api.title.replace('\n', " "),
        lib = opts.lib_name(),
        client = client_type(ir),
    )
}

pub(crate) fn emit(ir: &Ir, cfg: &TargetConfig, out: &mut FileSet) -> Diagnostics {
    let mut diags = Diagnostics::new();
    let (opts, _) = Options::resolve(ir, cfg);
    let Some(cli) = &opts.cli else {
        return diags;
    };
    let (model, _) = model::build(ir);
    let rs_stamp = header(CommentStyle::DoubleSlash, ir);
    let hash_stamp = header(CommentStyle::Hash, ir);
    let words = &ir.api.name.words;
    let about = match ir.api.description.as_deref().map(str::trim) {
        Some(d) if !d.is_empty() => format!(
            "Command-line client for the {} API.\n{}",
            ir.api.title,
            model::sentence(d)
        ),
        _ => format!("Command-line client for the {} API.", ir.api.title),
    };
    let meta = table::Meta {
        header: &rs_stamp,
        bin: &cli.bin,
        about: &about,
        version: &opts.version,
        env_prefix: &naming::to_case(words, Case::ScreamingSnake),
        config_dir: &naming::to_case(words, Case::Kebab),
    };
    let files = [
        (
            format!("{}/Cargo.toml", cli.package),
            cargo_toml(ir, &opts, &cli.bin, &cli.package, &cli.kit, &hash_stamp),
        ),
        (
            format!("{}/src/main.rs", cli.package),
            main_rs(ir, &opts, &rs_stamp),
        ),
        (
            format!("{}/src/table.rs", cli.package),
            table::source(&model, &meta),
        ),
        (
            ".github/workflows/release.yml".to_string(),
            release::workflow(&hash_stamp, &cli.bin, &cli.package),
        ),
    ];
    for (path, text) in files {
        if let Err(e) = out.add(path, text) {
            diags.push(Diagnostic::error("TG0701", format!("rust target: {e}")));
        }
    }
    diags
}
