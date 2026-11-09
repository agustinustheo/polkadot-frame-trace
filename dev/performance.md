# Performance: current numbers

These are the numbers behind the current defaults. All runs used Rust 1.93.0 release builds on macOS arm64 against `wss://westend-asset-hub-rpc.polkadot.io`, a public node. Endpoint load and block contents change throughput between runs. The round-by-round history is in git, up to commit 2e5c243.

## Follow throughput with 8 workers

`follow` uses 8 workers and 16 RPC slots that all workers share. One cold replay runs first, and the other workers then reuse its state.

Two 120-second windows on 2026-10-26 traced 49 and 38 consecutive blocks, with no blocks skipped. The chain advanced 56 and 46 heights. In the last 60 seconds the runs traced 40 and 37 blocks, so they kept up once startup was done. Peak RSS was 698–752 MB. The old default of 2 workers traced 20 and 18 blocks in the same window and skipped 22–24.

Eight is a judgement, not a measured optimum. A 12-worker run traced 46 blocks, but that window was on a loaded host and cannot separate the worker count from host load.

`follow` limits Wasm compilation to 2 threads. On one block replayed cold three times each way, 2 threads took 26.2–27.0 s and the default 12 threads took 26.0–29.4 s. The cap lowered peak RSS by 71–137 MB.

## Memory of a long follow

All workers share the downloaded state in memory. The limit is 128 MiB, counted as an upper estimate of each piece's real allocation, plus the hash table. When a new piece goes over, the least recently read pieces go first. The table may use at most a quarter of the limit and holds at most 229,376 entries.

Three runs set the limit to 32, 128 and 512 MiB for 60 minutes each. All three read 44–45% of state from memory, with 31.5–37 RPC calls and a 6.3–7.2 s median replay per block. A 16 times larger limit changed nothing, because most misses are pieces that no earlier block read. 128 MiB stays as headroom for chains whose blocks read more state.

On Westend Asset Hub the entry limit binds first, because pieces average about 200 bytes. An 80-minute run reached it at minute 40 and stayed at 99–108 MiB charged. Heap in use was 139–150 MiB. A 150-minute run beside the old code compared the heaps. From minute 60 to 140 the old heap grew by 128 MiB, and the new heap by 72 MiB.

The estimate is high on purpose. On macOS it overcounts pieces of 200–600 bytes by about 43%, and pieces of 1–4 KiB by about 23%.

## Reproduce

Build the binary:

```sh
cargo +1.93.0 build --release --locked
```

Measure `follow` throughput and memory. Stop it with Ctrl-C after the window and read the summary line:

```sh
target/release/polkadot-frame-trace follow --rpc wss://westend-asset-hub-rpc.polkadot.io \
  --workers 8
```
