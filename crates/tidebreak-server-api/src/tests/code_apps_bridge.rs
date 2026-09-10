//! External engines inherit Tidebreak's connected apps through the loopback
//! MCP bridge.

use super::code::*;

use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::CapLevel;

/// A session on an external engine is handed the bridge, and the bridge
/// answers that session's token with the mounted MCP tools. Without the
/// channel a Codex or Claude Code chat has no connected apps at all.
#[tokio::test]
async fn an_external_engine_session_mounts_the_connected_apps_bridge() {
    let adapter = ScriptedAdapter::new(plain_text_script()).with_approvals(CapLevel::Supported);
    let (router, token, runtime, dir) = code_app_with(adapter.clone()).await;
    let addr = serve(router).await;
    runtime.start(format!("http://{addr}")).await.unwrap();
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let created = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);

    let launched = adapter.launched_apps();
    assert_eq!(launched.len(), 1, "one launch");
    let apps = launched[0]
        .clone()
        .expect("an external engine is always handed the connected-apps bridge");
    assert_eq!(
        apps.mcp_endpoint_url,
        format!("http://{addr}/code/mcp/connected-apps")
    );

    let listed = client
        .post(&apps.mcp_endpoint_url)
        .bearer_auth(&apps.token)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = listed.json().await.unwrap();
    assert!(
        body["result"]["tools"].is_array(),
        "the bridge lists the MCP runtime's tools: {body}"
    );

    // The install token is not a bridge token: only the session-scoped
    // bearer the worker was handed reaches the apps.
    let refused = client
        .post(&apps.mcp_endpoint_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);
}
