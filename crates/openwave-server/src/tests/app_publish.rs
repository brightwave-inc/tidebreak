//! `POST /apps/{id}/publish` — the half of publishing that runs before the
//! gateway is ever asked.

use std::sync::atomic::{AtomicUsize, Ordering};

use openwave_core::id::{AppId, AppRevisionId};
use openwave_core::local_app::{
    AppBinding, AppGatewayOperationsBinding, AppManifest, CreateApp, NewAppRevision,
};

use crate::connected_apps::{
    GatewayConsentRelay, GatewayDraftSource, GatewayPublish, GatewayRegistration,
};

use super::*;

/// A registration seam that records anything reaching it and answers nothing.
#[derive(Default)]
struct CountingDrafts {
    reached: AtomicUsize,
}

#[async_trait::async_trait]
impl GatewayDraftSource for CountingDrafts {
    async fn ensure_registered(
        &self,
        _app: AppId,
        _gateway_base_url: &str,
    ) -> openwave_core::Result<GatewayRegistration> {
        self.reached.fetch_add(1, Ordering::SeqCst);
        Ok(GatewayRegistration::NotRegistered)
    }

    async fn relay_consent(
        &self,
        _app: AppId,
        _gateway_base_url: &str,
    ) -> openwave_core::Result<GatewayConsentRelay> {
        self.reached.fetch_add(1, Ordering::SeqCst);
        Ok(GatewayConsentRelay::NotRegistered)
    }

    async fn publish(
        &self,
        _app: AppId,
        _gateway_base_url: &str,
        _team_id: &str,
    ) -> openwave_core::Result<GatewayPublish> {
        self.reached.fetch_add(1, Ordering::SeqCst);
        Ok(GatewayPublish::Published)
    }
}

/// A profile with no model gateway has nowhere to publish, and says so as an
/// answer the renderer can render — not a 500, and not a call that goes out
/// anyway. The seam is counted rather than assumed untouched: publishing to a
/// deployment this profile is not paired with would register the app somewhere
/// nothing here will ever read again.
#[tokio::test]
async fn publishing_without_a_gateway_answers_and_reaches_nothing() {
    let (dir, store) = temp_db_store("app-publish.db").await;
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
    let drafts = Arc::new(CountingDrafts::default());
    state.gateway_drafts = drafts.clone();
    let bearer = format!("Bearer {}", state.token);
    let router = app(state);

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

    let publish = |id: AppId| {
        router.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/apps/{id}/publish"))
                .header("authorization", &bearer)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"team_id":"team-7"}"#))
                .unwrap(),
        )
    };

    let response = publish(app_id).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let answer: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(answer["outcome"], serde_json::json!("no_gateway"));

    // An app that does not exist is a bad request about local state, not a
    // publish answer.
    assert_eq!(
        publish(AppId::new()).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        drafts.reached.load(Ordering::SeqCst),
        0,
        "an unpaired profile must not reach the gateway at all"
    );
}

/// A managed profile nobody has signed into yet has no session to read teams
/// with — and cannot publish either way. That is an answer, not a failure: the
/// app page reads this on every visit, so surfacing a missing sign-in as an
/// error would log one per page view for a state the endpoint can state
/// calmly, and the affordance would be hidden regardless.
#[tokio::test]
async fn the_teams_read_answers_calmly_while_signed_out() {
    let (dir, store) = temp_db_store("app-publish-teams.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    crate::managed_policy::provision(&*store, "https://gateway.internal.example.com")
        .await
        .unwrap();
    let state = AppState::new(
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
    let bearer = format!("Bearer {}", state.token);
    let response = app(state)
        .oneshot(
            Request::builder()
                .uri("/gateway/teams")
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
        serde_json::json!({ "supported": false, "teams": [] }),
    );
}
