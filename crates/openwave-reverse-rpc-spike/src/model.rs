//! The first carried capability: host-proxied model inference.
//!
//! In the real system this is the host's model provider — the same
//! credential-free request that already crosses the execution seam. The spike
//! only needs a trait the host can call and test doubles that let a test hold a
//! completion open (to exercise cancellation, disconnect, and backpressure) and
//! count executions (to prove exactly-once under replay).

use std::{future::Future, pin::Pin};

use crate::protocol::{ModelInferenceParams, ModelInferenceResult};

/// A boxed future so the host can hold the model behind `dyn`.
pub type Completion<'a> = Pin<Box<dyn Future<Output = ModelInferenceResult> + Send + 'a>>;

/// The host's model proxy, as the reverse channel sees it.
pub trait ModelProvider: Send + Sync {
    /// Run one completion. Called at most once per `OperationId`; the host's
    /// operation log — not this trait — enforces that.
    fn complete(&self, params: ModelInferenceParams) -> Completion<'_>;
}
