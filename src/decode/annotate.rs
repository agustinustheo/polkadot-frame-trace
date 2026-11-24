//! A traced block with names: the form the exporters and the CLI consume.

use std::collections::BTreeMap;

use crate::model::{OpKind, Phase, PrintedBytes, Stats, StorageOp, TracedBlock};
use codec::Decode;
use serde::{Deserialize, Serialize};

use crate::decode::{
    BlockWeight, KeyName, Metadata, Weight,
    key::well_known,
    weight::{self, Dispatch},
};

/// A [`TracedBlock`] with every key and extrinsic named. Serialises to the `--json` output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotatedBlock {
    /// Public JSON format version.
    pub schema_version: u32,
    #[serde(default)]
    pub execution: Option<ExecutionInfo>,
    /// Runtime API context, absent for block traces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<AnnotatedCall>,
    /// A substitute runtime executed instead of the state's `:code`; absent normally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_override: Option<RuntimeOverrideInfo>,
    /// Observed span tree.
    pub runtime_spans: Vec<crate::model::RuntimeSpan>,
    /// Replay duration, absent for synthetic input.
    pub timing: Option<crate::model::MeasuredTiming>,
    /// Block number. `state_traceBlock` does not report it, so it is `None` unless the caller
    /// knows it (for example from the RPC source).
    pub block_number: Option<u64>,
    /// Block hash.
    pub block_hash: String,
    /// Parent hash.
    pub parent_hash: String,
    /// Events in the trace that were not storage ops.
    pub skipped_events: usize,
    /// Totals over the block.
    pub totals: Stats,
    /// The last `System::BlockWeight` written in the block; needs metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_weight: Option<BlockWeight>,
    /// Phases in execution order.
    pub phases: Vec<AnnotatedPhase>,
}

/// One authenticated runtime API execution. Block root checks do not apply to calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotatedCall {
    pub method: String,
    pub data: String,
    pub result: String,
    pub decoded: Option<String>,
    pub oracle_match: Option<bool>,
    pub oracle_source: Option<String>,
    pub proof_bytes: usize,
    pub state_root: String,
}

/// One phase with its ops.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotatedPhase {
    /// Measured phase bounds (including compute), absent if no defining spans exist.
    #[serde(default)]
    pub wall_time: Option<PhaseTiming>,
    /// Whether the encoded extrinsic is signed; absent when bytes are unavailable.
    #[serde(default)]
    pub signed: Option<bool>,
    /// `initialization`, `apply_extrinsic`, `finalization` or `unsegmented`.
    pub kind: String,
    /// Extrinsic index for `apply_extrinsic`.
    pub extrinsic_index: Option<u32>,
    /// `0x`-prefixed blake2-256 of the encoded extrinsic: the transaction hash a submitter
    /// (for example a load generator) sees. Absent when the bytes were not in the trace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extrinsic_hash: Option<String>,
    /// `Pallet.call` of the extrinsic, when its bytes were in the trace and decoded.
    pub call: Option<String>,
    /// Why the extrinsic bytes did not decode. Usually the metadata is not the block's runtime.
    pub call_error: Option<String>,
    /// Display name, for example `extrinsic 1: Balances.transfer_keep_alive`.
    pub label: String,
    /// Weight charged by the extrinsic's `ExtrinsicSuccess` or `ExtrinsicFailed` event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charged_weight: Option<Weight>,
    /// `normal`, `operational` or `mandatory`, from the same event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_class: Option<String>,
    /// `yes` or `no`, from the same event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pays_fee: Option<String>,
    /// `success` (`ExtrinsicSuccess`) or `failed` (`ExtrinsicFailed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Storage totals of the phase's own ops (without `post_inherents`).
    pub stats: Stats,
    /// The phase's own ops in order (without those of `post_inherents`).
    pub ops: Vec<AnnotatedOp>,
    /// `inherents_applied` work (`on_poll`) nested inside the first signed extrinsic, between its
    /// `check` reads and its own work.
    pub post_inherents: Option<Box<AnnotatedPhase>>,
}

impl AnnotatedBlock {
    /// Every phase, each followed by its nested phase.
    pub fn all_phases(&self) -> impl Iterator<Item = &AnnotatedPhase> {
        self.phases.iter().flat_map(AnnotatedPhase::with_nested)
    }
}

impl AnnotatedPhase {
    /// This phase, then its nested phase if any.
    pub(crate) fn with_nested(&self) -> impl Iterator<Item = &AnnotatedPhase> {
        std::iter::once(self).chain(self.post_inherents.as_deref())
    }

    /// Positions in the block of the first and past-the-last op, nested phase included.
    pub fn seq_span(&self) -> Option<std::ops::Range<usize>> {
        let seqs = self.with_nested().flat_map(|p| &p.ops).map(|op| op.seq);
        let (min, max) = seqs.fold(None, |acc: Option<(usize, usize)>, s| {
            Some(acc.map_or((s, s), |(lo, hi)| (lo.min(s), hi.max(s))))
        })?;
        Some(min..max + 1)
    }
}

/// One storage op with its key named.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotatedOp {
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub proof_bytes_added: Option<usize>,
    /// Measured local replay interval.
    pub timing: Option<crate::model::MeasuredTiming>,
    /// Parent capture span ID.
    pub parent_span: Option<u64>,
    /// Position in the block.
    pub seq: usize,
    /// Operation name (`get`, `put`, ...).
    pub op: String,
    /// Child trie storage key (hex, `0x`-prefixed), if the op is on a child trie.
    pub child_trie: Option<String>,
    /// The node printed the child trie key truncated (`<head>...<tail>` in `child_trie`).
    pub child_trie_truncated: bool,
    /// Raw key or prefix (hex, `0x`-prefixed); `<head>...<tail>` when truncated.
    pub key: String,
    /// The node printed only the first and last 512 bytes of the key. Such keys are named from
    /// their prefix only and never counted as duplicates.
    pub key_truncated: bool,
    /// The key was named in full: all map keys decoded, nothing left over. False for prefixes
    /// (`clear_prefix`), truncated keys, unknown keys and keys that do not decode.
    pub key_complete: bool,
    /// Pallet, when known.
    pub pallet: Option<String>,
    /// `Pallet::Item`, a well-known key, `child:<id>`, `<storage root>`, `<child storage root>`
    /// or `<unknown>`.
    pub item: String,
    /// Decoded map keys.
    pub map_keys: Vec<String>,
    /// The key's name, for example `System::Account(5Grw...)`; `…` marks missing map keys.
    pub name: String,
    /// Display name of the op, for example `get System::Account(5Grw...)`.
    pub label: String,
    /// Length of the value read or written, when known.
    pub value_len: Option<usize>,
    /// The value was printed truncated by the node.
    pub value_truncated: bool,
    /// The key was already read earlier in this phase.
    pub duplicate_read: bool,
}

/// Name keys and extrinsics; without metadata, only well-known keys are named.
pub fn annotate(block: &TracedBlock, metadata: Option<&Metadata>) -> AnnotatedBlock {
    annotate_at_block(block, metadata, None)
}

/// Name hooks with an authenticated block number for OnIdle rotation.
pub fn annotate_at_block(
    block: &TracedBlock,
    metadata: Option<&Metadata>,
    number: Option<u64>,
) -> AnnotatedBlock {
    let mut observed = std::collections::BTreeMap::<(String, String), usize>::new();
    for span in &block.runtime_spans {
        let krate = span.target.split("::").next().unwrap_or(&span.target);
        *observed
            .entry((span.name.clone(), krate.into()))
            .or_default() += 1;
    }
    let mut hooks = std::collections::BTreeMap::<(String, String), usize>::new();
    let (dispatches, block_weight) = metadata.map(|md| weights(block, md)).unwrap_or_default();
    AnnotatedBlock {
        schema_version: 2,
        execution: None,
        call: None,
        runtime_override: None,
        runtime_spans: block
            .runtime_spans
            .iter()
            .cloned()
            .map(|mut span| {
                if matches!(
                    span.name.as_str(),
                    "on_initialize" | "on_finalize" | "on_idle" | "on_poll"
                ) {
                    let krate = span.target.split("::").next().unwrap_or(&span.target);
                    let occurrence = hooks.entry((span.name.clone(), krate.into())).or_default();
                    let candidates = metadata
                        .map(|md| md.pallets_for_crate(krate))
                        .unwrap_or_default();
                    let ordered =
                        metadata.and_then(|md| md.hook_instances(krate, &span.name, number));
                    let pallet = ordered
                        .filter(|names| {
                            observed.get(&(span.name.clone(), krate.into())) == Some(&names.len())
                        })
                        .and_then(|names| names.get(*occurrence).cloned())
                        .or_else(|| (!candidates.is_empty()).then(|| candidates.join(" | ")))
                        .unwrap_or_else(|| crate_label(krate));
                    *occurrence += 1;
                    if krate != "runtime" && !krate.is_empty() {
                        span.name = format!("{}: {pallet}", span.name);
                    }
                }
                span
            })
            .collect(),
        timing: block.timing,
        block_number: number,
        block_hash: block.block_hash.clone(),
        parent_hash: block.parent_hash.clone(),
        skipped_events: block.skipped_events,
        totals: block.totals(),
        block_weight,
        phases: block
            .phases
            .iter()
            .map(|phase| annotate_phase(block, phase, metadata, &dispatches))
            .collect(),
    }
}

/// Each extrinsic's last `ExtrinsicSuccess` or `ExtrinsicFailed` event, and the committed
/// `System::BlockWeight` value. Both come from data already in the trace, with pallet names
/// and storage prefixes resolved from metadata.
fn weights(block: &TracedBlock, md: &Metadata) -> (BTreeMap<u32, Dispatch>, Option<BlockWeight>) {
    let Some(sys) = weight::SystemTypes::resolve(md) else {
        return Default::default();
    };
    let mut dispatches = BTreeMap::new();
    if let Some((events_key, event_ty)) = sys.events {
        for op in block
            .ops
            .iter()
            .filter(|op| op.child_trie.is_none() && op.kind == OpKind::Append)
        {
            let (Some(key), Some(value)) = (op.key.complete(), op.value.as_deref()) else {
                continue;
            };
            if key == events_key
                && let Some(dispatch) = weight::dispatch_with_type(md, &sys.pallet, event_ty, value)
            {
                dispatches.insert(dispatch.index, dispatch);
            }
        }
    }
    let block_weight = sys.block_weight.and_then(|(bw_key, bw_ty)| {
        // Local replays capture the committed post-execution overlay, immune to rollbacks.
        if let Some(committed) = &block.committed_values {
            return committed
                .get(&bw_key)
                .and_then(|value| value.as_deref())
                .and_then(|bytes| weight::block_weight_with_type(md, bw_ty, bytes));
        }
        // Fallback for non-replay traces: follow puts and clears in order without keeping stale values.
        let mut last = None;
        for op in block.ops.iter().filter(|op| op.child_trie.is_none()) {
            let Some(key) = op.key.complete() else {
                continue;
            };
            if key != bw_key {
                continue;
            }
            match op.kind {
                OpKind::Put => {
                    last = op
                        .value
                        .as_deref()
                        .and_then(|bytes| weight::block_weight_with_type(md, bw_ty, bytes));
                }
                OpKind::Clear => last = None,
                _ => {}
            }
        }
        last
    });
    (dispatches, block_weight)
}

fn annotate_phase(
    block: &TracedBlock,
    phase: &Phase,
    metadata: Option<&Metadata>,
    dispatches: &BTreeMap<u32, Dispatch>,
) -> AnnotatedPhase {
    let decoded = metadata
        .zip(phase.extrinsic.as_deref())
        .map(|(md, bytes)| md.call_name(bytes));
    let (call, call_error) = match decoded {
        Some(Ok(call)) => (Some(call.to_string()), None),
        Some(Err(err)) => (None, Some(err.to_string())),
        None => (None, None),
    };
    let label = match &call {
        Some(call) => format!("{}: {call}", phase.kind),
        None => phase.kind.to_string(),
    };
    let dispatch = phase
        .kind
        .extrinsic_index()
        .and_then(|i| dispatches.get(&i));
    AnnotatedPhase {
        wall_time: phase_bounds(block, phase),
        signed: phase.extrinsic.as_ref().and_then(|bytes| {
            let mut cursor = bytes.as_slice();
            codec::Compact::<u32>::decode(&mut cursor).ok()?;
            cursor.first().map(|b| b & 0x80 != 0)
        }),
        kind: phase.kind.name().into(),
        extrinsic_index: phase.kind.extrinsic_index(),
        extrinsic_hash: phase
            .extrinsic
            .as_deref()
            .map(|bytes| format!("0x{}", hex::encode(sp_crypto_hashing::blake2_256(bytes)))),
        call,
        call_error,
        label,
        charged_weight: dispatch.map(|d| d.weight),
        dispatch_class: dispatch.map(|d| d.class.clone()),
        pays_fee: dispatch.and_then(|d| d.pays_fee.clone()),
        outcome: dispatch.map(|d| d.outcome.to_owned()),
        stats: phase.stats,
        ops: block
            .phase_ops(phase)
            .map(|op| annotate_op(op, metadata))
            .collect(),
        post_inherents: phase
            .post_inherents
            .as_ref()
            .map(|nested| Box::new(annotate_phase(block, nested, metadata, dispatches))),
    }
}

fn annotate_op(op: &StorageOp, metadata: Option<&Metadata>) -> AnnotatedOp {
    let child = op.child_trie.as_ref().map(|trie| trie.head.as_slice());
    let name = match (op.kind, metadata, op.key.complete()) {
        // Keyless: name the work, so it is never counted with unknown keys.
        (OpKind::StorageRoot, _, _) if child.is_some() => {
            KeyName::WellKnown("<child storage root>".into())
        }
        (OpKind::StorageRoot, _, _) => KeyName::WellKnown("<storage root>".into()),
        (_, Some(md), Some(key)) => md.key_name(child, key),
        // Never decode map keys from a truncated key: the middle is missing.
        (_, Some(md), None) => md.item_name(child, &op.key.head),
        (_, None, key) => match (child, key.and_then(well_known)) {
            (Some(trie), _) => KeyName::Child {
                trie: trie.to_vec(),
                key: op.key.head.clone(),
            },
            (None, Some(name)) => name,
            (None, None) => KeyName::Unknown { pallet: None },
        },
    };
    let map_keys = match &name {
        KeyName::Storage { keys, .. } => keys.clone(),
        _ => Vec::new(),
    };
    let label = if op.kind.has_key() {
        format!("{} {name}", op.kind)
    } else {
        op.kind.to_string()
    };
    AnnotatedOp {
        timing: op.timing,
        origin: op.origin.clone(),
        proof_bytes_added: op.proof_bytes_added,
        parent_span: op.parent_span,
        seq: op.seq,
        op: op.kind.as_str().into(),
        child_trie: op.child_trie.as_ref().map(PrintedBytes::to_hex),
        child_trie_truncated: op
            .child_trie
            .as_ref()
            .is_some_and(PrintedBytes::is_truncated),
        key: op.key.to_hex(),
        key_truncated: op.key.is_truncated(),
        key_complete: name.is_complete() && op.key_id().is_some(),
        pallet: name.pallet().map(str::to_owned),
        item: name.item(),
        map_keys,
        name: name.to_string(),
        label,
        value_len: op.value_len,
        value_truncated: op.value_truncated,
        duplicate_read: op.duplicate_read,
    }
}

fn crate_label(krate: &str) -> String {
    krate
        .replace("pallet_", "")
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
        })
        .collect()
}

/// Explicit offsets on the replay clock. Duration includes compute and instrumentation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseTiming {
    pub start_ns: u64,
    pub end_ns: u64,
    pub duration_ns: u64,
    /// runtime_span includes observed executive/hook spans; storage_envelope has only calls.
    pub source: String,
}
fn phase_bounds(block: &TracedBlock, phase: &Phase) -> Option<PhaseTiming> {
    let mut extrinsic = 0;
    let mut bounds: Option<(u64, u64)> = None;
    for span in &block.runtime_spans {
        let matches = match span.name.as_str() {
            "apply_extrinsic" => {
                let i = extrinsic;
                extrinsic += 1;
                phase.kind.extrinsic_index() == Some(i)
            }
            "initialize_block" | "init_block" | "initial_checks" | "on_initialize" => {
                phase.kind.name() == "initialization"
            }
            "finalize_block" | "final_checks" | "on_finalize" | "on_idle" => {
                phase.kind.name() == "finalization"
            }
            "inherents_applied" => phase.kind.name() == "post_inherents",
            _ => false,
        };
        if matches && let Some(end) = span.end_ns {
            bounds = Some(bounds.map_or((span.start_ns, end), |(lo, hi)| {
                (lo.min(span.start_ns), hi.max(end))
            }));
        }
    }
    let source = if bounds.is_some() {
        "runtime_span"
    } else {
        "storage_envelope"
    };
    // Include native calls outside executive wrappers as well. The explicit interval
    // is the envelope of observed evidence, never a fabricated full-phase timer.
    for timing in block.phase_ops(phase).filter_map(|op| op.timing) {
        let end = timing.start_ns.saturating_add(timing.duration_ns);
        bounds = Some(bounds.map_or((timing.start_ns, end), |(lo, hi)| {
            (lo.min(timing.start_ns), hi.max(end))
        }));
    }
    bounds.map(|(start_ns, end_ns)| PhaseTiming {
        start_ns,
        end_ns,
        duration_ns: end_ns.saturating_sub(start_ns),
        source: source.into(),
    })
}

/// The executed Wasm was not the authenticated parent-state `:code`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeOverrideInfo {
    /// The substitute Wasm file.
    pub path: String,
    /// Blake2-256 of the executed substitute Wasm.
    pub code_hash: String,
    /// Blake2-256 of the authenticated `:code` that was not executed.
    pub chain_code_hash: String,
    /// RuntimeVersion fields that differ, or notes that a version could not be read.
    #[serde(default)]
    pub version_differences: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionInfo {
    pub root_check: bool,
    pub proof_bytes: usize,
    pub rpc_calls: usize,
    pub measured_rpc_calls: usize,
    pub proof_rounds: usize,
}

#[cfg(test)]
mod phase_timing_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[test]
    fn phase_json_has_explicit_bounds_and_marks_storage_only_envelopes() {
        let raw = serde_json::from_value::<crate::model::rpc::BlockTrace>(
            serde_json::from_str::<serde_json::Value>(include_str!(
                "../../fixtures/model/two-extrinsics.json"
            ))
            .expect("JSON")["result"]["blockTrace"]
                .take(),
        )
        .unwrap();
        let mut block = TracedBlock::from_trace(&raw).unwrap();
        for op in &mut block.ops {
            op.timing = Some(crate::model::MeasuredTiming {
                start_ns: 10 + op.seq as u64 * 10,
                duration_ns: 5,
            });
        }
        block.runtime_spans = vec![crate::model::RuntimeSpan {
            id: 1,
            parent_id: None,
            name: "init_block".into(),
            target: "runtime".into(),
            wasm: true,
            fields: Default::default(),
            start_ns: 1,
            end_ns: Some(99),
            duration_ns: 98,
            enters: 1,
            proof_start_bytes: 0,
            proof_end_bytes: 0,
        }];
        let named = annotate(&block, None);
        let init = named.phases[0].wall_time.as_ref().unwrap();
        assert_eq!((init.start_ns, init.end_ns, init.duration_ns), (1, 99, 98));
        assert_eq!(init.source, "runtime_span");
        let other = named.phases[1].wall_time.as_ref().unwrap();
        assert_eq!(other.duration_ns, other.end_ns - other.start_ns);
        assert_eq!(other.source, "storage_envelope");
        let json = serde_json::to_value(&named).unwrap();
        assert_eq!(json["phases"][0]["wall_time"]["end_ns"], 99);
        let loaded: AnnotatedBlock = serde_json::from_value(json).unwrap();
        assert_eq!(loaded.phases[0].wall_time.as_ref().unwrap().duration_ns, 98);
    }
}

#[cfg(test)]
mod weight_and_root_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn block() -> TracedBlock {
        let raw = serde_json::from_value::<crate::model::rpc::BlockTrace>(
            serde_json::from_str::<serde_json::Value>(include_str!(
                "../../fixtures/model/two-extrinsics.json"
            ))
            .unwrap()["result"]["blockTrace"]
                .take(),
        )
        .unwrap();
        TracedBlock::from_trace(&raw).unwrap()
    }

    #[test]
    fn storage_roots_are_named_and_never_unknown() {
        let mut block = block();
        let root = block
            .ops
            .iter()
            .position(|op| op.kind == OpKind::StorageRoot)
            .unwrap();
        let mut child = block.ops[root].clone();
        child.child_trie = Some(PrintedBytes::full(b"trie".to_vec()));
        let md = Metadata::from_bytes(include_bytes!("../../fixtures/metadata/westend-v16.scale"))
            .unwrap();
        for metadata in [None, Some(&md)] {
            let named = annotate_op(&block.ops[root], metadata);
            assert_eq!(
                (named.item.as_str(), named.name.as_str()),
                ("<storage root>", "<storage root>")
            );
            assert_eq!(named.pallet, None);
            let named = annotate_op(&child, metadata);
            assert_eq!(named.item, "<child storage root>");
            assert_eq!(named.child_trie.as_deref(), Some("0x74726965"));
        }
        // Keyless ops other than storage roots stay unknown.
        block.ops[root].kind = OpKind::IndexTransaction;
        assert_eq!(annotate_op(&block.ops[root], None).item, "<unknown>");
    }

    #[test]
    fn json_without_weights_still_loads_and_omits_them() {
        let named = annotate(&block(), None);
        assert!(named.block_weight.is_none());
        assert!(named.all_phases().all(|p| p.charged_weight.is_none()));
        let mut json = serde_json::to_value(&named).unwrap();
        assert!(json.get("block_weight").is_none());
        for key in ["charged_weight", "dispatch_class", "pays_fee", "outcome"] {
            assert!(json["phases"][1].get(key).is_none(), "{key}");
        }
        // The same JSON with the new fields round trips.
        json["block_weight"] = serde_json::json!({
            "normal": {"ref_time": 1, "proof_size": 2},
            "operational": {"ref_time": 0, "proof_size": 0},
            "mandatory": {"ref_time": 3, "proof_size": 4}
        });
        json["phases"][1]["charged_weight"] =
            serde_json::json!({"ref_time": 5, "proof_size": 6283});
        json["phases"][1]["outcome"] = serde_json::json!("failed");
        let loaded: AnnotatedBlock = serde_json::from_value(json).unwrap();
        assert_eq!(loaded.block_weight.unwrap().total().proof_size, 6);
        assert_eq!(loaded.phases[1].charged_weight.unwrap().proof_size, 6283);
        assert_eq!(loaded.phases[1].outcome.as_deref(), Some("failed"));
        assert_eq!(loaded.phases[1].dispatch_class, None);
    }

    #[test]
    fn block_weight_respects_committed_overlay_clears_and_undecodable_puts() {
        let md = Metadata::from_bytes(include_bytes!("../../fixtures/metadata/westend-v16.scale"))
            .unwrap();
        let (bw_key, bw_ty) = md
            .storage_item_key_and_type("System", "BlockWeight")
            .unwrap();
        let encode_bw = |normal: u128, mandatory: u128| {
            let w = |p| {
                scale_value::Value::named_composite([
                    ("ref_time", scale_value::Value::u128(1)),
                    ("proof_size", scale_value::Value::u128(p)),
                ])
            };
            let val = scale_value::Value::named_composite([
                ("normal", w(normal)),
                ("operational", w(0)),
                ("mandatory", w(mandatory)),
            ]);
            let mut out = Vec::new();
            scale_value::scale::encode_as_type(&val, bw_ty, md.types(), &mut out).unwrap();
            out
        };
        let template = block().ops[0].clone();
        let put_op = |value: Vec<u8>| {
            let mut op = template.clone();
            op.kind = OpKind::Put;
            op.child_trie = None;
            op.key = PrintedBytes::full(bw_key.to_vec());
            op.value = Some(value);
            op
        };
        let mut clear_op = template.clone();
        clear_op.kind = OpKind::Clear;
        clear_op.child_trie = None;
        clear_op.key = PrintedBytes::full(bw_key.to_vec());
        clear_op.value = None;

        // Fallback op scan: a later Clear clears BlockWeight rather than keeping the earlier Put.
        let mut blk = block();
        blk.ops = vec![put_op(encode_bw(10, 20)), clear_op];
        assert_eq!(annotate(&blk, Some(&md)).block_weight, None);

        // Fallback op scan: an undecodable later Put clears the stale earlier value.
        blk.ops = vec![put_op(encode_bw(10, 20)), put_op(vec![0xff])];
        assert_eq!(annotate(&blk, Some(&md)).block_weight, None);

        // Local replay committed overlay overrides a rolled-back Put recorded in block.ops.
        blk.ops = vec![put_op(encode_bw(10, 20)), put_op(encode_bw(999, 888))];
        blk.committed_values = Some(BTreeMap::from([(bw_key, Some(encode_bw(10, 20)))]));
        let named = annotate(&blk, Some(&md));
        assert_eq!(named.block_weight.unwrap().normal.proof_size, 10);
        assert_eq!(named.block_weight.unwrap().mandatory.proof_size, 20);
    }
}
