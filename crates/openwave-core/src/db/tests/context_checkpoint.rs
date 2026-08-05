use super::*;
use crate::semantic_checkpoint::{
    ContextCheckpoint, SaveContextCheckpointOutcome, CONTEXT_CHECKPOINT_FORMAT_V1,
    MAX_CONTEXT_CHECKPOINT_BYTES,
};

fn at(second: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(1_710_000_000 + second, 0).unwrap()
}

async fn store_with_messages() -> (tempfile::TempDir, DbStore, Chat, Message, Message) {
    let (dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let first = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        role: Role::User,
        reasoning: Default::default(),
        content: "Choose SQLite for local development.".into(),
        created_at: at(0),
    };
    let second = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "The local database choice is SQLite.".into(),
        created_at: at(1),
    };
    store.append_message(&first).await.unwrap();
    store.append_message(&second).await.unwrap();
    (dir, store, chat, first, second)
}

fn checkpoint(
    chat_id: ChatId,
    source_message_id: MessageId,
    content: &str,
    second: i64,
) -> ContextCheckpoint {
    ContextCheckpoint {
        chat_id,
        source_message_id,
        format_version: CONTEXT_CHECKPOINT_FORMAT_V1,
        content: content.into(),
        usage: crate::provider::Usage {
            input_tokens: 10,
            output_tokens: 4,
            cache_read_input_tokens: 2,
            cache_creation_input_tokens: 1,
        },
        created_at: at(second),
    }
}

#[tokio::test]
async fn checkpoint_is_durable_bounded_and_invisible_to_the_transcript() {
    let (_dir, store, chat, first, second) = store_with_messages().await;
    let initial = checkpoint(chat.id, first.id, "User selected SQLite.", 2);

    assert_eq!(
        store.save_context_checkpoint(&initial).await.unwrap(),
        SaveContextCheckpointOutcome::Saved(initial.clone())
    );
    assert_eq!(
        store.get_context_checkpoint(chat.id).await.unwrap(),
        Some(initial.clone())
    );
    assert_eq!(
        store.list_messages(chat.id).await.unwrap(),
        [first.clone(), second.clone()],
        "a checkpoint is provider context, not a visible message"
    );

    let advanced = checkpoint(
        chat.id,
        second.id,
        "User selected SQLite; the assistant confirmed it.",
        3,
    );
    assert_eq!(
        store.save_context_checkpoint(&advanced).await.unwrap(),
        SaveContextCheckpointOutcome::Saved(advanced.clone())
    );
    assert_eq!(
        store.save_context_checkpoint(&advanced).await.unwrap(),
        SaveContextCheckpointOutcome::Existing(advanced.clone()),
        "an ambiguous response can be retried without a second checkpoint"
    );
    assert_eq!(
        store
            .save_context_checkpoint(&checkpoint(chat.id, first.id, "old", 4))
            .await
            .unwrap(),
        SaveContextCheckpointOutcome::Stale(advanced.clone()),
        "a recovered older worker cannot replace newer context"
    );
    assert_eq!(
        store
            .save_context_checkpoint(&checkpoint(chat.id, second.id, "different", 4))
            .await
            .unwrap(),
        SaveContextCheckpointOutcome::Conflict(advanced),
        "the same boundary has one immutable payload"
    );
}

#[tokio::test]
async fn checkpoint_rejects_cross_chat_boundaries_and_malformed_payloads() {
    let (_dir, store, chat, first, _second) = store_with_messages().await;
    let other = sample_chat();
    store.create_chat(&other).await.unwrap();

    let cross_chat = checkpoint(other.id, first.id, "not allowed", 2);
    assert!(store.save_context_checkpoint(&cross_chat).await.is_err());
    assert!(store
        .get_context_checkpoint(other.id)
        .await
        .unwrap()
        .is_none());

    let empty = checkpoint(chat.id, first.id, "   ", 2);
    assert!(store.save_context_checkpoint(&empty).await.is_err());
    let oversized = checkpoint(
        chat.id,
        first.id,
        &"x".repeat(MAX_CONTEXT_CHECKPOINT_BYTES + 1),
        2,
    );
    assert!(store.save_context_checkpoint(&oversized).await.is_err());
    let unknown_format = ContextCheckpoint {
        format_version: CONTEXT_CHECKPOINT_FORMAT_V1 + 1,
        ..checkpoint(chat.id, first.id, "valid text", 2)
    };
    assert!(store
        .save_context_checkpoint(&unknown_format)
        .await
        .is_err());
    assert!(store
        .get_context_checkpoint(chat.id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn deleting_a_chat_removes_its_checkpoint_before_its_source_message() {
    let (_dir, store, chat, first, _second) = store_with_messages().await;
    store
        .save_context_checkpoint(&checkpoint(chat.id, first.id, "durable context", 2))
        .await
        .unwrap();

    assert_eq!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted
    );
    assert!(store
        .get_context_checkpoint(chat.id)
        .await
        .unwrap()
        .is_none());
}
