// SPDX-License-Identifier: AGPL-3.0-only
//! Verify the generated CLI, whose dispatch goes through the generated Rust SDK.
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(super) fn preflight() -> Result<(), &'static str> {
    if super::init_setup::execute(
        "cargo",
        &["--version"],
        Path::new("."),
        Duration::from_secs(5),
    )
    .is_ok()
    {
        Ok(())
    } else {
        Err("Cargo and Rust >=1.88 are required for Rust/CLI local setup")
    }
}

pub(super) fn build(directory: &Path) -> Result<(), String> {
    super::init_setup::execute(
        "cargo",
        &["build", "--workspace", "--target-dir", "target"],
        directory,
        Duration::from_secs(600),
    )
}

fn invoke(binary: &Path, args: &[&str], directory: &Path) -> Result<Vec<u8>, String> {
    let output_path = directory.join(".tungsten-init-output");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)
        .map_err(
            |_| "cannot create local CLI output; preserve any existing setup file before retrying",
        )?;
    let result = (|| {
        let mut command = Command::new(binary);
        command
            .args(args)
            .current_dir(directory)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(file)
            .stderr(Stdio::null());
        // Required by native Windows networking; no credentials or profiles.
        for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let mut child = command
            .spawn()
            .map_err(|_| "cannot start the generated CLI")?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => break,
                Ok(Some(_)) => return Err("generated CLI failed its local check".into()),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("generated CLI timed out".into());
                }
            }
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&output_path)
            .map_err(|_| "cannot read CLI check result")?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "cannot read CLI check result")?;
        if bytes.len() > 1024 * 1024 {
            return Err("CLI check output exceeds limit".into());
        }
        Ok(bytes)
    })();
    let _ = std::fs::remove_file(output_path);
    result
}

pub(super) fn run(
    directory: &Path,
    binary_name: &str,
    base_url: &str,
    operation_id: &str,
) -> Result<(), String> {
    let directory = std::path::absolute(directory).map_err(|_| "cannot resolve Rust output")?;
    let binary = directory
        .join("target/debug")
        .join(format!("{binary_name}{}", std::env::consts::EXE_SUFFIX));
    let operations: serde_json::Value =
        serde_json::from_slice(&invoke(&binary, &["--json", "operations"], &directory)?)
            .map_err(|_| "invalid generated CLI operation list")?;
    let operation = operations
        .as_array()
        .and_then(|ops| {
            ops.iter()
                .find(|op| op["kind"] == "operation" && op["id"] == operation_id)
        })
        .ok_or("generated CLI operation not found")?;
    let path = operation["command"]
        .as_str()
        .ok_or("generated CLI command path missing")?;
    let mut args = vec!["--json", "--base-url", base_url, "--timeout", "5"];
    args.extend(path.split_whitespace());
    if operation["safety"] == "irreversible" {
        args.extend(["--yes", "--i-understand"]);
    } else if operation["safety"] == "destructive" {
        args.push("--yes");
    }
    invoke(&binary, &args, &directory)?;
    Ok(())
}
