// SPDX-License-Identifier: AGPL-3.0-only
//! Finding external emitters: on `PATH` or by the configured command.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use tungsten_core::Diagnostic;

use super::{Command, EXECUTABLE_PREFIX};

/// An emitter executable found on the search path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    /// The target name (`go` for `tungsten-emit-go`).
    pub name: String,
    /// The executable.
    pub path: PathBuf,
}

#[cfg(windows)]
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT".into());
    exts.split(';')
        .filter(|e| !e.is_empty())
        .map(|e| dir.join(format!("{name}{e}")))
        .collect()
}

#[cfg(not(windows))]
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    vec![dir.join(name)]
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// The first executable file called `name` in the directories of
/// `search_path` (empty entries are skipped).
pub fn find_executable(name: &str, search_path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(search_path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .flat_map(|dir| candidates(&dir, name))
        .find(|p| is_executable(p))
}

/// Every `tungsten-emit-<name>` executable in `search_path`, sorted by
/// name; when a name occurs in several directories the first wins, as it
/// would when run.
pub fn discover(search_path: &OsStr) -> Vec<Discovered> {
    let mut found: Vec<Discovered> = vec![];
    for dir in std::env::split_paths(search_path).filter(|d| !d.as_os_str().is_empty()) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .collect();
        names.sort();
        for file in names {
            let stem = file.strip_suffix(".exe").unwrap_or(&file);
            let Some(name) = stem.strip_prefix(EXECUTABLE_PREFIX) else {
                continue;
            };
            let path = dir.join(&file);
            if is_valid_name(name) && is_executable(&path) && !found.iter().any(|d| d.name == name)
            {
                found.push(Discovered {
                    name: name.to_string(),
                    path,
                });
            }
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Split a configured command line into words. Spaces and tabs separate
/// words; single or double quotes group a word that contains spaces (no
/// escapes). `None` for an unterminated quote or an empty command.
pub fn command_line(text: &str) -> Option<Vec<String>> {
    let mut words = vec![];
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, ' ' | '\t') => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => {
                current.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if started {
        words.push(current);
    }
    (!words.is_empty()).then_some(words)
}

/// Resolve the emitter of target `name`. `configured` is the `external`
/// string of the target (`None`: find `tungsten-emit-<name>` on
/// `search_path`); a program with a slash is taken relative to `base_dir`,
/// any other is looked up on `search_path`. TG0801 when it does not exist.
pub fn locate(
    name: &str,
    configured: Option<&str>,
    base_dir: &Path,
    search_path: &OsStr,
) -> Result<Command, Diagnostic> {
    let not_found = |what: String, help: String| {
        Diagnostic::error(
            "TG0801",
            format!("external emitter for target `{name}` {what}"),
        )
        .with_help(help)
    };
    let Some(configured) = configured else {
        let exe = format!("{EXECUTABLE_PREFIX}{name}");
        return match find_executable(&exe, search_path) {
            Some(program) => Ok(Command {
                program,
                args: vec![],
                display: exe,
            }),
            None => Err(not_found(
                format!("was not found: no `{exe}` on PATH"),
                format!(
                    "install `{exe}`, or set `external: <command>` on target `{name}` in tungsten.yml"
                ),
            )),
        };
    };
    let Some(mut words) = command_line(configured) else {
        return Err(not_found(
            format!("has an invalid command `{configured}` (empty or unbalanced quote)"),
            "write the command as in a shell: program followed by arguments, quoting words that contain spaces".into(),
        ));
    };
    let program = words.remove(0);
    let path = if program.contains('/') || program.contains('\\') {
        let candidate = Path::new(&program);
        let resolved = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            base_dir.join(candidate)
        };
        is_executable(&resolved).then_some(resolved)
    } else {
        find_executable(&program, search_path)
    };
    match path {
        Some(program_path) => Ok(Command {
            program: program_path,
            args: words,
            display: configured.to_string(),
        }),
        None => Err(not_found(
            format!("was not found: `{program}` is not an executable file"),
            "check the `external` command in tungsten.yml (a path is relative to that file; a bare name is looked up on PATH) and its execute permission".into(),
        )),
    }
}
