//! Memory route contracts: owner isolation, review lifecycle, and honest
//! capability responses.

use super::*;

use crate::principal::{AuthContext, Principal, Role, UserId};
use axum::http::Request;
use axum::response::Response;
use serde_json::json;
use std::path::Path;
use tidebreak_core::memory::{
    MemoryAuthor, MemoryBackend, MemoryKind, MemoryRecord, MemoryRecordId, MemoryScope,
    MemoryStatus,
};
use tidebreak_core::OwnerId;

/// State over one database, with memory routes bound to that same database
/// the way production boot binds them.
fn state_with_memory(
    config: Config,
    db: Arc<tidebreak_core::DbStore>,
    agent_config: AgentConfig,
) -> AppState {
    let mut state = AppState::new(
        config,
        db.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        agent_config,
    );
    state.memory = Some(db);
    state
}

async fn memory_app() -> (
    Router,
    Arc<str>,
    Arc<tidebreak_core::DbStore>,
    tempfile::TempDir,
) {
    let (dir, store) = temp_db_store("memory-routes.db").await;
    let db = Arc::new(store);
    let state = state_with_memory(
        Config::desktop(dir.path()),
        db.clone(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    (app(state), token, db, dir)
}

fn user_record(id: MemoryRecordId) -> MemoryRecord {
    MemoryRecord {
        id,
        scope: MemoryScope::Personal,
        kind: MemoryKind::Lesson,
        status: MemoryStatus::Active,
        title: "When changing database migrations".to_owned(),
        body: "Run the migration chain test before publishing.".to_owned(),
        provenance: tidebreak_core::MemoryProvenance {
            author: MemoryAuthor::User,
            origin: Default::default(),
            evidence: Vec::new(),
        },
        links: Vec::new(),
        expires_at: None,
        superseded_by: None,
        observation_count: 0,
        revision: 1,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

async fn request(router: &Router, bearer: &str, method: &str, uri: String) -> Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn request_json(
    router: &Router,
    bearer: &str,
    method: &str,
    uri: String,
    body: serde_json::Value,
) -> Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn assert_ok(response: Response) -> serde_json::Value {
    let status = response.status();
    let body: serde_json::Value = json_body(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn memory_routes_isolate_records_by_owner() {
    let (_router, _token, store, _dir) = memory_app().await;
    let alice = OwnerId::new("user:alice").unwrap();
    let id = MemoryRecordId::new();

    store.put(&alice, user_record(id)).await.unwrap();

    let bob_router = Router::new()
        .route("/memory/records", get(crate::routes::list_records))
        .route(
            "/memory/records/{id}",
            get(crate::routes::get_record).delete(crate::routes::delete_record),
        )
        .with_state(state_with_memory(
            Config::desktop(Path::new("/tmp/tidebreak-memory-test")),
            tidebreak_core::DbStore::connect("sqlite::memory:")
                .await
                .unwrap()
                .into(),
            AgentConfig::default(),
        ))
        .layer(axum::middleware::from_fn(
            |mut request: Request<Body>, next: axum::middleware::Next| async move {
                request.extensions_mut().insert(AuthContext {
                    principal: Principal::User {
                        id: UserId::new("bob").unwrap(),
                        role: Role::Member,
                    },
                    client_executor: false,
                });
                next.run(request).await
            },
        ));

    let listed =
        assert_ok(request(&bob_router, "ignored", "GET", "/memory/records".into()).await).await;
    assert_eq!(listed, json!([]));
    let detail = request(
        &bob_router,
        "ignored",
        "GET",
        format!("/memory/records/{id}"),
    )
    .await;
    assert_eq!(detail.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn memory_routes_reject_bad_bodies_and_id_reuse() {
    let (router, token, _store, _dir) = memory_app().await;
    let bearer = format!("Bearer {token}");
    let id = MemoryRecordId::new();

    let invalid = json!({
        "id": id,
        "kind": "lesson",
        "status": "active",
        "title": "",
        "body": "Run the migration chain test before publishing.",
        "author": "user"
    });
    let rejected = request_json(&router, &bearer, "POST", "/memory/records".into(), invalid).await;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

    let valid = json!({
        "id": id,
        "kind": "lesson",
        "status": "active",
        "title": "When changing database migrations",
        "body": "Run the migration chain test before publishing.",
        "author": "user"
    });
    let created = request_json(&router, &bearer, "POST", "/memory/records".into(), valid).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let replay = request_json(
        &router,
        &bearer,
        "POST",
        "/memory/records".into(),
        json!({
            "id": id,
            "kind": "lesson",
            "status": "active",
            "title": "Duplicate",
            "body": "Duplicate.",
            "author": "user"
        }),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn memory_routes_move_proposals_and_report_revision_conflicts() {
    let (router, token, store, _dir) = memory_app().await;
    let bearer = format!("Bearer {token}");
    let id = MemoryRecordId::new();
    let mut record = user_record(id);
    record.status = MemoryStatus::Proposed;
    store.put(&OwnerId::local(), record).await.unwrap();

    let stale = json!({"expected_revision": 1, "status": "active"});
    let activation = request_json(
        &router,
        &bearer,
        "PUT",
        format!("/memory/records/{id}/status"),
        stale,
    )
    .await;
    assert_eq!(activation.status(), StatusCode::OK);
    let activated: serde_json::Value = json_body(activation).await;
    assert_eq!(activated["status"], json!("active"));
    assert_eq!(activated["revision"], json!(2));

    let conflict = json!({"expected_revision": 1, "status": "archived"});
    let stale_activation = request_json(
        &router,
        &bearer,
        "PUT",
        format!("/memory/records/{id}/status"),
        conflict,
    )
    .await;
    assert_eq!(stale_activation.status(), StatusCode::CONFLICT);
    let error: serde_json::Value = json_body(stale_activation).await;
    assert_eq!(error["kind"], json!("memory_revision_conflict"));
}

#[tokio::test]
async fn memory_search_digest_and_history_read_only_owner_rows() {
    let (router, token, store, _dir) = memory_app().await;
    let bearer = format!("Bearer {token}");
    let id = MemoryRecordId::new();
    store.put(&OwnerId::local(), user_record(id)).await.unwrap();

    let search = assert_ok(
        request(
            &router,
            &bearer,
            "GET",
            "/memory/search?query=migration&limit=10".into(),
        )
        .await,
    )
    .await;
    assert_eq!(search[0]["record_id"], json!(id));

    let digest = assert_ok(request(&router, &bearer, "GET", "/memory/digest".into()).await).await;
    assert_eq!(digest["record_count"], json!(1));
    assert!(digest["markdown"]
        .as_str()
        .unwrap()
        .contains("When changing database migrations"));

    let history = assert_ok(
        request(
            &router,
            &bearer,
            "GET",
            format!("/memory/records/{id}/revisions"),
        )
        .await,
    )
    .await;
    assert_eq!(history[0]["snapshot"]["id"], json!(id));
}

#[tokio::test]
async fn memory_ingest_answers_not_implemented_for_the_default_backend() {
    let (router, token, _store, _dir) = memory_app().await;
    let bearer = format!("Bearer {token}");
    let response = request_json(
        &router,
        &bearer,
        "POST",
        "/memory/ingest".into(),
        json!({
            "scope": {"kind": "personal"},
            "content": "Always run the release smoke test."
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    let error: serde_json::Value = json_body(response).await;
    assert_eq!(error["kind"], json!("not_implemented"));
}

#[tokio::test]
async fn memory_sweep_status_serves_the_recorded_last_run() {
    let (router, token, store, _dir) = memory_app().await;
    let bearer = format!("Bearer {token}");

    let before =
        assert_ok(request(&router, &bearer, "GET", "/memory/sweep".into()).await).await;
    assert_eq!(before["last_run"], json!(null));

    let run = tidebreak_core::MemorySweepRun {
        ran_at: chrono::Utc::now(),
        scope: Some(MemoryScope::Personal),
        outcome: tidebreak_core::MemorySweepOutcome::NoModel,
        expired: 2,
        proposed: 0,
    };
    tidebreak_core::db::memory_sweep::record_sweep_run(&store, &OwnerId::local(), &run)
        .await
        .unwrap();

    let after = assert_ok(request(&router, &bearer, "GET", "/memory/sweep".into()).await).await;
    assert_eq!(after["last_run"]["outcome"], json!("no_model"));
    assert_eq!(after["last_run"]["expired"], json!(2));
    assert_eq!(after["last_run"]["scope"]["kind"], json!("personal"));
}
