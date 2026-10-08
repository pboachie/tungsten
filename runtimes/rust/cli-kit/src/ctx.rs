// SPDX-License-Identifier: Apache-2.0
//! The state of one run: environment, streams, output mode and the
//! single-use standard input.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use crate::host::Host;
use crate::render::Style;
use crate::spec::CliSpec;

/// Why a command stopped before or without a call.
#[derive(Debug)]
pub(crate) enum Fail {
    /// The command line or the files it names are wrong (exit 2).
    Usage(String),
    /// The table and the SDK disagree, or the output could not be produced
    /// (exit 1).
    Internal(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StdinState {
    Unused,
    /// Some lines were read (secrets); more lines can follow.
    Lines,
    Consumed,
}

pub(crate) struct Ctx<'a> {
    pub spec: &'a CliSpec,
    pub env: BTreeMap<String, String>,
    pub json: bool,
    /// Indent `--json` documents (standard output is a terminal).
    pub pretty: bool,
    pub out_style: Style,
    pub err_style: Style,
    stdin: Box<dyn Read + Send>,
    stdin_state: StdinState,
    stdout: Box<dyn Write + Send>,
    stderr: Box<dyn Write + Send>,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(spec: &'a CliSpec, host: Host, json: bool, no_color: bool) -> Self {
        let color_ok = !no_color
            && host.env.get("NO_COLOR").is_none_or(String::is_empty)
            && host.env.get("TERM").is_none_or(|t| t != "dumb");
        Ctx {
            spec,
            json,
            pretty: host.stdout_tty,
            out_style: Style {
                on: color_ok && host.stdout_tty,
            },
            err_style: Style {
                on: color_ok && host.stderr_tty,
            },
            env: host.env,
            stdin: host.stdin,
            stdin_state: StdinState::Unused,
            stdout: host.stdout,
            stderr: host.stderr,
        }
    }

    pub(crate) fn disable_color(&mut self) {
        self.out_style = Style { on: false };
        self.err_style = Style { on: false };
    }

    /// A line on standard output. A closed pipe is not an error of the
    /// command.
    pub(crate) fn out(&mut self, text: &str) {
        let _ = writeln!(self.stdout, "{text}");
    }

    pub(crate) fn err(&mut self, text: &str) {
        let _ = writeln!(self.stderr, "{text}");
    }

    pub(crate) fn finish(&mut self) {
        let _ = self.stdout.flush();
        let _ = self.stderr.flush();
    }

    /// All of standard input, once.
    pub(crate) fn stdin_all(&mut self) -> Result<Vec<u8>, Fail> {
        if self.stdin_state != StdinState::Unused {
            return Err(Fail::Usage(
                "standard input is read by more than one option".into(),
            ));
        }
        self.stdin_state = StdinState::Consumed;
        let mut buf = Vec::new();
        self.stdin
            .read_to_end(&mut buf)
            .map_err(|e| Fail::Usage(format!("cannot read standard input: {e}")))?;
        Ok(buf)
    }

    /// One line of standard input, without its line ending.
    pub(crate) fn stdin_line(&mut self) -> Result<String, Fail> {
        if self.stdin_state == StdinState::Consumed {
            return Err(Fail::Usage(
                "standard input is read by more than one option".into(),
            ));
        }
        self.stdin_state = StdinState::Lines;
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match self.stdin.read(&mut byte) {
                Ok(0) => break,
                Ok(_) if byte[0] == b'\n' => break,
                Ok(_) => line.push(byte[0]),
                Err(e) => return Err(Fail::Usage(format!("cannot read standard input: {e}"))),
            }
        }
        let mut text = String::from_utf8(line)
            .map_err(|_| Fail::Usage("standard input is not valid UTF-8".into()))?;
        if text.ends_with('\r') {
            text.pop();
        }
        Ok(text)
    }
}
