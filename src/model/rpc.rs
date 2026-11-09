//! Serde mirror of the `state_traceBlock` response.
//!
//! These types follow `sp_rpc::tracing` (`substrate/primitives/rpc/src/tracing.rs`) field by
//! field, including the camelCase renames, so a response from any node deserialises as is. We
//! mirror them instead of depending on `sp-rpc`, which would pull in a large part of the Polkadot
//! SDK for a handful of plain structs.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// All spans and events recorded while re-executing one block.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockTrace {
    /// Hash of the traced block (hex, `0x`-prefixed).
    pub block_hash: String,
    /// Hash of its parent.
    pub parent_hash: String,
    /// Targets the node recorded (empty means all).
    #[serde(default)]
    pub tracing_targets: String,
    /// Storage key prefixes used to filter events (empty means no filter).
    #[serde(default)]
    pub storage_keys: String,
    /// Event methods used to filter events (empty means no filter).
    #[serde(default)]
    pub methods: String,
    /// Spans, only present for runtimes built with `with-tracing`.
    #[serde(default)]
    pub spans: Vec<Span>,
    /// Events in emission order. Storage events have target `state`.
    #[serde(default)]
    pub events: Vec<Event>,
}

/// A tracing span. The RPC carries no timing for spans.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    /// Span id.
    pub id: u64,
    /// Explicit parent, if any.
    pub parent_id: Option<u64>,
    /// Span name.
    pub name: String,
    /// Span target (usually a module path).
    pub target: String,
    /// Whether the span came from the Wasm runtime.
    pub wasm: bool,
}

/// A tracing event. Only string values survive the conversion in `sc-tracing`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// Event target, `state` for storage events.
    pub target: String,
    /// Recorded field values.
    #[serde(default)]
    pub data: Data,
    /// Explicit parent span, if any.
    pub parent_id: Option<u64>,
}

/// Values recorded on an event.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Data {
    /// Field name to formatted value.
    #[serde(default)]
    pub string_values: HashMap<String, String>,
}

impl Event {
    /// The value of a string field, if recorded.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.data.string_values.get(name).map(String::as_str)
    }
}
