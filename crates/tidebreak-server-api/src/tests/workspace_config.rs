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
