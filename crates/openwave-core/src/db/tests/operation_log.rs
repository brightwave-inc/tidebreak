//! Storage-tier tests for the durable reverse-RPC operation log (#858).
//!
//! These exercise the transactional predicate directly, in the raw
//! `(fingerprint, body)` bytes the store persists — the concurrency race and the
//! `external_effect` gate are localized here, closest to the transaction. The
//! typed `ClaimOutcome` mapping and the crash-recovery lifecycle live in the
//! `openwave-server` durable store, over these same ops.

use std::sync::Arc;

use sea_orm_migration::MigratorTrait;
use uuid::Uuid;

use super::temp_store;
use crate::db::migration::Migrator;
use crate::storage::{OperationClaimOutcome, OperationLogState, OperationLogWrite, Store};

#[tokio::test]
async fn fresh_claim_records_and_replays_the_body() {
    let (_dir, store) = temp_store().await;
    let run = Uuid::new_v4();
    let op = Uuid::new_v4();
    let epoch = Uuid::new_v4();

    assert_eq!(
        store
            .claim_operation(run, op, b"fingerprint", true, epoch)
            .await
            .unwrap(),
        OperationClaimOutcome::Fresh
    );
    assert_eq!(store.operation_log_len(run).await.unwrap(), 1);

    assert_eq!(
        store
            .record_operation(run, op, b"the-answer")
            .await
            .unwrap(),
        OperationLogWrite::Committed
    );

    // A re-issue with the same fingerprint replays the recorded body, whatever
    // epoch asks — a fresh process lifetime included.
    let later_epoch = Uuid::new_v4();
    assert_eq!(
        store
            .claim_operation(run, op, b"fingerprint", true, later_epoch)
            .await
            .unwrap(),
        OperationClaimOutcome::Recorded(b"the-answer".to_vec())
    );

    let entry = store.operation_state(run, op).await.unwrap().unwrap();
    assert_eq!(entry.state, OperationLogState::Recorded);
    assert!(entry.retained);
    assert_eq!(entry.body.as_deref(), Some(&b"the-answer"[..]));
}

#[tokio::test]
async fn a_different_fingerprint_conflicts() {
    let (_dir, store) = temp_store().await;
    let run = Uuid::new_v4();
    let op = Uuid::new_v4();
    let epoch = Uuid::new_v4();

    store
        .claim_operation(run, op, b"first", true, epoch)
        .await
        .unwrap();
    assert_eq!(
        store
            .claim_operation(run, op, b"second", true, epoch)
            .await
            .unwrap(),
        OperationClaimOutcome::Conflict
    );
}

#[tokio::test]
async fn terminal_writes_are_fenced_and_idempotent() {
    let (_dir, store) = temp_store().await;
    let run = Uuid::new_v4();
    let op = Uuid::new_v4();
    let epoch = Uuid::new_v4();

    // A terminal write with no claim settles nothing.
    assert_eq!(
        store.record_operation(run, op, b"x").await.unwrap(),
        OperationLogWrite::NotClaimed
    );

    store
        .claim_operation(run, op, b"fp", true, epoch)
        .await
        .unwrap();
    assert_eq!(
        store.record_operation(run, op, b"first").await.unwrap(),
        OperationLogWrite::Committed
    );
    // A re-delivered record is acknowledged, and never overwrites the first body.
    assert_eq!(
        store.record_operation(run, op, b"second").await.unwrap(),
        OperationLogWrite::AlreadyTerminal
    );
    assert_eq!(
        store
            .operation_state(run, op)
            .await
            .unwrap()
            .unwrap()
            .body
            .as_deref(),
        Some(&b"first"[..])
    );
    // Failing an already-recorded entry is refused: no terminal flip-flop.
    assert_eq!(
        store.fail_operation(run, op, b"err").await.unwrap(),
        OperationLogWrite::NotClaimed
    );
}

#[tokio::test]
async fn concurrent_claims_yield_exactly_one_fresh() {
    let (_dir, store) = temp_store().await;
    let store = Arc::new(store);
    let run = Uuid::new_v4();
    let op = Uuid::new_v4();
    let epoch = Uuid::new_v4();

    // Many duplicates of one identity race, all from the same process lifetime.
    let mut handles = Vec::new();
    for _ in 0..16 {
        let store = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            store
                .claim_operation(run, op, b"fp", true, epoch)
                .await
                .unwrap()
        }));
    }

    let mut fresh = 0;
    let mut owned = 0;
    for handle in handles {
        match handle.await.unwrap() {
            OperationClaimOutcome::Fresh => fresh += 1,
            OperationClaimOutcome::OwnedClaim => owned += 1,
            other => panic!("unexpected concurrent claim outcome: {other:?}"),
        }
    }
    // Exactly one claim executes; the rest attach. A double external effect is
    // impossible because only one claim was ever `Fresh`.
    assert_eq!(fresh, 1, "exactly one claim may be fresh");
    assert_eq!(owned, 15);
    assert_eq!(store.operation_log_len(run).await.unwrap(), 1);
}

#[tokio::test]
async fn external_effect_flag_gates_a_foreign_claim() {
    let (_dir, store) = temp_store().await;
    let run = Uuid::new_v4();
    let first_epoch = Uuid::new_v4();
    let crashed_epoch = Uuid::new_v4();

    // An external-effect claim left dangling by a prior lifetime is ambiguous:
    // a new lifetime must not re-execute it.
    let external_op = Uuid::new_v4();
    store
        .claim_operation(run, external_op, b"fp", true, first_epoch)
        .await
        .unwrap();
    assert_eq!(
        store
            .claim_operation(run, external_op, b"fp", true, crashed_epoch)
            .await
            .unwrap(),
        OperationClaimOutcome::ForeignClaim
    );

    // A no-external-effect claim, by contrast, is safe to re-drive: ownership is
    // taken over and the claim reported fresh.
    let pure_op = Uuid::new_v4();
    store
        .claim_operation(run, pure_op, b"fp", false, first_epoch)
        .await
        .unwrap();
    assert_eq!(
        store
            .claim_operation(run, pure_op, b"fp", false, crashed_epoch)
            .await
            .unwrap(),
        OperationClaimOutcome::Fresh
    );
    // Now owned by the new lifetime: a same-epoch re-issue attaches.
    assert_eq!(
        store
            .claim_operation(run, pure_op, b"fp", false, crashed_epoch)
            .await
            .unwrap(),
        OperationClaimOutcome::OwnedClaim
    );
}

#[tokio::test]
async fn eviction_removes_the_entry() {
    let (_dir, store) = temp_store().await;
    let run = Uuid::new_v4();
    let op = Uuid::new_v4();
    store
        .claim_operation(run, op, b"fp", true, Uuid::new_v4())
        .await
        .unwrap();
    store.record_operation(run, op, b"body").await.unwrap();

    store.evict_operation(run, op).await.unwrap();
    assert!(store.operation_state(run, op).await.unwrap().is_none());
    assert_eq!(store.operation_log_len(run).await.unwrap(), 0);
}

#[tokio::test]
async fn the_operation_log_migration_is_reversible() {
    let (_dir, store) = temp_store().await;
    let run = Uuid::new_v4();
    let op = Uuid::new_v4();

    // The additive migration created the table.
    store
        .claim_operation(run, op, b"fp", true, Uuid::new_v4())
        .await
        .unwrap();

    // Rolling back the last migration drops it symmetrically...
    Migrator::down(&store.conn, Some(1)).await.unwrap();
    assert!(
        store
            .claim_operation(run, Uuid::new_v4(), b"fp", true, Uuid::new_v4())
            .await
            .is_err(),
        "the table must be gone after down"
    );

    // ...and re-applying it restores a working, empty table.
    Migrator::up(&store.conn, None).await.unwrap();
    assert_eq!(store.operation_log_len(run).await.unwrap(), 0);
}
