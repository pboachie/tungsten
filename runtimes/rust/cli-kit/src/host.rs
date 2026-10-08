// SPDX-License-Identifier: Apache-2.0
//! The process surroundings of a run: environment, standard streams and
//! whether they are terminals. [`Host::process`] is the real one; passing
//! another makes a run independent of the process (embedding, tests).

use std::collections::BTreeMap;
use std::fmt;
use std::io::{IsTerminal, Read, Write};

/// What a run reads and writes. Nothing in the kit touches the process
/// environment or the standard streams except through this value.
pub struct Host {
    /// Environment variables (unicode names and values only).
    pub env: BTreeMap<String, String>,
    pub stdin: Box<dyn Read + Send>,
    pub stdout: Box<dyn Write + Send>,
    pub stderr: Box<dyn Write + Send>,
    pub stdin_tty: bool,
    pub stdout_tty: bool,
    pub stderr_tty: bool,
}

impl Host {
    /// The current process.
    pub fn process() -> Host {
        Host {
            env: std::env::vars().collect(),
            stdin: Box::new(std::io::stdin()),
            stdout: Box::new(std::io::stdout()),
            stderr: Box::new(std::io::stderr()),
            stdin_tty: std::io::stdin().is_terminal(),
            stdout_tty: std::io::stdout().is_terminal(),
            stderr_tty: std::io::stderr().is_terminal(),
        }
    }
}

impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Host")
            .field("env", &format_args!("{} variables", self.env.len()))
            .field("stdin_tty", &self.stdin_tty)
            .field("stdout_tty", &self.stdout_tty)
            .field("stderr_tty", &self.stderr_tty)
            .finish_non_exhaustive()
    }
}
