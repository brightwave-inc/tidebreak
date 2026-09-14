use super::*;
use crate::code::inference::{InferenceResolution, InferenceSponsor, PreparedSessionInference};
use crate::code::{
    CodeExternalGrant, CodeGrantId, CodeGrantKind, CodeHandshakeId, ExternalSessionResolution,
};
use crate::db::code::*;

async fn personal(
    store: &crate::db::DbStore,
    owner: &OwnerId,
    identity: &str,
    seed: &str,
) -> CodeExternalGrant {
    let nonce = fake_hash(&format!("{seed}-nonce"));
    let confirm = fake_hash(&format!("{seed}-confirm"));
    insert_connect_handshake(
        store,
        &nonce,
        &confirm,
        "csrf",
        "slack",
        identity,
        "T1",
        "Person",
        "Workspace",
        None,
        chrono::Duration::minutes(15),
        CodeGrantKind::Person,
        None,
    )
    .await
    .unwrap();
    view_connect_handshake_all_owners(store, &nonce, owner)
        .await
        .unwrap()
        .unwrap();
    approve_connect_handshake_all_owners(store, &nonce, "csrf", owner)
        .await
        .unwrap()
        .unwrap();
    complete_connect_handshake_and_mint_grant_all_owners(
        store,
        &nonce,
        &confirm,
        &fake_hash(&format!("{seed}-access")),
        &fake_hash(&format!("{seed}-refresh")),
    )
    .await
    .unwrap()
    .unwrap()
    .0
}

#[tokio::test]
async fn inference_explicit_proof_refuses_mismatch_incomplete_missing_and_revoked_grants() {
    let (_dir, store) = temp_store().await;
    let owner = OwnerId::local();
    let valid = personal(&store, &owner, "U1", "valid").await;
    assert_eq!(
        personal_inference_grant(&store, "T1", "U1", Some(valid.id))
            .await
            .unwrap()
            .unwrap()
            .id,
        valid.id
    );
    for (workspace, identity, id) in [
        ("T2", "U1", valid.id),
        ("T1", "U2", valid.id),
        ("T1", "U1", CodeGrantId::new()),
    ] {
        assert!(
            personal_inference_grant(&store, workspace, identity, Some(id))
                .await
                .is_err()
        );
    }
    let incomplete = mint_external_grant(
        &store,
        &owner,
        MintGrantSubject {
            channel_kind: "slack",
            external_identity: "U2",
            workspace_identity: "T1",
            kind: CodeGrantKind::Person,
        },
        &fake_hash("incomplete-access"),
        &fake_hash("incomplete-refresh"),
    )
    .await
    .unwrap();
    assert!(
        personal_inference_grant(&store, "T1", "U2", Some(incomplete.id))
            .await
            .is_err()
    );
    assert!(personal_inference_grant(&store, "T1", "U2", None)
        .await
        .unwrap()
        .is_none());
    revoke_external_grant(&store, &owner, valid.id, "test revocation")
        .await
        .unwrap();
    assert!(personal_inference_grant(&store, "T1", "U1", Some(valid.id))
        .await
        .is_err());
    assert!(personal_inference_grant(&store, "T1", "U1", None)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn inference_ambiguous_personal_identity_does_not_choose_an_owner() {
    let (_dir, store) = temp_store().await;
    let alice = personal(&store, &OwnerId::new("alice").unwrap(), "U1", "alice").await;
    let bob = personal(&store, &OwnerId::new("bob").unwrap(), "U1", "bob").await;
    assert!(personal_inference_grant(&store, "T1", "U1", None)
        .await
        .unwrap()
        .is_none());
    for grant in [alice, bob] {
        assert_eq!(
            personal_inference_grant(&store, "T1", "U1", Some(grant.id))
                .await
                .unwrap()
                .unwrap()
                .owner,
            grant.owner
        );
    }
}

#[tokio::test]
async fn inference_binding_freezes_one_selection_and_children_share_its_provider_results() {
    let (_dir, store, seed, _) = seeded_session().await;
    let owner = OwnerId::local();
    let grant = personal(&store, &owner, "U1", "frozen").await;
    let mut root = get_session(&store, &owner, seed).await.unwrap().unwrap();
    root.id = SessionId::new();
    let prepared = PreparedSessionInference {
        inherited: None,
        starter: Some("U1".into()),
        supported: true,
        personal: Some((owner.clone(), grant.id, CodeHandshakeId::new())),
        reason: None,
    };
    let context = ExternalSessionChannelContext {
        parent: None,
        inference: Some(&prepared),
        channel_id: Some("C1"),
        instructions: "Keep context",
    };
    let first = resolve_external_session_with_channel_context(
        &store,
        &owner,
        grant.id,
        "slack",
        "T1:C1:root",
        None,
        &root,
        Some(context),
    )
    .await
    .unwrap();
    assert!(matches!(first, ExternalSessionResolution::Created(_)));
    let frozen = session_inference(&store, &owner, root.id)
        .await
        .unwrap()
        .unwrap();
    let mut loser = root.clone();
    loser.id = SessionId::new();
    let fallback = PreparedSessionInference {
        personal: None,
        starter: Some("U2".into()),
        ..prepared.clone()
    };
    let replay = resolve_external_session_with_channel_context(
        &store,
        &owner,
        grant.id,
        "slack",
        "T1:C1:root",
        None,
        &loser,
        Some(ExternalSessionChannelContext {
            inference: Some(&fallback),
            ..context
        }),
    )
    .await
    .unwrap();
    assert!(matches!(replay, ExternalSessionResolution::Existing(_)));
    assert!(get_session(&store, &owner, loser.id)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        session_inference(&store, &owner, root.id)
            .await
            .unwrap()
            .unwrap(),
        frozen
    );

    let inherited = PreparedSessionInference::inherit(frozen.clone());
    let mut child = root.clone();
    child.id = SessionId::new();
    resolve_external_session_with_channel_context(
        &store,
        &owner,
        grant.id,
        "slack",
        "T1:C1:child",
        None,
        &child,
        Some(ExternalSessionChannelContext {
            parent: Some((root.id, "child")),
            inference: Some(&inherited),
            ..context
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        session_inference(&store, &owner, child.id)
            .await
            .unwrap()
            .unwrap(),
        frozen
    );
    assert_eq!(
        get_session(&store, &owner, child.id)
            .await
            .unwrap()
            .unwrap()
            .owner,
        root.owner
    );
    inherit_session_inference(&store, &owner, root.id, child.id)
        .await
        .unwrap();
    assert!(
        session_inference(&store, &OwnerId::new("foreign").unwrap(), child.id)
            .await
            .is_err()
    );
    assert!(inference_resolutions(&store, &owner, child.id)
        .await
        .unwrap()
        .is_empty());
    let resolution = InferenceResolution {
        scope_id: root.id.0,
        provider: "anthropic".into(),
        source: "owned_subscription".into(),
        reason: None,
        subscription_label: Some("Personal".into()),
    };
    record_inference_resolutions(&store, &owner, child.id, &[resolution.clone()])
        .await
        .unwrap();
    record_inference_resolutions(&store, &owner, root.id, &[resolution.clone()])
        .await
        .unwrap();
    assert_eq!(
        inference_resolutions(&store, &owner, root.id)
            .await
            .unwrap(),
        vec![resolution.clone()]
    );
    assert_eq!(
        inference_resolutions(&store, &owner, child.id)
            .await
            .unwrap(),
        vec![resolution.clone()]
    );
    let changed = InferenceResolution {
        source: "execution_default".into(),
        ..resolution.clone()
    };
    assert!(
        record_inference_resolutions(&store, &owner, root.id, &[changed])
            .await
            .is_err()
    );
    let foreign = InferenceResolution {
        scope_id: SessionId::new().0,
        ..resolution
    };
    assert!(
        record_inference_resolutions(&store, &owner, root.id, &[foreign])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn inference_binding_rollback_leaves_no_unbound_authority() {
    let (_dir, store, seed, _) = seeded_session().await;
    let owner = OwnerId::local();
    let grant = personal(&store, &owner, "U1", "rollback").await;
    let mut root = get_session(&store, &owner, seed).await.unwrap().unwrap();
    root.id = SessionId::new();
    let bad = PreparedSessionInference::inherit(crate::code::inference::SessionInference {
        root_session_id: root.id,
        starter_external_identity: None,
        sponsor: Some(InferenceSponsor::ExecutionDefault {
            inference_scope_id: uuid::Uuid::nil(),
        }),
        personal_grant_id: None,
        personal_owner: None,
        reason: None,
    });
    assert!(resolve_external_session_with_channel_context(
        &store,
        &owner,
        grant.id,
        "slack",
        "rollback",
        None,
        &root,
        Some(ExternalSessionChannelContext {
            parent: None,
            inference: Some(&bad),
            channel_id: None,
            instructions: ""
        })
    )
    .await
    .is_err());
    assert!(get_session(&store, &owner, root.id)
        .await
        .unwrap()
        .is_none());
    assert!(get_external_binding(&store, &owner, "slack", "rollback")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn inference_concurrent_binding_admission_keeps_the_winning_starter_and_default() {
    let (_dir, store, seed, _) = seeded_session().await;
    let owner = OwnerId::local();
    let grant = personal(&store, &owner, "U1", "race").await;
    let mut first = get_session(&store, &owner, seed).await.unwrap().unwrap();
    first.id = SessionId::new();
    let mut second = first.clone();
    second.id = SessionId::new();
    let default = PreparedSessionInference {
        inherited: None,
        starter: Some("U1".into()),
        supported: true,
        personal: None,
        reason: Some("personal_connection_unavailable".into()),
    };
    let preferred = PreparedSessionInference {
        starter: Some("U2".into()),
        personal: Some((owner.clone(), grant.id, CodeHandshakeId::new())),
        reason: None,
        ..default.clone()
    };
    let context = |selection| {
        Some(ExternalSessionChannelContext {
            parent: None,
            inference: Some(selection),
            channel_id: None,
            instructions: "",
        })
    };
    let (a, b) = tokio::join!(
        resolve_external_session_with_channel_context(
            &store,
            &owner,
            grant.id,
            "slack",
            "race",
            None,
            &first,
            context(&default)
        ),
        resolve_external_session_with_channel_context(
            &store,
            &owner,
            grant.id,
            "slack",
            "race",
            None,
            &second,
            context(&preferred)
        ),
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(
        [&a, &b]
            .iter()
            .filter(|r| matches!(r, ExternalSessionResolution::Created(_)))
            .count(),
        1
    );
    let binding = get_external_binding(&store, &owner, "slack", "race")
        .await
        .unwrap()
        .unwrap();
    let expected = if binding.session_id == first.id {
        default.freeze(first.id)
    } else {
        preferred.freeze(second.id)
    };
    assert_eq!(
        session_inference(&store, &owner, binding.session_id)
            .await
            .unwrap()
            .unwrap(),
        expected
    );
    let loser = if binding.session_id == first.id {
        second.id
    } else {
        first.id
    };
    assert!(get_session(&store, &owner, loser).await.unwrap().is_none());
}
