# Troubleshooting

Each heading below is an error message, or the start of one.
Search this page for the text you see.

## `invalid replay input: block <N> not found`

```text
root check: NOT VERIFIED; RPC calls: 1
Error: invalid replay input: block 999999999 not found
```

The node has no block at that height or with that hash.

Check the number against a block explorer for the same chain.
Check that the RPC URL is for the chain you mean.
A hash must be the full 32 bytes: `0x` and 64 hex digits.

## `invalid replay input: block number: invalid digit found in string`

```text
root check: NOT VERIFIED; RPC calls: 0
Error: invalid replay input: block number: invalid digit found in string
```

This run used `--block latest`.
`--block` takes only `best`, `finalized`, a number or a full hash.

## `parent state may be pruned (use an archive endpoint)`

```text
replay #18596954: fetching state (shared limit: 8 concurrent RPC requests)
root check: NOT VERIFIED; RPC calls: 4
Error: RPC unavailable: state_getReadProof: {"code":4003,"message":"Client error: State already discarded for 0x…"}; parent state may be pruned (use an archive endpoint)
```

The part inside `{…}` is the node's own message, so its words change from node to node.

The node no longer keeps the state of the block's parent.
Most nodes keep only recent state.
Use an [archive node](reference/glossary.md#archive-node) for older blocks, or trace a newer block with `--block finalized`.

## `runtime execution failed (root mismatch, unsupported host function, or invalid block)`

```text
replay #2: loading runtime and preparing state
root check: NOT VERIFIED; RPC calls: 0
Error: /tmp/ftdocs/tampered/artifact.json.zst: runtime execution failed (root mismatch, unsupported host function, or invalid block): Execution aborted due to trap: wasm trap: wasm `unreachable` instruction executed
WASM backtrace:
...
```

This run used a saved block whose header state root was changed.

The runtime stopped while it ran the block.
The usual cause is the root check: the runtime computed a different state root from the header, and its own check stopped it.
polkadot-frame-trace exports nothing in that case.
See [What a replay proves](concepts/what-a-replay-proves.md).

- With `--runtime-override`, the error ends with `the runtime override may be the cause`. Build the override from the exact source of the runtime on chain.
- With `--offline`, the saved block may have been changed. Save it again.
- Otherwise, the block may need something polkadot-frame-trace does not support. Open an issue with the block number and chain.

## `the runtime needs host functions polkadot-frame-trace does not have`

This output comes from a test runtime in the repository that imports one made-up host function, `ext_frame_trace_missing_version_1`.
Run this from the repository root to see it:

```sh
polkadot-frame-trace block --offline fixtures/replay/westend-asset-hub \
  --runtime-override fixtures/wasm/missing-host-function.wasm
```

```text
...
root check: NOT VERIFIED; RPC calls: 0
Error: fixtures/replay/westend-asset-hub/artifact.json.zst: the runtime needs host functions polkadot-frame-trace does not have: env:ext_frame_trace_missing_version_1; a runtime override was executed instead of the authenticated :code; the runtime override may be the cause. polkadot-frame-trace's host functions are fixed when it is built, so this chain needs a polkadot-frame-trace build that adds them. See https://agustinustheodorus.com/polkadot-frame-trace/troubleshooting.html#the-runtime-needs-host-functions-polkadot-frame-trace-does-not-have
```

Without `--runtime-override`, the error has no part about the runtime override.

The runtime calls host functions that polkadot-frame-trace does not have.
The list after the colon names each one.
polkadot-frame-trace has the same host functions as a stable2609 Polkadot SDK omni-node.
They are fixed when polkadot-frame-trace is built, so no flag adds more.
polkadot-frame-trace stops before it runs the block and exports nothing.

Open an issue with the chain name and the list of names from the error.
If you build polkadot-frame-trace yourself, [the developer notes](https://github.com/agustinustheo/polkadot-frame-trace/blob/main/dev/README.md#add-host-functions-for-a-chain) show how to add them.

## `--runtime-override <file>: … not a Wasm module`

```text
Error: --runtime-override /tmp/ftdocs/notwasm.wasm: invalid replay input: not a Wasm module: expected the \0asm magic, plain or inside an SDK-compressed blob
```

Related messages:

```text
Error: reading --runtime-override /tmp/ftdocs/nope.wasm: No such file or directory (os error 2)
error: the argument '--runtime-override <WASM>' cannot be used with '--save <DIR>'
```

The file is not a runtime, does not exist, or you used it with `--save`.
polkadot-frame-trace checks the file before it replays anything.

Pass the `.wasm` or `.compact.compressed.wasm` file from the runtime build, under `target/release/wbuild/`.
Remove `--save` when you use an override.
See [See runtime calls inside extrinsics](guides/see-runtime-calls.md).

## `<N> of <M> spans were not exported`

```text
Error: exporting to http://127.0.0.1:4999: 428 of 428 spans were not exported (first error: Operation failed: HTTP export failed: network error)
```

Nothing listens at the OTLP URL, or it refused the spans.

- Check that Jaeger or your OTLP receiver runs: `docker ps`.
- Check the port. OTLP/HTTP is usually 4318. The Jaeger web page, 16686, does not accept traces.
- Map the port when you start Jaeger: `-p 4318:4318`.

## `<N> of <M> spans rejected by the receiver`

```text
Error: exporting to http://127.0.0.1:4319: 5 of 428 spans rejected by the receiver: span attribute limit exceeded
```

This run used a test receiver that rejects five spans.

The receiver answered, but refused some spans.
The text after the last colon is the receiver's own reason.
polkadot-frame-trace exits with code 1, because the trace in your viewer is not complete.

Read the reason and the receiver's logs.
The usual causes are limits on attributes per span, span size or spans per request.
Raise the limit in the receiver's configuration, or send to a receiver without it, such as a local Jaeger.

## `follow poll failed`

```text
follow poll failed: RPC error: chain_getFinalizedHead: connect ws://127.0.0.1:9: IO error: Connection refused (os error 61); retrying in 2s
follow poll still failing after 6 attempts over 1m06s: RPC error: chain_getFinalizedHead: connect ws://127.0.0.1:9: IO error: Connection refused (os error 61); retrying in 1m00s
```

`follow` could not ask the node for its newest block.
It keeps running and tries again, and waits longer after each failure.
While it fails, it logs the line again once a minute, with the count so far.

Check that the node runs and that the `--rpc` URL and port are right.
When the node is back, `follow` logs `follow poll recovered after <N> failed attempt(s)`.

## `OTLP export failed`

```text
OTLP export failed (1 attempt(s) in a row): 578 of 578 spans were not exported (first error: Operation failed: HTTP export failed: network error); retrying in 1s, 10 trace(s) waiting
```

`follow` replayed a block but could not send its trace.
It keeps the trace and tries again. It does not replay the block, and it does not send acknowledged requests again.
If an acknowledgement is lost, the receiver may get the same spans twice, with the same IDs.
It logs `OTLP export recovered` when the receiver is back.

Start the receiver again, or fix the `--otlp` URL. The steps for [`<N> of <M> spans were not exported`](#n-of-m-spans-were-not-exported) apply.

## `OTLP export queue full`

```text
OTLP export queue full (32 traces); dropped the oldest, block #19025706 0x4c91709d161faccb5df6c0561e7a745f7b27d8d9b6e472c8d60f5e19dd2538ed (1 dropped so far)
```

The receiver stopped responding, and the export queue reached its limit of 32 traces.
To make room for a new trace, it drops the oldest one. The status line counts it as `export-dropped`.
While this goes on, it logs the line again once a minute.

Start the receiver again. Dropped traces are not sent later.
To trace a dropped block, run `polkadot-frame-trace block --rpc <URL> --block <hash> --otlp` with the hash from the line.
This works while the node still has the state before that block.

## No traces in Jaeger, but no error

polkadot-frame-trace printed `exported <N> spans`, but Jaeger shows nothing.

- Pick the service `polkadot-frame-trace` and click **Find Traces**.
- Widen **Lookback**. Traces carry the time you ran polkadot-frame-trace.
- Check that polkadot-frame-trace and the page you opened use the same Jaeger. Jaeger's default in-memory storage loses traces on restart.
- Without `--otlp`, polkadot-frame-trace sends nothing to Jaeger. With neither `--otlp` nor `--json`, `block` prints storage summary tables.

## `notice: local metadata unavailable (…); using unnamed storage and hex results`

```text
notice: local metadata unavailable (saved proof does not cover metadata execution); using unnamed storage and hex results
```

polkadot-frame-trace could not run the runtime's metadata API for this state.
The trace is still complete and checked.
Keys show as `<unknown>` or raw hex, and call results as hex.

For a call, use `--data` instead of `--args`.

## `overall request deadline exceeded`

```text
replay #15366850: fetching state (shared limit: 8 concurrent RPC requests)
root check: NOT VERIFIED; RPC calls: 8
Error: RPC error: state_getReadProof: overall request deadline exceeded
```

This run used `wss://asset-hub-paseo-rpc.n.dwellir.com`.

The node took too long to answer one request.
`--rpc-timeout` is the wait for one try, 30 seconds by default.
A request gets at most four tries, with a total deadline of 2 minutes by default.
The largest answer is the proof of the runtime code, about 2.7 MB.
Public Westend nodes took 7 to 20 seconds to send it, and this node 25 to 35 seconds.

Raise [`--rpc-timeout`](reference/commands.md#block) when one try needs more than 30 seconds.
This run failed with the old default of 7.5 seconds.
With `--rpc-timeout 120`, the same command passed:

```text
root check: PASS; RPC calls: 113 (timed replay: 0); execution proof: 101420 bytes
```

A slow first run without an error is normal. This line can stay on screen for 20 seconds or more:

```text
replay #18598757: fetching state (shared limit: 8 concurrent RPC requests)
```

polkadot-frame-trace fetches, with proofs, every piece of state the block reads, and compiles the runtime.
A Westend Asset Hub block needed 64 rounds of requests in [Your first trace](getting-started/first-trace.md).
To make later runs faster:

- Use a node close to you, or your own node.

Compiled runtimes are kept in `~/.cache/polkadot-frame-trace/wasm`.

## `failed to lookup address information`

```text
root check: NOT VERIFIED; RPC calls: 4
Error: RPC error: chain_getFinalizedHead: connect wss://asset-hub-westend-rpc.dwellir.com: IO error: failed to lookup address information: nodename nor servname provided, or not known
```

This run used `--block finalized`, so the first request was `chain_getFinalizedHead`.
With a block number it would be `chain_getBlockHash`.

The RPC host name does not resolve.
Check the URL for typos and check your network.

## `the argument '…' cannot be used with '…'`

```text
error: the argument '--rpc <URL>' cannot be used with '--offline <DIR>'

Usage: polkadot-frame-trace block --block <NUMBER|HASH|best|finalized> <--rpc <URL>|--offline <DIR>|--from-json <FILE>>
```

Two flags do not work together, so polkadot-frame-trace refuses to run. It does not ignore one of them.
For example, `--block` applies only to `--rpc`.
The exit code is 2.

Remove the flag that does not apply to your source.
[Commands and flags](reference/commands.md) shows which flags go with which source.

## `range limit is 10000 blocks`

```text
Error: range limit is 10000 blocks
Error: range must be ordered, non-genesis and finalized (head #18597087)
```

`report --from --to` got a range that is too long, starts at block 0, runs backwards, or ends after the latest finalized block.
Keep a range to 10,000 blocks or fewer.
Start at block 1 or later.
End at or before the finalized head shown in the message.

## `state_call check: MISMATCH`

With `--check-state-call`, the node's own `state_call` returned different bytes from the replay.
polkadot-frame-trace exits with code 3 after it writes all outputs.

Check that the node runs the same runtime as the block's state.
With `--runtime-override`, a mismatch is expected when the override behaves differently.
