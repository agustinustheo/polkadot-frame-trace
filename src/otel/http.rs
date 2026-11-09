//! Blocking OTLP/HTTP protobuf export, including partial rejection responses.

use std::{io::Read, time::Duration};

use crate::model::redact::{endpoint, untrusted};
use opentelemetry_proto::{
    tonic::collector::trace::v1::{
        ExportTracePartialSuccess, ExportTraceServiceRequest, ExportTraceServiceResponse,
    },
    transform::trace::tonic::group_spans_by_resource_and_scope,
};
use opentelemetry_sdk::trace::SpanData;
use prost::Message;
use reqwest::{
    blocking::Client,
    header::{HeaderMap, HeaderName, HeaderValue},
};

/// Largest response body read from the receiver.
const MAX_RESPONSE_BYTES: usize = 4 << 20;

#[derive(Debug)]
pub(crate) struct HttpExporter {
    client: Client,
    url: reqwest::Url,
    headers: HeaderMap,
    timeout: Duration,
}

impl HttpExporter {
    pub(crate) fn new(url: String, timeout: Option<Duration>) -> Result<Self, crate::otel::Error> {
        let invalid =
            || crate::otel::Error::Build(format!("invalid endpoint '{}'", endpoint(&url)));
        if url.chars().any(char::is_whitespace) {
            return Err(invalid());
        }
        let mut parsed = reqwest::Url::parse(&url).map_err(|_| invalid())?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(invalid());
        }
        // Authentication is supplied by headers, as with the previous exporter.
        let _ = parsed.set_username("");
        let _ = parsed.set_password(None);
        let timeout = timeout.unwrap_or_else(|| {
            [
                "OTEL_EXPORTER_OTLP_TRACES_TIMEOUT",
                "OTEL_EXPORTER_OTLP_TIMEOUT",
            ]
            .into_iter()
            .find_map(|name| std::env::var(name).ok()?.parse().ok())
            .map_or(Duration::from_secs(10), Duration::from_millis)
        });
        let mut headers = HeaderMap::new();
        if let Ok(input) = std::env::var("OTEL_EXPORTER_OTLP_TRACES_HEADERS")
            .or_else(|_| std::env::var("OTEL_EXPORTER_OTLP_HEADERS"))
        {
            for pair in input.split(',') {
                let Some((key, value)) = pair.split_once('=') else {
                    continue;
                };
                let value = percent_encoding::percent_decode_str(value.trim()).decode_utf8_lossy();
                if let (Ok(key), Ok(value)) = (
                    HeaderName::from_bytes(key.trim().as_bytes()),
                    HeaderValue::from_str(&value),
                ) {
                    headers.insert(key, value);
                }
            }
        }
        let client = Client::builder()
            .build()
            .map_err(|e| crate::otel::Error::Build(e.without_url().to_string()))?;
        Ok(Self {
            client,
            url: parsed,
            headers,
            timeout,
        })
    }

    pub(crate) fn send(&self, batch: Vec<SpanData>) -> Result<(u64, String), String> {
        let body = ExportTraceServiceRequest {
            resource_spans: group_spans_by_resource_and_scope(
                batch,
                &(&crate::otel::resource()).into(),
            ),
        }
        .encode_to_vec();
        let response = self
            .client
            .post(self.url.clone())
            .headers(self.headers.clone())
            .header("content-type", "application/x-protobuf")
            // A request timeout also covers the whole body, even across multiple reads.
            .timeout(self.timeout)
            .body(body)
            .send()
            .map_err(|e| e.without_url().to_string())?;
        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status()));
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let mut body = Vec::new();
        response
            .take(MAX_RESPONSE_BYTES as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|_| "failed to read OTLP response".to_owned())?;
        if body.len() > MAX_RESPONSE_BYTES {
            return Err("response body too large".into());
        }
        let partial = partial_success(&content_type, &body).unwrap_or_default();
        Ok((
            partial.rejected_spans.max(0) as u64,
            untrusted(&partial.error_message),
        ))
    }
}

fn partial_success(content_type: &str, body: &[u8]) -> Option<ExportTracePartialSuccess> {
    let json = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("application/json");
    if !json {
        return ExportTraceServiceResponse::decode(body)
            .ok()?
            .partial_success;
    }
    // OTLP JSON represents int64 as a decimal string; accept numbers as well.
    let response: serde_json::Value = serde_json::from_slice(body).ok()?;
    let partial = response
        .get("partialSuccess")
        .or_else(|| response.get("partial_success"))?;
    let rejected = partial
        .get("rejectedSpans")
        .or_else(|| partial.get("rejected_spans"));
    let rejected_spans = match rejected {
        None => 0,
        Some(value) => value.as_i64().or_else(|| value.as_str()?.parse().ok())?,
    };
    Some(ExportTracePartialSuccess {
        rejected_spans,
        error_message: partial
            .get("errorMessage")
            .or_else(|| partial.get("error_message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}
