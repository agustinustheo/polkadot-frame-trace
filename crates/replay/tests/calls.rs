//! Runtime API calls replayed from saved fixtures, without any network connection.
#![allow(clippy::unwrap_used)]
use polkadot_frame_trace_model::TracedBlock;
use polkadot_frame_trace_replay::{Artifact, ReplaySession, call};
use std::path::PathBuf;
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/replay")
        .join(name)
}
#[test]
fn offline_calls_reject_wrong_result_state_and_missing_proofs() {
    let original = Artifact::load(&fixture("call-people-nonce")).unwrap();
    let session = ReplaySession::new().unwrap();
    let mut artifact = original.clone();
    artifact.call.as_mut().unwrap().result = Some("0x01000000".into());
    assert!(
        call::replay(artifact, None, &session, false)
            .unwrap_err()
            .to_string()
            .contains("recorded call result")
    );
    let mut artifact = original.clone();
    artifact.parent_header["stateRoot"] = serde_json::json!(format!("0x{}", "42".repeat(32)));
    assert!(call::replay(artifact, None, &session, false).is_err());
    let mut artifact = original.clone();
    artifact.proof_nodes.clear();
    assert!(call::replay(artifact, None, &session, false).is_err());
    let mut artifact = original.clone();
    artifact.call.as_mut().unwrap().data = "0xff".into();
    let error = call::replay(artifact, None, &session, false)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("runtime rejected the call input:"),
        "{error}"
    );
    assert!(
        !error.contains("root") && !error.contains("backtrace"),
        "{error}"
    );
    assert!(polkadot_frame_trace_replay::replay(original.clone(), None).is_err());
    assert!(call::replay(original, None, &session, true).is_err());
}

#[test]
fn offline_dry_run_set_empty_code_uses_version_reader_and_matches_recorded_oracle() {
    let replay = call::replay(
        Artifact::load(&fixture("call-westend-dryrun")).unwrap(),
        None,
        &ReplaySession::new().unwrap(),
        false,
    )
    .unwrap();
    assert_eq!(replay.block_number, 18_563_012);
    assert_eq!(replay.oracle_match, Some(true));
    assert_eq!(replay.proof_bytes, 3047);
    assert_eq!(replay.measured_rpc_count, 0);
    assert!(!replay.artifact.metadata.is_empty());
    let trace = TracedBlock::from_call(&replay.capture).unwrap();
    assert_eq!(trace.ops.len(), 24);
}

#[test]
fn stored_metadata_is_ignored_and_unused_nodes_are_not_exported() {
    let mut artifact = Artifact::load(&fixture("call-westend-nonce")).unwrap();
    let original = artifact.clone();
    artifact.metadata = "lying metadata".into();
    let junk = "0xdeadbeefcafebabe";
    artifact.proof_nodes.push(junk.into());
    let replay = call::replay(artifact, None, &ReplaySession::new().unwrap(), false).unwrap();
    assert_eq!(replay.result, 520u32.to_le_bytes());
    assert_eq!(replay.artifact.metadata, original.metadata);
    assert!(!replay.artifact.proof_nodes.iter().any(|node| node == junk));
    assert!(call::replay(replay.artifact, None, &ReplaySession::new().unwrap(), false).is_ok());
}
