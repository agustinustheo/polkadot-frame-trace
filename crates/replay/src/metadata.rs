//! Metadata used for names and JSON arguments must come from authenticated code/state.
use crate::*;
use std::{collections::BTreeMap, sync::Arc};

fn negotiate(mut run: impl FnMut(&str, &[u8]) -> Result<Vec<u8>>) -> Result<String> {
    for version in [16u32, 15] {
        let bytes = match run("Metadata_metadata_at_version", &version.encode()) {
            Ok(bytes) => bytes,
            Err(Error::MethodNotFound(_)) => break,
            Err(error) => return Err(error),
        };
        let mut cursor = bytes.as_slice();
        let decoded = Option::<Vec<u8>>::decode(&mut cursor)
            .map_err(|e| Error::Invalid(format!("local metadata response: {e}")))?;
        if !cursor.is_empty() {
            return Err(Error::Invalid("local metadata: trailing bytes".into()));
        }
        if let Some(bytes) = decoded {
            return Ok(rpc::hex0(&bytes));
        }
    }
    let bytes = run("Metadata_metadata", &[])?;
    let mut cursor = bytes.as_slice();
    let decoded = Vec::<u8>::decode(&mut cursor)
        .map_err(|e| Error::Invalid(format!("local V14 metadata response: {e}")))?;
    if !cursor.is_empty() {
        return Err(Error::Invalid("local V14 metadata: trailing bytes".into()));
    }
    Ok(rpc::hex0(&decoded))
}

pub(crate) fn load(
    artifact: &mut Artifact,
    db: &ProofDb,
    wasm: &[u8],
    heap: Option<u64>,
    session: &ReplaySession,
    state: &Header,
) -> Result<()> {
    let hash = H256::from(sp_io::hashing::blake2_256(wasm));
    let cached = session.metadata.lock().get(&hash).cloned();
    if let Some(entry) = cached {
        use sp_state_machine::Backend;
        let backend = TracedBackend::new(db.clone(), *state.state_root(), Default::default());
        let validation = entry.matches(*state.state_root(), |key, child| {
            match child {
                Some(child) => {
                    backend.child_storage(&sp_core::storage::ChildInfo::new_default(child), key)
                }
                None => backend.storage(key),
            }
            .map(|value| value.map(|v| H256::from(sp_io::hashing::blake2_256(&v))))
            .map_err(|e| db.take_error(e))
        });
        let valid = match validation {
            Err(error) if error.is_transient() || matches!(error, Error::Cancelled) => {
                return Err(error);
            }
            other => other.unwrap_or(false),
        };
        db.failure.lock().take();
        if valid {
            if entry.iteration_root.is_some() {
                db.retain_nodes(&entry.proof_nodes)?;
            }
            artifact.metadata = entry.metadata.clone();
            return Ok(());
        }
    }
    #[cfg(test)]
    session.metadata_runs.fetch_add(1, Ordering::Relaxed);
    let executor = session.executor_for(wasm);
    let capture = Capture::new();
    let mut proof_nodes = std::collections::BTreeSet::new();
    let result = negotiate(|method, data| {
        let result = execute_method(
            db,
            *state.state_root(),
            wasm,
            heap,
            &executor,
            method,
            data,
            state.hash(),
            CallContext::Offchain,
            &capture,
        )
        .map(|(_, output)| output);
        if let Some(recorder) = &capture.0.lock().recorder {
            proof_nodes.extend(recorder.to_storage_proof().into_iter_nodes());
        }
        result
    });
    // A failed optional metadata export must not poison a subsequent --data execution.
    db.failure.lock().take();
    // Offline artifacts need only cover the requested execution. Optional metadata may
    // read additional state; a missing node here does not invalidate the saved call.
    let unavailable_reason = |error: &Error| match error {
        Error::Invalid(detail)
            if db.rpc().is_none()
                && detail.starts_with("proof node ")
                && detail.contains("missing/invalid at path ") =>
        {
            "saved proof does not cover metadata execution".to_owned()
        }
        other => other.to_string(),
    };
    match result {
        Ok(metadata) => {
            let events = &capture.0.lock().events;
            let mut reads = BTreeMap::new();
            let mut iteration = false;
            for event in events {
                let method = event
                    .fields
                    .get("method")
                    .map(String::as_str)
                    .unwrap_or_default();
                if method == "BackendRead" {
                    let key = unhex(&event.fields["key"])?;
                    let child = event
                        .fields
                        .get("child_info")
                        .filter(|c| !c.is_empty())
                        .map(|c| unhex(c))
                        .transpose()?;
                    let value = event
                        .fields
                        .get("value_hash")
                        .filter(|v| *v != "none")
                        .map(|v| v.parse::<H256>().map_err(|e| Error::Invalid(e.to_string())))
                        .transpose()?;
                    reads.insert((key, child), value);
                } else if root_dependent(method) {
                    iteration = true;
                }
            }
            let mut cache = session.metadata.lock();
            if cache.len() >= 4 {
                cache.clear();
            }
            cache.insert(
                hash,
                Arc::new(session::MetadataEntry {
                    metadata: metadata.clone(),
                    reads: reads
                        .into_iter()
                        .map(|((key, child), value)| (key, child, value))
                        .collect(),
                    iteration_root: iteration.then_some(*state.state_root()),
                    proof_nodes: if iteration {
                        proof_nodes.into_iter().collect()
                    } else {
                        Vec::new()
                    },
                }),
            );
            artifact.metadata = metadata;
        }
        // The block cannot run either; say why now instead of after a metadata notice.
        Err(error)
            if error.is_transient()
                || matches!(error, Error::Cancelled | Error::MissingHostFunctions { .. }) =>
        {
            return Err(error);
        }
        Err(error) => {
            note!(
                "notice: local metadata unavailable ({}); using unnamed storage and hex results",
                unavailable_reason(&error)
            );
            artifact.metadata.clear();
        }
    }
    Ok(())
}

impl session::MetadataEntry {
    fn matches(
        &self,
        root: H256,
        mut read: impl FnMut(&[u8], Option<&[u8]>) -> Result<Option<H256>>,
    ) -> Result<bool> {
        if self.iteration_root.is_some_and(|r| r != root) {
            return Ok(false);
        }
        for (key, child, expected) in &self.reads {
            if read(key, child.as_deref())? != *expected {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

// Whole-state roots can affect metadata even without a BackendRead event.
fn root_dependent(method: &str) -> bool {
    method.starts_with("Backend") || matches!(method, "StorageRoot" | "ChildStorageRoot")
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[test]
    fn cached_metadata_follows_dependency_values_and_cache_hits_save_and_execute_in_a_fresh_process()
     {
        const CHILD_PATH: &str = "FRAME_TRACE_R5_METADATA_ARTIFACT";
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            let mut artifact = Artifact::load(std::path::Path::new(&path)).unwrap();
            let expected = artifact.metadata.clone();
            let session = ReplaySession::new().unwrap();
            let state: Header = serde_json::from_value(artifact.parent_header.clone()).unwrap();
            let db = open_db(&artifact, None, &session, &state).unwrap();
            let (wasm, heap) = runtime_code(&db, *state.state_root()).unwrap();
            load(&mut artifact, &db, &wasm, heap, &session, &state).unwrap();
            assert_eq!(artifact.metadata, expected);
            assert_eq!(session.metadata_runs.load(Ordering::Relaxed), 1);
            return;
        }
        // A cached entry stays valid while the values it read are unchanged, whatever the state
        // root, unless the metadata iterated or took a root.
        let mut entry = session::MetadataEntry {
            metadata: "metadata".into(),
            reads: vec![(b"key".to_vec(), None, Some(H256::repeat_byte(1)))],
            iteration_root: None,
            proof_nodes: vec![],
        };
        let matches = |entry: &session::MetadataEntry, value| {
            entry
                .matches(H256::repeat_byte(3), |_, _| Ok(value))
                .unwrap()
        };
        assert!(matches(&entry, Some(H256::repeat_byte(1))));
        assert!(!matches(&entry, Some(H256::repeat_byte(2))));
        assert!(!matches(&entry, None));
        entry.iteration_root = Some(H256::repeat_byte(4));
        assert!(!matches(&entry, Some(H256::repeat_byte(1))));
        for operation in ["read", "child", "iterate", "root", "child_root"] {
            let (import, body) = match operation {
                "read" => (
                    r#"(import "env" "ext_storage_get_version_1" (func $op (param i64) (result i64)))"#,
                    "(drop (call $op (i64.const 12884901988)))",
                ),
                "child" => (
                    r#"(import "env" "ext_default_child_storage_get_version_1" (func $op (param i64 i64) (result i64)))"#,
                    "(drop (call $op (i64.const 21474836590) (i64.const 12884901988)))",
                ),
                "iterate" => (
                    r#"(import "env" "ext_storage_next_key_version_1" (func $op (param i64) (result i64)))"#,
                    "(drop (call $op (i64.const 12884901988)))",
                ),
                "child_root" => (
                    r#"(import "env" "ext_default_child_storage_root_version_2" (func $op (param i64 i32) (result i64)))"#,
                    "(drop (call $op (i64.const 21474836590) (i32.const 1)))",
                ),
                _ => (
                    r#"(import "env" "ext_storage_root_version_2" (func $op (param i32) (result i64)))"#,
                    "(drop (call $op (i32.const 1)))",
                ),
            };
            let wasm = wat::parse_str(format!(
                r#"(module {import}
                (memory (export "memory") 1)
                (global (export "__heap_base") i32 (i32.const 1024))
                (data (i32.const 100) "key") (data (i32.const 110) "child")
                (data (i32.const 200) "\01\10meta")
                (func (export "Metadata_metadata_at_version") (param i32 i32) (result i64)
                    {body} (i64.const 25769803976)))"#
            ))
            .unwrap();
            let mut seed =
                sp_state_machine::TestExternalities::<Blake2Hasher>::new_with_state_version(
                    Default::default(),
                    sp_core::storage::StateVersion::V1,
                );
            seed.execute_with(|| {
                sp_io::storage::set(b":code", &wasm);
                sp_io::storage::set(b"key", &[7; 128]);
                sp_io::storage::set(b"zzz", &[9; 128]);
                sp_io::default_child_storage::set(b"child", b"key", &[8; 128]);
            });
            seed.commit_all().unwrap();
            let (raw, root) = seed.into_raw_snapshot();
            let nodes: Vec<_> = raw.into_iter().map(|(_, (node, _))| node).collect();
            let state = Header::new(1, H256::zero(), root, H256::zero(), Default::default());
            let mut artifact = Artifact {
                format_version: 1,
                kind: ArtifactKind::Call,
                call: Some(call::Call {
                    method: "Metadata_metadata_at_version".into(),
                    data: "0x10000000".into(),
                    result: None,
                    oracle: None,
                }),
                block_hash: state.hash(),
                block: json!({"header": state, "extrinsics": []}),
                parent_header: json!(state),
                runtime_version: json!({}),
                metadata: String::new(),
                proof_nodes: vec![],
            };
            let session = ReplaySession::new().unwrap();
            for _ in 0..2 {
                // Each database has a fresh export set, as for another block after a cache hit.
                let db = ProofDb::with_session(None, state.hash(), session.clone());
                db.insert_nodes(nodes.clone());
                let (wasm, heap) = runtime_code(&db, root).unwrap();
                load(&mut artifact, &db, &wasm, heap, &session, &state).unwrap();
                assert_eq!(artifact.metadata, "0x6d657461", "{operation}");
                artifact.proof_nodes = db.export();
            }
            assert_eq!(
                session.metadata_runs.load(Ordering::Relaxed),
                1,
                "cache hit: {operation}"
            );
            if operation.ends_with("root") {
                let cache = session.metadata.lock();
                let entry = cache.values().next().unwrap();
                assert_eq!(entry.iteration_root, Some(root));
                assert!(
                    !entry
                        .matches(H256::repeat_byte(99), |_, _| panic!(
                            "root invalidation precedes reads"
                        ))
                        .unwrap()
                );
            }
            let dir = std::env::temp_dir().join(format!(
                "frame-r5-metadata-{}-{operation}",
                std::process::id()
            ));
            artifact.save(&dir).unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "metadata::tests::cached_metadata_follows_dependency_values_and_cache_hits_save_and_execute_in_a_fresh_process", "--nocapture"])
                .env(CHILD_PATH, &dir).output().unwrap();
            std::fs::remove_dir_all(dir).unwrap();
            assert!(
                output.status.success(),
                "{operation}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
