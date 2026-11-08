//! Exercise operations absent from the eight People blocks. This is host/API
//! coverage against a complete local trie, NOT remote child-proof coverage.
#![allow(clippy::unwrap_used)]
use super::*;
use sp_core::storage::StateVersion;
use std::collections::BTreeSet;

/// Runs `run` against a local trie seeded by `seed`, with a proof recorder and the proof size
/// extension, and returns what the capture saw and the recorder.
fn traced(
    seed: impl FnOnce(),
    run: impl FnOnce(&Recorder<Blake2Hasher>),
) -> (Capture, Recorder<Blake2Hasher>) {
    let mut state = sp_state_machine::TestExternalities::<Blake2Hasher>::default();
    state.execute_with(seed);
    state.commit_all().unwrap();
    let (raw, root) = state.into_raw_snapshot();
    let db = ProofDb::new(None, H256::zero());
    db.insert_nodes(raw.into_iter().map(|(_, (node, _))| node));
    let recorder = Recorder::default();
    let backend = TracedBackend::new(db, root, recorder.clone());
    let mut overlay = OverlayedChanges::default();
    let mut extensions = Extensions::default();
    extensions.register(ProofSizeExt::new(recorder.clone()));
    let mut ext = sp_state_machine::Ext::new(&mut overlay, &backend, Some(&mut extensions));
    let capture = Capture::new();
    capture.0.lock().recorder = Some(recorder.clone());
    tracing::dispatcher::with_default(&tracing::Dispatch::new(capture.clone()), || {
        sp_externalities::set_and_run_with_externalities(&mut ext, || run(&recorder));
    });
    (capture, recorder)
}

#[test]
fn captures_keyless_child_iteration_and_overlay_accesses() {
    let (capture, _) = traced(
        || {
            sp_io::storage::set(b"a", b"one");
            sp_io::storage::set(b"b", b"two");
            sp_io::default_child_storage::set(b"child", b"c1", b"three");
            sp_io::default_child_storage::set(b"child", b"c2", b"four");
        },
        |_| {
            sp_io::storage::set(b"aa", b"overlay");
            assert_eq!(sp_io::storage::get(b"aa").unwrap().as_ref(), b"overlay");
            assert_eq!(sp_io::storage::next_key(b"a"), Some(b"aa".to_vec()));
            assert_eq!(
                sp_io::default_child_storage::next_key(b"child", b"c1"),
                Some(b"c2".to_vec())
            );
            let _ = sp_io::default_child_storage::clear_prefix(b"child", b"c1", None);
            let _ = sp_io::default_child_storage::root(b"child", StateVersion::V1);
            let _ = sp_io::default_child_storage::storage_kill(b"child", None);
            let _ = sp_io::storage::clear_prefix(b"b", None);
            let _ = sp_io::storage::root(StateVersion::V1);
        },
    );
    let d = capture.0.lock();
    let methods: BTreeSet<_> = d
        .events
        .iter()
        .filter_map(|e| e.fields.get("method").map(String::as_str))
        .collect();
    for expected in [
        "Put",
        "Get",
        "ClearPrefix",
        "ChildKill",
        "ChildClearPrefix",
        "ChildStorageRoot",
        "StorageRoot",
        "BackendNextKey",
        "BackendChildNextKey",
        "BackendIterNextKey",
    ] {
        assert!(
            methods.contains(expected),
            "missing {expected}: {methods:?}"
        );
    }
    assert!(
        d.spans
            .iter()
            .any(|s| s.name == "next_key_version_1" && s.duration_ns > 0)
    );
}

/// Calls and block import see different proof sizes, and both get a runtime version reader:
/// invalid or empty code returns `None` rather than trapping on a missing extension.
#[test]
fn call_and_import_expose_different_proof_size_policies_and_a_runtime_version_reader() {
    let wasm = wat::parse_str(r#"(module
        (import "env" "ext_storage_proof_size_storage_proof_size_version_1" (func $size (result i64)))
        (import "env" "ext_misc_runtime_version_version_1" (func $version (param i64) (result i64)))
        (memory (export "memory") 1)
        (global (export "__heap_base") i32 (i32.const 1024))
        (func (export "Probe_size") (param i32 i32) (result i64)
            (i64.store (i32.const 8) (call $size))
            (i64.const 34359738376))
        (func (export "Probe_version") (param i32 i32) (result i64)
            (call $version (i64.const 0))))"#).unwrap();
    let session = ReplaySession::new(None).unwrap();
    let db = ProofDb::new(None, H256::zero());
    let executor = session.executor_for(&wasm);
    let run = |method, context| {
        execute_method(
            &db,
            H256::zero(),
            &wasm,
            None,
            &executor,
            method,
            &[],
            H256::zero(),
            context,
            &Capture::new(),
        )
        .unwrap()
    };
    for (context, expected) in [
        (CallContext::Offchain, u64::MAX),
        (CallContext::Onchain { import: true }, 0),
    ] {
        let (bytes, output) = run("Probe_size", context);
        assert_eq!(output, expected.to_le_bytes());
        assert_eq!(bytes, 0);
        assert_eq!(run("Probe_version", context).1, [0]);
    }
}

#[test]
fn read_origins_observe_backend_access_recorder_warmth_overlay_and_rollback_for_all_read_kinds() {
    let child = || sp_core::storage::ChildInfo::new_default(b"child");
    let (capture, recorder) = traced(
        || {
            sp_io::storage::set(b"key", b"value");
            sp_io::default_child_storage::set(b"child", b"key", b"value");
        },
        |_| {
            let _ = sp_io::storage::get(b"key");
            let _ = sp_io::storage::get(b"key");
            let _ = sp_io::storage::exists(b"key");
            let _ = sp_externalities::with_externalities(|ext| ext.storage_hash(b"key"));
            sp_io::storage::set(b"key", b"overlay");
            let _ = sp_io::storage::get(b"key");
            let _ = sp_io::storage::exists(b"key");
            let _ = sp_externalities::with_externalities(|ext| ext.storage_hash(b"key"));
            let _ = sp_io::default_child_storage::get(b"child", b"key");
            let _ = sp_io::default_child_storage::exists(b"child", b"key");
            let _ = sp_externalities::with_externalities(|ext| {
                ext.child_storage_hash(&child(), b"key")
            });
            sp_io::default_child_storage::set(b"child", b"key", b"overlay");
            let _ = sp_io::default_child_storage::get(b"child", b"key");
            let _ = sp_io::default_child_storage::exists(b"child", b"key");
            let _ = sp_externalities::with_externalities(|ext| {
                ext.child_storage_hash(&child(), b"key")
            });
        },
    );
    let d = capture.0.lock();
    let reads: Vec<_> = d
        .events
        .iter()
        .filter(|e| e.fields.contains_key("origin"))
        .collect();
    let origins: Vec<_> = reads.iter().map(|e| e.fields["origin"].as_str()).collect();
    assert_eq!(
        origins,
        [
            "backend_cold",
            "backend_warm",
            "backend_warm",
            "backend_warm",
            "overlay",
            "overlay",
            "overlay",
            "backend_cold",
            "backend_warm",
            "backend_warm",
            "overlay",
            "overlay",
            "overlay"
        ]
    );
    let proof: usize = d
        .events
        .iter()
        .filter(|e| e.fields.get("method").is_some_and(|m| m != "BackendRead"))
        .filter_map(|e| e.fields.get("proof_bytes_added"))
        .map(|n| n.parse::<usize>().unwrap())
        .sum();
    assert_eq!(proof, recorder.estimate_encoded_size());
    assert!(
        reads[0].fields["proof_bytes_added"]
            .parse::<usize>()
            .unwrap()
            > 0
    );

    // A recorder rollback keeps block-lifetime warmth but reports the new marginal growth.
    let (capture, _) = traced(
        || sp_io::storage::set(b"key", b"value"),
        |recorder| {
            // The SDK ignores runtime proof transactions; emulate an executor/host
            // recorder transaction to exercise actual recorder shrinkage as well.
            recorder.start_transaction();
            sp_io::storage::start_transaction();
            let _ = sp_io::storage::get(b"key");
            recorder.rollback_transaction().unwrap();
            sp_io::storage::rollback_transaction();
            let _ = sp_io::storage::get(b"key");
        },
    );
    let d = capture.0.lock();
    let reads: Vec<_> = d
        .events
        .iter()
        .filter(|e| e.fields.get("method").is_some_and(|m| m == "Get"))
        .collect();
    assert_eq!(reads[0].fields["origin"], "backend_cold");
    assert_eq!(reads[1].fields["origin"], "backend_warm");
    assert!(
        reads[1].fields["proof_bytes_added"]
            .parse::<usize>()
            .unwrap()
            > 0
    );
}

/// Proof growth belongs to the access that caused it: an append that reads the backend, not
/// a later overlay read; not an overlay append, but the next backend read; and an iteration,
/// which warms a later read of a value inlined in the same node.
#[test]
fn appends_and_iteration_own_exactly_the_backend_proof_growth_they_cause() {
    // The origin, if the access has one, and the proof bytes it added.
    let find = |capture: &Capture, method: &str| {
        let d = capture.0.lock();
        let fields = &d
            .events
            .iter()
            .find(|e| e.fields.get("method").is_some_and(|m| m == method))
            .unwrap()
            .fields;
        (
            fields.get("origin").cloned().unwrap_or_default(),
            fields["proof_bytes_added"].parse::<usize>().unwrap(),
        )
    };
    let (capture, recorder) = traced(
        || sp_io::storage::set(b"key", &vec![7u8; 63].encode()),
        |_| {
            sp_io::storage::set(b"unrelated", b"overlay");
            sp_io::storage::append(b"key", vec![8]);
            assert_eq!(
                sp_io::storage::get(b"unrelated").unwrap().as_ref(),
                b"overlay"
            );
        },
    );
    let (_, growth) = find(&capture, "Append");
    assert_eq!(growth, 148);
    assert_eq!(growth, recorder.estimate_encoded_size());
    assert_eq!(find(&capture, "Get"), ("overlay".into(), 0));

    let (capture, _) = traced(
        || sp_io::storage::set(b"backend", &[1; 80]),
        |_| {
            sp_io::storage::set(b"overlay", &[0]);
            sp_io::storage::append(b"overlay", vec![1]);
            let _ = sp_io::storage::get(b"backend");
        },
    );
    assert_eq!(find(&capture, "Append").1, 0);
    let (origin, growth) = find(&capture, "Get");
    assert_eq!(origin, "backend_cold");
    assert!(growth > 0);

    let (capture, recorder) = traced(
        || {
            sp_io::storage::set(b"a", b"one");
            sp_io::storage::set(b"b", b"two");
        },
        |_| {
            assert_eq!(sp_io::storage::next_key(b"a"), Some(b"b".to_vec()));
            assert_eq!(sp_io::storage::get(b"b").unwrap().as_ref(), b"two");
        },
    );
    assert_eq!(
        find(&capture, "BackendNextKey").1,
        recorder.estimate_encoded_size()
    );
    assert_eq!(find(&capture, "Get"), ("backend_warm".into(), 0));
}
