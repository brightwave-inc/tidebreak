use super::*;

/// The undo window is a bounded number of turns, and the bound is what keeps
/// snapshot storage from growing with editing activity. Falling out of the
/// window has to release the retained bytes too: a row deleted without
/// enqueueing its blob leaves the file behind forever with nothing pointing at
/// it, which is the leak this bound exists to avoid.
#[tokio::test]
async fn the_file_change_journal_keeps_only_the_newest_retained_turns() {
    use crate::model::{
        ExecFileChange, ExecFileSnapshotRecord, ExecUndoState, EXEC_SNAPSHOT_RETAINED_TURNS,
    };

    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let mut turns = Vec::new();
    for index in 0..(EXEC_SNAPSHOT_RETAINED_TURNS + 1) {
        let blob = DocumentBlob::from_bytes(format!("revision {index}").as_bytes());
        let turn_id = TurnId::new();
        store
            .record_exec_file_snapshots(
                chat.id,
                turn_id,
                &[ExecFileSnapshotRecord {
                    folder_path: "/Users/someone/Documents".into(),
                    relative_path: "notes.md".into(),
                    change: ExecFileChange::Overwritten,
                    prior_blob_id: Some(blob.id),
                    prior_byte_len: Some(blob.byte_len),
                    new_sha256: None,
                    undo: ExecUndoState::Available,
                }],
            )
            .await
            .unwrap();
        turns.push((turn_id, blob.id));
    }

    let retained = store.list_exec_file_snapshots(chat.id).await.unwrap();
    assert_eq!(retained.len(), EXEC_SNAPSHOT_RETAINED_TURNS);
    let (oldest_turn, oldest_blob) = turns[0];
    assert!(
        retained.iter().all(|row| row.turn_id != oldest_turn),
        "the turn that fell out of the window is still journaled"
    );
    assert_eq!(
        store
            .get_blob_retirement(oldest_blob)
            .await
            .unwrap()
            .map(|retirement| retirement.status),
        Some(BlobRetirementStatus::Queued),
        "its retained bytes were dropped without being released"
    );
    let (newest_turn, newest_blob) = *turns.last().unwrap();
    assert_eq!(retained.first().unwrap().turn_id, newest_turn);
    assert_eq!(store.get_blob_retirement(newest_blob).await.unwrap(), None);
}

/// Applied and rejected changes share one journal but not one write: a turn
/// records each in its own transaction, so its rows carry different timestamps.
/// Retention is a window over turns, and a turn inside it keeps both halves —
/// pruning that cut on a timestamp instead would retract the earlier half of a
/// turn it was supposed to be retaining.
#[tokio::test]
async fn retention_keeps_both_halves_of_a_turn_recorded_in_two_writes() {
    use crate::model::{
        ExecFileChange, ExecFileRejectionReason, ExecFileRejectionRecord, ExecFileSnapshotRecord,
        ExecUndoState, EXEC_SNAPSHOT_RETAINED_TURNS,
    };

    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    // The straddling turn: its rejection is journaled before its applied
    // change, so the turn's own rows bracket every later cutoff candidate.
    let straddling_turn = TurnId::new();
    store
        .record_exec_file_rejections(
            chat.id,
            straddling_turn,
            &[ExecFileRejectionRecord {
                folder_path: "/Users/someone/Documents".into(),
                relative_path: "locked.md".into(),
                reason: ExecFileRejectionReason::Stale,
            }],
        )
        .await
        .unwrap();
    store
        .record_exec_file_snapshots(
            chat.id,
            straddling_turn,
            &[ExecFileSnapshotRecord {
                folder_path: "/Users/someone/Documents".into(),
                relative_path: "notes.md".into(),
                change: ExecFileChange::Overwritten,
                prior_blob_id: None,
                prior_byte_len: None,
                new_sha256: None,
                undo: ExecUndoState::Available,
            }],
        )
        .await
        .unwrap();

    // Fill the window exactly, so pruning runs with that turn still inside it.
    for index in 1..EXEC_SNAPSHOT_RETAINED_TURNS {
        store
            .record_exec_file_snapshots(
                chat.id,
                TurnId::new(),
                &[ExecFileSnapshotRecord {
                    folder_path: "/Users/someone/Documents".into(),
                    relative_path: format!("later-{index}.md"),
                    change: ExecFileChange::Created,
                    prior_blob_id: None,
                    prior_byte_len: None,
                    new_sha256: None,
                    undo: ExecUndoState::Available,
                }],
            )
            .await
            .unwrap();
    }

    let rejections = store.list_exec_file_rejections(chat.id).await.unwrap();
    assert!(
        rejections.iter().any(|row| row.turn_id == straddling_turn),
        "a retained turn lost the half it journaled first"
    );
}
