//! A small safe-RPC boundary, with cancellable deadlines, pooled connections and retries.
use crate::{Error, Result};
use polkadot_frame_trace_model::redact::{endpoint, untrusted};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::runtime::Runtime;
pub use tokio_util::sync::CancellationToken;

const ALLOWED: &[&str] = &[
    "chain_getFinalizedHead",
    "chain_getBlock",
    "chain_getHeader",
    "chain_getBlockHash",
    "state_getRuntimeVersion",
    "state_getReadProof",
    "state_getChildReadProof",
    "state_getMetadata",
    "state_call",
];

#[derive(Clone)]
pub struct Rpc {
    inner: Arc<Inner>,
    // Per-client counters; forked clients share transport but isolate measured-run accounting.
    calls: Arc<AtomicUsize>,
    activity: Arc<Activity>,
}
/// Calls of one client and its clones that are running, and when the last one ended.
struct Activity(parking_lot::Mutex<(usize, std::time::Instant)>);
impl Default for Activity {
    fn default() -> Self {
        Self(parking_lot::Mutex::new((0, std::time::Instant::now())))
    }
}
struct Inner {
    url: String,
    http: reqwest::Client,
    parallelism: usize,
    limit: tokio::sync::Semaphore,
    cancel: CancellationToken,
    deadline: Duration,
    /// Threads that send concurrent requests for one replay, shared by all forks. Started on
    /// first use, one per request slot, and kept: replays do not start threads of their own.
    pool: OnceLock<std::result::Result<rayon::ThreadPool, String>>,
}

/// Largest RPC response read. The largest real one is the proof of
/// `:code`, about 5.5 MB as hex JSON.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
fn runtime() -> Result<&'static Runtime> {
    static RUNTIME: OnceLock<std::result::Result<Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| Error::Rpc(e.clone()))
}
impl Rpc {
    /// Lazy connections. DNS, TCP and TLS share a ten-second connect deadline;
    /// the logical call (including retries) has a two-minute deadline after a slot is acquired,
    /// and each of its four attempts a quarter of that. Public nodes can take 20 s or more to
    /// prove `:code`.
    pub fn new(url: &str) -> Result<Self> {
        Self::with_options(url, 8, Duration::from_secs(120), CancellationToken::new())
    }
    pub fn with_options(
        url: &str,
        parallelism: usize,
        deadline: Duration,
        cancel: CancellationToken,
    ) -> Result<Self> {
        if !["ws://", "wss://", "http://", "https://"]
            .iter()
            .any(|s| url.starts_with(s))
        {
            return Err(Error::Rpc(
                "expected ws://, wss://, http:// or https:// URL".into(),
            ));
        }
        if !(1..=64).contains(&parallelism) {
            return Err(Error::Invalid(
                "proof parallelism must be between 1 and 64".into(),
            ));
        }
        let url = if let Some(rest) = url.strip_prefix("wss://") {
            format!("https://{rest}")
        } else if let Some(rest) = url.strip_prefix("ws://") {
            format!("http://{rest}")
        } else {
            url.to_owned()
        };
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| Error::Rpc(e.to_string()))?;
        Ok(Self {
            inner: Arc::new(Inner {
                url,
                http,
                parallelism,
                limit: tokio::sync::Semaphore::new(parallelism),
                cancel,
                deadline,
                pool: OnceLock::new(),
            }),
            calls: Default::default(),
            activity: Default::default(),
        })
    }
    pub fn fork(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            calls: Default::default(),
            activity: Default::default(),
        }
    }
    /// How long since a call of this client or its clones last ended, or since the client
    /// was made. `None` while one is running. A replay that keeps the CPU busy this long is
    /// running the runtime, not waiting on the node.
    pub fn quiet_for(&self) -> Option<Duration> {
        let (running, last) = *self.activity.0.lock();
        (running == 0).then(|| last.elapsed())
    }
    pub fn parallelism(&self) -> usize {
        self.inner.parallelism
    }
    /// Runs `jobs` at once on the shared request threads and returns their results in order.
    /// A job that panics returns an error. The request limit still applies to every call.
    pub(crate) fn run_all<T: Send, F: FnOnce() -> Result<T> + Send>(
        &self,
        jobs: impl IntoIterator<Item = F>,
    ) -> Result<Vec<T>> {
        let pool = self
            .inner
            .pool
            .get_or_init(|| {
                rayon::ThreadPoolBuilder::new()
                    .num_threads(self.parallelism())
                    .thread_name(|index| format!("rpc-{index}"))
                    .build()
                    .map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(|e| Error::Invalid(format!("starting request threads: {e}")))?;
        let jobs: Vec<_> = jobs.into_iter().collect();
        let results: Vec<_> = jobs.iter().map(|_| parking_lot::Mutex::new(None)).collect();
        pool.in_place_scope(|scope| {
            for (job, slot) in jobs.into_iter().zip(&results) {
                scope.spawn(move |_| {
                    *slot.lock() =
                        Some(std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)));
                });
            }
        });
        results
            .into_iter()
            .map(|slot| match slot.into_inner() {
                Some(Ok(result)) => result,
                _ => Err(Error::Invalid("request thread panicked".into())),
            })
            .collect()
    }
    pub fn cancelled(&self) -> bool {
        self.inner.cancel.is_cancelled()
    }
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        if !ALLOWED.contains(&method) {
            return Err(Error::MethodNotAllowed(method.into()));
        }
        if method == "state_getMetadata"
            && (params.as_array().is_none_or(|a| a.len() != 1)
                || params[0]
                    .as_str()
                    .is_none_or(|s| s.parse::<sp_core::H256>().is_err()))
        {
            return Err(Error::Rpc(
                "metadata must be fetched at an explicit parent/block hash".into(),
            ));
        }
        if method == "state_call"
            && (params.as_array().is_none_or(|a| a.len() != 3)
                || params[0].as_str().is_none_or(str::is_empty)
                || params[1].as_str().is_none_or(|s| unhex(s).is_err())
                || params[2]
                    .as_str()
                    .is_none_or(|s| s.parse::<sp_core::H256>().is_err()))
        {
            return Err(Error::Rpc(
                "state_call requires method, SCALE hex and explicit state hash".into(),
            ));
        }
        struct Running<'a>(&'a Activity);
        impl Drop for Running<'_> {
            fn drop(&mut self) {
                let mut activity = self.0.0.lock();
                *activity = (activity.0 - 1, std::time::Instant::now());
            }
        }
        self.activity.0.lock().0 += 1;
        let _running = Running(&self.activity);
        runtime()?.block_on(async {
            tokio::select! {
                biased;
                _ = self.inner.cancel.cancelled() => Err(Error::Cancelled),
                result = async {
                    // Queue wait is cancellable but does not consume the request budget.
                    let _permit = self.inner.limit.acquire().await.map_err(|_| Error::Cancelled)?;
                    tokio::time::timeout(self.inner.deadline, self.retry(method, params))
                        .await
                        .unwrap_or_else(|_| Err(Error::TransientRpc(format!("{method}: overall request deadline exceeded"))))
                } => result,
            }
        })
    }
    async fn retry(&self, method: &str, params: Value) -> Result<Value> {
        for attempt in 0..4 {
            let id = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            let request = json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params});
            // A quarter of the call's deadline, so four attempts fit. A slow node needs a longer
            // deadline, not more attempts: a large proof can take tens of seconds to arrive.
            let attempt_deadline = self.inner.deadline / 4;
            let response = tokio::time::timeout(attempt_deadline, self.once(&request))
                .await
                .unwrap_or_else(|_| Err(("attempt deadline exceeded".into(), true)));
            match response {
                Ok(body) => {
                    if body["jsonrpc"] != "2.0" || body["id"] != id && !null_id_error(&body) {
                        return Err(Error::Rpc(format!(
                            "{method}: malformed response or response ID mismatch"
                        )));
                    }
                    if let Some(error) = body.get("error").filter(|e| !e.is_null()) {
                        let lower = error.to_string().to_lowercase();
                        if attempt < 3
                            && (error["code"] == 429 || lower.contains("too many requests"))
                        {
                            self.backoff(attempt, id).await;
                            continue;
                        }
                        // The node's text is printed: bounded, and without control characters.
                        let text = untrusted(&error.to_string());
                        let message = format!("{method}: {text}");
                        if lower.contains("state already discarded")
                            || lower.contains("state is not available")
                            || lower.contains("pruned")
                        {
                            return Err(Error::Unavailable(format!(
                                "{message}; parent state may be pruned (use an archive endpoint)"
                            )));
                        }
                        if method == "state_getMetadata" && error["code"] == -32601 {
                            return Err(Error::Unavailable(message));
                        }
                        if error["code"] == 429 || lower.contains("too many requests") {
                            return Err(Error::TransientRpc(message));
                        }
                        return Err(Error::RpcResponse {
                            method: method.into(),
                            code: error["code"].as_i64().unwrap_or(-32603),
                            message: error["message"].as_str().map_or(text, untrusted),
                        });
                    }
                    return body
                        .get("result")
                        .cloned()
                        .ok_or_else(|| Error::Rpc(format!("{method}: no result")));
                }
                Err((message, transient)) => {
                    if !transient || attempt == 3 {
                        return Err(if transient {
                            Error::TransientRpc(format!("{method}: {message}"))
                        } else {
                            Error::Rpc(format!("{method}: {message}"))
                        });
                    }
                    self.backoff(attempt, id).await;
                }
            }
        }
        unreachable!("bounded retry loop returns on last attempt")
    }
    async fn backoff(&self, attempt: u32, id: usize) {
        // Capped exponential delay, with per-request and wall-clock jitter to avoid herding.
        let jitter = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |t| (t.subsec_nanos() as u64 ^ id as u64) % 101);
        tokio::time::sleep(Duration::from_millis(
            (100u64 << attempt).min(2000) + jitter,
        ))
        .await;
    }
    async fn once(&self, request: &Value) -> std::result::Result<Value, (String, bool)> {
        let response = self
            .inner
            .http
            .post(&self.inner.url)
            .json(request)
            .send()
            .await
            .map_err(|e| {
                let transient = e.is_timeout() || e.is_connect() || e.is_request();
                let shown = endpoint(&self.inner.url);
                (
                    format!("{shown}: {}", with_sources(&e.without_url())),
                    transient,
                )
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err((
                format!("HTTP {status}"),
                status.as_u16() == 429 || status.is_server_error(),
            ));
        }
        let mut response = response;
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| (with_sources(&e.without_url()), true))?
        {
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err((too_large(), false));
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|e| (format!("malformed response: {e}"), false))
    }
    pub fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    /// The newest finalized header, checked to hash to the finalized hash the node announced.
    /// A node behind a load balancer can answer the two requests from different nodes, so a
    /// mismatch or a missing header is a failure that can pass.
    pub fn finalized_header(&self) -> Result<Value> {
        let hash: sp_core::H256 = serde_json::from_value(
            self.call("chain_getFinalizedHead", json!([]))?,
        )
        .map_err(|e| Error::Rpc(format!("chain_getFinalizedHead: malformed response: {e}")))?;
        let header = self.call("chain_getHeader", json!([hash]))?;
        if header.is_null() {
            return Err(Error::TransientRpc(format!(
                "chain_getHeader: no header for the finalized block {hash:?}"
            )));
        }
        let got = serde_json::from_value::<crate::Header>(header.clone())
            .map_err(|e| Error::Rpc(format!("chain_getHeader: malformed response: {e}")))?
            .hash();
        if got != hash {
            return Err(Error::TransientRpc(format!(
                "chain_getHeader: asked for the finalized block {hash:?}, got the header of {got:?}"
            )));
        }
        Ok(header)
    }
}
/// An error and its causes, which can say why a connection failed.
fn with_sources(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text = format!("{text}: {cause}");
        source = cause.source();
    }
    text
}
fn too_large() -> String {
    format!("response larger than {MAX_RESPONSE_BYTES} bytes")
}
/// A JSON-RPC error the node sent without an ID, because it could not read the request.
fn null_id_error(body: &Value) -> bool {
    body["id"].is_null() && body.get("error").is_some_and(|e| !e.is_null())
}
pub fn unhex(s: &str) -> Result<Vec<u8>> {
    hex::decode(s.strip_prefix("0x").unwrap_or(s))
        .map_err(|e| Error::Invalid(format!("invalid hex: {e}")))
}
pub fn hex0(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        thread,
    };
    fn responses(replies: Vec<(u16, &'static str)>) -> (Rpc, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            for (status, body) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(15)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut len = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 {
                        return;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut bytes = vec![0; len];
                reader.read_exact(&mut bytes).unwrap();
                let request: Value = serde_json::from_slice(&bytes).unwrap();
                if status == 0 {
                    let _ = reader.read_to_end(&mut Vec::new());
                    continue;
                }
                let status = if status == 0 { 200 } else { status };
                let body = body.replace("$ID", &request["id"].to_string());
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (Rpc::new(&url).unwrap(), worker)
    }
    #[test]
    fn queued_calls_get_the_full_request_budget_share_admission_with_forks_and_can_be_cancelled() {
        let (mut rpc, server) =
            responses(vec![(200, r#"{"jsonrpc":"2.0","id":$ID,"result":true}"#)]);
        Arc::get_mut(&mut rpc.inner).unwrap().deadline = Duration::from_millis(200);
        let fork = rpc.fork();
        assert!(Arc::ptr_eq(&rpc.inner, &fork.inner));
        let permit = runtime()
            .unwrap()
            .block_on(rpc.inner.limit.acquire_many(rpc.parallelism() as u32))
            .unwrap();
        let (done, finished) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let value = fork.call("chain_getHeader", json!([]));
            done.send((value, fork.count())).unwrap();
        });
        assert!(matches!(
            finished.recv_timeout(Duration::from_millis(400)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(rpc.count(), 0);
        drop(permit);
        let (value, calls) = finished.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(value.unwrap(), true);
        assert_eq!(calls, 1);
        assert_eq!(rpc.count(), 0);
        worker.join().unwrap();
        server.join().unwrap();
        // A queued call is cancelled without waiting for a slot.
        let permit = runtime()
            .unwrap()
            .block_on(rpc.inner.limit.acquire_many(rpc.parallelism() as u32))
            .unwrap();
        let fork = rpc.fork();
        let (done, finished) = std::sync::mpsc::channel();
        let worker =
            thread::spawn(move || done.send(fork.call("chain_getHeader", json!([]))).unwrap());
        assert!(matches!(
            finished.recv_timeout(Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        rpc.inner.cancel.cancel();
        assert!(matches!(
            finished.recv_timeout(Duration::from_secs(2)).unwrap(),
            Err(Error::Cancelled)
        ));
        assert_eq!(rpc.count(), 0);
        drop(permit);
        worker.join().unwrap();
    }
    #[test]
    fn rate_limits_are_retried_until_exhausted_then_transient_and_cancellation_is_typed() {
        // HTTP 429 and JSON-RPC 429 errors are retried.
        let (rpc, worker) = responses(vec![
            (429, "rate limited"),
            (
                200,
                r#"{"jsonrpc":"2.0","id":$ID,"error":{"code":429,"message":"busy"}}"#,
            ),
            (200, r#"{"jsonrpc":"2.0","id":$ID,"result":"ok"}"#),
        ]);
        assert_eq!(rpc.call("chain_getHeader", json!([])).unwrap(), "ok");
        assert_eq!(rpc.count(), 3);
        worker.join().unwrap();
        let (rpc, worker) = responses(vec![(429, "busy"); 4]);
        assert!(
            rpc.call("chain_getHeader", json!([]))
                .unwrap_err()
                .is_transient()
        );
        assert_eq!(rpc.count(), 4);
        worker.join().unwrap();
        rpc.inner.cancel.cancel();
        assert!(matches!(
            rpc.call("chain_getHeader", json!([])),
            Err(Error::Cancelled)
        ));
    }
    #[test]
    fn request_timeouts_are_retried_within_the_overall_deadline() {
        let (initial, worker) = responses(vec![
            (0, r#"{"jsonrpc":"2.0","id":$ID,"result":"late"}"#),
            (200, r#"{"jsonrpc":"2.0","id":$ID,"result":"ok"}"#),
        ]);
        let rpc = Rpc::with_options(
            &initial.inner.url,
            1,
            Duration::from_secs(8),
            CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(rpc.call("chain_getHeader", json!([])).unwrap(), "ok");
        assert_eq!(rpc.count(), 2);
        worker.join().unwrap();
    }
    #[test]
    fn a_response_over_the_size_limit_is_refused_and_not_retried() {
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":$ID,"result":"{}"}}"#,
            "0".repeat(MAX_RESPONSE_BYTES)
        );
        let (rpc, worker) = responses(vec![(200, Box::leak(body.into_boxed_str()))]);
        let error = rpc
            .call("chain_getFinalizedHead", json!([]))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("response larger than 67108864 bytes"),
            "{error}"
        );
        assert_eq!(rpc.count(), 1);
        worker.join().unwrap();
    }
    #[test]
    fn a_finalized_header_of_another_block_is_a_failure_that_can_pass() {
        let header = |number| {
            use sp_runtime::traits::Header as _;
            serde_json::to_value(crate::Header::new(
                number,
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
            ))
            .unwrap()
        };
        let (announced, other) = (header(1), header(2));
        let hash = json!(
            serde_json::from_value::<crate::Header>(announced.clone())
                .unwrap()
                .hash()
        );
        let reply = |result: &Value| -> &'static str {
            Box::leak(format!(r#"{{"jsonrpc":"2.0","id":$ID,"result":{result}}}"#).into_boxed_str())
        };
        let (rpc, worker) = responses(vec![
            (200, reply(&hash)),
            (200, reply(&other)),
            (200, reply(&hash)),
            (200, reply(&announced)),
        ]);
        let error = rpc.finalized_header().unwrap_err();
        assert!(error.is_transient(), "{error}");
        assert!(error.to_string().contains("got the header of"), "{error}");
        assert_eq!(rpc.finalized_header().unwrap(), announced);
        worker.join().unwrap();
    }
}
