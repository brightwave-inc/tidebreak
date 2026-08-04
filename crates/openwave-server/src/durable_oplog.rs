//! The durable, crash-safe reverse-RPC operation log (issue #858).
//!
//! This backs `openwave-sandbox-protocol`'s [`OperationStore`] seam with the
//! run's persistent [`Store`], so the operation-identity record and its recorded
//! response survive a host crash — the single biggest correctness risk the
//! reverse-RPC spike named. It is the reverse-RPC analogue of the run tier's
//! result-idempotency commit (see `docs/agent-runs.md`): the `OperationId`
//! fences a re-issue the way the result key fences a duplicate result delivery.
//!
//! # Where it lives, and why
//!
//! The transactional predicate lives beside the run tier's fenced transitions in
//! `openwave-core` (the `Store::*_operation` methods), which persist an opaque
//! `(fingerprint, body)` pair keyed by `(run_id, operation_id)` and know nothing
//! of reverse-RPC wire types. This adapter, in `openwave-server` — the crate that
//! already speaks the fenced-transaction `Store` contract *and* the sandbox
//! protocol — serializes the typed request/response to those bytes, derives the
//! external-effect flag, and maps the storage outcome onto [`ClaimOutcome`]. The
//! standalone protocol crate keeps no `openwave-core` dependency.
//!
//! # The commit predicate
//!
//! Each [`DurableOperationStore`] is scoped to one run and one *process
//! lifetime*, identified by a random `owner_epoch` minted at construction. That
//! epoch is what a durable store has and an in-memory one lacks: a `Claimed`
//! entry found under a *different* epoch is a claim a prior, now-dead lifetime
//! made and never recorded — the after-crash ambiguity. For an external-effect
//! operation (a model call with possible partial spend), that resolves to
//! [`ClaimOutcome::ClaimedElsewhere`], which the protocol host routes to a
//! conservative `OperationAmbiguous` refusal: it is never replayed. A `Claimed`
//! entry under the *same* epoch is a concurrent duplicate this lifetime and
//! attaches to the live execution ([`OperationState::Claimed`]).
//!
//! | claim finds | outcome |
//! | --- | --- |
//! | nothing | [`ClaimOutcome::Fresh`] — execute once |
//! | `Recorded`, same fingerprint | [`ClaimOutcome::Replay`] the recorded response |
//! | `Failed`, same fingerprint | [`ClaimOutcome::Replay`] the recorded failure |
//! | `Claimed`, same epoch | [`ClaimOutcome::Replay`]`(Claimed)` — concurrent duplicate attaches |
//! | terminal, body evicted (#859) | [`ClaimOutcome::Replay`]`(Evicted)` — done, do not re-execute, no body |
//! | `Claimed`, foreign epoch, external effect | [`ClaimOutcome::ClaimedElsewhere`] — after-crash refusal |
//! | any state, different fingerprint | [`ClaimOutcome::Conflict`] |

use std::sync::Arc;

use openwave_core::{OperationClaimOutcome, OperationLogState, OperationLogWrite, Store};
use openwave_sandbox_protocol::{
    ids::{OperationId, RunId},
    oplog::{ClaimOutcome, OperationFingerprint, OperationState, OperationStore, StoreError},
    protocol::ErrorResponse,
    reverse::{ReverseRequest, ReverseResult},
};
use uuid::Uuid;

/// A durable [`OperationStore`] over one run's persistent [`Store`].
///
/// Construct one per run per process lifetime; the `owner_epoch` minted here is
/// what separates a concurrent duplicate from an after-crash re-issue.
#[derive(Clone)]
pub struct DurableOperationStore {
    store: Arc<dyn Store>,
    run_id: RunId,
    owner_epoch: Uuid,
}

impl DurableOperationStore {
    /// A durable operation log for `run_id`, owned by a fresh process-lifetime
    /// epoch.
    #[must_use]
    pub fn new(store: Arc<dyn Store>, run_id: RunId) -> Self {
        Self {
            store,
            run_id,
            owner_epoch: Uuid::new_v4(),
        }
    }

    /// Whether a request carries an external effect that must never be replayed
    /// blindly after a crash. Model inference spends, so it does; the wildcard
    /// keeps a future capability external-by-default until proven otherwise.
    fn has_external_effect(request: &ReverseRequest) -> bool {
        match request {
            ReverseRequest::ModelInference(_) => true,
            _ => true,
        }
    }

    /// Atomically claim `id`, or resolve its existing state. See the module
    /// docs for the predicate.
    pub async fn claim_op(
        &self,
        id: OperationId,
        fingerprint: &OperationFingerprint,
    ) -> Result<ClaimOutcome, StoreError> {
        let bytes = encode(fingerprint)?;
        let external_effect = Self::has_external_effect(fingerprint);
        let outcome = self
            .store
            .claim_operation(
                self.run_id.as_uuid(),
                id.as_uuid(),
                &bytes,
                external_effect,
                self.owner_epoch,
            )
            .await
            .map_err(backend)?;
        Ok(match outcome {
            OperationClaimOutcome::Fresh => ClaimOutcome::Fresh,
            OperationClaimOutcome::Recorded(body) => {
                ClaimOutcome::Replay(OperationState::Recorded(decode::<ReverseResult>(&body)?))
            }
            OperationClaimOutcome::Failed(body) => {
                ClaimOutcome::Replay(OperationState::Failed(decode::<ErrorResponse>(&body)?))
            }
            OperationClaimOutcome::TerminalEvicted => ClaimOutcome::Replay(OperationState::Evicted),
            OperationClaimOutcome::OwnedClaim => ClaimOutcome::Replay(OperationState::Claimed),
            OperationClaimOutcome::ForeignClaim => ClaimOutcome::ClaimedElsewhere,
            OperationClaimOutcome::Conflict => ClaimOutcome::Conflict,
        })
    }

    /// Record a terminal success for a claimed identity; idempotent on redelivery.
    pub async fn record_op(
        &self,
        id: OperationId,
        result: ReverseResult,
    ) -> Result<(), StoreError> {
        let bytes = encode(&result)?;
        settle(
            self.store
                .record_operation(self.run_id.as_uuid(), id.as_uuid(), &bytes)
                .await
                .map_err(backend)?,
        )
    }

    /// Record a terminal failure for a claimed identity; idempotent on redelivery.
    pub async fn fail_op(&self, id: OperationId, error: ErrorResponse) -> Result<(), StoreError> {
        let bytes = encode(&error)?;
        settle(
            self.store
                .fail_operation(self.run_id.as_uuid(), id.as_uuid(), &bytes)
                .await
                .map_err(backend)?,
        )
    }

    /// The current state of `id`, if the log knows it.
    pub async fn state_op(&self, id: OperationId) -> Result<Option<OperationState>, StoreError> {
        let Some(entry) = self
            .store
            .operation_state(self.run_id.as_uuid(), id.as_uuid())
            .await
            .map_err(backend)?
        else {
            return Ok(None);
        };
        // A terminal entry whose body has been evicted (#859) reports `Evicted`,
        // exactly as `claim` does — the outcome is known, only the body is gone.
        // Keeping the two read paths identical is the point of this arm.
        let state = match (entry.state, entry.body) {
            (OperationLogState::Claimed, _) => OperationState::Claimed,
            (OperationLogState::Recorded, Some(body)) => {
                OperationState::Recorded(decode::<ReverseResult>(&body)?)
            }
            (OperationLogState::Failed, Some(body)) => {
                OperationState::Failed(decode::<ErrorResponse>(&body)?)
            }
            (OperationLogState::Recorded | OperationLogState::Failed, None) => {
                OperationState::Evicted
            }
        };
        Ok(Some(state))
    }

    /// Evict a terminal entry's body down to a commit marker after the sandbox
    /// acknowledges consuming its response; a later invalid re-issue then
    /// resolves as `Evicted`, never re-executing.
    pub async fn evict_op(&self, id: OperationId) -> Result<(), StoreError> {
        self.store
            .evict_operation(self.run_id.as_uuid(), id.as_uuid())
            .await
            .map_err(backend)
    }

    /// How many entries this run currently retains.
    pub async fn len_op(&self) -> Result<usize, StoreError> {
        self.store
            .operation_log_len(self.run_id.as_uuid())
            .await
            .map_err(backend)
    }
}

/// The synchronous [`OperationStore`] seam the capability host calls, bridged to
/// the async store. This runs on the host's multi-thread runtime; #823 owns
/// wiring it under `CapabilityHost` and the threading around a claim.
impl OperationStore for DurableOperationStore {
    fn claim(&self, id: OperationId, fingerprint: &OperationFingerprint) -> ClaimOutcome {
        match block_on(self.claim_op(id, fingerprint)) {
            Ok(outcome) => outcome,
            // A claim that cannot reach durable storage must fail closed: with no
            // committed claim, executing the effect would risk a double spend the
            // log could not later fence. Refuse conservatively.
            Err(error) => {
                eprintln!("openwave: durable operation-log claim failed, refusing conservatively: {error}");
                ClaimOutcome::ClaimedElsewhere
            }
        }
    }

    fn record(&self, id: OperationId, result: ReverseResult) -> Result<(), StoreError> {
        block_on(self.record_op(id, result))
    }

    fn fail(&self, id: OperationId, error: ErrorResponse) -> Result<(), StoreError> {
        block_on(self.fail_op(id, error))
    }

    /// Reads fail *open*: a backend error becomes `None` ("unknown"), so this
    /// must never gate execution — [`claim`](Self::claim) is the execution
    /// predicate and fails closed. Use this for observation only.
    fn state(&self, id: OperationId) -> Option<OperationState> {
        match block_on(self.state_op(id)) {
            Ok(state) => state,
            Err(error) => {
                eprintln!("openwave: durable operation-log state read failed: {error}");
                None
            }
        }
    }

    fn evict(&self, id: OperationId) {
        if let Err(error) = block_on(self.evict_op(id)) {
            eprintln!("openwave: durable operation-log eviction failed: {error}");
        }
    }

    /// Reads fail *open*: a backend error becomes `0`. For accounting only;
    /// never gate execution on it (that direction leans toward re-executing).
    fn len(&self) -> usize {
        match block_on(self.len_op()) {
            Ok(len) => len,
            Err(error) => {
                eprintln!("openwave: durable operation-log length read failed: {error}");
                0
            }
        }
    }
}

/// Drive an async store operation to completion from a synchronous trait method.
///
/// # Precondition (panic)
///
/// This uses [`tokio::task::block_in_place`], which **panics** on a
/// current-thread runtime and outside any runtime. `rt-multi-thread` only makes
/// the multi-thread flavor *available*; it does not guarantee the host built
/// one. So this checks the running flavor and, when it is not multi-thread,
/// fails closed with a [`StoreError`] rather than panicking. #823, which wires
/// this seam under `CapabilityHost`, MUST drive it from a multi-thread runtime.
fn block_on<T>(
    future: impl std::future::Future<Output = Result<T, StoreError>>,
) -> Result<T, StoreError> {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| handle.block_on(future))
        }
        Ok(_) => Err(StoreError::Backend(
            "durable operation store must run on a multi-thread tokio runtime".to_owned(),
        )),
        Err(_) => Err(StoreError::Backend(
            "durable operation store called outside a tokio runtime".to_owned(),
        )),
    }
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(value).map_err(|err| StoreError::Backend(err.to_string()))
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, StoreError> {
    serde_json::from_slice(bytes).map_err(|err| StoreError::Backend(err.to_string()))
}

fn backend(error: openwave_core::AgentError) -> StoreError {
    StoreError::Backend(error.to_string())
}

fn settle(write: OperationLogWrite) -> Result<(), StoreError> {
    match write {
        OperationLogWrite::Committed | OperationLogWrite::AlreadyTerminal => Ok(()),
        OperationLogWrite::NotClaimed => Err(StoreError::NotClaimed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwave_core::DbStore;
    use openwave_sandbox_protocol::reverse::{ModelInferenceParams, ModelInferenceResult};

    async fn store() -> (tempfile::TempDir, Arc<dyn Store>) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("oplog.db").display()
        );
        let store: Arc<dyn Store> = Arc::new(DbStore::connect(&url).await.unwrap());
        (dir, store)
    }

    fn request(prompt: &str) -> ReverseRequest {
        ReverseRequest::ModelInference(ModelInferenceParams {
            prompt: prompt.to_owned(),
        })
    }

    fn result(completion: &str) -> ReverseResult {
        ReverseResult::ModelInference(ModelInferenceResult {
            completion: completion.to_owned(),
        })
    }

    #[tokio::test]
    async fn a_claim_left_dangling_by_a_crash_refuses_conservatively() {
        let (_dir, store) = store().await;
        let run = RunId::new();
        let op = OperationId::new();

        // One process lifetime claims but crashes before recording — the entry is
        // left `Claimed` under the crashed epoch.
        let crashed = DurableOperationStore::new(Arc::clone(&store), run);
        assert!(matches!(
            crashed.claim_op(op, &request("spend")).await.unwrap(),
            ClaimOutcome::Fresh
        ));
        drop(crashed);

        // A fresh lifetime re-issues the same operation. It cannot prove the
        // model call did not already spend, so it refuses rather than replaying.
        let recovered = DurableOperationStore::new(Arc::clone(&store), run);
        assert!(matches!(
            recovered.claim_op(op, &request("spend")).await.unwrap(),
            ClaimOutcome::ClaimedElsewhere
        ));

        // The effect was never recorded, and the entry is still just claimed:
        // nothing re-executed it.
        assert!(matches!(
            recovered.state_op(op).await.unwrap(),
            Some(OperationState::Claimed)
        ));
    }

    #[tokio::test]
    async fn an_evicted_marker_replays_as_done_not_ambiguous() {
        let (_dir, store) = store().await;
        let run = RunId::new();
        let op = OperationId::new();
        let log = DurableOperationStore::new(store, run);

        log.claim_op(op, &request("q")).await.unwrap();
        log.record_op(op, result("done")).await.unwrap();
        // Retention (#859) evicts the completed body down to a commit marker.
        log.evict_op(op).await.unwrap();

        // A re-issue is answered "already recorded, do not re-execute" — an
        // `Evicted` replay, distinctly NOT the after-crash `ClaimedElsewhere`
        // ambiguity, and NOT a fresh claim that would re-run the model call.
        assert!(
            matches!(
                log.claim_op(op, &request("q")).await.unwrap(),
                ClaimOutcome::Replay(OperationState::Evicted)
            ),
            "an evicted terminal op must replay as done, never ClaimedElsewhere"
        );
        assert!(matches!(
            log.state_op(op).await.unwrap(),
            Some(OperationState::Evicted)
        ));
    }

    #[tokio::test]
    async fn a_reissue_replays_the_recorded_response() {
        let (_dir, store) = store().await;
        let run = RunId::new();
        let op = OperationId::new();
        let log = DurableOperationStore::new(store, run);

        log.claim_op(op, &request("q")).await.unwrap();
        log.record_op(op, result("the-answer")).await.unwrap();

        // A re-issue with the same operation identity returns the recorded body,
        // never a second execution.
        match log.claim_op(op, &request("q")).await.unwrap() {
            ClaimOutcome::Replay(OperationState::Recorded(recorded)) => {
                assert_eq!(recorded, result("the-answer"));
            }
            other => panic!("expected a recorded replay, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_reissue_with_a_different_request_conflicts() {
        let (_dir, store) = store().await;
        let run = RunId::new();
        let op = OperationId::new();
        let log = DurableOperationStore::new(store, run);

        log.claim_op(op, &request("first")).await.unwrap();
        assert!(matches!(
            log.claim_op(op, &request("second")).await.unwrap(),
            ClaimOutcome::Conflict
        ));
    }

    #[tokio::test]
    async fn recording_is_idempotent_across_redelivery() {
        let (_dir, store) = store().await;
        let run = RunId::new();
        let op = OperationId::new();
        let log = DurableOperationStore::new(store, run);

        log.claim_op(op, &request("q")).await.unwrap();
        log.record_op(op, result("first")).await.unwrap();
        // A re-delivered record is acknowledged, not rejected, and does not
        // overwrite the first-committed body.
        log.record_op(op, result("second")).await.unwrap();
        match log.claim_op(op, &request("q")).await.unwrap() {
            ClaimOutcome::Replay(OperationState::Recorded(recorded)) => {
                assert_eq!(recorded, result("first"));
            }
            other => panic!("expected the first recorded body, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_synchronous_seam_round_trips() {
        let (_dir, store) = store().await;
        let run = RunId::new();
        let op = OperationId::new();
        let log = DurableOperationStore::new(store, run);

        // Drive the trait the capability host actually calls, through the
        // blocking bridge, on a multi-thread runtime.
        assert!(matches!(
            OperationStore::claim(&log, op, &request("q")),
            ClaimOutcome::Fresh
        ));
        OperationStore::record(&log, op, result("bridged")).unwrap();
        match OperationStore::claim(&log, op, &request("q")) {
            ClaimOutcome::Replay(OperationState::Recorded(recorded)) => {
                assert_eq!(recorded, result("bridged"));
            }
            other => panic!("expected a recorded replay across the bridge, got {other:?}"),
        }
    }
}
