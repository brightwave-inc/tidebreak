use super::*;
use crate::id::{AgentRunId, AppId, AppRevisionId, ConnectedAppId};
use crate::local_app::{
    AppBinding, AppGatewayDraft, AppGrant, AppManifest, AppOperationsBinding, CreateApp,
    NewAppRevision, MAX_APP_BUNDLE_BYTES, MAX_APP_REVISIONS,
};
use crate::OwnerId;

fn at(second: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(1_710_000_000 + second, 0).unwrap()
}

fn digest(seed: u8) -> [u8; 32] {
    [seed; 32]
}

fn manifest(name: &str) -> AppManifest {
    AppManifest {
        name: name.to_owned(),
        bindings: vec![AppBinding::Operations(AppOperationsBinding {
            app: ConnectedAppId(uuid::Uuid::from_u128(1)),
            operation_ids: vec!["listIssues".into()],
        })],
    }
}

fn revision(seed: u8, second: i64) -> NewAppRevision {
    NewAppRevision {
        id: AppRevisionId::new(),
        manifest: manifest("Sentry triage"),
        byte_len: u64::from(seed) + 1,
        sha256: digest(seed),
        turn_id: None,
        producing_run_id: None,
        chat_id: None,
        created_at: at(second),
    }
}

fn create_request(seed: u8) -> CreateApp {
    CreateApp {
        id: AppId::new(),
        revision: revision(seed, 0),
    }
}

// Apps are profile-scoped: every test runs against a store with no chat at
// all, which is itself part of the contract under test.

#[tokio::test]
async fn an_exact_retry_never_creates_a_second_app_or_revision() {
    let (_dir, store) = temp_store().await;
    let request = create_request(1);
    let created = store.create_app(&request).await.unwrap();
    assert_eq!(created.id, request.id);
    assert_eq!(
        created.name, "Sentry triage",
        "name comes from the manifest"
    );
    assert_eq!(created.current_revision, request.revision.id);
    assert_eq!(created.revision_count, 1);

    let retried = store.create_app(&request).await.unwrap();
    assert_eq!(retried, created);

    let second = revision(2, 30);
    let appended = store
        .append_app_revision(created.id, &second)
        .await
        .unwrap();
    let appended_again = store
        .append_app_revision(created.id, &second)
        .await
        .unwrap();
    assert_eq!(appended_again, appended);
    assert_eq!(appended_again.revision_count, 2);
    assert_eq!(store.list_apps(10).await.unwrap().len(), 1);

    // The replaced revision stays addressable, and its stored manifest
    // round-trips intact.
    let replaced = store
        .get_app_revision(created.current_revision)
        .await
        .unwrap()
        .expect("the replaced revision is retained");
    assert_eq!(replaced.ordinal, 1);
    assert_eq!(replaced.manifest, request.revision.manifest);
    let revisions = store.list_app_revisions(created.id).await.unwrap();
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
async fn reusing_an_identity_for_different_content_is_rejected() {
    let (_dir, store) = temp_store().await;
    let request = create_request(1);
    let created = store.create_app(&request).await.unwrap();

    let mut conflicting = request.clone();
    conflicting.revision.manifest.name = "Other app".into();
    assert!(store.create_app(&conflicting).await.is_err());

    let second = revision(2, 30);
    store
        .append_app_revision(created.id, &second)
        .await
        .unwrap();
    let mut conflicting_revision = second.clone();
    conflicting_revision.sha256 = digest(9);
    assert!(store
        .append_app_revision(created.id, &conflicting_revision)
        .await
        .is_err());

    // The rejected retries left exactly the original history behind.
    assert_eq!(store.list_app_revisions(created.id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn revision_history_is_bounded_without_losing_content() {
    let (_dir, store) = temp_store().await;
    let created = store.create_app(&create_request(1)).await.unwrap();
    for ordinal in 2..=MAX_APP_REVISIONS {
        store
            .append_app_revision(created.id, &revision(2, i64::from(ordinal)))
            .await
            .unwrap();
    }

    // Refusing the write is deliberate: silently dropping the oldest revision
    // would lose exactly the history this record exists to keep.
    assert!(store
        .append_app_revision(created.id, &revision(3, 1_000))
        .await
        .is_err());
    assert_eq!(
        store.list_app_revisions(created.id).await.unwrap().len() as u32,
        MAX_APP_REVISIONS
    );
}

#[tokio::test]
async fn a_revision_records_one_producer_and_dangling_conversation_provenance() {
    let (_dir, store) = temp_store().await;

    // The originating conversation is provenance without a foreign key: this
    // chat id references no chat row at all and the write still lands, which
    // is exactly what keeps an app alive after its conversation is deleted.
    let orphan_chat = SessionId::new();
    let turn_id = TurnId::new();
    let mut request = create_request(1);
    request.revision.turn_id = Some(turn_id);
    request.revision.chat_id = Some(orphan_chat);
    let created = store.create_app(&request).await.unwrap();

    let stored = store
        .get_app_revision(created.current_revision)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.turn_id, Some(turn_id));
    assert_eq!(stored.chat_id, Some(orphan_chat));
    assert_eq!(stored.producing_run_id, None);

    // A revision may not name both a producing turn and a producing run.
    let mut both = revision(2, 30);
    both.turn_id = Some(TurnId::new());
    both.producing_run_id = Some(AgentRunId::new());
    assert!(store.append_app_revision(created.id, &both).await.is_err());
}

#[tokio::test]
async fn malformed_manifests_and_out_of_bounds_bundles_are_refused() {
    let (_dir, store) = temp_store().await;

    // A pinned id outside the operationId grammar can never match a declared
    // operation, so the store refuses it at the door.
    let mut bare_operation = create_request(1);
    bare_operation.revision.manifest.bindings[0] = AppBinding::Operations(AppOperationsBinding {
        app: ConnectedAppId(uuid::Uuid::from_u128(1)),
        operation_ids: vec!["not an operation id".into()],
    });
    assert!(store.create_app(&bare_operation).await.is_err());

    let mut empty_bundle = create_request(1);
    empty_bundle.revision.byte_len = 0;
    assert!(store.create_app(&empty_bundle).await.is_err());

    let mut oversized_bundle = create_request(1);
    oversized_bundle.revision.byte_len = MAX_APP_BUNDLE_BYTES as u64 + 1;
    assert!(store.create_app(&oversized_bundle).await.is_err());

    // A manifest over the 64 KiB bound is refused however well-formed it is.
    let mut oversized_manifest = create_request(1);
    oversized_manifest.revision.manifest.bindings = (0..2_000)
        .map(|index| {
            AppBinding::Operations(AppOperationsBinding {
                app: ConnectedAppId::new(),
                operation_ids: vec![format!("operation_{index:04}_padding_padding")],
            })
        })
        .collect();
    assert!(store.create_app(&oversized_manifest).await.is_err());

    assert!(store.list_apps(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn deleting_an_app_hides_it_and_refuses_revisions_until_restored() {
    let (_dir, store) = temp_store().await;
    let created = store.create_app(&create_request(1)).await.unwrap();

    assert!(store.delete_app(created.id, at(60)).await.unwrap());
    assert!(
        store.delete_app(created.id, at(90)).await.unwrap(),
        "deleting twice is the same durable outcome"
    );
    assert!(store.list_apps(10).await.unwrap().is_empty());
    let record = store.get_app(created.id).await.unwrap().unwrap();
    assert_eq!(record.deleted_at, Some(at(60)), "the first deletion stands");
    assert_eq!(
        store.list_app_revisions(created.id).await.unwrap().len(),
        1,
        "revisions are retained through a deletion"
    );
    assert!(store
        .append_app_revision(created.id, &revision(2, 120))
        .await
        .is_err());

    assert!(store.restore_app(created.id, at(150)).await.unwrap());
    assert_eq!(store.list_apps(10).await.unwrap().len(), 1);
    assert!(store
        .append_app_revision(created.id, &revision(2, 180))
        .await
        .is_ok());
    assert!(
        !store.delete_app(AppId::new(), at(60)).await.unwrap(),
        "an unknown app reports no deletion"
    );
    assert!(
        !store.restore_app(AppId::new(), at(60)).await.unwrap(),
        "an unknown app reports no restore"
    );
}

/// The app row is the authority root: every child path resolves through its
/// immutable owner, and another principal sees the same absence as for an
/// unknown id even when it guesses the exact app or revision id.
#[tokio::test]
async fn two_principals_cannot_cross_app_ownership() {
    let (_dir, store) = temp_store().await;
    let alice = OwnerId::new("user:alice").unwrap();
    let bob = OwnerId::new("user:bob").unwrap();
    let request = create_request(1);
    let created = store.create_app_scoped(&alice, &request).await.unwrap();
    assert_eq!(created.owner, alice);

    assert_eq!(store.get_app_scoped(&bob, created.id).await.unwrap(), None);
    assert!(store.list_apps_scoped(&bob, 10).await.unwrap().is_empty());
    assert_eq!(
        store
            .get_app_revision_scoped(&bob, created.current_revision)
            .await
            .unwrap(),
        None
    );
    assert!(store
        .list_app_revisions_scoped(&bob, created.id)
        .await
        .unwrap()
        .is_empty());
    assert!(store
        .append_app_revision_scoped(&bob, created.id, &revision(2, 30))
        .await
        .is_err());
    assert!(!store
        .delete_app_scoped(&bob, created.id, at(60))
        .await
        .unwrap());

    let grant = AppGrant {
        app_id: created.id,
        bindings: Vec::new(),
        created_at: at(30),
    };
    assert!(store.put_app_grant_scoped(&bob, &grant).await.is_err());
    store.put_app_grant_scoped(&alice, &grant).await.unwrap();
    assert_eq!(
        store.get_app_grant_scoped(&bob, created.id).await.unwrap(),
        None
    );
    assert!(!store
        .delete_app_grant_scoped(&bob, created.id)
        .await
        .unwrap());
    assert!(store
        .get_app_grant_scoped(&alice, created.id)
        .await
        .unwrap()
        .is_some());

    let draft = AppGatewayDraft {
        app_id: created.id,
        gateway_base_url: "https://gateway.example/".into(),
        shared_app_id: "shared-1".into(),
        gateway_revision_id: "revision-1".into(),
        synced_revision_id: created.current_revision,
        updated_at: at(45),
    };
    assert!(store
        .put_app_gateway_draft_scoped(&bob, &draft)
        .await
        .is_err());
    store
        .put_app_gateway_draft_scoped(&alice, &draft)
        .await
        .unwrap();
    assert_eq!(
        store
            .get_app_gateway_draft_scoped(&bob, created.id, &draft.gateway_base_url)
            .await
            .unwrap(),
        None
    );

    assert!(store.create_app_scoped(&bob, &request).await.is_err());
    assert_eq!(
        store
            .get_app_scoped(&alice, created.id)
            .await
            .unwrap()
            .unwrap()
            .owner,
        alice,
        "an id retry cannot transfer ownership"
    );
}

/// Tool-facing writes derive ownership from the durable authoring chat and
/// cannot append a second principal's app.
#[tokio::test]
async fn chat_derived_app_ownership_is_stamped_and_enforced_transactionally() {
    let (_dir, store) = temp_store().await;
    let alice = OwnerId::new("user:alice").unwrap();
    let bob = OwnerId::new("user:bob").unwrap();
    let alice_chat = sample_chat();
    let bob_chat = sample_chat();
    store.create_chat_scoped(&alice, &alice_chat).await.unwrap();
    store.create_chat_scoped(&bob, &bob_chat).await.unwrap();

    let created = store
        .create_app_for_chat(alice_chat.id, &create_request(1))
        .await
        .unwrap();
    assert_eq!(created.owner, alice);
    assert!(store
        .append_app_revision_for_chat(bob_chat.id, created.id, &revision(2, 30))
        .await
        .is_err());
    assert!(store
        .append_app_revision_for_chat(alice_chat.id, created.id, &revision(2, 60))
        .await
        .is_ok());
}
