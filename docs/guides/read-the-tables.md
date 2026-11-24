# Read the tables

Find repeated reads, the heaviest storage items and the largest parts of a block's [storage proof](../concepts/proof-bytes.md).
This guide uses block #4 of the People fixture.
Its extrinsic 2 is a `Utility.batch` of three `transfer_keep_alive` calls from Alice.

Run this from the `polkadot-frame-trace` directory:

```sh
polkadot-frame-trace block --offline fixtures/replay/people-traced --top 5
```

```text
root check: PASS; RPC calls: 0 (timed replay: 0); execution proof: 6586 bytes
...
```

For a live chain, use `--rpc <url> --block <block-number>` instead of `--offline`.
`--top 5` keeps five rows in each ranked table and per [phase](../concepts/phases.md) for repeated reads.
Times come from the replay on your machine, not from the node.

## Find repeated reads

A repeated read is a read of a key the same phase already read.
Many repeats are cheap [cache](../concepts/cache-or-trie.md) reads.
Check repeats that go to the trie first.

```text
1 block(s); 518 operations; reads 312 = cache 231 + trie 81 (already in proof 47, new 34) + unknown 0
Duplicate reads 203 = cache 177, trie (already in proof) 26, trie (new) 0, unknown 0; proof 6586 B (518 operations with proof data; 0 B not assigned to operations)
...
Duplicate reads per phase (top 5 each)
│ phase                                            ┆ key                                                               ┆ duplicate reads ┆ cache ┆ trie (already in proof) ┆ trie (new) ┆ unknown │
│ extrinsic 2: Utility.batch                       ┆ System::Account(5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY) ┆              47 ┆    42 ┆                       5 ┆          0 ┆       0 │
│ extrinsic 2: Utility.batch                       ┆ System::Account(5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty) ┆              15 ┆     8 ┆                       7 ┆          0 ┆       0 │
...
```

- `reads 312`: the block made 312 reads in total.
- `cache 231 + trie 81`: 231 came from the block's own changes in memory. 81 went to the chain state.
- `already in proof 47, new 34`: of the trie reads, 34 added new nodes to the proof.
- `Duplicate reads 203`: 203 reads were repeats within their phase.
  177 of them were cache reads, and 26 went to the trie again.

Each row is one key in one phase.
`duplicate reads` excludes the first read.
The next columns split the repeats by origin.

Alice's account, `System::Account(5Grw…utQY)`, has 47 repeats in the batch.
With the first read, that is 48 reads of the same key in one extrinsic:

| Reads | Origin | What happened |
| ---: | --- | --- |
| 1 | trie (new) | The first read. The runtime walked the trie to Alice's entry and added those nodes to the proof. |
| 5 | trie (already in proof) | Five more reads before anything wrote her account. Each walked the trie again but added 0 proof bytes. |
| 42 | cache | Her fee was taken, which wrote her account. Every read after that came from memory. |

Only the first read added proof bytes: 166 of them.
The other 47 were repeats: 5 extra trie walks and 42 memory reads.

Bob's row is different: 15 repeats, 8 from cache and 7 from the trie.
Until the first transfer writes his balance, every read of his account walks the trie again.
Reading a key does not put it in the cache. Only a write does.

To see the reads in order, [send the block to Jaeger](send-traces.md#jaeger) and open `extrinsic 2: Utility.batch`.
Check `frame.storage.origin` and `frame.storage.duplicate_read` on each read.
[Reading a trace](../getting-started/reading-a-trace.md) shows where they are.

## Find the heaviest storage items

`block` ranks items by proof bytes:

```text
Top items by proof bytes
│ item                                  ┆ reads: cache ┆ trie (already in proof) ┆ trie (new) ┆ unknown ┆ writes ┆ iterations ┆ bytes read ┆ bytes written ┆ duplicate reads ┆ proof bytes │
│ System::EventTopics                   ┆            0 ┆                       0 ┆          0 ┆       0 ┆      1 ┆          1 ┆          0 ┆             0 ┆               0 ┆        1120 │
...
│ System::Account                       ┆           52 ┆                      20 ┆          2 ┆       0 ┆      9 ┆          0 ┆       5200 ┆           720 ┆              69 ┆         332 │
...
… 69 more
...
```

| Column | Meaning |
| --- | --- |
| `item` | The storage item, as `Pallet::Item`. `<storage root>` holds the work of computing the state root. `<child storage root>` does the same for a child trie. `<unknown>` holds keys with no name. Keys starting with `:` are well-known keys, such as `:code`. |
| `reads: cache` | Reads answered from the block's own changes. See [Cache or trie](../concepts/cache-or-trie.md). |
| `trie (already in proof)` | Reads that went to the chain state and used only trie nodes seen earlier in this replay. A rollback may have removed them from the current proof. |
| `trie (new)` | Reads that added new trie nodes to the proof. |
| `unknown` | Reads with no origin recorded. |
| `writes` | Writes, including deletes and appends. |
| `iterations` | Times the runtime stepped through keys, for example to delete a prefix. |
| `bytes read`, `bytes written` | Sum of the value lengths, in bytes. A cache read still counts its bytes. |
| `duplicate reads` | Reads of a key already read in the same phase. See [Find repeated reads](#find-repeated-reads). |
| `proof bytes` | Bytes this item's operations added to the block's proof. |

- `System::EventTopics` added the most proof bytes, 1,120, with no reads at all.
  Initialization deletes this prefix. To delete the keys, the runtime walks them, and that adds trie nodes to the proof.
- `System::Account` was read 74 times: 52 from cache, 20 from the trie already in proof, 2 new.
  Only the two new reads added proof bytes, 166 each.

### Rank by operations or time

`report` takes saved JSON from `block --json` and has `--sort-by`:

```sh
polkadot-frame-trace block --offline fixtures/replay/people-traced --json block.json
polkadot-frame-trace report block.json --sort-by ops --top 5
```

```text
Top items by operations
│ item                ┆ reads: cache ┆ trie (already in proof) ┆ trie (new) ┆ unknown ┆ writes ┆ iterations ┆ bytes read ┆ bytes written ┆ duplicate reads ┆ proof bytes │
│ System::BlockWeight ┆           60 ┆                       0 ┆          0 ┆       0 ┆     58 ┆          0 ┆        724 ┆           692 ┆              54 ┆           0 │
│ System::Account     ┆           52 ┆                      20 ┆          2 ┆       0 ┆      9 ┆          0 ┆       5200 ┆           720 ┆              69 ┆         332 │
...
```

`System::BlockWeight` has 118 operations but 0 proof bytes.
Every read of it came from the cache, because the block keeps updating it.
Use `--sort-by time` to rank by storage time instead.

## Rank extrinsics

The same commands rank extrinsic types:

```text
Extrinsic distributions (ranked by total proof bytes)
│ pallet.call                         ┆ count ┆ operations median/p95 ┆ proof bytes median/p95 ┆ storage time µs median/p95 ┆ charged proof median/p95 ┆ proof gap mean │
│ Utility.batch                       ┆     1 ┆           200.0/200.0 ┆            827.0/827.0 ┆                581.0/581.0 ┆              605.0/605.0 ┆           +222 │
...
```

`count` is how many extrinsics of that type the input had.
The next columns give the median and the 95th percentile per extrinsic.
`proof gap mean` is the average gap per extrinsic, see [Find undercharged extrinsics](#find-undercharged-extrinsics).
With many blocks, these show which calls cost the most storage in a typical case and a bad case.
See [Analyse many blocks](analyse-many-blocks.md).

## Measure proof size

`execution proof: 6586 bytes` is the estimated storage proof size for block #4.
It is not the exact proof of validity (PoV).
[Proof bytes](../concepts/proof-bytes.md#what-proof-bytes-do-not-mean) explains the difference and how order changes the split.

The **Storage per phase** table gives each phase's proof bytes.
**Phase shares** gives each group's share of the block total:

```text
Phase shares
│ phase                           ┆ operations % ┆ storage time % ┆ proof % ┆ proof bytes │
│ initialization                  ┆         29.3 ┆           22.3 ┆    53.0 ┆        3490 │
│ signed extrinsics               ┆         38.6 ┆           40.3 ┆    12.6 ┆         827 │
│ finalization                    ┆         13.9 ┆           20.6 ┆    14.4 ┆         949 │
...
```

Initialization added 3,490 bytes, or 53% of the block's proof.
The first hooks read many items for the first time, so they get those trie nodes' bytes.
**Top items by proof bytes** and **Extrinsic distributions** show the split by item and extrinsic type.

In a trace viewer, each storage operation has `frame.storage.proof_bytes`.
Each `apply_extrinsic` runtime span has `frame.proof.marginal_bytes`: how much the proof grew during that extrinsic.
For the batch it is 1,513 bytes: 827 bytes for the batch plus 686 bytes for its post-inherent hooks.
Runtime spans need a runtime [built with tracing](see-runtime-calls.md).

For many blocks, [use `report`](analyse-many-blocks.md).

## Find undercharged extrinsics

Compare the proof size each extrinsic paid for with the proof bytes it added.
The charge comes from the extrinsic's `ExtrinsicSuccess` or `ExtrinsicFailed` event.
polkadot-frame-trace reads these events from the replay, with no extra RPC calls.
It needs the runtime's metadata, which a replay builds.

**Storage per phase** then has two more columns:

```text
│ phase                                            ┆ operations ┆ reads ┆ writes ┆ proof bytes ┆ storage time µs ┆ wall time µs ┆ charged proof ┆ proof gap │
│ initialization                                   ┆        152 ┆    74 ┆     77 ┆        3490 ┆           347.8 ┆        609.0 ┆             — ┆         — │
│ extrinsic 0: ParachainSystem.set_validation_data ┆         54 ┆    26 ┆     28 ┆         495 ┆           182.5 ┆        410.5 ┆             0 ┆      +495 │
│ extrinsic 1: Timestamp.set                       ┆         26 ┆    14 ┆     12 ┆         139 ┆            44.0 ┆         65.4 ┆          1493 ┆     -1354 │
│ extrinsic 2: Utility.batch                       ┆        200 ┆   146 ┆     54 ┆         827 ┆           626.5 ┆        919.9 ┆           605 ┆      +222 │
...
Block weight (System::BlockWeight): charged proof 272391, measured 6586, gap -265805
Mandatory class (hooks and mandatory extrinsics): charged proof 271553, measured 5759, gap -265794
```

| Column | Meaning |
| --- | --- |
| `charged proof` | The proof size, in bytes, that the extrinsic's weight charged. |
| `proof gap` | Proof bytes minus charged proof. `+` means the extrinsic added more than it paid for. |
| `—` | The phase has no event of its own, such as initialization. |

- `Utility.batch` added 827 proof bytes but paid for 605: 222 bytes more than its weight.
- `ParachainSystem.set_validation_data` charges no proof size, but added 495 bytes.
- `Timestamp.set` paid for 1,493 bytes and added 139.

A negative gap is normal. The first phase that reads a trie node gets its bytes.
A later extrinsic that reads the same node adds 0 bytes, but its weight still pays for it.
A positive gap means the extrinsic added more new proof bytes than its weight allows for.

The two lines under the table use the block's final `System::BlockWeight`.
The first compares the charge of all three dispatch classes with the whole block.
The second compares the mandatory class with initialization, post inherents, finalization and mandatory extrinsics.
It appears only when every extrinsic has a class.
Hook weights may be worst-case estimates, so the mandatory class often pays for far more than it uses.
