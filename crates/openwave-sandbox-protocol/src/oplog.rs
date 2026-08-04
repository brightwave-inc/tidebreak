//! Operation identity and the durable-storage seam for reverse-RPC idempotency.
//!
//! Two identities travel on every reverse request, mirroring the broker's split
//! between transport correlation and idempotency identity:
//!
//! - [`RequestId`](crate::ids::RequestId) correlates one request frame with one
//!   response frame; a re-issue after reconnect uses a *new* one.
//! - [`OperationId`](crate::ids::OperationId) is the durable per-run identity; a
//!   re-issue uses the *same* one, and that is what makes the answer idempotent.
//!
//! The log is a small state machine keyed by `OperationId`:
//! [`OperationState::Claimed`] on dispatch, then terminal
//! [`OperationState::Recorded`] or [`OperationState::Failed`]. The
//! [`OperationStore`] trait is the seam a durable, crash-safe implementation
//! plugs into; this module ships only an [`InMemoryOperationStore`].
//!
//! # Storage and retention
//!
//! This crate's in-memory store is **not** crash-safe; production uses the
//! durable adapter in `openwave-server`. The shared contract is:
//!
//! 1. A durable store persists the
//!    `Claimed -> Recorded / Failed` transition on the run, committed
//!    transactionally, and answers a re-issue from `Recorded`. Critically, a
//!    re-issue that finds a `Claimed` entry *with no live in-flight execution* —
//!    the after-crash case an in-memory store can never reach, because a live
//!    process always still holds the execution — must fail conservatively with
//!    [`ErrorCode::OperationAmbiguous`](crate::protocol::ErrorCode::OperationAmbiguous)
//!    rather than re-execute a call that may already have spent. The
//!    [`ClaimOutcome::ClaimedElsewhere`] arm is where that predicate belongs;
//!    in memory it is unreachable, and [`InMemoryOperationStore`] documents why.
//! 2. A sandbox-resident loop issues a reverse request per step, so full replay
//!    bodies are retained only until the sandbox acknowledges consuming the
//!    terminal response. [`OperationStore::evict`] then reduces the durable
//!    entry to a commit marker. This bounds retained response bodies by the
//!    request lane's in-flight window; markers remain for audit and to prevent
//!    an acknowledged identity from ever executing again.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use crate::{
    ids::OperationId,
    protocol::ErrorResponse,
    reverse::{ReverseRequest, ReverseResult},
};

/// The immutable fingerprint an `OperationId` was first claimed with.
///
/// A later re-issue whose request differs is a conflict, exactly as the broker
/// treats a reused mutation identity with a different fingerprint.
pub type OperationFingerprint = ReverseRequest;

/// Where one operation stands in the log.
#[derive(Debug, Clone)]
pub enum OperationState {
    /// Claimed and dispatched; no terminal outcome recorded yet.
    Claimed,
    /// Terminal success, keyed by the operation identity.
    Recorded(ReverseResult),
    /// Terminal failure.
    Failed(ErrorResponse),
    /// The operation completed terminally, but its recorded body has since been
    /// evicted to a commit marker by retention (#859). It ran exactly once and
    /// must not be re-executed; there is simply no body left to replay. This is
    /// *not* the after-crash ambiguity — the outcome is known, only the payload
    /// is gone — so it resolves to a distinct, non-ambiguous refusal, never
    /// [`ClaimOutcome::ClaimedElsewhere`]. An in-memory store never produces it.
    Evicted,
}

/// The result of claiming an operation identity against the store.
#[derive(Debug, Clone)]
pub enum ClaimOutcome {
    /// The identity was unknown; it is now `Claimed` and the caller must
    /// execute exactly once.
    Fresh,
    /// The identity is already terminal; answer the re-issue from this outcome
    /// without re-executing.
    Replay(OperationState),
    /// The identity is `Claimed` and the durable store cannot prove whether its
    /// external effect ran — the after-crash ambiguity. The caller must fail
    /// conservatively rather than re-execute. Unreachable for
    /// [`InMemoryOperationStore`]; see its docs.
    ClaimedElsewhere,
    /// The identity was reused for a structurally different request.
    Conflict,
}

/// A crash-safe error the durable store may surface. In-memory writes never
/// fail, so [`InMemoryOperationStore`] never returns one.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The operation identity was not `Claimed` when a terminal write arrived.
    #[error("operation was not in a claimable state for a terminal write")]
    NotClaimed,
    /// The durable backend failed (I/O, transaction, or (de)serialization).
    ///
    /// A durable store surfaces this so a caller never mistakes an unreachable
    /// database for a clean "not claimed"; the in-memory store never returns it.
    #[error("operation store backend failure: {0}")]
    Backend(String),
}

/// The durable-storage seam for the reverse-RPC operation log.
///
/// A production implementation persists these transitions transactionally on
/// the run (see the module-level TODO). The trait is intentionally small: claim
/// an identity, record its terminal outcome, read it back, and evict it once it
/// can no longer be re-issued.
pub trait OperationStore: Send + Sync {
    /// Atomically claim `id` for `fingerprint`, or observe its existing state.
    fn claim(&self, id: OperationId, fingerprint: &OperationFingerprint) -> ClaimOutcome;

    /// Record a terminal success for a previously `Claimed` identity.
    ///
    /// # Errors
    /// [`StoreError::NotClaimed`] if the identity is not currently `Claimed`.
    fn record(&self, id: OperationId, result: ReverseResult) -> Result<(), StoreError>;

    /// Record a terminal failure for a previously `Claimed` identity.
    ///
    /// # Errors
    /// [`StoreError::NotClaimed`] if the identity is not currently `Claimed`.
    fn fail(&self, id: OperationId, error: ErrorResponse) -> Result<(), StoreError>;

    /// The current state of `id`, if the log knows it.
    fn state(&self, id: OperationId) -> Option<OperationState>;

    /// Drop a terminal entry's replay body once the sandbox acknowledges it has
    /// consumed the response and can no longer re-issue `id`.
    ///
    /// Stores keep a commit marker so a stale or invalid re-issue can never
    /// execute again. Durable stores persist that marker; the in-memory
    /// reference store represents it as [`OperationState::Evicted`].
    fn evict(&self, id: OperationId);

    /// How many operations the log currently retains. Chiefly for tests and,
    /// later, retention accounting.
    fn len(&self) -> usize;

    /// Whether the log is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A process-local, insert-mostly [`OperationStore`] for the reference backend
/// and tests.
///
/// It is **not** crash-safe: because a live process always still holds the
/// in-flight execution behind a `Claimed` entry, [`ClaimOutcome::ClaimedElsewhere`]
/// is unreachable here — an in-memory `Claimed` is never crash-ambiguous, it is
/// simply concurrent, and a concurrent duplicate attaches to the running
/// execution instead (handled by the caller, not the store). A durable store
/// must return [`ClaimOutcome::ClaimedElsewhere`] for a `Claimed` entry it did
/// not itself dispatch this process lifetime.
#[derive(Clone, Default)]
pub struct InMemoryOperationStore {
    entries: Arc<Mutex<HashMap<OperationId, Entry>>>,
}

struct Entry {
    fingerprint: OperationFingerprint,
    state: OperationState,
}

impl InMemoryOperationStore {
    /// A fresh, empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl OperationStore for InMemoryOperationStore {
    fn claim(&self, id: OperationId, fingerprint: &OperationFingerprint) -> ClaimOutcome {
        let mut entries = self.entries.lock().expect("operation log lock");
        match entries.get(&id) {
            None => {
                entries.insert(
                    id,
                    Entry {
                        fingerprint: fingerprint.clone(),
                        state: OperationState::Claimed,
                    },
                );
                ClaimOutcome::Fresh
            }
            Some(entry) if &entry.fingerprint != fingerprint => ClaimOutcome::Conflict,
            Some(entry) => match &entry.state {
                // A concurrent duplicate; the caller attaches to the live
                // execution. A durable store instead cannot tell this apart
                // from an after-crash `Claimed`, and must fail closed.
                OperationState::Claimed => ClaimOutcome::Replay(OperationState::Claimed),
                terminal => ClaimOutcome::Replay(terminal.clone()),
            },
        }
    }

    fn record(&self, id: OperationId, result: ReverseResult) -> Result<(), StoreError> {
        self.settle(id, OperationState::Recorded(result))
    }

    fn fail(&self, id: OperationId, error: ErrorResponse) -> Result<(), StoreError> {
        self.settle(id, OperationState::Failed(error))
    }

    fn state(&self, id: OperationId) -> Option<OperationState> {
        self.entries
            .lock()
            .expect("operation log lock")
            .get(&id)
            .map(|entry| entry.state.clone())
    }

    fn evict(&self, id: OperationId) {
        let mut entries = self.entries.lock().expect("operation log lock");
        if let Some(entry) = entries.get_mut(&id) {
            if !matches!(entry.state, OperationState::Claimed) {
                entry.state = OperationState::Evicted;
            }
        }
    }

    fn len(&self) -> usize {
        self.entries.lock().expect("operation log lock").len()
    }
}

impl InMemoryOperationStore {
    fn settle(&self, id: OperationId, terminal: OperationState) -> Result<(), StoreError> {
        let mut entries = self.entries.lock().expect("operation log lock");
        match entries.get_mut(&id) {
            Some(entry) if matches!(entry.state, OperationState::Claimed) => {
                entry.state = terminal;
                Ok(())
            }
            _ => Err(StoreError::NotClaimed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reverse::{ModelInferenceParams, ModelInferenceResult};

    fn request(prompt: &str) -> ReverseRequest {
        ReverseRequest::ModelInference(ModelInferenceParams {
            prompt: prompt.to_owned(),
        })
    }

    fn result(text: &str) -> ReverseResult {
        ReverseResult::ModelInference(ModelInferenceResult {
            completion: text.to_owned(),
        })
    }

    #[test]
    fn claim_records_and_replays() {
        let store = InMemoryOperationStore::new();
        let id = OperationId::new();
        assert!(matches!(
            store.claim(id, &request("q")),
            ClaimOutcome::Fresh
        ));

        // A concurrent duplicate before the terminal write attaches, not conflicts.
        assert!(matches!(
            store.claim(id, &request("q")),
            ClaimOutcome::Replay(OperationState::Claimed)
        ));

        store.record(id, result("a")).unwrap();
        match store.claim(id, &request("q")) {
            ClaimOutcome::Replay(OperationState::Recorded(recorded)) => {
                assert_eq!(recorded, result("a"));
            }
            other => panic!("expected a recorded replay, got {other:?}"),
        }
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn a_different_request_conflicts() {
        let store = InMemoryOperationStore::new();
        let id = OperationId::new();
        store.claim(id, &request("first"));
        assert!(matches!(
            store.claim(id, &request("second")),
            ClaimOutcome::Conflict
        ));
    }

    #[test]
    fn terminal_write_requires_a_claim() {
        let store = InMemoryOperationStore::new();
        let id = OperationId::new();
        assert!(matches!(
            store.record(id, result("a")),
            Err(StoreError::NotClaimed)
        ));

        store.claim(id, &request("q"));
        store.record(id, result("a")).unwrap();
        // No double-settle.
        assert!(matches!(
            store.record(id, result("b")),
            Err(StoreError::NotClaimed)
        ));
    }

    #[test]
    fn eviction_leaves_a_commit_marker() {
        let store = InMemoryOperationStore::new();
        let id = OperationId::new();
        store.claim(id, &request("q"));
        store.record(id, result("a")).unwrap();
        store.evict(id);
        assert_eq!(store.len(), 1);
        assert!(matches!(store.state(id), Some(OperationState::Evicted)));
        assert!(matches!(
            store.claim(id, &request("q")),
            ClaimOutcome::Replay(OperationState::Evicted)
        ));
    }
}
