#!/usr/bin/env bash
set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
smoke_dir=$(mktemp -d "${TMPDIR:-/tmp}/arbor-m1-smoke.XXXXXX")
trap 'rm -rf -- "$smoke_dir"' EXIT

cd "$workspace_root"
cargo build --quiet -p arbor-cli

arbor_bin="$workspace_root/target/debug/arbor"
init_output=$("$arbor_bin" node init --data-dir "$smoke_dir")
[[ "$init_output" == "initialized $smoke_dir/config.toml" ]]
[[ -f "$smoke_dir/config.toml" ]]

inspect_output=$("$arbor_bin" db inspect --data-dir "$smoke_dir")
[[ "$inspect_output" == *"config_version=1 moniker=arbor-node"* ]]
[[ "$inspect_output" == *"database=not-initialized (storage is introduced in M3)"* ]]

if ! run_output=$(timeout --preserve-status --kill-after=5s --signal=TERM 1s \
  "$arbor_bin" node run --data-dir "$smoke_dir" 2>&1); then
  echo "arbor node run did not shut down cleanly after SIGTERM" >&2
  echo "$run_output" >&2
  exit 1
fi
[[ "$run_output" == *"starting Arbor workspace baseline"* ]]

echo "M1 smoke passed: init, inspect, and graceful SIGTERM shutdown"
