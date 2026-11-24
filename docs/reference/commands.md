# Commands and flags

This page lists every command and flag, taken from `polkadot-frame-trace <command> --help`.

| Command | Does |
| --- | --- |
| [`block`](#block) | Trace storage use in one block and export it to JSON or a trace viewer. |
| [`call`](#call) | Trace storage use and the result of a runtime API call. |
| [`report`](#report) | Compare storage use across saved blocks or a range of finalized blocks. |
| [`diff`](#diff) | Compare two block traces and check regression budgets. |
| [`follow`](#follow) | Trace new finalized blocks as they arrive (experimental). |

## Sources

`block` needs exactly one source:

| Flag | Source |
| --- | --- |
| `--rpc <URL>` with `--block <NUMBER\|HASH\|best\|finalized>` | Replay a block from a node. |
| `--offline <DIR>` | Replay a block saved with `--save`, with no network. |
| `--from-json <FILE>` | Read saved `block --json` output without replaying. |

## block

```sh
polkadot-frame-trace block --rpc URL --block finalized --otlp
polkadot-frame-trace block --offline fixtures/replay/people-traced --json block.json
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--rpc <URL>` | | Node RPC endpoint. See [supported URLs](support.md#nodes). |
| `--block <NUMBER\|HASH\|best\|finalized>` | | Block to trace with `--rpc`: a block number, a full hash, `best` (newest) or `finalized` (newest finalized). |
| `--offline <DIR>` | | Replay a saved block with no network. |
| `--from-json <FILE>` | | Read saved `block --json` output without replay. |
| `--save <DIR>` | | Save the block, its state and runtime for offline replay. |
| `--otlp [<URL>]` | `http://127.0.0.1:4318` when given without a URL | Send the trace over OTLP/HTTP. |
| `--json <FILE>` | | Write the trace as JSON. `-` writes to stdout. |
| `--top <N>` | 15 | Rows in each item, extrinsic and hook table, and per phase in the duplicate reads table. |
| `--rpc-timeout <SECONDS>` | 30 | Longest wait for one RPC response, more than 0 and at most 3600 seconds. A request gets four tries, so it can take four times this, 2 minutes by default. Raise it for a node that takes longer to send a large proof. |
| `--runtime-override <WASM>` | | Run this Wasm instead of the chain's runtime. See [See runtime calls inside extrinsics](../guides/see-runtime-calls.md). Cannot be combined with `--save`. |

Without `--otlp` or `--json`, `block` prints storage summary tables.

It prints the tables **Storage per phase**, **Phase shares**, **Top items by proof bytes** and **Extrinsic distributions**.
Then **Hooks**, only with runtime spans, and **Duplicate reads per phase**.
[Find the heaviest storage items](../guides/read-the-tables.md#find-the-heaviest-storage-items) explains the columns.

A flag that does not apply to the source fails with exit code 2.
`--block` and `--rpc-timeout` need `--rpc`.

Downloaded state stays in memory only.
Compiled runtimes are kept in `$XDG_CACHE_HOME/polkadot-frame-trace/wasm`, or `~/.cache/polkadot-frame-trace/wasm`.
The newest four compiled runtimes are kept; runtimes still loaded by workers also stay.
The cache must be private and owned by you, because it contains native code.
Without both `XDG_CACHE_HOME` and `HOME`, they go to `polkadot-frame-trace-<uid>/wasm` in the temporary directory.

These limits protect against a node that sends too much. They apply to every command that uses `--rpc`.

| Limit | Value | Meaning |
| --- | --- | --- |
| One RPC response | 64 MiB | A larger response fails the request at once, without another try. The largest real one, the proof of the runtime, is about 5.5 MB. |
| State the node sends for one block | 1,000,000 pieces or 256 MiB | More fails the block. Recent blocks of Polkadot, Kusama, Westend and their Asset Hubs, and a Westend Asset Hub runtime upgrade, needed at most 3,656 pieces and 5.6 MB. |
| One replay | 10 minutes | A runtime that never finishes cannot be stopped from inside polkadot-frame-trace. `block`, `call` and `report` warn once after 10 minutes of elapsed replay time, including RPC waits, and keep going: press Ctrl-C to stop them. `follow` exits with code 1 when a replay runs 10 minutes without RPC activity, see [follow](#follow). Real blocks take seconds, and compiling a new runtime about a minute. |

Output shows `--rpc` and `--otlp` URLs as scheme, host and port only.
A user name, password or API key in the URL stays out of logs.
Text from the node or the OTLP receiver in error lines is cut to 512 characters.
A plain `ws://` or `http://` URL to another machine prints one warning, because anyone on the way can read and change what is sent. `localhost` and loopback addresses do not.

## call

```sh
polkadot-frame-trace call --rpc URL --method Core_version --data 0x --check-state-call
polkadot-frame-trace call --offline fixtures/replay/call-people-nonce
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--rpc <URL>` | | Node RPC endpoint. |
| `--at <AT>` | `best` | Block whose state the call reads: a number, a full hash, `best` or `finalized`. |
| `--method <METHOD>` | | Runtime API function, for example `AccountNonceApi_account_nonce`. Required with `--rpc`. |
| `--data <DATA>` | | SCALE-encoded arguments in hex. `0x` for none. |
| `--args <ARGS>` | | Arguments as JSON: a positional array or a named object. Needs metadata V15 or V16. |
| `--offline <DIR>` | | Replay a saved call with no network. |
| `--save <DIR>` | | Save the call and its state for offline replay. |
| `--check-state-call` | off | Compare the result with the node's `state_call` at the same block. |
| `--json <FILE>` | | Write the trace and result as JSON. `-` writes to stdout and sends tables to stderr. |
| `--otlp [<URL>]` | `http://127.0.0.1:4318` when given without a URL | Send the trace over OTLP/HTTP. |
| `--rpc-timeout <SECONDS>` | 30 | Longest wait for one RPC response, more than 0 and at most 3600 seconds. A request gets four tries, so it can take four times this, 2 minutes by default. Raise it for a node that takes longer to send a large proof. |
| `--runtime-override <WASM>` | | As for `block`. |

`call` prints storage summary tables by default. With `--json -`, the tables go to stderr.
With `--rpc`, give `--method` and exactly one of `--data` or `--args`.

## report

```sh
polkadot-frame-trace report block-1.json block-2.json
polkadot-frame-trace report --rpc URL --last 10
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `[BLOCK.json]...` | | Saved `block --json` files. |
| `--rpc <URL>` | | Replay a range of finalized blocks from this node. |
| `--from <N>` | | First block of the range. Not the genesis block. |
| `--to <N>` | | Last block of the range, included. At most the latest finalized block. |
| `--last <K>` | | The K most recent finalized blocks. |
| `--workers <N>` | 2 | Blocks replayed at once, 1 to 4. |
| `--rpc-timeout <SECONDS>` | 30 | Longest wait for one RPC response, more than 0 and at most 3600 seconds. A request gets four tries, so it can take four times this, 2 minutes by default. Raise it for a node that takes longer to send a large proof. |
| `--top <N>` | 15 | Rows in each ranked table. JSON is always complete. |
| `--sort-by <proof\|ops\|time>` | `proof` | Rank items, extrinsics and hooks by proof bytes, operations or storage time. |
| `--json <FILE>` | | Write the report as JSON. `-` writes it to stdout and the tables to stderr. |

A range holds at most 10,000 blocks.
`--workers` and `--rpc-timeout` need `--rpc`; with saved files they are refused.

## diff

Compare [proof bytes](../concepts/proof-bytes.md) and storage operations between two block traces.
The tables show totals, [phases](../concepts/phases.md) and changed storage items.

Run these commands from the repository root.

```sh
polkadot-frame-trace block --offline fixtures/replay/people-transfer --json before.json
```

```text
root check: PASS; RPC calls: 0 (timed replay: 0); execution proof: 6479 bytes
...
```

```sh
polkadot-frame-trace block --offline fixtures/replay/people-traced --json after.json
```

```text
root check: PASS; RPC calls: 0 (timed replay: 0); execution proof: 6586 bytes
...
```

```sh
polkadot-frame-trace diff before.json after.json --max-proof-delta 0 --json diff.json
```

```text
Overview
...
│ proof bytes                    ┆       6479 ┆       6586 ┆       +107 ┆    +1.7% │
...
regression: proof bytes delta +107 exceeds 0 (--max-proof-delta)
```

Use two saved `block --json` files or replay one saved block with two runtimes.
Extract the traced People runtime from the fixture with `zstd`, `jq` and `xxd`.

```sh
zstd -dc fixtures/replay/people-traced/artifact.json.zst |
  jq -r '.proof_nodes | max_by(length) | ltrimstr("0x")' |
  xxd -r -p > candidate.wasm
```

This writes `candidate.wasm` in the repository root.

```sh
polkadot-frame-trace diff --offline fixtures/replay/people-transfer --runtime-override candidate.wasm
```

```text
root check: PASS; RPC calls: 0 (timed replay: 0); execution proof: 6479 bytes
...
root check: PASS with RUNTIME OVERRIDE; RPC calls: 0 (timed replay: 0); execution proof: 6479 bytes
...
```

The first replay uses the chain's runtime. The second replay uses `candidate.wasm`.
The candidate must reach the same state root as the block header, or the replay fails with exit code 1.
Use `--offline` with `--runtime-override` for tracing, read-path or optimization changes that preserve final state.

| Input or flag | Default | Meaning |
| --- | --- | --- |
| `<BEFORE.json> <AFTER.json>` | | Two traces from `block --json`. |
| `--offline <DIR>` | | Replay one saved block twice with no network. Requires `--runtime-override`. |
| `--runtime-override <WASM>` | | Use this runtime for the second replay. Requires `--offline`. |
| `--top <N>` | 15 | Maximum item rows and maximum extrinsic rows in the tables. |
| `--sort-by <proof\|ops\|reads\|duplicates>` | `proof` | Rank items and select extrinsics by absolute change in proof bytes, operations, reads or duplicate reads. |
| `--json <FILE>` | | Write all phase and changed item deltas as JSON. `-` writes to stdout and sends tables to stderr. |
| `--max-proof-delta <BYTES>` | | Maximum increase in proof bytes. |
| `--max-ops-delta <N>` | | Maximum increase in storage operations. |
| `--max-duplicate-reads-delta <N>` | | Maximum increase in duplicate reads. |

Each budget compares the after total minus the before total with the limit.
Use `0` to require no increase or a negative limit to require a decrease.
When a delta exceeds its limit, `diff` prints a `regression:` line to stderr and exits with code 4.
It still prints the tables and writes any requested JSON.

The block proof total uses `execution.proof_bytes` when both traces have it, and complete operation proof sums otherwise.
If either input lacks proof data, `diff` prints `n/a` or `(partial)` and exits with code 1 when `--max-proof-delta` is set.
If `execution.proof_bytes` differs from the attributed operation sum, `diff` warns on stderr and uses `execution.proof_bytes`.
The change column shows `—` when the before value is zero or when proof or timing data is unavailable.
The tables show [local replay timings](../concepts/timings.md) in microseconds. JSON uses nanoseconds.

## follow

> **Warning:** `follow` is experimental. Long runs may grow in memory or hit public RPC rate limits. Its flags and behavior may change or be removed.

```sh
polkadot-frame-trace follow --rpc URL --otlp
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--rpc <URL>` | | Node RPC endpoint. Required. See [supported URLs](support.md#nodes). |
| `--otlp [<URL>]` | `http://127.0.0.1:4318` when given without a URL | Send each trace over OTLP/HTTP. |
| `--workers <N>` | 8 | Blocks replayed at once, 1 to 16. |
| `--rpc-timeout <SECONDS>` | 30 | Longest wait for one RPC response, more than 0 and at most 3600 seconds. A request gets four tries, so it can take four times this, 2 minutes by default. Raise it for a node that takes longer to send a large proof. |

| Limit | Value | Meaning |
| --- | --- | --- |
| Waiting blocks | 4 × `--workers`, at least 8 | Blocks queued for replay. A block older than this many recent heights is skipped. |
| Check for new blocks | every 2 s | How often `follow` asks the node for its newest block. |
| Wait after a failed check | 2 s, doubling to 60 s | Resets after the next check that works. |
| Attempts per block | 4 | For temporary failures, such as a timeout. Other failures stop the block at once. |
| Status line | on change, and every 60 s | Printed at once when `retrying`, `stopped`, `skipped`, `export-rejected` or `export-failed` changes. A check for new blocks that waits on the node delays it. |
| Repeated failure lines | first, then at most once a minute | For failed checks, failed exports and a full queue. A failure that comes back within the minute is not logged again. |
| State kept in memory | 128 MiB | Downloaded state shared by all workers. Counts an upper estimate of what each piece and its index take in memory, so the real use is lower. When full, the least recently read piece goes first. |
| Traces waiting for export | 32 | Held while the receiver is down. Each holds all spans of one block in memory. When full, the oldest is dropped. |
| Spans per export request | 512 | An acknowledged request is never sent again. A retry sends only the others. A lost acknowledgement can deliver the same spans twice. |
| Wait between export tries | 1 s, doubling to 30 s | Each request takes at most the OTLP timeout. The block is not replayed again. |
| Wait at stop | 10 s for replays, then 10 s for exports | See below. |
| Replay deadline | 10 minutes | A replay that runs the runtime this long without waiting on the node stops `follow` with exit code 1, so a supervisor such as systemd starts a fresh process. Time waiting on the node does not count. |

`follow` stops on Ctrl-C, `SIGTERM` or `SIGHUP`. `SIGHUP` does not reload anything.
At the stop, replays waiting on the node end at once and count as `cancelled`.
Replays running on the CPU get the stop wait to finish. Those still running after it count as `cancelled`.
Then `follow` sends waiting traces and prints a summary.
A second signal exits at once.

| Exit code | Meaning |
| --- | --- |
| 0 | Stopped by a signal. Failed blocks and exports are counted in the summary, not in the exit code. |
| 1 | `follow` could not start, for example a malformed `--rpc` or `--otlp` URL, or its replay or export threads stopped unexpectedly, or a replay passed the replay deadline, or it could no longer write to stderr. |
| 2 | The command line was wrong. |
| 130 | A second signal arrived during the stop. |

## Environment variables

These standard OpenTelemetry variables change how `--otlp` sends traces.

| Variable | Default | Meaning |
| --- | --- | --- |
| `OTEL_EXPORTER_OTLP_HEADERS` | none | Headers sent with every export, as `key=value` pairs separated by commas. Values are URL-decoded, so write a space as `%20`. |
| `OTEL_EXPORTER_OTLP_TRACES_HEADERS` | none | The same. Replaces `OTEL_EXPORTER_OTLP_HEADERS` when both are set. |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | 10000 | Longest time for one export request, in milliseconds. Covers connecting and the whole response. A trace of several requests can take several times this. |
| `OTEL_EXPORTER_OTLP_TRACES_TIMEOUT` | 10000 | The same. Wins over `OTEL_EXPORTER_OTLP_TIMEOUT`. |

`--otlp` always sets the URL, so `OTEL_EXPORTER_OTLP_ENDPOINT` has no effect.
See [Headers and authentication](../guides/send-traces.md#headers-and-authentication).

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success. `follow` also exits 0 when a signal stops it. |
| 1 | The replay, an input or the export failed. For `report`, at least one input failed. For `follow`, see [its exit codes](#follow). |
| 2 | The command line was wrong, for example a missing or unknown flag. |
| 3 | `call --check-state-call` found a different result from the node's. JSON, save and export still ran. |
| 4 | A `diff` regression budget failed. Tables and any requested JSON are written first unless output fails. |
| 130 | `follow` got a second stop signal and exited at once. |
