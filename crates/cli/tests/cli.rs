//! Runs the binary on the handcrafted fixtures.

#![allow(clippy::unwrap_used)] // Tests may panic on bad fixtures.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::{Command, Output},
};

static REPLAY_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const BIN: &str = env!("CARGO_BIN_EXE_polkadot-frame-trace");

fn fixture(path: &str) -> String {
    format!("{}/../../fixtures/{path}", env!("CARGO_MANIFEST_DIR"))
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("binary runs")
}

#[test]
fn ignored_source_options_are_rejected_before_loading_or_connecting() {
    let rpc = "http://127.0.0.1:1";
    // A missing source or input mode, or two of them at once.
    let usage: Vec<Vec<&str>> = vec![
        vec!["block"],
        vec!["call"],
        vec!["report"],
        vec!["call", "--rpc", rpc],
        vec!["call", "--rpc", rpc, "--method", "Core_version"],
        vec![
            "call", "--rpc", rpc, "--method", "m", "--data", "0x", "--args", "[]",
        ],
        vec!["call", "--offline", "missing", "--rpc", rpc],
        vec!["call", "--offline", "missing", "--data", "0x"],
        vec!["report", "--rpc", rpc, "missing.json"],
        vec!["block", "--offline", "missing", "--from-json", "missing"],
    ];
    // A source with an option it would ignore.
    let mut ignored = vec![vec!["call", "--offline", "missing", "--check-state-call"]];
    ignored.push(vec!["block", "--from-json", "missing", "--block", "1"]);
    for flags in [
        ["--from", "1"],
        ["--to", "2"],
        ["--last", "2"],
        ["--workers", "2"],
    ] {
        ignored.push([&["report", "missing.json"][..], &flags].concat());
    }
    for (args, expected) in usage.iter().map(|args| (args, None)).chain(
        ignored
            .iter()
            .map(|args| (args, Some("cannot be used with"))),
    ) {
        let out = run(args);
        let error = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {error}");
        if let Some(expected) = expected {
            assert!(error.contains(expected), "{args:?}: {error}");
        }
    }
}

#[test]
fn rpc_source_reports_connection_failure() {
    let url = "ws://alice9:secret@127.0.0.1:1/path?apikey=k3y";
    let out = run(&["block", "--rpc", url, "--block", "1"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("connect ws://127.0.0.1:1:"), "{stderr}");
    for hidden in ["alice9", "secret", "apikey", "k3y", "/path"] {
        assert!(!stderr.contains(hidden), "{stderr}");
    }
}

#[test]
fn duplicate_reads_are_grouped_by_key_not_by_name() {
    let get = |method: &str, key: &str| {
        let child = if method == "ChildGet" {
            r#""child_info":"01","#
        } else {
            ""
        };
        format!(
            r#"{{"target":"state","data":{{"stringValues":{{"method":"{method}",{child}"key":"{key}","result":"None"}}}},"parentId":null}}"#
        )
    };
    let exists = r#"{"target":"state","data":{"stringValues":{"method":"Exists","key":"aa","result":"true"}},"parentId":null}"#;
    let json = format!(
        r#"{{"blockHash":"0x01","parentHash":"0x00","events":[{},{},{},{},{exists},{},{},{},{}]}}"#,
        get("Get", "aa"),
        get("Get", "aa"),
        get("Get", "bb"),
        get("Get", "bb"),
        get("ChildGet", "aa"),
        get("ChildGet", "aa"),
        get("ChildGet", "bb"),
        get("ChildGet", "bb"),
    );
    let path = std::env::temp_dir().join(format!(
        "polkadot-frame-trace-dups-{}.json",
        std::process::id()
    ));
    let raw = polkadot_frame_trace_model::rpc::parse_trace_block(&json).unwrap();
    let traced = polkadot_frame_trace_model::TracedBlock::from_trace(&raw).unwrap();
    let block = polkadot_frame_trace_decode::annotate(&traced, None);
    std::fs::write(&path, serde_json::to_vec(&block).unwrap()).unwrap();
    let out = run(&["block", "--from-json", path.to_str().unwrap()]);
    std::fs::remove_file(&path).ok();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).unwrap();
    let rows: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("unsegmented") && l.contains("<unknown>"))
        .collect();
    // aa: re-read by get and exists (2), bb: once (1). One row per key.
    assert_eq!(rows.len(), 2, "{stdout}");
    assert!(rows[0].contains("[0xaa]") && rows[0].trim_end_matches('│').trim_end().ends_with('2'));
    assert!(rows[1].contains("[0xbb]") && rows[1].trim_end_matches('│').trim_end().ends_with('1'));
    // Child keys are named by their trie and grouped by key too.
    let rows: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("unsegmented") && l.contains("child:"))
        .collect();
    assert_eq!(rows.len(), 2, "{stdout}");
    assert!(
        rows[0].contains("child:0x01[0xaa]") && !rows[0].contains(" ["),
        "{stdout}"
    );
    assert!(rows[1].contains("child:0x01[0xbb]"), "{stdout}");
}

/// A one-shot OTLP/HTTP receiver that waits `secs` for a request, answers with `status` and
/// `body`, and returns the request body.
fn otlp_response(
    status: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
    secs: u64,
) -> (String, std::thread::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(std::time::Instant::now() < deadline, "receiver timed out");
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => panic!("accept: {e}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut length = 0;
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "POST /v1/traces HTTP/1.1\r\n");
        loop {
            line.clear();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse::<usize>().unwrap();
            }
        }
        assert!(length > 0);
        let mut request = vec![0; length];
        reader.read_exact(&mut request).unwrap();
        write!(stream, "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len()).unwrap();
        stream.write_all(&body).unwrap();
        request
    });
    (url, server)
}

#[test]
fn block_fails_when_the_collector_rejects_spans() {
    let path = std::env::temp_dir().join(format!("frame-collector-{}.json", std::process::id()));
    let raw = polkadot_frame_trace_model::rpc::parse_trace_block(include_str!(
        "../../../fixtures/model/real/people-paseo-block-1-trace-state.json"
    ))
    .unwrap();
    let traced = polkadot_frame_trace_model::TracedBlock::from_trace(&raw).unwrap();
    let block = polkadot_frame_trace_decode::annotate(&traced, None);
    std::fs::write(&path, serde_json::to_vec(&block).unwrap()).unwrap();
    const PROTOBUF: &str = "application/x-protobuf";
    const JSON: &str = "application/json; charset=utf-8";
    // 1 block + 5 phases + 315 ops.
    let rejected = "7 of 321 spans rejected by the receiver: quota";
    for (status, content_type, body, code, expected) in [
        (
            "400 Bad Request",
            PROTOBUF,
            &b""[..],
            1,
            "321 of 321 spans were not exported",
        ),
        // ExportTraceServiceResponse { partial_success: { rejected_spans: 7, error_message: "quota" } }
        (
            "200 OK",
            PROTOBUF,
            b"\x0a\x09\x08\x07\x12\x05quota",
            1,
            rejected,
        ),
        (
            "200 OK",
            JSON,
            br#"{"partialSuccess":{"rejectedSpans":"7","errorMessage":"quota"}}"#,
            1,
            rejected,
        ),
        (
            "200 OK",
            JSON,
            br#"{"partialSuccess":{"rejectedSpans":7,"errorMessage":"quota"}}"#,
            1,
            rejected,
        ),
        ("200 OK", PROTOBUF, b"", 0, "exported 321 spans to "),
        (
            "200 OK",
            PROTOBUF,
            b"\x0a\x07\x12\x05quota",
            0,
            "exported 321 spans to ",
        ),
        ("200 OK", JSON, b"{}", 0, "exported 321 spans to "),
        (
            "200 OK",
            JSON,
            br#"{"partialSuccess":{"rejectedSpans":"0","errorMessage":"warning"}}"#,
            0,
            "exported 321 spans to ",
        ),
    ] {
        let (url, server) = otlp_response(status, content_type, body.to_vec(), 10);
        let out = run(&[
            "block",
            "--from-json",
            path.to_str().unwrap(),
            "--otlp",
            &url,
        ]);
        server.join().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(code), "{stderr}");
        assert!(stderr.contains(expected), "{stderr}");
        assert_eq!(code == 0, stderr.contains("exported 321 spans"), "{stderr}");
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn offline_block_sends_the_service_name_and_every_attribute_the_dashboards_query() {
    use opentelemetry_proto::tonic::{
        collector::trace::v1::ExportTraceServiceRequest,
        common::v1::{KeyValue, any_value::Value},
    };
    use prost::Message;
    let _serial = REPLAY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // The replay compiles its runtime first, which takes a while in a debug build.
    let (url, server) = otlp_response("200 OK", "application/x-protobuf", Vec::new(), 120);
    let with_user = url.replace("http://", "http://alice9:secret@");
    let out = run(&[
        "block",
        "--offline",
        &fixture("replay/people-transfer"),
        "--otlp",
        &with_user,
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains(&format!("spans to {url}\n")), "{stderr}");
    assert!(
        !format!("{stdout}{stderr}").contains("alice9"),
        "{stdout}{stderr}"
    );
    assert!(
        !format!("{stdout}{stderr}").contains("secret"),
        "{stdout}{stderr}"
    );
    let request = ExportTraceServiceRequest::decode(server.join().unwrap().as_slice()).unwrap();
    let value = |attributes: &[KeyValue], key: &str| {
        let found = attributes.iter().find(|a| a.key == key)?;
        found.value.clone()?.value
    };
    let text = |v: &str| Some(Value::StringValue(v.into()));
    let [resource] = &request.resource_spans[..] else {
        panic!("{request:?}")
    };
    let resource = &resource.resource.as_ref().unwrap().attributes;
    assert_eq!(
        value(resource, "service.name"),
        text("polkadot-frame-trace")
    );
    let spans: Vec<_> = request.resource_spans[0]
        .scope_spans
        .iter()
        .flat_map(|s| &s.spans)
        .collect();
    // The block panels select the root span by name and read its counts as numbers.
    let [root] = &spans
        .iter()
        .filter(|s| s.parent_span_id.is_empty())
        .collect::<Vec<_>>()[..]
    else {
        panic!("not one root span")
    };
    assert!(root.name.starts_with("block #2 "), "{}", root.name);
    assert_eq!(
        value(&root.attributes, "frame.block.number"),
        Some(Value::IntValue(2))
    );
    for key in [
        "frame.block.extrinsics",
        "frame.storage.ops",
        "frame.storage.reads",
        "frame.storage.writes",
        "frame.storage.duplicate_reads",
    ] {
        let found = value(&root.attributes, key);
        assert!(
            matches!(found, Some(Value::IntValue(_))),
            "{key}: {found:?}"
        );
    }
    assert!(spans.iter().any(|s| {
        value(&s.attributes, "frame.phase") == text("extrinsic")
            && value(&s.attributes, "frame.extrinsic.call") == text("Balances.transfer_keep_alive")
    }));
    // Storage panels filter and group on these, with these types.
    let ops: Vec<_> = spans
        .iter()
        .filter(|s| value(&s.attributes, "db.system.name") == text("substrate"))
        .collect();
    assert!(!ops.is_empty());
    for op in &ops {
        for key in [
            "db.operation.name",
            "db.collection.name",
            "frame.storage.item",
            "frame.storage.origin",
        ] {
            let found = value(&op.attributes, key);
            assert!(
                matches!(found, Some(Value::StringValue(_))),
                "{key}: {found:?}"
            );
        }
        let found = value(&op.attributes, "frame.storage.duplicate_read");
        assert!(matches!(found, Some(Value::BoolValue(_))), "{found:?}");
    }
    // A read new to the proof, with its bytes and its decoded map key.
    assert!(ops.iter().any(|s| {
        value(&s.attributes, "frame.storage.item") == text("System::Account")
            && value(&s.attributes, "frame.storage.origin") == text("trie (new)")
            && matches!(value(&s.attributes, "frame.storage.proof_bytes"), Some(Value::IntValue(n)) if n > 0)
            && matches!(value(&s.attributes, "frame.storage.map_keys"), Some(Value::ArrayValue(a)) if !a.values.is_empty())
    }));
}

#[test]
fn offline_block_reports_root_and_writes_measured_named_json() {
    let _serial = REPLAY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let path = std::env::temp_dir().join(format!(
        "polkadot-frame-trace-offline-cli-{}.json",
        std::process::id()
    ));
    let out = run(&[
        "block",
        "--offline",
        &fixture("replay/people-transfer"),
        "--json",
        path.to_str().unwrap(),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("root check: PASS; RPC calls: 0 (timed replay: 0)"),
        "{stderr}"
    );
    let block: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(block["timing"]["duration_ns"].as_u64().unwrap() > 0);
    assert!(
        block["phases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["call"] == "Balances.transfer_keep_alive")
    );
    assert!(block["phases"][0]["ops"][0]["timing"].is_object());
}

#[test]
fn offline_block_with_a_wrong_header_state_root_fails_the_root_check() {
    let _serial = REPLAY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut artifact = polkadot_frame_trace_replay::Artifact::load(std::path::Path::new(&fixture(
        "replay/people-transfer",
    )))
    .unwrap();
    artifact.block["header"]["stateRoot"] = format!("0x{}", "42".repeat(32)).into();
    // A matching block hash, so the replay gets past the header check to the root check.
    artifact.block_hash = polkadot_frame_trace_replay::header_hash(&artifact.block["header"])
        .unwrap()
        .parse()
        .unwrap();
    let dir = std::env::temp_dir().join(format!(
        "polkadot-frame-trace-wrong-root-{}",
        std::process::id()
    ));
    artifact.save(&dir).unwrap();
    let out = run(&["block", "--offline", dir.to_str().unwrap()]);
    std::fs::remove_dir_all(&dir).unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("root check: NOT VERIFIED"), "{stderr}");
    assert!(
        stderr.contains("runtime execution failed (root mismatch"),
        "{stderr}"
    );
}

struct MockNode {
    url: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl MockNode {
    fn new(
        handler: impl Fn(&serde_json::Value) -> serde_json::Value + Send + Sync + 'static,
    ) -> Self {
        use std::{
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
            thread,
            time::Duration,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let halt = stop.clone();
        let handler = Arc::new(handler);
        let worker = thread::spawn(move || {
            let mut workers = Vec::new();
            while !halt.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(socket) => socket,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                let handler = handler.clone();
                workers.push(thread::spawn(move || {
                    stream.set_nonblocking(false).unwrap();
                    stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                    stream.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap()); let mut len = 0;
                    loop {
                        let mut line = String::new(); if reader.read_line(&mut line).unwrap_or(0) == 0 { return; }
                        if line == "\r\n" { break; }
                        if let Some(v) = line.to_lowercase().strip_prefix("content-length:") { len = v.trim().parse().unwrap(); }
                    }
                    let mut raw = vec![0; len]; if reader.read_exact(&mut raw).is_err() { return; }
                    let request: serde_json::Value = serde_json::from_slice(&raw).unwrap();
                    let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(&request)));
                    let mut body = match &response {
                        Ok(body) => body.clone(),
                        Err(payload) => serde_json::json!({"error":{"code":-32099,"message":format!("mock handler assertion failed: {}", payload.downcast_ref::<String>().map(String::as_str).or_else(|| payload.downcast_ref::<&str>().copied()).unwrap_or("panic"))}}),
                    }; body["jsonrpc"] = "2.0".into(); body["id"] = request["id"].clone();
                    let body = body.to_string();
                    let _ = write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                    if let Err(payload) = response { std::panic::resume_unwind(payload); }
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self {
            url,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for MockNode {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(worker) = self.worker.take()
            && let Err(payload) = worker.join()
            && !std::thread::panicking()
        {
            std::panic::resume_unwind(payload);
        }
    }
}
fn artifact_rpc(
    artifact: &polkadot_frame_trace_replay::Artifact,
    request: &serde_json::Value,
) -> serde_json::Value {
    use serde_json::json;
    json!({"result": match request["method"].as_str().unwrap() {
        "chain_getFinalizedHead" | "chain_getBlockHash" => json!(artifact.block_hash),
        "chain_getBlock" => json!({"block":artifact.block}),
        "chain_getHeader" if request["params"].as_array().unwrap().is_empty() => artifact.block["header"].clone(),
        "chain_getHeader" if request["params"][0] == json!(artifact.block_hash) => artifact.block["header"].clone(),
        "chain_getHeader" => artifact.parent_header.clone(),
        "state_getRuntimeVersion" => artifact.runtime_version.clone(),
        "state_getReadProof" => json!({"at":artifact.block["header"]["parentHash"],"proof":artifact.proof_nodes}),
        method => panic!("unexpected method {method}"),
    }})
}

#[test]
fn block_pins_the_finalized_head_before_replay() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let _guard = REPLAY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let artifact = polkadot_frame_trace_replay::Artifact::load(std::path::Path::new(&fixture(
        "replay/people-transfer",
    )))
    .unwrap();
    let hash = serde_json::json!(artifact.block_hash);
    let heads = Arc::new(AtomicUsize::new(0));
    let requests = heads.clone();
    let node = MockNode::new(move |request| {
        match request["method"].as_str().unwrap() {
            "chain_getFinalizedHead" => {
                assert_eq!(request["params"], serde_json::json!([]));
                requests.fetch_add(1, Ordering::SeqCst);
            }
            "chain_getBlock" => assert_eq!(request["params"], serde_json::json!([hash])),
            "chain_getBlockHash" => panic!("finalized must use its pinned hash"),
            _ => {}
        }
        artifact_rpc(&artifact, request)
    });
    let out = run(&["block", "--rpc", &node.url, "--block", "finalized"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("root check: PASS"), "{stderr}");
    assert_eq!(heads.load(Ordering::SeqCst), 1);
}

#[test]
fn call_pins_finalized_state_for_replay_and_state_call_check() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let _guard = REPLAY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let artifact = polkadot_frame_trace_replay::Artifact::load(std::path::Path::new(&fixture(
        "replay/call-westend-nonce",
    )))
    .unwrap();
    let context = artifact.call.clone().unwrap();
    let hash = serde_json::json!(artifact.block_hash);
    let expected_hash = hash.clone();
    let heads = Arc::new(AtomicUsize::new(0));
    let requests = heads.clone();
    let node = MockNode::new(move |request| {
        use serde_json::json;
        let result = match request["method"].as_str().unwrap() {
            "chain_getFinalizedHead" => {
                assert_eq!(request["params"], json!([]));
                requests.fetch_add(1, Ordering::SeqCst);
                hash.clone()
            }
            "chain_getHeader" => {
                assert_eq!(request["params"], json!([hash]));
                artifact.parent_header.clone()
            }
            "state_getRuntimeVersion" => {
                assert_eq!(request["params"], json!([hash]));
                artifact.runtime_version.clone()
            }
            "state_getReadProof" => {
                assert_eq!(request["params"][1], hash);
                json!({"at":hash,"proof":artifact.proof_nodes})
            }
            "state_call" => {
                assert_eq!(request["params"][2], hash);
                json!("0x08020000")
            }
            method => panic!("unexpected method {method}"),
        };
        json!({"result":result})
    });
    let out = run(&[
        "call",
        "--rpc",
        &node.url,
        "--at",
        "finalized",
        "--method",
        &context.method,
        "--data",
        &context.data,
        "--check-state-call",
        "--json",
        "-",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["block_hash"], expected_hash);
    assert_eq!(json["call"]["oracle_match"], true);
    assert_eq!(heads.load(Ordering::SeqCst), 1);
}

#[test]
fn cli_save_then_offline_replays_without_rpc() {
    let _serial = REPLAY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let artifact = polkadot_frame_trace_replay::Artifact::load(std::path::Path::new(&fixture(
        "replay/people-transfer",
    )))
    .unwrap();
    let node = MockNode::new(move |request| artifact_rpc(&artifact, request));
    let dir = std::env::temp_dir().join(format!(
        "polkadot-frame-trace-cli-save-{}",
        std::process::id()
    ));
    let output = run(&[
        "block",
        "--rpc",
        &node.url,
        "--block",
        "2",
        "--save",
        dir.to_str().unwrap(),
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("root check: PASS"), "{stderr}");
    drop(node); // The round trip must work after the mock node has stopped.
    let saved = polkadot_frame_trace_replay::Artifact::load(&dir).unwrap();
    assert!(saved.proof_nodes.iter().all(|node| node.starts_with("0x")));
    let output = run(&["block", "--offline", dir.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("RPC calls: 0 (timed replay: 0)"));
    assert!(!output.stdout.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn follow_mock_node_retries_failed_blocks_observes_new_heads_and_cancels() {
    let _serial = REPLAY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    use std::{
        process::Stdio,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };
    let artifact = polkadot_frame_trace_replay::Artifact::load(std::path::Path::new(&fixture(
        "replay/people-transfer",
    )))
    .unwrap();
    let stage = Arc::new(AtomicUsize::new(0));
    let advance = stage.clone();
    let failures = Arc::new(AtomicUsize::new(0));
    let attempts = failures.clone();
    let mut new_header = artifact.block["header"].clone();
    new_header["parentHash"] = serde_json::json!(artifact.block_hash);
    new_header["number"] = "0x3".into();
    let new_hash = polkadot_frame_trace_replay::header_hash(&new_header).unwrap();
    let expected_new = new_hash.clone();
    let node = MockNode::new(move |request| {
        if request["method"] == "chain_getBlock" && request["params"][0] == new_hash {
            return serde_json::json!({"result": null});
        }
        if request["method"] == "state_getRuntimeVersion"
            && attempts.fetch_add(1, Ordering::SeqCst) < 4
        {
            return serde_json::json!({"error":{"code":429,"message":"too many requests"}});
        }
        if request["method"] == "chain_getFinalizedHead" && advance.load(Ordering::SeqCst) > 0 {
            return serde_json::json!({"result":new_hash});
        }
        if request["method"] == "chain_getHeader"
            && request["params"][0] == new_hash
            && advance.load(Ordering::SeqCst) > 0
        {
            return serde_json::json!({"result":new_header});
        }
        artifact_rpc(&artifact, request)
    });
    let log = std::env::temp_dir().join(format!(
        "polkadot-frame-trace-follow-test-{}.log",
        std::process::id()
    ));
    let file = std::fs::File::create(&log).unwrap();
    let mut child = Command::new(BIN)
        .args(["follow", "--rpc", &node.url])
        .stdout(Stdio::null())
        .stderr(file)
        .spawn()
        .unwrap();
    let start = Instant::now();
    let mut passed = false;
    while start.elapsed() < Duration::from_secs(120) {
        let text = std::fs::read_to_string(&log).unwrap();
        if text.contains("traced finalized block #2") {
            stage.store(1, Ordering::SeqCst);
        }
        if text.contains(&format!("block {expected_new} not found"))
            && text.contains("retrying block")
            && text.contains(&format!("stopped retrying block {expected_new}"))
        {
            passed = true;
            break;
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let stopped = Instant::now();
    Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    while child.try_wait().unwrap().is_none() && stopped.elapsed() < Duration::from_secs(10) {
        thread::sleep(Duration::from_millis(20));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let status = child.wait().unwrap();
    let text = std::fs::read_to_string(&log).unwrap();
    std::fs::remove_file(log).unwrap();
    assert!(passed && status.success(), "{status:?}: {text}");
    assert!(stopped.elapsed() < Duration::from_secs(10));
    assert!(failures.load(Ordering::SeqCst) >= 5);
    assert!(
        text.contains("in-flight=")
            && text.contains("stopped=")
            && text.contains("follow stopped after ")
            && !text.contains("panicked at")
    );
}

/// Sends `signal` to `child` and waits up to ten seconds for it to exit.
fn stop_child(child: &mut std::process::Child, signal: &str) -> std::process::ExitStatus {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let start = Instant::now();
    Command::new("kill")
        .args([signal, &child.id().to_string()])
        .status()
        .unwrap();
    while child.try_wait().unwrap().is_none() && start.elapsed() < Duration::from_secs(10) {
        thread::sleep(Duration::from_millis(20));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let status = child.wait().unwrap();
    assert!(start.elapsed() < Duration::from_secs(10), "{status:?}");
    status
}

/// Waits up to `secs` seconds for the log to contain `text`.
fn wait_for_log(log: &std::path::Path, text: &str, secs: u64) -> String {
    let start = std::time::Instant::now();
    loop {
        let current = std::fs::read_to_string(log).unwrap();
        if current.contains(text) || start.elapsed() > std::time::Duration::from_secs(secs) {
            return current;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Kills the child on drop, so a failed assertion cannot leave `follow` running.
struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// An OTLP/HTTP receiver that answers `503` while `down` is set and `200` otherwise, and
/// counts the requests it accepted.
fn switchable_receiver(
    down: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::Ordering;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = accepted.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut len = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            if reader.read_exact(&mut vec![0; len]).is_err() {
                continue;
            }
            let status = if down.load(Ordering::SeqCst) {
                "503 Service Unavailable"
            } else {
                count.fetch_add(1, Ordering::SeqCst);
                "200 OK"
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
        }
    });
    (url, accepted)
}

#[test]
fn follow_retries_only_the_export_while_the_collector_is_down() {
    let _serial = REPLAY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    use std::{
        process::Stdio,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };
    let artifact = polkadot_frame_trace_replay::Artifact::load(std::path::Path::new(&fixture(
        "replay/people-transfer",
    )))
    .unwrap();
    let blocks = Arc::new(AtomicUsize::new(0));
    let fetched = blocks.clone();
    let node = MockNode::new(move |request| {
        if request["method"] == "chain_getBlock" {
            fetched.fetch_add(1, Ordering::SeqCst);
        }
        artifact_rpc(&artifact, request)
    });
    let down = Arc::new(AtomicBool::new(true));
    let (otlp, accepted) = switchable_receiver(down.clone());
    let log = std::env::temp_dir().join(format!(
        "polkadot-frame-trace-follow-otlp-{}.log",
        std::process::id()
    ));
    let mut child = Child(
        Command::new(BIN)
            .args(["follow", "--rpc", &node.url, "--otlp", &otlp])
            // Keep each failed try short; the exporter retries 503 within this time.
            .env("OTEL_EXPORTER_OTLP_TIMEOUT", "500")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap(),
    );
    let text = wait_for_log(&log, "OTLP export failed", 120);
    assert!(
        text.contains("OTLP export failed (1 attempt(s) in a row)"),
        "{text}"
    );
    down.store(false, Ordering::SeqCst);
    wait_for_log(&log, "OTLP export recovered", 30);
    let status = stop_child(&mut child.0, "-TERM");
    let text = std::fs::read_to_string(&log).unwrap();
    std::fs::remove_file(log).unwrap();
    assert!(status.success(), "{status:?}: {text}");
    assert!(text.contains("OTLP export recovered after "), "{text}");
    assert!(
        text.contains(
            "traced=1 exported=1 export-rejected=0 export-failed=0 export-dropped=0 export-unsent=0"
        ),
        "{text}"
    );
    // The block was replayed once and its one batch accepted once.
    assert_eq!(blocks.load(Ordering::SeqCst), 1, "{text}");
    assert_eq!(accepted.load(Ordering::SeqCst), 1, "{text}");
}

/// A pipe already full, so that every write to it blocks until the test ends.
fn full_pipe() -> (std::io::PipeReader, std::io::PipeWriter) {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let (reader, writer) = std::io::pipe().unwrap();
    let mut filler = writer.try_clone().unwrap();
    let written = Arc::new(AtomicUsize::new(0));
    let progress = written.clone();
    std::thread::spawn(move || {
        while filler.write_all(&[b'x'; 512]).is_ok() {
            progress.fetch_add(512, Ordering::SeqCst);
        }
    });
    // Full once the filler stops making progress.
    let mut last = usize::MAX;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let now = written.load(Ordering::SeqCst);
        if now == last && now > 0 {
            break;
        }
        last = now;
    }
    (reader, writer)
}

#[test]
fn follow_stops_on_signals_with_a_summary_even_when_nobody_reads_its_stderr() {
    use std::process::Stdio;
    // The signals sent, whether stderr is a full pipe, and the exit code.
    for (signals, full, code) in [
        (&["-INT"][..], false, 0),
        (&["-TERM"], false, 0),
        (&["-TERM"], true, 0),
        (&["-TERM", "-INT"], true, 130),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let log = std::env::temp_dir().join(format!(
            "polkadot-frame-trace-signal-{}.log",
            std::process::id()
        ));
        let (_reader, stderr) = if full {
            let (reader, writer) = full_pipe();
            (Some(reader), Stdio::from(writer))
        } else {
            (None, Stdio::from(std::fs::File::create(&log).unwrap()))
        };
        let mut child = Child(
            Command::new(BIN)
                .args(["follow", "--rpc", &url])
                .stdout(Stdio::null())
                .stderr(stderr)
                .spawn()
                .unwrap(),
        );
        // The first check for new blocks waits on this stalled handshake.
        let (_stream, _) = listener.accept().unwrap();
        for signal in signals {
            Command::new("kill")
                .args([*signal, &child.0.id().to_string()])
                .status()
                .unwrap();
        }
        let exit = exit_code_within(&mut child, 15);
        let text = if full {
            String::new()
        } else {
            let text = std::fs::read_to_string(&log).unwrap();
            std::fs::remove_file(&log).unwrap();
            text
        };
        assert_eq!(exit, Some(code), "{signals:?}: {text}");
        if !full {
            for line in ["follow: stopping", "follow stopped after ", " traced=0 "] {
                assert!(text.contains(line), "{signals:?}: {text}");
            }
        }
    }
}

/// Waits up to `secs` seconds for `child` to exit and returns its exit code.
fn exit_code_within(child: &mut Child, secs: u64) -> Option<i32> {
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            return status.code();
        }
        if start.elapsed() > std::time::Duration::from_secs(secs) {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn follow_startup_errors_and_replays_do_not_wait_for_a_blocked_stderr() {
    let _serial = REPLAY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    use std::{
        process::Stdio,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };
    // A startup error is queued like every other line, then `follow` exits 1.
    let (_reader, writer) = full_pipe();
    let mut child = Child(
        Command::new(BIN)
            .args(["follow", "--rpc", "invalid"])
            .stdout(Stdio::null())
            .stderr(Stdio::from(writer))
            .spawn()
            .unwrap(),
    );
    assert_eq!(exit_code_within(&mut child, 10), Some(1));
    // Replays write progress lines from worker threads; a stop must not wait for them.
    let artifact = polkadot_frame_trace_replay::Artifact::load(std::path::Path::new(&fixture(
        "replay/people-transfer",
    )))
    .unwrap();
    let proofs = Arc::new(AtomicUsize::new(0));
    let seen = proofs.clone();
    let node = MockNode::new(move |request| {
        if request["method"] == "state_getReadProof" {
            seen.fetch_add(1, Ordering::SeqCst);
        }
        artifact_rpc(&artifact, request)
    });
    let (_reader, writer) = full_pipe();
    let mut child = Child(
        Command::new(BIN)
            .args(["follow", "--rpc", &node.url])
            .stdout(Stdio::null())
            .stderr(Stdio::from(writer))
            .spawn()
            .unwrap(),
    );
    let start = std::time::Instant::now();
    while proofs.load(Ordering::SeqCst) == 0 && start.elapsed().as_secs() < 60 {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        proofs.load(Ordering::SeqCst) > 0,
        "the replay never started"
    );
    Command::new("kill")
        .args(["-TERM", &child.0.id().to_string()])
        .status()
        .unwrap();
    // The stop waits at most 10 s for replays and 2 s for the log.
    assert_eq!(exit_code_within(&mut child, 20), Some(0));
}

#[test]
fn follow_exits_1_when_its_stderr_is_closed() {
    use std::process::Stdio;
    // Nobody will ever read: every write to this pipe fails, as after journald restarted.
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let mut child = Child(
        Command::new(BIN)
            .args(["follow", "--rpc", "ws://127.0.0.1:9"])
            .stdout(Stdio::null())
            .stderr(Stdio::from(writer))
            .spawn()
            .unwrap(),
    );
    assert_eq!(exit_code_within(&mut child, 15), Some(1));
}

#[test]
fn endpoint_credentials_never_reach_the_output_but_the_host_and_port_do() {
    // Nobody listens on port 1. 192.0.2.1 is a documentation address: plain and remote.
    let secret =
        |scheme: &str, host: &str| format!("{scheme}://alice9:secret@{host}/path?apikey=k3y");
    // A space in the path makes the OTLP exporter reject the URL and quote it.
    let unparsable = "http://alice9:secret@127.0.0.1:1/a path?apikey=k3y";
    let check = |text: &str, shown: &str| {
        for hidden in ["alice9", "secret", "apikey", "k3y", "/path", "a path"] {
            assert!(!text.contains(hidden), "{hidden} leaked: {text}");
        }
        assert!(text.contains(shown), "{shown} missing: {text}");
    };
    let trace = fixture("replay/people-transfer");
    for (args, shown) in [
        (
            [
                "--rpc",
                secret("http", "127.0.0.1:1").as_str(),
                "--block",
                "1",
            ],
            "chain_getBlockHash: http://127.0.0.1:1: ",
        ),
        // Gives up after the 1 s deadline, having warned that the node is not encrypted.
        (
            [
                "--rpc",
                secret("ws", "192.0.2.1:9944").as_str(),
                "--block=1",
                "--rpc-timeout=1",
            ],
            "warning: --rpc ws://192.0.2.1:9944 is not",
        ),
        (
            [
                "--offline",
                trace.as_str(),
                "--otlp",
                &secret("http", "127.0.0.1:1"),
            ],
            "exporting to http://127.0.0.1:1: ",
        ),
        (
            ["--offline", trace.as_str(), "--otlp", unparsable],
            "invalid endpoint 'http://127.0.0.1:1'",
        ),
    ] {
        let out = run(&[&["block"][..], &args].concat());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{stderr}");
        check(&String::from_utf8_lossy(&out.stdout), "");
        check(&stderr, shown);
    }
    // `follow` keeps polling an unreachable node, and warns about a remote plain collector.
    for scheme in ["ws", "http"] {
        let log = std::env::temp_dir().join(format!(
            "polkadot-frame-trace-secrets-{}.log",
            std::process::id()
        ));
        std::fs::File::create(&log).unwrap();
        let append = || std::fs::File::options().append(true).open(&log).unwrap();
        let mut child = Child(
            Command::new(BIN)
                .args(["follow", "--rpc", &secret(scheme, "127.0.0.1:1")])
                .args(["--otlp", &secret("http", "192.0.2.1:4318")])
                .stdout(append())
                .stderr(append())
                .spawn()
                .unwrap(),
        );
        wait_for_log(&log, "follow poll failed", 30);
        stop_child(&mut child.0, "-TERM");
        let text = std::fs::read_to_string(&log).unwrap();
        std::fs::remove_file(log).unwrap();
        check(
            &text,
            "follow poll failed: RPC error: chain_getFinalizedHead: ",
        );
        check(&text, &format!("{scheme}://127.0.0.1:1: "));
        check(
            &text,
            "warning: --otlp http://192.0.2.1:4318 is not encrypted",
        );
    }
    // A bad --otlp URL stops `follow` at startup.
    let out = run(&[
        "follow",
        "--rpc",
        &secret("ws", "127.0.0.1:1"),
        "--otlp",
        unparsable,
    ]);
    assert_eq!(out.status.code(), Some(1));
    check(&String::from_utf8_lossy(&out.stdout), "");
    check(
        &String::from_utf8_lossy(&out.stderr),
        "--otlp: cannot build OTLP exporter",
    );
}

#[test]
fn follow_exits_1_with_a_summary_when_its_threads_die() {
    use std::{process::Stdio, time::Duration};
    for (fault, reason) in [
        ("workers", "all replay workers stopped"),
        ("export-thread", "the OTLP export thread stopped"),
        (
            "hung-replay",
            "the replay of block #0 0xtest ran the runtime for more than 1s without finishing",
        ),
    ] {
        let log = std::env::temp_dir().join(format!(
            "polkadot-frame-trace-fault-{fault}-{}.log",
            std::process::id()
        ));
        let mut child = Child(
            Command::new(BIN)
                .args([
                    "follow",
                    "--rpc",
                    "ws://127.0.0.1:9",
                    "--otlp",
                    "http://127.0.0.1:9",
                ])
                .env("FRAME_TRACE_TEST_FAULT", fault)
                .env("RUST_BACKTRACE", "1")
                .stdout(Stdio::null())
                .stderr(std::fs::File::create(&log).unwrap())
                .spawn()
                .unwrap(),
        );
        let start = std::time::Instant::now();
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break Some(status);
            }
            if start.elapsed() > Duration::from_secs(30) {
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let text = std::fs::read_to_string(&log).unwrap();
        std::fs::remove_file(log).unwrap();
        assert_eq!(status.and_then(|s| s.code()), Some(1), "{fault}: {text}");
        assert!(
            text.contains(&format!("follow: {reason}; stopping")),
            "{text}"
        );
        assert!(text.contains("follow stopped after "), "{text}");
        assert!(
            text.contains(&format!("Error: follow stopped because {reason}")),
            "{text}"
        );
        if fault == "export-thread" {
            // The injected panic is logged with its backtrace.
            assert!(
                text.contains("injected export thread failure\nstack backtrace:"),
                "{text}"
            );
        }
    }
}

#[test]
fn rpc_timeout_lets_a_slow_node_answer() {
    let _serial = REPLAY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let artifact = polkadot_frame_trace_replay::Artifact::load(std::path::Path::new(&fixture(
        "replay/people-transfer",
    )))
    .unwrap();
    // The block takes 1.5 s to arrive, as a large response from a far node does.
    let node = MockNode::new(move |request| {
        if request["method"] == "chain_getBlock" {
            std::thread::sleep(std::time::Duration::from_millis(1500));
        }
        artifact_rpc(&artifact, request)
    });
    let out = run(&[
        "block",
        "--rpc",
        &node.url,
        "--block",
        "2",
        "--rpc-timeout",
        "0.5",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("chain_getBlock") && stderr.contains("deadline exceeded"),
        "{stderr}"
    );
    let out = run(&[
        "block",
        "--rpc",
        &node.url,
        "--block",
        "2",
        "--rpc-timeout",
        "3",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("root check: PASS"), "{stderr}");
    for bad in ["0", "-1", "soon", "3601"] {
        let out = run(&[
            "block",
            "--rpc",
            &node.url,
            "--block",
            "2",
            "--rpc-timeout",
            bad,
        ]);
        assert_eq!(out.status.code(), Some(2), "{bad}");
    }
    let out = run(&["block", "--offline", "missing", "--rpc-timeout", "3"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn call_offline_exports_measured_ops_and_self_contained_round_trip() {
    let _guard = REPLAY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = std::env::temp_dir().join(format!(
        "polkadot-frame-trace-call-roundtrip-{}",
        std::process::id()
    ));
    let out = run(&[
        "call",
        "--offline",
        &fixture("replay/call-people-nonce"),
        "--json",
        "-",
        "--save",
        dir.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["schema_version"], 2);
    assert_eq!(json["call"]["method"], "AccountNonceApi_account_nonce");
    assert_eq!(json["call"]["result"], "0x00000000");
    assert!(json["call"]["decoded"].is_null()); // saved proof does not cover metadata
    assert_eq!(json["phases"][0]["kind"], "call");
    assert_eq!(json["phases"][0]["ops"][0]["item"], "<unknown>");
    assert!(
        json["phases"][0]["ops"][0]["timing"]["duration_ns"]
            .as_u64()
            .unwrap()
            > 0
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("root check: NOT APPLICABLE"));
    assert!(stderr.contains("RPC calls: 0 (timed replay: 0)"));
    // The saved copy replays on its own. Without metadata the result stays hex.
    let repeated = run(&["call", "--offline", dir.to_str().unwrap(), "--json", "-"]);
    let stderr = String::from_utf8_lossy(&repeated.stderr);
    assert!(repeated.status.success(), "{stderr}");
    std::fs::remove_dir_all(dir).unwrap();
    assert!(
        stderr.contains("call AccountNonceApi_account_nonce at #1"),
        "{stderr}"
    );
    for label in ["reads: cache", "trie (already in proof)", "trie (new)"] {
        assert!(stderr.contains(label), "{stderr}");
    }
    let json: serde_json::Value = serde_json::from_slice(&repeated.stdout).unwrap();
    assert_eq!(json["call"]["result"], "0x00000000");
    assert!(json["call"]["decoded"].is_null());
    let origins: Vec<_> = json["phases"][0]["ops"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|op| op["origin"].as_str())
        .collect();
    assert!(!origins.is_empty());
    assert!(
        origins
            .iter()
            .all(|origin| ["overlay", "backend_warm", "backend_cold"].contains(origin))
    );

    // A V16 public recording decodes the result and labels its recorded oracle.
    let out = run(&[
        "call",
        "--offline",
        &fixture("replay/call-westend-nonce"),
        "--json",
        "-",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["call"]["decoded"], "520");
    assert_eq!(json["call"]["oracle_match"], true);
    assert_eq!(json["call"]["oracle_source"], "recorded (unverified)");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("state_call check: MATCH (recorded; unverified)")
    );
}

fn call_node(mismatch: bool) -> MockNode {
    let artifact = polkadot_frame_trace_replay::Artifact::load(std::path::Path::new(&fixture(
        "replay/call-westend-nonce",
    )))
    .unwrap();
    MockNode::new(move |request| {
        let result = match request["method"].as_str().unwrap() {
            "chain_getHeader" => artifact.parent_header.clone(),
            "chain_getBlockHash" | "chain_getFinalizedHead" => {
                serde_json::json!(artifact.block_hash)
            }
            "state_getRuntimeVersion" => artifact.runtime_version.clone(),
            "state_getReadProof" => {
                serde_json::json!({"at":artifact.block_hash,"proof":artifact.proof_nodes})
            }
            "state_call" => serde_json::json!(if mismatch { "0xffffffff" } else { "0x08020000" }),
            method => panic!("unexpected method {method}; metadata must execute locally"),
        };
        serde_json::json!({"result":result})
    })
}

#[test]
fn local_metadata_rejects_unknown_method_and_wrong_argument_types() {
    let _serial = REPLAY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let node = call_node(false);
    // Malformed input is caught before connecting; the first two rows use an unreachable node.
    let unreachable = "http://127.0.0.1:1";
    for (rpc, method, flag, value, expected) in [
        (
            unreachable,
            "Unknown_method",
            "--args",
            "{",
            "--args: malformed JSON",
        ),
        (
            unreachable,
            "Unknown_method",
            "--data",
            "0xz",
            "--data: invalid SCALE hex",
        ),
        (
            &node.url[..],
            "AccountNonceApi_account_nonc",
            "--args",
            "[]",
            "did you mean AccountNonceApi_account_nonce?",
        ),
        (
            &node.url,
            "AccountNonceApi_account_nonce",
            "--args",
            "{\"account\":\"wrong type\"}",
            "--args: cannot encode",
        ),
        (
            &node.url,
            "AccountNonceApi_account_nonce",
            "--args",
            "{\"account\":42}",
            "argument account: expected sp_core::crypto::AccountId32",
        ),
    ] {
        let out = run(&["call", "--rpc", rpc, "--method", method, flag, value]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{stderr}");
        assert!(
            stderr.contains(expected) && !stderr.contains("invalid metadata:"),
            "{stderr}"
        );
    }
}

#[test]
fn oracle_mismatch_exits_three_after_emitting_parseable_json_and_summary_to_stderr() {
    let _serial = REPLAY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let node = call_node(true);
    let out = run(&["call", "--rpc", &node.url, "--method", "AccountNonceApi_account_nonce", "--args",
        &serde_json::json!({"account":polkadot_frame_trace_replay::rpc::unhex("d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d").unwrap()}).to_string(), "--check-state-call", "--json", "-"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["call"]["oracle_match"], false);
    assert_eq!(json["call"]["oracle_source"], "live");
    assert!(
        stderr.contains("MISMATCH") && stderr.contains("Top items by proof bytes"),
        "{stderr}"
    );
}

#[test]
fn unknown_block_hash_reports_block_not_found() {
    let node = MockNode::new(|_| serde_json::json!({"result":null}));
    let out = run(&[
        "call",
        "--rpc",
        &node.url,
        "--at",
        &format!("0x{}", "ff".repeat(32)),
        "--method",
        "Metadata_metadata_versions",
        "--data",
        "0x",
    ]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("block not found"));
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn saved_block_json_drives_tables_and_multiblock_reports_without_replay() {
    let _guard = REPLAY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let path = std::env::temp_dir().join(format!("frame-report-{}.json", std::process::id()));
    let path2 = path.with_extension("second.json");
    let output = run(&[
        "block",
        "--offline",
        &fixture("replay/people-transfer"),
        "--json",
        path.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut block: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let count = block["totals"]["ops"].as_u64().unwrap();
    block["block_hash"] = "another block".into();
    std::fs::write(&path2, serde_json::to_vec(&block).unwrap()).unwrap();
    let summary = run(&["block", "--from-json", path.to_str().unwrap()]);
    assert!(
        summary.status.success(),
        "{}",
        String::from_utf8_lossy(&summary.stderr)
    );
    assert!(summary.stderr.is_empty(), "summary should not replay");
    let report = run(&[
        "report",
        path.to_str().unwrap(),
        path2.to_str().unwrap(),
        "--json",
        "-",
    ]);
    assert!(
        report.status.success(),
        "{}",
        String::from_utf8_lossy(&report.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(json["blocks"], 2);
    assert_eq!(json["totals"]["ops"], count * 2);
    let duplicate = run(&["report", path.to_str().unwrap(), path.to_str().unwrap()]);
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("duplicate block hash"));
    let absent = fixture("model/absent-round5.json");
    let out = run(&["report", &absent, path.to_str().unwrap(), "--json", "-"]);
    assert_eq!(out.status.code(), Some(1));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["blocks"], 1);
    assert_eq!(json["errors"].as_array().unwrap().len(), 1);
    assert_eq!(json["errors"][0]["input"], absent);
    assert!(String::from_utf8_lossy(&out.stderr).contains("Errors"));
    for p in [path, path2] {
        std::fs::remove_file(p).unwrap();
    }
}

#[test]
fn report_last_pins_finalized_hash_and_exports_replay_statistics() {
    let _guard = REPLAY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let artifact = polkadot_frame_trace_replay::Artifact::load(std::path::Path::new(&fixture(
        "replay/people-transfer",
    )))
    .unwrap();
    let other = polkadot_frame_trace_replay::Artifact::load(std::path::Path::new(&fixture(
        "replay/people-kill-prefix",
    )))
    .unwrap();
    // The mock aliases height 7 to the transfer fixture and height 8 to the kill
    // fixture; both pinned hashes and their execution proofs remain authentic.
    // This exercises concurrent scheduling independently of chain ancestry.
    let node = MockNode::new(move |request| {
        use serde_json::json;
        let method = request["method"].as_str().unwrap();
        if method == "chain_getFinalizedHead" {
            return json!({"result":other.block_hash});
        }
        if method == "chain_getBlockHash" {
            return json!({"result":if request["params"][0] == 7 {artifact.block_hash} else {other.block_hash}});
        }
        let at = if method == "state_getReadProof" {
            &request["params"][1]
        } else {
            &request["params"][0]
        };
        let first =
            *at == json!(artifact.block_hash) || *at == artifact.block["header"]["parentHash"];
        artifact_rpc(if first { &artifact } else { &other }, request)
    });
    let out = run(&["report", "--rpc", &node.url, "--last", "2", "--json", "-"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["blocks"], 2);
    assert!(json["totals"]["backend_cold"].as_u64().unwrap() > 0);
    assert_eq!(json["execution_proof_bytes"], 6479 + 6867);
    assert_eq!(json["totals"]["proof_bytes"], 6479 + 6867);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.matches("root check: PASS").count(), 2);
    assert_eq!(stderr.matches("(timed replay: 0)").count(), 2);
}

/// The traced People Wasm is the `:code` value node inside the `people-traced` artifact.
fn traced_people_wasm(tag: &str) -> std::path::PathBuf {
    let artifact = polkadot_frame_trace_replay::Artifact::load(std::path::Path::new(&fixture(
        "replay/people-traced",
    )))
    .unwrap();
    let wasm = artifact
        .proof_nodes
        .iter()
        .map(|node| polkadot_frame_trace_replay::rpc::unhex(node).unwrap())
        .max_by_key(Vec::len)
        .unwrap();
    assert!(
        wasm.len() > 1_000_000,
        "the largest node is the runtime code"
    );
    let path = std::env::temp_dir().join(format!(
        "polkadot-frame-trace-traced-people-{tag}-{}.wasm",
        std::process::id()
    ));
    std::fs::write(&path, wasm).unwrap();
    path
}

#[test]
fn runtime_override_replays_untraced_block_with_traced_wasm_and_labels_it() {
    let _serial = REPLAY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let wasm = traced_people_wasm("replay");
    let json = wasm.with_extension("json");
    let out = run(&[
        "block",
        "--offline",
        &fixture("replay/people-transfer"),
        "--runtime-override",
        wasm.to_str().unwrap(),
        "--json",
        json.to_str().unwrap(),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("root check: PASS"), "{stderr}");
    assert!(
        stderr.contains("RUNTIME OVERRIDE") && stderr.contains("code-hash check BYPASSED"),
        "{stderr}"
    );
    let block: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    let block_hash = block["block_hash"].clone();
    let label_code_hash = block["runtime_override"]["code_hash"].clone();
    let label = &block["runtime_override"];
    assert_eq!(
        label["chain_code_hash"],
        "0xee817a9ecd07f4c0f861dd6f2b5105fd316638b827d4d425ae062add4b651c1c"
    );
    assert_eq!(
        label["code_hash"],
        "0xba01b1de0c55e48c84c70a59381594c8540eb2482ac8e785908de120ce617074"
    );
    assert_eq!(label["path"], wasm.to_str().unwrap());
    assert_eq!(block["execution"]["root_check"], true);
    let wasm_spans: Vec<_> = block["runtime_spans"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["wasm"] == true)
        .map(|s| s["name"].as_str().unwrap().to_owned())
        .collect();
    for expected in [
        "apply_extrinsic",
        "on_initialize: System",
        "transfer_keep_alive",
    ] {
        assert!(wasm_spans.iter().any(|n| n == expected), "{wasm_spans:?}");
    }

    // The chain's own code has no runtime spans and no override label.
    let plain = wasm.with_extension("plain.json");
    let out = run(&[
        "block",
        "--offline",
        &fixture("replay/people-kill-prefix"),
        "--json",
        plain.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("RUNTIME OVERRIDE"));
    let block: serde_json::Value = serde_json::from_slice(&std::fs::read(&plain).unwrap()).unwrap();
    assert!(block.get("runtime_override").is_none());
    assert!(
        block["runtime_spans"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["wasm"] == false)
    );

    // Saved JSON keeps the provenance: block tables and mixed reports name the override first.
    let out = run(&["block", "--from-json", json.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(
        stdout.starts_with("RUNTIME OVERRIDE: 1 of 1 block(s) used a runtime override"),
        "{stdout}"
    );
    assert!(stdout.contains("0xba01b1de0c55e48c84c70a59381594c8540eb2482ac8e785908de120ce617074"));
    let report = wasm.with_extension("report.json");
    let out = run(&[
        "report",
        json.to_str().unwrap(),
        plain.to_str().unwrap(),
        "--sort-by",
        "time",
        "--json",
        report.to_str().unwrap(),
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.starts_with("RUNTIME OVERRIDE: 1 of 2 block(s) used a runtime override"),
        "{stdout}"
    );
    assert!(stdout.contains("Top items by storage time"), "{stdout}");
    let aggregate: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    let overrides = aggregate["runtime_overrides"].as_array().unwrap();
    assert_eq!(overrides.len(), 1);
    assert_eq!(overrides[0]["block_hash"], block_hash);
    assert_eq!(overrides[0]["code_hash"], label_code_hash);
    assert_eq!(overrides[0]["path"], wasm.to_str().unwrap());
    assert_eq!(aggregate["blocks"], 2);

    // Saved artifacts never mix a substitute runtime's discovery with the chain's code.
    let out = run(&[
        "block",
        "--offline",
        &fixture("replay/people-transfer"),
        "--runtime-override",
        wasm.to_str().unwrap(),
        "--save",
        json.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2), "block");
    let out = run(&[
        "call",
        "--offline",
        &fixture("replay/call-people-nonce"),
        "--runtime-override",
        wasm.to_str().unwrap(),
        "--save",
        json.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2));
    let out = run(&[
        "call",
        "--offline",
        &fixture("replay/call-people-nonce"),
        "--runtime-override",
        wasm.to_str().unwrap(),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("RUNTIME OVERRIDE"), "{stderr}");
    assert!(stderr.contains("RuntimeVersion matches"), "{stderr}");

    // Online replay through safe RPC accepts the same override.
    let artifact = polkadot_frame_trace_replay::Artifact::load(std::path::Path::new(&fixture(
        "replay/people-transfer",
    )))
    .unwrap();
    let node = MockNode::new(move |request| artifact_rpc(&artifact, request));
    let out = run(&[
        "block",
        "--rpc",
        &node.url,
        "--block",
        "2",
        "--runtime-override",
        wasm.to_str().unwrap(),
        "--json",
        json.to_str().unwrap(),
    ]);
    drop(node);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("root check: PASS with RUNTIME OVERRIDE"),
        "{stderr}"
    );
    let block: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    assert_eq!(block["runtime_override"]["code_hash"], label_code_hash);
    assert!(
        block["runtime_spans"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["wasm"] == true)
    );
    for path in [json, plain, report] {
        let _ = std::fs::remove_file(path);
    }
    let _ = std::fs::remove_file(wasm);
}

#[test]
fn unusable_runtime_override_files_and_missing_host_functions_are_named() {
    let _serial = REPLAY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Files that are not a runtime are rejected by name before any replay starts.
    let wasm = traced_people_wasm("bad");
    let bytes = std::fs::read(&wasm).unwrap();
    let mut corrupt = b"\0asm\x01\0\0\0".to_vec();
    corrupt.extend([0xff; 64]);
    for (name, content) in [
        ("text", b"this is a text file\n".to_vec()),
        ("truncated", bytes[..bytes.len() / 2].to_vec()),
        ("corrupt", corrupt),
    ] {
        let path = wasm.with_extension(format!("{name}.wasm"));
        std::fs::write(&path, content).unwrap();
        for command in ["block", "call"] {
            let out = run(&[
                command,
                "--offline",
                "unused-artifact",
                "--runtime-override",
                path.to_str().unwrap(),
            ]);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(out.status.code(), Some(1), "{name}: {stderr}");
            assert!(
                stderr.contains(&format!("--runtime-override {}", path.display())),
                "{name}: {stderr}"
            );
            assert!(
                !stderr.contains("unused-artifact") && !stderr.contains("root"),
                "{name}: {stderr}"
            );
        }
        std::fs::remove_file(path).unwrap();
    }
    std::fs::remove_file(wasm).unwrap();

    // A real runtime that imports a host function we lack fails with the import's name.
    let out = run(&[
        "block",
        "--offline",
        &fixture("replay/westend-asset-hub"),
        "--runtime-override",
        &fixture("wasm/missing-host-function.wasm"),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains(
            "the runtime needs host functions polkadot-frame-trace does not have: \
             env:ext_frame_trace_missing_version_1; a runtime override was executed"
        ),
        "{stderr}"
    );
    // Metadata loading runs the same runtime first. It must stop with this error, not print
    // a notice that sends the reader after metadata.
    assert!(!stderr.contains("local metadata unavailable"), "{stderr}");
    assert!(!stderr.contains("runtime execution failed"), "{stderr}");
    assert!(stderr.contains("root check: NOT VERIFIED"), "{stderr}");
}

#[test]
fn version_prints_the_crate_version_or_the_nightly_build_version() {
    let output = run(&["--version"]);
    assert!(output.status.success());
    let expected = match option_env!("FRAME_TRACE_BUILD_VERSION") {
        Some(version) if !version.is_empty() => version,
        _ => env!("CARGO_PKG_VERSION"),
    };
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("polkadot-frame-trace {expected}\n")
    );
}
