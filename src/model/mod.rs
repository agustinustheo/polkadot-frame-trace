//! Block trace model for polkadot-frame-trace.
//!
//! Turns captured `state` events into ordered
//! [`StorageOp`]s, splits them into execution [`Phase`]s and computes [`Stats`]. Pure data and
//! logic: no I/O, no runtime metadata.

mod block;
pub mod keys;
mod op;
mod phase;
pub(crate) mod redact;
pub(crate) mod replay;
pub mod rpc;
pub use replay::{MeasuredTiming, ReplayCapture, ReplayEvent, RuntimeSpan, SpanTransition};
mod stats;

pub use block::{Phase, TracedBlock};
pub use op::{OpKind, PrintedBytes, StorageOp};
pub use phase::PhaseKind;
pub use stats::Stats;

/// Errors from parsing a block trace.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The input is not valid JSON or does not have the expected shape.
    #[error("invalid trace JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// A storage event had an unexpected format.
    #[error("event {event_index}: {reason}")]
    Event {
        /// Index in `BlockTrace::events`.
        event_index: usize,
        /// What was wrong.
        reason: String,
    },
}
