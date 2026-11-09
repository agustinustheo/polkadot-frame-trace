//! Storage keys polkadot-frame-trace needs to know without runtime metadata.

/// `:extrinsic_index` (`sp_storage::well_known_keys::EXTRINSIC_INDEX`).
pub const EXTRINSIC_INDEX: &[u8] = b":extrinsic_index";

/// `twox128("System")`.
pub(crate) const SYSTEM_PREFIX: [u8; 16] = hex16("26aa394eea5630e07c48ae0c9558cef7");

/// `System::ExecutionPhase`: `twox128("System") ++ twox128("ExecutionPhase")`.
pub(crate) const EXECUTION_PHASE: [u8; 32] =
    concat(SYSTEM_PREFIX, hex16("ff553b5a9862a516939d82b3d3d8661a"));

/// Prefix of the `System::ExtrinsicData` map: `twox128("System") ++ twox128("ExtrinsicData")`.
/// The map is `Twox64Concat<u32> -> Vec<u8>`.
pub(crate) const EXTRINSIC_DATA_PREFIX: [u8; 32] =
    concat(SYSTEM_PREFIX, hex16("df1daeb8986837f21cc5d17596bb78d1"));

/// `System::ExtrinsicWeightReclaimed`, killed by `note_applied_extrinsic` right after it writes
/// the next `ExecutionPhase`.
pub const EXTRINSIC_WEIGHT_RECLAIMED: [u8; 32] =
    concat(SYSTEM_PREFIX, hex16("0ccf055743738b7a91a6fb88ece33cac"));

/// `System::InherentsApplied`, set to `true` once per block by `note_inherents_applied`.
pub(crate) const INHERENTS_APPLIED: [u8; 32] =
    concat(SYSTEM_PREFIX, hex16("156afe25323c70e43eb722ed7c886297"));

/// The extrinsic index of a `System::ExtrinsicData` key, if `key` is one.
pub(crate) fn extrinsic_data_index(key: &[u8]) -> Option<u32> {
    // prefix (32) ++ twox64 (8) ++ u32 little endian (4)
    let rest = key.strip_prefix(&EXTRINSIC_DATA_PREFIX[..])?;
    let index: [u8; 4] = rest.get(8..)?.try_into().ok()?;
    Some(u32::from_le_bytes(index))
}

const fn hex16(s: &str) -> [u8; 16] {
    let s = s.as_bytes();
    assert!(s.len() == 32, "expected 32 hex digits");
    let mut out = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        out[i] = nibble(s[2 * i]) << 4 | nibble(s[2 * i + 1]);
        i += 1;
    }
    out
}

const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => panic!("expected a lowercase hex digit"),
    }
}

const fn concat(a: [u8; 16], b: [u8; 16]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 16 {
        out[i] = a[i];
        out[16 + i] = b[i];
        i += 1;
    }
    out
}
