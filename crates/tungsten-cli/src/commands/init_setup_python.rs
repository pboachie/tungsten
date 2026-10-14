// SPDX-License-Identifier: AGPL-3.0-only
//! Build a generated Python package in its own virtual environment.
use std::path::Path;
use std::time::Duration;

pub(super) fn interpreter() -> Result<&'static str, &'static str> {
    for program in ["python3", "python"] {
        if super::init_setup::execute(
            program,
            &["-c", "import sys; sys.exit(sys.version_info < (3,12))"],
            Path::new("."),
            Duration::from_secs(5),
        )
        .is_ok()
        {
            return Ok(program);
        }
    }
    Err("Python >=3.12 with venv and pip is required for Python local setup")
}

fn python(directory: &Path) -> std::path::PathBuf {
    directory.join(if cfg!(windows) {
        ".venv/Scripts/python.exe"
    } else {
        ".venv/bin/python"
    })
}

pub(super) fn build(directory: &Path, runtime: Option<&Path>) -> Result<(), String> {
    let directory =
        std::path::absolute(directory).map_err(|_| "cannot resolve Python project directory")?;
    let directory = directory.as_path();
    use super::init_setup::execute;
    execute(
        interpreter()?,
        &["-m", "venv", ".venv"],
        directory,
        Duration::from_secs(60),
    )?;
    let python = python(directory);
    let python = python
        .to_str()
        .ok_or("Python environment path is not UTF-8")?;
    // Wheel-only downloads avoid arbitrary dependency build hooks. The generated
    // package and explicitly supplied runtime use the known installed backend.
    execute(
        python,
        &[
            "-m",
            "pip",
            "install",
            "--disable-pip-version-check",
            "--only-binary=:all:",
            "hatchling>=1.25",
            "httpx>=0.27,<1",
            "pydantic>=2.10,<3",
        ],
        directory,
        Duration::from_secs(180),
    )?;
    if let Some(runtime) = runtime {
        let runtime = std::path::absolute(runtime).map_err(|_| "cannot resolve Python runtime")?;
        execute(
            python,
            &[
                "-m",
                "pip",
                "install",
                "--disable-pip-version-check",
                "--no-deps",
                "--no-build-isolation",
                runtime.to_str().ok_or("Python runtime path is not UTF-8")?,
            ],
            directory,
            Duration::from_secs(90),
        )?;
    } else {
        execute(
            python,
            &[
                "-m",
                "pip",
                "install",
                "--disable-pip-version-check",
                "--only-binary=:all:",
                "tungsten-runtime>=0.1.0,<0.2.0",
            ],
            directory,
            Duration::from_secs(180),
        )?;
    }
    execute(
        python,
        &[
            "-m",
            "pip",
            "install",
            "--disable-pip-version-check",
            "--no-deps",
            "--no-build-isolation",
            ".",
        ],
        directory,
        Duration::from_secs(90),
    )
}

pub(super) fn run(directory: &Path, base_url: &str, operation_id: &str) -> Result<(), String> {
    let directory =
        std::path::absolute(directory).map_err(|_| "cannot resolve Python project directory")?;
    let directory = directory.as_path();
    super::init_setup::execute(
        python(directory)
            .to_str()
            .ok_or("Python path is not UTF-8")?,
        &["-c", LOCAL_CALL, base_url, operation_id],
        directory,
        Duration::from_secs(20),
    )
}

const LOCAL_CALL: &str = r#"
import importlib, inspect, pathlib, sys, tomllib
import httpx
from tungsten_runtime import ClientOptions
base, operation = sys.argv[1:]
manifest = tomllib.loads(pathlib.Path('pyproject.toml').read_text())
module = manifest['tool']['hatch']['build']['targets']['wheel']['packages'][0]
sdk = importlib.import_module(module.replace('/', '.'))
client_type = next(value for name, value in vars(sdk).items() if inspect.isclass(value) and name.endswith('Client') and not name.startswith('Async'))
class LocalOnly(httpx.BaseTransport):
    def __init__(self): self.inner = httpx.HTTPTransport(trust_env=False)
    def handle_request(self, request):
        expected = httpx.URL(base)
        if (request.url.scheme,request.url.host,request.url.port) != (expected.scheme,expected.host,expected.port):
            raise RuntimeError('non-local request blocked')
        return self.inner.handle_request(request)
    def close(self): self.inner.close()
client = client_type(ClientOptions(base_url=base, transport=LocalOnly(), timeout_ms=5000, retries={'max':0}))
seen = set()
def find(obj, depth=0):
    if depth > 5 or id(obj) in seen: return None
    seen.add(id(obj))
    for name, method in inspect.getmembers(obj, inspect.ismethod):
        if not name.startswith(('preview_', '_')) and f'(`{operation}`)' in (method.__doc__ or ''):
            return obj, name, method
    for name, value in vars(obj).items():
        if name.startswith('_') or name in ('core','macros'): continue
        if hasattr(value,'__dict__'):
            found = find(value,depth+1)
            if found: return found
found = find(client)
if not found: raise RuntimeError('generated operation not found')
owner, name, call = found
opts = {}
preview = getattr(owner,'preview_'+name,None)
if preview:
    result = preview()
    if not result.ok: raise RuntimeError('local preview failed')
    if result.value.get('confirmation_token'): opts['confirm'] = result.value['confirmation_token']
try:
    result = call(opts=opts)
    if not result.ok: raise RuntimeError('local generated call failed')
finally: client.close()
"#;
