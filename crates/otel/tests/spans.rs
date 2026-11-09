//! Span tree and attributes, checked with the SDK's in-memory exporter.

#![allow(clippy::unwrap_used)] // Tests may panic on bad fixtures.

use std::time::{Duration, SystemTime};

use polkadot_frame_trace_decode::{
    AnnotatedBlock, AnnotatedOp, AnnotatedPhase, Metadata, annotate,
};
use polkadot_frame_trace_model::{Stats, TracedBlock, rpc::parse_trace_block};

use opentelemetry::{
    Value,
    trace::{SpanId, TracerProvider},
};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use polkadot_frame_trace_otel::{Timing, record_block};

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/../../fixtures/{path}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("fixture exists")
}

fn export(block: &AnnotatedBlock, timing: Timing) -> Vec<SpanData> {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_max_attributes_per_span(256)
        .with_simple_exporter(exporter.clone())
        .build();
    let created = record_block(&provider.tracer("test"), block, timing);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), created);
    spans
}

fn attr(span: &SpanData, key: &str) -> Option<Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| kv.value.clone())
}

fn by_name<'a>(spans: &'a [SpanData], name: &str) -> &'a SpanData {
    spans
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no span named {name}"))
}

#[test]
fn builds_block_phase_op_tree() {
    let json = String::from_utf8(fixture("model/two-extrinsics.json")).unwrap();
    let block = TracedBlock::from_trace(&parse_trace_block(&json).unwrap()).unwrap();
    let md = Metadata::from_bytes(&fixture("metadata/westend-v16.scale")).unwrap();
    let annotated = annotate(&block, Some(&md));
    let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let spans = export(
        &annotated,
        Timing {
            start,
            op_width: Duration::from_millis(1),
        },
    );

    assert_eq!(spans.len(), 1 + 5 + 38);
    let trace_id = spans[0].span_context.trace_id();
    assert!(spans.iter().all(|s| s.span_context.trace_id() == trace_id));
    assert!(
        spans
            .iter()
            .all(|s| attr(s, "frame.timing") == Some("no timing".into()))
    );

    let root = by_name(&spans, "block 0x11111111…1111");
    assert_eq!(root.parent_span_id, SpanId::INVALID);
    assert_eq!(attr(root, "frame.storage.reads"), Some(Value::I64(17)));
    assert_eq!(attr(root, "frame.block.extrinsics"), Some(Value::I64(2)));
    assert_eq!(
        (root.start_time, root.end_time),
        (start, start + Duration::from_millis(38))
    );

    let phases: Vec<&SpanData> = spans
        .iter()
        .filter(|s| s.parent_span_id == root.span_context.span_id())
        .collect();
    let mut names: Vec<&str> = phases.iter().map(|s| s.name.as_ref()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "extrinsic 0: Timestamp.set",
            "extrinsic 1: Balances.transfer_keep_alive",
            "finalization",
            "initialization"
        ]
    );

    let ext1 = by_name(&spans, "extrinsic 1: Balances.transfer_keep_alive");
    assert_eq!(attr(ext1, "frame.phase"), Some("extrinsic".into()));
    assert_eq!(attr(ext1, "frame.extrinsic.index"), Some(Value::I64(1)));
    // The transaction hash a submitter knows, so load-test spans can be joined to this one.
    let hash = attr(ext1, "frame.extrinsic.hash").expect("extrinsic hash attribute");
    assert!(hash.as_str().starts_with("0x") && hash.as_str().len() == 66);
    assert_eq!(
        attr(ext1, "frame.storage.duplicate_reads"),
        Some(Value::I64(2))
    );
    assert_eq!(
        attr(ext1, "frame.storage.distinct_keys"),
        Some(Value::I64(7))
    );
    assert_eq!(
        attr(
            by_name(&spans, "extrinsic 0: Timestamp.set"),
            "frame.extrinsic.call"
        ),
        Some("Timestamp.set".into())
    );

    // Extrinsic 1 first runs inherents_applied (ops 16..19), as a nested phase span.
    let nested = by_name(&spans, "post inherents (on_poll)");
    assert_eq!(nested.parent_span_id, ext1.span_context.span_id());
    assert_eq!(nested.start_time, ext1.start_time);
    assert_eq!(
        (nested.start_time, nested.end_time),
        (
            start + Duration::from_millis(16),
            start + Duration::from_millis(19)
        )
    );
    let nested_ops = spans
        .iter()
        .filter(|s| s.parent_span_id == nested.span_context.span_id())
        .count();
    assert_eq!(nested_ops, 3);

    // Then its own ops, children of the extrinsic span, in consecutive synthetic slots.
    let mut ops: Vec<&SpanData> = spans
        .iter()
        .filter(|s| s.parent_span_id == ext1.span_context.span_id() && s.name != nested.name)
        .collect();
    ops.sort_by_key(|s| s.start_time);
    assert_eq!(ops.len(), 10);
    assert_eq!(ops[0].start_time, nested.end_time);
    assert_eq!(ops[9].end_time, ext1.end_time);
    assert!(ops.windows(2).all(|w| w[0].end_time == w[1].start_time));

    let alice = "get System::Account(5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY)";
    let (first, again) = (ops[2], ops[3]);
    assert_eq!((first.name.as_ref(), again.name.as_ref()), (alice, alice));
    assert_eq!(attr(first, "db.system.name"), Some("substrate".into()));
    assert_eq!(attr(first, "db.operation.name"), Some("get".into()));
    assert_eq!(attr(first, "db.collection.name"), Some("System".into()));
    assert_eq!(
        attr(first, "frame.storage.item"),
        Some("System::Account".into())
    );
    assert_eq!(attr(first, "frame.storage.value_len"), Some(Value::I64(16)));
    assert_eq!(
        attr(first, "frame.storage.duplicate_read"),
        Some(Value::Bool(false))
    );
    assert_eq!(
        attr(again, "frame.storage.duplicate_read"),
        Some(Value::Bool(true))
    );
    assert_eq!(attr(first, "frame.extrinsic.index"), Some(Value::I64(1)));
    assert_eq!(
        attr(first, "frame.storage.key_complete"),
        Some(Value::Bool(true))
    );
    let key = attr(first, "frame.storage.key")
        .unwrap()
        .as_str()
        .into_owned();
    assert!(key.starts_with("0x26aa394eea5630e07c48ae0c9558cef7b99d880ec681799c0cf30e8886371da9"));

    let child = ops
        .iter()
        .find(|s| attr(s, "frame.storage.child_trie").is_some())
        .unwrap();
    assert_eq!(child.name, "get child:0xbeef[0x01]");
    assert_eq!(attr(child, "db.collection.name"), Some("child trie".into()));
}

fn synthetic_block(ops: usize) -> AnnotatedBlock {
    let op = |seq| AnnotatedOp {
        timing: None,
        parent_span: None,
        seq,
        op: "get".into(),
        origin: None,
        proof_bytes_added: None,
        child_trie: None,
        child_trie_truncated: false,
        key: "0x00".into(),
        key_truncated: false,
        key_complete: false,
        pallet: None,
        item: "<unknown>".into(),
        map_keys: Vec::new(),
        name: "<unknown>".into(),
        label: "get <unknown>".into(),
        value_len: None,
        value_truncated: false,
        duplicate_read: seq > 0,
    };
    AnnotatedBlock {
        schema_version: 2,
        execution: None,
        call: None,
        runtime_override: None,
        runtime_spans: Vec::new(),
        timing: None,
        block_number: Some(7),
        block_hash: "0x01".into(),
        parent_hash: "0x00".into(),
        skipped_events: 0,
        totals: Stats::default(),
        phases: vec![AnnotatedPhase {
            kind: "unsegmented".into(),
            wall_time: None,
            signed: None,
            extrinsic_index: None,
            extrinsic_hash: None,
            call: None,
            call_error: None,
            label: "unsegmented".into(),
            stats: Stats::default(),
            ops: (0..ops).map(op).collect(),
            post_inherents: None,
        }],
    }
}

/// An OTLP/HTTP receiver that answers each request with the next status and records the
/// status and span IDs of every request it answered.
type Seen = std::sync::Arc<std::sync::Mutex<Vec<(u16, Vec<Vec<u8>>)>>>;
fn scripted_receiver(script: Vec<u16>) -> (String, Seen) {
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    use prost::Message;
    use std::io::Write;
    let seen = Seen::default();
    let log = seen.clone();
    let url = raw_receiver(move |index, body, stream| {
        let request = ExportTraceServiceRequest::decode(body).unwrap();
        // Dashboards filter on the service name, so every request must carry it.
        // A request without it gets status 599, which breaks the test's status checks.
        let named = request.resource_spans.iter().all(|r| {
            r.resource.as_ref().is_some_and(|resource| {
                resource.attributes.iter().any(|kv| {
                    kv.key == "service.name"
                        && format!("{:?}", kv.value).contains("polkadot-frame-trace")
                })
            })
        });
        let status = if named {
            script.get(index).copied().unwrap_or(200)
        } else {
            599
        };
        let ids = request
            .resource_spans
            .iter()
            .flat_map(|r| &r.scope_spans)
            .flat_map(|s| &s.spans)
            .map(|span| span.span_id.clone())
            .collect();
        let _ = write!(
            stream,
            "HTTP/1.1 {status} Test\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        );
        log.lock().unwrap().push((status, ids));
    });
    (url, seen)
}

#[test]
fn a_retry_sends_only_the_batches_not_yet_accepted_with_the_same_span_ids() {
    // 1002 spans: batch 1 is accepted, batch 2 fails, then the retry sends only batch 2.
    // 400 is not a status the exporter retries by itself, so the failure reaches the caller.
    let (url, seen) = scripted_receiver(vec![200, 400, 200]);
    let sender = polkadot_frame_trace_otel::OtlpSender::new(&url).unwrap();
    let mut trace = polkadot_frame_trace_otel::prepare(&synthetic_block(1000), Timing::default());
    let error = sender.send(&mut trace).unwrap_err().to_string();
    assert!(
        error.starts_with("490 of 1002 spans were not exported"),
        "{error}"
    );
    assert_eq!(trace.unsent(), 490);
    assert_eq!(sender.send(&mut trace).unwrap(), 1002);
    // The receiver records a request just after answering it.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while seen.lock().unwrap().len() < 3 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 3);
    assert_eq!(
        seen[1].1, seen[2].1,
        "the retry resends the failed batch unchanged"
    );
    let accepted: Vec<_> = seen
        .iter()
        .filter(|(status, _)| *status == 200)
        .flat_map(|(_, ids)| ids.clone())
        .collect();
    let unique: std::collections::BTreeSet<_> = accepted.iter().collect();
    assert_eq!(accepted.len(), 1002);
    assert_eq!(unique.len(), 1002, "no span was accepted twice");
}

/// Serves each connection with `respond(index, body, stream)` after reading a request to
/// `/v1/traces`. Requests to any other path get no answer.
fn raw_receiver(
    respond: impl Fn(usize, &[u8], &mut std::net::TcpStream) + Send + Sync + 'static,
) -> String {
    use std::io::{BufRead, BufReader, Read};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let respond = std::sync::Arc::new(respond);
    std::thread::spawn(move || {
        for (index, stream) in listener.incoming().enumerate() {
            let Ok(mut stream) = stream else { return };
            let respond = respond.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                let _ = reader.read_line(&mut request_line);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; length];
                if request_line == "POST /v1/traces HTTP/1.1\r\n"
                    && reader.read_exact(&mut body).is_ok()
                {
                    respond(index, &body, &mut stream);
                }
            });
        }
    });
    url
}

#[test]
fn a_slow_response_or_the_exporters_own_retries_cannot_outlast_the_timeout() {
    use std::io::Write;
    // Headers at once, then one body byte every 150 ms: each read is quick, the whole is not.
    let slow_body = raw_receiver(|_, _, stream| {
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/x-protobuf\r\ncontent-length: 40\r\n\r\n");
        for _ in 0..40 {
            if stream.write_all(&[0]).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    });
    // A slow 503 makes the exporter retry by itself; the retry then gets no answer at all.
    let slow_retry = raw_receiver(|index, _, stream| {
        if index == 0 {
            std::thread::sleep(Duration::from_millis(500));
            let _ = stream.write_all(
                b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            );
        } else {
            std::thread::sleep(Duration::from_secs(10));
        }
    });
    for (url, timeout, limit) in [(slow_body, 400, 700), (slow_retry, 800, 1000)] {
        let sender = polkadot_frame_trace_otel::OtlpSender::with_timeout(
            &url,
            Duration::from_millis(timeout),
        )
        .unwrap();
        let mut trace = polkadot_frame_trace_otel::prepare(&synthetic_block(10), Timing::default());
        let start = std::time::Instant::now();
        assert!(sender.send(&mut trace).is_err());
        let took = start.elapsed();
        assert!(took < Duration::from_millis(limit), "{url}: {took:?}");
    }
}

#[test]
fn otlp_partial_rejections_are_totalled_across_batches_without_retries() {
    use opentelemetry_proto::tonic::collector::trace::v1::{
        ExportTracePartialSuccess, ExportTraceServiceRequest, ExportTraceServiceResponse,
    };
    use polkadot_frame_trace_otel::Error;
    use prost::Message;
    use std::io::Write;

    // Rejected spans for each request, in order: each trace below is sent as two batches.
    let rejected = [3, 0, 2, 5, 0, 0];
    let batches = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = batches.clone();
    let url = raw_receiver(move |index, body, stream| {
        let request = ExportTraceServiceRequest::decode(body).unwrap();
        let spans: usize = request
            .resource_spans
            .iter()
            .flat_map(|r| &r.scope_spans)
            .map(|s| s.spans.len())
            .sum();
        log.lock().unwrap().push(spans);
        let body = ExportTraceServiceResponse {
            partial_success: Some(ExportTracePartialSuccess {
                rejected_spans: rejected.get(index).copied().unwrap_or(0),
                // Each message names its request, so a message left from an earlier trace shows.
                error_message: format!("request {index}"),
            }),
        }
        .encode_to_vec();
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/x-protobuf\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(&body);
    });
    // One sender for every trace, as `follow` keeps it, at an endpoint with a trailing slash.
    let sender = polkadot_frame_trace_otel::OtlpSender::new(&format!("{url}/v1/traces/")).unwrap();
    for expected in [Some((3, "request 0")), Some((7, "request 2")), None] {
        let mut trace =
            polkadot_frame_trace_otel::prepare(&synthetic_block(1000), Timing::default());
        match (sender.send(&mut trace), expected) {
            (Ok(spans), None) => assert_eq!(spans, 1002),
            (
                Err(Error::Rejected {
                    rejected,
                    total: 1002,
                    message,
                }),
                Some((expected, expected_message)),
            ) => assert_eq!((rejected, message.as_str()), (expected, expected_message)),
            (result, _) => panic!("{expected:?}: {result:?}"),
        }
    }
    let batches = batches.lock().unwrap();
    assert_eq!(batches.len(), 6, "no batch is sent twice: {batches:?}");
    assert_eq!(batches.iter().sum::<usize>(), 3 * 1002);
}

#[test]
fn runtime_override_is_labelled_on_the_root_span() {
    let mut block = synthetic_block(1);
    block.runtime_override = Some(polkadot_frame_trace_decode::RuntimeOverrideInfo {
        path: "traced.wasm".into(),
        code_hash: "0xaa".into(),
        chain_code_hash: "0xbb".into(),
        version_differences: vec!["spec_version 1 -> 2".into()],
    });
    let spans = export(&block, Timing::default());
    let root = by_name(&spans, "block #7 (0x01) [runtime override]");
    assert_eq!(
        attr(root, "frame.runtime.override"),
        Some(Value::Bool(true))
    );
    assert_eq!(
        attr(root, "frame.runtime.override_code_hash"),
        Some(Value::from("0xaa"))
    );
    assert_eq!(
        attr(root, "frame.runtime.chain_code_hash"),
        Some(Value::from("0xbb"))
    );
    assert_eq!(
        attr(root, "frame.runtime.override_version_differences"),
        Some(Value::from("spec_version 1 -> 2"))
    );
}

#[test]
fn traced_batch_keeps_named_extrinsic_runtime_and_ops_in_one_tree() {
    let replay = polkadot_frame_trace_replay::replay_with_session(
        polkadot_frame_trace_replay::Artifact::load(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/replay/people-traced"),
        )
        .unwrap(),
        None,
        &polkadot_frame_trace_replay::ReplaySession::new().unwrap(),
    )
    .unwrap();
    let metadata = Metadata::from_hex(&replay.artifact.metadata).unwrap();
    let traced = TracedBlock::from_replay(&replay.capture).unwrap();
    // Every op is measured: its interval is the runtime span that performed it.
    assert!(traced.ops.iter().all(|op| op.timing.is_some()));
    let mut block = annotate(&traced, Some(&metadata));
    let spans = export(&block, Timing::default());
    let ids: std::collections::HashMap<_, _> = spans
        .iter()
        .map(|s| (s.span_context.span_id(), s))
        .collect();
    for span in &spans {
        assert_eq!(attr(span, "frame.timing"), Some("timed replay".into()));
        if attr(span, "frame.phase").is_some() {
            assert_eq!(
                attr(span, "frame.phase.timing_available"),
                Some(true.into()),
                "{}",
                span.name
            );
        }
        // Measured intervals nest: each child lies inside its parent.
        if let Some(parent) = ids.get(&span.parent_span_id) {
            assert!(
                parent.start_time <= span.start_time && span.end_time <= parent.end_time,
                "{} is not inside {}",
                span.name,
                parent.name
            );
        }
    }
    let phase = spans
        .iter()
        .find(|s| s.name.contains("Utility.batch") && s.name.starts_with("extrinsic"))
        .unwrap();
    assert!(attr(phase, "frame.storage.ops").is_some());
    let batch = by_name(&spans, "batch");
    let mut current = batch;
    let mut found = false;
    for _ in 0..spans.len() {
        if current.span_context.span_id() == phase.span_context.span_id() {
            found = true;
            break;
        }
        current = ids.get(&current.parent_span_id).unwrap();
    }
    assert!(
        found,
        "batch must descend from the named Utility.batch phase"
    );
    assert!(
        spans.iter().any(|s| attr(s, "db.operation.name").is_some()
            && s.parent_span_id == batch.span_context.span_id())
            || spans.iter().any(|s| s.name.contains("transfer_keep_alive")
                && s.parent_span_id == batch.span_context.span_id())
    );
    for span in &spans {
        if attr(span, "frame.proof.marginal_bytes").is_some() {
            assert_eq!(span.name, "apply_extrinsic");
        }
        if span.name == "apply_extrinsic" {
            let parent = ids[&span.parent_span_id];
            assert_eq!(attr(parent, "frame.phase"), Some("extrinsic".into()));
            assert!(
                spans
                    .iter()
                    .any(|s| s.parent_span_id == span.span_context.span_id())
            );
        }
    }
    for phase in block.phases.iter().filter(|p| p.extrinsic_index.is_some()) {
        let exported = by_name(&spans, &phase.label);
        assert!(
            spans
                .iter()
                .any(|s| s.parent_span_id == exported.span_context.span_id())
        );
    }

    // Without runtime spans, ops hang directly under their phase and keep their measured
    // times. An empty phase has no bounds of its own: it spans the block and says so.
    block.runtime_spans.clear();
    let mut empty = block.phases[0].clone();
    (empty.kind, empty.label) = ("unsegmented".into(), "empty phase".into());
    (empty.ops, empty.post_inherents, empty.extrinsic_index) = (vec![], None, None);
    block.phases.push(empty);
    let start = SystemTime::UNIX_EPOCH;
    let at = |ns| start + Duration::from_nanos(ns);
    let spans = export(
        &block,
        Timing {
            start,
            ..Timing::default()
        },
    );
    let ids: std::collections::HashMap<_, _> = spans
        .iter()
        .map(|s| (s.span_context.span_id(), s))
        .collect();
    let ops: Vec<_> = spans
        .iter()
        .filter(|s| attr(s, "db.operation.name").is_some())
        .collect();
    for op in &ops {
        assert!(attr(ids[&op.parent_span_id], "frame.phase").is_some());
    }
    let mut exported: Vec<_> = ops.iter().map(|s| (s.start_time, s.end_time)).collect();
    let mut measured: Vec<_> = block
        .all_phases()
        .flat_map(|p| &p.ops)
        .map(|op| op.timing.unwrap())
        .map(|t| (at(t.start_ns), at(t.start_ns + t.duration_ns)))
        .collect();
    exported.sort();
    measured.sort();
    assert_eq!(exported, measured);
    let empty = by_name(&spans, "empty phase");
    assert_eq!(attr(empty, "frame.timing"), Some("no timing".into()));
    assert_eq!(
        attr(empty, "frame.phase.timing_available"),
        Some(false.into())
    );
    let whole = block.timing.unwrap().duration_ns;
    assert_eq!((empty.start_time, empty.end_time), (at(0), at(whole)));
}

#[test]
fn hook_names_resolve_pallet_metadata_and_fall_back_to_crates() {
    use polkadot_frame_trace_model::{MeasuredTiming, RuntimeSpan};
    let json = String::from_utf8(fixture("model/two-extrinsics.json")).unwrap();
    let mut traced = TracedBlock::from_trace(&parse_trace_block(&json).unwrap()).unwrap();
    traced.timing = Some(MeasuredTiming {
        start_ns: 0,
        duration_ns: 100,
    });
    traced.runtime_spans = ["on_initialize", "on_finalize", "on_idle", "on_poll"]
        .into_iter()
        .enumerate()
        .map(|(i, name)| RuntimeSpan {
            id: i as u64 + 1,
            parent_id: None,
            name: name.into(),
            target: "pallet_balances::pallet".into(),
            wasm: true,
            fields: Default::default(),
            start_ns: 10 + i as u64 * 10,
            end_ns: Some(20 + i as u64 * 10),
            duration_ns: 10,
            enters: 1,
            proof_start_bytes: 0,
            proof_end_bytes: 0,
        })
        .collect();
    let md = Metadata::from_bytes(&fixture("metadata/westend-v16.scale")).unwrap();
    assert_eq!(
        md.pallet_for_crate("pallet_balances"),
        Some("Balances".into())
    );
    let block = annotate(&traced, Some(&md));
    let spans = export(&block, Timing::default());
    for (hook, phase) in [
        ("on_initialize", "initialization"),
        ("on_finalize", "finalization"),
        ("on_idle", "finalization"),
        ("on_poll", "post inherents (on_poll)"),
    ] {
        let hook = by_name(&spans, &format!("{hook}: Balances"));
        assert_eq!(
            hook.parent_span_id,
            by_name(&spans, phase).span_context.span_id()
        );
        assert_eq!(
            attr(hook, "frame.runtime.target"),
            Some("pallet_balances::pallet".into())
        );
    }
    let fallback = annotate(&traced, None);
    assert_eq!(fallback.runtime_spans[0].name, "on_initialize: Balances");
    let artifact = polkadot_frame_trace_replay::Artifact::load(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/replay/call-westend-nonce"),
    )
    .unwrap();
    let asset_md = Metadata::from_hex(&artifact.metadata).unwrap();
    let candidates = asset_md.pallet_for_crate("pallet_assets").unwrap();
    assert!(candidates.contains(" | "), "{candidates}");
    for span in &mut traced.runtime_spans[..3] {
        span.target = "pallet_assets::pallet".into();
        span.name = "on_initialize".into();
    }
    let instances = annotate(&traced, Some(&asset_md));
    assert_eq!(
        instances.runtime_spans[..3]
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        [
            "on_initialize: Assets",
            "on_initialize: ForeignAssets",
            "on_initialize: PoolAssets"
        ]
    );
    let people = polkadot_frame_trace_replay::Artifact::load(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/replay/people-traced"),
    )
    .unwrap();
    let people_md = Metadata::from_hex(&people.metadata).unwrap();
    let instances = annotate(&traced, Some(&people_md));
    assert_eq!(
        instances.runtime_spans[0].name,
        "on_initialize: Assets | PoolAssets"
    );
    assert_eq!(
        instances.runtime_spans[1].name,
        "on_initialize: Assets | PoolAssets"
    );
}

#[test]
fn every_storage_op_has_a_collection_including_well_known_roots_and_iteration() {
    use polkadot_frame_trace_model::{ReplayCapture, ReplayEvent};
    let system = "26aa394eea5630e07c48ae0c9558cef7";
    let capture = ReplayCapture {
        duration_ns: 100,
        events: [
            (
                "state",
                "Get",
                "key",
                polkadot_frame_trace_replay::rpc::hex0(b":code"),
            ),
            (
                "state",
                "Get",
                "key",
                polkadot_frame_trace_replay::rpc::hex0(b":extrinsic_index"),
            ),
            (
                "state",
                "Get",
                "key",
                polkadot_frame_trace_replay::rpc::hex0(b":transaction_level:"),
            ),
            ("state", "StorageRoot", "storage_root", "00".into()),
            (
                "frame_trace_backend",
                "BackendIterNextKey",
                "prefix",
                system.into(),
            ),
            (
                "frame_trace_backend",
                "BackendIterNextPair",
                "prefix",
                String::new(),
            ),
        ]
        .into_iter()
        .enumerate()
        .map(|(seq, (target, method, field, value))| ReplayEvent {
            seq,
            at_ns: seq as u64 * 10,
            parent_id: None,
            target: target.into(),
            fields: [("method".into(), method.into()), (field.into(), value)].into(),
        })
        .collect(),
        ..Default::default()
    };
    let traced = TracedBlock::from_replay(&capture).unwrap();
    let md = Metadata::from_bytes(&fixture("metadata/westend-v16.scale")).unwrap();
    let spans = export(&annotate(&traced, Some(&md)), Timing::default());
    let mut ops: Vec<_> = spans
        .iter()
        .filter(|s| attr(s, "db.operation.name").is_some())
        .collect();
    ops.sort_by_key(|s| attr(s, "frame.storage.seq").map(|v| v.to_string()));
    let collections: Vec<_> = ops
        .iter()
        .map(|s| attr(s, "db.collection.name").unwrap().to_string())
        .collect();
    assert_eq!(
        collections,
        [
            "well known keys",
            "well known keys",
            "well known keys",
            "state",
            "System",
            "state"
        ]
    );
}

#[test]
fn runtime_call_root_and_empty_calls_export_one_trace_with_explicit_semantics() {
    for (ops, measured) in [(0, false), (2, false), (0, true), (2, true)] {
        let mut trace = synthetic_block(ops);
        trace.timing = measured.then_some(polkadot_frame_trace_model::MeasuredTiming {
            start_ns: 0,
            duration_ns: 1_000_000,
        });
        trace.call = Some(polkadot_frame_trace_decode::AnnotatedCall {
            method: "AccountNonceApi_account_nonce".into(),
            data: "0x00".into(),
            result: "0x00000000".into(),
            decoded: Some("0".into()),
            oracle_match: Some(true),
            oracle_source: Some("live".into()),
            proof_bytes: 1234,
            state_root: "0xab".into(),
        });
        let spans = export(&trace, Timing::default());
        let root = by_name(&spans, "call AccountNonceApi_account_nonce at #7");
        assert_eq!(root.parent_span_id, SpanId::INVALID);
        assert_eq!(attr(root, "frame.trace.kind"), Some("call".into()));
        assert_eq!(
            attr(root, "frame.timing"),
            Some(
                if measured {
                    "timed replay"
                } else {
                    "no timing"
                }
                .into()
            )
        );
        assert_eq!(
            attr(root, "frame.root_check"),
            Some("not applicable".into())
        );
        assert_eq!(attr(root, "frame.call.oracle_match"), Some(true.into()));
        assert_eq!(attr(root, "frame.call.oracle_source"), Some("live".into()));
        assert_eq!(attr(root, "frame.proof.bytes"), Some(1234i64.into()));
        assert!(
            spans
                .iter()
                .all(|s| s.span_context.trace_id() == root.span_context.trace_id())
        );
        assert_eq!(
            spans.len(),
            polkadot_frame_trace_otel::prepare(&trace, Timing::default()).total()
        );
    }
}

#[test]
fn observed_read_origins_and_proof_bytes_are_otlp_attributes() {
    // (origin in the JSON, exported origin, proof bytes)
    let rows = [
        (Some("overlay"), "cache", Some(0)),
        (Some("backend_warm"), "trie (already in proof)", Some(0)),
        (Some("backend_cold"), "trie (new)", Some(100)),
        (None, "unknown", None),
        (Some("unknown"), "unknown", None),
        (Some("legacy_origin"), "unknown", None),
    ];
    let mut block = synthetic_block(rows.len());
    // A timed replay, but the ops themselves have no timing.
    block.timing = Some(polkadot_frame_trace_model::MeasuredTiming {
        start_ns: 0,
        duration_ns: 100,
    });
    for (op, (raw, _, proof)) in block.phases[0].ops.iter_mut().zip(rows) {
        op.origin = raw.map(Into::into);
        op.proof_bytes_added = proof;
    }
    let spans = export(&block, Timing::default());
    for (seq, (raw, label, proof)) in rows.into_iter().enumerate() {
        // The JSON keeps the raw value.
        assert_eq!(block.phases[0].ops[seq].origin.as_deref(), raw);
        let span = spans
            .iter()
            .find(|s| attr(s, "frame.storage.seq") == Some((seq as i64).into()))
            .unwrap();
        assert_eq!(attr(span, "frame.storage.origin"), Some(label.into()));
        assert_eq!(attr(span, "frame.storage.origin_raw"), raw.map(Value::from));
        assert_eq!(
            attr(span, "frame.storage.proof_bytes"),
            proof.map(|bytes: usize| Value::from(bytes as i64))
        );
        // Missing operation timing stays explicit even within a timed replay.
        assert_eq!(attr(span, "frame.timing"), Some("no timing".into()));
    }
}
