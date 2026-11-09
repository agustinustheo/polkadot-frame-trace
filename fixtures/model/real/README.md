# Real block fixtures

Blocks of a `next-people-paseo` parachain (spec version 3004000) recorded from a real node by the
experiment-e1 agent, copied here so tests do not depend on those directories.

| File | Source |
| --- | --- |
| `people-paseo-block-1-trace-state.json` | `fixtures/next-people-paseo-traced-official/1/trace-state.json` (2026-10-24 19:13 UTC) |

All traces are `state_traceBlock(<hash>, "state", "", "")` responses.

- Block 1 (`0xb306…5719`): two inherents (`ParachainSystem.set_validation_data`,
  `Timestamp.set`), no signed extrinsic, so `inherents_applied` (`on_poll`) runs right before
  finalization. 315 storage ops.
