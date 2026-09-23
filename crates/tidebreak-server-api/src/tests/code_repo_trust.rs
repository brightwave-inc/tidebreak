//! Repository trust end to end: the decision, the engine config a checkout
//! carries, and what an engine launch is told to load.

use super::code::*;

use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::CapLevel;
use tidebreak_harness::ProjectConfig;

fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

async fn read_json(response: reqwest::Response) -> serde_json::Value {
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response.json().await.unwrap()
}

/// A repository's own engine config stays off until its owner trusts the
/// repository. The workspace read lists what the worktree carries, the
/// decision lands in the repository's own git config, and an idle session
/// restarts its engine under each new decision.
#[tokio::test]
async fn engine_config_loads_only_after_the_owner_trusts_the_repository() {
    let adapter = ScriptedAdapter::new(plain_text_script()).with_approvals(CapLevel::Supported);
    let (router, token, runtime, dir) = code_app_with(adapter.clone()).await;
    let addr = serve(router).await;
    runtime.start(format!("http://{addr}")).await.unwrap();
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    std::fs::create_dir_all(repo.join(".claude")).unwrap();
    std::fs::write(
        repo.join(".claude/settings.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"./setup.sh"}]}]}}"#,
    )
    .unwrap();
    git(&repo, &["add", ".claude/settings.json"]);
    git(&repo, &["commit", "-m", "engine settings"]);
    let (repo_body, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let repo_id = json_id(&repo_body).to_owned();
    let workspace_id = json_id(&workspace).to_owned();

    let before = read_json(
        client
            .get(format!(
                "http://{addr}/code/workspaces/{workspace_id}/trust"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(before["repo_id"], repo_id.as_str());
    assert_eq!(before["trust"], "undecided");
    assert_eq!(
        before["files"],
        serde_json::json!([{
            "path": ".claude/settings.json",
            "engines": ["claude_code"],
            "effects": [{ "kind": "hooks", "count": 1 }],
        }])
    );

    let created = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/sessions"
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
    assert_eq!(
        adapter.launched_project_configs(),
        [ProjectConfig::Skip],
        "an undecided repository's engine config stays off"
    );

    let trusted = read_json(
        client
            .put(format!("http://{addr}/code/repos/{repo_id}/trust"))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "trusted": true }))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(trusted["trust"], "trusted");
    assert_eq!(
        git(&repo, &["config", "--local", "--get", "tidebreak.trusted"]),
        "true"
    );
    assert_eq!(
        adapter.launched_project_configs(),
        [ProjectConfig::Skip, ProjectConfig::Load],
        "the idle session restarts its engine with the repository's config"
    );

    let revoked = read_json(
        client
            .put(format!("http://{addr}/code/repos/{repo_id}/trust"))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "trusted": false }))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(revoked["trust"], "untrusted");
    assert_eq!(
        adapter.launched_project_configs(),
        [
            ProjectConfig::Skip,
            ProjectConfig::Load,
            ProjectConfig::Skip
        ],
        "revoking trust turns the repository's config off again"
    );
    let after = read_json(
        client
            .get(format!("http://{addr}/code/repos/{repo_id}/trust"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(after["trust"], "untrusted");
    assert_eq!(after["files"][0]["path"], ".claude/settings.json");
}

/// A worktree without engine config reads as undecided with nothing listed,
/// which is what lets the desktop start the first session without asking.
#[tokio::test]
async fn a_worktree_without_engine_config_lists_nothing() {
    let (router, token, runtime, dir) =
        code_app_with(ScriptedAdapter::new(plain_text_script())).await;
    let addr = serve(router).await;
    runtime.start(format!("http://{addr}")).await.unwrap();
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let trust = read_json(
        client
            .get(format!(
                "http://{addr}/code/workspaces/{}/trust",
                json_id(&workspace)
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(trust["trust"], "undecided");
    assert_eq!(trust["files"], serde_json::json!([]));
}
