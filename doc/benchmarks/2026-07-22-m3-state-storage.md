# M3 production state/storage benchmark — 2026-07-22

This is an engineering baseline, not a throughput promise. It measures the M3
production implementation rather than the disposable M0 spike.

Environment:

- `rustc 1.97.0 (2d8144b78 2026-07-07)`, release profile;
- Linux 6.18.33.2 WSL2, x86_64;
- parity-db 0.5.5 with `sync_wal = true` and `sync_data = true`;
- 100,000 deterministic 32-byte values, then 1,000 updates.

Command:

```bash
cargo run --release -p arbor-storage --example m3_benchmark
```

Observed result:

```text
root1=0xd33a2652aa6db8e5a422b3033d6d3153c613d2ae64ffb9f9cb2e22b98ffbe846
root2=0x597f85abbb7d8acb202a5a1936befa09da5f5ed2463837b7a1d14d71a3e49798
initial_build_ms=256 initial_write_ms=530
initial_nodes=138636 initial_node_bytes=11775713
update_build_ms=257 update_write_ms=700
incremental_nodes=3022 incremental_node_bytes=671176
logical_update_bytes=32000 write_amplification=20.97
disk_bytes=84954904
```

The production representation stores every standard RLP node required for
root traversal and arbitrary proof construction, plus application schema,
reachability manifests, and flat-cache data. Its write amplification and disk
size therefore must not be compared directly with the M0 spike, which retained
only compact branch/proof material for selected targets.
