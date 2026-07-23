#!/usr/bin/env bash
set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

cd "$workspace_root"

# These are deliberately real loopback listeners. Do not replace them with
# in-memory transports when a restricted sandbox rejects socket binding.
cargo test --quiet -p arbor-network --test m7_block_sync
cargo test --quiet -p arbor-network \
  service::tests::unresponsive_peer_times_out_without_stalling_the_swarm
cargo test --quiet -p arbor-node \
  networked::tests::persistent_listener_snapshot_syncs_restarts_and_catches_up

echo "M7 smoke passed: multi-domain block/snapshot sync, bounded timeout, disconnect/restart catch-up, and durable reopen"
