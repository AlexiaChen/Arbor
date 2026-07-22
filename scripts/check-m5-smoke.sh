#!/usr/bin/env bash
set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
smoke_dir=$(mktemp -d "${TMPDIR:-/tmp}/arbor-m5-smoke.XXXXXX")
trap 'rm -rf -- "$smoke_dir"' EXIT

cd "$workspace_root"
cargo build --quiet -p arbor-cli

arbor_bin="$workspace_root/target/debug/arbor"
init_output=$("$arbor_bin" node init --dev --data-dir "$smoke_dir")
[[ "$init_output" == "initialized $smoke_dir/config.toml" ]]

if ! run_output=$(timeout --preserve-status --kill-after=5s --signal=TERM 2s \
  "$arbor_bin" node run --dev-validator --data-dir "$smoke_dir" 2>&1); then
  echo "M5 dev validator did not shut down cleanly after SIGTERM" >&2
  echo "$run_output" >&2
  exit 1
fi
[[ "$run_output" == *"finalized development block"* ]]

inspect_output=$("$arbor_bin" db inspect --data-dir "$smoke_dir")
[[ "$inspect_output" == *"root_reachability=ok roots=1 unhealthy=0"* ]]
finalized_height=$(sed -n 's/^finalized_height=\([0-9][0-9]*\).*/\1/p' <<<"$inspect_output")
[[ -n "$finalized_height" ]]
(( finalized_height >= 1 ))

echo "M5 smoke passed: dev genesis, continuous finality, graceful shutdown, and durable reopen"
