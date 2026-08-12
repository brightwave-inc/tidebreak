use super::*;
use crate::deliverable::{
    CreateOutput, DeliverableKind, NewOutputRevision, OutputRecord, MAX_DELIVERABLE_BYTES,
    MAX_OUTPUT_REVISIONS,
};
use crate::id::{OutputId, OutputRevisionId};

fn at(second: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(1_710_000_000 + second, 0).unwrap()
}

fn digest(seed: u8) -> [u8; 32] {
    [seed; 32]
}

fn revision(seed: u8, second: i64) -> NewOutputRevision {
    NewOutputRevision {
        id: OutputRevisionId::new(),
        byte_len: u64::from(seed) + 1,
        sha256: digest(seed),
        turn_id: None,
        producing_run_id: None,
        created_at: at(second),
    }
}

fn create_request(chat_id: ChatId, filename: &str, seed: u8) -> CreateOutput {
    CreateOutput {
        id: OutputId::new(),
        chat_id,
        filename: filename.to_owned(),
        kind: DeliverableKind::Text,
        revision: revision(seed, 0),
    }
}

async fn store_with_chat() -> (tempfile::TempDir, DbStore, Chat) {
    let (dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    (dir, store, chat)
}

#[tokio::test]
async fn creating_an_output_records_its_first_revision() {
    let (_dir, store, chat) = store_with_chat().await;
    let request = create_request(chat.id, "brief.md", 1);

    let record = store.create_output(&request).await.unwrap();

    assert_eq!(record.id, request.id);
    assert_eq!(record.chat_id, chat.id);
    assert_eq!(record.filename, "brief.md");
    assert_eq!(record.media_type, "text/markdown");
    assert_eq!(record.current_revision, request.revision.id);
    assert_eq!(record.revision_count, 1);
    assert_eq!(record.created_at, at(0));
    assert_eq!(record.updated_at, at(0));
    assert!(record.deleted_at.is_none());

    let revisions = store.list_output_revisions(record.id).await.unwrap();
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0].id, request.revision.id);
    assert_eq!(revisions[0].ordinal, 1);
    assert_eq!(revisions[0].sha256, digest(1));
    assert_eq!(revisions[0].byte_len, request.revision.byte_len);
}

#[tokio::test]
async fn updating_an_output_keeps_the_replaced_revision_addressable() {
    let (_dir, store, chat) = store_with_chat().await;
    let request = create_request(chat.id, "brief.md", 1);
    let first = store.create_output(&request).await.unwrap();
    let second = revision(2, 30);

    let updated = store
        .append_output_revision(first.id, &second)
        .await
        .unwrap();

    assert_eq!(updated.current_revision, second.id);
    assert_eq!(updated.revision_count, 2);
    assert_eq!(updated.created_at, at(0), "creation time is immutable");
    assert_eq!(updated.updated_at, at(30));

    // The point of the whole slice: the prior revision still resolves.
    let replaced = store
        .get_output_revision(first.current_revision)
        .await
        .unwrap()
        .expect("the replaced revision is retained");
    assert_eq!(replaced.ordinal, 1);
    assert_eq!(replaced.sha256, digest(1));

    let revisions = store.list_output_revisions(first.id).await.unwrap();
    assert_eq!(
        revisions
            .iter()
            .map(|entry| entry.ordinal)
            .collect::<Vec<_>>(),
        [2, 1],
        "revisions list newest first"
    );
}

#[tokio::test]
async fn an_exact_retry_never_creates_a_second_output_or_revision() {
    let (_dir, store, chat) = store_with_chat().await;
    let request = create_request(chat.id, "brief.md", 1);
    let created = store.create_output(&request).await.unwrap();

    let retried = store.create_output(&request).await.unwrap();
    assert_eq!(retried, created);

    let second = revision(2, 30);
    let appended = store
        .append_output_revision(created.id, &second)
        .await
        .unwrap();
    let appended_again = store
        .append_output_revision(created.id, &second)
        .await
        .unwrap();
    assert_eq!(appended_again, appended);
    assert_eq!(appended_again.revision_count, 2);
    assert_eq!(
        store.list_output_revisions(created.id).await.unwrap().len(),
        2
    );
    assert_eq!(store.list_outputs(chat.id, 10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn reusing_an_identity_for_different_content_is_rejected() {
    let (_dir, store, chat) = store_with_chat().await;
    let request = create_request(chat.id, "brief.md", 1);
    let created = store.create_output(&request).await.unwrap();

    let mut conflicting = request.clone();
    conflicting.filename = "other.md".to_owned();
    assert!(store.create_output(&conflicting).await.is_err());

    let second = revision(2, 30);
    store
        .append_output_revision(created.id, &second)
        .await
        .unwrap();
    let mut conflicting_revision = second.clone();
    conflicting_revision.sha256 = digest(9);
    assert!(store
        .append_output_revision(created.id, &conflicting_revision)
        .await
        .is_err());

    // The rejected retries left exactly the original history behind.
    let revisions = store.list_output_revisions(created.id).await.unwrap();
    assert_eq!(revisions.len(), 2);
}

#[tokio::test]
async fn outputs_are_exactly_conversation_scoped() {
    let (_dir, store, chat) = store_with_chat().await;
    let other = sample_chat();
    store.create_chat(&other).await.unwrap();
    let mine = store
        .create_output(&create_request(chat.id, "brief.md", 1))
        .await
        .unwrap();
    store
        .create_output(&create_request(other.id, "theirs.md", 2))
        .await
        .unwrap();

    let listed = store.list_outputs(chat.id, 10).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, mine.id);
}

#[tokio::test]
async fn deleting_an_output_hides_it_but_retains_its_revisions() {
    let (_dir, store, chat) = store_with_chat().await;
    let created = store
        .create_output(&create_request(chat.id, "brief.md", 1))
        .await
        .unwrap();

    assert!(store.delete_output(created.id, at(60)).await.unwrap());
    assert!(
        store.delete_output(created.id, at(90)).await.unwrap(),
        "deleting twice is the same durable outcome"
    );
    assert!(store.list_outputs(chat.id, 10).await.unwrap().is_empty());

    let record = store.get_output(created.id).await.unwrap().unwrap();
    assert_eq!(record.deleted_at, Some(at(60)), "the first deletion stands");
    assert_eq!(
        store.list_output_revisions(created.id).await.unwrap().len(),
        1
    );

    assert!(
        !store.delete_output(OutputId::new(), at(60)).await.unwrap(),
        "an unknown output reports no deletion"
    );
}

#[tokio::test]
async fn reverting_republishes_a_prior_revision_without_losing_history() {
    let (_dir, store, chat) = store_with_chat().await;
    let first = store
        .create_output(&create_request(chat.id, "brief.md", 1))
        .await
        .unwrap();
    let second = revision(2, 30);
    let updated = store
        .append_output_revision(first.id, &second)
        .await
        .unwrap();
    assert_eq!(updated.current_revision, second.id);
    assert_eq!(updated.revision_count, 2);

    // Revert steps the current pointer back to the first revision. Nothing is
    // appended or destroyed: the revision count is unchanged and the superseded
    // revision stays addressable.
    let reverted = store
        .set_current_output_revision(first.id, first.current_revision, at(60))
        .await
        .unwrap();
    assert_eq!(reverted.current_revision, first.current_revision);
    assert_eq!(reverted.revision_count, 2, "revert never mints a revision");
    assert_eq!(reverted.updated_at, at(60));
    assert_eq!(
        store.list_output_revisions(first.id).await.unwrap().len(),
        2
    );

    // Revert is reversible: the newer revision can be republished again.
    let forward = store
        .set_current_output_revision(first.id, second.id, at(90))
        .await
        .unwrap();
    assert_eq!(forward.current_revision, second.id);
    assert_eq!(forward.revision_count, 2);

    // A revision that belongs to another output can never become current.
    let other = store
        .create_output(&create_request(chat.id, "other.md", 3))
        .await
        .unwrap();
    assert!(store
        .set_current_output_revision(first.id, other.current_revision, at(120))
        .await
        .is_err());
}

#[tokio::test]
async fn restoring_a_retracted_output_is_the_exact_inverse_of_deleting_it() {
    let (_dir, store, chat) = store_with_chat().await;
    let created = store
        .create_output(&create_request(chat.id, "brief.md", 1))
        .await
        .unwrap();
    store.delete_output(created.id, at(60)).await.unwrap();
    assert!(store.list_outputs(chat.id, 10).await.unwrap().is_empty());

    assert!(store.restore_output(created.id, at(90)).await.unwrap());
    let restored = store.get_output(created.id).await.unwrap().unwrap();
    assert!(restored.deleted_at.is_none(), "the retraction is cleared");
    assert_eq!(
        store.list_outputs(chat.id, 10).await.unwrap().len(),
        1,
        "the output returns to the catalog"
    );
    // Reverting again after restore still works: the history was untouched.
    assert!(store
        .append_output_revision(created.id, &revision(2, 120))
        .await
        .is_ok());

    assert!(
        store.restore_output(created.id, at(150)).await.unwrap(),
        "restoring a live output is the same durable outcome"
    );
    assert!(
        !store.restore_output(OutputId::new(), at(60)).await.unwrap(),
        "an unknown output reports no restore"
    );
}

#[tokio::test]
async fn a_deleted_output_refuses_further_revisions() {
    let (_dir, store, chat) = store_with_chat().await;
    let created = store
        .create_output(&create_request(chat.id, "brief.md", 1))
        .await
        .unwrap();
    store.delete_output(created.id, at(60)).await.unwrap();

    assert!(store
        .append_output_revision(created.id, &revision(2, 90))
        .await
        .is_err());
}

#[tokio::test]
async fn revision_history_is_bounded_without_losing_content() {
    let (_dir, store, chat) = store_with_chat().await;
    let created = store
        .create_output(&create_request(chat.id, "brief.md", 1))
        .await
        .unwrap();
    for ordinal in 2..=MAX_OUTPUT_REVISIONS {
        store
            .append_output_revision(created.id, &revision(2, i64::from(ordinal)))
            .await
            .unwrap();
    }

    // Refusing the write is deliberate: silently dropping the oldest revision
    // would reintroduce the data loss this record exists to prevent.
    let over_limit = store
        .append_output_revision(created.id, &revision(3, 1_000))
        .await;
    assert!(over_limit.is_err());
    assert_eq!(
        store.list_output_revisions(created.id).await.unwrap().len() as u32,
        MAX_OUTPUT_REVISIONS
    );
}

#[tokio::test]
async fn outputs_reject_unusable_names_and_oversized_revisions() {
    let (_dir, store, chat) = store_with_chat().await;

    for filename in ["../escape.md", "report.pdf", "", ".hidden.md"] {
        assert!(
            store
                .create_output(&create_request(chat.id, filename, 1))
                .await
                .is_err(),
            "{filename}"
        );
    }

    let mut oversized = create_request(chat.id, "brief.md", 1);
    oversized.revision.byte_len = MAX_DELIVERABLE_BYTES as u64 + 1;
    assert!(store.create_output(&oversized).await.is_err());
}

#[tokio::test]
async fn an_output_requires_an_existing_conversation() {
    let (_dir, store) = temp_store().await;

    assert!(store
        .create_output(&create_request(ChatId::new(), "brief.md", 1))
        .await
        .is_err());
}

#[tokio::test]
async fn deleting_a_conversation_removes_its_outputs() {
    let (_dir, store, chat) = store_with_chat().await;
    let created = store
        .create_output(&create_request(chat.id, "brief.md", 1))
        .await
        .unwrap();

    store.delete_chat(chat.id).await.unwrap();

    assert!(store.get_output(created.id).await.unwrap().is_none());
    assert!(store
        .get_output_revision(created.current_revision)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn a_revision_records_the_turn_that_produced_it() {
    let (_dir, store, chat) = store_with_chat().await;
    let turn_id = TurnId::new();
    let mut request = create_request(chat.id, "brief.md", 1);
    request.revision.turn_id = Some(turn_id);

    let created: OutputRecord = store.create_output(&request).await.unwrap();

    let stored = store
        .get_output_revision(created.current_revision)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.turn_id, Some(turn_id));
}

#[cfg(feature = "tools")]
fn open_scratch(path: &std::path::Path) -> cap_std::fs::Dir {
    cap_std::fs::Dir::open_ambient_dir(path, cap_std::ambient_authority()).unwrap()
}

#[tokio::test]
async fn a_revision_records_a_producing_run_and_rejects_two_producers() {
    use crate::id::AgentRunId;

    let (_dir, store, chat) = store_with_chat().await;
    let request = create_request(chat.id, "brief.md", 1);
    let created = store.create_output(&request).await.unwrap();

    // A later revision can be attributed to a producing background run.
    let run_id = AgentRunId::new();
    let mut run_revision = revision(2, 10);
    run_revision.producing_run_id = Some(run_id);
    let updated = store
        .append_output_revision(created.id, &run_revision)
        .await
        .unwrap();
    let stored = store
        .get_output_revision(updated.current_revision)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.producing_run_id, Some(run_id));
    assert_eq!(stored.turn_id, None);

    // A revision may not name both a producing turn and a producing run.
    let mut both = revision(3, 20);
    both.turn_id = Some(TurnId::new());
    both.producing_run_id = Some(AgentRunId::new());
    assert!(store
        .append_output_revision(created.id, &both)
        .await
        .is_err());
}

#[cfg(feature = "tools")]
#[tokio::test]
async fn a_binary_workspace_artifact_is_accepted_published_and_attributed_to_its_run() {
    use crate::deliverable::{output_revision_relative_path, RevisionProducer};
    use crate::deliverable_acceptance::{accept_workspace_artifact, WorkspaceArtifactProposal};
    use crate::id::AgentRunId;

    let (_dir, store, chat) = store_with_chat().await;
    let scratch = tempfile::tempdir().unwrap();
    let dir = open_scratch(scratch.path());

    // A binary artifact larger than the 512 KiB text cap, which the text path
    // would reject, and carrying a real binary media type.
    let mut content = b"\x89PNG\r\n\x1a\n".to_vec();
    content.resize(700 * 1024, 7);
    let run_id = AgentRunId::new();
    let output_id = OutputId::new();
    let revision_id = OutputRevisionId::new();

    let record = accept_workspace_artifact(
        &store,
        &dir,
        &WorkspaceArtifactProposal {
            output_id,
            chat_id: chat.id,
            filename: "chart.png".into(),
            media_type: "image/png".into(),
            revision_id,
            producer: RevisionProducer::Run(run_id),
            revise: false,
            content: content.clone(),
            created_at: at(0),
        },
    )
    .await
    .unwrap();

    assert_eq!(record.media_type, "image/png");
    assert_eq!(record.revision_count, 1);

    // The bytes landed at the exact write-once revision path the desktop export
    // reads, unchanged and content-addressed.
    let published = scratch
        .path()
        .join(output_revision_relative_path(output_id, revision_id));
    assert_eq!(std::fs::read(&published).unwrap(), content);

    // The revision records the producing run, not a turn.
    let revision = store
        .get_output_revision(revision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(revision.producing_run_id, Some(run_id));
    assert_eq!(revision.turn_id, None);
    assert_eq!(revision.byte_len, content.len() as u64);
    use sha2::Digest as _;
    assert_eq!(
        revision.sha256,
        <[u8; 32]>::from(sha2::Sha256::digest(&content))
    );
}

#[cfg(feature = "tools")]
#[tokio::test]
async fn acceptance_enforces_the_binary_cap_and_rejects_empty_artifacts() {
    use crate::deliverable::{RevisionProducer, MAX_BINARY_DELIVERABLE_BYTES};
    use crate::deliverable_acceptance::{accept_workspace_artifact, WorkspaceArtifactProposal};
    use crate::id::AgentRunId;

    let (_dir, store, chat) = store_with_chat().await;
    let scratch = tempfile::tempdir().unwrap();
    let dir = open_scratch(scratch.path());

    let proposal = |content: Vec<u8>| WorkspaceArtifactProposal {
        output_id: OutputId::new(),
        chat_id: chat.id,
        filename: "blob.bin".into(),
        media_type: "application/octet-stream".into(),
        revision_id: OutputRevisionId::new(),
        producer: RevisionProducer::Run(AgentRunId::new()),
        revise: false,
        content,
        created_at: at(0),
    };

    assert!(
        accept_workspace_artifact(&store, &dir, &proposal(Vec::new()))
            .await
            .is_err()
    );
    assert!(accept_workspace_artifact(
        &store,
        &dir,
        &proposal(vec![0u8; MAX_BINARY_DELIVERABLE_BYTES + 1])
    )
    .await
    .is_err());
    // Exactly at the cap is accepted.
    assert!(accept_workspace_artifact(
        &store,
        &dir,
        &proposal(vec![0u8; MAX_BINARY_DELIVERABLE_BYTES])
    )
    .await
    .is_ok());
}

#[cfg(feature = "tools")]
mod output_scan {
    use super::*;
    use crate::deliverable::{output_revision_relative_path, RevisionProducer};
    use crate::id::CallId;
    use crate::output_scan::{sync_output_directory, OutputSyncStatus, EXEC_OUTPUT_DIRECTORY};

    async fn sync(
        store: &DbStore,
        scratch: &cap_std::fs::Dir,
        chat_id: ChatId,
        call_id: CallId,
        second: i64,
    ) -> crate::output_scan::OutputDirectorySync {
        sync_output_directory(
            store,
            scratch,
            scratch,
            chat_id,
            call_id,
            RevisionProducer::Turn(TurnId::new()),
            at(second),
        )
        .await
        .unwrap()
    }

    fn write_output_file(scratch: &std::path::Path, name: &str, content: &[u8]) {
        let path = scratch.join(EXEC_OUTPUT_DIRECTORY).join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[tokio::test]
    async fn the_scan_creates_updates_and_matches_by_filename() {
        let (_dir, store, chat) = store_with_chat().await;
        let scratch = tempfile::tempdir().unwrap();
        let dir = open_scratch(scratch.path());

        // First call: a text report and an oversized-for-text binary chart.
        write_output_file(scratch.path(), "report.md", b"# Draft");
        let mut pixels = b"\x89PNG\r\n\x1a\n".to_vec();
        pixels.resize(700 * 1024, 7);
        write_output_file(scratch.path(), "chart.png", &pixels);

        let first = sync(&store, &dir, chat.id, CallId::new(), 0).await;
        assert!(first.notes.is_empty(), "{:?}", first.notes);
        assert_eq!(first.entries.len(), 2);
        assert!(first
            .entries
            .iter()
            .all(|entry| entry.status == OutputSyncStatus::Created && entry.ordinal == 1));
        let report = first
            .entries
            .iter()
            .find(|entry| entry.filename == "report.md")
            .unwrap();
        let chart = first
            .entries
            .iter()
            .find(|entry| entry.filename == "chart.png")
            .unwrap();
        let chart_record = store.get_output(chart.output_id).await.unwrap().unwrap();
        assert_eq!(chart_record.media_type, "image/png");
        // The bytes landed at the write-once revision path the desktop reads.
        let published = scratch.path().join(output_revision_relative_path(
            chart_record.id,
            chart_record.current_revision,
        ));
        assert_eq!(std::fs::read(&published).unwrap(), pixels);
        // The revision is attributed to the producing turn.
        let revision = store
            .get_output_revision(chart_record.current_revision)
            .await
            .unwrap()
            .unwrap();
        assert!(revision.turn_id.is_some());

        // Second call, nothing rewritten: both files match their current
        // revisions and nothing is minted.
        let second = sync(&store, &dir, chat.id, CallId::new(), 30).await;
        assert!(second
            .entries
            .iter()
            .all(|entry| entry.status == OutputSyncStatus::Unchanged));
        assert_eq!(
            store
                .get_output(report.output_id)
                .await
                .unwrap()
                .unwrap()
                .revision_count,
            1
        );

        // Third call: same filename, new bytes — a revision on the same output,
        // never a second record.
        write_output_file(scratch.path(), "report.md", b"# Final");
        let third = sync(&store, &dir, chat.id, CallId::new(), 60).await;
        let updated = third
            .entries
            .iter()
            .find(|entry| entry.filename == "report.md")
            .unwrap();
        assert_eq!(updated.status, OutputSyncStatus::Updated);
        assert_eq!(updated.output_id, report.output_id);
        assert_eq!(updated.ordinal, 2);

        // Deleting a file from output/ never deletes the durable record, and a
        // renamed file is a new output rather than a revision.
        std::fs::remove_file(scratch.path().join(EXEC_OUTPUT_DIRECTORY).join("report.md")).unwrap();
        write_output_file(scratch.path(), "summary.md", b"# Summary");
        let fourth = sync(&store, &dir, chat.id, CallId::new(), 90).await;
        let summary = fourth
            .entries
            .iter()
            .find(|entry| entry.filename == "summary.md")
            .unwrap();
        assert_eq!(summary.status, OutputSyncStatus::Created);
        assert_ne!(summary.output_id, report.output_id);
        let report_record = store.get_output(report.output_id).await.unwrap().unwrap();
        assert!(report_record.deleted_at.is_none());
        assert_eq!(report_record.revision_count, 2);
        assert_eq!(store.list_outputs(chat.id, 10).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn an_exact_scan_retry_is_idempotent() {
        let (_dir, store, chat) = store_with_chat().await;
        let scratch = tempfile::tempdir().unwrap();
        let dir = open_scratch(scratch.path());
        write_output_file(scratch.path(), "report.md", b"# Draft");

        let call_id = CallId::new();
        let first = sync(&store, &dir, chat.id, call_id, 0).await;
        let retried = sync(&store, &dir, chat.id, call_id, 0).await;

        assert_eq!(first.entries[0].output_id, retried.entries[0].output_id);
        assert_eq!(retried.entries[0].status, OutputSyncStatus::Unchanged);
        assert_eq!(store.list_outputs(chat.id, 10).await.unwrap().len(), 1);
        assert_eq!(
            store
                .list_output_revisions(first.entries[0].output_id)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn concurrent_scans_of_one_filename_land_on_a_single_output() {
        let (_dir, store, chat) = store_with_chat().await;
        let conversation = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(store);
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let turn_id = TurnId::new();
        let run_id = crate::AgentRunId::new();

        // A foreground turn and a background run, each writing `report.md` in
        // its own workspace and publishing into the same conversation.
        let mut tasks = Vec::new();
        for (index, (bytes, producer)) in [
            (b"# Foreground".as_slice(), RevisionProducer::Turn(turn_id)),
            (b"# Background".as_slice(), RevisionProducer::Run(run_id)),
        ]
        .into_iter()
        .enumerate()
        {
            let workspace = tempfile::tempdir().unwrap();
            write_output_file(workspace.path(), "report.md", bytes);
            let workspace_dir = open_scratch(workspace.path());
            let publication = open_scratch(conversation.path());
            let store = store.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                let _keep = workspace;
                barrier.wait().await;
                sync_output_directory(
                    store.as_ref(),
                    &workspace_dir,
                    &publication,
                    chat.id,
                    CallId::new(),
                    producer,
                    at(index as i64),
                )
                .await
                .unwrap()
            }));
        }
        let syncs: Vec<_> = futures::future::join_all(tasks)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();

        // Whoever won, the conversation holds exactly one `report.md`, both
        // scans addressed it, and the loser's bytes became its second revision
        // rather than a second live record under the same name.
        let live = store
            .find_outputs_by_filename(chat.id, "report.md")
            .await
            .unwrap();
        assert_eq!(live.len(), 1, "concurrent scans forked the filename");
        let output = &live[0];
        assert_eq!(output.revision_count, 2);
        for sync in &syncs {
            assert!(sync.notes.is_empty(), "{:?}", sync.notes);
            assert_eq!(sync.entries.len(), 1);
            assert_eq!(sync.entries[0].output_id, output.id);
        }
        let mut statuses: Vec<_> = syncs.iter().map(|sync| sync.entries[0].status).collect();
        statuses.sort_by_key(|status| format!("{status:?}"));
        assert_eq!(
            statuses,
            vec![OutputSyncStatus::Created, OutputSyncStatus::Updated]
        );

        // Cross-producer merging preserves who wrote each version, and both
        // revisions' bytes remain readable where the record says they are.
        let revisions = store.list_output_revisions(output.id).await.unwrap();
        assert_eq!(revisions.len(), 2);
        assert!(revisions.iter().any(|revision| {
            revision.turn_id == Some(turn_id) && revision.producing_run_id.is_none()
        }));
        assert!(revisions.iter().any(|revision| {
            revision.turn_id.is_none() && revision.producing_run_id == Some(run_id)
        }));
        for revision in revisions {
            let relative = output_revision_relative_path(output.id, revision.id);
            assert_eq!(
                std::fs::read(conversation.path().join(&relative))
                    .unwrap()
                    .len() as u64,
                revision.byte_len
            );
        }
    }

    #[tokio::test]
    async fn unacceptable_files_become_notes_without_failing_the_scan() {
        let (_dir, store, chat) = store_with_chat().await;
        let scratch = tempfile::tempdir().unwrap();
        let dir = open_scratch(scratch.path());

        write_output_file(scratch.path(), "good.md", b"# Fine");
        write_output_file(scratch.path(), "empty.md", b"");
        write_output_file(
            scratch.path(),
            "huge.csv",
            &vec![b'x'; MAX_DELIVERABLE_BYTES + 1],
        );
        write_output_file(scratch.path(), "binary.md", b"text\xff\xfe");
        write_output_file(scratch.path(), ".hidden.md", b"# Plumbing");
        write_output_file(scratch.path(), "nested/index.html", b"<html></html>");
        write_output_file(scratch.path(), "fake.pdf", b"<html>not a pdf</html>");
        write_output_file(scratch.path(), "real.pdf", b"%PDF-1.7 minimal");

        let scan = sync(&store, &dir, chat.id, CallId::new(), 0).await;

        let published: Vec<&str> = scan
            .entries
            .iter()
            .map(|entry| entry.filename.as_str())
            .collect();
        assert_eq!(published, ["good.md", "real.pdf"]);
        assert!(scan
            .notes
            .iter()
            .any(|note| note.contains("fake.pdf") && note.contains("not a PDF")));
        assert!(scan.notes.iter().any(|note| note.contains("empty.md")));
        assert!(scan
            .notes
            .iter()
            .any(|note| note.contains("huge.csv") && note.contains("binary format")));
        assert!(scan
            .notes
            .iter()
            .any(|note| note.contains("binary.md") && note.contains("UTF-8")));
        assert!(!scan.notes.iter().any(|note| note.contains(".hidden.md")));
        assert!(scan
            .notes
            .iter()
            .any(|note| note.contains("nested") && note.contains("top level")));
    }

    #[tokio::test]
    async fn the_revision_cap_is_a_note_and_other_files_still_land() {
        let (_dir, store, chat) = store_with_chat().await;
        let scratch = tempfile::tempdir().unwrap();
        let dir = open_scratch(scratch.path());

        let capped = store
            .create_output(&create_request(chat.id, "capped.md", 1))
            .await
            .unwrap();
        for ordinal in 2..=MAX_OUTPUT_REVISIONS {
            store
                .append_output_revision(capped.id, &revision(2, i64::from(ordinal)))
                .await
                .unwrap();
        }

        write_output_file(scratch.path(), "capped.md", b"# One too many");
        write_output_file(scratch.path(), "fresh.md", b"# Lands anyway");
        let scan = sync(&store, &dir, chat.id, CallId::new(), 500).await;

        assert!(scan
            .notes
            .iter()
            .any(|note| note.contains("capped.md") && note.contains("new filename")));
        assert_eq!(scan.entries.len(), 1);
        assert_eq!(scan.entries[0].filename, "fresh.md");
        assert_eq!(scan.entries[0].status, OutputSyncStatus::Created);
    }

    /// The write-once publication boundary must not follow a symlinked
    /// revision directory out of the conversation's private scratch. (Ported
    /// from the removed create_deliverable tool tests; sync_output_directory
    /// is now the production writer on this path.)
    #[cfg(unix)]
    #[tokio::test]
    async fn publication_refuses_a_symlinked_revision_directory() {
        use crate::deliverable::OUTPUTS_DIRECTORY;

        let (_dir, store, chat) = store_with_chat().await;
        let outside = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), scratch.path().join(OUTPUTS_DIRECTORY)).unwrap();
        write_output_file(scratch.path(), "brief.md", b"must stay private");
        let dir = open_scratch(scratch.path());

        let scan = sync(&store, &dir, chat.id, CallId::new(), 0).await;

        assert!(scan.entries.is_empty());
        assert!(scan.notes[0].contains("could not be published"));
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
        assert!(store.list_outputs(chat.id, 10).await.unwrap().is_empty());
    }

    /// A symlinked `output/` planted by local exec must not hand arbitrary host
    /// files to the catalog.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_output_directory_is_refused_rather_than_followed() {
        let (_dir, store, chat) = store_with_chat().await;
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("private.md"), b"# Private").unwrap();
        let scratch = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), scratch.path().join(EXEC_OUTPUT_DIRECTORY))
            .unwrap();
        let dir = open_scratch(scratch.path());

        let scan = sync(&store, &dir, chat.id, CallId::new(), 0).await;

        assert!(scan.entries.is_empty());
        assert!(scan.notes[0].contains("not a private workspace directory"));
        assert!(store.list_outputs(chat.id, 10).await.unwrap().is_empty());
    }

    /// A background run writes in its own workspace but publishes into its
    /// parent conversation. The bytes have to land under the conversation's
    /// scratch regardless, because that is the only place a reader looks for a
    /// revision — publishing them beside the file that produced them would
    /// leave the catalog listing an output nothing can open.
    #[tokio::test]
    async fn publishing_from_another_workspace_writes_bytes_under_the_conversation() {
        let (_dir, store, chat) = store_with_chat().await;
        let workspace = tempfile::tempdir().unwrap();
        let conversation = tempfile::tempdir().unwrap();
        write_output_file(workspace.path(), "report.md", b"# Report");

        let scan = sync_output_directory(
            &store,
            &open_scratch(workspace.path()),
            &open_scratch(conversation.path()),
            chat.id,
            CallId::new(),
            RevisionProducer::Run(crate::AgentRunId::new()),
            at(0),
        )
        .await
        .unwrap();

        let output = store
            .get_output(scan.entries[0].output_id)
            .await
            .unwrap()
            .unwrap();
        let relative = output_revision_relative_path(output.id, output.current_revision);
        assert_eq!(
            std::fs::read(conversation.path().join(&relative)).unwrap(),
            b"# Report"
        );
        assert!(!workspace.path().join(&relative).exists());
    }

    /// A background agent that builds a deck often leaves the generator script
    /// next to the PPTX under output/. The scan must publish the deliverable
    /// and refuse the script with a note — otherwise junk lands in the catalog
    /// beside the real file. Foreground turns are out of scope for this skip.
    #[tokio::test]
    async fn a_sandbox_run_does_not_publish_helper_scripts_beside_deliverables() {
        let (_dir, store, chat) = store_with_chat().await;
        let scratch = tempfile::tempdir().unwrap();
        let dir = open_scratch(scratch.path());

        // Minimal well-formed empty ZIP = valid PPTX signature for the scan.
        write_output_file(
            scratch.path(),
            "deck.pptx",
            b"PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
        );
        write_output_file(scratch.path(), "build_deck.py", b"print('hi')\n");
        write_output_file(scratch.path(), "helper.sh", b"#!/bin/sh\n");

        let scan = sync_output_directory(
            &store,
            &dir,
            &dir,
            chat.id,
            CallId::new(),
            RevisionProducer::Run(crate::AgentRunId::new()),
            at(0),
        )
        .await
        .unwrap();

        let published: Vec<&str> = scan
            .entries
            .iter()
            .map(|entry| entry.filename.as_str())
            .collect();
        assert_eq!(published, ["deck.pptx"]);
        assert!(scan
            .notes
            .iter()
            .any(|note| { note.contains("build_deck.py") && note.contains("workspace root") }));
        assert!(scan
            .notes
            .iter()
            .any(|note| note.contains("helper.sh") && note.contains("workspace root")));
        assert_eq!(store.list_outputs(chat.id, 10).await.unwrap().len(), 1);
    }
}

#[cfg(feature = "tools")]
mod restore {
    use super::*;
    use crate::deliverable::{output_revision_relative_path, RevisionProducer};
    use crate::deliverable_acceptance::{restore_output_to_revision, save_user_output_revision};
    use crate::id::CallId;
    use crate::output_scan::sync_output_directory;

    /// Build an output with two real published revisions by running the
    /// production scan twice, so restore reads bytes that actually exist.
    async fn output_with_history(
        store: &DbStore,
        scratch: &std::path::Path,
        chat_id: ChatId,
    ) -> OutputRecord {
        let dir = open_scratch(scratch);
        for (second, content) in [(0, "# Draft"), (30, "# Final")] {
            let directory = scratch.join(crate::output_scan::EXEC_OUTPUT_DIRECTORY);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("report.md"), content).unwrap();
            sync_output_directory(
                store,
                &dir,
                &dir,
                chat_id,
                CallId::new(),
                RevisionProducer::Turn(TurnId::new()),
                at(second),
            )
            .await
            .unwrap();
        }
        let outputs = store.list_outputs(chat_id, 10).await.unwrap();
        assert_eq!(outputs[0].revision_count, 2);
        outputs[0].clone()
    }

    #[tokio::test]
    async fn restoring_appends_a_user_revision_with_the_target_content() {
        let (_dir, store, chat) = store_with_chat().await;
        let scratch = tempfile::tempdir().unwrap();
        let output = output_with_history(&store, scratch.path(), chat.id).await;
        let dir = open_scratch(scratch.path());
        let revisions = store.list_output_revisions(output.id).await.unwrap();
        let first = revisions.iter().find(|r| r.ordinal == 1).unwrap().clone();

        let restored =
            restore_output_to_revision(&store, &dir, chat.id, output.id, first.id, at(60))
                .await
                .unwrap();

        // Google-Docs style: v3 appended with v1's content, nothing rewound.
        assert_eq!(restored.revision_count, 3);
        let head = store
            .get_output_revision(restored.current_revision)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(head.ordinal, 3);
        assert_eq!(head.sha256, first.sha256);
        assert_eq!(head.byte_len, first.byte_len);
        // Both-absent producer durably marks the user action.
        assert_eq!(head.turn_id, None);
        assert_eq!(head.producing_run_id, None);
        // The restored bytes are published at the new revision's own path.
        let published = scratch
            .path()
            .join(output_revision_relative_path(output.id, head.id));
        assert_eq!(std::fs::read(&published).unwrap(), b"# Draft");
        assert_eq!(
            store.list_output_revisions(output.id).await.unwrap().len(),
            3
        );

        // An ambiguous retry of the same restore observes the target content
        // already at the head and appends nothing.
        let retried =
            restore_output_to_revision(&store, &dir, chat.id, output.id, first.id, at(60))
                .await
                .unwrap();
        assert_eq!(retried.revision_count, 3);
        assert_eq!(retried.current_revision, restored.current_revision);

        // The restore leaves the model a durable host note — a System message
        // the next turn's transcript picks up — and the no-op retry does not
        // repeat it.
        let notes: Vec<_> = store
            .list_messages(chat.id)
            .await
            .unwrap()
            .into_iter()
            .filter(|message| message.role == crate::model::Role::System)
            .collect();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].content.contains("restored output 'report.md'"));
        assert!(notes[0].content.contains("version 1"));
        assert!(notes[0].content.contains("v3"));
    }

    #[tokio::test]
    async fn restoring_the_current_revision_is_a_no_op() {
        let (_dir, store, chat) = store_with_chat().await;
        let scratch = tempfile::tempdir().unwrap();
        let output = output_with_history(&store, scratch.path(), chat.id).await;
        let dir = open_scratch(scratch.path());

        let unchanged = restore_output_to_revision(
            &store,
            &dir,
            chat.id,
            output.id,
            output.current_revision,
            at(90),
        )
        .await
        .unwrap();

        assert_eq!(unchanged.revision_count, 2);
        assert_eq!(unchanged.current_revision, output.current_revision);
    }

    #[tokio::test]
    async fn restore_refuses_deleted_outputs_and_foreign_revisions() {
        let (_dir, store, chat) = store_with_chat().await;
        let scratch = tempfile::tempdir().unwrap();
        let output = output_with_history(&store, scratch.path(), chat.id).await;
        let dir = open_scratch(scratch.path());
        let first = store
            .list_output_revisions(output.id)
            .await
            .unwrap()
            .pop()
            .unwrap();

        // A revision of a different output is refused.
        let other = store
            .create_output(&create_request(chat.id, "other.md", 9))
            .await
            .unwrap();
        assert!(restore_output_to_revision(
            &store,
            &dir,
            chat.id,
            output.id,
            other.current_revision,
            at(90)
        )
        .await
        .is_err());

        // A deleted output is refused.
        store.delete_output(output.id, at(120)).await.unwrap();
        assert!(
            restore_output_to_revision(&store, &dir, chat.id, output.id, first.id, at(150))
                .await
                .is_err()
        );
    }

    /// The whole safety argument for editing in place: a save is conditional on
    /// the revision it started from, and losing that condition costs nothing but
    /// the save. The bytes of the revision the editor was opened on — and of the
    /// one that overtook it — are the same before and after the rejected save.
    #[tokio::test]
    async fn a_stale_edit_is_refused_and_leaves_every_earlier_revision_intact() {
        let (_dir, store, chat) = store_with_chat().await;
        let scratch = tempfile::tempdir().unwrap();
        let output = output_with_history(&store, scratch.path(), chat.id).await;
        let dir = open_scratch(scratch.path());
        // The editor opens on v2 ("# Final").
        let opened_on = output.current_revision;

        // While it is open, the agent publishes v3.
        let directory = scratch
            .path()
            .join(crate::output_scan::EXEC_OUTPUT_DIRECTORY);
        std::fs::write(directory.join("report.md"), "# Agent revision").unwrap();
        sync_output_directory(
            &store,
            &dir,
            &dir,
            chat.id,
            CallId::new(),
            RevisionProducer::Turn(TurnId::new()),
            at(60),
        )
        .await
        .unwrap();
        let agent_head = store.get_output(output.id).await.unwrap().unwrap();
        assert_eq!(agent_head.revision_count, 3);

        let before: Vec<_> = store
            .list_output_revisions(output.id)
            .await
            .unwrap()
            .into_iter()
            .map(|revision| {
                let path = scratch
                    .path()
                    .join(output_revision_relative_path(output.id, revision.id));
                (revision, std::fs::read(path).unwrap())
            })
            .collect();

        let refused = save_user_output_revision(
            &store,
            &dir,
            chat.id,
            output.id,
            opened_on,
            "# Final, corrected",
            at(90),
        )
        .await
        .expect_err("an edit of superseded content must not publish");
        match refused {
            crate::error::AgentError::OutputRevisionConflict {
                output_id,
                current_revision,
            } => {
                // The rejection names what to reconcile against.
                assert_eq!(output_id, output.id);
                assert_eq!(current_revision, agent_head.current_revision);
            }
            other => panic!("expected a revision conflict, got {other:?}"),
        }

        // Nothing moved, and no revision's stored bytes were touched.
        let unchanged = store.get_output(output.id).await.unwrap().unwrap();
        assert_eq!(unchanged.revision_count, 3);
        assert_eq!(unchanged.current_revision, agent_head.current_revision);
        let after: Vec<_> = store
            .list_output_revisions(output.id)
            .await
            .unwrap()
            .into_iter()
            .map(|revision| {
                let path = scratch
                    .path()
                    .join(output_revision_relative_path(output.id, revision.id));
                (revision, std::fs::read(path).unwrap())
            })
            .collect();
        assert_eq!(before, after);

        // Reconciling against the revision that won publishes the edit as a new
        // user-authored head, and still leaves the earlier bytes alone.
        let saved = save_user_output_revision(
            &store,
            &dir,
            chat.id,
            output.id,
            agent_head.current_revision,
            "# Final, corrected",
            at(120),
        )
        .await
        .unwrap();
        assert_eq!(saved.revision_count, 4);
        let head = store
            .get_output_revision(saved.current_revision)
            .await
            .unwrap()
            .unwrap();
        // Producer absent on both sides: the user wrote this, not a turn or run.
        assert_eq!(head.turn_id, None);
        assert_eq!(head.producing_run_id, None);
        assert_eq!(
            std::fs::read(
                scratch
                    .path()
                    .join(output_revision_relative_path(output.id, head.id))
            )
            .unwrap(),
            b"# Final, corrected"
        );
        for (revision, bytes) in &before {
            assert_eq!(
                &std::fs::read(
                    scratch
                        .path()
                        .join(output_revision_relative_path(output.id, revision.id))
                )
                .unwrap(),
                bytes,
                "revision {} was rewritten",
                revision.ordinal
            );
        }

        // An ambiguous save retried after it committed appends nothing.
        let retried = save_user_output_revision(
            &store,
            &dir,
            chat.id,
            output.id,
            agent_head.current_revision,
            "# Final, corrected",
            at(150),
        )
        .await
        .unwrap();
        assert_eq!(retried.revision_count, 4);
        assert_eq!(retried.current_revision, saved.current_revision);
    }
}
