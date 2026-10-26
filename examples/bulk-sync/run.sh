#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Run from any directory. Requires tungsten, uv and Python 3.12.
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1
example_dir=$(cd -- "$(dirname -- "$0")" && pwd)
source_dir=$(cd -- "$example_dir/../.." && pwd)
workspace=${1:?Usage: run.sh NEW_WORKSPACE_DIRECTORY}
port=${BULK_SYNC_PORT:-8099}
mkdir -- "$workspace"
cd -- "$workspace"
cp "$example_dir/openapi.json" openapi.json
printf '%s\n' 'tungsten: 1' 'api: {name: records, title: Records}' 'inputs:' \
  '  - namespace: api' '    spec: openapi.json' 'targets:' \
  '  python: {package: records-sdk, module: records_sdk, out: generated/python}' > tungsten.yml
tungsten check
tungsten generate
tungsten generate --check
uv_binary=$(command -v uv)
python3 -I "$source_dir/crates/tungsten-cli/src/commands/bootstrap_python.py" --check-uv "$uv_binary"
"$uv_binary" venv --python 3.12 .venv
"$uv_binary" pip install --python .venv/bin/python "$source_dir/runtimes/python" ./generated/python
.venv/bin/python "$example_dir/server.py" "$port" > server.log 2>&1 &
server_pid=$!
trap 'kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true' EXIT
# Wait for the local server to listen, failing promptly if the process exits.
for attempt in {1..50}; do
  kill -0 "$server_pid"
  if .venv/bin/python -c "import socket; socket.create_connection(('127.0.0.1',$port), timeout=.2).close()" 2>/dev/null; then break; fi
  sleep .1
done
.venv/bin/python "$example_dir/sync.py" "http://127.0.0.1:$port"
