use super::*;
use crate::deliverable::{
    CreateOutput, NewOutputRevision, OutputRecord, MAX_DELIVERABLE_BYTES, MAX_OUTPUT_REVISIONS,
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
        created_at: at(second),
    }
}

fn create_request(chat_id: ChatId, filename: &str, seed: u8) -> CreateOutput {
    CreateOutput {
        id: OutputId::new(),
        chat_id,
        filename: filename.to_owned(),
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
