#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Run from any directory. Requires tungsten, uv and Python 3.12.
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1
example_dir=$(cd -- "$(dirname -- "$0")" && pwd)
source_dir=$(cd -- "$example_dir/../.." && pwd)
workspace=${1:?Usage: run.sh NEW_WORKSPACE_DIRECTORY}
python3 "$example_dir/prepare.py" "$workspace"
cd -- "$workspace"
tungsten check
tungsten generate
tungsten generate --check
tungsten ir dump > ir.json
uv_binary=$(command -v uv)
python3 -I "$source_dir/crates/tungsten-cli/src/commands/bootstrap_python.py" --check-uv "$uv_binary"
"$uv_binary" venv --python 3.12 .venv
"$uv_binary" pip install --python .venv/bin/python "$source_dir/runtimes/python" ./generated/python 'fastapi>=0.115,<1' 'uvicorn>=0.30,<1' 'pyright>=1.1.390,<2'
cp "$example_dir/consume.py" consume.py
printf '%s\n' '{"typeCheckingMode":"strict","pythonVersion":"3.12","venvPath":".","venv":".venv","include":["consume.py"]}' > pyrightconfig.json
.venv/bin/pyright
export SSE_API_KEY=example
.venv/bin/python "$example_dir/server.py" upstream.json
.venv/bin/uvicorn --app-dir "$example_dir" server:app --host 127.0.0.1 --port 8098 > server.log 2>&1 &
server_pid=$!
trap 'kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true' EXIT
# Wait for the local app to listen, failing promptly if the process exits.
for attempt in {1..50}; do
  kill -0 "$server_pid"
  if .venv/bin/python -c 'import socket; socket.create_connection(("127.0.0.1",8098), timeout=.2).close()' 2>/dev/null; then break; fi
  sleep .1
done
.venv/bin/python consume.py
