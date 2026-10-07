// SPDX-License-Identifier: AGPL-3.0-only
//! The `tungsten` binary: [`tungsten_cli::run`] on the process streams.

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use tungsten_cli::{CliEnv, exit};

fn main() -> ExitCode {
    let env = CliEnv {
        is_tty: std::io::stderr().is_terminal(),
        // https://no-color.org: set and non-empty disables color.
        no_color: std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
        search_path: None,
    };
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    let mut code = tungsten_cli::run(std::env::args_os(), &mut stdout, &mut stderr, &env);
    if stdout.flush().is_err() && code == exit::OK {
        code = exit::INTERNAL;
    }
    ExitCode::from(u8::try_from(code).unwrap_or(u8::MAX))
}
