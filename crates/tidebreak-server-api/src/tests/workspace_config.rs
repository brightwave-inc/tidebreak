use super::*;

use crate::workspace_config::{
    WorkspaceConfigAction, WorkspaceConfigApplyRequest, WorkspaceConfigDecision,
    WorkspaceConfigDocument, WorkspaceConfigSectionId, FORMAT_VERSION,
};

#[tokio::test]
async fn export_omits_secret_values() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/mcp/servers")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "servers": [{
                            "name": "docs",
                            "command": "sh",
                            "args": ["-c", "sleep 3600"],
                            "env": ["TOKEN"],
                            "env_values": {"TOKEN": "super-secret-value"},
                            "env_from": ["PARENT"],
                            "bearer_token_env": null,
                            "enabled": false
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        put.status(),
        StatusCode::OK,
        "{:?}",
        json_body::<serde_json::Value>(put).await
    );

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/workspace-config")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = json_body(response).await;
    let text = body.to_string();
    assert!(!text.contains("super-secret-value"));
    assert!(!text.contains("env_values"));
    assert!(!text.contains("transcript"));
    assert_eq!(body["tidebreak_config"], FORMAT_VERSION);
    assert_eq!(
        body["sections"]["mcp_servers"][0]["env"],
        serde_json::json!(["TOKEN"])
    );
}

#[tokio::test]
async fn preview_and_apply_refuse_overwrite_without_replace() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/mcp/servers")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "servers": [{
                            "name": "docs",
                            "command": "sh",
                            "args": [],
                            "enabled": false
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);

    let document = WorkspaceConfigDocument {
        tidebreak_config: FORMAT_VERSION,
        exported_at: chrono::Utc::now(),
        sections: crate::workspace_config::WorkspaceConfigSections {
            code_repositories: vec![],
            mcp_servers: vec![crate::workspace_config::ExportedMcpServer {
                name: "docs".into(),
                command: Some("sh".into()),
                args: vec!["--other".into()],
                env: vec![],
                env_from: vec![],
                cwd: None,
                url: None,
                bearer_token_env: None,
                bearer_token_stored: false,
                headers: Vec::new(),
                oauth: false,
                gateway_endpoint: None,
                request_timeout_ms: 60_000,
                enabled: false,
            }],
        },
    };
    let preview = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/workspace-config/preview")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&document).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(preview.status(), StatusCode::OK);
    let preview_body: serde_json::Value = json_body(preview).await;
    assert_eq!(preview_body["entries"][0]["status"], "conflict");

    let apply = WorkspaceConfigApplyRequest {
        approved_executables: Default::default(),
        document: document.clone(),
        decisions: vec![WorkspaceConfigDecision {
            section: WorkspaceConfigSectionId::McpServers,
            key: "docs".into(),
            action: WorkspaceConfigAction::Add,
            remaps: Default::default(),
            enabled: None,
        }],
    };
    let refused = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/workspace-config/apply")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&apply).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);

    let replace = WorkspaceConfigApplyRequest {
        approved_executables: Default::default(),
        document,
        decisions: vec![WorkspaceConfigDecision {
            section: WorkspaceConfigSectionId::McpServers,
            key: "docs".into(),
            action: WorkspaceConfigAction::Replace,
            remaps: Default::default(),
            enabled: None,
        }],
    };
    let ok = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/workspace-config/apply")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&replace).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
}

#[tokio::test]
async fn preview_refuses_newer_format() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/workspace-config/preview")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "tidebreak_config": 99,
                        "exported_at": "2026-09-02T00:00:00Z",
                        "sections": {}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = json_body(response).await;
    assert_eq!(body["kind"], "workspace_config_unsupported_version");
    assert!(body["message"].as_str().unwrap().contains("upgrade"));
}

fn local_command_document(enabled: bool) -> WorkspaceConfigDocument {
    WorkspaceConfigDocument {
        tidebreak_config: FORMAT_VERSION,
        exported_at: chrono::Utc::now(),
        sections: crate::workspace_config::WorkspaceConfigSections {
            code_repositories: vec![],
            mcp_servers: vec![crate::workspace_config::ExportedMcpServer {
                name: "local_command".into(),
                command: Some("/not/started".into()),
                args: vec!["--stdio".into()],
                env: vec![],
                env_from: vec![],
                cwd: None,
                url: None,
                bearer_token_env: None,
                bearer_token_stored: false,
                headers: Vec::new(),
                oauth: false,
                gateway_endpoint: None,
                request_timeout_ms: 60_000,
                enabled,
            }],
        },
    }
}

fn add_decision(key: &str, enabled: Option<bool>) -> WorkspaceConfigDecision {
    WorkspaceConfigDecision {
        section: WorkspaceConfigSectionId::McpServers,
        key: key.into(),
        action: WorkspaceConfigAction::Add,
        remaps: Default::default(),
        enabled,
    }
}

async fn post_apply(
    router: &Router,
    bearer: &str,
    native: bool,
    request: &WorkspaceConfigApplyRequest,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method("POST")
        .uri(if native {
            "/native/workspace-config/apply"
        } else {
            "/workspace-config/apply"
        })
        .header(header::AUTHORIZATION, bearer)
        .header(header::CONTENT_TYPE, "application/json");
    if native {
        builder = builder.header(
            crate::auth::CLIENT_EXECUTOR_HEADER,
            crate::state::TEST_CLIENT_EXECUTOR_TOKEN,
        );
    }
    router
        .clone()
        .oneshot(
            builder
                .body(Body::from(serde_json::to_string(request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn mcp_server_names(router: &Router, bearer: &str) -> Vec<String> {
    let info = get_mcp_servers_json(router, bearer).await;
    info["servers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|server| server["name"].as_str().unwrap().to_owned())
        .collect()
}

/// Decision 27 covers import as well as settings saves. An import that would
/// start a local MCP command at once is refused on the renderer's route and
/// writes nothing; only the native twin, which the desktop reaches after its
/// OS dialog, passes the check. Imported turned off, the same server starts
/// nothing and needs no confirmation.
#[tokio::test]
async fn apply_refuses_an_enabled_local_command_without_native_confirmation() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let starts = WorkspaceConfigApplyRequest {
        approved_executables: Default::default(),
        document: local_command_document(true),
        decisions: vec![add_decision("local_command", None)],
    };

    let renderer = post_apply(&router, &bearer, false, &starts).await;
    assert_eq!(renderer.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(renderer).await;
    assert_eq!(error.kind, "native_confirmation_required");
    assert!(mcp_server_names(&router, &bearer).await.is_empty());

    // The command does not exist, so the native commit fails to start it,
    // but not for want of a confirmation.
    let native = post_apply(&router, &bearer, true, &starts).await;
    assert_ne!(native.status(), StatusCode::UNAUTHORIZED);
    let error: AgentErrorInfo = json_body(native).await;
    assert_ne!(error.kind, "native_confirmation_required");

    let turned_off = WorkspaceConfigApplyRequest {
        approved_executables: Default::default(),
        document: local_command_document(true),
        decisions: vec![add_decision("local_command", Some(false))],
    };
    let applied = post_apply(&router, &bearer, false, &turned_off).await;
    assert_eq!(applied.status(), StatusCode::OK);
    assert_eq!(
        mcp_server_names(&router, &bearer).await,
        vec!["local_command".to_owned()]
    );
}

/// On a multi-user deployment the MCP servers are the deployment's, and
/// `PUT /mcp/servers` is an administrator's route (decision 6). An import is
/// held to the same rule: a member's MCP entries are refused and nothing is
/// written, and an administrator's go through.
#[tokio::test]
async fn only_an_administrator_imports_mcp_servers() {
    let tokens = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        tokens.path(),
        format!("alice {ALICE_TOKEN} admin\nbob {BOB_TOKEN}\n"),
    )
    .unwrap();
    let (router, _dir) = standalone_app(|config| {
        config.auth_tokens_file = Some(tokens.path().to_owned());
    })
    .await;
    let import = WorkspaceConfigApplyRequest {
        approved_executables: Default::default(),
        document: local_command_document(false),
        decisions: vec![add_decision("local_command", None)],
    };

    let member = format!("Bearer {BOB_TOKEN}");
    let refused = post_apply(&router, &member, false, &import).await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    let admin = format!("Bearer {ALICE_TOKEN}");
    assert!(mcp_server_names(&router, &admin).await.is_empty());

    let applied = post_apply(&router, &admin, false, &import).await;
    assert_eq!(applied.status(), StatusCode::OK);
    assert_eq!(
        mcp_server_names(&router, &admin).await,
        vec!["local_command".to_owned()]
    );
}

/// An imported remote server that sends a credential from this machine's
/// environment arrives turned off, even when the file has it on: nothing
/// connects, so the variable's value goes nowhere. Only a decision that turns
/// it on starts it. Here that start fails, because the test leaves the
/// variable unset, and the server stays off.
#[tokio::test]
async fn a_remote_server_that_sends_a_credential_imports_turned_off() {
    const TOKEN: &str = "TIDEBREAK_TEST_IMPORT_BEARER_UNSET_3C9E";
    assert!(std::env::var_os(TOKEN).is_none());
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let document = WorkspaceConfigDocument {
        tidebreak_config: FORMAT_VERSION,
        exported_at: chrono::Utc::now(),
        sections: crate::workspace_config::WorkspaceConfigSections {
            code_repositories: vec![],
            mcp_servers: vec![crate::workspace_config::ExportedMcpServer {
                name: "search".into(),
                command: None,
                args: vec![],
                env: vec![],
                env_from: vec![],
                cwd: None,
                url: Some("https://mcp.example.test/search".into()),
                bearer_token_env: Some(TOKEN.into()),
                bearer_token_stored: false,
                headers: Vec::new(),
                oauth: false,
                gateway_endpoint: None,
                request_timeout_ms: 60_000,
                enabled: true,
            }],
        },
    };

    let imported = post_apply(
        &router,
        &bearer,
        false,
        &WorkspaceConfigApplyRequest {
            approved_executables: Default::default(),
            document: document.clone(),
            decisions: vec![add_decision("search", None)],
        },
    )
    .await;
    assert_eq!(imported.status(), StatusCode::OK);
    let info = get_mcp_servers_json(&router, &bearer).await;
    assert_eq!(info["servers"][0]["name"], "search");
    assert_eq!(info["servers"][0]["enabled"], false);
    assert_eq!(info["servers"][0]["health"], "disabled");

    let start = WorkspaceConfigDecision {
        action: WorkspaceConfigAction::Replace,
        ..add_decision("search", Some(true))
    };
    let started = post_apply(
        &router,
        &bearer,
        false,
        &WorkspaceConfigApplyRequest {
            approved_executables: Default::default(),
            document,
            decisions: vec![start],
        },
    )
    .await;
    assert_ne!(started.status(), StatusCode::OK);
    let error: AgentErrorInfo = json_body(started).await;
    assert!(
        error.message.contains("search") && error.message.contains("failed to start"),
        "the decision turned the server on, so apply tried to start it: {}",
        error.message
    );
    let info = get_mcp_servers_json(&router, &bearer).await;
    assert_eq!(info["servers"][0]["enabled"], false);
}

async fn get_mcp_servers_json(router: &Router, bearer: &str) -> serde_json::Value {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/mcp/servers")
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

/// A refused decision anywhere in the request leaves this machine as it was:
/// the repository an earlier decision would register is not registered.
#[tokio::test]
async fn apply_validates_every_decision_before_writing() {
    let (router, token, _runtime, dir) = super::code::code_app(Vec::new()).await;
    let bearer = format!("Bearer {token}");
    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/mcp/servers")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "servers": [{ "name": "local_command", "command": "sh", "enabled": false }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);

    let checkout = super::code::init_git_repo(dir.path());
    let mut document = local_command_document(false);
    document.sections.code_repositories = vec![crate::workspace_config::ExportedCodeRepository {
        display_name: "origin".into(),
        origin_url: None,
        root_path: checkout.display().to_string(),
        default_base_ref: "main".into(),
        branch_prefix: "tidebreak/".into(),
        setup_script: None,
        archive_script: None,
        quick_actions: vec![],
        cloned_from: None,
    }];
    let register = WorkspaceConfigDecision {
        section: WorkspaceConfigSectionId::CodeRepositories,
        key: "origin".into(),
        action: WorkspaceConfigAction::Add,
        remaps: Default::default(),
        enabled: None,
    };
    // The MCP decision comes second and conflicts with the server on file.
    let refused = post_apply(
        &router,
        &bearer,
        false,
        &WorkspaceConfigApplyRequest {
            approved_executables: Default::default(),
            document: document.clone(),
            decisions: vec![register.clone(), add_decision("local_command", None)],
        },
    )
    .await;
    assert_eq!(refused.status(), StatusCode::CONFLICT);

    let repos = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/code/repos")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(repos.status(), StatusCode::OK);
    let repos: Vec<serde_json::Value> = json_body(repos).await;
    assert!(
        repos.is_empty(),
        "a refused import registered a repository: {repos:?}"
    );

    let replace = WorkspaceConfigDecision {
        action: WorkspaceConfigAction::Replace,
        ..add_decision("local_command", None)
    };
    let applied = post_apply(
        &router,
        &bearer,
        false,
        &WorkspaceConfigApplyRequest {
            approved_executables: Default::default(),
            document,
            decisions: vec![register, replace],
        },
    )
    .await;
    assert_eq!(
        applied.status(),
        StatusCode::OK,
        "{:?}",
        json_body::<serde_json::Value>(applied).await
    );
}

async fn registered_repos(router: &Router, bearer: &str) -> Vec<serde_json::Value> {
    let repos = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/code/repos")
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(repos.status(), StatusCode::OK);
    json_body(repos).await
}

/// An import's repositories and MCP servers land together or not at all.
/// The repository here is valid and is written first; the MCP server's
/// command does not exist, so the MCP set fails to start it, and the
/// repository comes back out. The refusal says so. A second import that
/// leaves the server undecided skips it and counts it.
#[tokio::test]
async fn an_import_whose_mcp_servers_fail_leaves_no_repository_behind() {
    let (router, token, _runtime, dir) = super::code::code_app(Vec::new()).await;
    let bearer = format!("Bearer {token}");
    let checkout = super::code::init_git_repo(dir.path());
    let mut document = local_command_document(true);
    document.sections.code_repositories = vec![crate::workspace_config::ExportedCodeRepository {
        display_name: "origin".into(),
        origin_url: None,
        root_path: checkout.display().to_string(),
        default_base_ref: "main".into(),
        branch_prefix: "tidebreak/".into(),
        setup_script: None,
        archive_script: None,
        quick_actions: vec![],
        cloned_from: None,
    }];
    let register = WorkspaceConfigDecision {
        section: WorkspaceConfigSectionId::CodeRepositories,
        key: "origin".into(),
        action: WorkspaceConfigAction::Add,
        remaps: Default::default(),
        enabled: None,
    };

    let refused = post_apply(
        &router,
        &bearer,
        true,
        &WorkspaceConfigApplyRequest {
            approved_executables: Default::default(),
            document: document.clone(),
            decisions: vec![register.clone(), add_decision("local_command", None)],
        },
    )
    .await;
    assert!(!refused.status().is_success());
    let error: AgentErrorInfo = json_body(refused).await;
    assert!(
        error.message.contains("local_command") && error.message.ends_with("Nothing changed."),
        "{}",
        error.message
    );
    assert!(
        registered_repos(&router, &bearer).await.is_empty(),
        "the repository outlived the MCP failure"
    );
    assert!(mcp_server_names(&router, &bearer).await.is_empty());

    let applied = post_apply(
        &router,
        &bearer,
        false,
        &WorkspaceConfigApplyRequest {
            approved_executables: Default::default(),
            document,
            decisions: vec![register],
        },
    )
    .await;
    assert_eq!(applied.status(), StatusCode::OK);
    let result: crate::workspace_config::WorkspaceConfigApplyResult = json_body(applied).await;
    assert_eq!((result.applied, result.skipped), (1, 1));
    assert_eq!(registered_repos(&router, &bearer).await.len(), 1);
    assert!(mcp_server_names(&router, &bearer).await.is_empty());
}

/// An import is its caller's own. Alice registered a checkout as "origin".
/// Bob's preview of a file with the same entry does not see hers, so it reads
/// as new, and his Replace registers his own; hers stays as she left it.
#[tokio::test]
async fn an_import_neither_sees_nor_changes_another_owners_repositories() {
    let (dir, store) = temp_db_store("import-owners.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let runtime = Arc::new(crate::code::CodeRuntime::new(
        db,
        dir.path().to_path_buf(),
        None,
        None,
        None,
        None,
        None,
        None,
    ));
    let checkout = super::code::init_git_repo(dir.path());
    let alice = tidebreak_core::OwnerId::new("user:alice").unwrap();
    let hers = tidebreak_core::CodeRepo {
        id: tidebreak_core::RepoId::new(),
        owner: alice.clone(),
        root_path: checkout.display().to_string(),
        display_name: "origin".into(),
        default_base_ref: "main".into(),
        branch_prefix: "alice/".into(),
        setup_script: None,
        archive_script: None,
        quick_actions: Vec::new(),
        created_at: chrono::Utc::now(),
        removed_at: None,
        cloned_from: None,
        origin_host: None,
        origin_owner: None,
        origin_name: None,
    };
    tidebreak_core::db::code::insert_repo(&runtime.db, &hers)
        .await
        .unwrap();
    let tokens_file = dir.path().join("tokens");
    std::fs::write(
        &tokens_file,
        format!("alice {ALICE_TOKEN} admin\nbob {BOB_TOKEN}\n"),
    )
    .unwrap();
    let mut config = Config::desktop(dir.path());
    config.profile = Profile::SelfHost;
    config.auth_tokens_file = Some(tokens_file);
    let mut state = AppState::new(
        config,
        store_trait,
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    state.code = Some(runtime.clone());
    let router = app(state);
    let bob = format!("Bearer {BOB_TOKEN}");

    let mut document = local_command_document(false);
    document.sections.mcp_servers.clear();
    document.sections.code_repositories = vec![crate::workspace_config::ExportedCodeRepository {
        display_name: "origin".into(),
        origin_url: None,
        root_path: checkout.display().to_string(),
        default_base_ref: "main".into(),
        branch_prefix: "bob/".into(),
        setup_script: None,
        archive_script: None,
        quick_actions: vec![],
        cloned_from: None,
    }];

    let preview = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/workspace-config/preview")
                .header(header::AUTHORIZATION, &bob)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&document).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(preview.status(), StatusCode::OK);
    let preview: serde_json::Value = json_body(preview).await;
    assert_eq!(preview["entries"][0]["status"], "new", "{preview}");

    let applied = post_apply(
        &router,
        &bob,
        false,
        &WorkspaceConfigApplyRequest {
            approved_executables: Default::default(),
            document,
            decisions: vec![WorkspaceConfigDecision {
                section: WorkspaceConfigSectionId::CodeRepositories,
                key: "origin".into(),
                action: WorkspaceConfigAction::Replace,
                remaps: Default::default(),
                enabled: None,
            }],
        },
    )
    .await;
    assert_eq!(
        applied.status(),
        StatusCode::OK,
        "{:?}",
        json_body::<serde_json::Value>(applied).await
    );
    let his = registered_repos(&router, &bob).await;
    assert_eq!(his.len(), 1, "{his:?}");
    assert_eq!(his[0]["branch_prefix"], "bob/");
    assert_ne!(his[0]["id"], hers.id.to_string());

    let stored = tidebreak_core::db::code::get_repo(&runtime.db, &alice, hers.id)
        .await
        .unwrap()
        .expect("Alice's registration stays");
    assert_eq!(stored.branch_prefix, "alice/");
    assert!(stored.removed_at.is_none());
}
