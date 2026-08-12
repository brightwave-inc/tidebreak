//! `POST /apps/{id}/gateway-page` — the address an author is sent to in order
//! to share an app, and the answers that come back instead when there is none.

use std::sync::atomic::{AtomicUsize, Ordering};

use openwave_core::id::{AppId, AppRevisionId};
use openwave_core::local_app::{
    AppBinding, AppGatewayOperationsBinding, AppManifest, CreateApp, NewAppRevision,
};

use crate::connected_apps::{GatewayConsentRelay, GatewayDraftSource, GatewayRegistration};

use super::*;

/// A registration seam that answers as scripted and counts what reaches it.
struct ScriptedDrafts {
    reached: AtomicUsize,
    answer: fn() -> GatewayRegistration,
}

impl ScriptedDrafts {
    fn new(answer: fn() -> GatewayRegistration) -> Self {
        Self {
            reached: AtomicUsize::new(0),
            answer,
        }
    }
}

#[async_trait::async_trait]
impl GatewayDraftSource for ScriptedDrafts {
    async fn ensure_registered(
        &self,
        _app: AppId,
        _gateway_base_url: &str,
    ) -> openwave_core::Result<GatewayRegistration> {
        self.reached.fetch_add(1, Ordering::SeqCst);
        Ok((self.answer)())
    }

    async fn relay_consent(
        &self,
        _app: AppId,
        _gateway_base_url: &str,
    ) -> openwave_core::Result<GatewayConsentRelay> {
        self.reached.fetch_add(1, Ordering::SeqCst);
        Ok(GatewayConsentRelay::NotRegistered)
    }
}

/// A gateway-bound app, so the route gets past the binding check.
async fn gateway_bound_app(store: &Arc<dyn Store>) -> AppId {
    let app_id = AppId::new();
    store
        .create_app(&CreateApp {
            id: app_id,
            revision: NewAppRevision {
                id: AppRevisionId::new(),
                manifest: AppManifest {
                    name: "Issue triage".into(),
                    bindings: vec![AppBinding::GatewayOperations(AppGatewayOperationsBinding {
                        gateway_app: "gw-issues".into(),
                        operation_ids: vec!["listIssues".into()],
                    })],
                },
                byte_len: 1,
                sha256: [0; 32],
                turn_id: None,
                producing_run_id: None,
                chat_id: None,
                created_at: chrono::Utc::now(),
            },
        })
        .await
        .unwrap();
    app_id
}

fn state_with(dir: &std::path::Path, store: Arc<dyn Store>) -> AppState {
    AppState::new(
        Config::desktop(dir),
        store,
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    )
}

/// A profile with no model gateway has no page to open, and says so as an
/// answer the renderer can render — not a 500, and not a registration that
/// goes out anyway. The seam is counted rather than assumed untouched:
/// registering against a deployment this profile is not paired with would
/// leave a draft somewhere nothing here will ever read again.
#[tokio::test]
async fn a_profile_without_a_gateway_has_no_page_and_registers_nothing() {
    let (dir, store) = temp_db_store("app-gateway-page.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let mut state = state_with(dir.path(), store.clone());
    let drafts = Arc::new(ScriptedDrafts::new(|| GatewayRegistration::NotRegistered));
    state.gateway_drafts = drafts.clone();
    let bearer = format!("Bearer {}", state.token);
    let router = app(state);

    let app_id = gateway_bound_app(&store).await;
    let ask = |id: AppId| {
        router.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/apps/{id}/gateway-page"))
                .header("authorization", &bearer)
                .body(Body::empty())
                .unwrap(),
        )
    };

    let response = ask(app_id).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let answer: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(answer["outcome"], serde_json::json!("no_gateway"));
    assert!(answer.get("url").is_none(), "no gateway, no address");

    // An app that does not exist is a bad request about local state, not an
    // answer about the gateway.
    assert_eq!(
        ask(AppId::new()).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        drafts.reached.load(Ordering::SeqCst),
        0,
        "an unpaired profile must not reach the gateway at all"
    );
}

/// The whole point of the route: a registered app resolves to its page at the
/// deployment holding it, assembled from the managed policy's gateway URL and
/// the gateway's own id for the app. This is the address the Publish
/// affordance opens, so a wrong shape here sends every author to a 404.
#[tokio::test]
async fn a_registered_app_resolves_to_its_page_at_the_gateway() {
    let (dir, store) = temp_db_store("app-gateway-page-ready.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    crate::managed_policy::provision(&*store, "https://gateway.internal.example.com")
        .await
        .unwrap();
    let mut state = state_with(dir.path(), store.clone());
    state.gateway_drafts = Arc::new(ScriptedDrafts::new(|| GatewayRegistration::Registered {
        shared_app_id: "sa-42".into(),
        revision_id: "rev-1".into(),
    }));
    let bearer = format!("Bearer {}", state.token);
    let app_id = gateway_bound_app(&store).await;

    let response = app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/apps/{app_id}/gateway-page"))
                .header("authorization", &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({
            "outcome": "ready",
            "url": "https://gateway.internal.example.com/shared-apps/sa-42",
        }),
    );
}

/// A gateway that will not hold the app answers in its own words, and the
/// answer carries no address: sending an author to a page for a draft the
/// gateway just refused would strand them there.
#[tokio::test]
async fn a_refused_registration_answers_in_the_gateways_words() {
    let (dir, store) = temp_db_store("app-gateway-page-refused.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    crate::managed_policy::provision(&*store, "https://gateway.internal.example.com")
        .await
        .unwrap();
    let mut state = state_with(dir.path(), store.clone());
    state.gateway_drafts = Arc::new(ScriptedDrafts::new(|| GatewayRegistration::Refused {
        message: "bundle calls files.read, which this gateway cannot serve".into(),
    }));
    let bearer = format!("Bearer {}", state.token);
    let app_id = gateway_bound_app(&store).await;

    let response = app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/apps/{app_id}/gateway-page"))
                .header("authorization", &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let answer: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(answer["outcome"], serde_json::json!("refused"));
    assert_eq!(
        answer["message"],
        serde_json::json!("bundle calls files.read, which this gateway cannot serve"),
    );
    assert!(answer.get("url").is_none(), "a refusal is not an address");
}
