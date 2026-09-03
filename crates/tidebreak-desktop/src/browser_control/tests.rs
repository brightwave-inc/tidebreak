use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use tokio::sync::oneshot;

fn target(name: &str) -> BrowserTargetRecord {
    BrowserTargetRecord {
        frame_path: Vec::new(),
        selector: "button:nth-of-type(1)".to_owned(),
        marker: "__marker".to_owned(),
        marker_value: "@e1".to_owned(),
        fingerprint: BrowserTargetFingerprint {
            tag: "button".to_owned(),
            role: "button".to_owned(),
            name: name.to_owned(),
            input_type: None,
            href: None,
            sensitive: false,
        },
        sensitive: false,
        consequential: false,
    }
}

fn ready_registry(visible: bool) -> (BrowserRegistry, u64) {
    let registry = BrowserRegistry::default();
    let instance = registry
        .register(
            "browser-1",
            "workspace-1",
            "https://example.com".to_owned(),
            visible,
        )
        .unwrap();
    registry
        .page_finished(
            "browser-1",
            "workspace-1",
            instance,
            "https://example.com".to_owned(),
        )
        .unwrap();
    (registry, instance)
}

fn controlled_registry() -> (BrowserRegistry, u64, BrowserOrigin, Uuid, tempfile::TempDir) {
    let (registry, instance) = ready_registry(true);
    let private = tempfile::tempdir().unwrap();
    registry.initialize_private_state(private.path()).unwrap();
    let origin = BrowserOrigin::from_url("https://example.com/path").unwrap();
    registry
        .grant_browser_access(
            "browser-1",
            "workspace-1",
            &origin,
            BrowserOriginScope::Origin {
                origin: origin.clone(),
            },
            &[BrowserGrantCapability::BrowserControlOrigin],
        )
        .unwrap();
    let capability = registry.issue_agent_capability("workspace-1", "Code agent");
    registry
        .begin_agent_control(capability, "browser-1")
        .unwrap();
    (registry, instance, origin, capability, private)
}

fn force_agent_controller(registry: &BrowserRegistry, browser_id: &str, capability_id: Uuid) {
    let mut state = registry.lock();
    let record = state.records.get_mut(browser_id).unwrap();
    record.dispatch.halt.send_replace(false);
    record.controller = BrowserController {
        kind: BrowserControllerKind::Agent,
        label: Some("Code agent".to_owned()),
        action: None,
        halted: false,
        takeover_required: false,
    };
    record.controller_capability_id = Some(capability_id);
}

async fn dispatch_probe(
    registry: BrowserRegistry,
    capability_id: Uuid,
    origin: BrowserOrigin,
    ran: Arc<AtomicBool>,
) -> Result<(), String> {
    registry
        .dispatch_agent(
            capability_id,
            "browser-1",
            &origin,
            BrowserGrantCapability::BrowserControlOrigin,
            "click",
            Some("Continue"),
            BrowserDispatchEffect::Mutate,
            None,
            move || async move {
                ran.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
}

#[test]
fn browser_identity_cannot_be_rebound_to_another_workspace() {
    let registry = BrowserRegistry::default();
    registry
        .register(
            "browser-1",
            "workspace-1",
            "https://example.com".to_owned(),
            true,
        )
        .unwrap();

    assert!(registry.snapshot("browser-1", "workspace-1").is_ok());
    assert!(registry.snapshot("browser-1", "workspace-2").is_err());
    assert!(registry
        .register(
            "browser-1",
            "workspace-2",
            "https://example.org".to_owned(),
            true,
        )
        .is_err());
}

#[test]
fn restart_recovers_only_the_last_completed_navigation_without_authority() {
    let private = tempfile::tempdir().unwrap();
    let owner = OwnerId::local();
    let registry = BrowserRegistry::default();
    registry.initialize_private_state(private.path()).unwrap();
    let instance = registry
        .register_managed(
            "browser-1",
            "workspace-1",
            owner.clone(),
            Uuid::new_v4().to_string(),
            "https://example.com/committed".to_owned(),
            true,
        )
        .unwrap();
    let ready = registry
        .page_started(
            "browser-1",
            "workspace-1",
            instance,
            "https://example.com/committed".to_owned(),
        )
        .and_then(|_| {
            registry.page_finished(
                "browser-1",
                "workspace-1",
                instance,
                "https://example.com/committed".to_owned(),
            )
        })
        .unwrap();
    registry
        .title_changed(
            "browser-1",
            "workspace-1",
            instance,
            Some("https://example.com/committed".to_owned()),
            "Committed page".to_owned(),
        )
        .unwrap();
    let origin = BrowserOrigin::from_url("https://example.com/committed").unwrap();
    registry
        .grant_browser_access(
            "browser-1",
            "workspace-1",
            &origin,
            BrowserOriginScope::Origin {
                origin: origin.clone(),
            },
            &[BrowserGrantCapability::BrowserControlOrigin],
        )
        .unwrap();
    let capability = registry.issue_agent_capability("workspace-1", "Code agent");
    force_agent_controller(&registry, "browser-1", capability);
    registry
        .record_semantic_snapshot(
            "browser-1",
            "workspace-1",
            ready.document_epoch.unwrap(),
            "snapshot-1".to_owned(),
            HashMap::from([("@e1".to_owned(), target("Continue"))]),
        )
        .unwrap();

    registry
        .page_started(
            "browser-1",
            "workspace-1",
            instance,
            "https://example.com/in-flight".to_owned(),
        )
        .unwrap();
    registry
        .title_changed(
            "browser-1",
            "workspace-1",
            instance,
            Some("https://example.com/in-flight".to_owned()),
            "In-flight page".to_owned(),
        )
        .unwrap();
    drop(registry);

    let reopened = BrowserRegistry::default();
    reopened.initialize_private_state(private.path()).unwrap();
    let recovered = reopened
        .recover_session(&owner, "browser-1", "workspace-1")
        .unwrap()
        .unwrap();
    assert_eq!(recovered.url, "https://example.com/committed");
    assert_eq!(recovered.title.as_deref(), Some("Committed page"));

    reopened
        .register_managed_with_title(
            "browser-1",
            "workspace-1",
            ManagedBrowserRegistration {
                owner_id: owner,
                profile_id: Uuid::new_v4().to_string(),
                url: recovered.url,
                title: recovered.title,
                visible: true,
            },
        )
        .unwrap();
    let snapshot = reopened.snapshot("browser-1", "workspace-1").unwrap();
    assert_eq!(snapshot.document_epoch, Some(0));
    assert_eq!(
        snapshot.controller.unwrap().kind,
        BrowserControllerKind::Human
    );
    let access = snapshot.agent_access.unwrap();
    assert!(!access.shared);
    assert!(!access.can_observe);
    assert!(!access.can_control);
    let state = reopened.lock();
    let record = state.records.get("browser-1").unwrap();
    assert!(record.controller_capability_id.is_none());
    assert!(record.semantic_snapshot.is_none());
    assert!(state.capabilities.is_empty());
    assert!(state.grants.is_empty());
}

#[test]
fn same_document_navigation_becomes_the_recovery_url() {
    let private = tempfile::tempdir().unwrap();
    let owner = OwnerId::local();
    let registry = BrowserRegistry::default();
    registry.initialize_private_state(private.path()).unwrap();
    let instance = registry
        .register_managed(
            "browser-1",
            "workspace-1",
            owner.clone(),
            Uuid::new_v4().to_string(),
            "https://example.com/start".to_owned(),
            true,
        )
        .unwrap();
    let ready = registry
        .page_started(
            "browser-1",
            "workspace-1",
            instance,
            "https://example.com/start".to_owned(),
        )
        .and_then(|_| {
            registry.page_finished(
                "browser-1",
                "workspace-1",
                instance,
                "https://example.com/start".to_owned(),
            )
        })
        .unwrap();
    registry
        .same_document_navigation(
            "browser-1",
            "workspace-1",
            instance,
            ready.document_epoch.unwrap(),
            "https://example.com/start#details".to_owned(),
        )
        .unwrap();
    drop(registry);

    let reopened = BrowserRegistry::default();
    reopened.initialize_private_state(private.path()).unwrap();
    assert_eq!(
        reopened
            .recover_session(&owner, "browser-1", "workspace-1")
            .unwrap()
            .unwrap()
            .url,
        "https://example.com/start#details"
    );
}

#[test]
fn explicit_close_forgets_only_the_exact_recovery_binding() {
    let private = tempfile::tempdir().unwrap();
    let owner = OwnerId::local();
    let registry = BrowserRegistry::default();
    registry.initialize_private_state(private.path()).unwrap();
    let instance = registry
        .register_managed(
            "browser-1",
            "workspace-1",
            owner.clone(),
            Uuid::new_v4().to_string(),
            "https://example.com/".to_owned(),
            true,
        )
        .unwrap();
    registry
        .page_finished(
            "browser-1",
            "workspace-1",
            instance,
            "https://example.com/".to_owned(),
        )
        .unwrap();

    assert!(registry
        .recover_session(&owner, "browser-1", "workspace-2")
        .is_err());
    assert!(registry
        .forget_recovery(&owner, "browser-1", "workspace-2")
        .is_err());
    registry.remove("browser-1", "workspace-1").unwrap();
    registry
        .forget_recovery(&owner, "browser-1", "workspace-1")
        .unwrap();
    drop(registry);

    let reopened = BrowserRegistry::default();
    reopened.initialize_private_state(private.path()).unwrap();
    assert!(reopened
        .recover_session(&owner, "browser-1", "workspace-1")
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn profile_reset_drains_and_removes_only_matching_native_sessions() {
    let registry = BrowserRegistry::default();
    let owner = OwnerId::local();
    let other_owner = OwnerId::new("other-owner").unwrap();
    let profile_id = Uuid::new_v4().to_string();
    registry
        .register_managed(
            "browser-1",
            "workspace-1",
            owner.clone(),
            profile_id.clone(),
            "https://example.com/one".to_owned(),
            true,
        )
        .unwrap();
    registry
        .register_managed(
            "browser-2",
            "workspace-2",
            owner.clone(),
            profile_id.clone(),
            "https://example.org/two".to_owned(),
            true,
        )
        .unwrap();
    registry
        .register_managed(
            "other-owner",
            "workspace-3",
            other_owner,
            profile_id.clone(),
            "https://owner.example/".to_owned(),
            true,
        )
        .unwrap();
    registry
        .register_managed(
            "other-profile",
            "workspace-1",
            owner.clone(),
            Uuid::new_v4().to_string(),
            "https://unrelated.example/".to_owned(),
            true,
        )
        .unwrap();
    let origin = BrowserOrigin::from_url("https://example.com/one").unwrap();
    registry
        .grant_browser_access(
            "browser-1",
            "workspace-1",
            &origin,
            BrowserOriginScope::Origin {
                origin: origin.clone(),
            },
            &[BrowserGrantCapability::BrowserObserveOrigin],
        )
        .unwrap();

    let reset = registry
        .begin_profile_reset("browser-1", "workspace-1")
        .await
        .unwrap();

    assert_eq!(
        reset
            .sessions()
            .iter()
            .map(|session| (session.browser_id.as_str(), session.workspace_id.as_str()))
            .collect::<Vec<_>>(),
        [("browser-1", "workspace-1"), ("browser-2", "workspace-2")]
    );
    assert_eq!(reset.profile_id(), profile_id);
    assert_eq!(
        registry.snapshot("browser-1", "workspace-1").unwrap_err(),
        "browser profile is being reset"
    );

    reset.finish();

    assert!(registry.snapshot("browser-1", "workspace-1").is_err());
    assert!(registry.snapshot("browser-2", "workspace-2").is_err());
    assert!(registry.snapshot("other-owner", "workspace-3").is_ok());
    assert!(registry.snapshot("other-profile", "workspace-1").is_ok());

    registry
        .register_managed(
            "fresh-browser",
            "workspace-1",
            owner,
            Uuid::new_v4().to_string(),
            "https://example.com/fresh".to_owned(),
            true,
        )
        .unwrap();
    assert!(
        registry
            .snapshot("fresh-browser", "workspace-1")
            .unwrap()
            .agent_access
            .unwrap()
            .can_observe
    );
}

#[tokio::test]
async fn aborted_profile_reset_restores_the_previous_dispatch_latch() {
    let (registry, _) = ready_registry(true);
    {
        let reset = registry
            .begin_profile_reset("browser-1", "workspace-1")
            .await
            .unwrap();
        assert_eq!(reset.sessions().len(), 1);
    }

    assert!(registry.snapshot("browser-1", "workspace-1").is_ok());
    assert!(!*registry
        .lock()
        .records
        .get("browser-1")
        .unwrap()
        .dispatch
        .halt
        .borrow());
}
#[test]
fn model_facing_lists_are_workspace_scoped_and_stably_ordered() {
    let registry = BrowserRegistry::default();
    registry
        .register(
            "browser-b",
            "workspace-1",
            "https://example.com/b".to_owned(),
            true,
        )
        .unwrap();
    registry
        .register(
            "browser-a",
            "workspace-1",
            "https://example.com/a".to_owned(),
            false,
        )
        .unwrap();
    registry
        .register(
            "browser-secret",
            "workspace-2",
            "https://private.example.com".to_owned(),
            true,
        )
        .unwrap();

    let sessions = registry.list_for_workspace("workspace-1");
    assert_eq!(
        sessions
            .iter()
            .map(|session| session.browser_id.as_str())
            .collect::<Vec<_>>(),
        ["browser-a", "browser-b"]
    );
    assert!(sessions
        .iter()
        .all(|session| !session.browser_id.contains("secret")));
    assert_eq!(registry.list_for_workspace("workspace-missing"), []);
}

#[test]
fn document_epoch_advances_on_each_started_document() {
    let registry = BrowserRegistry::default();
    let instance = registry
        .register(
            "browser-1",
            "workspace-1",
            "https://example.com".to_owned(),
            true,
        )
        .unwrap();

    let first = registry
        .page_started(
            "browser-1",
            "workspace-1",
            instance,
            "https://example.com".to_owned(),
        )
        .unwrap();
    let second = registry
        .page_started(
            "browser-1",
            "workspace-1",
            instance,
            "https://example.com/next".to_owned(),
        )
        .unwrap();

    assert_eq!(first.document_epoch, Some(1));
    assert_eq!(first.load_state, Some(BrowserLoadState::Loading));
    assert_eq!(second.document_epoch, Some(2));
}

#[test]
fn same_document_navigation_updates_every_registry_projection_without_advancing_epoch() {
    let registry = BrowserRegistry::default();
    let instance = registry
        .register(
            "browser-1",
            "workspace-1",
            "https://example.com/".to_owned(),
            true,
        )
        .unwrap();
    let ready = registry
        .page_started(
            "browser-1",
            "workspace-1",
            instance,
            "https://example.com/".to_owned(),
        )
        .and_then(|_| {
            registry.page_finished(
                "browser-1",
                "workspace-1",
                instance,
                "https://example.com/".to_owned(),
            )
        })
        .unwrap();
    let epoch = ready.document_epoch.unwrap();
    registry
        .record_semantic_snapshot(
            "browser-1",
            "workspace-1",
            epoch,
            "snapshot-1".to_owned(),
            HashMap::from([("@e1".to_owned(), target("Continue"))]),
        )
        .unwrap();
    registry
        .record_screenshot_epoch("browser-1", "workspace-1", epoch)
        .unwrap();

    for url in [
        "https://example.com/?view=details",
        "https://example.com/?view=replaced",
        "https://example.com/?view=replaced#summary",
        "https://example.com/?view=details",
        "https://example.com/?view=replaced#summary",
    ] {
        let snapshot = registry
            .same_document_navigation("browser-1", "workspace-1", instance, epoch, url.to_owned())
            .unwrap();
        assert_eq!(snapshot.url.as_deref(), Some(url));
        assert_eq!(snapshot.document_epoch, Some(epoch));
        assert_eq!(snapshot.load_state, Some(BrowserLoadState::Ready));
        assert_eq!(
            registry.list_for_workspace("workspace-1")[0].url.as_deref(),
            Some(url)
        );
        registry
            .validate_snapshot_id("browser-1", "workspace-1", "snapshot-1", epoch)
            .expect("same-document navigation keeps semantic targets live");
        assert_eq!(
            registry
                .lock()
                .records
                .get("browser-1")
                .and_then(|record| record.screenshot_epoch),
            Some(epoch)
        );
    }

    registry
        .page_started(
            "browser-1",
            "workspace-1",
            instance,
            "https://example.com/full-navigation".to_owned(),
        )
        .unwrap();
    assert!(registry
        .validate_snapshot_id("browser-1", "workspace-1", "snapshot-1", epoch)
        .is_err());
    assert_eq!(
        registry
            .lock()
            .records
            .get("browser-1")
            .and_then(|record| record.screenshot_epoch),
        None
    );
}

#[test]
fn stale_same_document_observers_cannot_update_a_later_document() {
    let (registry, instance) = ready_registry(true);
    let epoch = registry
        .snapshot("browser-1", "workspace-1")
        .unwrap()
        .document_epoch
        .unwrap();
    registry
        .page_started(
            "browser-1",
            "workspace-1",
            instance,
            "https://example.com/full-navigation".to_owned(),
        )
        .unwrap();

    assert!(registry
        .same_document_navigation(
            "browser-1",
            "workspace-1",
            instance,
            epoch,
            "https://example.com/stale".to_owned(),
        )
        .is_none());
    assert_eq!(
        registry
            .snapshot("browser-1", "workspace-1")
            .unwrap()
            .url
            .as_deref(),
        Some("https://example.com/full-navigation")
    );
}

#[test]
fn callbacks_from_a_replaced_native_view_cannot_mutate_the_new_record() {
    let registry = BrowserRegistry::default();
    let old_instance = registry
        .register(
            "browser-1",
            "workspace-1",
            "https://old.example".to_owned(),
            true,
        )
        .unwrap();
    assert!(registry.remove("browser-1", "workspace-1").unwrap());
    let new_instance = registry
        .register(
            "browser-1",
            "workspace-1",
            "https://new.example".to_owned(),
            true,
        )
        .unwrap();

    assert_ne!(old_instance, new_instance);
    assert!(registry
        .page_finished(
            "browser-1",
            "workspace-1",
            old_instance,
            "https://old.example/late".to_owned(),
        )
        .is_none());
    assert_eq!(
        registry
            .snapshot("browser-1", "workspace-1")
            .unwrap()
            .url
            .as_deref(),
        Some("https://new.example")
    );
}

#[test]
fn close_then_recreate_gets_a_fresh_native_instance() {
    let registry = BrowserRegistry::default();
    let first = registry
        .register(
            "browser-1",
            "workspace-1",
            "https://example.com/one".to_owned(),
            false,
        )
        .unwrap();
    assert!(registry.remove("browser-1", "workspace-1").unwrap());
    assert!(registry.snapshot("browser-1", "workspace-1").is_err());

    let second = registry
        .register(
            "browser-1",
            "workspace-1",
            "https://example.com/two".to_owned(),
            true,
        )
        .unwrap();
    assert!(second > first);
    let snapshot = registry.snapshot("browser-1", "workspace-1").unwrap();
    assert_eq!(snapshot.url.as_deref(), Some("https://example.com/two"));
    assert_eq!(snapshot.visible, Some(true));
}

#[test]
fn public_grants_cover_only_the_exact_normalized_origin() {
    let (registry, instance) = ready_registry(true);
    let origin = BrowserOrigin::from_url("https://example.com/private?token=secret").unwrap();
    let shared = registry
        .grant_browser_access(
            "browser-1",
            "workspace-1",
            &origin,
            BrowserOriginScope::Origin {
                origin: origin.clone(),
            },
            &[BrowserGrantCapability::BrowserControlOrigin],
        )
        .unwrap()
        .agent_access
        .unwrap();
    assert!(shared.shared);
    assert!(shared.can_observe);
    assert!(shared.can_control);
    assert!(!shared.can_transfer_files);
    assert_eq!(shared.origin.as_deref(), Some("https://example.com"));
    assert_eq!(shared.scope, Some(BrowserAgentAccessScope::Origin));

    let same_origin = registry
        .page_started(
            "browser-1",
            "workspace-1",
            instance,
            "https://example.com/another/path".to_owned(),
        )
        .unwrap()
        .agent_access
        .unwrap();
    assert!(same_origin.shared);

    let other_port = registry
        .page_started(
            "browser-1",
            "workspace-1",
            instance,
            "https://example.com:444/another/path".to_owned(),
        )
        .unwrap()
        .agent_access
        .unwrap();
    assert!(!other_port.shared);
    assert!(!other_port.can_observe);
    assert_eq!(other_port.scope, None);
}

#[test]
fn loopback_workspace_grants_follow_local_port_changes_only() {
    let registry = BrowserRegistry::default();
    let instance = registry
        .register(
            "browser-1",
            "workspace-1",
            "http://localhost:3000".to_owned(),
            true,
        )
        .unwrap();
    registry
        .page_finished(
            "browser-1",
            "workspace-1",
            instance,
            "http://localhost:3000".to_owned(),
        )
        .unwrap();
    let origin = BrowserOrigin::from_url("http://localhost:3000").unwrap();
    registry
        .grant_browser_access(
            "browser-1",
            "workspace-1",
            &origin,
            BrowserOriginScope::LoopbackWorkspace,
            &[BrowserGrantCapability::BrowserControlOrigin],
        )
        .unwrap();

    let another_local_origin = registry
        .page_started(
            "browser-1",
            "workspace-1",
            instance,
            "http://127.0.0.1:4317/review".to_owned(),
        )
        .unwrap()
        .agent_access
        .unwrap();
    assert!(another_local_origin.shared);
    assert!(another_local_origin.can_control);
    assert_eq!(
        another_local_origin.scope,
        Some(BrowserAgentAccessScope::LoopbackWorkspace)
    );

    let public_origin = registry
        .page_started(
            "browser-1",
            "workspace-1",
            instance,
            "https://example.com".to_owned(),
        )
        .unwrap()
        .agent_access
        .unwrap();
    assert!(!public_origin.shared);
    assert_eq!(public_origin.scope, None);
}

#[tokio::test]
async fn cross_workspace_capabilities_fail_before_engine_dispatch() {
    let (registry, _) = ready_registry(true);
    let origin = BrowserOrigin::from_url("https://example.com").unwrap();
    registry
        .grant_browser_access(
            "browser-1",
            "workspace-1",
            &origin,
            BrowserOriginScope::Origin {
                origin: origin.clone(),
            },
            &[BrowserGrantCapability::BrowserControlOrigin],
        )
        .unwrap();
    let capability = registry.issue_agent_capability("workspace-2", "Other agent");
    force_agent_controller(&registry, "browser-1", capability);
    let ran = Arc::new(AtomicBool::new(false));

    let error = dispatch_probe(registry, capability, origin, Arc::clone(&ran))
        .await
        .unwrap_err();
    assert!(error.contains("different workspace"));
    assert!(!ran.load(Ordering::SeqCst));
}

#[tokio::test]
async fn expired_and_revoked_capabilities_fail_before_engine_dispatch() {
    let (expired_registry, _) = ready_registry(true);
    let origin = BrowserOrigin::from_url("https://example.com").unwrap();
    let expired =
        expired_registry.issue_agent_capability_for("workspace-1", "Expired agent", Duration::ZERO);
    force_agent_controller(&expired_registry, "browser-1", expired);
    let expired_ran = Arc::new(AtomicBool::new(false));
    let error = dispatch_probe(
        expired_registry,
        expired,
        origin.clone(),
        Arc::clone(&expired_ran),
    )
    .await
    .unwrap_err();
    assert!(error.contains("capability is unavailable"));
    assert!(!expired_ran.load(Ordering::SeqCst));

    let (revoked_registry, _, _, revoked, _private) = controlled_registry();
    revoked_registry.revoke_agent_capability(revoked);
    let revoked_ran = Arc::new(AtomicBool::new(false));
    let error = dispatch_probe(revoked_registry, revoked, origin, Arc::clone(&revoked_ran))
        .await
        .unwrap_err();
    assert!(error.contains("capability is unavailable"));
    assert!(!revoked_ran.load(Ordering::SeqCst));
}

#[tokio::test]
async fn hidden_browsers_fail_before_engine_dispatch() {
    let (registry, _, origin, capability, _private) = controlled_registry();
    registry
        .set_visible("browser-1", "workspace-1", false)
        .unwrap();
    let ran = Arc::new(AtomicBool::new(false));

    let error = dispatch_probe(registry, capability, origin, Arc::clone(&ran))
        .await
        .unwrap_err();
    assert_eq!(error, "browser is hidden");
    assert!(!ran.load(Ordering::SeqCst));
}

#[tokio::test]
async fn revoking_origin_access_returns_the_tab_to_unshared_human_control() {
    let (registry, _, origin, capability, _private) = controlled_registry();
    let revoked = registry
        .revoke_browser_access("browser-1", "workspace-1")
        .unwrap();
    assert_eq!(
        revoked.controller.unwrap().kind,
        BrowserControllerKind::Human
    );
    let access = revoked.agent_access.unwrap();
    assert!(!access.shared);
    assert!(!access.paused);
    assert!(access.halted);

    let ran = Arc::new(AtomicBool::new(false));
    let error = dispatch_probe(registry, capability, origin, Arc::clone(&ran))
        .await
        .unwrap_err();
    assert!(error.contains("stopped by the user"));
    assert!(!ran.load(Ordering::SeqCst));
}

#[test]
fn semantic_targets_are_scoped_to_the_exact_snapshot_and_epoch() {
    let (registry, instance) = ready_registry(true);
    registry
        .record_semantic_snapshot(
            "browser-1",
            "workspace-1",
            0,
            "snapshot-1".to_owned(),
            HashMap::from([("@e1".to_owned(), target("Continue"))]),
        )
        .unwrap();

    assert!(registry
        .semantic_target("browser-1", "workspace-1", "snapshot-1", 0, "@e1",)
        .is_ok());
    assert_eq!(
        registry.semantic_target("browser-1", "workspace-1", "snapshot-other", 0, "@e1",),
        Err(BrowserTargetError::StaleTarget)
    );
    assert_eq!(
        registry.semantic_target("browser-1", "workspace-1", "snapshot-1", 1, "@e1",),
        Err(BrowserTargetError::StaleTarget)
    );

    registry
        .page_started(
            "browser-1",
            "workspace-1",
            instance,
            "https://example.com/next".to_owned(),
        )
        .unwrap();
    assert_eq!(
        registry.semantic_target("browser-1", "workspace-1", "snapshot-1", 0, "@e1",),
        Err(BrowserTargetError::StaleTarget)
    );
}

#[test]
fn missing_refs_never_fall_back_to_another_target() {
    let (registry, _) = ready_registry(true);
    registry
        .record_semantic_snapshot(
            "browser-1",
            "workspace-1",
            0,
            "snapshot-1".to_owned(),
            HashMap::from([("@e1".to_owned(), target("Continue"))]),
        )
        .unwrap();

    assert_eq!(
        registry.semantic_target("browser-1", "workspace-1", "snapshot-1", 0, "@e404",),
        Err(BrowserTargetError::StaleTarget)
    );
}

#[test]
fn hidden_or_revealed_browsers_require_a_fresh_snapshot() {
    let (registry, _) = ready_registry(true);
    registry
        .record_semantic_snapshot(
            "browser-1",
            "workspace-1",
            0,
            "snapshot-1".to_owned(),
            HashMap::from([("@e1".to_owned(), target("Continue"))]),
        )
        .unwrap();

    registry
        .set_visible("browser-1", "workspace-1", false)
        .unwrap();
    assert_eq!(
        registry.semantic_target("browser-1", "workspace-1", "snapshot-1", 0, "@e1",),
        Err(BrowserTargetError::BrowserHidden)
    );

    registry
        .set_visible("browser-1", "workspace-1", true)
        .unwrap();
    assert_eq!(
        registry.semantic_target("browser-1", "workspace-1", "snapshot-1", 0, "@e1",),
        Err(BrowserTargetError::StaleTarget)
    );
}

#[test]
fn platform_claims_only_implemented_agent_capabilities() {
    let descriptor = platform_default_engine();
    assert_eq!(
        descriptor.capabilities.semantic_actions,
        cfg!(target_os = "macos")
    );
    assert!(!descriptor.capabilities.screenshot);
    assert_eq!(
        descriptor.capabilities.profile_reset,
        cfg!(target_os = "macos")
    );
}

#[tokio::test]
async fn stop_latches_before_in_flight_dispatch_drains_and_rejects_queued_work() {
    let (registry, _, origin, capability, _private) = controlled_registry();
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let first_registry = registry.clone();
    let first_origin = origin.clone();
    let first = tokio::spawn(async move {
        first_registry
            .dispatch_agent(
                capability,
                "browser-1",
                &first_origin,
                BrowserGrantCapability::BrowserControlOrigin,
                "click",
                Some("First action"),
                BrowserDispatchEffect::Mutate,
                None,
                move || async move {
                    let _ = entered_tx.send(());
                    release_rx
                        .await
                        .map_err(|_| "test dispatch release was dropped".to_owned())?;
                    Ok(())
                },
            )
            .await
    });
    entered_rx.await.unwrap();

    let stop_registry = registry.clone();
    let stop = tokio::spawn(async move {
        stop_registry
            .stop_agent_control("browser-1", "workspace-1")
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = registry.snapshot("browser-1", "workspace-1").unwrap();
            if snapshot.agent_access.unwrap().halted {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Stop should publish its latch before the active dispatch returns");
    assert!(!stop.is_finished());

    let queued_ran = Arc::new(AtomicBool::new(false));
    let queued = tokio::spawn(dispatch_probe(
        registry.clone(),
        capability,
        origin,
        Arc::clone(&queued_ran),
    ));
    release_tx.send(()).unwrap();

    first.await.unwrap().unwrap();
    let queued_error = queued.await.unwrap().unwrap_err();
    assert!(queued_error.contains("stopped by the user"));
    assert!(!queued_ran.load(Ordering::SeqCst));
    let stopped = stop.await.unwrap().unwrap();
    assert!(stopped.controller.unwrap().halted);
}

#[tokio::test]
async fn human_takeover_wins_over_queued_agent_input() {
    let (registry, _, origin, capability, _private) = controlled_registry();
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let first_registry = registry.clone();
    let first_origin = origin.clone();
    let first = tokio::spawn(async move {
        first_registry
            .dispatch_agent(
                capability,
                "browser-1",
                &first_origin,
                BrowserGrantCapability::BrowserControlOrigin,
                "click",
                Some("First action"),
                BrowserDispatchEffect::Mutate,
                None,
                move || async move {
                    let _ = entered_tx.send(());
                    release_rx
                        .await
                        .map_err(|_| "test dispatch release was dropped".to_owned())?;
                    Ok(())
                },
            )
            .await
    });
    entered_rx.await.unwrap();

    let takeover_registry = registry.clone();
    let takeover = tokio::spawn(async move {
        takeover_registry
            .take_human_control("browser-1", "workspace-1")
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = registry.snapshot("browser-1", "workspace-1").unwrap();
            if snapshot.controller.unwrap().kind == BrowserControllerKind::Human {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("takeover should transfer ownership before the active dispatch returns");
    assert!(!takeover.is_finished());

    let queued_ran = Arc::new(AtomicBool::new(false));
    let queued = tokio::spawn(dispatch_probe(
        registry.clone(),
        capability,
        origin,
        Arc::clone(&queued_ran),
    ));
    release_tx.send(()).unwrap();

    first.await.unwrap().unwrap();
    let queued_error = queued.await.unwrap().unwrap_err();
    assert!(
        queued_error.contains("stopped by the user")
            || queued_error.contains("not controlled by this agent")
    );
    assert!(!queued_ran.load(Ordering::SeqCst));
    let human = takeover.await.unwrap().unwrap();
    assert_eq!(human.controller.unwrap().kind, BrowserControllerKind::Human);
}

#[tokio::test]
async fn native_confirmations_are_exact_expiring_and_single_use() {
    let (registry, _, origin, capability, _private) = controlled_registry();
    let confirmation = registry
        .record_native_confirmation(
            capability,
            "browser-1",
            &origin,
            BrowserGrantCapability::BrowserControlOrigin,
            "submit_form",
            Some("Create deployment"),
            None,
        )
        .unwrap();
    {
        let state = registry.lock();
        let record = state.confirmations.get(&confirmation).unwrap();
        assert_eq!(record.capability_id, capability);
        assert_eq!(record.browser_id, "browser-1");
        assert_eq!(record.workspace_id, "workspace-1");
        assert_eq!(record.origin, origin);
        assert_eq!(
            record.required_capability,
            BrowserGrantCapability::BrowserControlOrigin
        );
        assert_eq!(record.action_type, "submit_form");
        assert_eq!(record.target_label.as_deref(), Some("Create deployment"));
        assert!(record.binding.is_none());
    }

    let missing_ran = Arc::new(AtomicBool::new(false));
    let error = registry
        .dispatch_agent(
            capability,
            "browser-1",
            &origin,
            BrowserGrantCapability::BrowserControlOrigin,
            "submit_form",
            Some("Create deployment"),
            BrowserDispatchEffect::Consequential,
            None,
            {
                let missing_ran = Arc::clone(&missing_ran);
                move || async move {
                    missing_ran.store(true, Ordering::SeqCst);
                    Ok(())
                }
            },
        )
        .await
        .unwrap_err();
    assert!(error.contains("requires native confirmation"));
    assert!(!missing_ran.load(Ordering::SeqCst));

    let mismatch_ran = Arc::new(AtomicBool::new(false));
    let error = registry
        .dispatch_agent(
            capability,
            "browser-1",
            &origin,
            BrowserGrantCapability::BrowserControlOrigin,
            "submit_form",
            Some("Delete deployment"),
            BrowserDispatchEffect::Consequential,
            Some(confirmation),
            {
                let mismatch_ran = Arc::clone(&mismatch_ran);
                move || async move {
                    mismatch_ran.store(true, Ordering::SeqCst);
                    Ok(())
                }
            },
        )
        .await
        .unwrap_err();
    assert!(error.contains("does not match"));
    assert!(!mismatch_ran.load(Ordering::SeqCst));

    registry
        .dispatch_agent(
            capability,
            "browser-1",
            &origin,
            BrowserGrantCapability::BrowserControlOrigin,
            "submit_form",
            Some("Create deployment"),
            BrowserDispatchEffect::Consequential,
            Some(confirmation),
            || async { Ok(()) },
        )
        .await
        .unwrap();
    let error = registry
        .dispatch_agent(
            capability,
            "browser-1",
            &origin,
            BrowserGrantCapability::BrowserControlOrigin,
            "submit_form",
            Some("Create deployment"),
            BrowserDispatchEffect::Consequential,
            Some(confirmation),
            || async { Ok(()) },
        )
        .await
        .unwrap_err();
    assert!(error.contains("confirmation is unavailable"));

    let expired = registry
        .record_native_confirmation_for(
            capability,
            "browser-1",
            &origin,
            BrowserGrantCapability::BrowserControlOrigin,
            "submit_form",
            Some("Create deployment"),
            None,
            Duration::ZERO,
        )
        .unwrap();
    let error = registry
        .dispatch_agent(
            capability,
            "browser-1",
            &origin,
            BrowserGrantCapability::BrowserControlOrigin,
            "submit_form",
            Some("Create deployment"),
            BrowserDispatchEffect::Consequential,
            Some(expired),
            || async { Ok(()) },
        )
        .await
        .unwrap_err();
    assert!(error.contains("confirmation is unavailable"));
}

#[tokio::test]
async fn upload_confirmations_bind_capability_and_exact_file_digest() {
    let (registry, _, origin, capability, _private) = controlled_registry();
    registry
        .grant_browser_access(
            "browser-1",
            "workspace-1",
            &origin,
            BrowserOriginScope::Origin {
                origin: origin.clone(),
            },
            &[BrowserGrantCapability::BrowserTransferFiles],
        )
        .unwrap();
    let binding = BrowserConfirmationBinding {
        filename: "report.pdf".to_owned(),
        byte_len: 4,
        sha256: [7; 32],
    };
    let confirmation = registry
        .record_native_confirmation(
            capability,
            "browser-1",
            &origin,
            BrowserGrantCapability::BrowserTransferFiles,
            "upload_file",
            Some("File input"),
            Some(&binding),
        )
        .unwrap();

    let capability_mismatch_ran = Arc::new(AtomicBool::new(false));
    let error = registry
        .dispatch_agent_with_confirmation_binding(
            capability,
            "browser-1",
            &origin,
            BrowserGrantCapability::BrowserControlOrigin,
            "upload_file",
            Some("File input"),
            BrowserDispatchEffect::Consequential,
            Some(confirmation),
            Some(binding.clone()),
            {
                let ran = Arc::clone(&capability_mismatch_ran);
                move || async move {
                    ran.store(true, Ordering::SeqCst);
                    Ok(())
                }
            },
        )
        .await
        .unwrap_err();
    assert!(error.contains("does not match"));
    assert!(!capability_mismatch_ran.load(Ordering::SeqCst));

    let mut changed = binding.clone();
    changed.sha256[0] ^= 1;
    let digest_mismatch_ran = Arc::new(AtomicBool::new(false));
    let error = registry
        .dispatch_agent_with_confirmation_binding(
            capability,
            "browser-1",
            &origin,
            BrowserGrantCapability::BrowserTransferFiles,
            "upload_file",
            Some("File input"),
            BrowserDispatchEffect::Consequential,
            Some(confirmation),
            Some(changed),
            {
                let ran = Arc::clone(&digest_mismatch_ran);
                move || async move {
                    ran.store(true, Ordering::SeqCst);
                    Ok(())
                }
            },
        )
        .await
        .unwrap_err();
    assert!(error.contains("does not match"));
    assert!(!digest_mismatch_ran.load(Ordering::SeqCst));

    let exact_ran = Arc::new(AtomicBool::new(false));
    registry
        .dispatch_agent_with_confirmation_binding(
            capability,
            "browser-1",
            &origin,
            BrowserGrantCapability::BrowserTransferFiles,
            "upload_file",
            Some("File input"),
            BrowserDispatchEffect::Consequential,
            Some(confirmation),
            Some(binding),
            {
                let ran = Arc::clone(&exact_ran);
                move || async move {
                    ran.store(true, Ordering::SeqCst);
                    Ok(())
                }
            },
        )
        .await
        .unwrap();
    assert!(exact_ran.load(Ordering::SeqCst));
}

#[tokio::test]
async fn audit_intent_is_durable_before_dispatch_and_excludes_sensitive_data() {
    const ENTERED_TEXT: &str = "do-not-store-this-password";
    const FULL_URL: &str = "https://example.com/private/report?token=do-not-store";
    const PAGE_CONTENT: &str = "untrusted page content must not enter the audit";

    let (registry, _, origin, capability, private) = controlled_registry();
    let audit_path = private.path().join(BROWSER_AUDIT_FILE);
    let dispatch_audit_path = audit_path.clone();
    registry
        .dispatch_agent(
            capability,
            "browser-1",
            &origin,
            BrowserGrantCapability::BrowserControlOrigin,
            "type",
            Some("Email address"),
            BrowserDispatchEffect::Mutate,
            None,
            move || async move {
                let audit = std::fs::read_to_string(&dispatch_audit_path).unwrap();
                let lines = audit.lines().collect::<Vec<_>>();
                assert_eq!(lines.len(), 1);
                let intent: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
                assert_eq!(intent["phase"], "intent");
                assert_eq!(intent["outcome"], "pending");
                assert_eq!(intent["origin"], "https://example.com");
                assert_eq!(intent["actionType"], "type");
                assert_eq!(intent["semanticTargetLabel"], "Email address");
                let _engine_only = (ENTERED_TEXT, FULL_URL, PAGE_CONTENT);
                Ok(())
            },
        )
        .await
        .unwrap();

    let audit = std::fs::read_to_string(audit_path).unwrap();
    let events = audit
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["phase"], "intent");
    assert_eq!(events[1]["phase"], "outcome");
    assert_eq!(events[1]["outcome"], "succeeded");
    assert_eq!(events[0]["eventId"], events[1]["eventId"]);
    assert!(!audit.contains(ENTERED_TEXT));
    assert!(!audit.contains(FULL_URL));
    assert!(!audit.contains(PAGE_CONTENT));
}

#[tokio::test]
async fn stop_and_takeover_survive_broken_audit_storage() {
    let (registry, _, origin, capability, private) = controlled_registry();
    let audit_path = private.path().join(BROWSER_AUDIT_FILE);
    std::fs::create_dir(&audit_path).unwrap();
    let ran = Arc::new(AtomicBool::new(false));
    let error = dispatch_probe(registry.clone(), capability, origin, Arc::clone(&ran))
        .await
        .unwrap_err();
    assert!(error.contains("audit storage is unavailable"));
    assert!(!ran.load(Ordering::SeqCst));

    let stopped = registry
        .stop_agent_control("browser-1", "workspace-1")
        .await
        .unwrap();
    assert!(stopped.controller.unwrap().halted);
    let human = registry
        .take_human_control("browser-1", "workspace-1")
        .await
        .unwrap();
    assert_eq!(human.controller.unwrap().kind, BrowserControllerKind::Human);
}

#[test]
fn cross_origin_redirects_pause_before_the_destination_is_exposed() {
    let (registry, instance, _origin, _capability, _private) = controlled_registry();
    let destination_url = "https://accounts.example.org/login?continue=%2Fsettings";
    let destination = BrowserOrigin::from_url("https://accounts.example.org/login").unwrap();

    let decision = registry.authorize_navigation(
        "browser-1",
        "workspace-1",
        instance,
        destination_url,
        &destination,
    );
    let BrowserNavigationDecision::Pause {
        origin: paused_origin,
        snapshot,
    } = decision
    else {
        panic!("ungranted redirect should pause");
    };
    assert_eq!(paused_origin, "https://accounts.example.org");
    assert_eq!(snapshot.url.as_deref(), Some("https://example.com"));
    let access = snapshot.agent_access.unwrap();
    assert!(access.paused);
    assert!(access.halted);
    assert!(!access.shared);
    assert_eq!(
        access.origin.as_deref(),
        Some("https://accounts.example.org")
    );
    assert_eq!(
        registry
            .share_target_origin("browser-1", "workspace-1")
            .unwrap(),
        destination
    );

    let approved = registry
        .grant_browser_access(
            "browser-1",
            "workspace-1",
            &destination,
            BrowserOriginScope::Origin {
                origin: destination.clone(),
            },
            &[BrowserGrantCapability::BrowserControlOrigin],
        )
        .unwrap();
    assert!(!approved.agent_access.unwrap().paused);
    assert_eq!(
        registry
            .take_pending_navigation("browser-1", "workspace-1")
            .unwrap()
            .as_deref(),
        Some(destination_url)
    );
    assert!(registry
        .take_pending_navigation("browser-1", "workspace-1")
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn stop_and_takeover_make_agent_control_legible_and_recoverable() {
    let (registry, _) = ready_registry(true);
    let origin = BrowserOrigin::from_url("https://example.com").unwrap();
    registry
        .grant_browser_access(
            "browser-1",
            "workspace-1",
            &origin,
            BrowserOriginScope::Origin {
                origin: origin.clone(),
            },
            &[BrowserGrantCapability::BrowserControlOrigin],
        )
        .unwrap();
    let capability = registry.issue_agent_capability("workspace-1", "  Code   agent  ");
    let claimed = registry
        .begin_agent_control(capability, "browser-1")
        .unwrap();
    let controller = claimed.controller.unwrap();
    assert_eq!(controller.kind, BrowserControllerKind::Agent);
    assert_eq!(controller.label.as_deref(), Some("Code agent"));
    assert!(!controller.halted);

    let active = registry
        .set_agent_action(capability, "browser-1", Some("Clicking Continue"), false)
        .unwrap();
    assert_eq!(
        active.controller.unwrap().action.as_deref(),
        Some("Clicking Continue")
    );

    let stopped = registry
        .stop_agent_control("browser-1", "workspace-1")
        .await
        .unwrap();
    let controller = stopped.controller.unwrap();
    assert_eq!(controller.kind, BrowserControllerKind::Agent);
    assert!(controller.halted);
    assert!(controller.action.is_none());

    let human = registry
        .take_human_control("browser-1", "workspace-1")
        .await
        .unwrap();
    assert_eq!(human.controller.unwrap().kind, BrowserControllerKind::Human);
    assert!(!human.agent_access.unwrap().halted);

    let next_capability = registry.issue_agent_capability("workspace-1", "Next agent");
    let reclaimed = registry
        .begin_agent_control(next_capability, "browser-1")
        .unwrap();
    assert_eq!(
        reclaimed.controller.unwrap().label.as_deref(),
        Some("Next agent")
    );
}

#[test]
fn screenshot_snapshot_ids_are_validated_against_the_stored_snapshot() {
    let (registry, _) = ready_registry(true);
    registry
        .record_semantic_snapshot(
            "browser-1",
            "workspace-1",
            0,
            "snapshot-1".to_owned(),
            HashMap::from([("@e1".to_owned(), target("Continue"))]),
        )
        .unwrap();

    registry
        .validate_snapshot_id("browser-1", "workspace-1", "snapshot-1", 0)
        .expect("the live snapshot id validates");
    assert!(registry
        .validate_snapshot_id("browser-1", "workspace-1", "snapshot-forged", 0)
        .is_err());
    assert!(registry
        .validate_snapshot_id("browser-1", "workspace-1", "snapshot-1", 1)
        .is_err());
    assert!(registry
        .validate_snapshot_id("browser-1", "other-workspace", "snapshot-1", 0)
        .is_err());
}

// ── begin_agent_observation tests ─────────────────────────────

#[test]
fn begin_agent_observation_preserves_stored_snapshot_for_same_capability() {
    let (registry, _, _origin, capability, _private) = controlled_registry();
    // First acquire control via begin_agent_control (clears snapshot).
    let _ = registry
        .begin_agent_control(capability, "browser-1")
        .unwrap();
    // Then record a snapshot — this is what a real snapshot op does.
    registry
        .record_semantic_snapshot(
            "browser-1",
            "workspace-1",
            0,
            "snapshot-1".to_owned(),
            HashMap::from([("@e1".to_owned(), target("Continue"))]),
        )
        .unwrap();

    // begin_agent_observation must NOT clear the stored snapshot.
    let _snap = registry
        .begin_agent_observation(capability, "browser-1")
        .unwrap();

    registry
        .validate_snapshot_id("browser-1", "workspace-1", "snapshot-1", 0)
        .expect("snapshot must survive begin_agent_observation");
}

#[test]
fn begin_agent_observation_refuses_wrong_capability() {
    let (registry, _, _origin, capability, _private) = controlled_registry();
    let other = registry.issue_agent_capability("workspace-1", "Other");

    let result = registry.begin_agent_observation(other, "browser-1");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not controlled by this agent"));
    assert!(registry
        .begin_agent_observation(capability, "browser-1")
        .is_ok());
}

#[tokio::test]
async fn begin_agent_observation_refuses_human_takeover() {
    let (registry, _, _origin, capability, _private) = controlled_registry();
    // Human takes over — clears the agent controller.
    registry
        .take_human_control("browser-1", "workspace-1")
        .await
        .unwrap();

    // Observation must refuse: a human currently holds the browser.
    let result = registry.begin_agent_observation(capability, "browser-1");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not controlled by this agent"));
}

#[test]
fn begin_agent_observation_refuses_no_prior_control() {
    // ready_registry() registers a browser but issues no capability
    // and calls no begin_agent_control — the controller is Human.
    let (registry, _private) = ready_registry(true);
    let origin = BrowserOrigin::from_url("https://example.com").unwrap();
    registry
        .grant_browser_access(
            "browser-1",
            "workspace-1",
            &origin,
            BrowserOriginScope::Origin {
                origin: origin.clone(),
            },
            &[BrowserGrantCapability::BrowserControlOrigin],
        )
        .unwrap();
    let capability = registry.issue_agent_capability("workspace-1", "Code agent");
    let result = registry.begin_agent_observation(capability, "browser-1");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not controlled by this agent"));
}

#[tokio::test]
async fn begin_agent_observation_refuses_after_stop() {
    let (registry, _, _origin, capability, _private) = controlled_registry();
    let _ = registry
        .begin_agent_control(capability, "browser-1")
        .unwrap();
    registry
        .stop_agent_control("browser-1", "workspace-1")
        .await
        .unwrap();
    let result = registry.begin_agent_observation(capability, "browser-1");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("stopped"));
}

#[test]
fn begin_agent_observation_same_capability_continuation_preserves_snapshot() {
    let (registry, _, _origin, capability, _private) = controlled_registry();
    let _ = registry
        .begin_agent_control(capability, "browser-1")
        .unwrap();
    registry
        .record_semantic_snapshot(
            "browser-1",
            "workspace-1",
            0,
            "snap-2".to_owned(),
            HashMap::from([("@btn".to_owned(), target("Submit"))]),
        )
        .unwrap();

    // Same capability, observation continues.
    let snap = registry
        .begin_agent_observation(capability, "browser-1")
        .unwrap();
    assert_eq!(snap.controller.unwrap().kind, BrowserControllerKind::Agent);

    // Snapshot still validates.
    registry
        .validate_snapshot_id("browser-1", "workspace-1", "snap-2", 0)
        .expect("snapshot must persist across observation continuation");
}

#[tokio::test]
async fn observation_fence_and_halt_watch_fail_closed_on_stop() {
    let (registry, instance, _origin, capability, _private) = controlled_registry();

    let fence = registry.observation_fence(capability, "browser-1").unwrap();
    assert_eq!(fence.instance_id, instance);
    assert_eq!(fence.document_epoch, 0);

    let mut halt = registry.subscribe_halt("browser-1", "workspace-1").unwrap();
    assert!(!*halt.borrow_and_update());

    registry
        .stop_agent_control("browser-1", "workspace-1")
        .await
        .unwrap();
    assert!(*halt.borrow_and_update());
    assert!(registry.observation_fence(capability, "browser-1").is_err());
}

// ── Atomic completion regression tests ──────────────────────────

#[tokio::test]
async fn complete_semantic_snapshot_rejects_after_stop() {
    let (registry, instance, _origin, capability, _private) = controlled_registry();
    registry
        .stop_agent_control("browser-1", "workspace-1")
        .await
        .unwrap();

    let error = registry
        .complete_semantic_snapshot(
            capability,
            "browser-1",
            instance,
            0,
            "snapshot-1".to_owned(),
            HashMap::new(),
        )
        .unwrap_err();
    assert!(
        error.contains("stopped by the user") || error.contains("capability is unavailable"),
        "expected Stop to block completion, got: {error}"
    );
}

#[tokio::test]
async fn complete_semantic_snapshot_rejects_wrong_instance() {
    let (registry, instance, _origin, capability, _private) = controlled_registry();
    // The instance was registered as `instance`, but we pass a different value.
    let error = registry
        .complete_semantic_snapshot(
            capability,
            "browser-1",
            instance + 1, // intentionally wrong instance
            0,
            "snapshot-1".to_owned(),
            HashMap::new(),
        )
        .unwrap_err();
    assert!(
        error.contains("document changed while it was being inspected"),
        "expected instance-id fence to reject, got: {error}"
    );
}

#[tokio::test]
async fn complete_semantic_snapshot_rejects_wrong_document_epoch() {
    let (registry, instance, _origin, capability, _private) = controlled_registry();
    let error = registry
        .complete_semantic_snapshot(
            capability,
            "browser-1",
            instance,
            1, // wrong epoch
            "snapshot-1".to_owned(),
            HashMap::new(),
        )
        .unwrap_err();
    assert!(
        error.contains("document changed while it was being inspected"),
        "expected epoch fence to reject, got: {error}"
    );
}

#[tokio::test]
async fn complete_semantic_snapshot_rejects_when_hidden() {
    let (registry, instance, _origin, capability, _private) = controlled_registry();
    registry
        .set_visible("browser-1", "workspace-1", false)
        .unwrap();

    let error = registry
        .complete_semantic_snapshot(
            capability,
            "browser-1",
            instance,
            0,
            "snapshot-1".to_owned(),
            HashMap::new(),
        )
        .unwrap_err();
    assert!(
        error.contains("hidden"),
        "expected visibility check to reject, got: {error}"
    );
}

#[tokio::test]
async fn complete_semantic_snapshot_rejects_revoked_capability() {
    let (registry, instance, _origin, capability, _private) = controlled_registry();
    registry.revoke_agent_capability(capability);

    let error = registry
        .complete_semantic_snapshot(
            capability,
            "browser-1",
            instance,
            0,
            "snapshot-1".to_owned(),
            HashMap::new(),
        )
        .unwrap_err();
    assert!(
        error.contains("capability is unavailable"),
        "expected revoked capability to reject, got: {error}"
    );
}

#[tokio::test]
async fn complete_semantic_snapshot_rejects_wrong_controller() {
    let (registry, instance, _origin, _original_capability, _private) = controlled_registry();
    // Issue a second capability that never began control.
    let other_capability = registry.issue_agent_capability("workspace-1", "Other agent");

    let error = registry
        .complete_semantic_snapshot(
            other_capability,
            "browser-1",
            instance,
            0,
            "snapshot-1".to_owned(),
            HashMap::new(),
        )
        .unwrap_err();
    assert!(
        error.contains("not controlled by this agent"),
        "expected controller check to reject, got: {error}"
    );
}

#[tokio::test]
async fn complete_screenshot_recording_rejects_after_stop() {
    let (registry, instance, _origin, capability, _private) = controlled_registry();
    // Plant a stored snapshot so screenshot recording has something to validate.
    registry
        .record_semantic_snapshot(
            "browser-1",
            "workspace-1",
            0,
            "snapshot-1".to_owned(),
            HashMap::new(),
        )
        .unwrap();

    registry
        .stop_agent_control("browser-1", "workspace-1")
        .await
        .unwrap();

    let error = registry
        .complete_screenshot_recording(capability, "browser-1", instance, 0, "snapshot-1")
        .unwrap_err();
    assert!(
        error.contains("stopped by the user") || error.contains("capability is unavailable"),
        "expected Stop to block screenshot recording, got: {error}"
    );
}

#[test]
fn complete_screenshot_recording_rejects_wrong_instance() {
    let (registry, instance, _origin, capability, _private) = controlled_registry();
    registry
        .record_semantic_snapshot(
            "browser-1",
            "workspace-1",
            0,
            "snapshot-1".to_owned(),
            HashMap::new(),
        )
        .unwrap();

    let error = registry
        .complete_screenshot_recording(
            capability,
            "browser-1",
            instance + 1, // intentionally wrong
            0,
            "snapshot-1",
        )
        .unwrap_err();
    assert!(
        error.contains("document changed while screenshot"),
        "expected instance-id fence to reject, got: {error}"
    );
}

#[test]
fn complete_screenshot_recording_rejects_forged_snapshot_id() {
    let (registry, instance, _origin, capability, _private) = controlled_registry();
    registry
        .record_semantic_snapshot(
            "browser-1",
            "workspace-1",
            0,
            "snapshot-1".to_owned(),
            HashMap::new(),
        )
        .unwrap();

    let error = registry
        .complete_screenshot_recording(capability, "browser-1", instance, 0, "snapshot-forged")
        .unwrap_err();
    assert!(
        error.contains("snapshot is stale"),
        "expected forged snapshot id to reject, got: {error}"
    );
}

#[test]
fn complete_screenshot_recording_rejects_missing_snapshot() {
    let (registry, instance, _origin, capability, _private) = controlled_registry();
    // No record_semantic_snapshot call — there is no stored snapshot.

    let error = registry
        .complete_screenshot_recording(capability, "browser-1", instance, 0, "snapshot-1")
        .unwrap_err();
    assert!(
        error.contains("snapshot is stale"),
        "expected missing stored snapshot to reject, got: {error}"
    );
}
