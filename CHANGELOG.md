# Changelog

## Unreleased

- `block` shows each extrinsic's charged proof size next to its measured proof bytes, and the gap between them.
  The charge comes from the `ExtrinsicSuccess` and `ExtrinsicFailed` events in the trace, with no extra RPC calls.
- `block` compares the block's final `System::BlockWeight` with the measured proof bytes.
- `report` adds charged proof and the mean proof gap per extrinsic type.
- Block JSON adds `block_weight` and, per extrinsic, `charged_weight`, `dispatch_class`, `pays_fee` and `outcome`. Older JSON still loads.
- Computing the state root shows as `<storage root>` or `<child storage root>`, not `<unknown>`.

## [0.1.0] - 2026-11-24

First release: replay blocks and runtime API calls, check the state root, and print storage tables.
Export traces over OTLP/HTTP or as JSON, compare blocks with `report`, and trace new finalized blocks with `follow` (experimental).

See [Commands and flags](docs/reference/commands.md) and [supported chains](docs/reference/support.md).

[0.1.0]: https://github.com/agustinustheo/polkadot-frame-trace/releases/tag/v0.1.0
