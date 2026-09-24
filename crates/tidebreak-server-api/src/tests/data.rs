//! Data and privacy routes: the profile overview, the backup archive,
//! conversation exports, and resetting settings.

use super::*;

use std::io::Read as _;

use axum::response::Response;

fn state_with_database(dir: &tempfile::TempDir, db: Arc<DbStore>) -> AppState {
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        db.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    state.memory = Some(db);
    state
}

async fn data_app() -> (Router, String, Arc<DbStore>, tempfile::TempDir) {
    let (dir, store) = temp_db_store("data-routes.db").await;
    let db = Arc::new(store);
    let state = state_with_database(&dir, db.clone());
    let bearer = format!("Bearer {}", state.token);
    (app(state), bearer, db, dir)
}

async fn call(
    router: &Router,
    bearer: &str,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, bearer);
    let body = match body {
        Some(body) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap()
}

async fn bytes_of(response: Response) -> Vec<u8> {
    to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

async fn add_message(store: &Arc<DbStore>, chat: SessionId, role: Role, content: &str) {
    store
        .append_message(&Message {
            id: MessageId::new(),
            chat_id: chat,
            turn_id: TurnId::new(),
            role,
            reasoning: Default::default(),
            content: content.to_owned(),
            llm_content: None,
            created_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn the_overview_names_the_profile_and_its_disk_use() {
    let (router, bearer, _db, dir) = data_app().await;
    std::fs::create_dir_all(dir.path().join("logs")).unwrap();
    std::fs::write(dir.path().join("logs/tidebreak.log"), b"line\n").unwrap();

    let response = call(&router, &bearer, "GET", "/data", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let overview: crate::profile_data::DataOverview = json_body(response).await;

    assert_eq!(overview.data_dir, dir.path().display().to_string());
    assert_eq!(overview.storage, crate::profile_data::DataStorage::Sqlite);
    assert_eq!(overview.backup_unavailable, None);
    let logs = overview
        .usage
        .iter()
        .find(|entry| entry.category == crate::profile_data::DataCategory::Logs)
        .unwrap();
    assert_eq!(logs.bytes, 5);
    assert_eq!(
        overview.total_bytes,
        overview.usage.iter().map(|entry| entry.bytes).sum::<u64>()
    );
}

/// The backup is the whole database, whatever file the store opened, and it
/// arrives complete: the length the response announces is the length it
/// sends.
#[tokio::test]
async fn a_backup_downloads_an_archive_that_restores_the_conversations() {
    let (router, bearer, db, dir) = data_app().await;
    let chat = make_chat(&router, &bearer).await;
    add_message(&db, chat.id, Role::User, "Keep this conversation.").await;
    std::fs::create_dir_all(dir.path().join("blobs/ab")).unwrap();
    std::fs::write(dir.path().join("blobs/ab/blob"), b"attached bytes").unwrap();

    let response = call(&router, &bearer, "POST", "/data/backup", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/gzip");
    let announced: usize = response.headers()[header::CONTENT_LENGTH]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    let bytes = bytes_of(response).await;
    assert_eq!(bytes.len(), announced);

    let restored = tempfile::tempdir().unwrap();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_slice()));
    let mut names = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().display().to_string();
        let mut contents = Vec::new();
        entry.read_to_end(&mut contents).unwrap();
        if name == "tidebreak.db" {
            std::fs::write(restored.path().join("tidebreak.db"), &contents).unwrap();
        }
        if name == "blobs/ab/blob" {
            assert_eq!(contents, b"attached bytes");
        }
        names.push(name);
    }
    assert!(names.contains(&"tidebreak.db".to_owned()), "{names:?}");
    assert!(names.contains(&"blobs/ab/blob".to_owned()), "{names:?}");
    assert!(
        !names.iter().any(|name| name.starts_with("backups/")),
        "{names:?}"
    );

    let reopened = DbStore::connect(&format!(
        "sqlite://{}?mode=rwc",
        restored.path().join("tidebreak.db").display()
    ))
    .await
    .unwrap();
    let messages = reopened.list_messages(chat.id).await.unwrap();
    assert!(messages
        .iter()
        .any(|message| message.content == "Keep this conversation."));
}

/// A server with no local database, such as one on PostgreSQL, refuses the
/// backup and says why, and the overview carries the same reason.
#[tokio::test]
async fn a_server_without_a_local_database_refuses_a_backup() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let refused = call(&router, &bearer, "POST", "/data/backup", None).await;
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let error: AgentErrorInfo = json_body(refused).await;
    assert_eq!(error.kind, "backup_unavailable");

    let overview: crate::profile_data::DataOverview =
        json_body(call(&router, &bearer, "GET", "/data", None).await).await;
    assert!(overview.backup_unavailable.is_some());
}

#[tokio::test]
async fn an_export_holds_only_the_conversations_asked_for() {
    let (router, bearer, db, _dir) = data_app().await;
    let first = make_chat(&router, &bearer).await;
    let second = make_chat(&router, &bearer).await;
    add_message(&db, first.id, Role::User, "First question").await;
    add_message(&db, first.id, Role::Assistant, "First answer").await;
    add_message(&db, second.id, Role::User, "Second question").await;

    let chosen = call(
        &router,
        &bearer,
        "POST",
        "/data/export",
        Some(serde_json::json!({ "format": "json", "chat_ids": [first.id] })),
    )
    .await;
    assert_eq!(chosen.status(), StatusCode::OK);
    assert_eq!(chosen.headers()["x-tidebreak-conversations"], "1");
    let document: serde_json::Value = serde_json::from_slice(&bytes_of(chosen).await).unwrap();
    assert_eq!(document["tidebreak_export"], 1);
    let conversations = document["conversations"].as_array().unwrap();
    assert_eq!(conversations.len(), 1);
    let messages = conversations[0]["messages"].as_array().unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|message| (
                message["role"].as_str().unwrap(),
                message["text"].as_str().unwrap()
            ))
            .collect::<Vec<_>>(),
        [("user", "First question"), ("assistant", "First answer")]
    );

    let everything = call(
        &router,
        &bearer,
        "POST",
        "/data/export",
        Some(serde_json::json!({ "format": "markdown" })),
    )
    .await;
    assert_eq!(everything.status(), StatusCode::OK);
    assert_eq!(
        everything.headers()[header::CONTENT_TYPE],
        "application/zip"
    );
    let bytes = bytes_of(everything).await;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    assert_eq!(archive.len(), 2);
    let mut texts = Vec::new();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).unwrap();
        assert!(file.name().ends_with(".md"), "{}", file.name());
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();
        texts.push(text);
    }
    assert!(texts.iter().any(|text| text.contains("First answer")));
    assert!(texts.iter().any(|text| text.contains("Second question")));

    let unknown = call(
        &router,
        &bearer,
        "POST",
        "/data/export",
        Some(serde_json::json!({ "format": "json", "chat_ids": [SessionId::new()] })),
    )
    .await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}

/// A reset puts the preferences back to their defaults and leaves what the
/// person wrote alone.
#[tokio::test]
async fn a_settings_reset_restores_defaults_and_keeps_instructions() {
    let (router, bearer, _db, _dir) = data_app().await;
    let defaults: serde_json::Value =
        json_body(call(&router, &bearer, "GET", "/settings", None).await).await;
    let changed = call(
        &router,
        &bearer,
        "PUT",
        "/settings",
        Some(serde_json::json!({
            "max_active_background_agents": 2,
            "code_turn_recaps_enabled": false,
            "prompt_cache_retention": "one_hour",
            "compaction": { "threshold_fraction": 0.9 },
            "git_source_control": { "auto_rename_branches": false },
        })),
    )
    .await;
    assert_eq!(changed.status(), StatusCode::OK);
    let instructions = call(
        &router,
        &bearer,
        "PUT",
        "/settings/instructions",
        Some(serde_json::json!({ "instructions": "Answer in plain English." })),
    )
    .await;
    assert!(instructions.status().is_success());

    let reset = call(&router, &bearer, "POST", "/settings/reset", None).await;
    assert_eq!(reset.status(), StatusCode::OK);
    let after: serde_json::Value = json_body(reset).await;

    assert_eq!(after, defaults);
    let kept: serde_json::Value =
        json_body(call(&router, &bearer, "GET", "/settings/instructions", None).await).await;
    assert_eq!(kept["instructions"], "Answer in plain English.");
}
