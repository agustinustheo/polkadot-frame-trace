# Saved block format

`--save <DIR>` writes one file, `<DIR>/artifact.json.zst`.
It is a Zstandard-compressed JSON object.
It holds everything needed to replay the block with no network, and nothing else.
It has no paths to other files. Saving writes a complete temporary file before replacing it.
File errors include the path.

## What is inside

| Field | Meaning |
| --- | --- |
| `format_version` | `1`. Other versions are rejected. |
| `kind` | `block` or `call`. |
| `block_hash` | 0x-prefixed Blake2-256 hash of the original SCALE header, including seals. Checked before seals are removed for execution. |
| `block` | `chain_getBlock.result.block`, with an SDK u32 header and opaque extrinsics. |
| `parent_header` | The parent block's header. Its hash must match the block's parent hash. |
| `runtime_version` | `state_getRuntimeVersion` at the parent/state hash, kept for reference. |
| `metadata` | Metadata in hex, kept for reference. polkadot-frame-trace derives it again from the runtime before decoding. |
| `proof_nodes` | Raw trie nodes and external values in hex, deduplicated by Blake2 hash. They include the runtime code. |
| `call` | For calls only: `method`, hex SCALE `data`, optional hex `result`, and optional hex node `oracle` from `--check-state-call`. |

For a call, `block` and `parent_header` both hold the header of the block whose state the call read, and the block has no extrinsics. `kind` defaults to `block` for legacy v1 files; new saves write it explicitly. Kind/payload mismatches and nonempty call block bodies are rejected.

## What is checked on replay

The saved file is not trusted.
On `--offline`, polkadot-frame-trace checks:

- the block hash against the header, and the parent header against the block's parent hash;
- every trie node it uses, by walking down from the parent state root;
- the runtime code, read through those same nodes;
- the [root check](../concepts/what-a-replay-proves.md) at the end;
- for calls, that the replay returns the saved `result`.

`runtime_version` and `metadata` are never used to run the block.
A saved `state_call` answer is only compared, and the output marks it `recorded (unverified)`.

## Size limits

polkadot-frame-trace refuses files beyond these limits:

| Limit | Value |
| --- | --- |
| JSON after decompression | 64 MiB |
| Number of proof nodes | 100,000 |
| Total size of proof nodes | 24 MiB |

A live Westend Asset Hub block saved to 3.0 MB.
Most of that is the runtime code.

## What is not saved

- Traces, timings and tables. Replay the block, or keep `--json` output too.
- A runtime override. `--runtime-override` cannot be combined with `--save`.
- State the replay did not read. The file covers this block only.

## Proof acquisition and execution

Nodes share one content-addressed namespace. SDK `KeySpacedDB` supplies the child
namespace on misses; child requests strip that keyspace and use the prefixed
`:child_storage:default:` key with `state_getChildReadProof`. The top-level child-root
entry is loaded first using `state_getReadProof`. SDK traversal authenticates child
nodes against that entry and the published parent state root. An RPC proof's `at`
must equal the selected parent hash.

The runtime's `:code` and `:heappages` are obtained through the authenticated trie.
JSON call arguments are encoded using locally executed metadata before the call.
Every execution uses a fresh overlay and backend recorder. The stable2609 default
`ExecutionExtensions` factory is empty, then adds executor-backed
`ReadRuntimeVersionExt`: both import and ordinary `state_call` receive that reader.
Import with `enable_import_proof_recording` additionally supplies `ProofSizeExt`;
ordinary `state_call` supplies none. These are the two policies polkadot-frame-trace mirrors.
No node-specific custom extension factory, keystore or offchain-worker capability
is inferred from RPC. Unsupported custom host/extension requirements fail explicitly.
See SDK [execution extensions](https://github.com/paritytech/polkadot-sdk/blob/polkadot-stable2609/substrate/client/api/src/execution_extensions.rs),
[call executor](https://github.com/paritytech/polkadot-sdk/blob/polkadot-stable2609/substrate/client/service/src/client/call_executor.rs)
and [import path](https://github.com/paritytech/polkadot-sdk/blob/polkadot-stable2609/substrate/client/service/src/client/client.rs). Loading code uses
a separate backend without that recorder. Full-key hints acquire a lookup's path
in one request; a per-replay miss gate deduplicates concurrent strict cache misses.
Predicted keys are batched 32 at a time, across a bounded connection pool (default
8). Discarded speculative runs can gather further missing paths; only strict runs
produce accepted roots/captures. Shared nodes are authenticated anew at each root.
The SDK executor's code-hash cache is reused in memory and Wasmtime caches compiled
modules on disk. Neither changes execution overlays or recorder contents.

Online replay runs discovery, disables fetching, and repeats `Core_execute_block`.
Offline replay does both passes with fetching disabled. Missing proof nodes are
specific hash/path errors, guarded against SDK abort-on-panic hooks. It never constructs a network client. Success means the runtime accepted
the block, including its own state and extrinsic-root checks; neither assertion
nor signature checking is disabled. This verifies the transition relative to the
provided header/state root. The captured computed root is also compared explicitly
with the header root and both are reported. It does not independently establish chain consensus.

The capture is execution output, not trusted artifact input. It retains every
`state` event, raw backend iteration attempts, span fields/parents, enter/exit
transitions and monotonic nanosecond offsets. Native host-call intervals time
storage operations; backend attempts use their own measured durations. With Wasm
tracing, `apply_extrinsic` spans also retain recorder size before and after entry.
No warm timing includes network work.


## Call execution

Calls execute the requested export with `CallContext::Offchain`, fresh overlays,
the same host functions and the ordinary-call extension policy above. A backend
recorder reports proof bytes without exposing a proof-size extension to the runtime. Discovery authenticates missing
reads; the measured pass disables RPC. Oracle bytes are editable artifact input,
not an authenticated node attestation; a comparison only checks equality to those bytes.
Calls do **not** assert a new block root. They authenticate reads relative to the
selected header's state root. Metadata is re-executed locally (V16, V15, legacy V14)
for names and argument/result types. The code-hash cache retains proof dependencies
per state root. When local metadata execution is unavailable, names and results stay hex.


## Follow predictions and exported nodes

Follow predictions retain up to 4,096 top-trie keys/miss paths per successful
replay, across the last three completions (the combined per-replay prediction set is also
capped at 4,096, and concat-map families at 256). Families are built once per replay. This includes full storage keys and
sibling-prefix requests from iteration/root calculation. Before loading runtime
code at the next state root, prefetch batches them with metadata storage prefixes
using the bounded proof pool. Covered paths (including proven absence) are skipped.
No guessed value is accepted: content hashes plus traversal from the new root
authenticate every used node. Child paths continue to use namespaced demand fetching.
A new identifier in a single-key `Blake2_128Concat`/`Twox64Concat` layout can also
predict related map reads observed in that history: the concat hash must match,
the encoded identifier is exactly 32 bytes, and at most 31 additional keys join
one proof request. Identifiers with equal byte widths may have different meanings;
wrong guesses cost bounded bandwidth and cannot supply unauthenticated values.
This also expands pending keys from discarded dry runs before resuming execution.
`proof_round_trips` counts sequential proof request groups (one per parallel group,
one per strict demand fetch); it excludes context RPC and retry attempts.

Artifacts export only nodes read by the measured execution and code/metadata
bootstrap. Speculative, prefetch and cache-coverage probes cannot add unused nodes
to the artifact. Shared acquisition caches may retain those nodes for future runs.

## Repository fixtures

Repository fixtures may use `blob:<blake2-256>` strings in metadata and proof nodes.
They are rehydrated from the adjacent `blobs/<hash>.zst` directory, limited to 16 MiB per blob and verified by hash.
Saved user artifacts never contain these references.
