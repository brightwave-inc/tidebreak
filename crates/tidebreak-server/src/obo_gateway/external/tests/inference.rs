use super::*;
use crate::code::inference_preferences::*;
use tidebreak_core::{CodeGrantKind, HarnessKind};

#[tokio::test]
async fn dm_preference_uses_its_owner_without_channel_consent_and_honors_override() {
    let (_dir, _db, runtime, _browser, _base, state, owner) = setup().await;
    state.sponsorship_supported.store(true, Ordering::SeqCst);
    let person = connect(&runtime, &owner).await;
    let choose = || {
        prepare(
            &runtime,
            &person,
            None,
            HarnessKind::ClaudeCode,
            ChannelSubscriptionPreference::default(),
        )
    };
    let preferred = choose().await.unwrap();
    assert_eq!(
        preferred.personal.as_ref().map(|p| (&p.0, p.1)),
        Some((&owner, person.id))
    );
    let current = runtime
        .personal_inference_preferences(&owner, person.id)
        .await
        .unwrap();
    assert!(!current.preferences.channel_sponsorship_enabled);
    assert!(current.inference_sponsorship_supported);
    runtime
        .set_personal_inference_preferences(
            &owner,
            person.id,
            &PersonalInferencePreferences {
                dm_subscription_preference: DmSubscriptionPreference::GatewayDefault,
                channel_sponsorship_enabled: false,
                consent_version: None,
            },
            Some(&approval_lease()),
        )
        .await
        .unwrap();
    let default = choose().await.unwrap();
    assert!(default.personal.is_none());
    assert!(default.supported);
    assert_eq!(default.reason.as_deref(), Some("connection_default"));
    assert!(
        preferred.personal.is_some(),
        "preferences cannot replace an admitted choice"
    );
    let mismatched = ConversationStarter {
        external_identity: "U2".into(),
        personal_grant_id: None,
    };
    assert!(prepare(
        &runtime,
        &person,
        Some(&mismatched),
        HarnessKind::ClaudeCode,
        ChannelSubscriptionPreference::default()
    )
    .await
    .is_err());
}

#[tokio::test]
async fn channel_preference_requires_personal_consent_and_keeps_the_service_owner() {
    let (_dir, db, runtime, _browser, _base, state, owner) = setup().await;
    state.sponsorship_supported.store(true, Ordering::SeqCst);
    let person = connect(&runtime, &owner).await;
    let service_owner = OwnerId::new("service:channel").unwrap();
    let channel = tidebreak_core::db::code::mint_external_grant(
        &db,
        &service_owner,
        tidebreak_core::db::code::MintGrantSubject {
            channel_kind: "slack",
            external_identity: "A1",
            workspace_identity: "T1",
            kind: CodeGrantKind::Workspace,
        },
        &"c".repeat(64),
        &"d".repeat(64),
    )
    .await
    .unwrap();
    let starter = ConversationStarter {
        external_identity: "U1".into(),
        personal_grant_id: Some(person.id),
    };
    let choose = || {
        prepare(
            &runtime,
            &channel,
            Some(&starter),
            HarnessKind::Codex,
            ChannelSubscriptionPreference::default(),
        )
    };
    let before = choose().await.unwrap();
    assert!(before.personal.is_none());
    assert_eq!(
        before.reason.as_deref(),
        Some("channel_sponsorship_not_enabled")
    );
    runtime
        .set_personal_inference_preferences(
            &owner,
            person.id,
            &PersonalInferencePreferences {
                dm_subscription_preference: DmSubscriptionPreference::default(),
                channel_sponsorship_enabled: true,
                consent_version: Some(1),
            },
            Some(&approval_lease()),
        )
        .await
        .unwrap();
    let preferred = choose().await.unwrap();
    assert_eq!(
        preferred.personal.as_ref().map(|p| (&p.0, p.1)),
        Some((&owner, person.id))
    );
    assert_eq!(channel.owner, service_owner);
    let override_default = prepare(
        &runtime,
        &channel,
        Some(&starter),
        HarnessKind::Codex,
        ChannelSubscriptionPreference::GatewayDefault,
    )
    .await
    .unwrap();
    assert!(override_default.personal.is_none());
    let invalid = ConversationStarter {
        external_identity: "U1".into(),
        personal_grant_id: Some(CodeGrantId::new()),
    };
    let invalid_proof = prepare(
        &runtime,
        &channel,
        Some(&invalid),
        HarnessKind::Codex,
        ChannelSubscriptionPreference::GatewayDefault,
    )
    .await
    .unwrap_err();
    assert_eq!(
        invalid_proof.into_response().status(),
        StatusCode::FORBIDDEN
    );
    let missing = ConversationStarter {
        external_identity: "U99".into(),
        personal_grant_id: None,
    };
    assert!(prepare(
        &runtime,
        &channel,
        Some(&missing),
        HarnessKind::Codex,
        ChannelSubscriptionPreference::default()
    )
    .await
    .unwrap()
    .personal
    .is_none());
    assert_eq!(
        before.personal, None,
        "later consent does not change the initial default"
    );
    runtime
        .set_personal_inference_preferences(
            &owner,
            person.id,
            &PersonalInferencePreferences::default(),
            Some(&approval_lease()),
        )
        .await
        .unwrap();
    assert!(choose().await.unwrap().personal.is_none());
    assert_eq!(state.consent_updates.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn personal_consent_rejects_nonowners_missing_leases_and_unversioned_opt_in() {
    let (_dir, _db, runtime, _browser, _base, state, owner) = setup().await;
    state.sponsorship_supported.store(true, Ordering::SeqCst);
    let person = connect(&runtime, &owner).await;
    let enabled = PersonalInferencePreferences {
        channel_sponsorship_enabled: true,
        consent_version: Some(1),
        ..Default::default()
    };
    assert!(runtime
        .set_personal_inference_preferences(
            &OwnerId::new("user:other").unwrap(),
            person.id,
            &enabled,
            Some(&approval_lease())
        )
        .await
        .is_err());
    assert!(runtime
        .set_personal_inference_preferences(&owner, person.id, &enabled, None)
        .await
        .is_err());
    let invalid = PersonalInferencePreferences {
        consent_version: None,
        ..enabled
    };
    assert!(runtime
        .set_personal_inference_preferences(&owner, person.id, &invalid, Some(&approval_lease()))
        .await
        .is_err());
    assert_eq!(state.consent_updates.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn capability_negotiation_keeps_old_gateways_unchanged_and_fails_on_transport_errors() {
    let (_dir, _db, runtime, _browser, _base, state, owner) = setup().await;
    let person = connect(&runtime, &owner).await;
    let external = runtime
        .harness_llm()
        .unwrap()
        .external_delegations()
        .unwrap()
        .clone();
    let unsupported = prepare(
        &runtime,
        &person,
        None,
        HarnessKind::ClaudeCode,
        ChannelSubscriptionPreference::default(),
    )
    .await
    .unwrap();
    assert!(!unsupported.supported);
    assert!(unsupported.freeze(SessionId::new()).sponsor.is_none());
    *external.sponsorship_capability.lock().await = None;
    state.metadata_failed.store(true, Ordering::SeqCst);
    assert!(prepare(
        &runtime,
        &person,
        None,
        HarnessKind::ClaudeCode,
        ChannelSubscriptionPreference::default()
    )
    .await
    .is_err());
    *external.sponsorship_capability.lock().await = None;
    state.metadata_failed.store(false, Ordering::SeqCst);
    state.sponsorship_supported.store(true, Ordering::SeqCst);
    let unknown = prepare(
        &runtime,
        &person,
        None,
        HarnessKind::Internal,
        ChannelSubscriptionPreference::default(),
    )
    .await
    .unwrap();
    assert!(!unknown.supported);
}

#[tokio::test]
async fn new_connect_records_only_explicit_versioned_consent() {
    let (_dir, _db, runtime, _browser, _base, state, owner) = setup().await;
    state.sponsorship_supported.store(true, Ordering::SeqCst);
    let (_, nonce, confirm) = runtime
        .start_connect_handshake("slack", "U1", "T1", "Person", "Workspace", None)
        .await
        .unwrap();
    let (_, csrf) = runtime
        .view_connect_handshake(&owner, &nonce)
        .await
        .unwrap()
        .unwrap();
    runtime
        .approve_connect_handshake_with_consent(
            &owner,
            &nonce,
            &csrf,
            Some(&approval_lease()),
            Some(&InferenceSponsorshipConsent {
                enabled: true,
                consent_version: Some(1),
            }),
        )
        .await
        .unwrap()
        .unwrap();
    let person = runtime
        .complete_connect_handshake(&nonce, &confirm)
        .await
        .unwrap()
        .unwrap()
        .0;
    let saved = runtime
        .personal_inference_preferences(&owner, person.id)
        .await
        .unwrap();
    assert!(saved.preferences.channel_sponsorship_enabled);
    assert_eq!(saved.preferences.consent_version, Some(1));
}

#[tokio::test]
async fn frozen_sponsor_survives_token_renewal_and_reconnect_never_replaces_it() {
    let (_dir, db, runtime, _browser, base, state, owner) = setup().await;
    state.sponsorship_supported.store(true, Ordering::SeqCst);
    let person = connect(&runtime, &owner).await;
    let prepared = prepare(
        &runtime,
        &person,
        None,
        HarnessKind::ClaudeCode,
        ChannelSubscriptionPreference::default(),
    )
    .await
    .unwrap();
    let mut session = crate::code::remote::fixtures::session_value();
    session.owner = owner.clone();
    session.workspace_id = None;
    tidebreak_core::db::code::resolve_external_session_with_channel_context(
        &db,
        &owner,
        person.id,
        "slack",
        "T1:DM:frozen",
        None,
        &session,
        Some(tidebreak_core::db::code::ExternalSessionChannelContext {
            parent: None,
            inference: Some(&prepared),
            channel_id: None,
            instructions: "",
        }),
    )
    .await
    .unwrap();
    let external = ExternalDelegations::new(obo(&base), db.clone());
    let original = external
        .session_sponsor(&owner, session.id)
        .await
        .unwrap()
        .unwrap();
    let slot = external
        .slots
        .lock()
        .unwrap()
        .get(&person.id)
        .unwrap()
        .clone();
    slot.lock().await.as_mut().unwrap().expires_at = 0;
    assert_eq!(
        external
            .session_sponsor(&owner, session.id)
            .await
            .unwrap()
            .unwrap(),
        original
    );
    let restarted = ExternalDelegations::new(obo(&base), db.clone());
    assert_eq!(
        restarted
            .session_sponsor(&owner, session.id)
            .await
            .unwrap()
            .unwrap(),
        original
    );
    let replacement = connect(&runtime, &owner).await;
    assert_ne!(replacement.id, person.id);
    assert!(external.session_sponsor(&owner, session.id).await.is_err());
    assert_eq!(
        tidebreak_core::db::code::session_inference(&db, &owner, session.id)
            .await
            .unwrap()
            .unwrap()
            .sponsor,
        Some(original)
    );
}

#[tokio::test]
async fn unavailable_sponsor_does_not_remove_executor_runtime_control() {
    use crate::code::remote::RuntimeTokenSource;
    use tidebreak_core::code::inference::PreparedSessionInference;
    let (_dir, db, runtime, browser, _base, state, owner) = setup().await;
    state.sponsorship_supported.store(true, Ordering::SeqCst);
    let executor = connect(&runtime, &owner).await;
    let mut session = crate::code::remote::fixtures::session_value();
    session.owner = owner.clone();
    session.workspace_id = None;
    let prepared = PreparedSessionInference {
        inherited: None,
        starter: Some("U2".into()),
        supported: true,
        personal: Some((owner.clone(), CodeGrantId::new(), CodeHandshakeId::new())),
        reason: None,
    };
    tidebreak_core::db::code::resolve_external_session_with_channel_context(
        &db,
        &owner,
        executor.id,
        "slack",
        "T1:controls",
        None,
        &session,
        Some(tidebreak_core::db::code::ExternalSessionChannelContext {
            parent: None,
            inference: Some(&prepared),
            channel_id: None,
            instructions: "",
        }),
    )
    .await
    .unwrap();
    let external = runtime
        .harness_llm()
        .unwrap()
        .external_delegations()
        .unwrap()
        .clone();
    assert!(external.session_sponsor(&owner, session.id).await.is_err());
    let tokens = browser
        .runtime_tokens("tidebreak")
        .with_external_delegations(db.clone())
        .with_embedded_engine_registration(true);
    assert!(tokens.runtime_token(&owner, session.id).await.is_ok());
    let forms = state.exchange_forms.lock().unwrap();
    assert_eq!(forms.last().unwrap()["audience"], "runtime:tidebreak");
    assert!(!forms.last().unwrap().contains_key("inference_sponsor"));
}
