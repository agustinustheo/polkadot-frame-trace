//! Charged weights, decoded from the `System::Events` appends and `System::BlockWeight` writes
//! that a trace already carries. No extra RPC calls.

use scale_value::{At, Value, ValueDef};
use serde::{Deserialize, Serialize};

use crate::decode::Metadata;

/// A FRAME `Weight`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Weight {
    /// Computation time, in picoseconds.
    pub ref_time: u64,
    /// Proof size, in bytes.
    pub proof_size: u64,
}

impl Weight {
    fn add(self, other: Self) -> Self {
        Self {
            ref_time: self.ref_time.saturating_add(other.ref_time),
            proof_size: self.proof_size.saturating_add(other.proof_size),
        }
    }
}

/// The final `System::BlockWeight` of a block: weight charged per dispatch class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockWeight {
    pub normal: Weight,
    pub operational: Weight,
    pub mandatory: Weight,
}

impl BlockWeight {
    /// The sum over the three classes.
    pub fn total(&self) -> Weight {
        self.normal.add(self.operational).add(self.mandatory)
    }
}

/// What `ExtrinsicSuccess` or `ExtrinsicFailed` reported for one extrinsic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dispatch {
    /// The extrinsic index from `Phase::ApplyExtrinsic`.
    pub index: u32,
    pub weight: Weight,
    /// `normal`, `operational` or `mandatory`.
    pub class: String,
    /// `yes` or `no`; absent if the event has no `pays_fee`.
    pub pays_fee: Option<String>,
    /// `success` or `failed`.
    pub outcome: &'static str,
}

/// Decodes one appended `EventRecord`. `None` unless it is a `System` `ExtrinsicSuccess` or
/// `ExtrinsicFailed` in an `ApplyExtrinsic` phase with a two-part weight.
pub(crate) fn dispatch(md: &Metadata, record: &[u8]) -> Option<Dispatch> {
    let events = md.storage_value_type("System", "Events")?;
    let ty = match &md.types().resolve(events)?.type_def {
        scale_info::TypeDef::Sequence(seq) => seq.type_param.id,
        _ => return None,
    };
    let record = decode(md, record, ty)?;
    let phase = record.at("phase")?;
    if variant_name(phase)? != "ApplyExtrinsic" {
        return None;
    }
    let index = u32::try_from(phase.at(0)?.as_u128()?).ok()?;
    let event = record.at("event")?;
    if variant_name(event)? != "System" {
        return None;
    }
    let event = event.at(0)?;
    let outcome = match variant_name(event)? {
        "ExtrinsicSuccess" => "success",
        "ExtrinsicFailed" => "failed",
        _ => return None,
    };
    let info = event.at("dispatch_info")?;
    Some(Dispatch {
        index,
        weight: weight(info.at("weight")?)?,
        class: variant_name(info.at("class")?)?.to_lowercase(),
        pays_fee: info
            .at("pays_fee")
            .and_then(variant_name)
            .map(str::to_lowercase),
        outcome,
    })
}

/// Decodes a `System::BlockWeight` value (`PerDispatchClass<Weight>`).
pub(crate) fn block_weight(md: &Metadata, value: &[u8]) -> Option<BlockWeight> {
    let value = decode(md, value, md.storage_value_type("System", "BlockWeight")?)?;
    Some(BlockWeight {
        normal: weight(value.at("normal")?)?,
        operational: weight(value.at("operational")?)?,
        mandatory: weight(value.at("mandatory")?)?,
    })
}

/// The whole value must decode; trailing bytes mean the type does not match.
fn decode(md: &Metadata, bytes: &[u8], ty: u32) -> Option<Value<u32>> {
    let mut cursor = bytes;
    let value = scale_value::scale::decode_as_type(&mut cursor, ty, md.types()).ok()?;
    cursor.is_empty().then_some(value)
}

/// A V2 weight. Old one-number weights have no proof size, so they give `None`.
fn weight(value: &Value<u32>) -> Option<Weight> {
    let part = |name| u64::try_from(value.at(name)?.as_u128()?).ok();
    Some(Weight {
        ref_time: part("ref_time")?,
        proof_size: part("proof_size")?,
    })
}

fn variant_name(value: &Value<u32>) -> Option<&str> {
    match &value.value {
        ValueDef::Variant(variant) => Some(&variant.name),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use scale_value::Value;

    fn metadata() -> Metadata {
        Metadata::from_bytes(include_bytes!("../../fixtures/metadata/westend-v16.scale")).unwrap()
    }

    /// Encodes by field and variant names, so the test does not depend on type indices.
    fn encode(md: &Metadata, ty: u32, value: Value) -> Vec<u8> {
        let mut out = Vec::new();
        scale_value::scale::encode_as_type(&value, ty, md.types(), &mut out).unwrap();
        out
    }

    fn weight_value(ref_time: u128, proof_size: u128) -> Value {
        Value::named_composite([
            ("ref_time", Value::u128(ref_time)),
            ("proof_size", Value::u128(proof_size)),
        ])
    }

    fn record(md: &Metadata, phase: Value, event: Value) -> Vec<u8> {
        let events = md.storage_value_type("System", "Events").unwrap();
        let scale_info::TypeDef::Sequence(seq) = &md.types().resolve(events).unwrap().type_def
        else {
            panic!("System::Events is a Vec")
        };
        let value = Value::named_composite([
            ("phase", phase),
            ("event", Value::unnamed_variant("System", [event])),
            ("topics", Value::unnamed_composite([])),
        ]);
        encode(md, seq.type_param.id, value)
    }

    fn info(class: &str, pays: &str) -> Value {
        Value::named_composite([
            ("weight", weight_value(475_773_000, 6283)),
            ("class", Value::unnamed_variant(class, [])),
            ("pays_fee", Value::unnamed_variant(pays, [])),
        ])
    }

    #[test]
    fn extrinsic_success_and_failed_give_the_charged_weight() {
        let md = metadata();
        let apply = |i| Value::unnamed_variant("ApplyExtrinsic", [Value::u128(i)]);
        let success = record(
            &md,
            apply(2),
            Value::named_variant(
                "ExtrinsicSuccess",
                [("dispatch_info", info("Normal", "Yes"))],
            ),
        );
        assert_eq!(
            dispatch(&md, &success),
            Some(Dispatch {
                index: 2,
                weight: Weight {
                    ref_time: 475_773_000,
                    proof_size: 6283
                },
                class: "normal".into(),
                pays_fee: Some("yes".into()),
                outcome: "success",
            })
        );
        let failed = record(
            &md,
            apply(7),
            Value::named_variant(
                "ExtrinsicFailed",
                [
                    ("dispatch_error", Value::unnamed_variant("BadOrigin", [])),
                    ("dispatch_info", info("Operational", "No")),
                ],
            ),
        );
        let failed = dispatch(&md, &failed).unwrap();
        assert_eq!(
            (failed.index, failed.outcome, failed.class.as_str()),
            (7, "failed", "operational")
        );
        assert_eq!(failed.pays_fee.as_deref(), Some("no"));
        // Other events, other phases and bytes that do not decode in full give nothing.
        let other = record(
            &md,
            apply(1),
            Value::named_variant(
                "Remarked",
                [
                    ("sender", Value::from_bytes([1; 32])),
                    ("hash", Value::from_bytes([2; 32])),
                ],
            ),
        );
        assert_eq!(dispatch(&md, &other), None);
        let finalization = record(
            &md,
            Value::unnamed_variant("Finalization", []),
            Value::named_variant(
                "ExtrinsicSuccess",
                [("dispatch_info", info("Normal", "Yes"))],
            ),
        );
        assert_eq!(dispatch(&md, &finalization), None);
        let mut trailing = success.clone();
        trailing.push(0);
        assert_eq!(dispatch(&md, &trailing), None);
        assert_eq!(dispatch(&md, &success[..success.len() - 1]), None);
    }

    #[test]
    fn block_weight_decodes_every_class() {
        let md = metadata();
        let ty = md.storage_value_type("System", "BlockWeight").unwrap();
        let value = encode(
            &md,
            ty,
            Value::named_composite([
                ("normal", weight_value(1, 30_385)),
                ("operational", weight_value(2, 0)),
                ("mandatory", weight_value(3, 261_722)),
            ]),
        );
        let weight = block_weight(&md, &value).unwrap();
        assert_eq!(weight.mandatory.proof_size, 261_722);
        assert_eq!(
            weight.total(),
            Weight {
                ref_time: 6,
                proof_size: 292_107
            }
        );
        assert_eq!(block_weight(&md, &value[1..]), None);
    }
}
