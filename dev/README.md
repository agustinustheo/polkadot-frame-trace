# Internal notes

The pre-project research, experiments E1 and E2, and the upstream bug drafts were removed on 7 November 2026. They are in commit 2e5c243.

Material for people working on polkadot-frame-trace itself. User documentation is in [`docs/`](../docs/README.md).

| Path | What it is |
| --- | --- |
| [`STATUS.md`](STATUS.md) | Known limits, open decisions and upstream bugs. |
| [`RELEASING.md`](RELEASING.md) | How to cut a release: version bump, changelog, tag, draft release. |
| [`performance.md`](performance.md) | Current numbers: follow throughput, the memory limit, and how to reproduce them. |

The replay artifact format is specified in [`docs/reference/saved-block-format.md`](../docs/reference/saved-block-format.md). Fixture provenance is in [`fixtures/replay/README.md`](../fixtures/replay/README.md).

## Add host functions for a chain

polkadot-frame-trace executes every runtime with one fixed set of host functions: the `HostFunctions`
tuple in [`src/replay/mod.rs`](../src/replay/mod.rs). It matches a stable2609
omni-node: `sp_io::SubstrateHostFunctions`, storage proof size, statement store and the
RFC-163 elliptic curve functions. Executors disallow missing imports.
A runtime that imports anything else fails before it runs.
See [Troubleshooting](../docs/troubleshooting.md#the-runtime-needs-host-functions-polkadot-frame-trace-does-not-have).

To support such a chain:

1. Find the crate that defines the functions. A `#[runtime_interface]` module exports a
   `HostFunctions` type; the names in the error are `ext_<interface>_<function>_version_<n>`.
2. Add the crate to `Cargo.toml` and update `Cargo.lock` (builds use
   `--locked`). Most SDK crates there are pinned with `=` to one SDK release; pin the new one
   the same way, to the version from that release. Two are not pinned exactly:
   `sp-panic-handler = "13.0.2"` and `sp-crypto-hashing` (`"0.1.0"`) use caret
   ranges. See [`RELEASING.md`](RELEASING.md#polkadot-sdk-upgrades) for SDK upgrades.
3. Append its `HostFunctions` type to the tuple.
4. Replay a real block of that chain with `polkadot-frame-trace block --rpc <url> --block <n>` and
   check `root check: PASS`. Save it with `--save` if it should become a fixture
   (see [`fixtures/replay/README.md`](../fixtures/replay/README.md)).

Every added function is offered to every chain. Only add functions that the node of that
chain also provides, so polkadot-frame-trace never accepts a runtime the chain itself could not run.
The CLI test `unusable_runtime_override_files_and_missing_host_functions_are_named` covers the
error path.
`fixtures/wasm/missing-host-function.wasm` reproduces it from the command line.

## Live tests

Tests that need a public node are `#[ignore]`d. Run them with
`cargo +1.93.0 test --locked -- --ignored`. Each reads its endpoint from an
environment variable and falls back to the public endpoint:

| Variable | Default | Tests |
| --- | --- | --- |
| `FRAME_TRACE_WESTEND_AH_RPC` | `wss://westend-asset-hub-rpc.polkadot.io` | `public_network` (best block), `runtime_upgrade_enactment_and_next_block_live` (the upgrade block #18,985,169 and the first block on the new code; needs archive state for #18,985,169) |
| `FRAME_TRACE_WESTEND_RPC` | `wss://westend-rpc.polkadot.io` | `public_relay_network` (finalized block) |

The `FRAME_TRACE_*` variables used by tests, CI and the build version kept their names after the rename: they are internal.

The nightly workflow (`.github/workflows/nightly.yml`) runs all three with the defaults.
Runtime upgrade blocks and relay chain blocks have no offline fixture. Their runtimes
would add 1.5 to 2.7 MB each to the repository (see
[`fixtures/replay/README.md`](../fixtures/replay/README.md)). These live tests are their only
tests, so run them when a change touches runtime loading, the session caches or block
execution.
