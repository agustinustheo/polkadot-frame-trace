# Trace a block from any node

polkadot-frame-trace needs only the normal RPC methods every Polkadot SDK node serves.
You do not need your own node, unsafe RPC or a special build.

## Pick the block

`--block` takes one of four values:

| Value | Example | Picks |
| --- | --- | --- |
| `finalized` | `--block finalized` | The newest block the chain has finalized. A good default. |
| `best` | `--block best` | The newest block the node knows. A reorg can still replace it. |
| A number | `--block 1000000` | The block at that height on the node's chain. |
| A hash | `--block 0x8494…c58b` | That exact block. Use the full 32-byte hash. |

Run it in a terminal.
This example traces block 1,000,000 of Westend Asset Hub:

```sh
polkadot-frame-trace block --rpc wss://westend-asset-hub-rpc.polkadot.io --block 1000000
```

```text
replay #1000000: fetching state (shared limit: 8 concurrent RPC requests)
replay #1000000: loading runtime and preparing state
root check: PASS; RPC calls: 17 (timed replay: 0); execution proof: 9840 bytes
fetching state #1000000: 13 proof request rounds; 17 RPC calls
state root: computed 0x614e27580d5a379f5a7a57dd7598df672545859d85d387ae5e5ab7ebfcd9b608; header 0x614e27580d5a379f5a7a57dd7598df672545859d85d387ae5e5ab7ebfcd9b608
```

Without `--otlp` or `--json`, polkadot-frame-trace prints storage summary tables.
Add an output to export the trace:

| Flag | Output |
| --- | --- |
| `--otlp` | Send the trace to Grafana, Jaeger or any OTLP receiver. See [Choose where to send traces](send-traces.md). |
| `--json <file>` | Write the whole trace as JSON. `-` writes to stdout. See [JSON output](../reference/json.md). |
| `--save <dir>` | Keep everything needed to replay the block later with no network. See [Save a block and replay it offline](save-and-replay.md). |

To see tables in the terminal instead, run `block` without `--otlp` or `--json`:

```sh
polkadot-frame-trace block --rpc wss://westend-asset-hub-rpc.polkadot.io --block finalized
```

[Find the heaviest storage items](find-heaviest-items.md) explains its tables.

## Choose an endpoint

The URL can be `ws://`, `wss://`, `http://` or `https://`.
The node must serve the standard methods, including `state_getReadProof`. polkadot-frame-trace uses it for every piece of state.

One Westend Asset Hub block takes about 90 requests, and `report` and `follow` send that many for every block.
Before you trace ranges or follow a chain, check the endpoint's:

- request limits: public endpoints can rate-limit you, and polkadot-frame-trace then retries and slows down;
- pricing, for paid providers that charge per request;
- historical state, if you need old blocks.

For an old block, the node must still have that block's parent state.

> **Warning:** Most nodes keep only recent state. For older blocks, use an archive node. Otherwise the replay fails with `parent state may be pruned (use an archive endpoint)`.

The public Parity endpoints, such as `wss://westend-asset-hub-rpc.polkadot.io`, served block 1,000,000 in the example above.

## Make repeated runs faster

- Add `--cache-dir <dir>` to keep a [disk cache](../reference/commands.md#disk-cache) of downloaded state and the compiled runtime between runs.

Defaults and limits are in [Commands and flags](../reference/commands.md).

## Names and metadata

polkadot-frame-trace decodes keys such as `System::Account(5Grw…)` using the runtime's own metadata.
It gets the metadata by running the runtime's metadata API on your machine, checked like any other state.
Runtimes need metadata V14, V15 or V16, see [What polkadot-frame-trace supports](../reference/support.md).
If that fails, polkadot-frame-trace prints a notice and shows raw keys and hex values.
