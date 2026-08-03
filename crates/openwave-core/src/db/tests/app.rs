use super::*;
use crate::id::{AgentRunId, AppId, AppRevisionId, ConnectedAppId};
use crate::local_app::{
    AppBinding, AppManifest, AppToolsBinding, CreateApp, NewAppRevision, MAX_APP_BUNDLE_BYTES,
    MAX_APP_REVISIONS,
};

fn at(second: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(1_710_000_000 + second, 0).unwrap()
}

fn digest(seed: u8) -> [u8; 32] {
    [seed; 32]
}

fn manifest(name: &str) -> AppManifest {
    AppManifest {
        name: name.to_owned(),
        bindings: vec![AppBinding::Tools(AppToolsBinding {
            app: ConnectedAppId(uuid::Uuid::from_u128(1)),
            tools: vec!["mcp__sentry__list_issues".into()],
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
    let orphan_chat = ChatId::new();
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

    // A pinned name that is not shaped like a mounted tool can never match
    // one, so the store refuses it at the door.
    let mut bare_tool = create_request(1);
    bare_tool.revision.manifest.bindings[0] = AppBinding::Tools(AppToolsBinding {
        app: ConnectedAppId(uuid::Uuid::from_u128(1)),
        tools: vec!["list_issues".into()],
    });
    assert!(store.create_app(&bare_tool).await.is_err());

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
            AppBinding::Tools(AppToolsBinding {
                app: ConnectedAppId::new(),
                tools: vec![format!("mcp__server_{index:04}__tool_padding_padding")],
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
