//! The gateway registration lifecycle: what the registry establishes, when it
//! pushes a revision, and what happens when the gateway cannot or will not
//! hold the app.
//!
//! The relay itself is covered in [`super::app_invoke`]; this module drives
//! the half that runs before a relay is possible at all.

use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

use openwave_core::id::{AppId, AppRevisionId};
use openwave_core::local_app::{
    app_revision_relative_path, AppBinding, AppGatewayOperationsBinding, AppManifest, CreateApp,
    NewAppRevision,
};

use base64::Engine as _;

use crate::connected_apps::{GatewayConsentRelay, GatewayDraftSource, GatewayRegistration};
use crate::connectors::{GatewayConsentOutcome, GatewayRegistrationOutcome};
use crate::gateway_drafts::{GatewayDraftClient, GatewayDraftRegistry, SharedAppProjection};

use super::*;

const GATEWAY_A: &str = "https://gateway-a.internal.example.com/";
const GATEWAY_B: &str = "https://gateway-b.internal.example.com/";

/// A gateway that answers registration calls from a script, recording every
/// call so a test can assert what crossed and in what order.
#[derive(Default)]
struct FakeGateway {
    calls: StdMutex<Vec<String>>,
    creates: StdMutex<VecDeque<Option<GatewayRegistrationOutcome>>>,
    appends: StdMutex<VecDeque<Option<GatewayRegistrationOutcome>>>,
    consents: StdMutex<VecDeque<Option<GatewayConsentOutcome>>>,
    projections: StdMutex<Vec<SharedAppProjection>>,
}

impl FakeGateway {
    fn registered(id: &str, revision: &str) -> Option<GatewayRegistrationOutcome> {
        Some(GatewayRegistrationOutcome::Registered {
            shared_app_id: id.to_owned(),
            revision_id: revision.to_owned(),
        })
    }

    fn creates(
        self,
        answers: impl IntoIterator<Item = Option<GatewayRegistrationOutcome>>,
    ) -> Self {
        *self.creates.lock().unwrap() = answers.into_iter().collect();
        self
    }

    fn appends(
        self,
        answers: impl IntoIterator<Item = Option<GatewayRegistrationOutcome>>,
    ) -> Self {
        *self.appends.lock().unwrap() = answers.into_iter().collect();
        self
    }

    fn consents(self, answers: impl IntoIterator<Item = Option<GatewayConsentOutcome>>) -> Self {
        *self.consents.lock().unwrap() = answers.into_iter().collect();
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl GatewayDraftClient for FakeGateway {
    async fn create(
        &self,
        gateway_base_url: &str,
        slug: Option<&str>,
        projection: &SharedAppProjection,
    ) -> openwave_core::Result<Option<GatewayRegistrationOutcome>> {
        self.calls.lock().unwrap().push(format!(
            "create {gateway_base_url} slug={}",
            slug.unwrap_or("-")
        ));
        self.projections.lock().unwrap().push(projection.clone());
        Ok(self
            .creates
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Self::registered("shared-1", "gw-rev-1")))
    }

    async fn append(
        &self,
        gateway_base_url: &str,
        shared_app_id: &str,
        projection: &SharedAppProjection,
    ) -> openwave_core::Result<Option<GatewayRegistrationOutcome>> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("append {gateway_base_url} {shared_app_id}"));
        self.projections.lock().unwrap().push(projection.clone());
        Ok(self
            .appends
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Self::registered(shared_app_id, "gw-rev-2")))
    }

    async fn consent(
        &self,
        gateway_base_url: &str,
        shared_app_id: &str,
        revision_id: &str,
    ) -> openwave_core::Result<Option<GatewayConsentOutcome>> {
        self.calls.lock().unwrap().push(format!(
            "consent {gateway_base_url} {shared_app_id} {revision_id}"
        ));
        Ok(self
            .consents
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Some(GatewayConsentOutcome::Consented)))
    }
}

/// A profile holding one app that binds one gateway app, with its bundle
/// bytes published where the projection reads them.
struct Fixture {
    _dir: tempfile::TempDir,
    store: Arc<dyn Store>,
    data_dir: std::path::PathBuf,
    app_id: AppId,
}

impl Fixture {
    async fn new() -> Self {
        let (dir, store) = temp_db_store("gateway-drafts.db").await;
        let store: Arc<dyn Store> = Arc::new(store);
        let data_dir = dir.path().to_path_buf();
        let app_id = AppId::new();
        let revision_id = AppRevisionId::new();
        publish_bundle(&data_dir, app_id, revision_id, b"<html>one</html>");
        store
            .create_app(&CreateApp {
                id: app_id,
                revision: revision(revision_id, b"<html>one</html>"),
            })
            .await
            .unwrap();
        Self {
            _dir: dir,
            store,
            data_dir,
            app_id,
        }
    }

    fn registry(&self, gateway: Arc<FakeGateway>) -> GatewayDraftRegistry {
        GatewayDraftRegistry::new(self.store.clone(), self.data_dir.clone(), gateway)
    }

    /// Append a fresh local revision, as an app edit would.
    async fn revise(&self, bundle: &[u8]) -> AppRevisionId {
        let revision_id = AppRevisionId::new();
        publish_bundle(&self.data_dir, self.app_id, revision_id, bundle);
        self.store
            .append_app_revision(self.app_id, &revision(revision_id, bundle))
            .await
            .unwrap();
        revision_id
    }

    async fn held(
        &self,
        gateway_base_url: &str,
    ) -> Option<openwave_core::local_app::AppGatewayDraft> {
        self.store
            .get_app_gateway_draft(self.app_id, gateway_base_url)
            .await
            .unwrap()
    }
}

fn publish_bundle(
    data_dir: &std::path::Path,
    app_id: AppId,
    revision_id: AppRevisionId,
    bundle: &[u8],
) {
    let path = data_dir.join(app_revision_relative_path(app_id, revision_id));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bundle).unwrap();
}

fn revision(id: AppRevisionId, bundle: &[u8]) -> NewAppRevision {
    use sha2::Digest as _;

    NewAppRevision {
        id,
        manifest: AppManifest {
            name: "Issue triage".into(),
            bindings: vec![AppBinding::GatewayOperations(AppGatewayOperationsBinding {
                gateway_app: "gw-issues".into(),
                operation_ids: vec!["listIssues".into()],
            })],
        },
        byte_len: bundle.len() as u64,
        sha256: sha2::Sha256::digest(bundle).into(),
        turn_id: None,
        producing_run_id: None,
        chat_id: None,
        created_at: chrono::Utc::now(),
    }
}

fn shared_app_id(registration: &GatewayRegistration) -> &str {
    match registration {
        GatewayRegistration::Registered { shared_app_id, .. } => shared_app_id,
        other => panic!("expected a registration, got {other:?}"),
    }
}

/// The first relay of a granted app is what registers it, and the mapping it
/// leaves is what keeps every later relay from registering again. A slug the
/// deployment already holds is the one refusal worth asking about differently
/// — once, under a name carrying the app's own identity.
#[tokio::test]
async fn a_first_registration_persists_and_retries_a_taken_slug_once() {
    let fixture = Fixture::new().await;
    let gateway = Arc::new(FakeGateway::default().creates([
        Some(GatewayRegistrationOutcome::SlugTaken),
        FakeGateway::registered("shared-1", "gw-rev-1"),
    ]));
    let registry = fixture.registry(gateway.clone());

    let registration = registry
        .ensure_registered(fixture.app_id, GATEWAY_A)
        .await
        .unwrap();
    assert_eq!(shared_app_id(&registration), "shared-1");

    let calls = gateway.calls();
    assert_eq!(
        calls.len(),
        2,
        "a taken slug is retried exactly once: {calls:?}"
    );
    assert!(
        calls[0].ends_with("slug=-"),
        "the gateway derives the first slug: {calls:?}"
    );
    let identity = fixture.app_id.0.simple().to_string();
    assert!(
        calls[1].ends_with(&format!("slug=issue-triage-{}", &identity[..8])),
        "the retry names a slug the app's own identity makes unique: {calls:?}"
    );

    let held = fixture
        .held(GATEWAY_A)
        .await
        .expect("the mapping is durable");
    assert_eq!(held.shared_app_id, "shared-1");
    assert_eq!(held.gateway_revision_id, "gw-rev-1");

    // A second relay reads the mapping rather than registering again.
    registry
        .ensure_registered(fixture.app_id, GATEWAY_A)
        .await
        .unwrap();
    assert_eq!(
        gateway.calls().len(),
        2,
        "a registered app is not registered twice"
    );
}

/// A registration belongs to one deployment. A profile re-paired elsewhere
/// holds nothing there and registers afresh — it must never relay a consented
/// invoke at a shared app another gateway minted.
#[tokio::test]
async fn a_registration_at_one_gateway_never_answers_for_another() {
    let fixture = Fixture::new().await;
    let gateway = Arc::new(FakeGateway::default().creates([
        FakeGateway::registered("shared-a", "gw-rev-a"),
        FakeGateway::registered("shared-b", "gw-rev-b"),
    ]));
    let registry = fixture.registry(gateway.clone());

    let first = registry
        .ensure_registered(fixture.app_id, GATEWAY_A)
        .await
        .unwrap();
    let second = registry
        .ensure_registered(fixture.app_id, GATEWAY_B)
        .await
        .unwrap();

    assert_eq!(shared_app_id(&first), "shared-a");
    assert_eq!(shared_app_id(&second), "shared-b");
    assert_eq!(
        fixture.held(GATEWAY_A).await.unwrap().shared_app_id,
        "shared-a",
        "registering elsewhere must not move the first deployment's mapping"
    );
}

/// The revision sync is lazy: an app that has been edited since its last
/// relay pushes the revision it is about to serve, and only that one.
#[tokio::test]
async fn an_edited_app_pushes_its_current_revision_before_the_next_relay() {
    let fixture = Fixture::new().await;
    let gateway = Arc::new(FakeGateway::default());
    let registry = fixture.registry(gateway.clone());
    registry
        .ensure_registered(fixture.app_id, GATEWAY_A)
        .await
        .unwrap();

    // Two local edits; only the current one is ever pushed.
    fixture.revise(b"<html>two</html>").await;
    let current = fixture.revise(b"<html>three</html>").await;
    let registration = registry
        .ensure_registered(fixture.app_id, GATEWAY_A)
        .await
        .unwrap();

    assert_eq!(shared_app_id(&registration), "shared-1");
    let calls = gateway.calls();
    assert_eq!(
        calls.len(),
        2,
        "one create and one append — the skipped revision is never pushed: {calls:?}"
    );
    assert!(calls[1].starts_with("append"), "{calls:?}");
    let pushed = gateway.projections.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&pushed.bundle_base64)
            .unwrap(),
        b"<html>three</html>",
        "the pushed bundle is the revision about to be served"
    );
    let held = fixture.held(GATEWAY_A).await.unwrap();
    assert_eq!(held.synced_revision_id, current);
    assert_eq!(held.gateway_revision_id, "gw-rev-2");
}

/// A deployment that does not serve shared-app registration leaves the app
/// unregistered rather than half-registered: nothing is persisted, so the next
/// attempt is a first attempt again.
#[tokio::test]
async fn a_gateway_that_cannot_hold_the_app_records_no_mapping() {
    let fixture = Fixture::new().await;
    let gateway = Arc::new(FakeGateway::default().creates([None]));
    let registry = fixture.registry(gateway.clone());

    let registration = registry
        .ensure_registered(fixture.app_id, GATEWAY_A)
        .await
        .unwrap();
    assert_eq!(registration, GatewayRegistration::NotRegistered);
    assert!(fixture.held(GATEWAY_A).await.is_none());
}

/// Consent names a revision. When the gateway reports the pin has moved, the
/// registration is re-established from the current local revision and consent
/// is re-stated against what the gateway now serves — once.
#[tokio::test]
async fn a_moved_revision_pin_is_re_synced_before_consent_is_relayed_again() {
    let fixture = Fixture::new().await;
    let gateway = Arc::new(
        FakeGateway::default()
            .consents([
                Some(GatewayConsentOutcome::RevisionMoved),
                Some(GatewayConsentOutcome::Consented),
            ])
            .appends([FakeGateway::registered("shared-1", "gw-rev-9")]),
    );
    let registry = fixture.registry(gateway.clone());

    let relayed = registry
        .relay_consent(fixture.app_id, GATEWAY_A)
        .await
        .unwrap();

    assert_eq!(relayed, GatewayConsentRelay::Consented);
    let calls = gateway.calls();
    assert_eq!(
        calls
            .iter()
            .map(|call| call.split(' ').next().unwrap())
            .collect::<Vec<_>>(),
        ["create", "consent", "append", "consent"],
        "{calls:?}"
    );
    assert!(
        calls[3].ends_with("gw-rev-9"),
        "the second consent pins what the gateway now serves: {calls:?}"
    );
}

/// The grant is a local decision. Registration runs after it is durable and
/// is allowed to fail: a gateway that is down, unreachable, or too old must
/// never cost the user their consent — the first invoke registers instead.
#[tokio::test]
async fn a_grant_survives_a_registration_the_gateway_refuses() {
    struct UnreachableGateway;

    #[async_trait::async_trait]
    impl GatewayDraftSource for UnreachableGateway {
        async fn ensure_registered(
            &self,
            _app: AppId,
            _gateway_base_url: &str,
        ) -> openwave_core::Result<GatewayRegistration> {
            Err(openwave_core::AgentError::Store(
                "the gateway is down".into(),
            ))
        }

        async fn relay_consent(
            &self,
            _app: AppId,
            _gateway_base_url: &str,
        ) -> openwave_core::Result<GatewayConsentRelay> {
            Err(openwave_core::AgentError::Store(
                "the gateway is down".into(),
            ))
        }
    }

    let (dir, store) = temp_db_store("gateway-drafts-grant.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    state.gateway_catalogs = Arc::new(super::app_grant::FakeGatewayCatalogs::signed_in(
        "https://gateway.internal.example.com",
        &[("gw-issues", "Issues (gateway)", &["listIssues"])],
    ));
    state.gateway_drafts = Arc::new(UnreachableGateway);
    let bearer = format!("Bearer {}", state.token);
    let router = app(state);

    let app_id = AppId::new();
    store
        .create_app(&CreateApp {
            id: app_id,
            revision: revision(AppRevisionId::new(), b"<html>one</html>"),
        })
        .await
        .unwrap();

    let consented = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/apps/{app_id}/grant"))
                .header("authorization", &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(consented.status(), StatusCode::OK);
    assert!(
        store.get_app_grant(app_id).await.unwrap().is_some(),
        "the grant is durable even though nothing could be registered"
    );
}
