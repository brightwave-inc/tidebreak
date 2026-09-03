use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::channel::mpsc::unbounded;
use futures::stream::{self, BoxStream};
use futures::StreamExt;

use super::*;
use crate::context;
use crate::db::DbStore;
use crate::error::Result;
use crate::event::AgentEvent;
use crate::image::{ImageMediaType, ImageRef};
use crate::model::Chat;
use crate::provider::{ChatRequest, ProviderEvent, ProviderId};

/// An in-memory blob store: enough to prove hydration reads the bytes the
/// attachment names, without a filesystem in the way.
#[derive(Default)]
struct MemBlobs {
    bytes: Mutex<HashMap<uuid::Uuid, Vec<u8>>>,
}

#[async_trait]
impl BlobStore for MemBlobs {
    async fn put(&self, id: uuid::Uuid, bytes: Vec<u8>) -> Result<()> {
        self.bytes.lock().unwrap().insert(id, bytes);
        Ok(())
    }

    async fn get(&self, id: uuid::Uuid) -> Result<Option<Vec<u8>>> {
        Ok(self.bytes.lock().unwrap().get(&id).cloned())
    }

    async fn delete(&self, id: uuid::Uuid) -> Result<()> {
        self.bytes.lock().unwrap().remove(&id);
        Ok(())
    }
}

/// Captures the exact request an adapter would have serialized.
struct CaptureProvider {
    seen: Arc<Mutex<Option<ChatRequest>>>,
}

#[async_trait]
impl ModelProvider for CaptureProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("capture")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        *self.seen.lock().unwrap() = Some(req);
        Ok(stream::iter(vec![
            ProviderEvent::TextDelta { text: "ok".into() },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

fn image_ref(blob_id: uuid::Uuid, bytes: &[u8]) -> ImageRef {
    ImageRef {
        blob_id,
        media_type: ImageMediaType::Png,
        width: 64,
        height: 48,
        byte_len: bytes.len() as u64,
    }
}

async fn store_with_chat(name: &str) -> (Arc<DbStore>, Chat, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join(name).display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: SessionId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    (store, chat, dir)
}

/// Run one turn against a capturing provider and return the request it saw.
async fn captured_request(
    store: Arc<DbStore>,
    blobs: Option<Arc<dyn BlobStore>>,
    chat: &Chat,
    input: &str,
) -> ChatRequest {
    let seen: Arc<Mutex<Option<ChatRequest>>> = Arc::new(Mutex::new(None));
    let mut agent = Agent::new(
        Arc::new(CaptureProvider { seen: seen.clone() }),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    );
    if let Some(blobs) = blobs {
        agent = agent.with_blobs(blobs);
    }
    let (tx, rx) = unbounded();
    agent.run_turn(chat, input, &tx).await.unwrap();
    drop(tx);
    let _: Vec<AgentEvent> = rx.collect().await;
    let request = seen.lock().unwrap().take();
    request.expect("provider was called")
}

#[tokio::test]
async fn hydration_gives_the_adapter_bytes_for_every_surviving_image_block() {
    let (store, chat, _dir) = store_with_chat("hydrate.db").await;
    let blobs = Arc::new(MemBlobs::default());
    let pixels = b"\x89PNG\r\n\x1a\n pretend pixels".to_vec();
    let blob_id = uuid::Uuid::from_u128(7);
    blobs.put(blob_id, pixels.clone()).await.unwrap();
    let image = image_ref(blob_id, &pixels);
    assert!(store.publish_chat_image(chat.id, &image).await.unwrap());
    store
        .accept_turn_with_attachments(
            TurnId::new(),
            chat.id,
            "fake",
            "what is in this screenshot?",
            &[image],
            &[],
            &[],
        )
        .await
        .unwrap();

    let request = captured_request(
        store,
        Some(blobs as Arc<dyn BlobStore>),
        &chat,
        "and what about the corner?",
    )
    .await;

    let block_ids: Vec<uuid::Uuid> = request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Image { image } => Some(image.blob_id),
            _ => None,
        })
        .collect();
    assert_eq!(block_ids, vec![blob_id], "the image block must survive");
    let data = request
        .images
        .get(blob_id)
        .expect("an adapter must find bytes for a surviving image block");
    assert_eq!(data.bytes(), pixels.as_slice());
    assert_eq!(data.media_type(), ImageMediaType::Png);
}

#[tokio::test]
async fn an_agent_without_a_byte_source_tells_the_model_instead_of_sending_a_bare_block() {
    let (store, chat, _dir) = store_with_chat("no-blobs.db").await;
    let pixels = b"pixels".to_vec();
    let image = image_ref(uuid::Uuid::from_u128(9), &pixels);
    assert!(store.publish_chat_image(chat.id, &image).await.unwrap());
    store
        .accept_turn_with_attachments(TurnId::new(), chat.id, "fake", "look", &[image], &[], &[])
        .await
        .unwrap();

    let request = captured_request(store, None, &chat, "again").await;
    assert!(request.images.is_empty());
    assert!(
        !request.messages.iter().any(|message| message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Image { .. }))),
        "an unhydratable block must become a stand-in, never reach an adapter"
    );
    assert!(request
        .messages
        .iter()
        .any(|message| message.content.iter().any(
            |block| matches!(block, ContentBlock::Text { text } if text.contains("image omitted"))
        )));
}

#[tokio::test]
async fn hydration_is_bounded_so_a_long_chat_cannot_grow_the_outbound_body() {
    let (store, chat, _dir) = store_with_chat("bounded.db").await;
    let blobs = Arc::new(MemBlobs::default());
    let total = context::MAX_HYDRATED_IMAGES + 3;
    let mut newest = Vec::new();
    for index in 0..total {
        let pixels = format!("pixels-{index}").into_bytes();
        let blob_id = uuid::Uuid::from_u128(100 + index as u128);
        blobs.put(blob_id, pixels.clone()).await.unwrap();
        let turn_id = TurnId::new();
        let image = image_ref(blob_id, &pixels);
        assert!(store.publish_chat_image(chat.id, &image).await.unwrap());
        store
            .accept_turn_with_attachments(
                turn_id,
                chat.id,
                "fake",
                &format!("image {index}"),
                &[image],
                &[],
                &[],
            )
            .await
            .unwrap();
        // A chat holds one live turn at a time, so each history entry has to
        // reach a terminal state before the next is accepted.
        store
            .request_turn_cancellation_and_append_event(turn_id, Utc::now())
            .await
            .unwrap();
        newest.push(blob_id);
    }

    let request =
        captured_request(store, Some(blobs as Arc<dyn BlobStore>), &chat, "summarize").await;
    // The count cap is a ceiling, not an exact fill: eviction advances its
    // boundary in quantized half-cap jumps to spare the provider prompt cache,
    // so the kept count lands in the documented `cap - bucket + 1 ..= cap` band
    // (see `evict_images_beyond`, whose own test owns the band property). At
    // this transcript length that band resolves to one number, and pinning the
    // number rather than re-deriving it is what would catch a boundary that
    // started sliding image by image again.
    //
    // 11 attachments, cap 8, bucket 4: cutoff = ceil((11 - 8) / 4) * 4 = 4, so
    // 7 keep their pixels.
    let expected_hydrated = 7;
    assert_eq!(
        total, 11,
        "the pinned count of {expected_hydrated} assumes 11 attachments; re-derive it"
    );
    assert_eq!(
        request.images.len(),
        expected_hydrated,
        "{total} attachments under a cap of {} evict one bucket",
        context::MAX_HYDRATED_IMAGES,
    );
    // The newest attachments keep their pixels; the oldest become stand-ins.
    for blob_id in &newest[total - expected_hydrated..] {
        assert!(
            request.images.contains(*blob_id),
            "{blob_id} lost its bytes"
        );
    }
    for blob_id in &newest[..total - expected_hydrated] {
        assert!(
            !request.images.contains(*blob_id),
            "{blob_id} was hydrated past the bound"
        );
    }
    // Every block that kept its identity has bytes, so the adapter contract
    // ("a surviving image block is hydrated") still holds after the bound.
    for block in request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
    {
        if let ContentBlock::Image { image } = block {
            assert!(request.images.contains(image.blob_id));
        }
    }
}
