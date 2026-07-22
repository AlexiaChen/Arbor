#!/usr/bin/env bash
set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
smoke_dir=$(mktemp -d "${TMPDIR:-/tmp}/arbor-m6-smoke.XXXXXX")
trap 'rm -rf -- "$smoke_dir"' EXIT

cd "$workspace_root"
cargo build --quiet -p arbor-cli

arbor_bin="$workspace_root/target/debug/arbor"
init_output=$("$arbor_bin" node init --dev --data-dir "$smoke_dir")
[[ "$init_output" == "initialized $smoke_dir/config.toml" ]]

smoke_output=$("$arbor_bin" chain m6-smoke --data-dir "$smoke_dir")
[[ "$smoke_output" == *"m6_acceptance=ok"* ]]
[[ "$smoke_output" == *"finalized_height=3"* ]]
[[ "$smoke_output" == *"child_proof=ok"* ]]
[[ "$smoke_output" == *"grandchild_proof=ok"* ]]

child_root=$(sed -n 's/^child_proof=ok root=//p' <<<"$smoke_output")
grandchild_root=$(sed -n 's/^grandchild_proof=ok root=//p' <<<"$smoke_output")
[[ -n "$child_root" && "$child_root" == "$grandchild_root" ]]

inspect_output=$("$arbor_bin" db inspect --data-dir "$smoke_dir")
[[ "$inspect_output" == *"finalized_height=3"* ]]
[[ "$inspect_output" == *"root_reachability=ok roots=3 unhealthy=0"* ]]

echo "M6 smoke passed: two-level domains, dual-domain contracts, shared finalized proof root, and durable reopen"
