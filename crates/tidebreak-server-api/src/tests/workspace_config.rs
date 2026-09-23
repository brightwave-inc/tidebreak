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
    let info: serde_json::Value = json_body(response).await;
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
